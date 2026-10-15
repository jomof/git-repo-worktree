"""Stateful fuzzing of git-repo-worktree against real repo clients.

A tiny world (projects a, lib/b, prebuilts/pre with clone-depth, an unused
remote c, a manifest repository, a source client made with `repo init` +
`repo sync`, and a reference client) is driven by random sequences of
upstream, manifest, source, tree, and failure operations. Invariants are
checked after every step; `repo sync` in a tree is compared with the same sync
in the reference client. Hypothesis shrinks a failing sequence, and its
commands are written to the findings directory.

    GRW_BIN=target/debug/git-repo-worktree python fuzz/harness.py [examples]

Modes (environment):
    GRW_FUZZ_OVERLAY      auto (default; on where fuse-overlayfs works), 1, 0
    GRW_FUZZ_MAINTENANCE  1 to let background pool maintenance run
    GRW_FUZZ_CRASH        1 to crash add and move mid-way (needs a binary
                          built with --features test-hooks)
    GRW_FUZZ_STEPS        steps per sequence (default 10)
    GRW_FUZZ_FINDINGS     where reproductions go (default fuzz/findings)
"""

import json
import os
import shutil
import subprocess
import sys
import tarfile
import time
from pathlib import Path

from hypothesis import HealthCheck, settings
from hypothesis import strategies as st
from hypothesis.stateful import (
    Bundle,
    RuleBasedStateMachine,
    consumes,
    initialize,
    invariant,
    precondition,
    rule,
    run_state_machine_as_test,
)

HERE = Path(__file__).resolve().parent
BIN = os.path.abspath(os.environ.get("GRW_BIN", "target/debug/git-repo-worktree"))
REPO_SRC = os.environ.get("GRW_TEST_REPO_URL", "/tmp/git-repo-src")
WORKER = os.environ.get("GRW_FUZZ_WORKER", "0")
BASE = Path(os.environ.get("GRW_FUZZ_BASE", f"/tmp/grw-fuzz-{WORKER}")).resolve()
FINDINGS = Path(os.environ.get("GRW_FUZZ_FINDINGS", HERE / "findings"))
WORLD = BASE / "w"
CLIENT = WORLD / "client"
TEMPLATE = BASE / "template.tar"
EDITABLE = ["a", "lib/b"]
MAINTENANCE = os.environ.get("GRW_FUZZ_MAINTENANCE") == "1"
CRASH = os.environ.get("GRW_FUZZ_CRASH") == "1"


def overlay_mode():
    wanted = os.environ.get("GRW_FUZZ_OVERLAY", "auto")
    if wanted in ("0", "1"):
        return wanted == "1"
    return (
        sys.platform.startswith("linux")
        and shutil.which("fuse-overlayfs") is not None
        and Path("/dev/fuse").exists()
    )


OVERLAY = overlay_mode()
ENV = {
    name: value for name, value in os.environ.items() if not name.startswith("GIT_REPO_WORKTREE_")
}
ENV.update(
    {
        "GIT_AUTHOR_NAME": "fuzz",
        "GIT_AUTHOR_EMAIL": "fuzz@example.com",
        "GIT_COMMITTER_NAME": "fuzz",
        "GIT_COMMITTER_EMAIL": "fuzz@example.com",
        "GRW_TEST_POOL_RESERVE_BYTES": "0",
    }
)
if not MAINTENANCE:
    ENV["GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE"] = "1"
if not OVERLAY:
    ENV["GIT_REPO_WORKTREE_NO_OVERLAY"] = "1"

LOG = []
STATS = {}


class Finding(AssertionError):
    """A violated expectation, carrying the commands that led to it."""

    def __init__(self, message):
        super().__init__(message)
        self.log = list(LOG)


def step(description):
    LOG.append(f"# step: {description}")
    STATS[description] = STATS.get(description, 0) + 1


def run(args, cwd, check=True, env=None):
    """Runs a command, logging it as shell for the reproduction."""
    shown = " ".join(str(arg) for arg in args).replace(str(BIN), "grw")
    prefix = "".join(f"{key}={value} " for key, value in (env or {}).items())
    LOG.append(f"(cd {cwd} && {prefix}{shown})")
    result = subprocess.run(
        [str(arg) for arg in args],
        cwd=cwd,
        env={**ENV, **(env or {})},
        capture_output=True,
        text=True,
    )
    if check and result.returncode != 0:
        raise Finding(f"{shown} in {cwd} exited {result.returncode}:\n{result.stderr[-2000:]}")
    return result


def grw(*args, check=True, env=None):
    return run([BIN, "-C", CLIENT, *args], WORLD, check, env)


def grw_json(*args, check=True, env=None):
    result = grw(*args, "--json", check=False, env=env)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        raise Finding(f"grw {' '.join(args)} printed no JSON (exit {result.returncode}):\n{result.stdout}\n{result.stderr[-2000:]}")
    if check and not value["ok"]:
        raise Finding(f"grw {' '.join(args)} failed: {json.dumps(value['error'])}\n{result.stderr[-2000:]}")
    return value


def git(cwd, *args, check=True):
    return run(["git", *args], cwd, check).stdout.strip()


MANIFEST = """<?xml version="1.0" encoding="UTF-8"?>
<manifest>
  <remote name="origin" fetch="file://{world}/remotes"/>
  <default remote="origin" revision="main"/>
{projects}</manifest>
"""
PROJECT_LINES = {
    "a": '  <project name="a" path="a">{link}<copyfile src="README" dest="A_COPY"/></project>\n',
    "b": '  <project name="b" path="lib/b"/>\n',
    "pre": '  <project name="pre" path="prebuilts/pre" clone-depth="1"/>\n',
    "c": '  <project name="c" path="c"/>\n',
}


def manifest_text(projects, linkfile):
    link = '<linkfile src="README" dest="A_README"/>' if linkfile else ""
    lines = "".join(PROJECT_LINES[name].format(link=link) for name in ["a", "b", "pre", "c"] if name in projects)
    return MANIFEST.format(world=WORLD, projects=lines)


def build_template():
    if WORLD.exists():
        shutil.rmtree(WORLD)
    (WORLD / "remotes").mkdir(parents=True)
    for name in ["a", "b", "pre", "c", "manifest"]:
        git(WORLD, "init", "-q", "--bare", "-b", "main", WORLD / "remotes" / f"{name}.git")
        seed = WORLD / "seed" / name
        seed.mkdir(parents=True)
        git(seed, "init", "-q", "-b", "main")
        if name == "manifest":
            (seed / "default.xml").write_text(manifest_text({"a", "b", "pre"}, True))
        else:
            (seed / "README").write_text(f"{name} 0\n")
        git(seed, "add", ".")
        git(seed, "commit", "-qm", "init")
        git(seed, "push", "-q", WORLD / "remotes" / f"{name}.git", "main")
    for client in ["client", "ref"]:
        (WORLD / client).mkdir()
        run(
            ["repo", "init", "-q", "-u", f"file://{WORLD}/remotes/manifest.git", "-b", "main",
             "--repo-url", REPO_SRC, "--repo-rev", "main", "--no-repo-verify"],
            WORLD / client,
        )
        run(["repo", "sync", "-q"], WORLD / client)
    with tarfile.open(TEMPLATE, "w") as archive:
        archive.add(WORLD, arcname="w")


def unmount_all():
    if not OVERLAY:
        return
    mounts = Path("/proc/self/mountinfo").read_text().splitlines()
    points = [line.split(" ")[4] for line in mounts if str(BASE) in line.split(" ")[4]]
    for point in sorted(points, key=len, reverse=True):
        subprocess.run(["fusermount3", "-uz", point], capture_output=True)


def fresh_world():
    subprocess.run(["pkill", "-f", f"-C {CLIENT} -q pool maintain"], capture_output=True)
    unmount_all()
    if WORLD.exists():
        subprocess.run(["chmod", "-R", "u+w", WORLD], capture_output=True)
        shutil.rmtree(WORLD, ignore_errors=True)
    for pattern in ["t[0-9]*", "t-moved-*", ".git-repo-worktree-trash-*"]:
        for stray in BASE.glob(pattern):
            shutil.rmtree(stray, ignore_errors=True)
    with tarfile.open(TEMPLATE) as archive:
        archive.extractall(BASE, filter="fully_trusted")


def source_snapshot():
    snapshot = {}
    for path in ["a", "lib/b", "prebuilts/pre", "c"]:
        checkout = CLIENT / path
        if checkout.is_dir():
            snapshot[path] = (
                git(checkout, "rev-parse", "HEAD"),
                git(checkout, "status", "--porcelain", "--untracked-files=all"),
            )
    return snapshot


def maintenance_running():
    result = subprocess.run(
        ["pgrep", "-f", f"-C {CLIENT} -q pool maintain"], capture_output=True, text=True
    )
    return result.returncode == 0


def owned_projects(root):
    """Projects the tree checks out itself: real directories, not shared links."""
    owned = set()
    for name, path in [("a", "a"), ("b", "lib/b"), ("pre", "prebuilts/pre"), ("c", "c")]:
        checkout = root / path
        if checkout.is_symlink() or not checkout.is_dir():
            continue
        owned.add(name)
    return owned


def repo_list(root):
    result = run(["repo", "list", "-n"], root, check=False)
    return result, set(result.stdout.split())


class TreeWorld(RuleBasedStateMachine):
    trees = Bundle("trees")

    def __init__(self):
        super().__init__()
        LOG.clear()
        fresh_world()
        self.count = 0
        self.live = {}
        self.unbranched = {}
        self.branches = {}
        self.dead = set()
        self.manifest_projects = {"a", "b", "pre"}
        self.linkfile = True

    def teardown(self):
        subprocess.run(["pkill", "-f", f"-C {CLIENT} -q pool maintain"], capture_output=True)
        unmount_all()

    # -- upstream, manifest, and source actors ---------------------------------

    @rule(name=st.sampled_from(["a", "b", "pre", "c"]))
    def upstream_commit(self, name):
        step(f"upstream_commit({name})")
        seed = WORLD / "seed" / name
        self.count += 1
        (seed / "README").write_text(f"{name} {self.count}\n")
        git(seed, "commit", "-qam", f"upstream {self.count}")
        git(seed, "push", "-q", WORLD / "remotes" / f"{name}.git", "main")

    @rule(change=st.sampled_from(["add c", "remove c", "remove b", "restore b", "toggle linkfile"]))
    def manifest_change(self, change):
        step(f"manifest_change({change})")
        projects = set(self.manifest_projects)
        linkfile = self.linkfile
        if change == "add c":
            projects.add("c")
        elif change == "remove c":
            projects.discard("c")
        elif change == "remove b":
            projects.discard("b")
        elif change == "restore b":
            projects.add("b")
        else:
            linkfile = not linkfile
        if (projects, linkfile) == (self.manifest_projects, self.linkfile):
            return
        seed = WORLD / "seed" / "manifest"
        (seed / "default.xml").write_text(manifest_text(projects, linkfile))
        git(seed, "commit", "-qam", f"manifest: {change}")
        git(seed, "push", "-q", WORLD / "remotes" / "manifest.git", "main")
        self.manifest_projects = projects
        self.linkfile = linkfile

    @rule()
    def source_sync(self):
        step("source_sync()")
        result = run(["repo", "sync", "-q"], CLIENT, check=False)
        if result.returncode != 0 and "lib/b" not in result.stderr:
            raise Finding(f"repo sync failed in the source:\n{result.stderr[-2000:]}")

    @rule(path=st.sampled_from(EDITABLE))
    def source_commit(self, path):
        step(f"source_commit({path})")
        checkout = CLIENT / path
        if not checkout.is_dir():
            return
        self.count += 1
        (checkout / f"local{self.count}").write_text("x\n")
        git(checkout, "add", ".")
        git(checkout, "commit", "-qm", f"source {self.count}")

    # -- tool operations -------------------------------------------------------

    def tool_step(self, operation):
        before = source_snapshot()
        result = operation()
        after = source_snapshot()
        if before != after:
            raise Finding(f"the source client changed:\nbefore {before}\nafter {after}")
        return result

    def new_tree(self, name, path):
        self.live[name] = path
        self.unbranched[name] = False
        self.branches[name] = set()

    @initialize(target=trees)
    def first_tree(self):
        return self.add()

    @rule(target=trees)
    def add(self):
        step("add()")
        self.count += 1
        name = f"t{self.count}"
        path = BASE / name
        self.tool_step(lambda: grw_json("add", "--name", name, str(path)))
        self.new_tree(name, path)
        return name

    @precondition(lambda self: CRASH)
    @rule(target=trees, after=st.integers(1, 3))
    def add_crash_and_continue(self, after):
        step(f"add_crash_and_continue(after={after})")
        self.count += 1
        name = f"t{self.count}"
        path = BASE / name
        crashed = grw("add", "--name", name, str(path), check=False, env={"GRW_TEST_CRASH_AFTER": str(after)})
        if crashed.returncode != 0:
            self.tool_step(lambda: grw_json("add", "--continue", name))
        self.new_tree(name, path)
        return name

    @rule(tree=consumes(trees), force=st.booleans())
    def remove(self, tree, force):
        step(f"remove({tree}, force={force})")
        if tree not in self.live:
            return
        args = ["remove", tree] + (["--force"] if force else [])
        value = self.tool_step(lambda: grw_json(*args, check=False))
        if value["ok"]:
            del self.live[tree]
            self.dead.discard(tree)
        elif value["error"]["code"] == "E_DIRTY" and self.unbranched[tree] and not force:
            pass
        else:
            raise Finding(f"remove {tree} failed: {json.dumps(value['error'])}")

    @rule(tree=trees)
    def repair(self, tree):
        step(f"repair({tree})")
        if tree in self.live:
            self.tool_step(lambda: grw_json("repair", tree))
            self.dead.discard(tree)

    @precondition(lambda self: self.live)
    @rule()
    def repair_all(self):
        step("repair_all()")
        if self.live:
            self.tool_step(lambda: grw_json("repair"))
            self.dead.clear()

    @rule(tree=trees)
    def move(self, tree):
        step(f"move({tree})")
        if tree not in self.live or tree in self.dead:
            return
        self.count += 1
        target = BASE / f"t-moved-{self.count}"
        self.tool_step(lambda: grw_json("move", tree, str(target)))
        self.live[tree] = target

    @precondition(lambda self: CRASH)
    @rule(tree=trees)
    def move_crash_and_repair(self, tree):
        step(f"move_crash_and_repair({tree})")
        if tree not in self.live or tree in self.dead:
            return
        self.count += 1
        target = BASE / f"t-moved-{self.count}"
        grw("move", tree, str(target), check=False, env={"GRW_TEST_CRASH_AFTER_MOVE": "1"})
        self.tool_step(lambda: grw_json("repair", tree))
        trees = {entry["name"]: entry for entry in grw_json("list")["result"]["trees"]}
        self.live[tree] = Path(trees[tree]["path"])

    @rule(path=st.sampled_from(EDITABLE), mode=st.sampled_from(["pool", "auto"]))
    def set_default(self, path, mode):
        step(f"set_default({path}, {mode})")
        self.tool_step(lambda: grw_json("defaults", mode, path))

    @precondition(lambda self: OVERLAY)
    @rule(tree=trees)
    def kill_overlay(self, tree):
        step(f"kill_overlay({tree})")
        if tree not in self.live:
            return
        overlays = CLIENT / ".repo/repo-worktrees" / tree / "overlays"
        killed = run(["pkill", "-9", "-f", f"fuse-overlayfs.*{overlays}"], WORLD, check=False)
        if killed.returncode == 0:
            time.sleep(0.3)
            self.dead.add(tree)

    @precondition(lambda self: OVERLAY)
    @rule(tree=trees)
    def repair_mounts(self, tree):
        step(f"repair_mounts({tree})")
        if tree in self.live:
            self.tool_step(lambda: grw_json("repair", "--mounts-only", tree))
            self.dead.discard(tree)

    @precondition(lambda self: MAINTENANCE and self.live)
    @rule()
    def settle(self):
        step("settle()")
        deadline = time.time() + 180
        while maintenance_running():
            if time.time() > deadline:
                raise Finding("background maintenance did not finish within 180 s")
            time.sleep(0.2)
        trash = CLIENT / ".repo/repo-worktrees/.trash"
        if trash.is_dir() and any(trash.iterdir()):
            raise Finding(f"trash not emptied: {sorted(p.name for p in trash.iterdir())}")
        pool = grw_json("pool", "list")["result"]
        capacity = pool["capacity"]
        for project in pool["projects"]:
            if project["spares"] > capacity:
                raise Finding(f"pool over capacity: {json.dumps(project)}")
        for spare in pool["spares"]:
            spare_path = Path(spare["path"])
            status = git(spare_path, "status", "--porcelain", "--untracked-files=all")
            if status:
                raise Finding(f"dirty spare {spare_path}:\n{status}")
            source = CLIENT / spare["project"]
            listing = git(source, "worktree", "list", "--porcelain")
            if str(spare_path.resolve()) not in listing and str(spare_path) not in listing:
                raise Finding(f"spare {spare_path} is not registered:\n{listing}")

    # -- work inside trees -----------------------------------------------------

    @rule(tree=trees, path=st.sampled_from(EDITABLE))
    def tree_commit(self, tree, path):
        step(f"tree_commit({tree}, {path})")
        if tree not in self.live or not (self.live[tree] / path).is_dir():
            return
        checkout = self.live[tree] / path
        self.count += 1
        (checkout / f"work{self.count}").write_text("x\n")
        git(checkout, "add", ".")
        git(checkout, "commit", "-qm", f"tree {self.count}")
        if git(checkout, "symbolic-ref", "-q", "HEAD", check=False) == "":
            self.unbranched[tree] = True

    @rule(tree=trees, path=st.sampled_from(EDITABLE))
    def tree_start(self, tree, path):
        step(f"tree_start({tree}, {path})")
        if tree not in self.live or not (self.live[tree] / path).is_dir():
            return
        self.count += 1
        run(["repo", "start", f"topic{self.count}", path], self.live[tree])
        self.branches[tree].add(path)

    @rule(tree=trees)
    def tree_sync(self, tree):
        step(f"tree_sync({tree})")
        if tree not in self.live or tree in self.dead:
            return
        root = self.live[tree]
        result = run(["repo", "sync", "-q"], root, check=False)
        reference = run(["repo", "sync", "-q"], WORLD / "ref", check=False)
        if result.returncode != 0:
            if self.unbranched[tree] or self.branches[tree] or reference.returncode != 0:
                return
            raise Finding(f"repo sync failed in {tree} but not in a fresh client:\n{result.stderr[-2000:]}")
        _, ours = repo_list(root)
        _, theirs = repo_list(WORLD / "ref")
        shared = {
            name
            for name, path in [("a", "a"), ("b", "lib/b"), ("pre", "prebuilts/pre"), ("c", "c")]
            if (root / path).is_symlink()
        }
        if ours != theirs - shared:
            raise Finding(f"after repo sync, {tree} lists {sorted(ours)}; a fresh client lists {sorted(theirs)} (shared {sorted(shared)})")
        for name in ["A_README", "A_COPY"]:
            if (root / name).exists() != (WORLD / "ref" / name).exists():
                raise Finding(f"after repo sync, {tree}/{name} exists={ (root / name).exists() } but a fresh client has exists={ (WORLD / 'ref' / name).exists() }")
        for path in EDITABLE:
            checkout = root / path
            if not checkout.is_dir() or not (WORLD / "ref" / path).is_dir():
                continue
            if git(checkout, "symbolic-ref", "-q", "HEAD", check=False) != "":
                continue
            ours_head = git(checkout, "rev-parse", "HEAD")
            ref_head = git(WORLD / "ref" / path, "rev-parse", "HEAD")
            if ours_head != ref_head and not self.unbranched[tree]:
                raise Finding(f"after repo sync, {tree}/{path} is at {ours_head} but a fresh client is at {ref_head}")

    # -- invariants ------------------------------------------------------------

    @invariant()
    def repo_works_in_every_tree(self):
        for name, root in self.live.items():
            if name in self.dead:
                continue
            expected = owned_projects(root)
            result, listed = repo_list(root)
            if result.returncode != 0 or listed != expected:
                raise Finding(
                    f"repo list in {name} gave {sorted(listed)} (exit {result.returncode}), "
                    f"expected {sorted(expected)}:\n{result.stderr[-1500:]}"
                )

    @invariant()
    def tool_state_matches_git(self):
        if not self.live:
            return
        listed = grw_json("list")["result"]["trees"]
        names = {tree["name"]: tree for tree in listed}
        if set(names) != set(self.live):
            raise Finding(f"list shows {sorted(names)}, expected {sorted(self.live)}")
        for name, tree in names.items():
            if name in self.dead:
                if "prebuilts/pre" not in tree.get("overlays_unmounted", []):
                    raise Finding(f"tree {name} has a dead overlay that list does not report: {json.dumps(tree)}")
                continue
            if tree["state"] != "complete" or tree["failed_projects"]:
                raise Finding(f"tree {name}: {json.dumps(tree)}")
            if tree.get("overlays_unmounted"):
                raise Finding(f"tree {name} reports unmounted overlays: {tree['overlays_unmounted']}")
            status = grw_json("status", name)["result"]
            for project in status["projects"]:
                checkout = self.live[name] / project["path"]
                head = git(checkout, "rev-parse", "HEAD")
                if project["head"] != head:
                    raise Finding(f"status says {name}/{project['path']} is at {project['head']}, git says {head}")

    @invariant()
    def no_stale_registrations(self):
        for path in ["a", "lib/b", "prebuilts/pre", "c"]:
            checkout = CLIENT / path
            if not checkout.is_dir():
                continue
            listing = git(checkout, "worktree", "list", "--porcelain")
            name = {"a": "a", "lib/b": "b", "prebuilts/pre": "pre", "c": "c"}[path]
            for block in listing.split("\n\n"):
                if "prunable" not in block:
                    continue
                if MAINTENANCE and ".pool/" in block:
                    continue
                if name not in self.manifest_projects:
                    # repo sync in a tree deletes a project dropped from the
                    # manifest; its registration is pruned when the tree goes.
                    continue
                raise Finding(f"stale worktree registration in {path}:\n{block}")


def describe_binary():
    result = subprocess.run([BIN, "version"], capture_output=True, text=True)
    return result.stdout.strip() or result.stderr.strip()


def main():
    examples = int(sys.argv[1]) if len(sys.argv) > 1 else 50
    steps = int(os.environ.get("GRW_FUZZ_STEPS", "10"))
    version = describe_binary()
    modes = f"overlay={OVERLAY} maintenance={MAINTENANCE} crash={CRASH}"
    print(f"{version}; {modes}", flush=True)
    BASE.mkdir(parents=True, exist_ok=True)
    if not TEMPLATE.exists():
        build_template()
    FINDINGS.mkdir(parents=True, exist_ok=True)
    started = time.time()
    try:
        run_state_machine_as_test(
            TreeWorld,
            settings=settings(
                max_examples=examples,
                stateful_step_count=steps,
                deadline=None,
                suppress_health_check=list(HealthCheck),
                database=None,
            ),
        )
    except Exception as error:
        failures = list(error.exceptions) if isinstance(error, BaseExceptionGroup) else [error]
        stamp = time.strftime("%Y%m%d-%H%M%S")
        for index, failure in enumerate(failures):
            path = FINDINGS / f"{stamp}-w{WORKER}-{index}.sh"
            notes = "\n".join(getattr(failure, "__notes__", []))
            path.write_text(
                f"# {version}; {modes}\n# {type(failure).__name__}: "
                + "\n# ".join(str(failure).splitlines()[:12])
                + "\n#\n# Hypothesis' minimal case:\n# "
                + notes.replace("\n", "\n# ")
                + "\n\n"
                + "\n".join(getattr(failure, "log", LOG))
                + "\n"
            )
            print(f"FINDING {index}: {str(failure).splitlines()[0][:200]}\n  reproduction: {path}")
        print(f"after {time.time() - started:.0f}s")
        sys.exit(1)
    print(f"no findings in {examples} examples ({time.time() - started:.0f}s)")
    print("steps run:", json.dumps(dict(sorted(STATS.items(), key=lambda item: -item[1]))))


if __name__ == "__main__":
    main()
