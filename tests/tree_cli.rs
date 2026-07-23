use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use tempfile::TempDir;

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
    command
        .env("PATH", &fixture.path_env)
        .env("GRW_TEST_SNAPSHOT", &fixture.snapshot)
        .env("HOME", fixture.temp.path())
        .arg("-C")
        .arg(&fixture.client);
    command
}

fn output_json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

#[test]
fn happy_path_list_move_lock_unlock_and_remove() {
    let fixture = fixture();
    let added = output_json(
        command(&fixture)
            .arg("add")
            .arg(&fixture.destination)
            .arg("--json")
            .assert()
            .success(),
    );
    assert_eq!(added["result"]["state"], "complete");
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
            .args(["add", "-b", "topic/test", "--name", "branch-tree"])
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
            .args(["add", "--continue", "branch-tree", "--json"])
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
            .args(["add", "--continue", "crashed", "--json"])
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
    value["format_version"] = Value::from(2);
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let output = output_json(command(&fixture).args(["list", "--json"]).assert().code(1));
    assert_eq!(output["error"]["code"], "E_FORMAT_TOO_NEW");
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
