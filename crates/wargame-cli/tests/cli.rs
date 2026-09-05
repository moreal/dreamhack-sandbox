//! Black-box acceptance tests for the `wargame` binary (PLAN.md Task 7,
//! Step 1). Each test spawns the real compiled binary
//! (`env!("CARGO_BIN_EXE_wargame")`) against a fresh `$WARGAME_HOME`
//! tempdir, so nothing here shares state across tests or with a
//! developer's real `~/Library/Application Support/wargame-vm`.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wargame")
}

/// A command wired to `WARGAME_HOME=<home>`, `WARGAME_PROVIDER=fixture`
/// (no Dreamhack account/network needed) and `WARGAME_VM=fake` (no Tart
/// needed; every built-in profile's image is pre-registered so `launch`
/// needs no prior `profile build`).
fn fixture_fake_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(bin());
    cmd.env("WARGAME_HOME", home)
        .env("WARGAME_PROVIDER", "fixture")
        .env("WARGAME_VM", "fake");
    cmd
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn combined_of(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn help_lists_every_command() {
    let out = Command::new(bin()).arg("--help").output().unwrap();
    assert!(out.status.success(), "{:?}", out);
    let stdout = stdout_of(&out);
    for name in [
        "search", "open", "launch", "sessions", "session", "ssh", "gui", "stop", "resume", "keep",
        "delete", "prune", "doctor", "profiles", "profile", "auth",
    ] {
        assert!(
            stdout.contains(name),
            "--help is missing `{name}`:\n{stdout}"
        );
    }
}

#[test]
fn no_subcommand_reports_tui_not_built_and_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout_of(&out).contains("TUI not built yet"));
}

#[test]
fn profiles_json_lists_both_builtin_profiles() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["profiles", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);

    let profiles: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let names: Vec<String> = profiles
        .as_array()
        .expect("profiles --json prints a JSON array")
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"macos-minimal-arm64".to_string()));
    assert!(names.contains(&"macos-re-arm64".to_string()));
}

#[test]
fn search_with_fixture_provider_returns_fixture_one() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["search", "hello", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);

    let page: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["ref"]["provider"], "fixture");
    assert_eq!(items[0]["ref"]["id"], "1");
}

#[test]
fn launch_then_sessions_then_delete_round_trip() {
    let home = tempfile::tempdir().unwrap();

    let launch_out = fixture_fake_cmd(home.path())
        .args([
            "launch",
            "fixture:1",
            "--profile",
            "macos-minimal-arm64",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(launch_out.status.success(), "{:?}", launch_out);
    let launch_stdout = stdout_of(&launch_out);
    assert!(
        launch_stdout.contains("Ready"),
        "launch output did not mention Ready:\n{launch_stdout}"
    );

    let sessions_out = fixture_fake_cmd(home.path())
        .args(["sessions", "--json"])
        .output()
        .unwrap();
    assert!(sessions_out.status.success(), "{:?}", sessions_out);
    let sessions: serde_json::Value = serde_json::from_str(&stdout_of(&sessions_out)).unwrap();
    let sessions = sessions.as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["state"], "ready");
    let sha256 = sessions[0]["assets"][0]["sha256"]
        .as_str()
        .expect("assets[0].sha256 must be present");
    assert!(!sha256.is_empty());
    let id = sessions[0]["id"].as_str().unwrap().to_string();

    let delete_out = fixture_fake_cmd(home.path())
        .args(["delete", &id])
        .output()
        .unwrap();
    assert!(delete_out.status.success(), "{:?}", delete_out);

    let sessions_out2 = fixture_fake_cmd(home.path())
        .args(["sessions", "--json"])
        .output()
        .unwrap();
    let sessions2: serde_json::Value = serde_json::from_str(&stdout_of(&sessions_out2)).unwrap();
    assert!(
        sessions2.as_array().unwrap().is_empty(),
        "session should have been deleted: {sessions2}"
    );
}

#[test]
fn doctor_json_is_an_array_and_exits_zero_even_without_tart() {
    let home = tempfile::tempdir().unwrap();
    // Fixture provider only (no Dreamhack/Keychain traffic) but no
    // WARGAME_VM override, so this exercises the real Tart backend check --
    // which should report unavailable, not error out, when `tart` isn't on
    // PATH.
    let out = Command::new(bin())
        .env("WARGAME_HOME", home.path())
        .env("WARGAME_PROVIDER", "fixture")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let checks: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    assert!(
        !checks.as_array().unwrap().is_empty(),
        "doctor --json should return a non-empty array"
    );
}

#[test]
fn launch_nat_without_yes_or_tty_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["launch", "fixture:1", "--network", "nat"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let combined = combined_of(&out);
    assert!(
        combined.contains("Continue"),
        "expected a NAT confirmation prompt mentioning \"Continue\":\n{combined}"
    );
}
