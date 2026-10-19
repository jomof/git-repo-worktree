"""Concurrent operations on one client, checked afterwards.

Each round starts from the harness's world with two trees, then runs a random
batch of tool operations at the same time (add, remove, remove --idle,
restore, archive prune, repair, move, materialize, pool fill, defaults,
detach, status, list), each after a random delay. When all have finished and background maintenance has settled, every
result must be a success or an error that a concurrent operation explains,
and the world must satisfy the harness's invariants. A failing round's seed,
operations, and outputs are written to the findings directory; rerun one with
`race.py --seed <seed>`.

    GRW_BIN=target/debug/git-repo-worktree python fuzz/race.py [rounds]
"""

import json
import random
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness  # noqa: E402
from harness import BASE, BIN, CLIENT, ENV, FINDINGS, PATHS, WORLD, Finding  # noqa: E402

EXPLAINED = {"E_NO_SUCH_TREE", "E_NO_POOL_PROJECTS", "E_NAME_EXISTS", "E_NO_SUCH_ARCHIVE", "E_DEST_NOT_EMPTY"}


def tool(args):
    started = time.time()
    result = subprocess.run(
        [BIN, "-C", str(CLIENT), *args, "--json"], cwd=WORLD, env=ENV, capture_output=True, text=True
    )
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        value = None
    return {
        "args": args,
        "exit": result.returncode,
        "value": value,
        "stderr": result.stderr[-1500:],
        "ms": int((time.time() - started) * 1000),
    }


def plan(rng, trees, round_number):
    operations = []
    for index in range(rng.randint(3, 7)):
        kind = rng.choice(
            ["add", "add", "remove", "repair", "repair-all", "move", "materialize",
             "pool-fill", "default", "detach", "status", "list", "restore", "restore",
             "remove-idle", "archive-prune"]
        )
        tree = rng.choice(trees)
        unique = f"r{round_number}x{index}"
        if kind == "add":
            args = ["add", "--name", unique, str(BASE / unique)]
        elif kind == "remove":
            args = ["remove", tree] + (["--force"] if rng.random() < 0.5 else [])
        elif kind == "restore":
            args = ["restore", "s3"]
        elif kind == "remove-idle":
            args = ["remove", "--idle", "0s"]
        elif kind == "archive-prune":
            args = ["archive", "prune"]
        elif kind == "repair":
            args = ["repair", tree]
        elif kind == "repair-all":
            args = ["repair"]
        elif kind == "move":
            args = ["move", tree, str(BASE / f"{unique}-moved")]
        elif kind == "materialize":
            args = ["materialize", tree, "prebuilts/pre"]
        elif kind == "pool-fill":
            args = ["pool", "fill"]
        elif kind == "default":
            args = ["defaults", rng.choice(["pool", "auto"]), rng.choice(["a", "lib/b"])]
        elif kind == "detach":
            args = ["detach", "--branch", "nonexistent-branch"]
        elif kind == "status":
            args = ["status", tree]
        else:
            args = ["list"]
        operations.append((rng.uniform(0, 0.5), args))
    return operations


def check_results(results):
    for result in results:
        if result["exit"] == 101 or "panicked at" in result["stderr"]:
            raise Finding(f"{' '.join(result['args'])} panicked:\n{result['stderr']}")
        if result["value"] is None:
            raise Finding(f"{' '.join(result['args'])} printed no JSON:\n{result['stderr']}")
        if not result["value"]["ok"]:
            code = result["value"]["error"]["code"]
            if code not in EXPLAINED:
                raise Finding(f"{' '.join(result['args'])} failed with {code}: {json.dumps(result['value']['error'])}")


def check_world():
    deadline = time.time() + 180
    while harness.maintenance_running():
        if time.time() > deadline:
            raise Finding("maintenance did not settle")
        time.sleep(0.2)
    trees = harness.grw_json("list")["result"]["trees"]
    for tree in trees:
        root = Path(tree["path"])
        if not root.is_dir():
            raise Finding(f"tree {tree['name']} is registered but {root} is missing")
        if tree["state"] != "complete" or tree["failed_projects"]:
            raise Finding(f"tree {tree['name']}: {json.dumps(tree)}")
        expected = harness.owned_projects(root)
        result, listed = harness.repo_list(root)
        if result.returncode != 0 or listed != expected:
            raise Finding(f"repo list in {tree['name']} gave {sorted(listed)}, expected {sorted(expected)}:\n{result.stderr[-1000:]}")
        status = harness.grw_json("status", tree["name"])["result"]
        for project in status["projects"]:
            head = harness.git(root / project["path"], "rev-parse", "HEAD")
            if project["head"] != head:
                raise Finding(f"status says {tree['name']}/{project['path']} is at {project['head']}, git says {head}")
    registered = {str(Path(tree["path"]).resolve()) for tree in trees}
    for path in PATHS.values():
        checkout = CLIENT / path
        if not (checkout / ".git").exists():
            continue
        listing = harness.git(checkout, "worktree", "list", "--porcelain")
        for block in listing.split("\n\n"):
            if "prunable" in block and ".pool/" not in block:
                raise Finding(f"stale registration in {path}:\n{block}")
    for leftover in list(BASE.glob("r[0-9]*")):
        resolved = str(leftover.resolve())
        if leftover.is_dir() and any(leftover.iterdir()) and not any(
            resolved == root or resolved.startswith(root + "/") for root in registered
        ):
            raise Finding(f"{leftover} is left behind but belongs to no tree")
    archives = CLIENT / ".repo/repo-worktrees/.archive"
    records = {entry.name for entry in archives.iterdir()} if archives.is_dir() else set()
    orphans = harness.archive_ref_ids() - records
    if orphans:
        raise Finding(f"archive refs without a record: {sorted(orphans)}")
    listed = {entry["id"] for entry in harness.grw_json("archive", "list")["result"]["archives"]}
    if not listed <= records:
        raise Finding(f"archive list shows {sorted(listed - records)} with no record")
    trash = CLIENT / ".repo/repo-worktrees/.trash"
    if trash.is_dir() and any(trash.iterdir()):
        raise Finding(f"trash not emptied: {sorted(p.name for p in trash.iterdir())}")


def run_round(seed):
    rng = random.Random(seed)
    harness.LOG.clear()
    harness.fresh_world()
    for pattern in ["r[0-9]*", "s[0-9]", "s[0-9]-*"]:
        for stray in BASE.glob(pattern):
            subprocess.run(["chmod", "-R", "u+w", str(stray)])
            subprocess.run(["rm", "-rf", str(stray)])
    for name in ["s1", "s2", "s3"]:
        harness.grw_json("add", "--name", name, str(BASE / name))
    (BASE / "s3" / "a" / "scratch").write_text("archived work\n")
    harness.grw_json("remove", "s3")
    operations = plan(rng, ["s1", "s2"], seed)
    results = [None] * len(operations)

    def worker(index, delay, args):
        time.sleep(delay)
        results[index] = tool(args)

    threads = [
        threading.Thread(target=worker, args=(index, delay, args))
        for index, (delay, args) in enumerate(operations)
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    try:
        check_results(results)
        check_world()
    except Finding as finding:
        return finding, operations, results
    return None, operations, results


def main():
    harness.BASE.mkdir(parents=True, exist_ok=True)
    if not harness.TEMPLATE.exists():
        harness.build_template()
    FINDINGS.mkdir(parents=True, exist_ok=True)
    if len(sys.argv) > 2 and sys.argv[1] == "--seed":
        seeds = [int(sys.argv[2])]
    else:
        rounds = int(sys.argv[1]) if len(sys.argv) > 1 else 20
        start = random.randrange(1 << 30)
        seeds = range(start, start + rounds)
    print(f"{harness.describe_binary()}; race; world={harness.VARIANT} overlay={harness.OVERLAY} maintenance={harness.MAINTENANCE}", flush=True)
    started = time.time()
    found = 0
    for seed in seeds:
        finding, operations, results = run_round(seed)
        if finding is None:
            continue
        found += 1
        path = FINDINGS / f"race-{seed}-w{harness.WORKER}.txt"
        lines = [f"# seed {seed}: {str(finding).splitlines()[0][:300]}", str(finding), "", "# operations (delay s, args):"]
        lines += [f"{delay:.3f} {' '.join(args)}" for delay, args in operations]
        lines += ["", "# results:"] + [json.dumps(result) for result in results]
        path.write_text("\n".join(lines) + "\n")
        print(f"FINDING seed {seed}: {str(finding).splitlines()[0][:200]}\n  details: {path}")
    harness.stop_background()
    harness.unmount_all()
    print(f"{found} finding(s) in {len(seeds)} rounds ({time.time() - started:.0f}s)")
    sys.exit(1 if found else 0)


if __name__ == "__main__":
    main()
