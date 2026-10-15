"""Stateful fuzzing of git-repo-worktree against real repo clients.

A tiny world (three projects, a manifest repository, a source client made with
`repo init` + `repo sync`, and a reference client) is driven by random
sequences of upstream, source, and tree operations. After every step the
harness checks invariants, and `repo sync` in a tree is compared with the same
sync in the reference client. Hypothesis shrinks a failing sequence to a
minimal one, whose shell commands are written to `fuzz/findings/`.

    GRW_BIN=target/debug/git-repo-worktree python fuzz/harness.py [examples]
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
    invariant,
    rule,
    run_state_machine_as_test,
)

HERE = Path(__file__).resolve().parent
BIN = os.path.abspath(os.environ.get("GRW_BIN", "target/debug/git-repo-worktree"))
REPO_SRC = os.environ.get("GRW_TEST_REPO_URL", "/tmp/git-repo-src")
WORKER = os.environ.get("GRW_FUZZ_WORKER", "0")
BASE = Path(os.environ.get("GRW_FUZZ_BASE", f"/tmp/grw-fuzz-{WORKER}")).resolve()
WORLD = BASE / "w"
TEMPLATE = BASE / "template.tar"
PROJECTS = {"a": "a", "b": "lib/b", "pre": "prebuilts/pre"}
EDITABLE = ["a", "lib/b"]
ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "fuzz",
    "GIT_AUTHOR_EMAIL": "fuzz@example.com",
    "GIT_COMMITTER_NAME": "fuzz",
    "GIT_COMMITTER_EMAIL": "fuzz@example.com",
    "GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE": "1",
    "GIT_REPO_WORKTREE_NO_OVERLAY": os.environ.get("GIT_REPO_WORKTREE_NO_OVERLAY", "1"),
}
for name in list(ENV):
    if name.startswith("GIT_REPO_WORKTREE_") and name not in (
        "GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE",
        "GIT_REPO_WORKTREE_NO_OVERLAY",
    ):
        del ENV[name]

LOG = []


class Finding(AssertionError):
    """A violated expectation, carrying the commands that led to it."""

    def __init__(self, message):
        super().__init__(message)
        self.log = list(LOG)


def step(description):
    LOG.append(f"# step: {description}")


def run(args, cwd, check=True):
    """Runs a command, logging it as shell for the reproduction."""
    shown = " ".join(str(arg) for arg in args).replace(str(BIN), "grw")
    LOG.append(f"(cd {cwd} && {shown})")
    result = subprocess.run(
        [str(arg) for arg in args], cwd=cwd, env=ENV, capture_output=True, text=True
    )
    if check and result.returncode != 0:
        raise Finding(f"{shown} in {cwd} exited {result.returncode}:\n{result.stderr[-2000:]}")
    return result


def grw(*args, cwd=None, check=True):
    return run([BIN, "-C", WORLD / "client", *args], cwd or WORLD, check)


def grw_json(*args, check=True):
    result = grw(*args, "--json", check=False)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        raise Finding(f"grw {' '.join(args)} printed no JSON:\n{result.stdout}\n{result.stderr}")
    if check and not value["ok"]:
        raise Finding(f"grw {' '.join(args)} failed: {json.dumps(value['error'])}\n{result.stderr}")
    return value


def git(cwd, *args, check=True):
    return run(["git", *args], cwd, check).stdout.strip()


def build_template():
    if WORLD.exists():
        shutil.rmtree(WORLD)
    (WORLD / "remotes").mkdir(parents=True)
    for name in ["a", "b", "pre", "manifest"]:
        git(WORLD, "init", "-q", "--bare", "-b", "main", WORLD / "remotes" / f"{name}.git")
        seed = WORLD / "seed" / name
        seed.mkdir(parents=True)
        git(seed, "init", "-q", "-b", "main")
        if name == "manifest":
            (seed / "default.xml").write_text(
                f"""<?xml version="1.0" encoding="UTF-8"?>
<manifest>
  <remote name="origin" fetch="file://{WORLD}/remotes"/>
  <default remote="origin" revision="main"/>
  <project name="a" path="a"><linkfile src="README" dest="A_README"/><copyfile src="README" dest="A_COPY"/></project>
  <project name="b" path="lib/b"/>
  <project name="pre" path="prebuilts/pre" clone-depth="1"/>
</manifest>
"""
            )
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


def fresh_world():
    if WORLD.exists():
        subprocess.run(["chmod", "-R", "u+w", WORLD], capture_output=True)
        shutil.rmtree(WORLD, ignore_errors=True)
    for pattern in ["t[0-9]*", "t-moved-*"]:
        for stray in BASE.glob(pattern):
            shutil.rmtree(stray, ignore_errors=True)
    with tarfile.open(TEMPLATE) as archive:
        archive.extractall(BASE, filter="fully_trusted")


def source_snapshot():
    snapshot = {}
    for path in PROJECTS.values():
        checkout = WORLD / "client" / path
        snapshot[path] = (
            git(checkout, "rev-parse", "HEAD"),
            git(checkout, "status", "--porcelain", "--untracked-files=all"),
        )
    return snapshot


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

    # -- upstream and source actors ------------------------------------------

    @rule(name=st.sampled_from(sorted(PROJECTS)))
    def upstream_commit(self, name):
        step(f"upstream_commit({name})")
        seed = WORLD / "seed" / name
        self.count += 1
        (seed / "README").write_text(f"{name} {self.count}\n")
        git(seed, "commit", "-qam", f"upstream {self.count}")
        git(seed, "push", "-q", WORLD / "remotes" / f"{name}.git", "main")

    @rule()
    def source_sync(self):
        step("source_sync()")
        run(["repo", "sync", "-q"], WORLD / "client")

    @rule(path=st.sampled_from(EDITABLE))
    def source_commit(self, path):
        step(f"source_commit({path})")
        checkout = WORLD / "client" / path
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

    @rule(target=trees)
    def add(self):
        step("add()")
        self.count += 1
        name = f"t{self.count}"
        path = BASE / name
        self.tool_step(lambda: grw_json("add", "--name", name, str(path)))
        self.live[name] = path
        self.unbranched[name] = False
        self.branches[name] = set()
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
        elif value["error"]["code"] == "E_DIRTY" and self.unbranched[tree] and not force:
            pass
        else:
            raise Finding(f"remove {tree} failed: {json.dumps(value['error'])}")

    @rule(tree=trees)
    def repair(self, tree):
        step(f"repair({tree})")
        if tree in self.live:
            self.tool_step(lambda: grw_json("repair", tree))

    @rule()
    def repair_all(self):
        step("repair_all()")
        if self.live:
            self.tool_step(lambda: grw_json("repair"))

    @rule(tree=trees)
    def move(self, tree):
        step(f"move({tree})")
        if tree not in self.live:
            return
        self.count += 1
        target = BASE / f"t-moved-{self.count}"
        self.tool_step(lambda: grw_json("move", tree, str(target)))
        self.live[tree] = target

    # -- work inside trees -----------------------------------------------------

    @rule(tree=trees, path=st.sampled_from(EDITABLE))
    def tree_commit(self, tree, path):
        step(f"tree_commit({tree}, {path})")
        if tree not in self.live:
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
        if tree not in self.live:
            return
        self.count += 1
        branch = f"topic{self.count}"
        run(["repo", "start", branch, path], self.live[tree])
        self.branches[tree].add(path)

    @rule(tree=trees)
    def tree_sync(self, tree):
        step(f"tree_sync({tree})")
        if tree not in self.live:
            return
        root = self.live[tree]
        result = run(["repo", "sync", "-q"], root, check=False)
        run(["repo", "sync", "-q"], WORLD / "ref")
        if result.returncode != 0:
            if self.unbranched[tree] or self.branches[tree]:
                return
            raise Finding(f"repo sync failed in {tree}:\n{result.stderr[-2000:]}")
        for path in EDITABLE:
            checkout = root / path
            if git(checkout, "symbolic-ref", "-q", "HEAD", check=False) != "":
                continue
            ours = git(checkout, "rev-parse", "HEAD")
            reference = git(WORLD / "ref" / path, "rev-parse", "HEAD")
            if ours != reference and not self.unbranched[tree]:
                raise Finding(
                    f"after repo sync, {tree}/{path} is at {ours} but a fresh client is at {reference}"
                )

    # -- invariants ------------------------------------------------------------

    @invariant()
    def repo_works_in_every_tree(self):
        for name, root in self.live.items():
            expected = {
                project
                for project, path in PROJECTS.items()
                if (root / path).exists() and not (root / path).is_symlink()
            }
            result = run(["repo", "list", "-n"], root, check=False)
            listed = set(result.stdout.split())
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
            if tree["state"] != "complete" or tree["failed_projects"]:
                raise Finding(f"tree {name}: {json.dumps(tree)}")
            status = grw_json("status", name)["result"]
            for project in status["projects"]:
                head = git(self.live[name] / project["path"], "rev-parse", "HEAD")
                if project["head"] != head:
                    raise Finding(f"status says {name}/{project['path']} is at {project['head']}, git says {head}")

    @invariant()
    def no_stale_registrations(self):
        for path in PROJECTS.values():
            listing = git(WORLD / "client" / path, "worktree", "list", "--porcelain")
            if "prunable" in listing:
                raise Finding(f"stale worktree registration in {path}:\n{listing}")


def main():
    examples = int(sys.argv[1]) if len(sys.argv) > 1 else 50
    steps = int(os.environ.get("GRW_FUZZ_STEPS", "10"))
    BASE.mkdir(parents=True, exist_ok=True)
    if not TEMPLATE.exists():
        build_template()
    findings = HERE / "findings"
    findings.mkdir(exist_ok=True)
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
            path = findings / f"{stamp}-w{WORKER}-{index}.sh"
            notes = "\n".join(getattr(failure, "__notes__", []))
            path.write_text(
                f"# {BIN}\n# {type(failure).__name__}: "
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


if __name__ == "__main__":
    main()
