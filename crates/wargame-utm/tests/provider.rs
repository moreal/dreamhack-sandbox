//! `UtmProvider` against a fake `ProcessRunner`, fed with the real output
//! captured in `tests/fixtures/` (see its `README.md`, and
//! `docs/utm-compatibility.md` for what each row means).
//!
//! Nothing here talks to UTM: every test drives `utmctl`/`osascript`/`ps`
//! through `FakeRunner`, so `cargo test` runs on a host with no UTM
//! installed (spec.md §9's "일반 cargo test에서 VM 생성·대용량
//! 다운로드·앱 자동화를 수행하지 않는다").

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use wargame_core::domain::{
    ExecutionMode, GuestArch, GuestOs, NetworkMode, ResolvedBackend, Resources, VmRef,
};
use wargame_core::error::WargameError;
use wargame_core::infra::process::{Cmd, Output, ProcessRunner};
use wargame_core::ports::{
    ConsoleDescriptor, ImageRef, StartOptions, VmProvider, VmSpec, VmStatus,
};
use wargame_core::testing::FakeRunner;

use wargame_utm::{Automation, Ejected, StopMethod, UtmConfig, UtmProvider, BUNDLED_UTMCTL};

// --- fixtures (real output, byte-exact) ------------------------------------

const LIST_MULTI: &str = include_str!("fixtures/utmctl-list-multi.txt");
const LIST_EMPTY: &str = include_str!("fixtures/utmctl-list-empty.txt");
const ERR_NOT_FOUND: &str = include_str!("fixtures/utmctl-error-vm-not-found.stderr.txt");
const ERR_NOT_RUNNING: &str = include_str!("fixtures/utmctl-error-not-running.stderr.txt");
const ERR_GUEST_AGENT: &str = include_str!("fixtures/utmctl-error-guest-agent-missing.stderr.txt");
const ERR_ORPHAN_LOCK: &str = include_str!("fixtures/utmctl-error-orphan-qemu-lock.stderr.txt");
const ERR_AUTOMATION_DENIED: &str =
    include_str!("fixtures/applescript-error-appleevents-denied.txt");
const ERR_MUST_BE_STOPPED: &str = include_str!("fixtures/applescript-error-vm-must-be-stopped.txt");
/// The three lines `utmctl ip-address` really answers with, once a guest
/// agent is running (phase A, D8): IPv4, a global IPv6, a link-local one.
const IP_THREE_LINES: &str = include_str!("fixtures/utmctl-ip-address-three-lines.txt");
/// The x86_64 guest 13 s before its DHCPv4 lease: a routable ULA and a
/// link-local address, no IPv4 (D3).
const IP_IPV6_ONLY: &str = include_str!("fixtures/utmctl-ip-address-ipv6-only.txt");
/// The very next poll of that guest, with the IPv4 lease joined (D3).
const IP_IPV4_JOINED: &str = include_str!("fixtures/utmctl-ip-address-ipv4-joined.txt");
/// T8: `utmctl start` right after a stop, with UTM not back yet -- exit 0,
/// `-609` on stderr, and the VM does not start.
const ERR_609: &str = include_str!("fixtures/utmctl-error-609-app-not-running.stderr.txt");

/// The three id-addressed verb scripts, recognised by the verb applied to
/// the **single-object specifier**. Never `"<verb> vm"`: applying a verb to
/// `repeat with vm in (virtual machines)`'s loop reference is what made one
/// `delete` empty a whole UTM library (task-7-b2b-report.md D1), and
/// `automation.rs`'s `no_script_applies_a_verb_to_the_loop_reference`
/// forbids that shape outright.
const START_SCRIPT: &str = "start (first virtual machine";
const STOP_SCRIPT: &str = "stop (first virtual machine";
const DELETE_SCRIPT: &str = "delete (first virtual machine";
/// The two scripts that both touch `network interfaces of c`, told apart by
/// the argument each one binds: the network-mode write and the MAC write.
const NETWORK_SCRIPT: &str = "set netMode to";
const MAC_SCRIPT: &str = "address:macAddr";

/// The first UUID in `utmctl-list-multi.txt` (`wargame-t1-x86`).
const X86_ID: &str = "A8E9F979-A198-433A-B2DE-74A6D7302D2F";
/// A UUID that is *not* in any fixture listing.
const UNKNOWN_ID: &str = "00000000-0000-0000-0000-000000000000";
/// The VM of trap T12, whose QEMU process outlived a `stop --kill`.
const ORPHAN_ID: &str = "7633A4A1-8256-4D65-8405-07E327886D78";
/// The `ps -Aww -o pid=,command=` line for that orphan (T12).
const ORPHAN_PS_LINE: &str = "58445 /Applications/UTM.app/Contents/XPCServices/QEMUHelper.xpc/Contents/MacOS/QEMULauncher.app/Contents/MacOS/QEMULauncher /Applications/UTM.app/Contents/Frameworks/qemu-aarch64-softmmu.framework/Versions/A/qemu -name wargamet1attach -uuid 7633A4A1-8256-4D65-8405-07E327886D78 -accel hvf";

// --- fake-runner plumbing --------------------------------------------------

fn ok(stdout: &str) -> Output {
    Output {
        status: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

/// `utmctl`'s Apple-Event failure shape: exit 0, the error on stderr (T1).
fn apple_event_failure(stderr: &str) -> Output {
    Output {
        status: 0,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

fn failure(status: i32, stderr: &str) -> Output {
    Output {
        status,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

/// The `utmctl` subcommand of `cmd`, if it is a `utmctl` invocation.
fn utmctl_sub(cmd: &Cmd) -> Option<&str> {
    if !cmd.program.ends_with("utmctl") {
        return None;
    }
    cmd.args.first().map(String::as_str)
}

/// The AppleScript `osascript` was fed on stdin (the script is always a
/// trusted constant; data travels in argv).
fn script_of(cmd: &Cmd) -> String {
    cmd.stdin
        .as_ref()
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .unwrap_or_default()
}

fn is_osascript(cmd: &Cmd, marker: &str) -> bool {
    cmd.program == "osascript" && script_of(cmd).contains(marker)
}

/// A runner that answers the app handshake (T8) and `utmctl version`, and
/// delegates everything else to `rules`; an unmatched command succeeds with
/// empty output, which is what UTM's `update configuration` scripts do.
fn runner(rules: impl Fn(&Cmd) -> Option<Output> + Send + Sync + 'static) -> Arc<FakeRunner> {
    let fake = Arc::new(FakeRunner::new());
    fake.set_which("utmctl", PathBuf::from("/opt/homebrew/bin/utmctl"));
    fake.set_responder(move |cmd| {
        if let Some(out) = rules(cmd) {
            return Ok(out);
        }
        if cmd.program == "open" {
            return Ok(ok(""));
        }
        if utmctl_sub(cmd) == Some("version") {
            return Ok(ok("4.7.5\n"));
        }
        if cmd.program == "osascript" && script_of(cmd).contains("UTM version") {
            return Ok(ok("4.7.5\n"));
        }
        Ok(ok(""))
    });
    fake
}

fn provider(fake: Arc<FakeRunner>) -> UtmProvider {
    UtmProvider::new(fake as Arc<dyn ProcessRunner>, UtmConfig::default())
}

/// A short-timeout config, so the timeout tests do not have to model 30
/// minutes of polling.
fn fast_config() -> UtmConfig {
    UtmConfig {
        boot_timeout: Duration::from_secs(20),
        virtualize_boot_timeout: Duration::from_secs(20),
        stop_timeout: Duration::from_secs(10),
        ip_poll_interval: Duration::from_secs(2),
        status_poll_interval: Duration::from_millis(500),
        ..UtmConfig::default()
    }
}

/// A `utmctl list` table with a single VM this app owns.
fn owned_list(id: &str, status: &str, name: &str) -> String {
    format!("UUID                                 Status   Name\n{id} {status}  {name}\n")
}

fn vm_ref(id: &str, name: &str) -> VmRef {
    VmRef {
        backend: "utm".to_string(),
        id: id.to_string(),
        name: name.to_string(),
    }
}

fn resources() -> Resources {
    Resources {
        cpu: 4,
        memory_mib: 8192,
        disk_gib: 80,
    }
}

fn kali_x86_spec(name: &str) -> VmSpec {
    VmSpec {
        name: name.to_string(),
        image: ImageRef(X86_ID.to_string()),
        resources: resources(),
        execution: ResolvedBackend {
            backend: wargame_core::domain::Backend::Utm,
            mode: ExecutionMode::Emulate,
            os: GuestOs::Linux,
            arch: GuestArch::X86_64,
            distribution: Some("kali".to_string()),
        },
        network: NetworkMode::Isolated,
    }
}

fn start_opts(network: NetworkMode) -> StartOptions {
    StartOptions {
        network,
        headless: true,
        log_path: PathBuf::from("/tmp/wargame-utm-test/run.log"),
    }
}

// --- discovery and health --------------------------------------------------

#[test]
fn utmctl_discovery_prefers_config_then_path_then_bundle() {
    // 1. the configured path wins, spaces and all.
    let fake = Arc::new(FakeRunner::new());
    let configured = "/Users/me/Applications/UTM 4.app/Contents/MacOS/utmctl";
    fake.set_which(configured, PathBuf::from(configured));
    fake.set_which("utmctl", PathBuf::from("/opt/homebrew/bin/utmctl"));
    let p = UtmProvider::new(
        fake as Arc<dyn ProcessRunner>,
        UtmConfig {
            utmctl: Some(configured.to_string()),
            ..UtmConfig::default()
        },
    );
    assert_eq!(p.utmctl_path().unwrap(), configured);

    // 2. no config: PATH.
    let fake = Arc::new(FakeRunner::new());
    fake.set_which("utmctl", PathBuf::from("/opt/homebrew/bin/utmctl"));
    let p = provider(fake);
    assert_eq!(p.utmctl_path().unwrap(), "/opt/homebrew/bin/utmctl");

    // 3. not on PATH: the app bundle.
    let fake = Arc::new(FakeRunner::new());
    fake.set_which(BUNDLED_UTMCTL, PathBuf::from(BUNDLED_UTMCTL));
    let p = UtmProvider::new(fake as Arc<dyn ProcessRunner>, UtmConfig::default());
    assert_eq!(p.utmctl_path().unwrap(), BUNDLED_UTMCTL);

    // 4. nowhere: an error naming all three locations, not a panic.
    let fake = Arc::new(FakeRunner::new());
    let p = UtmProvider::new(fake as Arc<dyn ProcessRunner>, UtmConfig::default());
    let err = p.utmctl_path().unwrap_err().to_string();
    assert!(err.contains("utmctl"), "{err}");
    assert!(err.contains(BUNDLED_UTMCTL), "{err}");
}

#[tokio::test]
async fn healthcheck_reports_the_version_and_the_unverified_isolation_wording() {
    let p = provider(runner(|_| None));
    let status = p.healthcheck().await.unwrap();
    assert!(status.available);
    assert_eq!(status.version.as_deref(), Some("4.7.5"));
    // isolated is usable (the mechanism is verified) but must never be
    // reported as a plain "ok" (docs/utm-compatibility.md §5).
    assert_eq!(status.isolated_network, Some(true));
    assert_ne!(status.detail, "ok");
    assert!(status.detail.contains("unverified"), "{}", status.detail);
}

#[tokio::test]
async fn healthcheck_without_utm_installed_is_unavailable_not_an_error() {
    // No `which` entries at all: UTM is not installed on this host. A Tart
    // profile must still work, so this may not error (FR-6).
    let fake = Arc::new(FakeRunner::new());
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, UtmConfig::default());
    let status = p.healthcheck().await.unwrap();
    assert!(!status.available);
    assert!(status.detail.contains("utmctl"), "{}", status.detail);
    // Nothing was launched: no `open -a UTM` against a missing app.
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[tokio::test]
async fn healthcheck_refuses_a_utm_older_than_4_3() {
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("version")).then(|| ok("4.2.1\n"))
    }));
    let status = p.healthcheck().await.unwrap();
    assert!(!status.available);
    assert!(status.detail.contains("4.3"), "{}", status.detail);
    assert!(status.detail.contains("4.2.1"), "{}", status.detail);
}

#[tokio::test]
async fn healthcheck_reports_a_denied_automation_grant_with_recovery_steps() {
    let p = provider(runner(|cmd| {
        is_osascript(cmd, "UTM version").then(|| failure(1, ERR_AUTOMATION_DENIED))
    }));
    let status = p.healthcheck().await.unwrap();
    assert!(!status.available);
    assert!(status.detail.contains("-1743"), "{}", status.detail);

    let rows = p.diagnostics().await;
    let automation = rows
        .iter()
        .find(|c| c.name == "utm:automation")
        .expect("an automation row");
    assert!(!automation.ok);
    let hint = automation.hint.as_deref().unwrap_or_default();
    assert!(hint.contains("Automation"), "{hint}");
    assert!(hint.contains("System Settings"), "{hint}");
}

#[tokio::test]
async fn diagnostics_name_the_binary_its_version_and_the_automation_grant() {
    let p = provider(runner(|_| None));
    let rows = p.diagnostics().await;
    let names: Vec<&str> = rows.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"utm:utmctl"), "{names:?}");
    assert!(names.contains(&"utm:version"), "{names:?}");
    assert!(names.contains(&"utm:automation"), "{names:?}");
    let binary = rows.iter().find(|c| c.name == "utm:utmctl").unwrap();
    assert!(binary.ok);
    assert!(
        binary.detail.contains("/opt/homebrew/bin/utmctl"),
        "{}",
        binary.detail
    );
    assert!(rows.iter().all(|c| c.ok), "{rows:?}");
}

#[test]
fn capabilities_cover_both_architectures_both_modes_and_both_networks() {
    let caps = provider(runner(|_| None)).capabilities();
    assert!(caps.guest_os.contains(&GuestOs::Linux));
    assert!(caps.guest_os.contains(&GuestOs::Windows));
    assert!(!caps.guest_os.contains(&GuestOs::Macos));
    assert!(caps.guest_arch.contains(&GuestArch::Arm64));
    assert!(caps.guest_arch.contains(&GuestArch::X86_64));
    assert!(caps.supports_mode(ExecutionMode::Emulate));
    assert!(caps.supports_mode(ExecutionMode::Virtualize));
    assert!(caps.supports_network(NetworkMode::Isolated));
    assert!(caps.supports_network(NetworkMode::Nat));
    assert!(caps.graphical_console);
}

// --- list and status -------------------------------------------------------

#[tokio::test]
async fn list_parses_the_utmctl_table_uuid_first_and_keeps_only_owned_vms() {
    let p = UtmProvider::new(
        runner(|cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_MULTI)))
            as Arc<dyn ProcessRunner>,
        UtmConfig {
            vm_prefix: "wargame-t1".to_string(),
            ..UtmConfig::default()
        },
    );
    let vms = p.list().await.unwrap();
    assert_eq!(vms.len(), 4, "{vms:?}");
    assert_eq!(vms[0].backend, "utm");
    assert_eq!(vms[0].id, X86_ID);
    assert_eq!(vms[0].name, "wargame-t1-x86");
    // A display name with spaces and Korean survives intact.
    assert_eq!(vms[3].name, "wargame-t1 한글 이름 vm");
    assert_eq!(vms[3].id, "AE4AC721-7F87-431F-A7D0-795C91939BD6");

    // With this app's real prefix, none of those VMs are ours -- so `prune`
    // can never see a VM outside the namespace (spec.md §3 FR-5).
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_MULTI))
    }));
    assert!(p.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn list_of_an_empty_registry_is_empty() {
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_EMPTY))
    }));
    assert!(p.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn list_rejects_output_it_cannot_parse() {
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("list")).then(|| ok("total nonsense\nnot a table\n"))
    }));
    let err = p.list().await.unwrap_err();
    assert!(
        matches!(err, WargameError::ProviderChanged { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn status_maps_every_state_of_the_sdef_enumeration() {
    for (utm_state, expected) in [
        ("stopped", VmStatus::Stopped),
        ("starting", VmStatus::Running),
        ("started", VmStatus::Running),
        ("pausing", VmStatus::Running),
        ("paused", VmStatus::Running),
        ("resuming", VmStatus::Running),
        ("stopping", VmStatus::Stopped),
    ] {
        let reply = format!("{utm_state}\tqemu\n");
        let p = provider(runner(move |cmd| {
            is_osascript(cmd, "status of vm").then(|| ok(&reply))
        }));
        assert_eq!(
            p.status(&vm_ref(X86_ID, "wg-x")).await.unwrap(),
            expected,
            "{utm_state}"
        );
    }
}

#[tokio::test]
async fn status_of_a_vm_deleted_outside_wargame_is_missing_not_an_error() {
    let p = provider(runner(|cmd| {
        is_osascript(cmd, "status of vm").then(|| ok("wargame-utm:vm-not-found\n"))
    }));
    assert_eq!(
        p.status(&vm_ref(UNKNOWN_ID, "wg-gone")).await.unwrap(),
        VmStatus::Missing
    );

    // `utmctl`'s own not-found shape (exit 1) must not become an error either.
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("status")).then(|| failure(1, ERR_NOT_FOUND))
    }));
    assert_eq!(
        p.status(&vm_ref(UNKNOWN_ID, "wg-gone")).await.unwrap(),
        VmStatus::Missing
    );
}

/// `backend: unavailable` is the third value of the `backend` enum: the VM
/// is registered but its backend cannot be used right now. Never an error,
/// never a delete (docs/utm-compatibility.md §2, open item 9).
#[tokio::test]
async fn status_of_a_vm_whose_backend_is_unavailable_is_missing() {
    let p = provider(runner(|cmd| {
        is_osascript(cmd, "status of vm").then(|| ok("stopped\tunavailable\n"))
    }));
    assert_eq!(
        p.status(&vm_ref(X86_ID, "wg-x")).await.unwrap(),
        VmStatus::Missing
    );
}

// --- create ----------------------------------------------------------------

#[tokio::test]
async fn create_clones_the_source_vm_and_returns_the_new_uuid() {
    let new_id = "11111111-2222-3333-4444-555555555555";
    let before = LIST_MULTI.to_string();
    let after = format!("{before}{new_id} stopped  wg-session-1\n");
    let calls_seen = Arc::new(std::sync::Mutex::new(0usize));
    let seen = calls_seen.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            // First listing: before the duplicate. Later: after it.
            return Some(ok(if *n == 1 { &before } else { &after }));
        }
        None
    });
    let p = provider(fake.clone());

    let vm = p.create(&kali_x86_spec("wg-session-1")).await.unwrap();
    assert_eq!(vm.backend, "utm");
    assert_eq!(vm.id, new_id, "the UUID is the provider id, not the name");
    assert_eq!(vm.name, "wg-session-1");

    let calls = fake.calls();
    // The clone is addressed by the source UUID and names the copy.
    let duplicate = calls
        .iter()
        .find(|c| is_osascript(c, "duplicate"))
        .expect("a duplicate call");
    assert_eq!(duplicate.args[0], "-");
    assert_eq!(duplicate.args[1], X86_ID);
    assert_eq!(duplicate.args[2], "wg-session-1");
    // Resources, network and a display are applied to the *new* UUID.
    for marker in ["cpu cores", NETWORK_SCRIPT, "displays of c", MAC_SCRIPT] {
        let call = calls
            .iter()
            .find(|c| is_osascript(c, marker))
            .unwrap_or_else(|| panic!("expected an osascript call containing {marker:?}"));
        assert_eq!(call.args[1], new_id, "{marker}");
    }
    // Isolated -> UTM `host` (§5), never `emulated` or `bridged`.
    let network = calls
        .iter()
        .find(|c| is_osascript(c, NETWORK_SCRIPT))
        .unwrap();
    assert_eq!(network.args[2], "host");
}

/// **D4.** `duplicate` copies the source's network interface verbatim, MAC
/// address included, so both sessions launched from one image came up on
/// the same host-only segment with the same address -- and an `scp`
/// addressed to it landed in the other session's guest
/// (task-7-b2b-report.md D4). Every copy gets its own address, written
/// while it is still stopped.
#[tokio::test]
async fn create_gives_the_clone_a_fresh_locally_administered_mac() {
    let new_id = "11111111-2222-3333-4444-555555555555";
    let before = LIST_MULTI.to_string();
    let after = format!("{before}{new_id} stopped  wg-session-1\n");
    let n = Arc::new(std::sync::Mutex::new(0usize));
    let seen = n.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            return Some(ok(if *n == 1 { &before } else { &after }));
        }
        None
    });
    let p = provider(fake.clone());
    p.create(&kali_x86_spec("wg-session-1")).await.unwrap();

    let calls = fake.calls();
    let mac_call = calls
        .iter()
        .find(|c| is_osascript(c, MAC_SCRIPT))
        .expect("the copy is given its own MAC address");
    assert_eq!(
        mac_call.args[1], new_id,
        "written to the copy, not the source"
    );
    let mac = &mac_call.args[2];
    // The address travels as argv data, never inside the script text.
    assert!(
        !script_of(mac_call).contains(mac.as_str()),
        "the MAC must be an argument, not interpolated into the script"
    );
    let first = u8::from_str_radix(&mac[..2], 16).expect("hex octets");
    assert_eq!(
        first & 0b11,
        0b10,
        "locally administered and unicast: {mac}"
    );
    assert_eq!(mac.len(), 17, "UTM's XX:XX:XX:XX:XX:XX form: {mac}");

    // ...and it is written before the VM could ever be started.
    let mac_index = calls.iter().position(|c| is_osascript(c, MAC_SCRIPT));
    let start_index = calls.iter().position(|c| is_osascript(c, START_SCRIPT));
    assert!(mac_index.is_some());
    assert!(
        start_index.is_none() || mac_index < start_index,
        "the MAC is written while the copy is still stopped"
    );
}

/// The clone inherits the source's configuration, so the session's own
/// execution mode is written explicitly: `emulate` must land as
/// `hypervisor:false` even if the source VM was built virtualized.
#[tokio::test]
async fn create_writes_the_hypervisor_flag_of_the_sessions_mode() {
    let new_id = "11111111-2222-3333-4444-555555555555";
    let before = LIST_MULTI.to_string();
    let after = format!("{before}{new_id} stopped  wg-session-1\n");
    let n = Arc::new(std::sync::Mutex::new(0usize));
    let seen = n.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            return Some(ok(if *n == 1 { &before } else { &after }));
        }
        None
    });
    let p = provider(fake.clone());
    p.create(&kali_x86_spec("wg-session-1")).await.unwrap();

    let calls = fake.calls();
    let hardware = calls
        .iter()
        .find(|c| is_osascript(c, "cpu cores"))
        .expect("a hardware call");
    // id, memory (MiB), cores, hypervisor
    assert_eq!(hardware.args[1], new_id);
    assert_eq!(hardware.args[2], "8192");
    assert_eq!(hardware.args[3], "4");
    assert_eq!(hardware.args[4], "false", "emulate means hypervisor:false");
}

/// Copying a running VM captures a torn disk, and the copy inherits its
/// state; refuse before `duplicate`.
#[tokio::test]
async fn create_refuses_to_clone_a_running_source_vm() {
    let listing = owned_list(X86_ID, "started", "wargame-t1-x86");
    let fake = runner(move |cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(&listing)));
    let p = provider(fake.clone());
    let err = p.create(&kali_x86_spec("wg-session-1")).await.unwrap_err();
    assert!(err.to_string().contains("started"), "{err}");
    assert!(!fake.calls().iter().any(|c| is_osascript(c, "duplicate")));
}

#[tokio::test]
async fn create_maps_nat_to_utms_shared_mode() {
    let new_id = "11111111-2222-3333-4444-555555555555";
    let before = LIST_MULTI.to_string();
    let after = format!("{before}{new_id} stopped  wg-session-1\n");
    let n = Arc::new(std::sync::Mutex::new(0usize));
    let seen = n.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            return Some(ok(if *n == 1 { &before } else { &after }));
        }
        None
    });
    let p = provider(fake.clone());
    let mut spec = kali_x86_spec("wg-session-1");
    spec.network = NetworkMode::Nat;
    p.create(&spec).await.unwrap();
    let calls = fake.calls();
    let network = calls
        .iter()
        .find(|c| is_osascript(c, NETWORK_SCRIPT))
        .unwrap();
    assert_eq!(network.args[2], "shared");
}

/// T6: UTM accepts `hypervisor:true` for an x86_64 guest on Apple Silicon
/// and silently ignores it. The resolver already refuses this combination;
/// the provider refuses it again, before any command runs.
#[tokio::test]
async fn create_refuses_virtualize_for_an_x86_64_guest_before_running_anything() {
    let fake = runner(|_| None);
    let p = provider(fake.clone());
    let mut spec = kali_x86_spec("wg-session-1");
    spec.execution.mode = ExecutionMode::Virtualize;

    let err = p.create(&spec).await.unwrap_err();
    assert!(matches!(err, WargameError::Incompatible(_)), "{err:?}");
    let message = err.to_string();
    assert!(message.contains("x86_64"), "{message}");
    assert!(message.contains("emulate"), "{message}");
    assert!(
        fake.calls().is_empty(),
        "nothing may run before the check: {:?}",
        fake.calls()
    );
}

#[tokio::test]
async fn create_without_a_source_vm_fails_before_cloning_anything() {
    let fake = runner(|cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_EMPTY)));
    let p = provider(fake.clone());
    let err = p.create(&kali_x86_spec("wg-session-1")).await.unwrap_err();
    let message = err.to_string();
    assert!(message.contains(X86_ID), "{message}");
    assert!(
        !fake.calls().iter().any(|c| is_osascript(c, "duplicate")),
        "must not clone anything"
    );
}

/// spec.md §5 item 6: AppleScript templates are trusted constants and every
/// input travels as data. Nothing user-controlled may appear in the script
/// text, and no host shell string is ever built.
#[tokio::test]
async fn user_values_travel_as_argv_and_never_enter_the_script_text() {
    let hostile = "wg-\"quoted\" \\ back \u{c608}제\n newline";
    let new_id = "11111111-2222-3333-4444-555555555555";
    let before = LIST_MULTI.to_string();
    let after = format!("{before}{new_id} stopped  {hostile}\n");
    let n = Arc::new(std::sync::Mutex::new(0usize));
    let seen = n.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            return Some(ok(if *n == 1 { &before } else { &after }));
        }
        None
    });
    let p = provider(fake.clone());
    let _ = p.create(&kali_x86_spec(hostile)).await;

    let calls = fake.calls();
    let duplicate = calls
        .iter()
        .find(|c| is_osascript(c, "duplicate"))
        .expect("a duplicate call");
    // The name reaches osascript verbatim, as one argv element...
    assert!(duplicate.args.contains(&hostile.to_string()));
    // ...and never as part of the script.
    assert!(!script_of(duplicate).contains("quoted"));
    assert!(!script_of(duplicate).contains("newline"));
    // No shell anywhere.
    for call in &calls {
        assert!(
            !matches!(call.program.as_str(), "sh" | "bash" | "zsh" | "/bin/sh"),
            "a shell was used: {call:?}"
        );
        assert!(!call.args.iter().any(|a| a == "-c"), "{call:?}");
    }
}

// --- start / wait_ip -------------------------------------------------------

#[tokio::test]
async fn start_applies_the_network_mode_then_starts_without_hide_or_attach() {
    let fake = runner(|cmd| is_osascript(cmd, "status of vm").then(|| ok("stopped\tqemu\n")));
    let p = provider(fake.clone());

    let info = p
        .start(&vm_ref(X86_ID, "wg-x"), &start_opts(NetworkMode::Isolated))
        .await
        .unwrap();
    // UTM owns the QEMU process; there is no pid of ours to record.
    assert_eq!(info.pid, None);

    let calls = fake.calls();
    let network = calls
        .iter()
        .find(|c| is_osascript(c, NETWORK_SCRIPT))
        .expect("the network mode is enforced before every start");
    assert_eq!(network.args[2], "host");
    assert!(calls.iter().any(|c| is_osascript(c, START_SCRIPT)));
    // T7: `--hide` is broken; §3: `--attach` streams nothing.
    for call in &calls {
        assert!(!call.args.iter().any(|a| a == "--hide"), "{call:?}");
        assert!(!call.args.iter().any(|a| a == "--attach"), "{call:?}");
    }
}

#[tokio::test]
async fn wait_ip_returns_the_first_address_the_guest_agent_reports() {
    let attempts = Arc::new(std::sync::Mutex::new(0usize));
    let seen = attempts.clone();
    let p = UtmProvider::new(
        runner(move |cmd| {
            if utmctl_sub(cmd) != Some("ip-address") {
                return None;
            }
            let mut n = seen.lock().unwrap();
            *n += 1;
            // The agent is not up for the first two polls (T1: exit 0).
            Some(if *n < 3 {
                apple_event_failure(ERR_GUEST_AGENT)
            } else {
                ok("192.168.128.42\nfe80::1\n")
            })
        }) as Arc<dyn ProcessRunner>,
        fast_config(),
    );

    let ip = p
        .wait_ip(&vm_ref(X86_ID, "wg-x"), Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(ip, IpAddr::from_str("192.168.128.42").unwrap());
    assert_eq!(*attempts.lock().unwrap(), 3);
}

#[tokio::test(start_paused = true)]
async fn wait_ip_timeout_names_the_guest_agent_the_log_and_the_timeout_setting() {
    let p = UtmProvider::new(
        runner(|cmd| {
            if is_osascript(cmd, "status of vm") {
                return Some(ok("stopped\tqemu\n"));
            }
            (utmctl_sub(cmd) == Some("ip-address")).then(|| apple_event_failure(ERR_GUEST_AGENT))
        }) as Arc<dyn ProcessRunner>,
        fast_config(),
    );
    let vm = vm_ref(X86_ID, "wg-x");
    p.start(&vm, &start_opts(NetworkMode::Isolated))
        .await
        .unwrap();

    let err = p.wait_ip(&vm, Duration::from_secs(20)).await.unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("guest agent"),
        "the guest-agent requirement must be named: {message}"
    );
    assert!(message.contains("20"), "the elapsed/limit time: {message}");
    let hint = err.hint().unwrap_or_default();
    assert!(hint.contains("run.log"), "the log path: {hint}");
    assert!(
        hint.contains("boot_timeout_secs"),
        "how to raise it: {hint}"
    );
}

#[tokio::test]
async fn wait_ip_picks_the_ipv4_out_of_the_agents_three_addresses() {
    // The real answer on an installed Kali guest is three lines. Reading
    // line 1 blindly happens to work there and breaks the moment the agent
    // orders them differently, so the choice is by kind, not by position.
    let p = UtmProvider::new(
        runner(|cmd| (utmctl_sub(cmd) == Some("ip-address")).then(|| ok(IP_THREE_LINES)))
            as Arc<dyn ProcessRunner>,
        fast_config(),
    );
    let ip = p
        .wait_ip(&vm_ref(X86_ID, "wg-x"), Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(ip, IpAddr::from_str("192.168.64.27").unwrap());
}

#[tokio::test]
async fn wait_ip_never_takes_the_first_line_and_never_a_link_local_one() {
    // Same three addresses, reordered: a `lines[0]` implementation would
    // hand ssh an `fe80::` address that cannot be connected to.
    const REORDERED: &str =
        "fe80::3176:9ad0:835c:65b9\n192.168.64.27\nfd14:a432:91d3:c29f:e585:fc6f:d6b4:3529\n";
    let p = UtmProvider::new(
        runner(|cmd| (utmctl_sub(cmd) == Some("ip-address")).then(|| ok(REORDERED)))
            as Arc<dyn ProcessRunner>,
        fast_config(),
    );
    let ip = p
        .wait_ip(&vm_ref(X86_ID, "wg-x"), Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(ip, IpAddr::from_str("192.168.64.27").unwrap());
}

/// **D3.** On these guests the SLAAC ULA appears ~13 s before the DHCPv4
/// lease. The gate used to take the first routable address it saw, so the
/// build went on with an IPv6 literal -- `ssh` accepts one, and the first
/// `scp` then died on `user@fd14:...:299:/path`, after the whole boot
/// (task-7-b2b-report.md D2/D3). The two polls here are the two captured
/// live, in order.
#[tokio::test]
async fn wait_ip_keeps_polling_until_the_ipv4_lease_joins_the_ipv6_one() {
    let polls = Arc::new(std::sync::Mutex::new(0usize));
    let seen = polls.clone();
    let p = UtmProvider::new(
        runner(move |cmd| {
            if utmctl_sub(cmd) != Some("ip-address") {
                return None;
            }
            let mut n = seen.lock().unwrap();
            *n += 1;
            Some(ok(if *n == 1 {
                IP_IPV6_ONLY
            } else {
                IP_IPV4_JOINED
            }))
        }) as Arc<dyn ProcessRunner>,
        fast_config(),
    );
    let ip = p
        .wait_ip(&vm_ref(X86_ID, "wg-x"), Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(ip, IpAddr::from_str("192.168.64.29").unwrap());
    assert_eq!(
        *polls.lock().unwrap(),
        2,
        "the IPv6-only poll must not have been accepted"
    );
}

/// And when the whole boot timeout goes by with nothing but IPv6, the
/// failure says so rather than handing an unusable literal onwards.
#[tokio::test]
async fn wait_ip_fails_after_the_timeout_when_the_guest_only_ever_reports_ipv6() {
    let p = UtmProvider::new(
        runner(|cmd| (utmctl_sub(cmd) == Some("ip-address")).then(|| ok(IP_IPV6_ONLY)))
            as Arc<dyn ProcessRunner>,
        fast_config(),
    );
    let err = p
        .wait_ip(&vm_ref(X86_ID, "wg-x"), Duration::from_secs(1))
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("IPv6"),
        "the message must say IPv6-only was seen: {message}"
    );
    assert!(
        message.contains("IPv4"),
        "and that IPv4 is what is required: {message}"
    );
    assert!(
        message.contains("fd14:a432:91d3:c29f:53c1:5b8f:5d2b:299"),
        "and name the address it did see: {message}"
    );
}

#[tokio::test]
async fn wait_ip_treats_only_link_local_answers_as_not_ready_yet() {
    let p = UtmProvider::new(
        runner(|cmd| {
            (utmctl_sub(cmd) == Some("ip-address")).then(|| ok("fe80::3176:9ad0:835c:65b9\n"))
        }) as Arc<dyn ProcessRunner>,
        fast_config(),
    );
    let err = p
        .wait_ip(&vm_ref(X86_ID, "wg-x"), Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("fe80"), "{err}");
}

// --- T8: the app was not up (-609) -----------------------------------------

/// Every verb that can hit a cold or wedged app re-handshakes and retries
/// once: `utmctl start` right after a stop answers `-609` **with exit 0**
/// and does not start the VM (phase A, D8).
#[tokio::test]
async fn a_609_from_a_cold_app_is_re_handshaked_and_retried_once() {
    let attempts = Arc::new(std::sync::Mutex::new(0usize));
    let seen = attempts.clone();
    let listing = owned_list(X86_ID, "stopped", "wg-x");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&listing));
        }
        if is_osascript(cmd, "status of vm") {
            return Some(ok("stopped\tqemu\n"));
        }
        if !is_osascript(cmd, START_SCRIPT) {
            return None;
        }
        let mut n = seen.lock().unwrap();
        *n += 1;
        Some(if *n == 1 {
            apple_event_failure(ERR_609)
        } else {
            ok("ok")
        })
    });
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, fast_config());

    p.start(&vm_ref(X86_ID, "wg-x"), &start_opts(NetworkMode::Nat))
        .await
        .expect("a -609 must be retried after re-handshaking, not reported as a failure");

    assert_eq!(*attempts.lock().unwrap(), 2, "exactly one retry");
    let calls = fake.calls();
    let opens = calls
        .iter()
        .filter(|c| c.program == "open" && c.args == ["-a", "UTM"])
        .count();
    assert!(
        opens >= 2,
        "the app has to be re-opened before the retry, saw {opens} `open -a UTM` calls"
    );
}

/// The same retry, on the `utmctl` side of the automation surface.
#[tokio::test]
async fn a_609_on_a_utmctl_call_is_re_handshaked_and_retried_once() {
    let attempts = Arc::new(std::sync::Mutex::new(0usize));
    let seen = attempts.clone();
    let listing = owned_list(X86_ID, "stopped", "wg-x");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) != Some("list") {
            return None;
        }
        let mut n = seen.lock().unwrap();
        *n += 1;
        Some(if *n == 1 {
            apple_event_failure(ERR_609)
        } else {
            ok(&listing)
        })
    });
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, fast_config());

    let vms = p.list().await.expect("a -609 must be retried");
    assert_eq!(vms.len(), 1);
    assert_eq!(*attempts.lock().unwrap(), 2);
}

// --- stop ------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn stop_polls_status_until_stopped_rather_than_trusting_the_verb() {
    let polls = Arc::new(std::sync::Mutex::new(0usize));
    let seen = polls.clone();
    let listing = owned_list(X86_ID, "started", "wg-x");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&listing));
        }
        if !is_osascript(cmd, "status of vm") {
            return None;
        }
        let mut n = seen.lock().unwrap();
        *n += 1;
        // started -> stopping -> stopping -> stopped (T9).
        Some(ok(match *n {
            1 => "started\tqemu\n",
            2 | 3 => "stopping\tqemu\n",
            _ => "stopped\tqemu\n",
        }))
    });
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, fast_config());

    p.stop(&vm_ref(X86_ID, "wg-x")).await.unwrap();

    let calls = fake.calls();
    let stop = calls
        .iter()
        .find(|c| is_osascript(c, STOP_SCRIPT))
        .expect("a stop call");
    assert_eq!(stop.args[1], X86_ID);
    assert_eq!(stop.args[2], "force", "the first attempt is graceful");
    assert!(
        !calls
            .iter()
            .any(|c| is_osascript(c, STOP_SCRIPT) && c.args[2] == "kill"),
        "a VM that stops on its own must not be killed"
    );
    assert!(*polls.lock().unwrap() >= 4);
}

#[tokio::test(start_paused = true)]
async fn stop_escalates_to_kill_only_after_the_graceful_timeout() {
    let polls = Arc::new(std::sync::Mutex::new(0usize));
    let seen = polls.clone();
    let killed = Arc::new(std::sync::Mutex::new(false));
    let was_killed = killed.clone();
    let listing = owned_list(X86_ID, "started", "wg-x");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&listing));
        }
        if is_osascript(cmd, STOP_SCRIPT) && cmd.args[2] == "kill" {
            *was_killed.lock().unwrap() = true;
            return Some(ok("ok"));
        }
        if !is_osascript(cmd, "status of vm") {
            return None;
        }
        let mut n = seen.lock().unwrap();
        *n += 1;
        // Wedged in `stopping` until the kill lands.
        Some(ok(if *n == 1 {
            "started\tqemu\n"
        } else if *was_killed.lock().unwrap() {
            "stopped\tqemu\n"
        } else {
            "stopping\tqemu\n"
        }))
    });
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, fast_config());

    p.stop(&vm_ref(X86_ID, "wg-x")).await.unwrap();
    assert!(*killed.lock().unwrap(), "the kill escalation must happen");
}

/// T12: `status` says `stopped` while the QEMU process is still alive and
/// holding `efi_vars.fd`. Report the stop as incomplete, name the pid --
/// and never signal a process wargame did not spawn.
#[tokio::test(start_paused = true)]
async fn stop_fails_when_an_orphan_qemu_process_still_holds_the_vm() {
    let listing = owned_list(ORPHAN_ID, "stopped", "wg-orphan");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&listing));
        }
        if is_osascript(cmd, "status of vm") {
            return Some(ok("stopped\tqemu\n"));
        }
        if cmd.program == "ps" {
            return Some(ok(&format!(
                "  1234 /usr/sbin/mDNSResponder\n{ORPHAN_PS_LINE}\n"
            )));
        }
        None
    });
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, fast_config());

    let err = p.stop(&vm_ref(ORPHAN_ID, "wg-orphan")).await.unwrap_err();
    assert!(
        matches!(err, WargameError::OrphanProcess { pid: 58445, .. }),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(message.contains("58445"), "{message}");
    assert!(message.contains(ORPHAN_ID), "{message}");
    let hint = err.hint().unwrap_or_default();
    assert!(hint.contains("kill -9 58445"), "{hint}");

    // Not one signal was sent.
    for call in fake.calls() {
        assert_ne!(call.program, "kill", "{call:?}");
        assert_ne!(call.program, "pkill", "{call:?}");
    }
}

#[tokio::test]
async fn stop_of_a_missing_vm_is_a_no_op() {
    let fake = runner(|cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(LIST_EMPTY));
        }
        is_osascript(cmd, "status of vm").then(|| ok("wargame-utm:vm-not-found\n"))
    });
    let p = provider(fake.clone());
    p.stop(&vm_ref(UNKNOWN_ID, "wg-gone")).await.unwrap();
    assert!(!fake.calls().iter().any(|c| is_osascript(c, STOP_SCRIPT)));
}

/// A paused VM refuses `stop` ("The virtual machine is not running", exit 0)
/// and has to be resumed first (docs/utm-compatibility.md §2).
#[tokio::test(start_paused = true)]
async fn stop_resumes_a_paused_vm_before_stopping_it() {
    let polls = Arc::new(std::sync::Mutex::new(0usize));
    let seen = polls.clone();
    let listing = owned_list(X86_ID, "paused", "wg-x");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&listing));
        }
        if !is_osascript(cmd, "status of vm") {
            return None;
        }
        let mut n = seen.lock().unwrap();
        *n += 1;
        Some(ok(match *n {
            1 => "paused\tqemu\n",
            2 => "started\tqemu\n",
            _ => "stopped\tqemu\n",
        }))
    });
    let p = UtmProvider::new(fake.clone() as Arc<dyn ProcessRunner>, fast_config());

    p.stop(&vm_ref(X86_ID, "wg-x")).await.unwrap();
    let calls = fake.calls();
    let start_index = calls.iter().position(|c| is_osascript(c, START_SCRIPT));
    let stop_index = calls.iter().position(|c| is_osascript(c, STOP_SCRIPT));
    assert!(start_index.is_some(), "a paused VM is resumed first");
    assert!(start_index < stop_index, "{calls:?}");
}

// --- delete and ownership --------------------------------------------------

/// FR-5: "앱 기록의 UUID와 소유권을 대조한다. 이름 prefix만으로 외부 VM을
/// 삭제하지 않는다." A UUID that is registered in UTM but outside this
/// app's namespace is refused even when it is handed to us directly.
#[tokio::test]
async fn delete_refuses_a_vm_outside_the_apps_namespace() {
    let fake = runner(|cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_MULTI)));
    let p = provider(fake.clone());

    let err = p
        .delete(&vm_ref(X86_ID, "wargame-t1-x86"))
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("wargame-t1-x86"), "{message}");
    assert!(message.contains("wg-"), "the expected prefix: {message}");
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| is_osascript(c, DELETE_SCRIPT) || utmctl_sub(c) == Some("delete")),
        "no delete may be issued: {:?}",
        fake.calls()
    );
}

/// The same guard applies to a stop: FR-5 forbids modifying the user's own
/// VMs, not only deleting them.
#[tokio::test]
async fn stop_refuses_a_vm_outside_the_apps_namespace() {
    let fake = runner(|cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_MULTI)));
    let p = provider(fake.clone());
    let err = p.stop(&vm_ref(X86_ID, "wargame-t1-x86")).await.unwrap_err();
    assert!(err.to_string().contains("wargame-t1-x86"), "{err}");
    assert!(!fake.calls().iter().any(|c| is_osascript(c, STOP_SCRIPT)));
}

#[tokio::test]
async fn delete_of_a_vm_already_gone_is_a_no_op() {
    let fake = runner(|cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_EMPTY)));
    let p = provider(fake.clone());
    p.delete(&vm_ref(UNKNOWN_ID, "wg-gone")).await.unwrap();
    assert!(!fake.calls().iter().any(|c| is_osascript(c, DELETE_SCRIPT)));
}

#[tokio::test]
async fn delete_stops_an_owned_running_vm_then_deletes_it_by_uuid() {
    let owned = format!(
        "UUID                                 Status   Name\n{X86_ID} started  wg-session-1\n"
    );
    let polls = Arc::new(std::sync::Mutex::new(0usize));
    let seen = polls.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&owned));
        }
        if is_osascript(cmd, "status of vm") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            return Some(ok(if *n == 1 {
                "started\tqemu\n"
            } else {
                "stopped\tqemu\n"
            }));
        }
        None
    });
    let p = provider(fake.clone());

    p.delete(&vm_ref(X86_ID, "wg-session-1")).await.unwrap();
    let calls = fake.calls();
    assert!(calls.iter().any(|c| is_osascript(c, STOP_SCRIPT)));
    let delete = calls
        .iter()
        .find(|c| is_osascript(c, DELETE_SCRIPT))
        .expect("a delete call");
    assert_eq!(delete.args[1], X86_ID, "delete is addressed by UUID");
}

// --- error handling --------------------------------------------------------

/// T1: `utmctl` exits 0 even when the Apple Event failed. Any
/// `Error from event:` / `Error:` line on stderr is a failure.
#[tokio::test]
async fn a_utmctl_apple_event_error_is_a_failure_despite_exit_zero() {
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("list")).then(|| apple_event_failure(ERR_NOT_RUNNING))
    }));
    let err = p.list().await.unwrap_err();
    let message = err.to_string();
    assert!(message.contains("-2700"), "{message}");
}

/// The `efi_vars.fd` lock of T12 as it arrives from a start: exit 0, the
/// real failure on stderr. It must not look like a successful start.
#[tokio::test]
async fn a_start_that_failed_on_a_locked_image_is_reported_as_a_failure() {
    let p = provider(runner(|cmd| {
        if is_osascript(cmd, "status of vm") {
            return Some(ok("stopped\tqemu\n"));
        }
        is_osascript(cmd, START_SCRIPT).then(|| failure(1, ERR_ORPHAN_LOCK))
    }));
    let err = p
        .start(
            &vm_ref(ORPHAN_ID, "wg-orphan"),
            &start_opts(NetworkMode::Isolated),
        )
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("efi_vars.fd"), "{message}");
}

/// A configuration write against a running VM fails with -2700. The message
/// has to survive: it is the difference between "retry" and "bug".
#[tokio::test]
async fn a_configuration_write_on_a_running_vm_surfaces_the_2700_error() {
    let p = provider(runner(|cmd| {
        if is_osascript(cmd, "status of vm") {
            return Some(ok("stopped\tqemu\n"));
        }
        is_osascript(cmd, NETWORK_SCRIPT).then(|| failure(1, ERR_MUST_BE_STOPPED))
    }));
    let err = p
        .start(&vm_ref(X86_ID, "wg-x"), &start_opts(NetworkMode::Isolated))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("-2700"), "{err}");
}

// --- console and the build-time operations Task 4 needs --------------------

#[test]
fn console_is_a_window_opened_by_utmctl_start_on_the_uuid() {
    let p = provider(runner(|_| None));
    let ConsoleDescriptor::Window { argv } = p.console(&vm_ref(X86_ID, "wg-x")) else {
        panic!("expected a Window descriptor");
    };
    assert_eq!(argv[0], "/opt/homebrew/bin/utmctl");
    assert_eq!(argv[1], "start");
    assert_eq!(argv[2], X86_ID);
}

#[tokio::test]
async fn create_installer_vm_makes_a_blank_disk_an_iso_and_a_display() {
    let new_id = "99999999-8888-7777-6666-555555555555";
    let fake = runner(move |cmd| {
        is_osascript(cmd, "make new virtual machine")
            .then(|| ok(&format!("{new_id}\twg-install-kali\n")))
    });
    let p = provider(fake.clone());

    let iso = Path::new("/Users/moreal/Library/Application Support/wargame-vm/iso/kali-linux-2026.2-installer-amd64.iso");
    let vm = p
        .create_installer_vm(
            "wg-install-kali",
            GuestOs::Linux,
            GuestArch::X86_64,
            ExecutionMode::Emulate,
            &resources(),
            iso,
        )
        .await
        .unwrap();
    assert_eq!(vm.id, new_id);
    assert_eq!(vm.name, "wg-install-kali");

    let calls = fake.calls();
    let make = calls
        .iter()
        .find(|c| is_osascript(c, "make new virtual machine"))
        .expect("a make call");
    // name, architecture, memory (MiB), cores, hypervisor, uefi, disk (MiB), iso
    assert_eq!(make.args[1], "wg-install-kali");
    assert_eq!(make.args[2], "x86_64");
    assert_eq!(make.args[3], "8192");
    assert_eq!(make.args[4], "4");
    assert_eq!(make.args[5], "false", "emulate means hypervisor:false (T6)");
    assert_eq!(make.args[6], "true", "UEFI is on");
    assert_eq!(make.args[7], (80u64 * 1024).to_string(), "disk in MiB");
    // The ISO path, spaces and all, is one argv element (§1).
    assert_eq!(make.args[8], iso.display().to_string());
    // `drives` is always explicit, never inherited (T5).
    assert!(script_of(make).contains("drives:"));
    // A scripted VM gets no display at all, so one is added (§3).
    let display = calls
        .iter()
        .find(|c| is_osascript(c, "displays of c"))
        .expect("a display is added");
    assert_eq!(display.args[1], new_id);
    assert_eq!(display.args[2], "virtio-vga");
}

#[tokio::test]
async fn create_installer_vm_for_arm64_virtualize_turns_the_hypervisor_on() {
    let new_id = "99999999-8888-7777-6666-555555555555";
    let fake = runner(move |cmd| {
        is_osascript(cmd, "make new virtual machine").then(|| ok(&format!("{new_id}\twg-i\n")))
    });
    let p = provider(fake.clone());
    p.create_installer_vm(
        "wg-i",
        GuestOs::Linux,
        GuestArch::Arm64,
        ExecutionMode::Virtualize,
        &resources(),
        Path::new("/iso/kali-arm64.iso"),
    )
    .await
    .unwrap();
    let calls = fake.calls();
    let make = calls
        .iter()
        .find(|c| is_osascript(c, "make new virtual machine"))
        .unwrap();
    assert_eq!(make.args[2], "aarch64");
    assert_eq!(make.args[5], "true");
    let display = calls
        .iter()
        .find(|c| is_osascript(c, "displays of c"))
        .unwrap();
    assert_eq!(display.args[2], "virtio-ramfb");
}

#[tokio::test]
async fn create_installer_vm_refuses_virtualize_on_x86_64() {
    let fake = runner(|_| None);
    let p = provider(fake.clone());
    let err = p
        .create_installer_vm(
            "wg-i",
            GuestOs::Linux,
            GuestArch::X86_64,
            ExecutionMode::Virtualize,
            &resources(),
            Path::new("/iso/kali-amd64.iso"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WargameError::Incompatible(_)), "{err:?}");
    assert!(fake.calls().is_empty());
}

/// T2: `duplicate` returns nothing at all, so the new UUID has to be found
/// by comparing the registry before and after -- never by trusting a name.
#[tokio::test]
async fn clone_vm_finds_the_new_uuid_by_comparing_the_registry() {
    let new_id = "11111111-2222-3333-4444-555555555555";
    let before = LIST_MULTI.to_string();
    let after = format!("{before}{new_id} stopped  wg-clone\n");
    let n = Arc::new(std::sync::Mutex::new(0usize));
    let seen = n.clone();
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            let mut n = seen.lock().unwrap();
            *n += 1;
            return Some(ok(if *n == 1 { &before } else { &after }));
        }
        None
    });
    let p = provider(fake.clone());

    let vm = p.clone_vm(X86_ID, "wg-clone").await.unwrap();
    assert_eq!(vm.id, new_id);
    assert_eq!(vm.name, "wg-clone");
}

#[tokio::test]
async fn clone_vm_fails_when_the_registry_gained_nothing() {
    let fake = runner(|cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_MULTI)));
    let p = provider(fake);
    let err = p.clone_vm(X86_ID, "wg-clone").await.unwrap_err();
    assert!(err.to_string().contains("wg-clone"), "{err}");
}

#[tokio::test]
async fn attach_iso_appends_a_removable_drive_with_the_path_as_data() {
    let fake = runner(|_| None);
    let p = provider(fake.clone());
    let iso = Path::new("/Users/moreal/Library/Application Support/wargame-vm/iso/tools.iso");
    p.attach_iso(&vm_ref(X86_ID, "wg-x"), iso).await.unwrap();
    let calls = fake.calls();
    let call = calls
        .iter()
        .find(|c| is_osascript(c, "drives of c"))
        .expect("an attach call");
    assert_eq!(call.args[1], X86_ID);
    assert_eq!(call.args[2], iso.display().to_string());
    // T5: `POSIX file` is evaluated outside the `tell` block.
    let script = script_of(call);
    let posix = script.find("POSIX file").unwrap();
    let tell = script.find("tell application").unwrap();
    assert!(
        posix < tell,
        "POSIX file must be resolved outside the tell block"
    );
}

// --- ejecting the installer media (D3) -------------------------------------

/// The verified mechanism (docs §2 "Change any setting"): write the whole
/// `drives` list back without the removable entries, on a **stopped** VM.
/// Each surviving drive is echoed back by its own `id`, the same rule that
/// makes "replace an ISO" replace rather than append.
#[tokio::test]
async fn eject_writes_the_drives_back_without_the_removable_ones() {
    let fake = runner(|cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&owned_list(X86_ID, "stopped", "wg-x")));
        }
        is_osascript(cmd, "removable of d").then(|| ok("1\n"))
    });
    let p = provider(fake.clone());

    let removed = p
        .eject_removable_drives(&vm_ref(X86_ID, "wg-x"))
        .await
        .unwrap();
    assert_eq!(removed, Ejected::Detached(1));

    let calls = fake.calls();
    let call = calls
        .iter()
        .find(|c| is_osascript(c, "removable of d"))
        .expect("an eject call");
    assert_eq!(call.args[1], X86_ID, "the VM travels as argv, by UUID");
    let script = script_of(call);
    assert!(
        script.contains("if (id of vm) is vmId then"),
        "addressed by UUID, never by name: {script}"
    );
    assert!(!script.contains("virtual machine named"), "{script}");
    assert!(
        script.contains("set drives of c to"),
        "the drives list is written back: {script}"
    );
    assert!(
        script.contains("update configuration of (first virtual machine whose id is vmId) with c"),
        "the write needs `update configuration`: {script}"
    );
    assert!(
        script.contains("id:(id of d)"),
        "a kept drive must echo its own id back, or UTM adds one instead: {script}"
    );
}

/// Idempotent: a VM whose installer CD is already gone reports zero and
/// still succeeds, so `profile build` and `profile eject` can both run twice.
#[tokio::test]
async fn ejecting_a_vm_with_no_removable_drive_removes_nothing_and_succeeds() {
    let fake = runner(|cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&owned_list(X86_ID, "stopped", "wg-x")));
        }
        is_osascript(cmd, "removable of d").then(|| ok("0\n"))
    });
    let p = provider(fake);
    assert_eq!(
        p.eject_removable_drives(&vm_ref(X86_ID, "wg-x"))
            .await
            .unwrap(),
        Ejected::Detached(0)
    );
}

/// T9: `update configuration` is accepted only while the VM is `stopped`,
/// so the drive list is never written to a live one.
/// The one write in this crate that could destroy a user's OS install. A
/// VM adopted with `--from-vm` can have its boot disk flagged removable
/// (`removable` is fixed when a drive is created), and "detach every
/// removable drive" would then empty it. The script refuses instead, and
/// the refusal reaches the caller as its own outcome rather than as "0
/// drives detached".
#[tokio::test]
async fn eject_refuses_when_no_drive_would_survive_it() {
    let fake = runner(|cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&owned_list(X86_ID, "stopped", "wg-x")));
        }
        is_osascript(cmd, "removable of d").then(|| ok("wargame-utm:no-surviving-drive\n"))
    });
    let p = provider(fake.clone());

    assert_eq!(
        p.eject_removable_drives(&vm_ref(X86_ID, "wg-x"))
            .await
            .unwrap(),
        Ejected::EveryDriveRemovable,
        "it must not be reported as a successful eject of nothing"
    );

    // The script is the only thing that decides whether to write, so the
    // guard is asserted on its text (see automation.rs's own unit test);
    // here, what matters is that nothing *else* went out to write a
    // configuration afterwards.
    let writes: Vec<String> = fake
        .calls()
        .iter()
        .map(script_of)
        .filter(|script| script.contains("update configuration"))
        .collect();
    assert_eq!(
        writes.len(),
        1,
        "only the eject script itself, which takes its own refusal branch"
    );
    assert!(writes[0].contains("if keptCount is 0 then"));
}

#[tokio::test]
async fn eject_refuses_a_vm_that_is_not_stopped() {
    for status in ["started", "paused", "stopping"] {
        let listing = owned_list(X86_ID, status, "wg-x");
        let fake = runner(move |cmd| (utmctl_sub(cmd) == Some("list")).then(|| ok(&listing)));
        let p = provider(fake.clone());
        let err = p
            .eject_removable_drives(&vm_ref(X86_ID, "wg-x"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(status), "{status}: {err}");
        assert!(
            !fake
                .calls()
                .iter()
                .any(|c| is_osascript(c, "removable of d")),
            "{status}: nothing may be written to a VM that is not stopped"
        );
    }
}

#[tokio::test]
async fn eject_refuses_a_vm_outside_the_apps_namespace() {
    let fake = runner(|cmd| {
        (utmctl_sub(cmd) == Some("list")).then(|| ok(&owned_list(X86_ID, "stopped", "kindle-vm")))
    });
    let p = provider(fake.clone());
    let err = p
        .eject_removable_drives(&vm_ref(X86_ID, "kindle-vm"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("kindle-vm"), "{err}");
    assert!(!fake
        .calls()
        .iter()
        .any(|c| is_osascript(c, "removable of d")));
}

#[tokio::test]
async fn eject_of_a_vm_utm_no_longer_knows_is_an_error() {
    let p = provider(runner(|cmd| {
        (utmctl_sub(cmd) == Some("list")).then(|| ok(LIST_EMPTY))
    }));
    let err = p
        .eject_removable_drives(&vm_ref(UNKNOWN_ID, "wg-x"))
        .await
        .unwrap_err();
    assert!(matches!(err, WargameError::VmNotFound(_)), "{err:?}");
}

/// D1: the qcow2 does not go anywhere near `$WARGAME_HOME`. UTM is
/// sandboxed and writes its bundles into its own container, so that is the
/// volume a free-space check has to measure -- and only once it exists,
/// because it does not until UTM has run at least once.
#[test]
fn the_storage_directory_is_utms_own_sandbox_container_when_it_exists() {
    assert_eq!(
        wargame_utm::UTM_VM_DIRECTORY,
        "Library/Containers/com.utmapp.UTM/Data/Documents"
    );
    // Against a home of this test's own making, so the answer does not
    // depend on whether the machine running it has UTM installed.
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        wargame_utm::utm_vm_directory_in(home.path()),
        None,
        "a home where UTM has never run has no container yet"
    );

    let container = home.path().join(wargame_utm::UTM_VM_DIRECTORY);
    std::fs::create_dir_all(&container).unwrap();
    assert_eq!(
        wargame_utm::utm_vm_directory_in(home.path()),
        Some(container)
    );
}

#[tokio::test]
async fn image_readiness_without_a_build_record_is_not_ready() {
    let p = provider(runner(|_| None));
    // Task 4 records built images; until then nothing is ready, and the
    // answer is a plain `false`, never an error.
    assert!(!p
        .image_exists(&ImageRef("wargame-profile-kali".to_string()))
        .await
        .unwrap());
}

// --- a VM that vanished mid-operation --------------------------------------

/// An id-addressed script whose `repeat` loop finds nothing returns the
/// sentinel with **exit 0** -- a perfectly successful `osascript` run. Every
/// verb has to turn that into an error: a silent no-op reported as success
/// is how an isolated session comes up on the source VM's network, or a
/// delete reports it deleted something it did not.
#[tokio::test]
async fn every_id_addressed_verb_errors_when_the_vm_is_gone() {
    macro_rules! assert_vanished {
        ($call:expr, $what:literal) => {{
            let err = $call
                .await
                .expect_err(concat!($what, " must not silently succeed"));
            assert!(
                matches!(err, WargameError::VmNotFound(_)),
                "{}: {err:?}",
                $what
            );
            assert!(err.to_string().contains(UNKNOWN_ID), "{}: {err}", $what);
        }};
    }

    let fake = runner(|cmd| {
        // Everything but the version probe falls through its repeat loop.
        (cmd.program == "osascript" && !script_of(cmd).contains("UTM version"))
            .then(|| ok("wargame-utm:vm-not-found\n"))
    });
    let a = Automation::new(fake as Arc<dyn ProcessRunner>, UtmConfig::default());
    let iso = Path::new("/iso/tools.iso");

    // lifecycle verbs
    assert_vanished!(a.start(UNKNOWN_ID), "start");
    assert_vanished!(a.stop(UNKNOWN_ID, StopMethod::Force), "stop");
    assert_vanished!(a.delete(UNKNOWN_ID), "delete");
    assert_vanished!(a.duplicate(UNKNOWN_ID, "wg-copy"), "duplicate");
    // configuration writes
    assert_vanished!(a.set_hardware(UNKNOWN_ID, 4, 8192, false), "set_hardware");
    assert_vanished!(a.set_network_mode(UNKNOWN_ID, "host"), "set_network_mode");
    assert_vanished!(a.set_display(UNKNOWN_ID, "virtio-vga"), "set_display");
    // drive writes
    assert_vanished!(a.attach_iso(UNKNOWN_ID, iso), "attach_iso");
    assert_vanished!(a.replace_iso(UNKNOWN_ID, iso), "replace_iso");

    // `status` is the one verb that interprets the sentinel itself: there,
    // "gone" is the answer rather than a failure.
    assert_eq!(
        a.status(UNKNOWN_ID).await.unwrap(),
        wargame_utm::UtmStatus::Missing
    );
}

/// The realistic race: the VM is there when `start` reads its status and
/// gone by the time the network mode is written. The session must not boot
/// on whatever network the source VM had.
#[tokio::test]
async fn start_fails_when_the_vm_vanishes_before_its_network_is_applied() {
    let fake = runner(|cmd| {
        if is_osascript(cmd, "status of vm") {
            return Some(ok("stopped\tqemu\n"));
        }
        is_osascript(cmd, NETWORK_SCRIPT).then(|| ok("wargame-utm:vm-not-found\n"))
    });
    let p = provider(fake.clone());

    let err = p
        .start(&vm_ref(X86_ID, "wg-x"), &start_opts(NetworkMode::Isolated))
        .await
        .unwrap_err();
    assert!(matches!(err, WargameError::VmNotFound(_)), "{err:?}");
    assert!(
        !fake.calls().iter().any(|c| is_osascript(c, START_SCRIPT)),
        "the VM must not be started after the network write was lost"
    );
}

/// Same shape one level up: `delete` may not report success when the
/// `delete vm` script found nothing to delete.
#[tokio::test]
async fn delete_errors_when_the_delete_script_finds_nothing() {
    let listing = owned_list(X86_ID, "stopped", "wg-x");
    let fake = runner(move |cmd| {
        if utmctl_sub(cmd) == Some("list") {
            return Some(ok(&listing));
        }
        if is_osascript(cmd, "status of vm") {
            return Some(ok("stopped\tqemu\n"));
        }
        is_osascript(cmd, DELETE_SCRIPT).then(|| ok("wargame-utm:vm-not-found\n"))
    });
    let p = provider(fake);
    let err = p.delete(&vm_ref(X86_ID, "wg-x")).await.unwrap_err();
    assert!(matches!(err, WargameError::VmNotFound(_)), "{err:?}");
}

/// Without UTM there is no console; an argv pointing at a `utmctl` that is
/// not installed would fail as "no such file or directory".
#[test]
fn console_without_utm_installed_is_no_console_at_all() {
    let fake = Arc::new(FakeRunner::new()); // no `which` entries
    let p = UtmProvider::new(fake as Arc<dyn ProcessRunner>, UtmConfig::default());
    assert_eq!(p.console(&vm_ref(X86_ID, "wg-x")), ConsoleDescriptor::None);
}
