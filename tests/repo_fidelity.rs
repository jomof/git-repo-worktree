//! End-to-end tests against a real `repo` client built from local bare
//! repositories. They need `repo` on PATH and a git-repo source to initialize
//! from, named by `GRW_TEST_REPO_URL` (a local clone works). Without both, the
//! tests report that they were skipped and pass.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use tempfile::TempDir;

struct Client {
    _temp: TempDir,
    base: PathBuf,
    root: PathBuf,
}

/// The caller's own `GIT_REPO_WORKTREE_*` settings, which tests must not inherit.
fn inherited_tool_variables() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_string_lossy().starts_with("GIT_REPO_WORKTREE_"))
        .collect()
}

fn repo_url() -> Option<String> {
    let url = std::env::var("GRW_TEST_REPO_URL").ok()?;
    let has_repo = StdCommand::new("repo")
        .arg("--version")
        .output()
        .map(|output| output.status.success() || !output.stdout.is_empty())
        .unwrap_or(false);
    has_repo.then_some(url)
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> String {
    let output = StdCommand::new(program)
        .current_dir(cwd)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?} in {} failed: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn seed(base: &Path, name: &str) {
    let remote = base.join(format!("remotes/{name}.git"));
    let work = base.join(format!("seed/{name}"));
    fs::create_dir_all(&work).unwrap();
    run(
        base,
        "git",
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    run(&work, "git", &["init", "-q", "-b", "main"]);
    fs::write(work.join("README"), format!("{name} content\n")).unwrap();
    run(&work, "git", &["add", "."]);
    run(&work, "git", &["commit", "-qm", "init"]);
    run(
        &work,
        "git",
        &["push", "-q", remote.to_str().unwrap(), "main"],
    );
}

fn client(worktree_layout: bool) -> Option<Client> {
    let Some(url) = repo_url() else {
        eprintln!(
            "skipping: set GRW_TEST_REPO_URL and put repo on PATH to run repo fidelity tests"
        );
        return None;
    };
    let temp = TempDir::new().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    for name in ["a", "b", "pre"] {
        seed(&base, name);
    }
    let manifest = base.join("seed/manifest");
    fs::create_dir_all(&manifest).unwrap();
    run(&manifest, "git", &["init", "-q", "-b", "main"]);
    fs::write(
        manifest.join("default.xml"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest>
  <remote name="origin" fetch="file://{remotes}"/>
  <default remote="origin" revision="main"/>
  <project name="a" path="a"><linkfile src="README" dest="A_README"/></project>
  <project name="b" path="lib/b"/>
  <project name="pre" path="prebuilts/pre" clone-depth="1"/>
</manifest>
"#,
            remotes = base.join("remotes").display()
        ),
    )
    .unwrap();
    run(&manifest, "git", &["add", "."]);
    run(&manifest, "git", &["commit", "-qm", "manifest"]);
    let manifest_remote = base.join("remotes/manifest.git");
    run(
        &base,
        "git",
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            manifest_remote.to_str().unwrap(),
        ],
    );
    run(
        &manifest,
        "git",
        &["push", "-q", manifest_remote.to_str().unwrap(), "main"],
    );
    let root = base.join("client");
    fs::create_dir_all(&root).unwrap();
    let manifest_url = format!("file://{}", manifest_remote.display());
    let mut init = vec![
        "init",
        "-q",
        "-u",
        manifest_url.as_str(),
        "-b",
        "main",
        "--repo-url",
        url.as_str(),
        "--repo-rev",
        "main",
        "--no-repo-verify",
    ];
    if worktree_layout {
        init.push("--worktree");
    }
    run(&root, "repo", &init);
    run(&root, "repo", &["sync", "-q"]);
    Some(Client {
        _temp: temp,
        base,
        root,
    })
}

fn tool(client: &Client) -> Command {
    let mut command = Command::cargo_bin("git-repo-worktree").unwrap();
    for name in inherited_tool_variables() {
        command.env_remove(name);
    }
    command
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE", "1")
        .env("GIT_REPO_WORKTREE_NO_OVERLAY", "1")
        .arg("-C")
        .arg(&client.root);
    command
}

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

fn add(client: &Client, name: &str) -> PathBuf {
    let tree = client.base.join(name);
    let added = json(
        tool(client)
            .args(["add", "--force", "--json"])
            .arg(&tree)
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["state"], "complete");
    assert_eq!(added["result"]["repo_bridge"]["state"], "done");
    tree
}

#[test]
fn repo_commands_work_inside_a_tree_without_touching_the_source() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = add(&client, "tree");
    assert_eq!(
        fs::read_link(tree.join("a/.git")).unwrap(),
        PathBuf::from("../.repo/projects/a.git")
    );
    assert_eq!(
        fs::read_to_string(tree.join(".repo/project.list")).unwrap(),
        "a\nlib/b\n"
    );

    run(&tree, "repo", &["start", "feat", "a"]);
    assert_eq!(
        run(
            &tree.join("a"),
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "feat"
    );
    assert_eq!(
        run(&tree.join("a"), "git", &["config", "branch.feat.merge"]),
        "refs/heads/main"
    );
    fs::write(tree.join("a/README"), "tree change\n").unwrap();
    run(&tree.join("a"), "git", &["commit", "-qam", "tree change"]);

    let branches = run(&tree, "repo", &["branches"]);
    assert!(branches.contains("*  feat"), "{branches}");
    let source_branches = run(&client.root, "repo", &["branches"]);
    assert!(!source_branches.contains("*  feat"), "{source_branches}");
    let forall = run(&tree, "repo", &["forall", "-c", "echo $REPO_PATH"]);
    assert_eq!(forall.lines().collect::<Vec<_>>(), vec!["a", "lib/b"]);

    fs::write(tree.join("lib/b/new.txt"), "untracked\n").unwrap();
    run(&tree, "repo", &["sync", "-l", "-q"]);
    assert_eq!(
        fs::read_link(tree.join("a/.git")).unwrap(),
        PathBuf::from("../.repo/projects/a.git")
    );
    assert_eq!(
        run(
            &tree.join("a"),
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "feat"
    );
    assert!(fs::symlink_metadata(tree.join("prebuilts/pre"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_link(client.root.join("prebuilts/pre/.git")).unwrap(),
        PathBuf::from("../../.repo/projects/prebuilts/pre.git")
    );
    assert_eq!(
        run(&client.root.join("a"), "git", &["status", "--porcelain"]),
        ""
    );
    assert_eq!(
        run(
            &client.root.join("a"),
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "HEAD"
    );

    let listed = json(tool(&client).args(["list", "--json"]).assert().success());
    assert_eq!(
        listed["result"]["trees"][0]["checked_out_branches"],
        serde_json::json!([{"project": "a", "branch": "feat"}])
    );
    let dirty = json(
        tool(&client)
            .args(["remove", "--json", "tree"])
            .assert()
            .code(1),
    );
    assert_eq!(dirty["error"]["code"], "E_DIRTY");
    assert_eq!(
        dirty["error"]["details"]["dirty_projects"][0]["path"],
        "lib/b"
    );

    let moved = client.base.join("moved");
    tool(&client)
        .arg("move")
        .arg("tree")
        .arg(&moved)
        .assert()
        .success();
    assert!(run(&moved, "repo", &["status"]).contains("branch feat"));
    tool(&client).args(["lock", "tree"]).assert().success();
    tool(&client)
        .args(["remove", "--force", "--force", "tree"])
        .assert()
        .success();
    assert!(!moved.exists());
    let registrations = run(
        &client.root.join("a"),
        "git",
        &["worktree", "list", "--porcelain"],
    );
    assert!(
        !registrations.contains(moved.to_str().unwrap()),
        "{registrations}"
    );
    let manifest_worktrees = run(
        &client.root.join(".repo/manifests"),
        "git",
        &["worktree", "list", "--porcelain"],
    );
    assert!(!manifest_worktrees.contains(moved.to_str().unwrap()));
    assert_eq!(
        run(
            &client.root.join(".repo/manifests"),
            "git",
            &["branch", "--list", "repo-worktree/*"]
        ),
        ""
    );
    assert!(client.root.join(".repo/projects/a.git/config").is_file());
    assert_eq!(
        run(&client.root.join("a"), "git", &["branch", "--list", "feat"]),
        "feat"
    );
}

#[test]
fn per_worktree_refs_stay_private_to_the_tree() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = add(&client, "tree");
    let head = run(&tree.join("a"), "git", &["rev-parse", "HEAD"]);
    run(
        &tree.join("a"),
        "git",
        &["update-ref", "refs/bisect/bad", &head],
    );
    assert!(!client
        .root
        .join(".repo/projects/a.git/refs/bisect")
        .exists());
    let source_refs = run(
        &client.root.join("a"),
        "git",
        &["for-each-ref", "refs/bisect"],
    );
    assert_eq!(source_refs, "");
    tool(&client)
        .args(["remove", "--force", "tree"])
        .assert()
        .success();
    assert!(client.root.join(".repo/projects/a.git/refs/heads").is_dir());
}

#[test]
fn commands_run_inside_a_tree_target_its_source_client() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = add(&client, "tree");
    let mut inside = Command::cargo_bin("git-repo-worktree").unwrap();
    for name in inherited_tool_variables() {
        inside.env_remove(name);
    }
    let listed = json(
        inside
            .current_dir(tree.join("lib/b"))
            .args(["list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(listed["result"]["trees"].as_array().unwrap().len(), 1);
    let mut nested = Command::cargo_bin("git-repo-worktree").unwrap();
    for name in inherited_tool_variables() {
        nested.env_remove(name);
    }
    let refused = json(
        nested
            .current_dir(&tree)
            .args(["add", "--json"])
            .arg(client.base.join("nested"))
            .assert()
            .code(1),
    );
    assert_eq!(refused["error"]["code"], "E_NO_REPO_CLIENT");
    tool(&client).args(["remove", "tree"]).assert().success();
}

#[test]
fn worktree_layout_sources_bridge_natively() {
    let Some(client) = client(true) else {
        return;
    };
    let tree = add(&client, "tree");
    assert!(!fs::symlink_metadata(tree.join(".repo/worktrees"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(tree.join(".repo/worktrees/a.git"))
        .unwrap()
        .file_type()
        .is_symlink());
    run(&tree, "repo", &["start", "feat", "a"]);
    assert!(run(&tree, "repo", &["branches"]).contains("*  feat"));
    run(&tree, "repo", &["sync", "-l", "-q"]);
    assert_eq!(
        run(
            &tree.join("a"),
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "feat"
    );
    assert_eq!(
        run(
            &client.root.join("a"),
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "HEAD"
    );
    tool(&client).args(["remove", "tree"]).assert().success();
}

#[test]
fn git_only_trees_have_no_repo_directory() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = client.base.join("plain");
    let added = json(
        tool(&client)
            .args(["add", "--force", "--git-only", "--json"])
            .arg(&tree)
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["repo_bridge"], Value::Null);
    assert!(!tree.join(".repo").exists());
    assert!(tree.join("a/.git").is_file());
    tool(&client).args(["remove", "plain"]).assert().success();
}

#[cfg(target_os = "linux")]
#[test]
fn overlay_projects_work_with_repo_commands() {
    if std::env::var_os("GRW_TEST_REQUIRE_OVERLAY").is_none() {
        eprintln!("skipping: set GRW_TEST_REQUIRE_OVERLAY=1 where fuse-overlayfs is usable");
        return;
    }
    let Some(client) = client(false) else {
        return;
    };
    tool(&client)
        .args(["defaults", "overlay", "a"])
        .assert()
        .success();
    let tree = client.base.join("tree");
    tool(&client)
        .env_remove("GIT_REPO_WORKTREE_NO_OVERLAY")
        .args(["add", "--force"])
        .arg(&tree)
        .assert()
        .success();
    let listed = json(
        tool(&client)
            .args(["list", "-v", "--json"])
            .assert()
            .success(),
    );
    let mode = |path: &str| {
        listed["result"]["trees"][0]["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["path"] == path)
            .unwrap()["mode"]
            .clone()
    };
    assert_eq!(mode("a"), "overlay");
    assert_eq!(mode("prebuilts/pre"), "overlay");
    let forall = run(&tree, "repo", &["forall", "-c", "echo $REPO_PATH"]);
    assert!(forall.contains("prebuilts/pre"), "{forall}");
    assert_eq!(
        fs::read_link(tree.join("a/.git")).unwrap(),
        PathBuf::from("../.repo/projects/a.git")
    );
    run(&tree, "repo", &["start", "feat", "a"]);
    fs::write(tree.join("a/README"), "overlay edit\n").unwrap();
    assert!(run(&tree, "repo", &["status"]).contains("branch feat"));
    run(
        &tree.join("a"),
        "git",
        &["commit", "-qam", "overlay change"],
    );
    run(&tree, "repo", &["sync", "-l", "-q"]);
    assert_eq!(
        fs::read_to_string(tree.join("a/README")).unwrap(),
        "overlay edit\n"
    );
    assert_eq!(
        fs::read_to_string(client.root.join("a/README")).unwrap(),
        "a content\n"
    );
    tool(&client).args(["remove", "tree"]).assert().success();
    assert!(!tree.exists());
}

fn advance_upstream(client: &Client, name: &str) {
    let work = client.base.join(format!("seed/{name}"));
    fs::write(work.join("README"), format!("{name} upstream\n")).unwrap();
    run(
        &work,
        "git",
        &["commit", "-qam", &format!("upstream {name}")],
    );
    let remote = client.base.join(format!("remotes/{name}.git"));
    run(
        &work,
        "git",
        &["push", "-q", remote.to_str().unwrap(), "main"],
    );
}

#[test]
fn repo_sync_moves_a_tree_to_upstream_like_a_client() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = add(&client, "tree");
    let local =
        fs::read_to_string(tree.join(".repo/local_manifests/zz-git-repo-worktree.xml")).unwrap();
    assert!(
        local.contains(r#"<remove-project name="pre" path="prebuilts/pre" optional="true" />"#),
        "{local}"
    );
    assert!(fs::read_to_string(tree.join(".repo/manifest.xml"))
        .unwrap()
        .contains("default.xml"));

    run(&tree, "repo", &["start", "review", "a"]);
    fs::write(tree.join("a/mine.txt"), "mine\n").unwrap();
    run(&tree.join("a"), "git", &["add", "mine.txt"]);
    run(&tree.join("a"), "git", &["commit", "-qm", "my change"]);
    run(
        &tree.join("a"),
        "git",
        &["update-ref", "refs/published/review", "HEAD"],
    );
    advance_upstream(&client, "a");
    advance_upstream(&client, "b");
    let source_a = run(&client.root.join("a"), "git", &["rev-parse", "HEAD"]);
    let source_b = run(&client.root.join("lib/b"), "git", &["rev-parse", "HEAD"]);

    let refused = StdCommand::new("repo")
        .current_dir(&tree)
        .args(["sync", "-q"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("published (but not merged)"));
    assert_eq!(
        run(&tree.join("lib/b"), "git", &["log", "-1", "--format=%s"]),
        "upstream b"
    );

    run(&tree, "repo", &["sync", "-q", "--rebase"]);
    assert_eq!(
        run(
            &tree.join("a"),
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "review"
    );
    assert_eq!(
        run(&tree.join("a"), "git", &["log", "-1", "--format=%s"]),
        "my change"
    );
    assert_eq!(
        run(
            &tree.join("a"),
            "git",
            &["log", "-1", "--skip=1", "--format=%s"]
        ),
        "upstream a"
    );
    assert_eq!(
        fs::read_link(tree.join("a/.git")).unwrap(),
        PathBuf::from("../.repo/projects/a.git")
    );
    assert!(fs::symlink_metadata(tree.join("prebuilts/pre"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        run(&client.root.join("a"), "git", &["rev-parse", "HEAD"]),
        source_a
    );
    assert_eq!(
        run(&client.root.join("lib/b"), "git", &["rev-parse", "HEAD"]),
        source_b
    );
    assert_eq!(
        fs::read_link(client.root.join("prebuilts/pre/.git")).unwrap(),
        PathBuf::from("../../.repo/projects/prebuilts/pre.git")
    );
    tool(&client)
        .args(["remove", "--force", "tree"])
        .assert()
        .success();
    assert_eq!(
        run(
            &client.root.join("a"),
            "git",
            &["branch", "--list", "review"]
        ),
        "review"
    );
}

#[test]
fn repo_gc_in_a_tree_cannot_delete_the_sources_object_stores() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = add(&client, "tree");
    let objects = tree.join(".repo/project-objects");
    assert!(!fs::symlink_metadata(&objects)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(objects.join("a.git"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(objects.join("pre.git")).is_err());

    run(&tree, "repo", &["gc", "--yes"]);
    assert!(client
        .root
        .join(".repo/project-objects/pre.git/objects")
        .is_dir());
    assert!(client
        .root
        .join(".repo/project-objects/a.git/objects")
        .is_dir());
    run(
        &client.root.join("prebuilts/pre"),
        "git",
        &["status", "--porcelain"],
    );
    run(&tree, "repo", &["status"]);
    tool(&client).args(["remove", "tree"]).assert().success();
}

#[test]
fn background_removal_unregisters_bridged_projects_and_spares_the_source() {
    let Some(client) = client(false) else {
        return;
    };
    let tree = add(&client, "tree");
    let removed = json(
        tool(&client)
            .env_remove("GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE")
            .args(["remove", "--json", "tree"])
            .assert()
            .success(),
    );
    assert_eq!(removed["result"]["deleting_in_background"], true);
    assert!(!tree.exists());
    for project in ["a", "lib/b"] {
        let worktrees = run(
            &client.root.join(project),
            "git",
            &["worktree", "list", "--porcelain"],
        );
        assert!(!worktrees.contains(tree.to_str().unwrap()), "{worktrees}");
        assert!(!worktrees.contains("prunable"), "{worktrees}");
    }
    let history = client.root.join(".repo/repo-worktrees/history.jsonl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !fs::read_to_string(&history)
        .unwrap_or_default()
        .contains("\"pool maintain\"")
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        fs::read_dir(client.root.join(".repo/repo-worktrees/.trash"))
            .unwrap()
            .count(),
        0
    );
    assert!(client.root.join("prebuilts/pre/README").is_file());
    assert!(client
        .root
        .join(".repo/project-objects/a.git/objects")
        .is_dir());
    run(&client.root, "repo", &["status"]);
}
