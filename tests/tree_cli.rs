use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use tempfile::TempDir;

/// The caller's own `GIT_REPO_WORKTREE_*` settings, which tests must not inherit.
fn inherited_tool_variables() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_string_lossy().starts_with("GIT_REPO_WORKTREE_"))
        .collect()
}

struct Fixture {
    temp: TempDir,
    client: PathBuf,
    destination: PathBuf,
    path_env: String,
    snapshot: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = StdCommand::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_project(client: &Path, path: &str, files: &[(&str, &str)]) -> String {
    let project = client.join(path);
    fs::create_dir_all(&project).unwrap();
    git(&project, &["init"]);
    git(&project, &["config", "user.email", "test@example.com"]);
    git(&project, &["config", "user.name", "Test"]);
    for (name, contents) in files {
        fs::write(project.join(name), contents).unwrap();
    }
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["rev-parse", "HEAD"])
}

fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    let client = temp.path().join("client");
    let project = client.join("p");
    fs::create_dir_all(client.join(".repo/manifests")).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(client.join(".repo/project.list"), "p\n").unwrap();
    git(&project, &["init"]);
    git(&project, &["config", "user.email", "test@example.com"]);
    git(&project, &["config", "user.name", "Test"]);
    fs::write(project.join("source.txt"), "snapshot\n").unwrap();
    git(&project, &["add", "source.txt"]);
    git(&project, &["commit", "-m", "initial"]);
    let sha = git(&project, &["rev-parse", "HEAD"]);

    let snapshot = temp.path().join("snapshot.xml");
    fs::write(
        &snapshot,
        format!(
            r#"<manifest><project name="platform/p" path="p" revision="{sha}"><copyfile src="source.txt" dest="copied.txt"/><linkfile src="missing.txt" dest="linked.txt"/></project></manifest>"#
        ),
    )
    .unwrap();

    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let repo = bin.join("repo");
    fs::write(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = manifest ] && [ \"$2\" = -r ] && [ \"$3\" = -o ]; then\n  cp \"$GRW_TEST_SNAPSHOT\" \"$4\"\n  exit $?\nfi\nexit 64\n",
    )
    .unwrap();
    fs::set_permissions(&repo, fs::Permissions::from_mode(0o755)).unwrap();
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let destination = temp.path().join("tree");
    Fixture {
        temp,
        client,
        destination,
        path_env,
        snapshot,
    }
}

fn command(fixture: &Fixture) -> Command {
    let mut command = Command::cargo_bin("git-repo-worktree").unwrap();
    for name in inherited_tool_variables() {
        command.env_remove(name);
    }
    command
        .env("PATH", &fixture.path_env)
        .env("GRW_TEST_SNAPSHOT", &fixture.snapshot)
        .env("GIT_REPO_WORKTREE_NO_OVERLAY", "1")
        .env("GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE", "1")
        .env("GRW_TEST_POOL_RESERVE_BYTES", "0")
        .env("HOME", fixture.temp.path())
        .arg("-C")
        .arg(&fixture.client);
    command
}

fn output_json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

fn write_fixture_snapshot(fixture: &Fixture, sha: &str) {
    fs::write(
        &fixture.snapshot,
        format!(
            r#"<manifest><project name="platform/p" path="p" revision="{sha}"><copyfile src="source.txt" dest="copied.txt"/><linkfile src="missing.txt" dest="linked.txt"/></project></manifest>"#
        ),
    )
    .unwrap();
}

fn enable_clone_depth_sharing(fixture: &Fixture, all_fixed_depth: bool) {
    let p_sha = git(&fixture.client.join("p"), &["rev-parse", "HEAD"]);
    let q_sha = init_project(&fixture.client, "q", &[("q.txt", "ordinary\n")]);
    fs::write(fixture.client.join(".repo/project.list"), "p\nq\n").unwrap();
    let q_depth = if all_fixed_depth {
        r#" clone-depth="1""#
    } else {
        ""
    };
    fs::write(
        &fixture.snapshot,
        format!(
            r#"<manifest><project name="platform/p" path="p" revision="{p_sha}" clone-depth="1"><copyfile src="source.txt" dest="copied.txt"/><linkfile src="missing.txt" dest="linked.txt"/></project><project name="platform/q" path="q" revision="{q_sha}"{q_depth}/></manifest>"#
        ),
    )
    .unwrap();
}

#[test]
fn happy_path_list_move_lock_unlock_and_remove() {
    let fixture = fixture();
    let added = output_json(
        command(&fixture)
            .args(["-v", "add"])
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["state"], "complete");
    assert!(
        added["result"]["projects"][0]["est_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(matches!(
        added["result"]["projects"][0]["strategy"].as_str(),
        Some("reflink" | "checkout")
    ));
    assert!(added["result"]["projects"][0]["populate_ms"].is_u64());
    assert_eq!(
        fs::read_to_string(fixture.destination.join("copied.txt")).unwrap(),
        "snapshot\n"
    );
    assert_eq!(
        fs::read_link(fixture.destination.join("linked.txt")).unwrap(),
        PathBuf::from("p/missing.txt")
    );
    assert_eq!(
        git(
            &fixture.destination.join("p"),
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        "HEAD"
    );

    let listed = output_json(
        command(&fixture)
            .args(["list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(listed["result"]["trees"].as_array().unwrap().len(), 1);

    command(&fixture)
        .args(["lock", "--reason", "reference"])
        .arg(&fixture.destination)
        .assert()
        .success();
    let locked = output_json(
        command(&fixture)
            .arg("remove")
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .code(1),
    );
    assert_eq!(locked["error"]["code"], "E_TREE_LOCKED");
    command(&fixture)
        .arg("unlock")
        .arg(&fixture.destination)
        .assert()
        .success();

    let moved = fixture.temp.path().join("moved");
    command(&fixture)
        .arg("move")
        .arg(&fixture.destination)
        .arg(&moved)
        .assert()
        .success();
    assert_eq!(git(&moved.join("p"), &["status", "--porcelain"]), "");
    command(&fixture)
        .arg("remove")
        .arg(&moved)
        .assert()
        .success();
    assert!(!moved.exists());
}

#[test]
fn branch_collision_is_resumable_without_repinning() {
    let fixture = fixture();
    git(&fixture.client.join("p"), &["branch", "topic/test"]);
    let partial = output_json(
        command(&fixture)
            .args([
                "-v",
                "add",
                "-b",
                "topic/test",
                "--all-projects",
                "--name",
                "branch-tree",
            ])
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .code(3),
    );
    assert_eq!(partial["error"]["code"], "E_PARTIAL");
    assert_eq!(partial["result"]["projects"][0]["reason"], "branch-exists");
    let snapshot_before = fs::read(
        fixture
            .client
            .join(".repo/repo-worktrees/branch-tree/snapshot.xml"),
    )
    .unwrap();
    git(&fixture.client.join("p"), &["branch", "-D", "topic/test"]);

    let resumed = output_json(
        command(&fixture)
            .args(["-v", "add", "--continue", "branch-tree", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(resumed["result"]["state"], "complete");
    assert_eq!(
        fs::read(
            fixture
                .client
                .join(".repo/repo-worktrees/branch-tree/snapshot.xml")
        )
        .unwrap(),
        snapshot_before
    );
    command(&fixture)
        .args(["remove", "--force", "branch-tree"])
        .assert()
        .success();
}

#[test]
fn dirty_remove_and_prune_match_git_semantics() {
    let fixture = fixture();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    fs::write(fixture.destination.join("p/untracked"), "dirty").unwrap();
    let dirty = output_json(
        command(&fixture)
            .arg("remove")
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .code(1),
    );
    assert_eq!(dirty["error"]["code"], "E_DIRTY");
    command(&fixture)
        .args(["remove", "--force"])
        .arg(&fixture.destination)
        .assert()
        .success();

    let second = fixture.temp.path().join("prunable");
    command(&fixture).arg("add").arg(&second).assert().success();
    fs::remove_dir_all(&second).unwrap();
    command(&fixture)
        .args(["prune", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("would prune prunable"));
    command(&fixture).arg("prune").assert().success();
    assert!(!fixture
        .client
        .join(".repo/repo-worktrees/prunable")
        .exists());
}

#[test]
fn sync_detection_and_force_override() {
    let fixture = fixture();
    let lock = fixture.client.join(".repo/manifests.git/index.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "").unwrap();
    let value = output_json(
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .code(4),
    );
    assert_eq!(value["error"]["code"], "E_SYNC_ACTIVE");
    command(&fixture)
        .args(["add", "--force"])
        .arg(&fixture.destination)
        .assert()
        .success();
    command(&fixture)
        .args(["remove", "--force"])
        .arg(&fixture.destination)
        .assert()
        .success();
}

/// Starts a process whose command line looks like repo's `main.py` running
/// `sync` for the client whose `.repo` is `repo_dir`.
fn fake_repo_sync(fixture: &Fixture, repo_dir: &Path) -> std::process::Child {
    let script = fixture.temp.path().join("fake/main.py");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "import time\ntime.sleep(60)\n").unwrap();
    let child = StdCommand::new("python3")
        .arg(&script)
        .arg(format!("--repo-dir={}", repo_dir.display()))
        .arg("sync")
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    child
}

#[test]
fn only_a_sync_of_this_client_blocks_add() {
    let fixture = fixture();
    let other = fixture.temp.path().join("other-client/.repo");
    fs::create_dir_all(&other).unwrap();
    let mut elsewhere = fake_repo_sync(&fixture, &other);
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    elsewhere.kill().unwrap();
    let _ = elsewhere.wait();

    let mut here = fake_repo_sync(&fixture, &fixture.client.join(".repo"));
    let refused = output_json(
        command(&fixture)
            .args(["add", "--json"])
            .arg(fixture.temp.path().join("second"))
            .assert()
            .code(4),
    );
    here.kill().unwrap();
    let _ = here.wait();
    assert_eq!(refused["error"]["code"], "E_SYNC_ACTIVE");
}

#[test]
fn crash_hook_leaves_a_resumable_original_snapshot() {
    let fixture = fixture();
    command(&fixture)
        .env("GRW_TEST_CRASH_AFTER", "1")
        .args(["add", "--name", "crashed"])
        .arg(&fixture.destination)
        .assert()
        .failure();
    let listed = output_json(
        command(&fixture)
            .args(["list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(listed["result"]["trees"][0]["state"], "creating");
    let resumed = output_json(
        command(&fixture)
            .args(["-v", "add", "--continue", "crashed", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(resumed["result"]["state"], "complete");
    command(&fixture)
        .args(["remove", "--force", "crashed"])
        .assert()
        .success();
}

#[test]
fn newer_registry_format_is_rejected() {
    let fixture = fixture();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let path = fixture.client.join(".repo/repo-worktrees/tree/meta.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["format_version"] = Value::from(4);
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let output = output_json(command(&fixture).args(["list", "--json"]).assert().code(1));
    assert_eq!(output["error"]["code"], "E_FORMAT_TOO_NEW");
}

#[test]
fn default_add_shares_clean_fixed_depth_projects_and_remove_preserves_source() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    let assert = command(&fixture)
        .args(["-v", "add", "--json"])
        .arg(&fixture.destination)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains(
        "sharing 1 project(s) with the source checkout; materialize one before editing it"
    ));
    let added = output_json(assert);
    let projects = added["result"]["projects"].as_array().unwrap();
    let shared = projects
        .iter()
        .find(|project| project["path"] == "p")
        .unwrap();
    assert_eq!(shared["mode"], "symlink-shared");
    assert_eq!(shared["clone_depth"], 1);
    assert_eq!(shared["strategy"], Value::Null);
    assert_eq!(
        fs::read_link(fixture.destination.join("p")).unwrap(),
        fs::canonicalize(fixture.client.join("p")).unwrap()
    );
    assert!(!fs::symlink_metadata(fixture.destination.join("q"))
        .unwrap()
        .file_type()
        .is_symlink());

    command(&fixture)
        .args(["list", "--porcelain"])
        .assert()
        .success()
        .stdout(predicates::str::contains("shared 1"));
    command(&fixture)
        .arg("remove")
        .arg(&fixture.destination)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.client.join("p/source.txt")).unwrap(),
        "snapshot\n"
    );
}

#[test]
fn persistent_defaults_are_symmetric_and_affect_only_future_trees() {
    let fixture = fixture();
    let configured = output_json(
        command(&fixture)
            .args(["-v", "defaults", "symlink", "p", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(configured["result"]["projects"]["p"], "symlink");

    let shared_tree = fixture.temp.path().join("shared-default");
    let shared = output_json(
        command(&fixture)
            .args(["-v", "add", "--name", "shared-default", "--json"])
            .arg(&shared_tree)
            .assert()
            .success(),
    );
    assert_eq!(shared["result"]["projects"][0]["mode"], "symlink-shared");
    assert_eq!(shared["result"]["default_modes"]["p"], "symlink");

    command(&fixture)
        .args(["defaults", "worktree", "p"])
        .assert()
        .success();
    let materialized_tree = fixture.temp.path().join("worktree-default");
    let materialized = output_json(
        command(&fixture)
            .args(["-v", "add", "--name", "worktree-default", "--json"])
            .arg(&materialized_tree)
            .assert()
            .success(),
    );
    assert_eq!(materialized["result"]["projects"][0]["mode"], "worktree");
    assert_eq!(materialized["result"]["default_modes"]["p"], "worktree");

    let listed = output_json(
        command(&fixture)
            .args(["list", "--json"])
            .assert()
            .success(),
    );
    let original = listed["result"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["name"] == "shared-default")
        .unwrap();
    assert_eq!(
        original["project_counts"],
        serde_json::json!({"symlink-shared": 1})
    );
    assert!(original.get("projects").is_none());
    let detailed = output_json(
        command(&fixture)
            .args(["list", "-v", "--json"])
            .assert()
            .success(),
    );
    let original = detailed["result"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["name"] == "shared-default")
        .unwrap();
    assert_eq!(original["projects"][0]["mode"], "symlink-shared");
    assert_eq!(original["default_modes"]["p"], "symlink");

    let automatic = output_json(
        command(&fixture)
            .args(["-v", "defaults", "auto", "p", "--json"])
            .assert()
            .success(),
    );
    assert!(automatic["result"]["projects"]
        .as_object()
        .unwrap()
        .is_empty());
    let automatic_tree = fixture.temp.path().join("automatic-default");
    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--name", "automatic-default", "--json"])
            .arg(&automatic_tree)
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["projects"][0]["mode"], "worktree");
    assert!(added["result"]["default_modes"]
        .as_object()
        .unwrap()
        .is_empty());
}

#[test]
fn persistent_defaults_override_clone_depth_candidate_inference() {
    for preference in ["worktree", "symlink-all-clone"] {
        let fixture = fixture();
        enable_clone_depth_sharing(&fixture, preference == "symlink-all-clone");
        let mode = if preference == "worktree" {
            "worktree"
        } else {
            "symlink"
        };
        command(&fixture)
            .args(["defaults", mode, "p"])
            .assert()
            .success();
        let added = output_json(
            command(&fixture)
                .args(["-v", "add", "--json"])
                .arg(&fixture.destination)
                .assert()
                .success(),
        );
        let projects = added["result"]["projects"].as_array().unwrap();
        let p = projects
            .iter()
            .find(|project| project["path"] == "p")
            .unwrap();
        if preference == "worktree" {
            assert_eq!(p["mode"], "worktree");
        } else {
            assert_eq!(p["mode"], "symlink-shared");
            let q = projects
                .iter()
                .find(|project| project["path"] == "q")
                .unwrap();
            assert_eq!(q["mode"], "worktree");
        }
    }
}

#[test]
fn persistent_symlink_default_does_not_bypass_source_safety() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "symlink", "p"])
        .assert()
        .success();
    fs::write(fixture.client.join("p/source.txt"), "dirty source\n").unwrap();
    fs::write(fixture.client.join("p/untracked.txt"), "source only\n").unwrap();
    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["projects"][0]["mode"], "worktree");
    assert_eq!(
        fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
        "snapshot\n"
    );
    assert!(!fixture.destination.join("p/untracked.txt").exists());
}

#[test]
fn defaults_validate_new_preferences_but_auto_can_remove_stale_paths() {
    let fixture = fixture();
    let unknown = output_json(
        command(&fixture)
            .args(["defaults", "symlink", "missing", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(unknown["error"]["code"], "E_UNKNOWN_PROJECT");
    let unsafe_path = output_json(
        command(&fixture)
            .args(["defaults", "worktree", "../p", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(unsafe_path["error"]["code"], "E_BAD_PROJECT_PATH");

    command(&fixture)
        .args(["defaults", "symlink", "p"])
        .assert()
        .success();
    fs::write(fixture.client.join(".repo/project.list"), "").unwrap();
    let cleared = output_json(
        command(&fixture)
            .args(["-v", "defaults", "auto", "p", "--json"])
            .assert()
            .success(),
    );
    assert!(cleared["result"]["projects"]
        .as_object()
        .unwrap()
        .is_empty());
}

#[test]
fn interrupted_add_retains_its_captured_defaults() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "symlink", "p"])
        .assert()
        .success();
    command(&fixture)
        .env("GRW_TEST_CRASH_AFTER", "1")
        .args(["add", "--name", "captured-default"])
        .arg(&fixture.destination)
        .assert()
        .failure();
    command(&fixture)
        .args(["defaults", "worktree", "p"])
        .assert()
        .success();

    let resumed = output_json(
        command(&fixture)
            .args(["-v", "add", "--continue", "captured-default", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(resumed["result"]["default_modes"]["p"], "symlink");
    assert_eq!(resumed["result"]["projects"][0]["mode"], "symlink-shared");
}

#[test]
fn interrupted_shared_add_replays_idempotently() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    command(&fixture)
        .env("GRW_TEST_CRASH_AFTER", "1")
        .args(["add", "--name", "shared-crash"])
        .arg(&fixture.destination)
        .assert()
        .failure();
    let resumed = output_json(
        command(&fixture)
            .args(["-v", "add", "--continue", "shared-crash", "--json"])
            .assert()
            .success(),
    );
    let shared = resumed["result"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["path"] == "p")
        .unwrap();
    assert_eq!(shared["mode"], "symlink-shared");
    assert_eq!(
        fs::read_link(fixture.destination.join("p")).unwrap(),
        fs::canonicalize(fixture.client.join("p")).unwrap()
    );
}

#[test]
fn full_controls_and_client_wide_guardrail_materialize_instead() {
    for control in ["full", "full-project", "branch", "all-fixed-depth"] {
        let fixture = fixture();
        enable_clone_depth_sharing(&fixture, control == "all-fixed-depth");
        let mut add = command(&fixture);
        add.args(["-v", "add", "--json"]);
        match control {
            "full" => {
                add.arg("--full");
            }
            "full-project" => {
                add.args(["--full-project", "p"]);
            }
            "branch" => {
                add.args(["-b", "topic/share-test", "-p", "p"]);
            }
            "all-fixed-depth" => {}
            _ => unreachable!(),
        }
        let added = output_json(add.arg(&fixture.destination).assert().success());
        assert_eq!(
            added["result"]["projects"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|project| project["mode"] == "symlink-shared")
                .count(),
            0,
            "{control}"
        );
        command(&fixture)
            .args(["remove", "--force"])
            .arg(&fixture.destination)
            .assert()
            .success();
    }
}

#[test]
fn materialize_converts_shared_project_and_preserves_tree_lock() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    command(&fixture)
        .args(["add", "--lock", "--reason", "reference"])
        .arg(&fixture.destination)
        .assert()
        .success();
    let converted = output_json(
        command(&fixture)
            .args(["-v", "materialize", "tree", "p", "--json"])
            .assert()
            .success(),
    );
    let project = converted["result"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["path"] == "p")
        .unwrap();
    assert_eq!(project["mode"], "worktree");
    assert!(matches!(
        project["strategy"].as_str(),
        Some("reflink" | "checkout")
    ));
    assert!(!fs::symlink_metadata(fixture.destination.join("p"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        git(&fixture.destination.join("p"), &["status", "--porcelain"]),
        ""
    );
    let worktrees = git(
        &fixture.client.join("p"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(worktrees.contains(&fixture.destination.join("p").display().to_string()));
    assert!(worktrees.contains("locked"));
    let again = output_json(
        command(&fixture)
            .args(["materialize", "tree", "q", "p/sub/dir", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(
        again["result"]["already_materialized"],
        serde_json::json!(["q", "p"])
    );
    assert_eq!(again["result"]["materialized"], serde_json::json!([]));
    let unknown = output_json(
        command(&fixture)
            .args(["materialize", "tree", "nope", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(unknown["error"]["code"], "E_UNKNOWN_PROJECT");

    fs::write(fixture.destination.join("p/tree-only.txt"), "isolated\n").unwrap();
    assert!(!fixture.client.join("p/tree-only.txt").exists());
    command(&fixture)
        .args(["remove", "--force", "--force", "tree"])
        .assert()
        .success();
}

#[test]
fn materialize_resumes_after_the_shared_link_was_durably_unlinked() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let entry = fixture.client.join(".repo/repo-worktrees/tree");
    let mut journal = fs::OpenOptions::new()
        .append(true)
        .open(entry.join("journal.jsonl"))
        .unwrap();
    writeln!(
        journal,
        r#"{{"ts":"2026-07-23T00:00:00Z","step":"project-materialize","status":"started","project":"p"}}"#
    )
    .unwrap();
    journal.sync_all().unwrap();
    fs::remove_file(fixture.destination.join("p")).unwrap();

    let converted = output_json(
        command(&fixture)
            .args(["-v", "materialize", "tree", "p", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(
        converted["result"]["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["path"] == "p")
            .unwrap()["mode"],
        "worktree"
    );
    assert_eq!(
        fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
        "snapshot\n"
    );
}

#[test]
fn sharing_demotes_for_dirty_wrong_head_and_sparse_sources() {
    for condition in ["dirty", "wrong-head", "sparse"] {
        let fixture = fixture();
        enable_clone_depth_sharing(&fixture, false);
        match condition {
            "dirty" => {
                fs::write(fixture.client.join("p/source.txt"), "dirty source\n").unwrap();
                fs::write(fixture.client.join("p/source-only.txt"), "untracked\n").unwrap();
            }
            "wrong-head" => {
                fs::write(fixture.client.join("p/source.txt"), "later\n").unwrap();
                git(&fixture.client.join("p"), &["add", "source.txt"]);
                git(&fixture.client.join("p"), &["commit", "-m", "later"]);
            }
            "sparse" => {
                git(
                    &fixture.client.join("p"),
                    &["config", "core.sparseCheckout", "true"],
                );
            }
            _ => unreachable!(),
        }
        let added = output_json(
            command(&fixture)
                .args(["-v", "add", "--json"])
                .arg(&fixture.destination)
                .assert()
                .success(),
        );
        let project = added["result"]["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["path"] == "p")
            .unwrap();
        assert_eq!(project["mode"], "worktree", "{condition}");
        assert_eq!(
            fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
            "snapshot\n",
            "{condition}"
        );
        assert!(
            !fixture.destination.join("p/source-only.txt").exists(),
            "{condition}"
        );
    }
}

#[test]
fn directive_destinations_and_requested_submodules_demote_sharing() {
    for condition in ["directive-destination", "submodules"] {
        let fixture = fixture();
        enable_clone_depth_sharing(&fixture, false);
        if condition == "submodules" {
            fs::write(fixture.client.join("p/.gitmodules"), "").unwrap();
            git(&fixture.client.join("p"), &["add", ".gitmodules"]);
            git(
                &fixture.client.join("p"),
                &["commit", "-m", "submodule marker"],
            );
        }
        let p_sha = git(&fixture.client.join("p"), &["rev-parse", "HEAD"]);
        let q_sha = git(&fixture.client.join("q"), &["rev-parse", "HEAD"]);
        let directive = if condition == "directive-destination" {
            r#"<copyfile src="source.txt" dest="p/from-directive.txt"/>"#
        } else {
            ""
        };
        fs::write(
            &fixture.snapshot,
            format!(
                r#"<manifest><project name="platform/p" path="p" revision="{p_sha}" clone-depth="1">{directive}</project><project name="platform/q" path="q" revision="{q_sha}"/></manifest>"#
            ),
        )
        .unwrap();
        let mut add = command(&fixture);
        add.args(["-v", "add", "--json"]);
        if condition == "submodules" {
            add.arg("--submodules");
        }
        let added = output_json(add.arg(&fixture.destination).assert().success());
        let project = added["result"]["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["path"] == "p")
            .unwrap();
        assert_eq!(project["mode"], "worktree", "{condition}");
        if condition == "directive-destination" {
            assert_eq!(
                fs::read_to_string(fixture.destination.join("p/from-directive.txt")).unwrap(),
                "snapshot\n"
            );
        }
    }
}

#[test]
fn a_fixed_depth_parent_of_another_project_is_materialized() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    fs::write(fixture.client.join("p/.gitignore"), "child/\n").unwrap();
    git(&fixture.client.join("p"), &["add", ".gitignore"]);
    git(
        &fixture.client.join("p"),
        &["commit", "-m", "ignore nested project"],
    );
    let child_sha = init_project(
        &fixture.client,
        "p/child",
        &[("child.txt", "nested project\n")],
    );
    fs::write(fixture.client.join(".repo/project.list"), "p\np/child\nq\n").unwrap();
    let p_sha = git(&fixture.client.join("p"), &["rev-parse", "HEAD"]);
    let q_sha = git(&fixture.client.join("q"), &["rev-parse", "HEAD"]);
    fs::write(
        &fixture.snapshot,
        format!(
            r#"<manifest><project name="platform/p" path="p" revision="{p_sha}" clone-depth="1"/><project name="platform/child" path="p/child" revision="{child_sha}"/><project name="platform/q" path="q" revision="{q_sha}"/></manifest>"#
        ),
    )
    .unwrap();

    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    let parent = added["result"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["path"] == "p")
        .unwrap();
    assert_eq!(parent["mode"], "worktree");
    assert_eq!(
        fs::read_to_string(fixture.destination.join("p/child/child.txt")).unwrap(),
        "nested project\n"
    );
}

#[test]
fn repair_restores_shared_links_but_preserves_unexpected_real_data() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    fs::remove_file(fixture.destination.join("p")).unwrap();
    command(&fixture)
        .args(["repair", "tree"])
        .assert()
        .success();
    assert_eq!(
        fs::read_link(fixture.destination.join("p")).unwrap(),
        fs::canonicalize(fixture.client.join("p")).unwrap()
    );

    fs::remove_file(fixture.destination.join("p")).unwrap();
    fs::create_dir(fixture.destination.join("p")).unwrap();
    fs::write(fixture.destination.join("p/keep"), "user data\n").unwrap();
    command(&fixture).args(["repair", "tree"]).assert().code(3);
    assert_eq!(
        fs::read_to_string(fixture.destination.join("p/keep")).unwrap(),
        "user data\n"
    );
}

#[test]
fn snapshot_failure_is_retryable_under_the_same_name() {
    let fixture = fixture();
    fs::remove_file(&fixture.snapshot).unwrap();
    let output = output_json(
        command(&fixture)
            .args(["add", "--name", "retry-me"])
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .code(1),
    );
    assert_eq!(output["error"]["code"], "E_SNAPSHOT_FAILED");
    assert!(!fixture
        .client
        .join(".repo/repo-worktrees/retry-me")
        .exists());
}

#[test]
fn explicit_names_are_validated_without_silent_rewriting() {
    let fixture = fixture();
    let output = output_json(
        command(&fixture)
            .args(["add", "--name", "not a valid/name"])
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .code(1),
    );
    assert_eq!(output["error"]["code"], "E_BAD_NAME");
}

#[test]
fn verbose_add_reports_live_project_progress_and_phase_timings() {
    let fixture = fixture();
    let assert = command(&fixture)
        .args(["-v", "add"])
        .arg(&fixture.destination)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains("phase snapshot: starting"));
    assert!(stderr.contains("phase snapshot:"));
    assert!(stderr.contains("phase projects: starting 1 project(s)"));
    assert!(stderr.contains("project 1/1: added p"));
    assert!(stderr.contains("phase projects:"));
    assert!(stderr.contains("phase total:"));
}

#[test]
fn dirty_source_content_never_enters_the_tree() {
    let fixture = fixture();
    fs::write(fixture.client.join("p/source.txt"), "dirty source\n").unwrap();
    fs::write(fixture.client.join("p/untracked.txt"), "source only\n").unwrap();
    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["projects"][0]["strategy"], "checkout");
    assert_eq!(
        fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
        "snapshot\n"
    );
    assert!(!fixture.destination.join("p/untracked.txt").exists());
}

#[test]
fn wrong_head_sparse_and_no_reflink_all_fall_back_to_checkout() {
    for mode in ["wrong-head", "sparse", "disabled"] {
        let fixture = fixture();
        match mode {
            "wrong-head" => {
                fs::write(fixture.client.join("p/source.txt"), "later\n").unwrap();
                git(&fixture.client.join("p"), &["add", "source.txt"]);
                git(&fixture.client.join("p"), &["commit", "-m", "later"]);
            }
            "sparse" => {
                git(
                    &fixture.client.join("p"),
                    &["config", "core.sparseCheckout", "true"],
                );
            }
            "disabled" => {}
            _ => unreachable!(),
        }
        let mut add = command(&fixture);
        add.args(["-v", "add", "--json"]);
        if mode == "disabled" {
            add.args(["--no-reflink", "--heavy-jobs", "1"]);
        }
        let added = output_json(add.arg(&fixture.destination).assert().success());
        assert_eq!(added["result"]["projects"][0]["strategy"], "checkout");
        if mode == "disabled" {
            assert_eq!(added["result"]["no_reflink"], true);
            assert_eq!(added["result"]["heavy_jobs"], 1);
        }
        assert_eq!(
            fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
            "snapshot\n"
        );
    }
}

#[test]
fn reflink_population_preserves_symlinks_modes_and_cleanliness_when_supported() {
    let fixture = fixture();
    let project = fixture.client.join("p");
    fs::write(project.join("executable"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        project.join("executable"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("source.txt", project.join("source-link")).unwrap();
    git(&project, &["add", "executable", "source-link"]);
    git(&project, &["commit", "-m", "add modes"]);
    init_project(
        &project,
        "nested-submodule",
        &[("submodule.txt", "nested\n")],
    );
    git(&project, &["add", "nested-submodule"]);
    git(&project, &["commit", "-m", "add gitlink"]);
    let sha = git(&project, &["rev-parse", "HEAD"]);
    write_fixture_snapshot(&fixture, &sha);

    let probe_source = fixture.temp.path().join("reflink-source");
    let probe_dest = fixture.temp.path().join("reflink-dest");
    fs::write(&probe_source, "probe").unwrap();
    let reflink_supported = reflink_copy::reflink(&probe_source, &probe_dest).is_ok();

    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    if reflink_supported {
        assert_eq!(added["result"]["projects"][0]["strategy"], "reflink");
    }
    assert_eq!(
        git(&fixture.destination.join("p"), &["status", "--porcelain"]),
        ""
    );
    assert_eq!(
        fs::read_link(fixture.destination.join("p/source-link")).unwrap(),
        PathBuf::from("source.txt")
    );
    assert!(fixture.destination.join("p/nested-submodule").is_dir());
    assert_ne!(
        fs::metadata(fixture.destination.join("p/executable"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
}

#[test]
fn resume_resets_an_incomplete_registered_worktree_even_when_head_matches() {
    let fixture = fixture();
    command(&fixture)
        .args(["add", "--no-reflink"])
        .arg(&fixture.destination)
        .assert()
        .success();
    fs::remove_file(fixture.destination.join("p/source.txt")).unwrap();
    let meta_path = fixture.client.join(".repo/repo-worktrees/tree/meta.json");
    let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta["state"] = Value::from("incomplete");
    meta["projects"][0]["state"] = Value::from("pending");
    meta["projects"][0]["strategy"] = Value::Null;
    fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();

    let resumed = output_json(
        command(&fixture)
            .args(["-v", "add", "--continue", "tree", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(resumed["result"]["projects"][0]["strategy"], "checkout");
    assert_eq!(
        fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
        "snapshot\n"
    );
    let usage = output_json(
        command(&fixture)
            .args([
                "add",
                "--continue",
                "--no-reflink",
                "--heavy-jobs",
                "1",
                "tree",
                "--json",
            ])
            .assert()
            .code(2),
    );
    assert_eq!(usage["error"]["code"], "E_USAGE");
}

#[test]
fn projects_missing_from_repo_snapshot_are_pinned_from_disk_with_directives() {
    let temp = TempDir::new().unwrap();
    let client = temp.path().join("client");
    fs::create_dir_all(client.join(".repo/manifests")).unwrap();
    let default_sha = init_project(&client, "p", &[("source.txt", "default\n")]);
    let supplemental_sha = init_project(
        &client,
        "prebuilts/ndk",
        &[("version.txt", "r99\n"), ("tool", "binary\n")],
    );
    fs::write(client.join(".repo/project.list"), "p\nprebuilts/ndk\n").unwrap();
    fs::write(
        client.join(".repo/manifest.xml"),
        r#"<manifest><project name="platform/p" path="p"/><include name="nondefault.xml"/></manifest>"#,
    )
    .unwrap();
    fs::write(
        client.join(".repo/manifests/nondefault.xml"),
        r#"<manifest><project name="toolchain/ndk" path="prebuilts/ndk" groups="notdefault,platform-linux"><copyfile src="version.txt" dest="ndk-version.txt"/><linkfile src="tool" dest="ndk-tool"/></project></manifest>"#,
    )
    .unwrap();

    let snapshot = temp.path().join("snapshot.xml");
    fs::write(
        &snapshot,
        format!(
            r#"<manifest><project name="platform/p" path="p" revision="{default_sha}"/></manifest>"#
        ),
    )
    .unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let repo = bin.join("repo");
    fs::write(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = manifest ] && [ \"$2\" = -r ] && [ \"$3\" = -o ]; then\n  cp \"$GRW_TEST_SNAPSHOT\" \"$4\"\n  exit $?\nfi\nexit 64\n",
    )
    .unwrap();
    fs::set_permissions(&repo, fs::Permissions::from_mode(0o755)).unwrap();
    let fixture = Fixture {
        destination: temp.path().join("tree"),
        path_env: format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
        snapshot,
        client,
        temp,
    };

    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success()
            .stderr(predicates::str::contains("supplemented prebuilts/ndk")),
    );
    let projects = added["result"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2);
    let supplemental = projects
        .iter()
        .find(|project| project["path"] == "prebuilts/ndk")
        .unwrap();
    assert_eq!(supplemental["sha"], supplemental_sha);
    assert_eq!(
        fs::read_to_string(fixture.destination.join("ndk-version.txt")).unwrap(),
        "r99\n"
    );
    assert_eq!(
        fs::read_link(fixture.destination.join("ndk-tool")).unwrap(),
        PathBuf::from("prebuilts/ndk/tool")
    );
    let pinned = fs::read_to_string(
        fixture
            .client
            .join(".repo/repo-worktrees/tree/snapshot.xml"),
    )
    .unwrap();
    assert!(pinned.contains(r#"path="prebuilts/ndk""#));
    assert!(pinned.contains(&supplemental_sha));

    let supplemental_source = fixture.client.join("prebuilts/ndk");
    fs::write(supplemental_source.join("version.txt"), "r100\n").unwrap();
    git(&supplemental_source, &["add", "version.txt"]);
    git(
        &supplemental_source,
        &["commit", "-m", "later source state"],
    );
    let later_sha = git(&supplemental_source, &["rev-parse", "HEAD"]);
    assert_ne!(later_sha, supplemental_sha);

    let meta_path = fixture.client.join(".repo/repo-worktrees/tree/meta.json");
    let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta["state"] = Value::from("incomplete");
    meta["projects"] = Value::Array(Vec::new());
    meta["copyfiles"] = Value::Array(Vec::new());
    meta["linkfiles"] = Value::Array(Vec::new());
    fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();

    let resumed = output_json(
        command(&fixture)
            .args(["-v", "add", "--continue", "tree", "--json"])
            .assert()
            .success(),
    );
    let resumed_supplemental = resumed["result"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["path"] == "prebuilts/ndk")
        .unwrap();
    assert_eq!(resumed_supplemental["sha"], supplemental_sha);
}

fn two_project_snapshot(fixture: &Fixture) {
    let p_sha = git(&fixture.client.join("p"), &["rev-parse", "HEAD"]);
    let q_sha = init_project(&fixture.client, "q", &[("q.txt", "ordinary\n")]);
    fs::write(fixture.client.join(".repo/project.list"), "p\nq\n").unwrap();
    fs::write(
        &fixture.snapshot,
        format!(
            r#"<manifest><remote name="origin" fetch="."/><default remote="origin" revision="main"/><project name="platform/p" path="p" revision="{p_sha}" upstream="release"/><project name="platform/q" path="q" revision="{q_sha}"/></manifest>"#
        ),
    )
    .unwrap();
}

#[test]
fn remove_reports_structured_dirty_details_including_stranded_commits() {
    let fixture = fixture();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let project = fixture.destination.join("p");
    fs::write(project.join("source.txt"), "edited\n").unwrap();
    git(&project, &["commit", "-qam", "detached work"]);
    fs::write(project.join("scratch.txt"), "untracked\n").unwrap();

    let status = output_json(
        command(&fixture)
            .args(["-v", "status", "tree", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(status["result"]["dirty"], true);
    let inspected = &status["result"]["projects"][0];
    assert_eq!(inspected["unbranched_commits"], 1);
    assert_eq!(inspected["untracked"], 1);
    assert_eq!(inspected["branch"], Value::Null);

    let refused = output_json(
        command(&fixture)
            .args(["remove", "--json"])
            .arg(&fixture.destination)
            .assert()
            .code(1),
    );
    assert_eq!(refused["error"]["code"], "E_DIRTY");
    let dirty = &refused["error"]["details"]["dirty_projects"][0];
    assert_eq!(dirty["path"], "p");
    assert_eq!(
        dirty["reasons"],
        serde_json::json!(["untracked", "unbranched-commits"])
    );
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unbranched-commits"));

    fs::remove_file(project.join("scratch.txt")).unwrap();
    git(&project, &["branch", "keep"]);
    command(&fixture)
        .arg("remove")
        .arg(&fixture.destination)
        .assert()
        .success();
    assert_eq!(
        git(
            &fixture.client.join("p"),
            &["log", "-1", "--format=%s", "keep"]
        ),
        "detached work"
    );
}

#[test]
fn branch_scope_creates_tracking_branches_only_in_named_projects() {
    let fixture = fixture();
    two_project_snapshot(&fixture);
    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "-b", "topic", "-p", "p/some/subdir", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success()
            .stderr(predicates::str::contains(
                "resolved --project p/some/subdir to project p",
            )),
    );
    assert_eq!(added["result"]["branch_projects"], serde_json::json!(["p"]));
    let p = fixture.destination.join("p");
    let q = fixture.destination.join("q");
    assert_eq!(git(&p, &["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
    assert_eq!(git(&p, &["config", "branch.topic.remote"]), "origin");
    assert_eq!(
        git(&p, &["config", "branch.topic.merge"]),
        "refs/heads/release"
    );
    assert_eq!(git(&q, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");
    assert_eq!(
        git(&fixture.client.join("q"), &["branch", "--list", "topic"]),
        ""
    );

    let listed = output_json(
        command(&fixture)
            .args(["list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(
        listed["result"]["trees"][0]["checked_out_branches"],
        serde_json::json!([{"project": "p", "branch": "topic"}])
    );

    let blocked = StdCommand::new("git")
        .current_dir(fixture.client.join("p"))
        .args(["checkout", "topic"])
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    let dry = output_json(
        command(&fixture)
            .args(["detach", "--branch", "topic", "--dry-run", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(dry["result"]["detached"][0]["location"], "tree");
    assert_eq!(git(&p, &["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
    let detached = output_json(
        command(&fixture)
            .args(["detach", "--branch", "topic", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(detached["result"]["detached"][0]["project"], "p");
    assert_eq!(git(&p, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");
    git(&fixture.client.join("p"), &["checkout", "-q", "topic"]);
    let from_source = output_json(
        command(&fixture)
            .args(["detach", "--source", "-p", "p", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(from_source["result"]["detached"][0]["location"], "source");
    command(&fixture)
        .args(["remove", "tree"])
        .assert()
        .success();
}

#[test]
fn branch_without_scope_is_a_usage_error() {
    let fixture = fixture();
    let refused = output_json(
        command(&fixture)
            .args(["add", "-b", "topic", "--json"])
            .arg(&fixture.destination)
            .assert()
            .code(2),
    );
    assert_eq!(refused["error"]["code"], "E_BRANCH_NEEDS_PROJECT_SCOPE");
    assert!(!fixture.destination.exists());
}

#[test]
fn subpaths_resolve_to_their_project_for_full_project_and_defaults() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--full-project", "p/deep/file.txt", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["full_projects"], serde_json::json!(["p"]));
    let unknown = output_json(
        command(&fixture)
            .args(["add", "--full-project", "nope", "--json"])
            .arg(fixture.temp.path().join("other"))
            .assert()
            .code(2),
    );
    assert_eq!(unknown["error"]["code"], "E_UNKNOWN_PROJECT");
    let defaults = output_json(
        command(&fixture)
            .args(["-v", "defaults", "worktree", "p/deep/file.txt", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(
        defaults["result"]["projects"],
        serde_json::json!({"p": "worktree"})
    );
    command(&fixture)
        .args(["remove", "tree"])
        .assert()
        .success();
}

#[test]
fn pool_projects_reuse_spares_and_clean_projects_return_on_remove() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    let first = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    assert_ne!(first["result"]["projects"][0]["strategy"], "pool");
    fs::write(fixture.client.join("p/.git/info/exclude"), "build-out/\n").unwrap();
    fs::create_dir(fixture.destination.join("p/build-out")).unwrap();
    fs::write(fixture.destination.join("p/build-out/artifact"), "stale\n").unwrap();
    let removed = output_json(
        command(&fixture)
            .args(["remove", "--json", "tree"])
            .assert()
            .success(),
    );
    assert_eq!(
        removed["result"]["returned_to_pool"],
        serde_json::json!(["p"])
    );
    let spares = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(spares["result"]["spares"].as_array().unwrap().len(), 1);

    let second_path = fixture.temp.path().join("second");
    let second = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&second_path)
            .assert()
            .success(),
    );
    assert_eq!(second["result"]["projects"][0]["strategy"], "pool");
    let p = second_path.join("p");
    assert_eq!(git(&p, &["status", "--porcelain"]), "");
    assert!(!p.join("build-out").exists());
    assert_eq!(
        git(&p, &["rev-parse", "HEAD"]),
        second["result"]["projects"][0]["sha"].as_str().unwrap()
    );
    let worktrees = git(
        &fixture.client.join("p"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(worktrees.contains(p.to_str().unwrap()), "{worktrees}");
    let spares = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    assert!(spares["result"]["spares"].as_array().unwrap().is_empty());

    fs::write(p.join("dirty.txt"), "keep me\n").unwrap();
    command(&fixture)
        .args(["remove", "--force", "--no-pool", "second"])
        .assert()
        .success();
    let spares = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    assert!(spares["result"]["spares"].as_array().unwrap().is_empty());
}

#[test]
fn pool_fill_creates_spares_and_clear_removes_their_registrations() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    let filled = output_json(
        command(&fixture)
            .args(["pool", "fill", "--count", "2", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(filled["result"]["filled"].as_array().unwrap().len(), 2);
    let worktrees = git(
        &fixture.client.join("p"),
        &["worktree", "list", "--porcelain"],
    );
    assert_eq!(worktrees.matches(".pool").count(), 2, "{worktrees}");
    command(&fixture).args(["pool", "clear"]).assert().success();
    let worktrees = git(
        &fixture.client.join("p"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(!worktrees.contains(".pool"), "{worktrees}");
}

fn pool_entry(listed: &Value, project: &str) -> Value {
    listed["result"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["project"] == project)
        .unwrap_or_else(|| panic!("{project} missing from {listed}"))
        .clone()
}

#[test]
fn orphaned_spares_are_reported_cleared_and_reaped_while_pooled_ones_refill() {
    let fixture = fixture();
    two_project_snapshot(&fixture);
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    command(&fixture)
        .args(["pool", "fill", "--count", "1", "p", "q"])
        .assert()
        .success();
    let listed = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    let q = pool_entry(&listed, "q");
    assert!(
        q["orphaned"]
            .as_str()
            .unwrap()
            .starts_with("quick to check out"),
        "{q}"
    );
    let p = pool_entry(&listed, "p");
    assert!(p["orphaned"].is_null(), "{p}");
    assert_eq!(p["spares"], 1);
    assert_eq!(listed["result"]["depth"], 1);
    assert_eq!(listed["result"]["capacity"], 2);
    assert_eq!(listed["result"]["orphaned_spares"], 1);
    command(&fixture)
        .args(["pool", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("orphaned: quick to check out"))
        .stdout(predicates::str::contains("depth 1 of 2"));

    let filled = output_json(
        command(&fixture)
            .args(["pool", "fill", "--json"])
            .assert()
            .success(),
    );
    let filled = filled["result"]["filled"].as_array().unwrap();
    assert_eq!(filled.len(), 1);
    assert_eq!(filled[0]["project"], "p");

    let cleared = output_json(
        command(&fixture)
            .args(["pool", "clear", "--orphaned", "--json"])
            .assert()
            .success(),
    );
    let cleared = cleared["result"]["cleared"].as_array().unwrap();
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0]["project"], "q");
    let worktrees = git(
        &fixture.client.join("q"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(!worktrees.contains(".pool"), "{worktrees}");

    command(&fixture)
        .args(["pool", "fill", "--count", "1", "q"])
        .assert()
        .success();
    command(&fixture)
        .args(["pool", "clear", "p"])
        .assert()
        .success();
    let maintained = output_json(
        command(&fixture)
            .args(["pool", "maintain", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(maintained["result"]["reaped"][0]["project"], "q");
    let refilled = maintained["result"]["filled"].as_array().unwrap();
    assert_eq!(refilled.len(), 2, "{maintained}");
    assert!(refilled.iter().all(|spare| spare["project"] == "p"));
    let listed = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(listed["result"]["depth"], 2);
    assert_eq!(listed["result"]["orphaned_spares"], 0);
}

#[test]
fn remove_keeps_spares_only_for_projects_a_default_add_pools() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    command(&fixture)
        .args(["defaults", "auto", "p"])
        .assert()
        .success();
    let removed = output_json(
        command(&fixture)
            .args(["remove", "--json", "tree"])
            .assert()
            .success(),
    );
    assert_eq!(removed["result"]["returned_to_pool"], serde_json::json!([]));
    let worktrees = git(
        &fixture.client.join("p"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(!worktrees.contains(".pool"), "{worktrees}");
}

#[test]
fn add_refills_the_pool_in_the_background() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    command(&fixture)
        .env_remove("GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE")
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let history = fixture.client.join(".repo/repo-worktrees/history.jsonl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let maintained = loop {
        let event = fs::read_to_string(&history)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|event| event["command"] == "pool maintain");
        if let Some(event) = event {
            break event;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background pool maintenance never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(maintained["exit_code"], 0, "{maintained}");
    let add = fs::read_to_string(&history)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["command"] == "add")
        .unwrap();
    assert_eq!(add["pool_maintenance"], "started");
    let listed = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(pool_entry(&listed, "p")["spares"], 2, "{listed}");
    let second = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(fixture.temp.path().join("second"))
            .assert()
            .success(),
    );
    assert_eq!(second["result"]["projects"][0]["strategy"], "pool");
}

#[test]
fn tree_names_reserved_for_the_registry_are_rejected() {
    let fixture = fixture();
    let refused = output_json(
        command(&fixture)
            .args(["add", "--name", ".pool", "--json"])
            .arg(&fixture.destination)
            .assert()
            .code(1),
    );
    assert_eq!(refused["error"]["code"], "E_BAD_NAME");
}

#[cfg(target_os = "linux")]
mod overlay_mode {
    use super::*;

    fn overlay_usable() -> bool {
        let usable = ["fuse-overlayfs", "fusermount3"].iter().all(|program| {
            StdCommand::new("sh")
                .args(["-c", &format!("command -v {program}")])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        }) && Path::new("/dev/fuse").exists();
        if !usable {
            assert!(
                std::env::var_os("GRW_TEST_REQUIRE_OVERLAY").is_none(),
                "GRW_TEST_REQUIRE_OVERLAY is set but fuse-overlayfs is unusable"
            );
            eprintln!("skipping: fuse-overlayfs is not usable here");
        }
        usable
    }

    fn mounted(path: &Path) -> bool {
        fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split(' ').nth(4) == Some(path.to_str().unwrap()))
    }

    fn overlay_fixture() -> Fixture {
        let fixture = fixture();
        two_project_snapshot(&fixture);
        command(&fixture)
            .args(["defaults", "overlay", "p"])
            .assert()
            .success();
        fixture
    }

    fn command(fixture: &Fixture) -> Command {
        let mut command = super::command(fixture);
        command.env_remove("GIT_REPO_WORKTREE_NO_OVERLAY");
        command
    }

    #[test]
    fn fixed_depth_projects_become_overlays_by_default() {
        if !overlay_usable() {
            return;
        }
        let fixture = fixture();
        enable_clone_depth_sharing(&fixture, false);
        let added = output_json(
            command(&fixture)
                .args(["-v", "add", "--json"])
                .arg(&fixture.destination)
                .assert()
                .success(),
        );
        let mode = |path: &str| {
            added["result"]["projects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|project| project["path"] == path)
                .unwrap()["mode"]
                .clone()
        };
        assert_eq!(mode("p"), "overlay");
        assert_eq!(mode("q"), "worktree");
        assert!(mounted(&fixture.destination.join("p")));
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();

        let opted_out = output_json(
            command(&fixture)
                .args(["-v", "add", "--no-overlay", "--json"])
                .arg(&fixture.destination)
                .assert()
                .success(),
        );
        assert_eq!(opted_out["result"]["no_overlay"], true);
        assert_eq!(opted_out["result"]["projects"][0]["mode"], "symlink-shared");
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
    }

    #[test]
    fn remove_warns_about_processes_still_using_the_tree_and_reflink_is_probed_once() {
        let fixture = fixture();
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .assert()
            .success();
        let mut sleeper = StdCommand::new("sleep")
            .arg("60")
            .current_dir(fixture.destination.join("p"))
            .spawn()
            .unwrap();
        let tree = fs::canonicalize(&fixture.destination).unwrap();
        let mut remove = StdCommand::new("sh");
        for name in inherited_tool_variables() {
            remove.env_remove(name);
        }
        let output = remove
            .arg("-c")
            .arg(format!(
                "'{}' -C '{}' remove --force --json tree # {}",
                assert_cmd::cargo::cargo_bin("git-repo-worktree").display(),
                fixture.client.display(),
                tree.display()
            ))
            .env("PATH", &fixture.path_env)
            .env("HOME", fixture.temp.path())
            .env("GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let removed: Value = serde_json::from_slice(&output.stdout).unwrap();
        let lingering = removed["result"]["lingering_processes"].as_array().unwrap();
        assert!(
            lingering
                .iter()
                .all(|process| !process["command"].as_str().unwrap().starts_with("sh -c")),
            "{lingering:?}"
        );
        assert!(
            lingering
                .iter()
                .any(|process| process["pid"] == sleeper.id() && process["reason"] == "cwd"),
            "{lingering:?}"
        );
        sleeper.kill().unwrap();
        let _ = sleeper.wait();
        let history =
            fs::read_to_string(fixture.client.join(".repo/repo-worktrees/history.jsonl")).unwrap();
        assert!(!history.contains("reflink fallback: reflink"), "{history}");
    }

    #[test]
    fn overlay_projects_are_isolated_copy_on_write_worktrees() {
        if !overlay_usable() {
            return;
        }
        let fixture = overlay_fixture();
        let added = output_json(
            command(&fixture)
                .args(["-v", "add", "--json"])
                .arg(&fixture.destination)
                .assert()
                .success(),
        );
        let project = &added["result"]["projects"][0];
        assert_eq!(project["path"], "p");
        assert_eq!(project["mode"], "overlay");
        let p = fixture.destination.join("p");
        assert!(mounted(&p));
        assert_eq!(
            fs::canonicalize(p.join("source.txt")).unwrap(),
            p.join("source.txt")
        );
        assert_eq!(git(&p, &["status", "--porcelain"]), "");
        let worktrees = git(
            &fixture.client.join("p"),
            &["worktree", "list", "--porcelain"],
        );
        assert!(worktrees.contains(p.to_str().unwrap()), "{worktrees}");

        fs::write(p.join("source.txt"), "tree edit\n").unwrap();
        assert_eq!(
            fs::read_to_string(fixture.client.join("p/source.txt")).unwrap(),
            "snapshot\n"
        );
        let status = output_json(
            command(&fixture)
                .args(["-v", "status", "tree", "--json"])
                .assert()
                .success(),
        );
        assert_eq!(status["result"]["projects"][0]["modified"], 1);
        assert_eq!(status["result"]["projects"][0]["mounted"], true);
        command(&fixture).arg("remove").arg("tree").assert().code(1);

        let source = fixture.client.join("p");
        let source_head = git(&source, &["rev-parse", "HEAD"]);
        let source_branch = git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
        git(&p, &["switch", "-q", "-c", "overlay-topic"]);
        git(&p, &["commit", "-qam", "overlay commit"]);
        assert_eq!(
            git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]),
            source_branch
        );
        assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
        assert_eq!(git(&source, &["status", "--porcelain"]), "");
        let clean = output_json(
            command(&fixture)
                .args(["status", "tree", "--json"])
                .assert()
                .success(),
        );
        assert_eq!(clean["result"]["dirty"], false);

        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
        assert!(!mounted(&p));
        assert!(!fixture.destination.exists());
        let worktrees = git(
            &fixture.client.join("p"),
            &["worktree", "list", "--porcelain"],
        );
        assert!(!worktrees.contains(p.to_str().unwrap()), "{worktrees}");
        assert_eq!(
            git(
                &fixture.client.join("p"),
                &["log", "-1", "--format=%s", "overlay-topic"]
            ),
            "overlay commit"
        );
    }

    #[test]
    fn unmounted_overlay_registrations_survive_git_worktree_prune() {
        if !overlay_usable() {
            return;
        }
        let fixture = overlay_fixture();
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .assert()
            .success();
        let p = fixture.destination.join("p");
        let source = fixture.client.join("p");
        assert!(git(&source, &["worktree", "list", "--porcelain"]).contains("locked"));
        assert!(StdCommand::new("fusermount3")
            .arg("-u")
            .arg(&p)
            .status()
            .unwrap()
            .success());
        git(&source, &["worktree", "prune"]);
        command(&fixture)
            .args(["repair", "--mounts-only", "tree"])
            .assert()
            .success();
        assert_eq!(git(&p, &["status", "--porcelain"]), "");
        assert_eq!(
            fs::read_to_string(p.join("source.txt")).unwrap(),
            "snapshot\n"
        );

        command(&fixture)
            .args(["materialize", "tree", "p"])
            .assert()
            .success();
        let listing = git(&source, &["worktree", "list", "--porcelain"]);
        let entry = listing
            .split("\n\n")
            .find(|block| block.contains(p.to_str().unwrap()))
            .unwrap()
            .to_string();
        assert!(!entry.contains("locked"), "{entry}");
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
        let listing = git(&source, &["worktree", "list", "--porcelain"]);
        assert!(!listing.contains(p.to_str().unwrap()), "{listing}");
    }

    #[test]
    fn dead_overlays_are_reported_and_mounts_only_repair_restores_them() {
        if !overlay_usable() {
            return;
        }
        let fixture = overlay_fixture();
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .assert()
            .success();
        let p = fixture.destination.join("p");
        let upper = fixture
            .client
            .join(".repo/repo-worktrees/tree/overlays/p/upper");
        assert!(StdCommand::new("pkill")
            .args(["-9", "-f"])
            .arg(format!("fuse-overlayfs.*{}", upper.display()))
            .status()
            .unwrap()
            .success());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(mounted(&p), "a killed daemon leaves its mount behind");

        let listed = output_json(
            command(&fixture)
                .args(["list", "--json"])
                .assert()
                .success(),
        );
        assert_eq!(
            listed["result"]["trees"][0]["overlays_unmounted"],
            serde_json::json!(["p"]),
            "{listed}"
        );
        let status = output_json(
            command(&fixture)
                .args(["status", "tree", "--json"])
                .assert()
                .success(),
        );
        let project = &status["result"]["projects"][0];
        assert_eq!(project["mounted"], false, "{status}");
        assert!(
            project["warning"].as_str().unwrap().contains("dead"),
            "{status}"
        );
        assert_eq!(status["result"]["dirty"], false, "{status}");

        let before = fs::read(fixture.client.join(".repo/repo-worktrees/tree/meta.json")).unwrap();
        command(&fixture)
            .args(["repair", "--mounts-only", "tree"])
            .assert()
            .success();
        assert_eq!(
            fs::read_to_string(p.join("source.txt")).unwrap(),
            "snapshot\n"
        );
        let after: Value = serde_json::from_slice(
            &fs::read(fixture.client.join(".repo/repo-worktrees/tree/meta.json")).unwrap(),
        )
        .unwrap();
        let before: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(after["projects"], before["projects"]);
        let listed = output_json(
            command(&fixture)
                .args(["list", "--json"])
                .assert()
                .success(),
        );
        assert_eq!(
            listed["result"]["trees"][0]["overlays_unmounted"],
            serde_json::json!([])
        );
        assert!(StdCommand::new("pkill")
            .args(["-9", "-f"])
            .arg(format!("fuse-overlayfs.*{}", upper.display()))
            .status()
            .unwrap()
            .success());
        std::thread::sleep(std::time::Duration::from_millis(300));
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
        assert!(!mounted(&p));
    }

    #[test]
    fn untouched_overlays_are_clean_and_repair_remounts_them() {
        if !overlay_usable() {
            return;
        }
        let fixture = overlay_fixture();
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .assert()
            .success();
        let p = fixture.destination.join("p");
        let upper = fixture
            .client
            .join(".repo/repo-worktrees/tree/overlays/p/upper");
        assert_eq!(
            fs::read_dir(&upper)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from(".git")]
        );
        assert!(StdCommand::new("fusermount3")
            .arg("-u")
            .arg(&p)
            .status()
            .unwrap()
            .success());
        let status = output_json(
            command(&fixture)
                .args(["-v", "status", "tree", "--json"])
                .assert()
                .success(),
        );
        assert_eq!(status["result"]["projects"][0]["mounted"], false);
        assert_eq!(status["result"]["dirty"], false);
        command(&fixture)
            .args(["repair", "tree"])
            .assert()
            .success();
        assert!(mounted(&p));
        assert_eq!(
            fs::read_to_string(p.join("source.txt")).unwrap(),
            "snapshot\n"
        );

        let moved = fixture.temp.path().join("moved");
        command(&fixture)
            .arg("move")
            .arg("tree")
            .arg(&moved)
            .assert()
            .success();
        assert!(mounted(&moved.join("p")));
        assert_eq!(git(&moved.join("p"), &["status", "--porcelain"]), "");
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
        assert!(!moved.exists());
    }

    #[test]
    fn materialize_converts_a_clean_overlay_into_a_checkout() {
        if !overlay_usable() {
            return;
        }
        let fixture = overlay_fixture();
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .assert()
            .success();
        let p = fixture.destination.join("p");
        fs::write(p.join("scratch.txt"), "untracked\n").unwrap();
        command(&fixture)
            .args(["materialize", "tree", "p"])
            .assert()
            .code(3);
        assert!(mounted(&p));
        fs::remove_file(p.join("scratch.txt")).unwrap();
        let converted = output_json(
            command(&fixture)
                .args(["-v", "materialize", "tree", "p", "--json"])
                .assert()
                .success(),
        );
        assert_eq!(
            converted["result"]["materialized"],
            serde_json::json!(["p"])
        );
        assert_eq!(converted["result"]["projects"][0]["mode"], "worktree");
        assert!(!mounted(&p));
        assert_eq!(
            fs::read_to_string(p.join("source.txt")).unwrap(),
            "snapshot\n"
        );
        assert_eq!(git(&p, &["status", "--porcelain"]), "");
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
    }

    #[test]
    fn overlay_falls_back_to_a_worktree_when_the_source_is_dirty() {
        if !overlay_usable() {
            return;
        }
        let fixture = overlay_fixture();
        fs::write(fixture.client.join("p/source.txt"), "dirty source\n").unwrap();
        let added = output_json(
            command(&fixture)
                .args(["-v", "add", "--json"])
                .arg(&fixture.destination)
                .assert()
                .success(),
        );
        assert_eq!(added["result"]["projects"][0]["mode"], "worktree");
        assert_eq!(
            fs::read_to_string(fixture.destination.join("p/source.txt")).unwrap(),
            "snapshot\n"
        );
        command(&fixture)
            .args(["remove", "tree"])
            .assert()
            .success();
    }
}

#[test]
fn history_records_invocations_and_report_summarizes_them() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    let tagged = |fixture: &Fixture| {
        let mut command = command(fixture);
        command.env("GIT_REPO_WORKTREE_SESSION", "scenario-1");
        command
    };
    tagged(&fixture)
        .args(["add", "--name", "secret-name"])
        .arg(&fixture.destination)
        .assert()
        .success();
    fs::write(fixture.destination.join("p/scratch"), "x").unwrap();
    tagged(&fixture)
        .args(["remove", "secret-name"])
        .assert()
        .code(1);
    fs::remove_file(fixture.destination.join("p/scratch")).unwrap();
    tagged(&fixture)
        .args(["remove", "secret-name"])
        .assert()
        .success();
    tagged(&fixture)
        .args(["add", "-b", "topic"])
        .arg(fixture.temp.path().join("other"))
        .assert()
        .code(2);
    tagged(&fixture)
        .args(["add", "--full-all"])
        .arg(fixture.temp.path().join("other"))
        .assert()
        .code(2);
    tagged(&fixture)
        .arg("add")
        .arg(fixture.temp.path().join("second"))
        .assert()
        .success();
    command(&fixture)
        .env("GIT_REPO_WORKTREE_NO_HISTORY", "1")
        .args(["status", "second"])
        .assert()
        .success();

    let history =
        fs::read_to_string(fixture.client.join(".repo/repo-worktrees/history.jsonl")).unwrap();
    assert!(!history.contains(fixture.destination.to_str().unwrap()));
    assert!(!history.contains("\"status\""));
    let add = history
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["command"] == "add")
        .unwrap();
    assert_eq!(add["args"], serde_json::json!(["-C", "add", "--name"]));
    assert_eq!(add["session"], "scenario-1");
    assert!(add["phases"]["projects"].is_u64());
    for phase in [
        "snapshot-repo-manifest",
        "snapshot-git-dirs",
        "snapshot-estimates",
        "snapshot-preflight",
        "projects-journal-sync",
        "projects-idle-slot",
    ] {
        assert!(add["phases"][phase].is_u64(), "{phase}: {add}");
    }
    assert!(add["projects"][0]["ms"].is_u64());
    assert!(add["projects"][0]["queue_ms"].is_u64());
    assert!(add["environment"]["os"].is_string());
    assert!(add["system_start"].is_object());
    assert!(add["system_end"].is_object());
    assert_eq!(add["estimate_cache_hits"], 0);
    let last_add = history
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .rfind(|event| event["command"] == "add" && event["exit_code"] == 0)
        .unwrap();
    assert_eq!(last_add["estimate_cache_hits"], 1, "{last_add}");

    let report = output_json(
        command(&fixture)
            .args(["report", "--session", "scenario-1", "--json"])
            .assert()
            .success(),
    );
    let result = &report["result"];
    assert_eq!(result["events"], 6);
    assert_eq!(result["commands"]["add"]["latency"]["count"], 4);
    assert_eq!(result["commands"]["remove"]["errors"]["E_DIRTY"], 1);
    assert_eq!(result["remove"]["dirty_reasons"]["untracked"], 1);
    assert_eq!(result["remove"]["returned_to_pool"], 1);
    assert_eq!(
        result["add"]["pool"],
        serde_json::json!({"hits": 1, "misses": 1})
    );
    let usage = result["usage_errors"].as_object().unwrap();
    assert!(
        usage.contains_key("-C add -b [E_BRANCH_NEEDS_PROJECT_SCOPE]"),
        "{usage:?}"
    );
    assert!(
        usage.contains_key("-C add --full-all [E_USAGE]"),
        "{usage:?}"
    );
    assert_eq!(result["live_trees"]["count"], 1);
    command(&fixture)
        .arg("report")
        .assert()
        .success()
        .stdout(predicates::str::contains("slowest projects to populate"));
}

fn hold_lock(path: &Path) -> &'static mut fd_lock::RwLock<fs::File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    Box::leak(Box::new(fd_lock::RwLock::new(file)))
}

fn wait_for_maintenance(fixture: &Fixture, count: usize) -> Value {
    let history = fixture.client.join(".repo/repo-worktrees/history.jsonl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let events = fs::read_to_string(&history)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["command"] == "pool maintain")
            .collect::<Vec<_>>();
        if events.len() >= count {
            return events[count - 1].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background maintenance never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn remove_deletes_files_in_the_background() {
    let fixture = fixture();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    fs::write(fixture.destination.join("p/build-output"), "ignored\n").unwrap();
    fs::write(fixture.client.join("p/.git/info/exclude"), "build-output\n").unwrap();
    let removed = output_json(
        command(&fixture)
            .env_remove("GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE")
            .args(["remove", "--json", "tree"])
            .assert()
            .success(),
    );
    assert_eq!(removed["result"]["deleting_in_background"], true);
    assert!(!fixture.destination.exists());
    let worktrees = git(
        &fixture.client.join("p"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(
        !worktrees.contains(fixture.destination.to_str().unwrap()),
        "{worktrees}"
    );
    assert!(!worktrees.contains("prunable"), "{worktrees}");
    let maintained = wait_for_maintenance(&fixture, 1);
    assert_eq!(maintained["trees_deleted"], 1, "{maintained}");
    let trash = fixture.client.join(".repo/repo-worktrees/.trash");
    assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);

    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let refused = output_json(
        command(&fixture)
            .args(["add", "--name", ".trash", "--json"])
            .arg(fixture.temp.path().join("other"))
            .assert()
            .code(1),
    );
    assert_eq!(refused["error"]["code"], "E_BAD_NAME");
}

#[test]
fn pool_maintenance_waits_for_tree_operations() {
    let fixture = fixture();
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let tree_lock = hold_lock(&fixture.client.join(".repo/repo-worktrees/tree/lock"));
    let guard = tree_lock.try_write().unwrap();
    let mut maintain = StdCommand::new(assert_cmd::cargo::cargo_bin("git-repo-worktree"));
    for name in inherited_tool_variables() {
        maintain.env_remove(name);
    }
    let maintain = maintain
        .env("PATH", &fixture.path_env)
        .env("HOME", fixture.temp.path())
        .env("GIT_REPO_WORKTREE_NO_OVERLAY", "1")
        .env("GRW_TEST_POOL_RESERVE_BYTES", "0")
        .arg("-C")
        .arg(&fixture.client)
        .args(["pool", "maintain", "--json"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let listed = output_json(
        command(&fixture)
            .args(["pool", "list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(pool_entry(&listed, "p")["spares"], 0, "{listed}");
    drop(guard);
    let output = maintain.wait_with_output().unwrap();
    let maintained: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        maintained["result"]["filled"].as_array().unwrap().len(),
        2,
        "{maintained}"
    );
    let event = fs::read_to_string(fixture.client.join(".repo/repo-worktrees/history.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|event| event["command"] == "pool maintain")
        .unwrap();
    assert!(
        event["phases"]["paused"].as_u64().unwrap() >= 1000,
        "{event}"
    );
}

#[test]
fn readers_never_block_and_writers_wait_for_busy_trees() {
    let fixture = fixture();
    command(&fixture)
        .arg("add")
        .arg(&fixture.destination)
        .assert()
        .success();
    let tree_lock = hold_lock(&fixture.client.join(".repo/repo-worktrees/tree/lock"));
    let registry_lock = hold_lock(&fixture.client.join(".repo/repo-worktrees/lock"));
    let tree_guard = tree_lock.try_write().unwrap();
    let registry_guard = registry_lock.try_write().unwrap();

    command(&fixture)
        .args(["list", "--json"])
        .assert()
        .success();
    command(&fixture)
        .args(["status", "tree", "--json"])
        .assert()
        .success();
    command(&fixture)
        .args(["report", "--json"])
        .assert()
        .success();
    let started = std::time::Instant::now();
    let refused = output_json(
        command(&fixture)
            .args(["lock", "tree", "--lock-timeout", "1", "--json"])
            .assert()
            .code(4),
    );
    assert_eq!(refused["error"]["code"], "E_LOCKED");
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    drop(registry_guard);
    let other = fixture.temp.path().join("other");
    command(&fixture)
        .arg("add")
        .arg(&other)
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .success();

    let mut waiting = command(&fixture);
    waiting.args(["lock", "tree", "--reason", "waited"]);
    let handle = std::thread::spawn(move || waiting.assert().success());
    std::thread::sleep(std::time::Duration::from_millis(1500));
    drop(tree_guard);
    handle
        .join()
        .unwrap()
        .stderr(predicates::str::contains("waiting for tree tree"));
    let listed = output_json(
        command(&fixture)
            .args(["list", "--json"])
            .assert()
            .success(),
    );
    let tree = listed["result"]["trees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tree| tree["name"] == "tree")
        .unwrap()
        .clone();
    assert_eq!(tree["locked"]["reason"], "waited");
}

#[test]
fn branched_projects_use_the_pool_and_spares_follow_the_pool_size() {
    let fixture = fixture();
    two_project_snapshot(&fixture);
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    let first = fixture.temp.path().join("first");
    let second = fixture.temp.path().join("second");
    for path in [&first, &second] {
        command(&fixture).arg("add").arg(path).assert().success();
    }
    for name in ["first", "second"] {
        command(&fixture).args(["remove", name]).assert().success();
    }
    let size = output_json(
        command(&fixture)
            .args(["pool", "size", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(size["result"]["spares"], 2);
    let spares = output_json(
        command(&fixture)
            .args(["pool", "status", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(spares["result"]["spares"].as_array().unwrap().len(), 2);

    let branched = output_json(
        command(&fixture)
            .args(["-v", "add", "-b", "topic", "-p", "p", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success(),
    );
    let p = &branched["result"]["projects"][0];
    assert_eq!(p["path"], "p");
    assert_eq!(p["strategy"], "pool");
    let checkout = fixture.destination.join("p");
    assert_eq!(
        git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "topic"
    );
    assert_eq!(
        git(&checkout, &["config", "branch.topic.merge"]),
        "refs/heads/release"
    );
    command(&fixture)
        .args(["remove", "tree"])
        .assert()
        .success();
    assert_eq!(
        git(&fixture.client.join("p"), &["branch", "--list", "topic"]),
        "topic"
    );

    command(&fixture)
        .args(["pool", "size", "1"])
        .assert()
        .success();
    command(&fixture).args(["pool", "clear"]).assert().success();
    command(&fixture)
        .args(["defaults", "auto", "p"])
        .assert()
        .success();
    let unknown = output_json(
        command(&fixture)
            .args(["pool", "fill", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(unknown["error"]["code"], "E_NO_POOL_PROJECTS");
    command(&fixture)
        .args(["defaults", "pool", "p"])
        .assert()
        .success();
    command(&fixture).arg("add").arg(&first).assert().success();
    let filled = output_json(
        command(&fixture)
            .args(["pool", "fill", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(filled["result"]["filled"].as_array().unwrap().len(), 1);
    assert_eq!(filled["result"]["filled"][0]["project"], "p");
    command(&fixture)
        .args(["defaults", "auto", "p"])
        .assert()
        .success();
    let orphaned = output_json(
        command(&fixture)
            .args(["pool", "fill", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(orphaned["error"]["code"], "E_NO_POOL_PROJECTS");
}

#[test]
fn version_reports_the_build_commit() {
    let fixture = fixture();
    let version = output_json(
        command(&fixture)
            .args(["version", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(version["result"]["version"], env!("CARGO_PKG_VERSION"));
    if Path::new(env!("CARGO_MANIFEST_DIR")).join(".git").exists() {
        assert!(version["result"]["commit"].is_string());
    }
}

fn install_hook(fixture: &Fixture, event: &str, body: &str, executable: bool) {
    let directory = fixture.client.join(".repo/repo-worktrees/hooks");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(event);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn site_hooks_run_after_add_and_before_remove_without_changing_outcomes() {
    let fixture = fixture();
    install_hook(
        &fixture,
        "post-add",
        r#"echo "hook says hello"
printf '%s|%s|%s|%s|%s\n' "$PWD" "$GIT_REPO_WORKTREE_TREE" "$GIT_REPO_WORKTREE_NAME" "$GIT_REPO_WORKTREE_EVENT" "$GIT_REPO_WORKTREE_SOURCE" > site-env.txt"#,
        true,
    );
    let marker = fixture.temp.path().join("pre-remove-saw");
    install_hook(
        &fixture,
        "pre-remove",
        &format!(
            r#"test -f site-env.txt && echo "$GIT_REPO_WORKTREE_EVENT" > '{}'"#,
            marker.display()
        ),
        true,
    );
    let listed = output_json(
        command(&fixture)
            .args(["hooks", "list", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(listed["result"]["hooks"][0]["event"], "post-add");
    assert_eq!(listed["result"]["hooks"][0]["executable"], true);

    let assert = command(&fixture)
        .args(["add", "--json"])
        .arg(&fixture.destination)
        .assert()
        .success()
        .stderr(predicates::str::contains("hook says hello"))
        .stderr(predicates::str::contains("ran post-add hook"));
    let added = output_json(assert);
    assert_eq!(added["result"]["hooks"]["post-add"]["status"], "ok");
    assert!(added["result"].get("projects").is_none());
    assert_eq!(added["result"]["project_counts"]["worktree"], 1);
    assert_eq!(added["result"]["failed_projects"], serde_json::json!([]));
    let root = fs::canonicalize(&fixture.destination).unwrap();
    let source = fs::canonicalize(&fixture.client).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("site-env.txt"))
            .unwrap()
            .trim(),
        format!(
            "{}|{}|tree|post-add|{}",
            root.display(),
            root.display(),
            source.display()
        )
    );

    let removed = output_json(
        command(&fixture)
            .args(["remove", "--force", "--json", "tree"])
            .assert()
            .success(),
    );
    assert_eq!(removed["result"]["hooks"]["pre-remove"]["status"], "ok");
    assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "pre-remove");

    let report = output_json(
        command(&fixture)
            .args(["report", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(report["result"]["hooks"]["post-add"]["statuses"]["ok"], 1);
    assert_eq!(report["result"]["hooks"]["pre-remove"]["statuses"]["ok"], 1);
}

#[test]
fn failing_and_slow_hooks_are_recorded_and_repair_retries_post_add() {
    let fixture = fixture();
    install_hook(&fixture, "post-add", "echo broken >&2\nexit 3", true);
    let added = output_json(
        command(&fixture)
            .args(["-v", "add", "--json"])
            .arg(&fixture.destination)
            .assert()
            .success()
            .stderr(predicates::str::contains("warning: post-add hook failed")),
    );
    assert_eq!(added["result"]["state"], "complete");
    assert_eq!(added["result"]["hooks"]["post-add"]["status"], "failed");
    assert_eq!(added["result"]["hooks"]["post-add"]["exit_code"], 3);

    install_hook(&fixture, "post-add", "touch fixed", true);
    let repaired = output_json(
        command(&fixture)
            .args(["repair", "tree", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(repaired["result"]["hooks"]["post-add"]["status"], "ok");
    assert!(fixture.destination.join("fixed").exists());
    fs::remove_file(fixture.destination.join("fixed")).unwrap();
    command(&fixture)
        .args(["repair", "tree"])
        .assert()
        .success();
    assert!(!fixture.destination.join("fixed").exists());

    install_hook(&fixture, "post-add", "sleep 10", true);
    let second = fixture.temp.path().join("slow");
    let started = std::time::Instant::now();
    let slow = output_json(
        command(&fixture)
            .env("GIT_REPO_WORKTREE_HOOK_TIMEOUT", "1")
            .args(["add", "--json"])
            .arg(&second)
            .assert()
            .success(),
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    assert_eq!(slow["result"]["hooks"]["post-add"]["status"], "timeout");

    install_hook(&fixture, "post-add", "true", false);
    let third = fixture.temp.path().join("plain");
    let plain = output_json(
        command(&fixture)
            .args(["add", "--json"])
            .arg(&third)
            .assert()
            .success(),
    );
    assert!(plain["result"]["hooks"]["post-add"]["reason"]
        .as_str()
        .unwrap()
        .contains("chmod +x"));

    install_hook(&fixture, "post-add", "touch ran", true);
    let skipped_path = fixture.temp.path().join("skipped");
    let skipped = output_json(
        command(&fixture)
            .args(["add", "--no-hooks", "--json"])
            .arg(&skipped_path)
            .assert()
            .success(),
    );
    assert_eq!(skipped["result"]["hooks"]["post-add"]["status"], "skipped");
    assert!(!skipped_path.join("ran").exists());
    let repaired = output_json(
        command(&fixture)
            .args(["repair", "skipped", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(repaired["result"]["hooks"]["post-add"]["status"], "ok");
    assert!(skipped_path.join("ran").exists());
    assert!(repaired["result"].get("projects").is_none());
    assert!(repaired["result"]["project_counts"]["worktree"].is_u64());
}
