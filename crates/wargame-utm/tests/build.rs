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
use wargame_core::ports::image_builder::{
    BuildRequest, EjectOutcome, ImageRegistrar, InstallerRequest,
};
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

    /// Whether a VM already carries this display name. UTM names each
    /// bundle after its VM, so a `duplicate` under a name that is taken
    /// cannot land (D5).
    fn has_name(&self, name: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, existing)| existing == name)
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
        ("delete (first virtual machine", "delete"),
        ("start (first virtual machine", "start"),
        ("stop (first virtual machine", "stop"),
        ("status of vm", "status"),
        // The MAC write also touches `network interfaces of c`, so its
        // own argument is matched first.
        ("address:macAddr", "mac"),
        ("network interfaces of c", "network"),
        ("set displays of c", "display"),
        ("set hypervisor of c", "hardware"),
        // The eject also touches `drives of c`, so it is matched first.
        ("removable of d", "eject"),
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
    /// How many removable drives the scripted VM still has. The eject
    /// script answers with this and empties it, so a second eject really
    /// does report "nothing attached".
    removable: Arc<Mutex<usize>>,
    /// Overrides what the eject script answers, for the branch where every
    /// drive is removable and the script refuses to write.
    eject_answer: Arc<Mutex<Option<String>>>,
    /// Makes the next `duplicate` fail the way UTM fails one whose bundle
    /// cannot be moved: the copy is registered, and *then* the error comes.
    duplicate_error: Arc<Mutex<Option<String>>>,
}

/// A fresh UUID for each copy: UTM's `duplicate` never reuses one, and a
/// rebuild has to be able to tell the new image from the one it replaces.
fn next_image_id(copies: &Arc<Mutex<usize>>) -> String {
    let mut n = copies.lock().unwrap();
    *n += 1;
    if *n == 1 {
        IMAGE_ID.to_string()
    } else {
        format!("B{n}111111-2222-3333-4444-555555555555")
    }
}

/// A recording [`ImageRegistrar`]: what the publish step wrote down, in
/// order.
///
/// When it is given the harness's call log it writes `registered:<id>` into
/// it too, so a test can place the registration *among* the osascript calls
/// rather than only after them -- which is the whole point of the contract:
/// nothing may happen to a registered VM before it is written down.
#[derive(Clone, Default)]
struct Registered {
    images: Arc<Mutex<Vec<PublishedImage>>>,
    calls: Option<Calls>,
}

impl Registered {
    fn with_calls(calls: Calls) -> Self {
        Registered {
            images: Arc::new(Mutex::new(Vec::new())),
            calls: Some(calls),
        }
    }

    fn all(&self) -> Vec<PublishedImage> {
        self.images.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ImageRegistrar for Registered {
    async fn image_created(&self, image: &PublishedImage) -> wargame_core::error::Result<()> {
        if let Some(calls) = &self.calls {
            calls.push(format!("registered:{}", image.id));
        }
        self.images.lock().unwrap().push(image.clone());
        Ok(())
    }
}

/// A runner that plays the scripted registry back. `guest_ip` answers with
/// `10.0.0.9` unless `no_guest_agent` is set.
fn harness(registry: Registry, no_guest_agent: bool) -> Harness {
    let calls = Calls::default();
    let fake = Arc::new(FakeRunner::new());
    fake.set_which("utmctl", PathBuf::from("/opt/homebrew/bin/utmctl"));
    let reg = registry.clone();
    let recorded = calls.clone();
    // Every installation VM here was made from an ISO, so it starts with
    // one removable drive.
    let removable = Arc::new(Mutex::new(1usize));
    let drives = removable.clone();
    let eject_answer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let eject_override = eject_answer.clone();
    let copies = Arc::new(Mutex::new(0usize));
    let duplicate_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let duplicate_override = duplicate_error.clone();
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
                    let forced = duplicate_override.lock().unwrap().take();
                    let taken = forced.is_some() || reg.has_name(&new_name);
                    let id = next_image_id(&copies);
                    // What UTM really does, verified live on 4.7.5: it
                    // copies the bundle, registers the copy, applies the
                    // requested name -- and only then moves the bundle into
                    // Documents. A name another VM already carries fails
                    // that move with -2700, naming the *staging* bundle
                    // rather than the name that clashed, and leaves the
                    // copy it registered behind (task-7-b2b-report.md D5).
                    reg.push(&id, "stopped", &new_name);
                    if taken {
                        Output {
                            status: 1,
                            stdout: String::new(),
                            stderr: forced.unwrap_or_else(|| "execution error: UTM got an error: \u{201c}wg-build-kali-custom 2.utm\u{201d} couldn\u{2019}t be moved to \u{201c}Documents\u{201d} because an item with the same name already exists. (-2700)\n".to_string()),
                        }
                    } else {
                        ok("ok\n")
                    }
                }
                "start" => {
                    reg.set_status(&first, "started");
                    ok("ok\n")
                }
                "stop" => {
                    reg.set_status(&first, "stopped");
                    ok("ok\n")
                }
                "eject" => {
                    if let Some(answer) = eject_override.lock().unwrap().clone() {
                        return Ok(ok(&answer));
                    }
                    let mut left = drives.lock().unwrap();
                    let removed = *left;
                    *left = 0;
                    ok(&format!("{removed}\n"))
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
        removable,
        eject_answer,
        duplicate_error,
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

    let outcome = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
        .await
        .unwrap_or_else(|e| panic!("build failed: {}", e.error));

    // Published image: a *copy*, so the installation VM stays available.
    assert_eq!(outcome.image.id, IMAGE_ID);
    assert!(
        outcome
            .image
            .name
            .starts_with("wg-build-image-kali-custom-"),
        "{}",
        outcome.image.name
    );
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
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("wg-build-image-kali-custom-")),
        "{names:?}"
    );

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

    let outcome = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
        .await
        .unwrap_or_else(|e| panic!("build failed: {}", e.error));
    assert!(
        outcome
            .image
            .name
            .starts_with("wg-build-image-kali-custom-"),
        "{}",
        outcome.image.name
    );
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

    let failure = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
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

// --- ejecting the installer CD (phase A, D3) -------------------------------

/// The build takes the CD out *before* it starts the VM. With the CD in,
/// UEFI boots the installer instead of the installed system, so the build
/// would burn its whole boot timeout and then publish a copy that still
/// carries the CD.
#[tokio::test]
async fn the_flow_ejects_the_installer_cd_before_starting_the_vm() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let outcome = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
        .await
        .unwrap_or_else(|e| panic!("build failed: {}", e.error));
    assert_eq!(outcome.media, EjectOutcome::Detached(1));

    let calls = h.calls.all();
    let eject = h.calls.index_of("osascript:eject").expect("an eject");
    let start = h.calls.index_of("osascript:start").expect("a start");
    assert!(
        eject < start,
        "the CD comes out before the VM boots: {calls:?}"
    );
    assert!(
        eject < h.calls.index_of("osascript:duplicate").unwrap(),
        "and long before the copy is taken: {calls:?}"
    );
    assert_eq!(*h.removable.lock().unwrap(), 0);
}

/// Idempotent: the second build finds nothing to eject and says so.
#[tokio::test]
async fn ejecting_a_vm_whose_cd_is_already_out_is_a_no_op() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    let vm = VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    };
    assert_eq!(
        h.builder
            .eject_installer_media(&vm, &mut |_| {})
            .await
            .unwrap(),
        EjectOutcome::Detached(1)
    );
    assert_eq!(
        h.builder
            .eject_installer_media(&vm, &mut |_| {})
            .await
            .unwrap(),
        EjectOutcome::NothingAttached
    );
}

/// T9: UTM writes a configuration only to a stopped VM. `profile console`
/// leaves the installation VM running, and that must not turn into a failed
/// build -- it is reported and the build carries on.
#[tokio::test]
async fn ejecting_a_running_installation_vm_is_blocked_not_a_failure() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "started", "wg-build-kali-custom")]),
        false,
    );
    let vm = VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    };
    let outcome = h
        .builder
        .eject_installer_media(&vm, &mut |_| {})
        .await
        .unwrap();
    match outcome {
        EjectOutcome::Blocked(reason) => {
            assert!(reason.contains("started"), "{reason}");
            assert!(
                reason.contains("stopped virtual machine"),
                "it has to say why: {reason}"
            );
        }
        other => panic!("a running VM must not be written to: {other:?}"),
    }
    assert!(!h.calls.contains("osascript:eject"), "{:?}", h.calls.all());
    assert_eq!(*h.removable.lock().unwrap(), 1);
}

/// The destructive branch, refused. `removable` is fixed when a drive is
/// created, so a VM adopted with `--from-vm` can have its boot disk flagged
/// removable; writing the list back without it would detach the user's
/// installed system. The script returns its refusal sentinel instead, the
/// build **carries on**, and the reason travels with the outcome.
#[tokio::test]
async fn a_build_continues_when_every_drive_is_removable_and_nothing_is_detached() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    *h.eject_answer.lock().unwrap() = Some("wargame-utm:no-surviving-drive\n".to_string());
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let mut messages = Vec::new();
    let outcome = run_build_flow(&h.builder, &req, &mut |m| messages.push(m), &(), &())
        .await
        .unwrap_or_else(|e| panic!("the build must not fail over this: {}", e.error));

    match &outcome.media {
        EjectOutcome::EveryDriveRemovable(reason) => {
            assert!(reason.contains("removable"), "{reason}");
            assert!(
                reason.contains("Nothing was changed"),
                "it has to say nothing happened: {reason}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        messages.iter().any(|m| m.contains("refusing to detach")),
        "{messages:?}"
    );
    // The build still ran to a published image.
    assert_eq!(outcome.image.id, IMAGE_ID);
}

#[tokio::test]
async fn ejecting_from_a_vm_utm_no_longer_knows_is_an_error() {
    let h = harness(Registry::default(), false);
    let vm = VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    };
    let err = h
        .builder
        .eject_installer_media(&vm, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, WargameError::VmNotFound(_)), "{err:?}");
}

/// The installation VM was deleted in UTM behind our back: say so, and never
/// silently create another one (spec.md §3 FR-5).
#[tokio::test]
async fn a_missing_installation_vm_is_reported_not_recreated() {
    let h = harness(Registry::default(), false);
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let failure = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
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

/// **D5.** A profile that already had an image could never be rebuilt:
/// `duplicate` was asked for the fixed image name, the previous image still
/// carried it, and UTM fails the bundle move it does last -- so the build
/// died at `Publishing` *after* a complete provision and verify, and left
/// the copy it had already registered behind (task-7-b2b-report.md D5, AC-06
/// and AC-09).
///
/// The fake here is the mechanism, not a stand-in for it: it refuses a
/// duplicate under a name any registered VM already carries, exactly as UTM
/// 4.7.5 does.
#[tokio::test]
async fn a_second_build_publishes_a_new_image_and_leaves_the_previous_one_alone() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);

    let first = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
        .await
        .unwrap_or_else(|e| panic!("first build failed: {}", e.error));
    let second = run_build_flow(&h.builder, &req, &mut |_| {}, &(), &())
        .await
        .unwrap_or_else(|e| panic!("the rebuild failed: {}", e.error));

    assert_ne!(first.image.id, second.image.id, "a rebuild makes a new VM");
    assert_ne!(
        first.image.name, second.image.name,
        "and asks UTM for a name nothing holds"
    );
    // Both are still registered: publishing never deletes the image it
    // replaces -- that happens only once the record names the new one.
    let names = h.registry.names();
    assert!(names.contains(&first.image.name), "{names:?}");
    assert!(names.contains(&second.image.name), "{names:?}");
    assert!(
        !h.calls.contains("osascript:delete"),
        "publishing deletes nothing: {:?}",
        h.calls.all()
    );
}

/// And when UTM refuses the copy anyway, the VM it registered before
/// refusing is written down before the error is raised -- so the record
/// carries it as a leaked image instead of nobody knowing it exists (D5).
#[tokio::test]
async fn a_publish_utm_refuses_still_records_the_vm_it_registered() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    *h.duplicate_error.lock().unwrap() = Some(
        "execution error: UTM got an error: \u{201c}wg-build-kali-custom 2.utm\u{201d} couldn\u{2019}t be moved to \u{201c}Documents\u{201d} because an item with the same name already exists. (-2700)\n"
            .to_string(),
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);
    let vm = VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    };

    let registered = Registered::default();
    let err = h
        .builder
        .publish(&req, &vm, &mut |_| {}, &registered)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("-2700"), "{err}");

    let seen = registered.all();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].id, IMAGE_ID);
    assert!(
        h.registry.names().contains(&seen[0].name),
        "the recorded VM is the one UTM actually registered"
    );
}

/// **The registrar's contract.** `ImageRegistrar` promises the backend
/// reports a VM it created "before it does anything else with it". The
/// copy's MAC is written by the same call that makes the copy, so the
/// report has to come from *inside* it -- between the registry diff that
/// finds the new UUID and the `update configuration` that rewrites its
/// hardware address. A crash in that window would otherwise leave an image
/// VM no record names, which `prune` then skips forever.
#[tokio::test]
async fn the_registrar_hears_about_the_copy_before_its_mac_is_written() {
    let h = harness(
        Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]),
        false,
    );
    let profile = kali_profile();
    let record = record_with_installer(&profile);
    let req = request(&profile, &record);
    let vm = VmRef {
        backend: "utm".to_string(),
        id: INSTALLER_ID.to_string(),
        name: "wg-build-kali-custom".to_string(),
    };

    let registered = Registered::with_calls(h.calls.clone());
    let image = h
        .builder
        .publish(&req, &vm, &mut |_| {}, &registered)
        .await
        .unwrap();

    let seen = registered.all();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].id, image.id);

    let calls = h.calls.all();
    let announced = h
        .calls
        .index_of(&format!("registered:{}", image.id))
        .unwrap_or_else(|| panic!("the copy was never registered: {calls:?}"));
    let mac = h
        .calls
        .index_of(&format!("osascript:mac:{}", image.id))
        .unwrap_or_else(|| panic!("the copy never got its own MAC: {calls:?}"));
    assert!(
        announced < mac,
        "the copy must be written down before its MAC is rewritten: {calls:?}"
    );
    // ...and before anything else touches it at all.
    let after: Vec<&String> = calls.iter().skip(announced).collect();
    assert!(
        !after.iter().any(|c| c.starts_with("osascript:")
            && c.ends_with(&image.id)
            && *c != &format!("registered:{}", image.id)
            && !c.starts_with("osascript:mac")),
        "nothing but the MAC write may follow the registration: {after:?}"
    );
}

/// A copy under a name UTM already has is refused *before* the copy is
/// attempted, with the name that actually clashed -- UTM's own message
/// names the staging bundle (`<source> 2.utm`) instead, which sent the
/// first investigation looking in the wrong place.
#[tokio::test]
async fn a_copy_is_refused_before_utm_can_fail_on_a_name_it_already_has() {
    let h = harness(
        Registry::with(&[
            (SOURCE_ID, "stopped", "kali base"),
            (INSTALLER_ID, "stopped", "wg-build-kali-custom"),
        ]),
        false,
    );
    let profile = kali_profile();
    let source = BuildSource::UtmVm {
        uuid: SOURCE_ID.to_string(),
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
    let message = err.to_string();
    assert!(message.contains("wg-build-kali-custom"), "{message}");
    assert!(message.contains(INSTALLER_ID), "{message}");
    assert!(
        !h.calls.contains("osascript:duplicate"),
        "nothing was copied: {:?}",
        h.calls.all()
    );
}

#[tokio::test]
async fn the_image_vm_name_is_inside_the_build_namespace_and_never_repeats() {
    let h = harness(Registry::default(), false);
    let name = h.builder.image_vm_name("kali-custom");
    assert!(
        name.starts_with("wg-build-image-kali-custom-"),
        "the profile is still recognisable in UTM's sidebar: {name}"
    );
    // Not the session prefix, so `prune`'s name-independent ownership check
    // is the only thing that can reach it.
    assert!(!name.starts_with("wg-dh-"));
    // D5: UTM names each bundle after its VM and refuses a copy under a
    // name that is taken, so two builds of one profile must never ask for
    // the same one -- the previous image still holds its name while the new
    // one is being made.
    let names: std::collections::HashSet<String> = (0..32)
        .map(|_| h.builder.image_vm_name("kali-custom"))
        .collect();
    assert!(names.len() > 28, "{names:?}");
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

    let registry = Registry::with(&[(IMAGE_ID, "stopped", "wg-build-image-kali-custom")]);
    let provider = catalog_provider(&registry, builds.clone());

    assert!(provider
        .image_exists(&ImageRef(IMAGE_ID.to_string()))
        .await
        .unwrap());
    assert!(!provider
        .image_exists(&ImageRef("NOT-BUILT".to_string()))
        .await
        .unwrap());
}

/// **D7.** The build record naming an image is not the same as UTM having
/// it. After the images were deleted in UTM, `profiles` and `doctor` both
/// still said `ready` and `launch` accepted it (task-7-b2b-report.md D7;
/// spec.md §6's "외부에서 VM 삭제 → missing 반영"). Tart asks its backend;
/// UTM asks its own now.
#[tokio::test]
async fn an_image_the_record_names_but_utm_no_longer_has_is_not_ready() {
    let builds = Arc::new(MemoryBuildRepo::new());
    let profile = kali_profile();
    let mut record = record_with_installer(&profile);
    record.image = Some(PublishedImage::new(IMAGE_ID, "wg-build-image-kali-custom"));
    record.set_phase(BuildPhase::Ready);
    builds.save(&record).await.unwrap();

    // Everything the record says is still true; UTM simply does not have
    // the VM any more.
    let registry = Registry::with(&[(INSTALLER_ID, "stopped", "wg-build-kali-custom")]);
    let provider = catalog_provider(&registry, builds.clone());

    assert!(!provider
        .image_exists(&ImageRef(IMAGE_ID.to_string()))
        .await
        .unwrap());
}

/// A `UtmProvider` whose `utmctl list` answers from `registry` and whose
/// image catalog is `builds`.
fn catalog_provider(registry: &Registry, builds: Arc<MemoryBuildRepo>) -> UtmProvider {
    let reg = registry.clone();
    let fake = Arc::new(FakeRunner::new());
    fake.set_which("utmctl", PathBuf::from("/opt/homebrew/bin/utmctl"));
    fake.set_responder(move |cmd: &Cmd| {
        if cmd.program.ends_with("utmctl") {
            return Ok(match cmd.args.first().map(String::as_str).unwrap_or("") {
                "version" => ok("4.7.5\n"),
                "list" => ok(&reg.table()),
                _ => ok(""),
            });
        }
        Ok(ok(""))
    });
    UtmProvider::new(fake as Arc<dyn ProcessRunner>, UtmConfig::default()).with_image_catalog(
        Arc::new(RecordImageCatalog::new(
            builds as Arc<dyn BuildRecordRepository>,
        )),
    )
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
    let image = h
        .builder
        .publish(&req, &vm, &mut |_| {}, &())
        .await
        .unwrap();
    assert!(
        image.name.starts_with("wg-build-image-kali-custom-"),
        "{}",
        image.name
    );
}
