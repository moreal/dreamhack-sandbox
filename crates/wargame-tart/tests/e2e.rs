//! Real end-to-end proof against an actual Tart VM (PLAN.md Task 9,
//! DESIGN.md §9 "VM integration"): clone the locally cached vanilla macOS
//! image, boot it, push a fixture zip in over password-authenticated SSH
//! (this test never touches wargame-cli's wiring or a guest key -- it
//! drives `wargame-tart` directly), run the real `guest/bootstrap.sh`
//! guest-side, and verify the extracted file's content -- then always
//! stops and deletes the VM.
//!
//! This is the M0 "VM proof" milestone (MVP.md §36) exercised for real,
//! rather than through `FakeRunner`.
//!
//! Manual only. Requires a real Apple Silicon Mac with:
//!   - `tart` on PATH and the vanilla image already pulled locally
//!     (`tart pull ghcr.io/cirruslabs/macos-sequoia-vanilla:latest`,
//!     override with `WARGAME_E2E_IMAGE`);
//!   - `ssh`, `scp`, `sshpass`, `zip`, `bash` on PATH.
//!
//! `#[ignore]` keeps this out of `cargo test --workspace`, and it *also*
//! skips (rather than fails) unless `WARGAME_E2E=1` is set, so a blanket
//! `cargo test -- --ignored` can never accidentally try to boot a macOS VM:
//!
//!     WARGAME_E2E=1 cargo test -p wargame-tart --test e2e -- --ignored --nocapture

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use wargame_core::domain::{NetworkMode, Resources, VmRef};
use wargame_core::infra::process::{ProcessRunner, RealRunner};
use wargame_core::ports::{ImageRef, StartOptions, VmProvider, VmSpec};

use wargame_tart::{TartConfig, TartProvider};

const DEFAULT_BASE_IMAGE: &str = "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest";
const GUEST_USER: &str = "admin";
const GUEST_PASSWORD: &str = "admin";
const SSH_READY_TIMEOUT: Duration = Duration::from_secs(300);
const SSH_READY_INTERVAL: Duration = Duration::from_secs(5);
const FIXTURE_CONTENT: &str = "hello e2e\n";

fn enabled() -> bool {
    std::env::var("WARGAME_E2E").as_deref() == Ok("1")
}

fn base_image() -> String {
    std::env::var("WARGAME_E2E_IMAGE").unwrap_or_else(|_| DEFAULT_BASE_IMAGE.to_string())
}

fn have(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bootstrap_sh_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../guest/bootstrap.sh")
}

/// Builds `<dir>/fixture.zip` containing one entry, `hello.txt`, via the
/// real `zip` binary (mirrors `tests/bootstrap_sh.rs`'s helper -- this crate
/// has no zip-writing dependency of its own, on purpose).
fn build_fixture_zip(dir: &Path) -> PathBuf {
    let staging = tempfile::tempdir().unwrap();
    std::fs::write(staging.path().join("hello.txt"), FIXTURE_CONTENT).unwrap();
    let zip_path = dir.join("fixture.zip");
    let status = Command::new("zip")
        .current_dir(staging.path())
        .args(["-q", "-X"])
        .arg(&zip_path)
        .arg("hello.txt")
        .status()
        .expect("failed to spawn zip");
    assert!(status.success(), "zip failed building the fixture archive");
    zip_path
}

fn ssh_base_args() -> Vec<&'static str> {
    vec![
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "LogLevel=ERROR",
    ]
}

/// Runs `remote_cmd` on `ip` via `sshpass`/`ssh` with the guest's build-time
/// password (DESIGN.md §5's build flow, not the per-install guest key --
/// this test never generates or touches one).
fn ssh_exec(ip: IpAddr, remote_cmd: &str) -> Result<String, String> {
    let output = Command::new("sshpass")
        .args(["-p", GUEST_PASSWORD])
        .arg("ssh")
        .args(ssh_base_args())
        .arg(format!("{GUEST_USER}@{ip}"))
        .arg(remote_cmd)
        .output()
        .map_err(|e| format!("failed to spawn sshpass ssh: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ssh `{remote_cmd}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn scp_to_guest(local: &Path, ip: IpAddr, remote: &str) -> Result<(), String> {
    let status = Command::new("sshpass")
        .args(["-p", GUEST_PASSWORD])
        .arg("scp")
        .args(ssh_base_args())
        .arg(local)
        .arg(format!("{GUEST_USER}@{ip}:{remote}"))
        .status()
        .map_err(|e| format!("failed to spawn sshpass scp: {e}"))?;
    if !status.success() {
        return Err(format!("scp {} -> {remote} failed", local.display()));
    }
    Ok(())
}

async fn wait_for_ssh(ip: IpAddr) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + SSH_READY_TIMEOUT;
    loop {
        if ssh_exec(ip, "true").is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "guest SSH never became ready within {SSH_READY_TIMEOUT:?}"
            ));
        }
        tokio::time::sleep(SSH_READY_INTERVAL).await;
    }
}

/// The M0 flow itself (MVP.md §36): boot, copy in, guest-side bootstrap,
/// verify. Returns `Err` instead of panicking so the caller can always run
/// VM cleanup afterwards regardless of outcome.
async fn run_m0_flow(tart: &TartProvider, vm: &VmRef) -> Result<(), String> {
    let opts = StartOptions {
        network: NetworkMode::Nat,
        headless: true,
        log_path: std::env::temp_dir().join(format!("{}-run.log", vm.name)),
    };
    tart.start(vm, &opts)
        .await
        .map_err(|e| format!("start failed: {e}"))?;

    let ip = tart
        .wait_ip(vm, Duration::from_secs(180))
        .await
        .map_err(|e| format!("wait_ip failed: {e}"))?;

    wait_for_ssh(ip).await?;

    ssh_exec(ip, "mkdir -p ~/e2e-workspace/incoming")?;

    let staging = tempfile::tempdir().map_err(|e| e.to_string())?;
    let zip_path = build_fixture_zip(staging.path());
    scp_to_guest(&zip_path, ip, "~/e2e-workspace/incoming/fixture.zip")?;
    scp_to_guest(&bootstrap_sh_path(), ip, "~/bootstrap.sh")?;

    ssh_exec(ip, "bash ~/bootstrap.sh ~/e2e-workspace --extract=true")?;

    let content = ssh_exec(
        ip,
        "cat ~/e2e-workspace/incoming/extracted/fixture/hello.txt",
    )?;
    if content != FIXTURE_CONTENT.trim() {
        return Err(format!(
            "extracted file content mismatch: expected {:?}, got {:?}",
            FIXTURE_CONTENT.trim(),
            content
        ));
    }
    Ok(())
}

#[tokio::test]
#[ignore]
async fn m0_disposable_vm_proof() {
    if !enabled() {
        eprintln!(
            "skipping m0_disposable_vm_proof: set WARGAME_E2E=1 to run this against a real Tart VM"
        );
        return;
    }
    for bin in ["tart", "ssh", "scp", "sshpass", "zip", "bash"] {
        assert!(have(bin), "WARGAME_E2E=1 but `{bin}` is not on PATH");
    }

    let runner: Arc<dyn ProcessRunner> = Arc::new(RealRunner);
    let tart = TartProvider::new(runner, TartConfig::default());

    let image = ImageRef(base_image());
    match tart.image_exists(&image).await {
        Ok(true) => {}
        Ok(false) => panic!(
            "base image {} is not present locally; pull it first: tart pull {}",
            image.0, image.0
        ),
        Err(e) => panic!("tart image_exists failed: {e}"),
    }

    let session_id = format!("e2e-{}", std::process::id());
    let vm_name = tart.vm_name(&session_id);

    let spec = VmSpec::new(
        vm_name.clone(),
        image,
        Resources {
            cpu: 2,
            memory_mib: 4096,
            disk_gib: 40,
        },
    );
    let vm = tart.create(&spec).await.expect("tart clone/set (create)");

    let outcome = run_m0_flow(&tart, &vm).await;

    // Best-effort cleanup regardless of outcome (NFR-02: nothing lingers).
    let _ = tart.stop(&vm).await;
    if let Err(e) = tart.delete(&vm).await {
        eprintln!("warning: failed to delete e2e VM {}: {e}", vm.name);
    }

    outcome.expect("M0 flow failed");
}
