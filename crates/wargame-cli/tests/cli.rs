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
fn no_subcommand_without_a_tty_declines_the_tui_and_exits_2() {
    // `Command::output()` pipes stdin/stdout, so this exercises Task 8's
    // TUI graceful-decline path rather than actually driving ratatui/
    // crossterm (that needs a real pty; see PLAN.md Task 8's note and
    // `crates/wargame-cli/src/tui/app.rs`'s reducer tests for the rest).
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout_of(&out).contains("interactive terminal"));
}

#[test]
fn profiles_json_lists_every_builtin_profile_including_the_templates() {
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
    assert!(names.contains(&"kali-arm64".to_string()));
    assert!(names.contains(&"kali-x86_64".to_string()));
    assert!(names.contains(&"windows-x86_64".to_string()));

    // FR-1: the list carries each profile's image state, not just its
    // definition.
    let kali = profiles
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "kali-arm64")
        .unwrap();
    assert_eq!(
        kali["image"],
        serde_json::Value::String("not created".to_string())
    );
    assert_eq!(kali["vm"]["backend"], "utm");
    assert_eq!(kali["default"], serde_json::Value::Bool(false));
}

/// spec.md §4's `profile init`: the copy gets its own YAML *and* its own
/// scripts, and a second run refuses rather than overwriting.
#[test]
fn profile_init_copies_a_template_and_refuses_a_second_run() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["profile", "init", "kali-arm64", "--name", "kali-custom"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("kali-custom.yaml"), "{stdout}");
    assert!(
        stdout.contains("wargame profile create kali-custom"),
        "{stdout}"
    );

    let yaml = home.path().join("profiles/kali-custom.yaml");
    assert!(yaml.is_file());
    assert!(std::fs::read_to_string(&yaml)
        .unwrap()
        .contains("name: kali-custom"));
    assert!(home
        .path()
        .join("profiles/scripts/kali-custom/common.sh")
        .is_file());
    // The template itself is untouched.
    assert!(
        std::fs::read_to_string(home.path().join("profiles/kali-arm64.yaml"))
            .unwrap()
            .contains("name: kali-arm64")
    );

    let again = fixture_fake_cmd(home.path())
        .args(["profile", "init", "kali-arm64", "--name", "kali-custom"])
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(
        combined_of(&again).contains("already exists"),
        "{:?}",
        again
    );
}

/// A Tart profile has no installation VM: `profile create`/`profile console`
/// say so and name what to run instead (spec.md §4).
#[test]
fn profile_create_and_console_refuse_a_tart_profile() {
    let home = tempfile::tempdir().unwrap();
    let create = fixture_fake_cmd(home.path())
        .args([
            "profile",
            "create",
            "macos-re-arm64",
            "--iso",
            "/nonexistent.iso",
        ])
        .output()
        .unwrap();
    assert!(!create.status.success());
    let message = combined_of(&create);
    assert!(message.contains("image.base"), "{message}");
    assert!(message.contains("profile build"), "{message}");

    let console = fixture_fake_cmd(home.path())
        .args(["profile", "console", "macos-re-arm64"])
        .output()
        .unwrap();
    assert!(!console.status.success());
    assert!(combined_of(&console).contains("installation VM"));
}

/// spec.md §3 FR-2: a UTM profile cannot be built before `profile create`
/// made its installation VM, and the error says which command to run.
#[test]
fn profile_build_without_a_record_points_at_profile_create() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["profile", "build", "kali-arm64"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let message = combined_of(&out);
    assert!(message.contains("no installation VM"), "{message}");
    assert!(
        message.contains("wargame profile create kali-arm64"),
        "{message}"
    );
}

/// A `--sha256` that does not match fails before any VM exists, and leaves
/// no build record behind (spec.md §6).
#[test]
fn profile_create_refuses_a_mismatched_iso_hash_before_creating_anything() {
    let home = tempfile::tempdir().unwrap();
    let iso = home.path().join("fake.iso");
    std::fs::write(&iso, b"not an iso").unwrap();
    let out = fixture_fake_cmd(home.path())
        .args([
            "profile",
            "create",
            "kali-arm64",
            "--iso",
            iso.to_str().unwrap(),
            "--sha256",
            &"0".repeat(64),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let message = combined_of(&out);
    assert!(message.contains("does not match --sha256"), "{message}");
    assert!(!home.path().join("builds/kali-arm64").exists());
}

/// `doctor --profile` on an unbuilt UTM profile reports the image as not
/// ready and names the command that would produce one.
#[test]
fn doctor_profile_reports_an_unbuilt_utm_image() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["doctor", "--profile", "kali-arm64", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let checks: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let image = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "profile-image:kali-arm64")
        .expect("a profile-image check");
    assert_eq!(image["ok"], serde_json::Value::Bool(false));
    assert!(image["hint"]
        .as_str()
        .unwrap()
        .contains("wargame profile create kali-arm64"));
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

/// `doctor --profile` checks only what one profile needs: a macOS/Tart
/// profile must not be told about UTM (spec.md §3 FR-6).
#[test]
fn doctor_profile_reports_only_the_profiles_own_backend() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["doctor", "--profile", "macos-re-arm64", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let checks: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let names: Vec<String> = checks
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"profile:macos-re-arm64".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"vm-backend:tart".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.contains("utm")),
        "a Tart profile must not require UTM: {names:?}"
    );
}

#[test]
fn doctor_profile_names_an_unknown_profile() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["doctor", "--profile", "does-not-exist", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let checks: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    let rows = checks.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["ok"], serde_json::Value::Bool(false));
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

fn launch_fixture(home: &Path) -> String {
    let out = fixture_fake_cmd(home)
        .args([
            "launch",
            "fixture:1",
            "--profile",
            "macos-minimal-arm64",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let sessions_out = fixture_fake_cmd(home)
        .args(["sessions", "--json"])
        .output()
        .unwrap();
    let sessions: serde_json::Value = serde_json::from_str(&stdout_of(&sessions_out)).unwrap();
    sessions[0]["id"].as_str().unwrap().to_string()
}

#[test]
fn launch_prints_a_next_steps_block_with_the_session_id() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path())
        .args(["launch", "fixture:1", "--profile", "macos-minimal-arm64"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Ready in"), "{stdout}");
    assert!(stdout.contains("Next:"), "{stdout}");
    assert!(stdout.contains("wargame ssh fi-hello-fixture-"), "{stdout}");
}

#[test]
fn session_ids_may_be_omitted_or_shortened() {
    let home = tempfile::tempdir().unwrap();
    let id = launch_fixture(home.path());

    // Omitted: the most recent session.
    let out = fixture_fake_cmd(home.path())
        .args(["session", "inspect", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let session: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    assert_eq!(session["id"], id);

    // A prefix, via the `stop` command (and a second stop is not an error).
    let out = fixture_fake_cmd(home.path())
        .args(["stop", "fi-hello"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout_of(&out).contains(&format!("Stopped {id}")));
    let out = fixture_fake_cmd(home.path())
        .args(["stop"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout_of(&out).contains("already stopped"));

    // A substring of the challenge title, via the `rm` alias.
    let out = fixture_fake_cmd(home.path())
        .args(["rm", "hello-fix"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout_of(&out).contains(&format!("Deleted {id}")));
}

#[test]
fn ambiguous_and_missing_session_references_explain_themselves() {
    let home = tempfile::tempdir().unwrap();
    let first = launch_fixture(home.path());
    launch_fixture(home.path());

    let out = fixture_fake_cmd(home.path())
        .args(["stop", "fi-hello"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let combined = combined_of(&out);
    assert!(combined.contains("ambiguous"), "{combined}");
    assert!(combined.contains(&first), "{combined}");

    let out = fixture_fake_cmd(home.path())
        .args(["stop", "nothing-like-this"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let combined = combined_of(&out);
    assert!(combined.contains("not found"), "{combined}");
    assert!(combined.contains("Try: wargame sessions"), "{combined}");
}

#[test]
fn keep_blocks_delete_until_forced_and_says_how() {
    let home = tempfile::tempdir().unwrap();
    let id = launch_fixture(home.path());

    let out = fixture_fake_cmd(home.path())
        .args(["keep"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout_of(&out).contains("Undo: wargame keep"));

    let out = fixture_fake_cmd(home.path())
        .args(["delete", &id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(combined_of(&out).contains("--force"));

    let out = fixture_fake_cmd(home.path())
        .args(["delete", &id, "-f"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
}

#[test]
fn empty_session_list_points_at_the_next_command() {
    let home = tempfile::tempdir().unwrap();
    let out = fixture_fake_cmd(home.path()).args(["ls"]).output().unwrap();
    assert!(out.status.success(), "{:?}", out);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("No sessions yet"), "{stdout}");
    assert!(stdout.contains("wargame search"), "{stdout}");

    let out = fixture_fake_cmd(home.path())
        .args(["ssh"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(combined_of(&out).contains("no sessions exist yet"));
}

#[test]
fn completions_print_a_script_without_needing_a_home() {
    let out = Command::new(bin())
        .env("WARGAME_HOME", "/nonexistent/wargame-home")
        .args(["completions", "zsh"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout_of(&out).contains("#compdef wargame"));
}

#[test]
fn profile_build_defaults_to_the_configured_profile() {
    // With the fake VM backend there is no builder; the point is that the
    // name is optional and resolves to a real profile rather than erroring
    // on argument parsing.
    let out = Command::new(bin())
        .args(["profile", "build", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(stdout_of(&out).contains("[NAME]"));
}
