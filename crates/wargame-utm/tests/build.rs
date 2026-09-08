//! `UtmImageBuilder` against a fake `ProcessRunner` (spec.md §3 FR-2/FR-3,
//! §7 AC-04/AC-05/AC-09).
//!
//! Like `tests/provider.rs`, nothing here talks to UTM: every `utmctl` and
//! `osascript` invocation is answered from a scripted registry, so the whole
//! build flow -- installation VM, boot, connect, publish, cleanup -- runs on
//! a host with no UTM installed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wargame_core::app::profile_build::run_build_flow;
use wargame_core::domain::build::{
    BuildPhase, BuildRecord, BuildSource, PublishedImage, VerificationResult,
};
use wargame_core::domain::{
    compat, ExecutionMode, GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, ProvisionSpec,
    Resources, ToolsSpec, VmRef, VmSettings, WorkspaceSpec,
};
use wargame_core::error::WargameError;
use wargame_core::infra::process::{Cmd, Output, ProcessRunner};
use wargame_core::ports::image_builder::{BuildRequest, InstallerRequest};
use wargame_core::ports::{
    BuildRecordRepository, GuestTransport, ImageRef, ProfileImageBuilder, VmProvider,
};
use wargame_core::testing::{FakeRunner, FakeTransport, MemoryBuildRepo};

use wargame_utm::{ImageCatalog, RecordImageCatalog, UtmConfig, UtmImageBuilder, UtmProvider};

const INSTALLER_ID: &str = "A8E9F979-A198-433A-B2DE-74A6D7302D2F";
const IMAGE_ID: &str = "B1111111-2222-3333-4444-555555555555";
/// An installed VM the user already has, used as a `--from-vm` source.
const SOURCE_ID: &str = "C2222222-3333-4444-5555-666666666666";

/// A scripted UTM registry: `utmctl list` answers from it, `duplicate`
/// appends to it, and `status` reads it.
#[derive(Clone, Default)]
struct Registry(Arc<Mutex<Vec<(String, String, String)>>>);

impl Registry {
    fn with(vms: &[(&str, &str, &str)]) -> Self {
        Registry(Arc::new(Mutex::new(
            vms.iter()
                .map(|(id, status, name)| (id.to_string(), status.to_string(), name.to_string()))
                .collect(),
        )))
    }

    fn table(&self) -> String {
        let mut out = "UUID                                 Status   Name\n".to_string();
        for (id, status, name) in self.0.lock().unwrap().iter() {
            out.push_str(&format!("{id} {status}  {name}\n"));
        }
        out
    }

    fn push(&self, id: &str, status: &str, name: &str) {
        self.0
            .lock()
            .unwrap()
            .push((id.to_string(), status.to_string(), name.to_string()));
    }

    fn set_status(&self, id: &str, status: &str) {
        if let Some(entry) = self
            .0
            .lock()
            .unwrap()
            .iter_mut()
            .find(|(vm_id, _, _)| vm_id == id)
        {
            entry.1 = status.to_string();
        }
    }

    fn status_of(&self, id: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|(vm_id, _, _)| vm_id == id)
            .map(|(_, status, _)| status.clone())
    }

    fn names(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, name)| name.clone())
            .collect()
    }
}

fn ok(stdout: &str) -> Output {
    Output {
        status: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn script_of(cmd: &Cmd) -> String {
    cmd.stdin
        .as_ref()
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .unwrap_or_default()
}

/// Every command the fake answered, so a test can assert what ran and in
/// what order. `osascript` invocations are recorded as
/// `osascript:<verb>:<argv[0]>`.
#[derive(Clone, Default)]
struct Calls(Arc<Mutex<Vec<String>>>);

impl Calls {
    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn push(&self, call: String) {
        self.0.lock().unwrap().push(call);
    }

    fn contains(&self, needle: &str) -> bool {
        self.all().iter().any(|c| c.contains(needle))
    }

    fn index_of(&self, needle: &str) -> Option<usize> {
        self.all().iter().position(|c| c.contains(needle))
    }
}

/// The AppleScript verb a script performs, for call recording.
fn verb_of(script: &str) -> &'static str {
    for (marker, verb) in [
        ("make new virtual machine", "make"),
        ("duplicate (first virtual machine", "duplicate"),
        ("delete vm", "delete"),
        ("start vm", "start"),
        ("stop vm", "stop"),
        ("status of vm", "status"),
        ("network interfaces of c", "network"),
        ("set displays of c", "display"),
        ("set hypervisor of c", "hardware"),
        ("drives of c", "drives"),
        ("UTM version", "version"),
    ] {
        if script.contains(marker) {
            return verb;
        }
    }
    "other"
}

struct Harness {
    builder: UtmImageBuilder,
    registry: Registry,
    calls: Calls,
    transport: Arc<FakeTransport>,
}

/// A runner that plays the scripted registry back. `guest_ip` answers with
/// `10.0.0.9` unless `no_guest_agent` is set.
fn harness(registry: Registry, no_guest_agent: bool) -> Harness {
    let calls = Calls::default();
    let fake = Arc::new(FakeRunner::new());
    fake.set_which("utmctl", PathBuf::from("/opt/homebrew/bin/utmctl"));
    let reg = registry.clone();
    let recorded = calls.clone();
    fake.set_responder(move |cmd: &Cmd| {
        if cmd.program.ends_with("utmctl") {
            let sub = cmd.args.first().map(String::as_str).unwrap_or("");
            recorded.push(format!("utmctl:{sub}"));
            return Ok(match sub {
                "version" => ok("4.7.5\n"),
                "list" => ok(&reg.table()),
                "ip-address" => {
                    if no_guest_agent {
                        Output {
                            status: 1,
                            stdout: String::new(),
                            stderr: "QEMU guest agent is not running".to_string(),
                        }
                    } else {
                        ok("10.0.0.9\n")
                    }
                }
                _ => ok(""),
            });
        }
        if cmd.program == "osascript" {
            let script = script_of(cmd);
            let verb = verb_of(&script);
            let first = cmd.args.get(1).cloned().unwrap_or_default();
            recorded.push(format!("osascript:{verb}:{first}"));
            return Ok(match verb {
                "version" => ok("4.7.5\n"),
                "make" => {
                    // argv[0] is the requested name.
                    let name = first;
                    reg.push(INSTALLER_ID, "stopped", &name);
                    ok(&format!("{INSTALLER_ID}\t{name}\n"))
                }
                "duplicate" => {
                    // argv: <source id> <new name>; the copy is a new UUID
                    // that only shows up in the next `list` (T2).
                    let new_name = cmd.args.get(2).cloned().unwrap_or_default();
                    reg.push(IMAGE_ID, "stopped", &new_name);
                    ok("ok\n")
                }
                "start" => {
                    reg.set_status(&first, "started");
                    ok("ok\n")
                }
                "stop" => {
                    reg.set_status(&first, "stopped");
                    ok("ok\n")
                }
                "status" => match reg.status_of(&first) {
                    Some(status) => ok(&format!("{status}\tqemu\n")),
                    None => ok("wargame-utm:vm-not-found\n"),
                },
                _ => ok("ok\n"),
            });
        }
        if cmd.program == "open" {
            recorded.push("open".to_string());
            return Ok(ok(""));
        }
        if cmd.program == "ps" {
            // No orphaned QEMU process (T12).
            recorded.push("ps".to_string());
            return Ok(ok(""));
        }
        recorded.push(cmd.program.clone());
        Ok(ok(""))
    });

    let utm = Arc::new(UtmProvider::new(
        fake as Arc<dyn ProcessRunner>,
        UtmConfig {
            boot_timeout: Duration::from_secs(4),
            virtualize_boot_timeout: Duration::from_secs(4),
            stop_timeout: Duration::from_secs(2),
            ip_poll_interval: Duration::from_millis(200),
            status_poll_interval: Duration::from_millis(100),
            ..UtmConfig::default()
        },
    ));
    let transport = Arc::new(FakeTransport::new());
    Harness {
        builder: UtmImageBuilder::new(utm, transport.clone() as Arc<dyn GuestTransport>),
        registry,
        calls,
        transport,
    }
}

fn kali_profile() -> Profile {
    Profile {
        name: "kali-custom".to_string(),
        description: String::new(),
        vm: VmSettings {
            backend: wargame_core::domain::BackendChoice::Utm,
            mode: ExecutionMode::Virtualize,
        },
        guest: GuestSpec {
            os: GuestOs::Linux,
            arch: GuestArch::Arm64,
            distribution: Some("kali".to_string()),
            user: "kali".to_string(),
            password: None,
        },
        image: ImageSpec {
            base: String::new(),
        },
        resources: Resources {
            cpu: 4,
            memory_mib: 8192,
            disk_gib: 80,
        },
        provision: ProvisionSpec {
            scripts: vec!["scripts/kali/common.sh".to_string()],
        },
        tools: ToolsSpec {
            verify: vec!["python3 --version".to_string()],
        },
        workspace: WorkspaceSpec {
            root: "/home/kali/wargame".to_string(),
        },
    }
}

fn record_with_installer(profile: &Profile) -> BuildRecord {
    let mut record = BuildRecord::new(
        profile,
        "utm",
        BuildSource::Iso {
            path: PathBuf::from("/iso/kali-arm64.iso"),
            sha256: None,
            verified_sha256: false,
        },
    );
    record.installer_vm = Some(VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    });
    record.set_phase(BuildPhase::AwaitingInstall);
    record
}

fn request<'a>(profile: &'a Profile, record: &'a BuildRecord) -> BuildRequest<'a> {
    BuildRequest {
        profile,
        record,
        execution: compat::resolve(profile).unwrap(),
        scripts_dir: PathBuf::from("/home/profiles"),
        log_path: std::env::temp_dir().join("wargame-utm-build-test/profile-build.log"),
        boot_timeout: Duration::from_secs(4),
    }
}

// --- profile create -------------------------------------------------------

#[tokio::test]
async fn an_iso_source_makes_one_vm_with_the_installer_media() {
    let h = harness(Registry::default(), false);
    let profile = kali_profile();
    let source = BuildSource::Iso {
        path: PathBuf::from("/iso/kali-arm64.iso"),
        sha256: None,
        verified_sha256: false,
    };
    let vm = h
        .builder
        .create_installer_vm(&InstallerRequest {
            profile: &profile,
            name: "wg-build-kali-custom".to_string(),
            execution: compat::resolve(&profile).unwrap(),
            source: &source,
        })
        .await
        .unwrap();

    assert_eq!(vm.backend, "utm");
    assert_eq!(vm.id, INSTALLER_ID);
    assert_eq!(vm.name, "wg-build-kali-custom");
    assert!(h.calls.contains("osascript:make:wg-build-kali-custom"));
    assert_eq!(h.registry.names(), vec!["wg-build-kali-custom".to_string()]);
    // The installation VM gets NAT and a display so the user can install
    // through the console.
    assert!(h.calls.contains("osascript:network"));
    assert!(h.calls.contains("osascript:display"));
}

/// AC-05: cloning an installed VM copies it and leaves the source alone.
#[tokio::test]
async fn a_from_vm_source_duplicates_and_never_touches_the_source() {
    let h = harness(
        Registry::with(&[(SOURCE_ID, "stopped", "kali base")]),
        false,
    );
    let profile = kali_profile();
    let source = BuildSource::UtmVm {
        uuid: SOURCE_ID.to_string(),
    };
    let vm = h
        .builder
        .create_installer_vm(&InstallerRequest {
            profile: &profile,
            name: "wg-build-kali-custom".to_string(),
            execution: compat::resolve(&profile).unwrap(),
            source: &source,
        })
        .await
        .unwrap();

    assert_eq!(vm.name, "wg-build-kali-custom");
    assert_ne!(vm.id, SOURCE_ID);
    assert!(h
        .calls
        .contains(&format!("osascript:duplicate:{SOURCE_ID}")));
    assert!(
        !h.calls.contains(&format!("osascript:start:{SOURCE_ID}")),
        "the source VM must never be started: {:?}",
        h.calls.all()
    );
    assert!(
        !h.calls.contains("osascript:delete"),
        "the source VM must never be deleted"
    );
    // The source is still registered, untouched.
    assert!(h.registry.names().contains(&"kali base".to_string()));
}

#[tokio::test]
async fn a_tart_base_source_is_refused() {
    let h = harness(Registry::default(), false);
    let profile = kali_profile();
    let source = BuildSource::TartBase {
        image: "ghcr.io/x:latest".to_string(),
    };
    let err = h
        .builder
        .create_installer_vm(&InstallerRequest {
            profile: &profile,
            name: "wg-build-kali-custom".to_string(),
            execution: compat::resolve(&profile).unwrap(),
            source: &source,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, WargameError::InvalidProfile { .. }),
        "{err:?}"
    );
    assert!(h.calls.all().is_empty());
}

// --- profile build --------------------------------------------------------

#[tokio::test]
async fn the_whole_flow_provisions_the_installation_vm_and_publishes_a_copy() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let outcome = run_build_flow(&h.builder, &req, &mut |_| {})
        .await
        .unwrap_or_else(|e| panic!("build failed: {}", e.error));

    // Published image: a *copy*, so the installation VM stays available.
    assert_eq!(outcome.image.id, IMAGE_ID);
    assert_eq!(outcome.image.name, "wg-build-image-kali-custom");
    assert_eq!(outcome.vm.id, INSTALLER_ID, "provisioned in place");
    assert_eq!(
        outcome.verification,
        vec![VerificationResult {
            command: "python3 --version".to_string(),
            exit_code: 0
        }]
    );

    let calls = h.calls.all();
    let start = h.calls.index_of("osascript:start").expect("VM started");
    let ip = h.calls.index_of("utmctl:ip-address").expect("IP awaited");
    let stop = h.calls.index_of("osascript:stop").expect("VM stopped");
    let duplicate = h
        .calls
        .index_of("osascript:duplicate")
        .expect("image published");
    assert!(start < ip, "the VM must boot before its address is read");
    assert!(ip < stop, "provisioning happens before the stop");
    assert!(
        stop < duplicate,
        "the image is copied only from a stopped VM: {calls:?}"
    );
    assert!(
        !h.calls.contains("osascript:delete"),
        "a build deletes nothing: {calls:?}"
    );

    // Both VMs now exist: the installation VM and the image.
    let names = h.registry.names();
    assert!(names.contains(&"wg-build-kali-custom".to_string()));
    assert!(names.contains(&"wg-build-image-kali-custom".to_string()));

    // The guest really was provisioned over the transport.
    let guest: Vec<String> = h
        .transport
        .calls()
        .into_iter()
        .map(|c| format!("{c:?}"))
        .collect();
    assert!(guest
        .iter()
        .any(|c| c.contains("wg-provision/scripts/kali/common.sh")));
}

/// `profile console` leaves the installation VM running, and that is the
/// normal way in to `profile build`. UTM refuses a second `start` with
/// -2700, so the build must use the running VM rather than starting it.
#[tokio::test]
async fn a_build_uses_an_installation_vm_the_console_left_running() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "started", "wg-build-kali-custom")]),
        false,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let outcome = run_build_flow(&h.builder, &req, &mut |_| {})
        .await
        .unwrap_or_else(|e| panic!("build failed: {}", e.error));
    assert_eq!(outcome.image.name, "wg-build-image-kali-custom");
    assert!(
        !h.calls.contains("osascript:start"),
        "a running VM must not be started again: {:?}",
        h.calls.all()
    );
    // It still gets stopped before the image is copied.
    let stop = h.calls.index_of("osascript:stop").expect("VM stopped");
    let duplicate = h.calls.index_of("osascript:duplicate").expect("published");
    assert!(stop < duplicate);
}

/// spec.md §6's "SSH/guest agent 미준비" and "긴 emulate 부팅" rows: the
/// guest that never reports an address fails at `connect` with the stage,
/// the timeout and how to raise it -- and the installation VM survives.
#[tokio::test(start_paused = true)]
async fn a_guest_without_the_agent_fails_at_connect_and_keeps_the_installation_vm() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        true,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let failure = run_build_flow(&h.builder, &req, &mut |_| {})
        .await
        .err()
        .expect("the build must fail");
    assert_eq!(
        failure.stage,
        wargame_core::domain::build::BuildStage::Connecting
    );
    assert!(
        failure.awaiting_install,
        "an unreachable guest is a wait, not a broken build"
    );
    let message = failure.error.to_string();
    assert!(message.contains("guest agent"), "{message}");
    let hint = failure.error.hint().unwrap();
    assert!(hint.contains("boot_timeout_secs"), "{hint}");

    assert!(
        !h.calls.contains("osascript:delete"),
        "the installation VM must never be deleted by a failure: {:?}",
        h.calls.all()
    );
    assert!(h
        .registry
        .names()
        .contains(&"wg-build-kali-custom".to_string()));
    assert!(
        !h.registry
            .names()
            .contains(&"wg-build-image-kali-custom".to_string()),
        "a failed build publishes nothing"
    );
}

/// The installation VM was deleted in UTM behind our back: say so, and never
/// silently create another one (spec.md §3 FR-5).
#[tokio::test]
async fn a_missing_installation_vm_is_reported_not_recreated() {
    let h = harness(Registry::default(), false);
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let failure = run_build_flow(&h.builder, &req, &mut |_| {})
        .await
        .err()
        .unwrap();
    assert_eq!(
        failure.stage,
        wargame_core::domain::build::BuildStage::Preparing
    );
    assert!(matches!(failure.error, WargameError::VmNotFound(_)));
    let message = failure.error.to_string();
    assert!(message.contains("builds/kali-custom"), "{message}");
    assert!(message.contains("profile create kali-custom"), "{message}");
    assert!(!h.calls.contains("osascript:make"));
    assert!(!h.calls.contains("osascript:duplicate"));
}

#[tokio::test]
async fn cleanup_after_a_failure_stops_the_installation_vm_but_never_deletes_it() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "started", "wg-build-kali-custom")]),
        false,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);
    let vm = record.installer_vm.clone().unwrap();

    let failures = h.builder.cleanup_failed_build(&req, &vm).await;
    assert!(failures.is_empty(), "{failures:?}");
    assert!(h.calls.contains("osascript:stop"));
    assert!(!h.calls.contains("osascript:delete"));
}

#[tokio::test]
async fn discarding_a_replaced_image_deletes_that_vm_only() {
    let h = harness(
        Registry::with(&[
            (INSTALLER_ID, "stopped", "wg-build-kali-custom"),
            (IMAGE_ID, "stopped", "wg-build-image-kali-custom"),
        ]),
        false,
    );
    h.builder
        .discard_image(&PublishedImage::new(IMAGE_ID, "wg-build-image-kali-custom"))
        .await
        .unwrap();
    assert!(h.calls.contains(&format!("osascript:delete:{IMAGE_ID}")));
    assert!(!h
        .calls
        .contains(&format!("osascript:delete:{INSTALLER_ID}")));
}

#[tokio::test]
async fn the_image_vm_name_is_inside_the_build_namespace() {
    let h = harness(Registry::default(), false);
    assert_eq!(
        h.builder.image_vm_name("kali-custom"),
        "wg-build-image-kali-custom"
    );
    // Not the session prefix, so `prune`'s name-independent ownership check
    // is the only thing that can reach it.
    assert!(!h.builder.image_vm_name("kali-custom").starts_with("wg-dh-"));
}

// --- the image catalog ----------------------------------------------------

#[tokio::test]
async fn a_published_image_is_the_only_thing_the_catalog_calls_ready() {
    let builds = Arc::new(MemoryBuildRepo::new());
    let catalog = RecordImageCatalog::new(builds.clone() as Arc<dyn BuildRecordRepository>);
    let profile = kali_profile();

    // No record at all.
    assert!(!catalog
        .image_ready(&ImageRef(IMAGE_ID.to_string()))
        .await
        .unwrap());

    // Created but not published.
    let mut record = record_with_installer(&profile);
    builds.save(&record).await.unwrap();
    assert!(!catalog
        .image_ready(&ImageRef(IMAGE_ID.to_string()))
        .await
        .unwrap());

    // Published.
    record.image = Some(PublishedImage::new(IMAGE_ID, "wg-build-image-kali-custom"));
    record.set_phase(BuildPhase::Ready);
    builds.save(&record).await.unwrap();
    assert!(catalog
        .image_ready(&ImageRef(IMAGE_ID.to_string()))
        .await
        .unwrap());
    // A UUID nobody published stays unknown.
    assert!(!catalog
        .image_ready(&ImageRef(SOURCE_ID.to_string()))
        .await
        .unwrap());
}

/// The provider consults the catalog, so `image_exists` (and therefore
/// `doctor`) sees what a build published.
#[tokio::test]
async fn the_provider_reports_a_published_image_as_existing() {
    let builds = Arc::new(MemoryBuildRepo::new());
    let profile = kali_profile();
    let mut record = record_with_installer(&profile);
    record.image = Some(PublishedImage::new(IMAGE_ID, "wg-build-image-kali-custom"));
    record.set_phase(BuildPhase::Ready);
    builds.save(&record).await.unwrap();

    let provider = UtmProvider::new(
        Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        UtmConfig::default(),
    )
    .with_image_catalog(Arc::new(RecordImageCatalog::new(
        builds as Arc<dyn BuildRecordRepository>,
    )));

    assert!(provider
        .image_exists(&ImageRef(IMAGE_ID.to_string()))
        .await
        .unwrap());
    assert!(!provider
        .image_exists(&ImageRef("NOT-BUILT".to_string()))
        .await
        .unwrap());
}

#[tokio::test]
async fn a_stopped_installation_vm_is_what_publishing_copies() {
    // Guards the "never publish from a live disk" rule: the provider's own
    // status has to be `stopped` by the time `publish` runs.
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    let vm = VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    };
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    h.builder.shutdown(&req, &vm, &mut |_| {}).await.unwrap();
    assert_eq!(
        h.registry.status_of(INSTALLER_ID).as_deref(),
        Some("stopped")
    );
    let image = h.builder.publish(&req, &vm, &mut |_| {}).await.unwrap();
    assert_eq!(image.name, "wg-build-image-kali-custom");
}
