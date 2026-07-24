use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
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
            .arg("add")
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
    value["format_version"] = Value::from(3);
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let output = output_json(command(&fixture).args(["list", "--json"]).assert().code(1));
    assert_eq!(output["error"]["code"], "E_FORMAT_TOO_NEW");
}

#[test]
fn default_add_shares_clean_fixed_depth_projects_and_remove_preserves_source() {
    let fixture = fixture();
    enable_clone_depth_sharing(&fixture, false);
    let assert = command(&fixture)
        .args(["add", "--json"])
        .arg(&fixture.destination)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(stderr.contains(
        "sharing 1 clone-depth project(s) with the source checkout; materialize one before editing it"
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
            .args(["add", "--continue", "shared-crash", "--json"])
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
        add.arg("add").arg("--json");
        match control {
            "full" => {
                add.arg("--full");
            }
            "full-project" => {
                add.args(["--full-project", "p"]);
            }
            "branch" => {
                add.args(["-b", "topic/share-test"]);
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
            .args(["materialize", "tree", "p", "--json"])
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
    let not_shared = output_json(
        command(&fixture)
            .args(["materialize", "tree", "q", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(not_shared["error"]["code"], "E_NOT_SHARED");

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
            .args(["materialize", "tree", "p", "--json"])
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
                .args(["add", "--json"])
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
        add.args(["add", "--json"]);
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
            .args(["add", "--json"])
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
            .args(["add", "--json"])
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
        add.args(["add", "--json"]);
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
            .args(["add", "--json"])
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
            .args(["add", "--continue", "tree", "--json"])
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
            .args(["add", "--continue", "tree", "--json"])
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
