use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn command(home: &TempDir, cwd: &TempDir) -> Command {
    let mut command = Command::cargo_bin("git-repo-worktree").unwrap();
    command
        .current_dir(cwd.path())
        .env("HOME", home.path())
        .env_remove("GRW_SKILL_DIRS");
    command
}

fn json_stdout(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

#[test]
fn no_framework_roots_reports_remedies() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let value = json_stdout(
        command(&home, &cwd)
            .args(["skill", "add", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(value["error"]["code"], "E_NO_SKILL_TARGET");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("mkdir -p"));
    assert!(!home.path().join(".agents").exists());
}

#[test]
fn user_add_is_idempotent_and_protects_foreign_files() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();

    let first = json_stdout(
        command(&home, &cwd)
            .args(["skill", "add", "--scope", "user", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(first["result"]["actions"][0]["action"], "installed");
    let path = home
        .path()
        .join(".claude/skills/git-repo-worktree/SKILL.md");
    let installed = fs::read_to_string(&path).unwrap();
    assert!(installed.contains("x-installed-by: git-repo-worktree"));
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    thread::sleep(Duration::from_millis(20));

    let second = json_stdout(
        command(&home, &cwd)
            .args(["skill", "add", "--scope", "user", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(second["result"]["actions"][0]["action"], "unchanged");
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);

    fs::write(&path, "---\nname: mine\ndescription: mine\n---\nowned\n").unwrap();
    let blocked = json_stdout(
        command(&home, &cwd)
            .args(["skill", "add", "--scope", "user", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(blocked["error"]["code"], "E_SKILL_FOREIGN");
    command(&home, &cwd)
        .args(["skill", "add", "--scope", "user", "--force", "--json"])
        .assert()
        .success();
}

#[test]
fn agents_alias_covers_gemini_but_explicit_target_overrides() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    fs::create_dir(home.path().join(".agents")).unwrap();
    fs::create_dir(home.path().join(".gemini")).unwrap();

    let value = json_stdout(
        command(&home, &cwd)
            .args(["skill", "add", "--scope", "user", "--json"])
            .assert()
            .success(),
    );
    let actions = value["result"]["actions"].as_array().unwrap();
    assert!(actions
        .iter()
        .any(|action| action["id"] == "agents-user" && action["action"] == "installed"));
    assert!(actions
        .iter()
        .any(|action| action["id"] == "gemini-user" && action["action"] == "skipped-alias"));
    assert!(home
        .path()
        .join(".agents/skills/git-repo-worktree/SKILL.md")
        .is_file());
    assert!(!home
        .path()
        .join(".gemini/skills/git-repo-worktree/SKILL.md")
        .exists());

    command(&home, &cwd)
        .args(["skill", "add", "--target", "gemini-user", "--json"])
        .assert()
        .success();
    assert!(home
        .path()
        .join(".gemini/skills/git-repo-worktree/SKILL.md")
        .is_file());

    command(&home, &cwd)
        .args(["skill", "remove", "--json"])
        .assert()
        .success();
    assert!(!home
        .path()
        .join(".agents/skills/git-repo-worktree/SKILL.md")
        .exists());
    assert!(!home
        .path()
        .join(".gemini/skills/git-repo-worktree/SKILL.md")
        .exists());
}

#[test]
fn antigravity_uses_config_skills_only_with_specific_evidence() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join(".gemini/antigravity")).unwrap();
    command(&home, &cwd)
        .args(["skill", "add", "--target", "antigravity-user", "--json"])
        .assert()
        .success();
    assert!(home
        .path()
        .join(".gemini/config/skills/git-repo-worktree/SKILL.md")
        .is_file());

    let other_home = TempDir::new().unwrap();
    fs::create_dir(other_home.path().join(".gemini")).unwrap();
    let value = json_stdout(
        command(&other_home, &cwd)
            .args(["skill", "add", "--target", "antigravity-user", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(value["error"]["code"], "E_NO_SKILL_TARGET");
}

#[test]
fn show_is_clean_and_manual_copy_is_foreign() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = command(&home, &cwd)
        .args(["skill", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&output).contains("x-installed-by"));
    let path = home
        .path()
        .join(".agents/skills/git-repo-worktree/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, output).unwrap();
    let value = json_stdout(
        command(&home, &cwd)
            .args(["skill", "list", "--json"])
            .assert()
            .success(),
    );
    let agents = value["result"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == "agents-user")
        .unwrap();
    assert_eq!(agents["foreign"], true);
}

#[test]
fn sync_updates_every_installed_copy_to_test_version() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    fs::create_dir(home.path().join(".gemini")).unwrap();
    command(&home, &cwd)
        .args(["skill", "add", "--scope", "user"])
        .assert()
        .success();

    let value = json_stdout(
        command(&home, &cwd)
            .env("GRW_TEST_VERSION", "9.8.7")
            .args(["skill", "sync", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(value["result"]["actions"].as_array().unwrap().len(), 2);
    for relative in [
        ".claude/skills/git-repo-worktree/SKILL.md",
        ".gemini/skills/git-repo-worktree/SKILL.md",
    ] {
        assert!(fs::read_to_string(home.path().join(relative))
            .unwrap()
            .contains("x-installed-version: \"9.8.7\""));
    }
}

#[test]
fn project_scope_error_uses_dash_c_client_root() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let client = cwd.path().join("client");
    fs::create_dir_all(client.join(".repo/manifests")).unwrap();
    let value = json_stdout(
        command(&home, &cwd)
            .arg("-C")
            .arg(&client)
            .args(["skill", "add", "--scope", "project", "--json"])
            .assert()
            .code(1),
    );
    assert_eq!(value["error"]["code"], "E_NO_SKILL_TARGET");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains(&client.join(".agents/skills").to_string_lossy().to_string()));
}

#[test]
fn json_usage_errors_still_emit_one_envelope() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let value = json_stdout(
        command(&home, &cwd)
            .args(["skill", "add", "--scope", "user", "--all", "--json"])
            .assert()
            .code(2),
    );
    assert_eq!(value["command"], "skill add");
    assert_eq!(value["error"]["code"], "E_USAGE");
    assert_eq!(value["exit_code"], 2);
}
