//! Exercises the real `guest/bootstrap.sh` end to end through actual
//! `bash`/`zip`/`unzip` binaries (PLAN.md Task 6). This lives in
//! `wargame-tart` because the CLI crate does not exist yet; it has nothing
//! to do with Tart otherwise.
//!
//! Each test skips (rather than fails) when a required tool isn't on PATH,
//! since `bootstrap.sh` itself is guest-side shell, not something every CI
//! image is guaranteed to carry `zip`/`unzip` for.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}

fn bootstrap_sh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../guest/bootstrap.sh")
}

fn run_bootstrap(workspace: &Path, extract: bool) -> Output {
    Command::new("bash")
        .arg(bootstrap_sh())
        .arg(workspace)
        .arg(format!("--extract={extract}"))
        .output()
        .expect("failed to spawn bash")
}

/// Builds `<dir>/name` containing one entry `hello.txt` via the real `zip`
/// binary.
fn zip_simple(dir: &Path, name: &str) -> PathBuf {
    let staging = tempfile::tempdir().unwrap();
    std::fs::write(staging.path().join("hello.txt"), "hi\n").unwrap();
    let zip_path = dir.join(name);
    let status = Command::new("zip")
        .current_dir(staging.path())
        .args(["-q", "-X"])
        .arg(&zip_path)
        .arg("hello.txt")
        .status()
        .expect("failed to spawn zip");
    assert!(status.success(), "zip failed building {name}");
    zip_path
}

/// Builds `<dir>/name` containing `hello.txt` plus a zip-slip entry stored
/// literally as `../evil.txt` (the classic zip-slip PoC shape), by asking
/// `zip` to add a file one directory above its own cwd.
fn zip_with_slip(dir: &Path, name: &str) -> PathBuf {
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("hello.txt"), "hi\n").unwrap();
    std::fs::write(outer.path().join("evil.txt"), "pwned\n").unwrap();

    let zip_path = dir.join(name);
    let status = Command::new("zip")
        .current_dir(&inner)
        .args(["-q", "-X"])
        .arg(&zip_path)
        .args(["hello.txt", "../evil.txt"])
        .status()
        .expect("failed to spawn zip");
    assert!(status.success(), "zip failed building {name}");
    zip_path
}

#[test]
fn creates_the_standard_directories() {
    if !have("bash") {
        eprintln!("skipping: bash not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");

    let out = run_bootstrap(&ws, false);
    assert!(
        out.status.success(),
        "bootstrap.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for dir in ["incoming", "work", "notes", "output"] {
        assert!(ws.join(dir).is_dir(), "missing {dir}/ under {ws:?}");
    }
    assert!(ws.join("notes").join("README.md").is_file());
}

#[test]
fn extract_false_leaves_the_zip_untouched() {
    if !(have("bash") && have("zip")) {
        eprintln!("skipping: bash/zip not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    let zip_path = zip_simple(&ws.join("incoming"), "challenge.zip");

    let out = run_bootstrap(&ws, false);
    assert!(out.status.success());
    assert!(zip_path.is_file(), "the zip itself must still be there");
    assert!(
        !ws.join("incoming").join("extracted").exists(),
        "nothing should have been unpacked with --extract=false"
    );
}

#[test]
fn extract_true_unpacks_a_safe_zip() {
    if !(have("bash") && have("zip") && have("unzip")) {
        eprintln!("skipping: bash/zip/unzip not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    zip_simple(&ws.join("incoming"), "challenge.zip");

    let out = run_bootstrap(&ws, true);
    assert!(
        out.status.success(),
        "bootstrap.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ws
        .join("incoming")
        .join("extracted")
        .join("challenge")
        .join("hello.txt")
        .is_file());
}

/// spec.md §7 AC-11: one unsafe entry refuses the *whole* archive. The
/// original stays in `incoming/`, nothing is unpacked at all, and nothing
/// lands outside the workspace.
#[test]
fn extract_true_refuses_a_zip_slip_archive_whole() {
    if !(have("bash") && have("zip") && have("unzip")) {
        eprintln!("skipping: bash/zip/unzip not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    let archive = zip_with_slip(&ws.join("incoming"), "challenge.zip");

    let out = run_bootstrap(&ws, true);
    assert!(
        out.status.success(),
        "bootstrap.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(archive.is_file(), "the original archive must be kept");
    assert!(
        !ws.join("incoming").join("extracted").exists(),
        "a refused archive must leave no partially unpacked tree"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("challenge.zip"),
        "the refusal must name the archive: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The zip-slip entry must never land outside the workspace tree: not
    // beside the temp dir, and not beside the workspace itself.
    assert!(!tmp.path().join("evil.txt").is_file());
    assert!(!ws.join("evil.txt").is_file());
    assert!(!ws.join("incoming").join("evil.txt").is_file());
    assert!(tmp
        .path()
        .parent()
        .map(|p| !p.join("evil.txt").is_file())
        .unwrap_or(true));
}

#[test]
fn welcome_message_and_metadata_summary_are_printed() {
    if !have("bash") {
        eprintln!("skipping: bash not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    std::fs::write(
        ws.join("incoming").join(".wargame.json"),
        r#"{"provider":"dreamhack","challenge_id":"11","title":"out_of_bound","download_sha256":{"challenge.zip":"deadbeef"}}"#,
    )
    .unwrap();

    let out = run_bootstrap(&ws, false);
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("wargame-vm: workspace ready"));
    assert!(stdout.contains("NOT executed automatically"));

    let readme =
        std::fs::read_to_string(ws.join("notes").join("README.md")).expect("README.md written");
    assert!(readme.contains("dreamhack") || readme.contains("out_of_bound"));
}
