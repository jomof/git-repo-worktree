"""Stateful fuzzing of git-repo-worktree against real repo clients.

A small world is driven by random sequences of upstream, manifest, source,
tree, review, and fault operations; invariants are checked after every step,
and `repo sync` in a tree is compared with the same sync in a reference
client. Hypothesis shrinks a failing sequence, and its commands are written to
the findings directory.

The world: projects `a` (copyfile and linkfile), `lib/b` with `lib/b/nested`
inside it, `prebuilts/pre` (clone-depth, shared or overlaid), `hooks` (the
manifest's repo-hooks project, whose pre-upload hook records what it was
given), `sub` (with a submodule), `extra/grouped` (notdefault group, from an
included manifest), and `c` (added and removed by manifest changes). Remotes
have a review URL whose pushes land in local bare "gerrit" repositories.

    GRW_BIN=target/debug/git-repo-worktree python fuzz/harness.py [examples]

Modes (environment):
    GRW_FUZZ_WORLD        plain (default), partial (blob:none partial clones),
                          or worktree (`repo init --worktree`)
    GRW_FUZZ_OVERLAY      auto (default; on where fuse-overlayfs works), 1, 0
    GRW_FUZZ_MAINTENANCE  1 to let background pool maintenance run
    GRW_FUZZ_CRASH        1 to crash add and move at the tool's crash hooks
                          (needs a binary built with --features test-hooks)
    GRW_FUZZ_KILL         1 to kill the tool at random moments (outcomes are
                          timing-dependent; Hypothesis may report flakiness)
    GRW_FUZZ_FAULTS       1 for read-only projects and unreachable remotes
    GRW_FUZZ_DISK         1 to fill the disk (only on a small dedicated
                          filesystem such as a tmpfs GRW_FUZZ_BASE)
    GRW_FUZZ_OLD_BINS     name=path,... older releases that create trees
    GRW_FUZZ_STEPS        steps per sequence (default 10)
    GRW_FUZZ_FINDINGS     where reproductions go (default fuzz/findings)
"""

import hashlib
import json
import os
import shutil
import signal
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
    multiple,
    precondition,
    rule,
    run_state_machine_as_test,
)

HERE = Path(__file__).resolve().parent
BIN = os.path.abspath(os.environ.get("GRW_BIN", "target/debug/git-repo-worktree"))
REPO_SRC = os.environ.get("GRW_TEST_REPO_URL", "/tmp/git-repo-src")
WORKER = os.environ.get("GRW_FUZZ_WORKER", "0")
VARIANT = os.environ.get("GRW_FUZZ_WORLD", "plain")
BASE = Path(os.environ.get("GRW_FUZZ_BASE", f"/tmp/grw-fuzz-{WORKER}")).resolve()
FINDINGS = Path(os.environ.get("GRW_FUZZ_FINDINGS", HERE / "findings"))
WORLD = BASE / "w"
CLIENT = WORLD / "client"
REF = WORLD / "ref"
TEMPLATE = BASE / f"template-{VARIANT}.tar"
MAINTENANCE = os.environ.get("GRW_FUZZ_MAINTENANCE") == "1"
CRASH = os.environ.get("GRW_FUZZ_CRASH") == "1"
KILL = os.environ.get("GRW_FUZZ_KILL") == "1"
FAULTS = os.environ.get("GRW_FUZZ_FAULTS") == "1"
DISK = os.environ.get("GRW_FUZZ_DISK") == "1"
OLD_BINS = dict(
    item.split("=", 1) for item in os.environ.get("GRW_FUZZ_OLD_BINS", "").split(",") if "=" in item
)

PATHS = {
    "a": "a",
    "b": "lib/b",
    "nested": "lib/b/nested",
    "pre": "prebuilts/pre",
    "hooks": "hooks",
    "sub": "sub",
    "grouped": "extra/grouped",
    "c": "c",
}
EDITABLE = ["a", "lib/b", "lib/b/nested"]
GERRIT_HOST = "http://gerrit.invalid/"


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


def git_config_env(pairs):
    env = {"GIT_CONFIG_COUNT": str(len(pairs))}
    for index, (key, value) in enumerate(pairs):
        env[f"GIT_CONFIG_KEY_{index}"] = key
        env[f"GIT_CONFIG_VALUE_{index}"] = value
    return env


BASE_CONFIG = [("protocol.file.allow", "always"), ("advice.detachedHead", "false")]
UPLOAD_CONFIG = BASE_CONFIG + [(f"url.file://{WORLD}/gerrit/.insteadOf", GERRIT_HOST)]
ENV = {name: value for name, value in os.environ.items() if not name.startswith("GIT_REPO_WORKTREE_")}
ENV.update(
    {
        "GIT_AUTHOR_NAME": "fuzz",
        "GIT_AUTHOR_EMAIL": "fuzz@example.com",
        "GIT_COMMITTER_NAME": "fuzz",
        "GIT_COMMITTER_EMAIL": "fuzz@example.com",
        "GRW_TEST_POOL_RESERVE_BYTES": "0",
        "GRW_FUZZ_HOOK_LOG": str(WORLD / "hook.log"),
        "REPO_IGNORE_SSH_INFO": "1",
        **git_config_env(BASE_CONFIG),
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
    STATS[description.split("(")[0]] = STATS.get(description.split("(")[0], 0) + 1


def run(args, cwd, check=True, env=None):
    """Runs a command, logging it as shell for the reproduction."""
    shown = " ".join(str(arg) for arg in args).replace(BIN, "grw")
    prefix = "".join(f"{key}={value} " for key, value in (env or {}).items())
    LOG.append(f"(cd {cwd} && {prefix}{shown})")
    result = subprocess.run(
        [str(arg) for arg in args], cwd=cwd, env={**ENV, **(env or {})}, capture_output=True, text=True
    )
    if check and result.returncode != 0:
        raise Finding(f"{shown} in {cwd} exited {result.returncode}:\n{result.stderr[-2000:]}")
    return result


def no_panic(result, what):
    if result.returncode == 101 or "panicked at" in result.stderr:
        raise Finding(f"{what} panicked:\n{result.stderr[-2000:]}")


def grw(*args, check=True, env=None, binary=None):
    result = run([binary or BIN, "-C", CLIENT, *args], WORLD, check=False, env=env)
    no_panic(result, f"grw {' '.join(args)}")
    if check and result.returncode != 0:
        raise Finding(f"grw {' '.join(args)} exited {result.returncode}:\n{result.stderr[-2000:]}")
    return result


def grw_json(*args, check=True, env=None, binary=None):
    result = grw(*args, "--json", check=False, env=env, binary=binary)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        raise Finding(
            f"grw {' '.join(args)} printed no JSON (exit {result.returncode}):\n{result.stdout}\n{result.stderr[-2000:]}"
        )
    if check and not value["ok"]:
        raise Finding(f"grw {' '.join(args)} failed: {json.dumps(value['error'])}\n{result.stderr[-2000:]}")
    return value


def git(cwd, *args, check=True, env=None):
    return run(["git", *args], cwd, check, env).stdout.strip()


# -- world construction ---------------------------------------------------------

HOOK = '''import json, os, subprocess

def main(project_list, worktree_list=None, **kwargs):
    with open(os.environ["GRW_FUZZ_HOOK_LOG"], "a") as log:
        for project, worktree in zip(project_list, worktree_list or []):
            head = subprocess.run(["git", "-C", worktree, "rev-parse", "HEAD"],
                                  capture_output=True, text=True).stdout.strip()
            log.write(json.dumps({"project": project, "worktree": os.path.realpath(worktree),
                                  "head": head}) + "\\n")
'''


def manifest_text(projects, linkfile):
    link = '<linkfile src="README" dest="A_README"/>' if linkfile else ""
    lines = {
        "a": f'  <project name="a" path="a">{link}<copyfile src="README" dest="A_COPY"/></project>\n',
        "b": '  <project name="b" path="lib/b"/>\n',
        "nested": '  <project name="nested" path="lib/b/nested"/>\n',
        "pre": '  <project name="pre" path="prebuilts/pre" clone-depth="1"/>\n',
        "hooks": '  <project name="hooks" path="hooks"/>\n',
        "sub": '  <project name="sub" path="sub"/>\n',
        "c": '  <project name="c" path="c"/>\n',
    }
    body = "".join(lines[name] for name in lines if name in projects)
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<manifest>
  <remote name="origin" fetch="file://{WORLD}/remotes" review="{GERRIT_HOST}"/>
  <default remote="origin" revision="main"/>
{body}  <repo-hooks in-project="hooks" enabled-list="pre-upload"/>
  <include name="extra.xml"/>
</manifest>
"""


INITIAL = {"a", "b", "nested", "pre", "hooks", "sub"}


def build_template():
    if WORLD.exists():
        shutil.rmtree(WORLD)
    (WORLD / "remotes").mkdir(parents=True)
    (WORLD / "gerrit").mkdir()
    names = ["a", "b", "nested", "pre", "hooks", "subtarget", "sub", "grouped", "c", "manifest"]
    for name in names:
        remote = WORLD / "remotes" / f"{name}.git"
        git(WORLD, "init", "-q", "--bare", "-b", "main", remote)
        git(remote, "config", "uploadpack.allowFilter", "true")
        git(remote, "config", "uploadpack.allowAnySHA1InWant", "true")
        git(WORLD, "init", "-q", "--bare", "-b", "main", WORLD / "gerrit" / f"{name}.git")
        seed = WORLD / "seed" / name
        seed.mkdir(parents=True)
        git(seed, "init", "-q", "-b", "main")
        if name == "manifest":
            (seed / "default.xml").write_text(manifest_text(INITIAL, True))
            (seed / "extra.xml").write_text(
                '<manifest>\n  <project name="grouped" path="extra/grouped" groups="notdefault"/>\n</manifest>\n'
            )
        else:
            (seed / "README").write_text(f"{name} 0\n")
            if name == "hooks":
                (seed / "pre-upload.py").write_text(HOOK)
            if name == "sub":
                git(seed, "submodule", "add", "-q", f"file://{WORLD}/remotes/subtarget.git", "lib")
        git(seed, "add", ".")
        git(seed, "commit", "-qm", "init")
        git(seed, "push", "-q", remote, "main")
    for client in [CLIENT, REF]:
        client.mkdir()
        init = ["repo", "init", "-q", "-u", f"file://{WORLD}/remotes/manifest.git", "-b", "main",
                "--repo-url", REPO_SRC, "--repo-rev", "main", "--no-repo-verify", "-g", "default,notdefault"]
        if VARIANT == "partial":
            init += ["--partial-clone", "--clone-filter=blob:none"]
        elif VARIANT == "worktree":
            init += ["--worktree"]
        run(init, client)
        run(["repo", "sync", "-q"], client)
    with tarfile.open(TEMPLATE, "w") as archive:
        archive.add(WORLD, arcname="w")


def unmount_all():
    if not OVERLAY:
        return
    mounts = Path("/proc/self/mountinfo").read_text().splitlines()
    points = [line.split(" ")[4] for line in mounts if str(BASE) in line.split(" ")[4]]
    for point in sorted(points, key=len, reverse=True):
        subprocess.run(["fusermount3", "-uz", point], capture_output=True)


BACKGROUND = ("pool maintain", "pool empty-trash")


def stop_background():
    for command in BACKGROUND:
        subprocess.run(["pkill", "-9", "-f", "--", f"{CLIENT} -q {command}"], capture_output=True)


def fresh_world():
    stop_background()
    unmount_all()
    for victim in [WORLD, *BASE.glob("t[0-9]*"), *BASE.glob("t-moved-*"), *BASE.glob(".git-repo-worktree-trash-*"), BASE / "filler"]:
        if victim.exists():
            subprocess.run(["chmod", "-R", "u+w", victim], capture_output=True)
            if victim.is_dir():
                shutil.rmtree(victim, ignore_errors=True)
            else:
                victim.unlink()
    with tarfile.open(TEMPLATE) as archive:
        archive.extractall(BASE, filter="fully_trusted")


def source_snapshot():
    snapshot = {}
    for path in PATHS.values():
        checkout = CLIENT / path
        if (checkout / ".git").exists():
            snapshot[path] = (
                git(checkout, "rev-parse", "HEAD"),
                git(checkout, "status", "--porcelain", "--untracked-files=all", "--ignore-submodules=all"),
            )
    return snapshot


def maintenance_running():
    for command in BACKGROUND:
        result = subprocess.run(["pgrep", "-f", "--", f"{CLIENT} -q {command}"], capture_output=True)
        if result.returncode > 1:
            raise RuntimeError(f"pgrep failed: {result.stderr!r}")
        if result.returncode == 0:
            return True
    return False


def owned_projects(root):
    """Projects the tree checks out itself: real directories with a .git."""
    return {
        name
        for name, path in PATHS.items()
        if (root / path).is_dir() and not (root / path).is_symlink() and os.path.lexists(root / path / ".git")
    }


def repo_list(root):
    result = run(["repo", "list", "-n"], root, check=False)
    return result, set(result.stdout.split())


def rebasing(checkout):
    return any(
        Path(checkout, git(checkout, "rev-parse", "--git-path", name)).exists()
        for name in ("rebase-merge", "rebase-apply")
    )


def fingerprint(root, paths):
    """Each project's HEAD, branch, exact index, and every non-ignored file with
    its contents and executable bit: what `restore` must reproduce."""
    prints = {}
    for path in paths:
        checkout = root / path
        if checkout.is_symlink() or not (checkout / ".git").exists():
            continue
        listing = run(["git", "ls-files", "-z", "-c", "-o", "--exclude-standard"], checkout, check=False).stdout
        files = {}
        for name in sorted(set(filter(None, listing.split("\0")))):
            target = checkout / name
            if target.is_symlink():
                files[name] = ("link", os.readlink(target))
            elif target.is_file():
                files[name] = (hashlib.sha1(target.read_bytes()).hexdigest(), os.access(target, os.X_OK))
            elif target.is_dir():
                files[name] = ("dir",)
            else:
                files[name] = ("missing",)
        prints[path] = {
            "head": git(checkout, "rev-parse", "HEAD", check=False),
            "branch": git(checkout, "symbolic-ref", "-q", "--short", "HEAD", check=False),
            "index": git(checkout, "ls-files", "--stage", check=False),
            "files": files,
        }
    return prints


def archive_ref_ids():
    """Ids of archive refs in every source project and the manifest repository."""
    ids = set()
    repositories = [CLIENT / path for path in PATHS.values() if (CLIENT / path / ".git").exists()]
    repositories.append(CLIENT / ".repo" / "manifests")
    for repository in repositories:
        refs = git(repository, "for-each-ref", "--format=%(refname)", "refs/repo-worktree/archive/", check=False)
        ids.update(line.rsplit("/", 1)[1] for line in refs.splitlines() if line)
    return ids


ACCEPTED_ARCHIVE_ITEMS = {"in-progress", "conflicts", "submodule-changes", "unmanaged-project", "embedded-repository"}


def current_layout_problems(root):
    """Ways a bridged tree's .repo differs from what this build creates."""
    problems = []
    repo_dir = root / ".repo"
    objects = repo_dir / ("worktrees" if VARIANT == "worktree" else "project-objects")
    if objects.is_symlink():
        problems.append(f"{objects.name} is a whole-directory link")
    if not (repo_dir / "copy-link-files.json").is_file():
        problems.append("copy-link-files.json is missing")
    if not (repo_dir / "local_manifests" / "zz-git-repo-worktree.xml").is_file():
        problems.append("the generated local manifest is missing")
    return problems


class TreeWorld(RuleBasedStateMachine):
    trees = Bundle("trees")
    archives = Bundle("archives")

    def __init__(self):
        super().__init__()
        LOG.clear()
        fresh_world()
        self.count = 0
        self.live = {}
        self.unbranched = {}
        self.branches = {}
        self.dirty_readme = {}
        self.dead = set()
        self.legacy = set()
        self.archived = {}
        self.fault = None
        self.manifest_projects = set(INITIAL)
        self.linkfile = True

    def teardown(self):
        self.clear_fault()
        stop_background()
        unmount_all()

    # -- helpers -----------------------------------------------------------------

    def tool_step(self, operation):
        if self.fault and self.fault[0] == "disk":
            return operation()
        before = source_snapshot()
        result = operation()
        after = source_snapshot()
        if before != after and not self.fault:
            raise Finding(f"the source client changed:\nbefore {before}\nafter {after}")
        return result

    def new_tree(self, name, path):
        self.live[name] = path
        self.unbranched[name] = False
        self.branches[name] = {}
        self.dirty_readme[name] = False

    def checked(self, name):
        return name in self.live and name not in self.dead and name not in self.legacy

    def recover(self):
        """Brings every registered tree back to a usable state after a fault."""
        trees = grw_json("list")["result"]["trees"]
        for tree in trees:
            name = tree["name"]
            if tree["state"] == "removing":
                grw_json("remove", name)
                continue
            if tree["state"] in ("creating", "incomplete") and name not in self.live:
                value = grw_json("add", "--continue", name, check=False)
                if not value["ok"]:
                    grw_json("remove", "--force", "--force", "--discard", name)
                    continue
                self.new_tree(name, Path(tree["path"]))
            elif name not in self.live:
                self.new_tree(name, Path(tree["path"]))
        registered = {tree["name"] for tree in grw_json("list")["result"]["trees"]}
        for name in list(self.live):
            if name not in registered:
                del self.live[name]
        if self.live:
            grw_json("repair")
            self.dead.clear()
            self.legacy.clear()
        trees = {tree["name"]: tree for tree in grw_json("list")["result"]["trees"]}
        for name in self.live:
            self.live[name] = Path(trees[name]["path"])

    def clear_fault(self):
        fault, self.fault = self.fault, None
        if fault is None:
            return
        kind, target = fault
        if kind == "readonly":
            subprocess.run(["chmod", "-R", "u+w", target], capture_output=True)
        elif kind == "unreachable":
            if Path(target + ".away").exists():
                os.rename(target + ".away", target)
        elif kind == "disk":
            Path(target).unlink(missing_ok=True)

    # -- upstream, manifest, and source actors ---------------------------------

    @rule(name=st.sampled_from(["a", "b", "nested", "pre", "c", "grouped"]))
    def upstream_commit(self, name):
        step(f"upstream_commit({name})")
        if self.fault and self.fault[0] in ("disk", "unreachable"):
            return
        seed = WORLD / "seed" / name
        self.count += 1
        (seed / "README").write_text(f"{name} {self.count}\n")
        git(seed, "commit", "-qam", f"upstream {self.count}")
        git(seed, "push", "-q", WORLD / "remotes" / f"{name}.git", "main")

    @rule(change=st.sampled_from(["add c", "remove c", "remove b", "restore b", "toggle linkfile"]))
    def manifest_change(self, change):
        step(f"manifest_change({change})")
        if self.fault and self.fault[0] in ("disk", "unreachable"):
            return
        projects, linkfile = set(self.manifest_projects), self.linkfile
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
        self.manifest_projects, self.linkfile = projects, linkfile

    @rule()
    def source_sync(self):
        step("source_sync()")
        if self.fault and self.fault[0] == "disk":
            return
        run(["repo", "sync", "-q"], CLIENT, check=False)

    @rule(path=st.sampled_from(EDITABLE))
    def source_commit(self, path):
        step(f"source_commit({path})")
        if self.fault and self.fault[0] == "disk":
            return
        checkout = CLIENT / path
        if not (checkout / ".git").exists():
            return
        self.count += 1
        (checkout / f"local{self.count}").write_text("x\n")
        git(checkout, "add", f"local{self.count}")
        git(checkout, "commit", "-qm", f"source {self.count}")

    @rule(path=st.sampled_from(["prebuilts/pre", "a"]), remove=st.booleans())
    def source_untracked(self, path, remove):
        step(f"source_untracked({path}, remove={remove})")
        if self.fault and self.fault[0] == "disk":
            return
        directory = CLIENT / path / "environments"
        if not (CLIENT / path / ".git").exists():
            return
        if remove:
            shutil.rmtree(directory, ignore_errors=True)
            return
        directory.mkdir(exist_ok=True)
        self.count += 1
        (directory / f"state{self.count}").write_text("runtime\n")

    # -- tool operations -------------------------------------------------------

    @initialize(target=trees)
    def first_tree(self):
        return self.add(False)

    @rule(target=trees, submodules=st.booleans(), from_source=st.booleans())
    def add(self, submodules, from_source=False):
        step(f"add(submodules={submodules}, from_source={from_source})")
        self.count += 1
        name = f"t{self.count}"
        path = BASE / name
        args = ["add", "--name", name, str(path)] + (["--submodules"] if submodules else [])
        args += ["--from-source"] if from_source else []
        if self.fault:
            value = self.tool_step(lambda: grw_json(*args, check=False))
            if value["ok"] or value.get("exit_code") == 3:
                self.new_tree(name, path)
            return name
        self.tool_step(lambda: grw_json(*args))
        self.new_tree(name, path)
        if submodules and (path / "sub").is_dir() and not (path / "sub").is_symlink():
            status = git(path / "sub", "submodule", "status")
            if status.startswith("-"):
                raise Finding(f"add --submodules left the submodule uninitialized:\n{status}")
        return name

    @precondition(lambda self: bool(OLD_BINS))
    @rule(target=trees, version=st.sampled_from(sorted(OLD_BINS) or ["none"]))
    def add_with_old_release(self, version):
        step(f"add_with_old_release({version})")
        self.count += 1
        name = f"t{self.count}"
        path = BASE / name
        result = grw("add", "--name", name, str(path), check=False, binary=OLD_BINS[version])
        registered = {tree["name"] for tree in grw_json("list")["result"]["trees"]}
        if name in registered:
            self.new_tree(name, path)
            self.legacy.add(name)
        elif result.returncode == 0:
            raise Finding(f"{version} created {name}, but this build does not list it")
        return name

    @rule(tree=trees)
    def upgrade(self, tree):
        step(f"upgrade({tree})")
        if tree not in self.live or self.fault:
            return
        self.tool_step(lambda: grw_json("repair", tree))
        self.legacy.discard(tree)
        self.dead.discard(tree)
        problems = current_layout_problems(self.live[tree])
        if problems:
            raise Finding(f"repair left {tree} with an old layout: {problems}")

    @precondition(lambda self: CRASH)
    @rule(target=trees, after=st.integers(1, 5))
    def add_crash_and_continue(self, after):
        step(f"add_crash_and_continue(after={after})")
        self.count += 1
        name = f"t{self.count}"
        path = BASE / name
        crashed = grw("add", "--name", name, str(path), check=False, env={"GRW_TEST_CRASH_AFTER": str(after)})
        if crashed.returncode != 0 and not self.fault:
            self.tool_step(lambda: grw_json("add", "--continue", name))
        if crashed.returncode == 0 or not self.fault:
            self.new_tree(name, path)
        return name

    def archive_state(self, tree):
        """The tree's projects as `restore` must bring them back."""
        if not self.checked(tree) or self.fault:
            return None
        status = grw_json("status", tree, check=False)
        if not status["ok"]:
            return None
        paths = [project["path"] for project in status["result"]["projects"]]
        return {
            "name": tree,
            "path": self.live[tree],
            "prints": fingerprint(self.live[tree], paths),
            "unbranched": self.unbranched[tree],
            "branches": dict(self.branches[tree]),
            "dirty_readme": self.dirty_readme[tree],
        }

    def check_incomplete(self, tree, error):
        """An archive refusal must name only state an archive cannot hold."""
        for item in error.get("details", {}).get("items", []):
            if item["kind"] not in ACCEPTED_ARCHIVE_ITEMS:
                raise Finding(f"remove {tree} could not archive {item}")
            if item["kind"] in ("in-progress", "conflicts") and not rebasing(self.live[tree] / item["project"]):
                raise Finding(f"remove {tree} reported {item} with no operation in progress")

    def forget(self, tree):
        del self.live[tree]
        self.dead.discard(tree)
        self.legacy.discard(tree)

    @rule(target=archives, tree=consumes(trees), mode=st.sampled_from(["plain", "force", "discard"]))
    def remove(self, tree, mode):
        step(f"remove({tree}, {mode})")
        if tree not in self.live:
            return multiple()
        state = self.archive_state(tree) if mode != "discard" else None
        args = ["remove", tree] + {"plain": [], "force": ["--force"], "discard": ["--discard"]}[mode]
        value = self.tool_step(lambda: grw_json(*args, check=False))
        if not value["ok"] and value["error"]["code"] == "E_ARCHIVE_INCOMPLETE" and mode == "plain" and not self.fault:
            self.check_incomplete(tree, value["error"])
            mode = "force"
            value = self.tool_step(lambda: grw_json("remove", tree, "--force", check=False))
        if value["ok"]:
            self.forget(tree)
            archive = value["result"].get("archive")
            if mode == "discard":
                if archive is not None:
                    raise Finding(f"remove --discard {tree} archived it: {archive}")
                return multiple()
            if archive is None:
                raise Finding(f"remove {tree} succeeded without an archive")
            if state is not None:
                state["dropped"] = {item["project"] for item in archive["dropped"]}
                self.archived[archive["id"]] = state
            return archive["id"]
        if self.fault:
            return multiple()
        raise Finding(f"remove {tree} ({mode}) failed: {json.dumps(value['error'])}")

    @rule(target=trees, archive=consumes(archives))
    def restore(self, archive):
        step(f"restore({archive})")
        state = self.archived.pop(archive, None)
        if self.fault or state is None:
            return multiple()
        name, path = state["name"], state["path"]
        if name in self.live or os.path.lexists(path):
            return multiple()
        value = self.tool_step(lambda: grw_json("restore", archive, check=False))
        if not value["ok"]:
            restored = (value.get("result") or {}).get("restored") or []
            failed = [entry for entry in restored if entry.get("ok") is False]
            gone = [entry for entry in failed if not (CLIENT / entry["path"] / ".git").exists()]
            if value.get("exit_code") == 3 and failed and len(gone) == len(failed):
                grw_json("remove", "--discard", name)
                return multiple()
            raise Finding(f"restore {archive} failed: {json.dumps(value.get('error'))}\n{json.dumps(restored)}")
        if value["result"]["archive"]["kept"]:
            raise Finding(f"restore {archive} succeeded but kept the archive")
        if archive in {entry["id"] for entry in grw_json("archive", "list")["result"]["archives"]}:
            raise Finding(f"restore {archive} left the archive listed")
        self.new_tree(name, path)
        self.unbranched[name] = state["unbranched"]
        self.branches[name] = state["branches"]
        self.dirty_readme[name] = state["dirty_readme"]
        restored = {entry["path"]: entry for entry in value["result"]["restored"]}
        for project, expected in state.get("heads", {}).items():
            if (path / project / ".git").exists():
                head = git(path / project, "rev-parse", "HEAD")
                if head != expected["head"]:
                    raise Finding(f"restore {archive}: {project} is at {head}, its pruned tree was at {expected['head']}")
        after = fingerprint(path, list(state["prints"]))
        for project, before in state["prints"].items():
            if project in state["dropped"]:
                continue
            now = after.get(project)
            if now is None:
                raise Finding(f"restore {archive} did not recreate {project}")
            entry = restored.get(project, {})
            expected = dict(before)
            if before["branch"] and not entry.get("branch_restored"):
                expected["branch"] = ""
                self.unbranched[name] = True
                self.branches[name].pop(project, None)
            for key in ("head", "branch", "index", "files"):
                if now[key] != expected[key]:
                    raise Finding(
                        f"restore {archive}: {project} {key} differs\nbefore {expected[key]}\nafter  {now[key]}\n{json.dumps(entry)}"
                    )
        return name

    @rule(tree=trees, mode=st.sampled_from(["plain", "force"]))
    def round_trip(self, tree, mode):
        step(f"round_trip({tree}, {mode})")
        archive = self.remove(tree, mode)
        if isinstance(archive, str):
            self.restore(archive)

    @rule(archive=consumes(archives))
    def drop_archive(self, archive):
        step(f"drop_archive({archive})")
        self.archived.pop(archive, None)
        if self.fault:
            return
        value = grw_json("archive", "drop", archive, check=False)
        if not value["ok"] and value["error"]["code"] != "E_NO_SUCH_ARCHIVE":
            raise Finding(f"archive drop {archive} failed: {json.dumps(value['error'])}")
        if archive in archive_ref_ids():
            raise Finding(f"archive drop {archive} left its refs behind")

    @precondition(lambda self: not OVERLAY)
    @rule(target=archives, tree=consumes(trees))
    def delete_by_hand_and_prune(self, tree):
        step(f"delete_by_hand_and_prune({tree})")
        if not self.checked(tree) or self.fault:
            return multiple()
        root = self.live[tree]
        heads = {
            project: {"head": prints["head"]}
            for project, prints in fingerprint(root, PATHS.values()).items()
        }
        run(["rm", "-rf", root], BASE)
        value = self.tool_step(lambda: grw_json("prune"))
        pruned = {entry["name"]: entry for entry in value["result"]["pruned"]}
        if tree not in pruned or not pruned[tree].get("archive"):
            raise Finding(f"prune did not archive {tree}: {json.dumps(value['result'])}")
        self.forget(tree)
        archive = pruned[tree]["archive"]
        self.archived[archive] = {
            "name": tree,
            "path": root,
            "prints": {},
            "heads": heads,
            "unbranched": True,
            "branches": {},
            "dirty_readme": True,
            "dropped": set(),
        }
        return archive

    @rule(target=archives)
    def remove_idle(self):
        step("remove_idle()")
        if self.fault or not self.live:
            return multiple()
        states = {tree: self.archive_state(tree) for tree in list(self.live)}
        value = self.tool_step(lambda: grw_json("remove", "--idle", "0s"))
        ids = []
        for entry in value["result"]["removed"]:
            tree = entry["name"]
            self.forget(tree)
            archive = entry["archive"]
            if states.get(tree) is not None:
                states[tree]["dropped"] = set()
                self.archived[archive["id"]] = states[tree]
            ids.append(archive["id"])
        for entry in value["result"]["skipped"]:
            if entry["name"] in self.live and not entry["reason"].startswith("E_ARCHIVE_INCOMPLETE"):
                reason = entry["reason"]
                if reason != "locked" and "process" not in reason and "another command" not in reason:
                    raise Finding(f"remove --idle 0s skipped {entry['name']}: {reason}")
        return multiple(*ids)

    @rule(tree=trees)
    def repair(self, tree):
        step(f"repair({tree})")
        if tree not in self.live:
            return
        value = self.tool_step(lambda: grw_json("repair", tree, check=not self.fault))
        if value["ok"]:
            self.dead.discard(tree)
            self.legacy.discard(tree)

    @precondition(lambda self: self.live)
    @rule()
    def repair_all(self):
        step("repair_all()")
        value = self.tool_step(lambda: grw_json("repair", check=not self.fault))
        if value["ok"]:
            self.dead.clear()
            self.legacy.clear()

    @rule(tree=trees)
    def move(self, tree):
        step(f"move({tree})")
        if tree not in self.live or tree in self.dead:
            return
        self.count += 1
        target = BASE / f"t-moved-{self.count}"
        value = self.tool_step(lambda: grw_json("move", tree, str(target), check=not self.fault))
        if value["ok"]:
            self.live[tree] = target

    @precondition(lambda self: CRASH)
    @rule(tree=trees)
    def move_crash_and_repair(self, tree):
        step(f"move_crash_and_repair({tree})")
        if tree not in self.live or tree in self.dead or self.fault:
            return
        self.count += 1
        target = BASE / f"t-moved-{self.count}"
        grw("move", tree, str(target), check=False, env={"GRW_TEST_CRASH_AFTER_MOVE": "1"})
        self.tool_step(lambda: grw_json("repair", tree))
        trees = {entry["name"]: entry for entry in grw_json("list")["result"]["trees"]}
        self.live[tree] = Path(trees[tree]["path"])
        self.legacy.discard(tree)

    @rule(path=st.sampled_from(EDITABLE), mode=st.sampled_from(["pool", "auto"]))
    def set_default(self, path, mode):
        step(f"set_default({path}, {mode})")
        self.tool_step(lambda: grw_json("defaults", mode, path, check=not self.fault))

    @precondition(lambda self: OVERLAY)
    @rule(tree=trees)
    def kill_overlay(self, tree):
        step(f"kill_overlay({tree})")
        if tree not in self.live:
            return
        overlays = CLIENT / ".repo/repo-worktrees" / tree / "overlays"
        if run(["pkill", "-9", "-f", "--", f"fuse-overlayfs.*{overlays}"], WORLD, check=False).returncode == 0:
            time.sleep(0.3)
            self.dead.add(tree)

    @precondition(lambda self: OVERLAY)
    @rule(tree=trees)
    def repair_mounts(self, tree):
        step(f"repair_mounts({tree})")
        if tree in self.live and not self.fault:
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
        if self.fault:
            return
        trash = CLIENT / ".repo/repo-worktrees/.trash"
        if trash.is_dir() and any(trash.iterdir()):
            raise Finding(f"trash not emptied: {sorted(p.name for p in trash.iterdir())}")
        pool = grw_json("pool", "list")["result"]
        for project in pool["projects"]:
            if project["spares"] > pool["capacity"]:
                raise Finding(f"pool over capacity: {json.dumps(project)}")
        for spare in pool["spares"]:
            spare_path = Path(spare["path"])
            status = git(spare_path, "status", "--porcelain", "--untracked-files=all")
            if status:
                raise Finding(f"dirty spare {spare_path}:\n{status}")
            listing = git(CLIENT / spare["project"], "worktree", "list", "--porcelain")
            if str(spare_path.resolve()) not in listing and str(spare_path) not in listing:
                raise Finding(f"spare {spare_path} is not registered:\n{listing}")

    # -- faults ----------------------------------------------------------------

    @precondition(lambda self: KILL)
    @rule(operation=st.sampled_from(["add", "remove", "repair", "move"]), delay=st.integers(0, 1500))
    def kill_tool(self, operation, delay):
        step(f"kill_tool({operation}, {delay}ms)")
        if self.fault:
            return
        self.count += 1
        target = next(iter(self.live), None)
        if operation == "add":
            args = ["add", "--name", f"t{self.count}", str(BASE / f"t{self.count}")]
        elif target is None:
            return
        elif operation == "remove":
            args = ["remove", "--force", target]
        elif operation == "move":
            args = ["move", target, str(BASE / f"t-moved-{self.count}")]
        else:
            args = ["repair", target]
        shown = " ".join(args)
        LOG.append(f"(cd {WORLD} && grw -C {CLIENT} {shown} & sleep {delay / 1000}; kill -9 $!)")
        process = subprocess.Popen([BIN, "-C", str(CLIENT), *args], cwd=WORLD, env=ENV,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(delay / 1000)
        process.send_signal(signal.SIGKILL)
        process.wait()
        self.recover()

    @precondition(lambda self: FAULTS)
    @rule(tree=trees, path=st.sampled_from(EDITABLE))
    def make_readonly(self, tree, path):
        step(f"make_readonly({tree}, {path})")
        if self.fault or tree not in self.live or not (self.live[tree] / path).is_dir():
            return
        target = str(self.live[tree] / path)
        subprocess.run(["chmod", "-R", "a-w", target], capture_output=True)
        self.fault = ("readonly", target)

    @precondition(lambda self: FAULTS)
    @rule(name=st.sampled_from(["a", "b", "manifest"]))
    def make_remote_unreachable(self, name):
        step(f"make_remote_unreachable({name})")
        if self.fault:
            return
        target = str(WORLD / "remotes" / f"{name}.git")
        os.rename(target, target + ".away")
        self.fault = ("unreachable", target)

    @precondition(lambda self: DISK)
    @rule()
    def fill_disk(self):
        step("fill_disk()")
        if self.fault:
            return
        filler = BASE / "filler"
        with open(filler, "wb") as handle:
            chunk = b"\0" * (1 << 20)
            try:
                while True:
                    handle.write(chunk)
                    handle.flush()
            except OSError:
                pass
        self.fault = ("disk", str(filler))

    @precondition(lambda self: self.fault is not None)
    @rule()
    def clear_fault_and_recover(self):
        step("clear_fault_and_recover()")
        self.clear_fault()
        self.recover()

    # -- work inside trees -----------------------------------------------------

    @rule(tree=trees, path=st.sampled_from(EDITABLE), readme=st.booleans())
    def tree_commit(self, tree, path, readme):
        step(f"tree_commit({tree}, {path}, readme={readme})")
        if not self.checked(tree) or self.fault or not (self.live[tree] / path / ".git").exists():
            return
        checkout = self.live[tree] / path
        if rebasing(checkout):
            return
        self.count += 1
        name = "README" if readme else f"work{self.count}"
        (checkout / name).write_text(f"tree {self.count}\n")
        git(checkout, "add", name)
        git(checkout, "commit", "-qm", f"tree {self.count}")
        if git(checkout, "symbolic-ref", "-q", "HEAD", check=False) == "":
            self.unbranched[tree] = True

    @rule(tree=trees, path=st.sampled_from(EDITABLE))
    def tree_start(self, tree, path):
        step(f"tree_start({tree}, {path})")
        if not self.checked(tree) or self.fault or not (self.live[tree] / path / ".git").exists():
            return
        self.count += 1
        branch = f"topic{self.count}"
        run(["repo", "start", branch, path], self.live[tree])
        self.branches[tree][path] = branch

    @rule(tree=trees, rebase=st.booleans())
    def tree_sync(self, tree, rebase):
        step(f"tree_sync({tree}, rebase={rebase})")
        if not self.checked(tree) or self.fault:
            return
        root = self.live[tree]
        result = run(["repo", "sync", "-q"] + (["--rebase"] if rebase else []), root, check=False)
        reference = run(["repo", "sync", "-q"], REF, check=False)
        if result.returncode != 0:
            self.dirty_readme[tree] = True
            if self.unbranched[tree] or self.branches[tree] or reference.returncode != 0:
                return
            raise Finding(f"repo sync failed in {tree} but not in a fresh client:\n{result.stderr[-2000:]}")
        if reference.returncode != 0:
            return
        _, ours = repo_list(root)
        _, theirs = repo_list(REF)
        shared = {name for name, path in PATHS.items() if (root / path).is_symlink()}
        if ours != theirs - shared:
            raise Finding(f"after repo sync, {tree} lists {sorted(ours)}; a fresh client lists {sorted(theirs)} (shared {sorted(shared)})")
        for name in ["A_README", "A_COPY"]:
            if os.path.lexists(root / name) != os.path.lexists(REF / name):
                raise Finding(f"after repo sync, {tree}/{name} exists={os.path.lexists(root / name)}; a fresh client: {os.path.lexists(REF / name)}")
        for path in EDITABLE:
            checkout, reference_checkout = root / path, REF / path
            if not (checkout / ".git").exists() or not (reference_checkout / ".git").exists():
                continue
            if git(checkout, "symbolic-ref", "-q", "HEAD", check=False) != "":
                continue
            ours_head = git(checkout, "rev-parse", "HEAD")
            ref_head = git(reference_checkout, "rev-parse", "HEAD")
            if ours_head != ref_head and not self.unbranched[tree]:
                raise Finding(f"after repo sync, {tree}/{path} is at {ours_head}; a fresh client is at {ref_head}")

    @rule(tree=trees, how=st.sampled_from(["theirs", "ours", "abort"]))
    def resolve_conflicts(self, tree, how):
        step(f"resolve_conflicts({tree}, {how})")
        if not self.checked(tree) or self.fault:
            return
        for path in EDITABLE:
            checkout = self.live[tree] / path
            if not (checkout / ".git").exists() or not rebasing(checkout):
                continue
            if how == "abort":
                git(checkout, "rebase", "--abort")
                continue
            for _ in range(10):
                conflicted = git(checkout, "diff", "--name-only", "--diff-filter=U").split()
                for name in conflicted:
                    git(checkout, "checkout", f"--{how}", "--", name)
                    git(checkout, "add", name)
                result = run(["git", "-c", "core.editor=true", "rebase", "--continue"], checkout, check=False)
                if result.returncode == 0:
                    break
                if "nothing to commit" in result.stdout + result.stderr:
                    git(checkout, "rebase", "--skip", check=False)
                    break

    @rule(tree=trees, path=st.sampled_from(EDITABLE))
    def tree_upload(self, tree, path):
        step(f"tree_upload({tree}, {path})")
        branch = self.branches.get(tree, {}).get(path)
        if not self.checked(tree) or self.fault or branch is None:
            return
        root = self.live[tree]
        checkout = root / path
        if git(checkout, "symbolic-ref", "-q", "--short", "HEAD", check=False) != branch:
            return
        if git(checkout, "rev-list", "--count", f"{branch}@{{upstream}}..{branch}", check=False) in ("", "0"):
            return
        log = WORLD / "hook.log"
        log.unlink(missing_ok=True)
        result = run(["repo", "upload", "--yes", "--verify", f"--br={branch}", "-t", path], root,
                     check=False, env=git_config_env(UPLOAD_CONFIG))
        if result.returncode != 0:
            raise Finding(f"repo upload of {branch} in {tree}/{path} failed:\n{result.stdout[-1000:]}\n{result.stderr[-2000:]}")
        name = next(name for name, candidate in PATHS.items() if candidate == path)
        head = git(checkout, "rev-parse", branch)
        refs = git(WORLD / "gerrit" / f"{name}.git", "for-each-ref", "--format=%(objectname) %(refname)")
        if not any(line.startswith(head) and "refs/for/main" in line for line in refs.splitlines()):
            raise Finding(f"repo upload of {tree}/{path} did not push {head}; gerrit has:\n{refs}")
        entries = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        mine = [entry for entry in entries if entry["project"] == name]
        if not mine:
            raise Finding(f"the pre-upload hook did not run for {name}: {entries}")
        if Path(mine[-1]["worktree"]) != checkout.resolve() or mine[-1]["head"] != head:
            raise Finding(f"the pre-upload hook saw {mine[-1]}, expected {checkout.resolve()} at {head}")

    @rule(tree=trees, owner=trees)
    def abandon_shared_branch(self, tree, owner):
        step(f"abandon_shared_branch({tree}, {owner})")
        if not self.checked(tree) or self.fault or not self.branches.get(owner):
            return
        path, branch = next(iter(self.branches[owner].items()))
        run(["repo", "abandon", branch], self.live[tree], check=False)
        if owner in self.live:
            self.unbranched[owner] = True
            checkout = self.live[owner] / path
            exists = (checkout / ".git").exists() and run(
                ["git", "rev-parse", "-q", "--verify", f"refs/heads/{branch}"], checkout, check=False
            ).returncode == 0
            if not exists:
                self.branches[owner].pop(path, None)

    # -- invariants ------------------------------------------------------------

    @invariant()
    def repo_works_in_every_tree(self):
        if self.fault:
            return
        for name, root in self.live.items():
            if not self.checked(name):
                continue
            # A project dropped from the manifest can linger (repo refuses to
            # delete a checkout with a nested project in it), as in any client.
            declared = self.manifest_projects | {"grouped"}
            expected = owned_projects(root) & declared
            result, listed = repo_list(root)
            listed &= declared
            if result.returncode != 0 or listed != expected:
                raise Finding(
                    f"repo list in {name} gave {sorted(listed)} (exit {result.returncode}), "
                    f"expected {sorted(expected)}:\n{result.stderr[-1500:]}"
                )

    @invariant()
    def tool_state_matches_git(self):
        if not self.live or self.fault:
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
            if name in self.legacy:
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
            for entry in tree["checked_out_branches"]:
                actual = git(self.live[name] / entry["project"], "symbolic-ref", "-q", "--short", "HEAD", check=False)
                if actual != entry["branch"]:
                    raise Finding(f"list says {name}/{entry['project']} has {entry['branch']} checked out; git says {actual!r}")

    @invariant()
    def archive_refs_belong_to_records(self):
        if self.fault:
            return
        directory = CLIENT / ".repo" / "repo-worktrees" / ".archive"
        records = {entry.name for entry in directory.iterdir()} if directory.is_dir() else set()
        orphans = archive_ref_ids() - records
        if orphans:
            raise Finding(f"archive refs without a record: {sorted(orphans)}")

    @invariant()
    def no_stale_registrations(self):
        if self.fault:
            return
        for name, path in PATHS.items():
            checkout = CLIENT / path
            if not (checkout / ".git").exists():
                continue
            listing = git(checkout, "worktree", "list", "--porcelain")
            for block in listing.split("\n\n"):
                if "prunable" not in block:
                    continue
                if MAINTENANCE and ".pool/" in block:
                    continue
                if name not in self.manifest_projects and name in PATHS:
                    continue
                if any(str(root) in block for tree, root in self.live.items() if tree in self.legacy):
                    continue
                raise Finding(f"stale worktree registration in {path}:\n{block}")


def describe_binary():
    result = subprocess.run([BIN, "version"], capture_output=True, text=True)
    return result.stdout.strip() or result.stderr.strip()


def main():
    examples = int(sys.argv[1]) if len(sys.argv) > 1 else 50
    steps = int(os.environ.get("GRW_FUZZ_STEPS", "10"))
    version = describe_binary()
    modes = (
        f"world={VARIANT} overlay={OVERLAY} maintenance={MAINTENANCE} crash={CRASH} "
        f"kill={KILL} faults={FAULTS} disk={DISK} old={','.join(sorted(OLD_BINS)) or '-'}"
    )
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
        print("steps run:", json.dumps(dict(sorted(STATS.items(), key=lambda item: -item[1]))))
        sys.exit(1)
    print(f"no findings in {examples} examples ({time.time() - started:.0f}s)")
    print("steps run:", json.dumps(dict(sorted(STATS.items(), key=lambda item: -item[1]))))


if __name__ == "__main__":
    main()
