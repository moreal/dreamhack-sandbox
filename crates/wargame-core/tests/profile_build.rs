//! `profile init | create | console | build` over fakes (spec.md §3 FR-2/
//! FR-3, §7 AC-04/05/06/09).
//!
//! Nothing here touches UTM, Tart or a real VM: a [`FakeBuilder`] stands in
//! for the backend half of the build flow, so the record state machine --
//! what a re-run reuses, what a failure preserves, when a profile may be
//! called ready -- is exercised on its own, including the failure injections
//! AC-09 asks for.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use wargame_core::app::profile_build::{self, CreateSource};
use wargame_core::app::{
    AppContext, BackendRegistry, BuilderRegistry, ProfileStore, TransportRegistry,
};
use wargame_core::domain::build::{
    BuildPhase, BuildRecord, BuildSource, BuildStage, InstallerMedia, PublishedImage,
};
use wargame_core::domain::{GuestEndpoint, GuestShell, VmRef};
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::ProcessRunner;
use wargame_core::ports::image_builder::{
    BuildRequest, EjectOutcome, GuestLink, InstallerRequest, Progress,
};
use wargame_core::ports::{
    BuildRecordRepository, DiskSpace, GuestTransport, ProfileImageBuilder, VmProvider,
};
use wargame_core::testing::{
    FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryBuildRepo, MemoryRepo,
    TransportCall,
};

// --- fakes ----------------------------------------------------------------

/// A backend that records every build step it was asked for and can be made
/// to fail at any one of them.
struct FakeBuilder {
    backend: String,
    installer: bool,
    /// Whether `publish` re-uses one id for every build, the way Tart does
    /// (its image *is* `wargame-profile-<name>`), or mints a new one, the way
    /// UTM's `duplicate` does.
    stable_image_id: bool,
    /// The images that exist on this fake backend right now: `publish` adds
    /// one, `discard_image` removes it.
    live_images: Mutex<Vec<String>>,
    transport: Arc<FakeTransport>,
    /// Overrides `transport` when a test needs a guest that behaves
    /// differently (a reboot mid-provisioning, say).
    guest: Mutex<Option<Arc<dyn GuestTransport>>>,
    calls: Mutex<Vec<String>>,
    fail_on: Mutex<Option<&'static str>>,
    created: Mutex<Vec<String>>,
    published: Mutex<Vec<String>>,
    discarded: Mutex<Vec<String>>,
    cleanup_failures: Mutex<Vec<String>>,
    next_id: Mutex<u32>,
    /// The record store, so `discard_image` can prove the *new* image was
    /// already persisted when the old one was deleted.
    builds: Mutex<Option<Arc<MemoryBuildRepo>>>,
    last_profile: Mutex<String>,
    image_at_discard: Mutex<Option<String>>,
    /// What `eject_installer_media` answers, when a test wants something
    /// other than "one drive came out".
    eject: Mutex<Option<EjectOutcome>>,
}

impl FakeBuilder {
    fn utm() -> Self {
        FakeBuilder {
            backend: "utm".to_string(),
            installer: true,
            stable_image_id: false,
            live_images: Mutex::new(Vec::new()),
            transport: Arc::new(FakeTransport::new()),
            guest: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            fail_on: Mutex::new(None),
            created: Mutex::new(Vec::new()),
            published: Mutex::new(Vec::new()),
            discarded: Mutex::new(Vec::new()),
            cleanup_failures: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
            builds: Mutex::new(None),
            last_profile: Mutex::new(String::new()),
            image_at_discard: Mutex::new(None),
            eject: Mutex::new(None),
        }
    }

    fn set_eject(&self, outcome: EjectOutcome) {
        *self.eject.lock().unwrap() = Some(outcome);
    }

    fn set_transport(&self, transport: Arc<dyn GuestTransport>) {
        *self.guest.lock().unwrap() = Some(transport);
    }

    fn watch(&self, builds: Arc<MemoryBuildRepo>) {
        *self.builds.lock().unwrap() = Some(builds);
    }

    fn tart() -> Self {
        FakeBuilder {
            backend: "tart".to_string(),
            installer: false,
            stable_image_id: true,
            ..FakeBuilder::utm()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Whether the backend still has this image.
    fn has_image(&self, id: &str) -> bool {
        self.live_images.lock().unwrap().iter().any(|i| i == id)
    }

    fn record(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_string());
    }

    fn set_fail_on(&self, step: &'static str) {
        *self.fail_on.lock().unwrap() = Some(step);
    }

    /// Makes cleanup after a failed build report a leftover.
    fn set_cleanup_failure(&self, detail: &str) {
        self.cleanup_failures
            .lock()
            .unwrap()
            .push(detail.to_string());
    }

    fn should_fail(&self, step: &str) -> Result<()> {
        if self.fail_on.lock().unwrap().map(|s| s == step) == Some(true) {
            return Err(WargameError::VmCreationFailed(format!(
                "fake {step} failure"
            )));
        }
        Ok(())
    }

    fn new_id(&self, prefix: &str) -> String {
        let mut n = self.next_id.lock().unwrap();
        *n += 1;
        format!("{prefix}-{n}")
    }
}

#[async_trait]
impl ProfileImageBuilder for FakeBuilder {
    fn backend(&self) -> &str {
        &self.backend
    }

    fn uses_installer_vm(&self) -> bool {
        self.installer
    }

    async fn create_installer_vm(&self, req: &InstallerRequest<'_>) -> Result<VmRef> {
        self.record("create_installer_vm");
        self.should_fail("create_installer_vm")?;
        self.created.lock().unwrap().push(req.name.clone());
        Ok(VmRef {
            backend: self.backend.clone(),
            id: self.new_id("INSTALLER"),
            name: req.name.clone(),
        })
    }

    async fn open_console(&self, vm: &VmRef) -> Result<()> {
        self.record(&format!("open_console:{}", vm.id));
        self.should_fail("open_console")
    }

    async fn prepare(&self, _req: &BuildRequest<'_>, _progress: Progress<'_>) -> Result<()> {
        self.record("prepare");
        self.should_fail("prepare")
    }

    async fn clone_for_build(
        &self,
        req: &BuildRequest<'_>,
        _progress: Progress<'_>,
    ) -> Result<VmRef> {
        self.record("clone_for_build");
        self.should_fail("clone_for_build")?;
        if self.installer {
            return req.installer_vm().cloned().ok_or_else(|| {
                WargameError::VmNotFound("no installation VM in the record".to_string())
            });
        }
        let name = req.profile.image_name();
        self.created.lock().unwrap().push(name.clone());
        Ok(VmRef::new(self.backend.clone(), name))
    }

    async fn eject_installer_media(
        &self,
        vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<EjectOutcome> {
        if !self.installer {
            return Ok(EjectOutcome::NotApplicable);
        }
        self.record(&format!("eject_installer_media:{}", vm.id));
        self.should_fail("eject_installer_media")?;
        Ok(self
            .eject
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(EjectOutcome::Detached(1)))
    }

    async fn boot(
        &self,
        _req: &BuildRequest<'_>,
        vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<()> {
        self.record(&format!("boot:{}", vm.id));
        self.should_fail("boot")
    }

    async fn connect(
        &self,
        req: &BuildRequest<'_>,
        _vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<GuestLink> {
        self.record("connect");
        self.should_fail("connect")?;
        Ok(GuestLink {
            // The guest's OS decides the endpoint's shell, exactly as the
            // real UTM/Tart builders do.
            endpoint: GuestEndpoint {
                shell: GuestShell::for_os(req.profile.guest.os),
                ..GuestEndpoint::new("10.0.0.9".parse().unwrap(), req.profile.guest.user.clone())
            },
            transport: self
                .guest
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| self.transport.clone() as Arc<dyn GuestTransport>),
        })
    }

    async fn shutdown(
        &self,
        _req: &BuildRequest<'_>,
        vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<()> {
        self.record(&format!("shutdown:{}", vm.id));
        self.should_fail("shutdown")
    }

    async fn publish(
        &self,
        req: &BuildRequest<'_>,
        _vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<PublishedImage> {
        self.record("publish");
        *self.last_profile.lock().unwrap() = req.profile.name.clone();
        self.should_fail("publish")?;
        let (name, id) = if self.stable_image_id {
            // Tart: the built VM is the image, so the same name comes back
            // from every build.
            let name = req.profile.image_name();
            (name.clone(), name)
        } else {
            let name = format!("wg-build-image-{}", req.profile.name);
            (name, self.new_id("IMAGE"))
        };
        self.published.lock().unwrap().push(name.clone());
        self.live_images.lock().unwrap().push(id.clone());
        Ok(PublishedImage::new(id, name))
    }

    async fn discard_image(&self, image: &PublishedImage) -> Result<()> {
        self.record(&format!("discard_image:{}", image.id));
        self.discarded.lock().unwrap().push(image.id.clone());
        self.live_images
            .lock()
            .unwrap()
            .retain(|id| id != &image.id);
        let watched = self.builds.lock().unwrap().clone();
        if let Some(builds) = watched {
            let profile = self.last_profile.lock().unwrap().clone();
            let current = builds.load(&profile).await?;
            *self.image_at_discard.lock().unwrap() = current.and_then(|r| r.image.map(|i| i.id));
        }
        self.should_fail("discard_image")
    }

    async fn cleanup_failed_build(&self, _req: &BuildRequest<'_>, vm: &VmRef) -> Vec<String> {
        self.record(&format!("cleanup:{}", vm.id));
        self.cleanup_failures.lock().unwrap().clone()
    }
}

/// A guest whose provisioning script asks for a reboot: the script exits
/// 3010, SSH then refuses connections until it comes back.
struct RebootingTransport {
    /// Every `wait_ready` this transport was asked for.
    waits: Mutex<usize>,
    /// How many `true` probes are still answered with "connection refused"
    /// before the guest is back.
    down_for: Mutex<usize>,
    execs: Mutex<Vec<Vec<String>>>,
}

impl RebootingTransport {
    fn new(down_for: usize) -> Self {
        RebootingTransport {
            waits: Mutex::new(0),
            down_for: Mutex::new(down_for),
            execs: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl GuestTransport for RebootingTransport {
    async fn wait_ready(
        &self,
        _endpoint: &GuestEndpoint,
        _timeout: std::time::Duration,
    ) -> Result<()> {
        *self.waits.lock().unwrap() += 1;
        Ok(())
    }

    async fn exec(
        &self,
        _endpoint: &GuestEndpoint,
        argv: &[String],
    ) -> Result<wargame_core::ports::ExecResult> {
        self.execs.lock().unwrap().push(argv.to_vec());
        if argv == ["true".to_string()] {
            let mut down = self.down_for.lock().unwrap();
            if *down > 0 {
                *down -= 1;
                return Err(WargameError::NetworkUnavailable(
                    "ssh: connect to host 10.0.0.9 port 22: Connection refused".to_string(),
                ));
            }
        }
        let status = if argv.first().map(String::as_str) == Some("bash") {
            3010
        } else {
            0
        };
        Ok(wargame_core::ports::ExecResult {
            status,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    async fn exec_script(
        &self,
        _endpoint: &GuestEndpoint,
        script: &str,
    ) -> Result<wargame_core::ports::ExecResult> {
        self.execs.lock().unwrap().push(vec![script.to_string()]);
        if script.contains("wargame-ready") {
            let mut down = self.down_for.lock().unwrap();
            if *down > 0 {
                *down -= 1;
                return Err(WargameError::NetworkUnavailable(
                    "ssh: connect to host 10.0.0.9 port 22: Connection refused".to_string(),
                ));
            }
        }
        Ok(wargame_core::ports::ExecResult {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    async fn copy_in(&self, _endpoint: &GuestEndpoint, _src: &Path, _dest: &str) -> Result<()> {
        Ok(())
    }

    fn interactive_shell_cmd(&self, _endpoint: &GuestEndpoint) -> std::process::Command {
        std::process::Command::new("true")
    }
}

/// A disk with however much free space a test wants, per volume.
///
/// It records every path it was asked about, because *which* filesystem a
/// check measures is half the answer: a UTM disk never lands in
/// `$WARGAME_HOME` (phase A, D1).
struct FakeDisk {
    default: Mutex<u64>,
    /// Paths this disk cannot answer for, and the paths it was asked about.
    unreadable: Mutex<Vec<PathBuf>>,
    per_path: Mutex<Vec<(PathBuf, u64)>>,
    asked: Mutex<Vec<PathBuf>>,
}

impl FakeDisk {
    fn with_gib(gib: u64) -> Self {
        FakeDisk {
            default: Mutex::new(gib * 1024 * 1024 * 1024),
            unreadable: Mutex::new(Vec::new()),
            per_path: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn with_gib_at(self, path: impl Into<PathBuf>, gib: u64) -> Self {
        self.per_path
            .lock()
            .unwrap()
            .push((path.into(), gib * 1024 * 1024 * 1024));
        self
    }

    /// A volume whose free space cannot be read at all -- a sandboxed
    /// directory this process may not stat.
    fn unreadable_at(self, path: impl Into<PathBuf>) -> Self {
        self.unreadable.lock().unwrap().push(path.into());
        self
    }
}

impl DiskSpace for FakeDisk {
    fn free_bytes(&self, path: &Path) -> Result<u64> {
        self.asked.lock().unwrap().push(path.to_path_buf());
        if self.unreadable.lock().unwrap().iter().any(|p| p == path) {
            return Err(WargameError::Other(format!(
                "cannot stat {}: Operation not permitted",
                path.display()
            )));
        }
        if let Some((_, free)) = self
            .per_path
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| p == path)
        {
            return Ok(*free);
        }
        Ok(*self.default.lock().unwrap())
    }
}

// --- fixture --------------------------------------------------------------

const KALI_YAML: &str = r#"apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: kali-arm64
  description: "Kali template"

vm:
  backend: utm
  mode: virtualize

guest:
  os: linux
  distribution: kali
  arch: arm64
  user: kali

image:
  base: ""

resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 80

provision:
  scripts:
    - scripts/kali/common.sh

tools:
  verify:
    - python3 --version

workspace:
  root: /home/kali/wargame
"#;

const WINDOWS_YAML: &str = r#"apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: windows-x86_64

vm:
  backend: utm
  mode: emulate

guest:
  os: windows
  arch: x86_64
  user: wargame

image:
  base: ""

resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 80

provision:
  scripts:
    - scripts/windows/common.ps1

workspace:
  root: 'C:\wargame'
"#;

const MACOS_YAML: &str = r#"apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: macos-re-arm64

guest:
  os: macos
  arch: arm64
  user: admin

image:
  base: ghcr.io/cirruslabs/macos-sequoia-vanilla:latest

resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 60

provision:
  scripts:
    - scripts/macos/common.sh

tools:
  verify:
    - python3 --version

workspace:
  root: /Users/admin/wargame
"#;

struct Fixture {
    ctx: AppContext,
    utm: Arc<FakeBuilder>,
    tart: Arc<FakeBuilder>,
    builds: Arc<MemoryBuildRepo>,
    /// The UTM-named `VmProvider`, so a test can say where that backend
    /// stores its disks.
    utm_vms: Arc<FakeVmProvider>,
    disk: Arc<FakeDisk>,
    home: PathBuf,
    iso: PathBuf,
}

impl Fixture {
    fn profiles_dir(&self) -> PathBuf {
        self.home.join("profiles")
    }

    async fn record(&self, profile: &str) -> BuildRecord {
        self.builds
            .load(profile)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("expected a build record for {profile}"))
    }
}

fn fixture() -> Fixture {
    fixture_with_disk(FakeDisk::with_gib(500))
}

fn fixture_with_disk(disk: FakeDisk) -> Fixture {
    let disk = Arc::new(disk);
    let home = tempfile::tempdir().unwrap().keep();
    let dirs = AppDirs { root: home.clone() };
    dirs.ensure_layout().unwrap();
    let profiles_dir = dirs.profiles_dir();
    std::fs::write(profiles_dir.join("kali-arm64.yaml"), KALI_YAML).unwrap();
    std::fs::write(profiles_dir.join("windows-x86_64.yaml"), WINDOWS_YAML).unwrap();
    std::fs::write(profiles_dir.join("macos-re-arm64.yaml"), MACOS_YAML).unwrap();
    for (rel, body) in [
        ("scripts/kali/common.sh", "#!/usr/bin/env bash\necho kali\n"),
        ("scripts/windows/common.ps1", "Write-Host windows\n"),
        ("scripts/macos/common.sh", "#!/usr/bin/env bash\necho mac\n"),
    ] {
        let path = profiles_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    let iso = home.join("kali.iso");
    std::fs::write(&iso, b"not really an iso").unwrap();

    let utm = Arc::new(FakeBuilder::utm());
    let tart = Arc::new(FakeBuilder::tart());
    let guest_transport = Arc::new(FakeTransport::new()) as Arc<dyn GuestTransport>;
    let builds = Arc::new(MemoryBuildRepo::new());
    utm.watch(builds.clone());
    tart.watch(builds.clone());
    let utm_vms = Arc::new(FakeVmProvider::with_backend("utm"));
    let ctx = AppContext {
        config: Default::default(),
        dirs,
        wargame: Arc::new(FakeWargameProvider::new()),
        backends: BackendRegistry::new(vec![
            Arc::new(FakeVmProvider::new()) as Arc<dyn VmProvider>,
            utm_vms.clone() as Arc<dyn VmProvider>,
        ]),
        // One transport serves both shells: it phrases each command for the
        // endpoint it is given, exactly as `wiring.rs` registers it.
        transports: TransportRegistry::new(guest_transport.clone())
            .with_powershell(guest_transport),
        repo: Arc::new(MemoryRepo::new()),
        builds: builds.clone(),
        builders: BuilderRegistry::new(vec![
            utm.clone() as Arc<dyn ProfileImageBuilder>,
            tart.clone() as Arc<dyn ProfileImageBuilder>,
        ]),
        disk: disk.clone(),
        profiles: ProfileStore::new(Vec::new(), profiles_dir),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap: wargame_core::app::BootstrapScripts::new("#!/bin/bash\n", "# ps\n"),
    };
    Fixture {
        ctx,
        utm,
        tart,
        builds,
        utm_vms,
        disk,
        home,
        iso,
    }
}

fn iso_source(path: &Path) -> CreateSource {
    CreateSource::Iso {
        path: path.to_path_buf(),
        sha256: None,
    }
}

fn noop(_: String) {}

// --- profile init ---------------------------------------------------------

#[tokio::test]
async fn init_copies_the_template_with_its_own_scripts_directory() {
    let f = fixture();
    let outcome = profile_build::init(&f.ctx, "kali-arm64", "kali-custom")
        .await
        .unwrap();

    assert_eq!(outcome.profile.name, "kali-custom");
    assert_eq!(
        outcome.profile.provision.scripts,
        vec!["scripts/kali-custom/common.sh".to_string()],
        "the copy must own its scripts so re-seeding the built-ins cannot overwrite them"
    );
    assert!(outcome.yaml_path.is_file());
    assert!(f
        .profiles_dir()
        .join("scripts/kali-custom/common.sh")
        .is_file());
    // The template itself is untouched.
    assert_eq!(
        std::fs::read_to_string(f.profiles_dir().join("kali-arm64.yaml")).unwrap(),
        KALI_YAML
    );
    // And the new profile is visible to the store.
    assert_eq!(
        f.ctx.profiles.get("kali-custom").unwrap().guest.user,
        "kali"
    );
}

#[tokio::test]
async fn init_refuses_a_name_that_already_exists() {
    let f = fixture();
    profile_build::init(&f.ctx, "kali-arm64", "kali-custom")
        .await
        .unwrap();
    let err = profile_build::init(&f.ctx, "kali-arm64", "kali-custom")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    // The message carries the recovery step, so no misleading "check the
    // provisioning log" hint is attached to it.
    assert_eq!(err.hint(), None, "{err}");

    let err = profile_build::init(&f.ctx, "kali-arm64", "kali-arm64")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("template"), "{err}");
}

// --- profile create -------------------------------------------------------

#[tokio::test]
async fn create_makes_one_installer_vm_and_records_it_as_awaiting_install() {
    let f = fixture();
    let outcome = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();

    assert!(!outcome.reused);
    assert_eq!(outcome.record.phase, BuildPhase::AwaitingInstall);
    let vm = outcome.record.installer_vm.clone().unwrap();
    assert_eq!(
        vm.name, "wg-build-kali-arm64",
        "an installation VM must carry the build prefix, not the session one"
    );
    assert!(outcome.record.ready_image().is_none());

    let guidance = outcome.guidance.join("\n");
    assert!(guidance.contains(&vm.id), "the installer UUID is printed");
    assert!(guidance.contains("wargame profile console kali-arm64"));
    assert!(guidance.contains("qemu-guest-agent"));
    assert!(guidance.contains("wargame profile build kali-arm64"));
    // The baseline script needs `sudo -n` and refuses without it, so the
    // guidance has to ask for it *before* the first build -- with the
    // profile's own user substituted, ready to paste.
    assert!(
        guidance.contains("kali ALL=(ALL) NOPASSWD:ALL"),
        "{guidance}"
    );
    assert!(guidance.contains("/etc/sudoers.d/wargame"), "{guidance}");

    assert_eq!(f.record("kali-arm64").await.installer_vm, Some(vm));
}

/// spec.md §3 FR-2: "같은 명령을 다시 실행하면 설치 VM을 재사용·안내하고
/// 중복 VM/디스크를 만들지 않는다".
#[tokio::test]
async fn create_re_run_with_the_same_source_reuses_the_installer_vm() {
    let f = fixture();
    let first = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let again = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();

    assert!(again.reused);
    assert_eq!(again.record.installer_vm, first.record.installer_vm);
    assert_eq!(
        f.utm
            .calls()
            .iter()
            .filter(|c| *c == "create_installer_vm")
            .count(),
        1,
        "a re-run must not create a second VM: {:?}",
        f.utm.calls()
    );
    assert!(again.guidance.join("\n").contains("Re-using"));
}

#[tokio::test]
async fn create_with_a_different_source_is_a_conflict_that_names_both() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let other = f.home.join("other.iso");
    std::fs::write(&other, b"another").unwrap();

    let err = profile_build::create(&f.ctx, "kali-arm64", iso_source(&other))
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("kali.iso"), "{message}");
    assert!(message.contains("other.iso"), "{message}");
    assert!(message.contains("profile init"), "{message}");
    assert_eq!(
        f.utm
            .calls()
            .iter()
            .filter(|c| *c == "create_installer_vm")
            .count(),
        1
    );
}

#[tokio::test]
async fn create_verifies_a_given_sha256_before_any_vm_exists() {
    let f = fixture();
    let err = profile_build::create(
        &f.ctx,
        "kali-arm64",
        CreateSource::Iso {
            path: f.iso.clone(),
            sha256: Some("0".repeat(64)),
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(err, WargameError::HashOrWrite(_)), "{err:?}");
    assert!(
        f.utm.calls().is_empty(),
        "nothing may be created before the hash matches: {:?}",
        f.utm.calls()
    );
    assert!(f.builds.load("kali-arm64").await.unwrap().is_none());

    // The real hash is accepted and recorded as verified.
    let (digest, _) = wargame_core::security::hash::sha256_file(&f.iso).unwrap();
    let outcome = profile_build::create(
        &f.ctx,
        "kali-arm64",
        CreateSource::Iso {
            path: f.iso.clone(),
            sha256: Some(digest),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome.record.source,
        BuildSource::Iso {
            verified_sha256: true,
            ..
        }
    ));
}

#[tokio::test]
async fn create_refuses_a_missing_iso_before_any_vm_exists() {
    let f = fixture();
    let err = profile_build::create(&f.ctx, "kali-arm64", iso_source(Path::new("/no/such.iso")))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("/no/such.iso"), "{err}");
    assert!(f.utm.calls().is_empty());
}

/// Kali's `qemu-guest-agent` unit has no `[Install]` section, so
/// `systemctl is-enabled` answers `static`. A user who checks the wrong
/// command reads a correct guest as broken (phase A, D8).
#[tokio::test]
async fn the_guest_setup_steps_say_that_static_is_the_right_answer_on_kali() {
    let f = fixture();
    let outcome = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let guidance = outcome.guidance.join("\n");
    assert!(guidance.contains("static"), "{guidance}");
    assert!(guidance.contains("is-active"), "{guidance}");
}

/// Two installs at once on one host starved the emulated guest (phase A's
/// timings): the defaults are unchanged, and the user is told why they may
/// want to change them.
#[tokio::test]
async fn create_says_that_concurrent_builds_compete_for_cores() {
    let f = fixture();
    let outcome = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let guidance = outcome.guidance.join("\n");
    assert!(
        guidance.contains("4 vCPU"),
        "the profile's own ask: {guidance}"
    );
    assert!(
        guidance.contains("one at a time") || guidance.contains("at the same time"),
        "{guidance}"
    );
    // Nothing was quietly lowered.
    assert_eq!(f.ctx.profiles.get("kali-arm64").unwrap().resources.cpu, 4);
}

// --- the free-space precondition (phase A, D1) -----------------------------
//
// A UTM disk is a *sparse* qcow2: a fresh one occupies nothing at all and a
// finished minimal Kali install about 2.3 GiB of its nominal 20. Demanding
// the nominal size up front blocked both shipped Kali profiles on any host
// with less than 80 GiB free, for a VM that would have used single digits.

#[tokio::test]
async fn create_refuses_only_when_the_volume_is_below_the_working_floor() {
    let f = fixture_with_disk(FakeDisk::with_gib(4)); // the floor is 8 GiB
    let err = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WargameError::InsufficientResources { .. }),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(message.contains("4.0 GiB"), "what is free: {message}");
    assert!(
        message.contains("8"),
        "and the floor it is under: {message}"
    );

    let hint = err.hint().unwrap_or_default();
    assert!(
        !hint.contains("wargame sessions"),
        "a UTM profile has no session to delete for space: {hint}"
    );
    assert!(
        hint.contains("disk_gib"),
        "the profile's own request is the thing to lower: {hint}"
    );

    assert!(f.utm.calls().is_empty(), "{:?}", f.utm.calls());
    assert!(f.builds.load("kali-arm64").await.unwrap().is_none());
}

#[tokio::test]
async fn create_allows_less_free_space_than_the_nominal_disk_and_says_why() {
    // 20 GiB free against a profile that asks for 80: fine, because the
    // disk grows as it is used -- but the user has to be told.
    let f = fixture_with_disk(FakeDisk::with_gib(20));
    let outcome = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let guidance = outcome.guidance.join("\n");
    assert!(guidance.contains("20.0 GiB"), "{guidance}");
    assert!(guidance.contains("80 GiB"), "{guidance}");
    assert!(
        guidance.contains("grows") || guidance.contains("sparse"),
        "it has to explain why this is allowed: {guidance}"
    );
    assert_eq!(f.utm.created.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn create_with_room_for_the_whole_disk_warns_about_nothing() {
    let f = fixture_with_disk(FakeDisk::with_gib(500));
    let outcome = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let guidance = outcome.guidance.join("\n");
    assert!(!guidance.contains("GiB is free"), "{guidance}");
}

/// The volume that matters is the one the *backend* writes disks to. UTM is
/// sandboxed and keeps its bundles in its own container, so measuring
/// `$WARGAME_HOME` measures the wrong filesystem and names a directory that
/// will never hold the disk.
#[tokio::test]
async fn the_space_check_measures_the_volume_the_backend_stores_disks_on() {
    let container = PathBuf::from("/Users/x/Library/Containers/com.utmapp.UTM/Data/Documents");
    let f = fixture_with_disk(FakeDisk::with_gib(500).with_gib_at(&container, 2));
    f.utm_vms.set_storage_dir(&container);

    let err = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("com.utmapp.UTM"),
        "the message must name the directory that is actually short: {err}"
    );
    assert!(
        f.disk.asked.lock().unwrap().iter().any(|p| p == &container),
        "the container volume was never measured: {:?}",
        f.disk.asked.lock().unwrap()
    );
}

/// That directory is `drwx------` inside an app sandbox. If this process
/// cannot read its free space at all, fall back to the wargame home rather
/// than refusing to create anything.
#[tokio::test]
async fn a_storage_volume_that_cannot_be_read_falls_back_to_the_wargame_home() {
    let container = PathBuf::from("/Users/x/Library/Containers/com.utmapp.UTM/Data/Documents");
    let f = fixture_with_disk(FakeDisk::with_gib(500).unreadable_at(&container));
    f.utm_vms.set_storage_dir(&container);

    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .expect("an unreadable container must not block the create");
    let asked = f.disk.asked.lock().unwrap().clone();
    assert!(asked.iter().any(|p| p == &container));
    assert!(asked.iter().any(|p| p == &f.home));
}

#[tokio::test]
async fn create_on_a_tart_profile_explains_that_tart_uses_image_base() {
    let f = fixture();
    let err = profile_build::create(&f.ctx, "macos-re-arm64", iso_source(&f.iso))
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("image.base"), "{message}");
    assert!(message.contains("profile build"), "{message}");
    assert_eq!(
        err.hint(),
        None,
        "the message already names the next command"
    );
    assert!(f.tart.calls().is_empty());
}

// --- profile console ------------------------------------------------------

#[tokio::test]
async fn console_opens_the_installer_vm_and_refuses_tart() {
    let f = fixture();
    let err = profile_build::console(&f.ctx, "kali-arm64")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("installation VM"),
        "a profile with no record must be told to create one: {err}"
    );
    assert!(err.hint().unwrap().contains("profile create"));

    let outcome = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let opened = profile_build::console(&f.ctx, "kali-arm64").await.unwrap();
    let id = outcome.record.installer_vm.unwrap().id;
    assert!(f.utm.calls().contains(&format!("open_console:{id}")));

    // The console *starts* the VM, and it stays up afterwards -- the one
    // thing the old wording never said, which is how phase A ended up
    // stopping installation VMs with `utmctl` (D4).
    let guidance = opened.guidance.join("\n");
    assert!(
        guidance.contains("running") || guidance.contains("started"),
        "the console starts the VM and must say so: {guidance}"
    );
    assert!(
        guidance.contains("wargame profile stop kali-arm64"),
        "and how to stop it again: {guidance}"
    );
    assert!(
        guidance.contains("wargame profile build kali-arm64"),
        "and what comes next: {guidance}"
    );

    let err = profile_build::console(&f.ctx, "macos-re-arm64")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("installation VM"), "{err}");
}

/// Once the OS is installed the CD is what the machine boots, so the
/// console has to name the verb that takes it out.
#[tokio::test]
async fn the_console_of_a_vm_that_still_has_its_installer_media_says_how_to_eject_it() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let opened = profile_build::console(&f.ctx, "kali-arm64").await.unwrap();
    let guidance = opened.guidance.join("\n");
    assert!(
        guidance.contains("wargame profile eject kali-arm64"),
        "{guidance}"
    );
}

// --- profile build --------------------------------------------------------

#[tokio::test]
async fn build_without_a_record_names_profile_create() {
    let f = fixture();
    let err = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap_err();
    assert!(
        err.hint().unwrap().contains("wargame profile create"),
        "{err}"
    );
    assert!(f.utm.calls().is_empty());
}

#[tokio::test]
async fn a_successful_build_runs_every_step_in_order_and_publishes_a_ready_image() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let mut messages = Vec::new();
    let record = profile_build::build(&f.ctx, "kali-arm64", &mut |m| messages.push(m))
        .await
        .unwrap();

    let steps: Vec<String> = f
        .utm
        .calls()
        .into_iter()
        .filter(|c| c != "create_installer_vm")
        .map(|c| c.split(':').next().unwrap().to_string())
        .collect();
    assert_eq!(
        steps,
        vec![
            "prepare",
            "clone_for_build",
            // The installer CD comes out *before* the VM boots: UEFI boots
            // the CD first, so a build that skipped this would sit in the
            // installer until its boot timeout ran out, and the image it
            // published would still carry the CD (phase A, D3).
            "eject_installer_media",
            "boot",
            "connect",
            "shutdown",
            "publish",
        ],
        "spec.md §5 item 4's order"
    );

    assert_eq!(record.phase, BuildPhase::Ready);
    let image = record.ready_image().expect("a ready record has an image");
    assert_eq!(image.name, "wg-build-image-kali-arm64");
    assert_eq!(record.previous_image, None);
    assert_eq!(record.verification.len(), 1);
    assert_eq!(record.verification[0].command, "python3 --version");
    assert!(record.verification[0].passed());
    assert!(!record.profile_hash.is_empty());
    assert!(!record.scripts_hash.is_empty());
    assert!(record.last_error.is_none());
    assert!(record.cleanup_failures.is_empty());
    assert!(!messages.is_empty());

    // The scripts really were copied in and run in the guest.
    let guest_calls: Vec<String> = f
        .utm
        .transport
        .calls()
        .into_iter()
        .map(|c| format!("{c:?}"))
        .collect();
    assert!(
        guest_calls
            .iter()
            .any(|c| c.contains("wg-provision/scripts/kali/common.sh")),
        "{guest_calls:?}"
    );
    assert!(guest_calls.iter().any(|c| c.contains("CopyIn")));
}

/// The launchable image only exists once the build published it.
#[tokio::test]
async fn an_unbuilt_utm_profile_is_not_launchable_but_a_built_one_is() {
    let f = fixture();
    let profile = f.ctx.profiles.get("kali-arm64").unwrap();
    let err = profile_build::launch_image(&f.ctx, &profile)
        .await
        .unwrap_err();
    assert!(matches!(err, WargameError::ImageNotBuilt { .. }), "{err:?}");
    assert!(err.hint().unwrap().contains("profile create"));

    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    // Created but not built: still nothing to launch, and the hint moves on
    // to `profile build`.
    let err = profile_build::launch_image(&f.ctx, &profile)
        .await
        .unwrap_err();
    assert!(err.hint().unwrap().contains("profile build"), "{err}");

    let record = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let image = profile_build::launch_image(&f.ctx, &profile).await.unwrap();
    assert_eq!(
        image.0,
        record.ready_image().unwrap().id,
        "a UTM launch clones the published image VM by UUID"
    );
}

/// spec.md §3 FR-3: editing the profile or its scripts asks for a rebuild.
#[tokio::test]
async fn changing_a_provisioning_script_marks_the_image_as_needing_a_rebuild() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let profile = f.ctx.profiles.get("kali-arm64").unwrap();
    assert!(
        !profile_build::image_status(&f.ctx, &profile)
            .await
            .unwrap()
            .rebuild_needed
    );

    std::fs::write(
        f.profiles_dir().join("scripts/kali/common.sh"),
        "#!/usr/bin/env bash\necho more tools\n",
    )
    .unwrap();
    let status = profile_build::image_status(&f.ctx, &profile).await.unwrap();
    assert!(status.rebuild_needed);
    assert!(
        status.detail.contains("rebuild needed"),
        "{}",
        status.detail
    );
    // ... and the old image is still launchable in the meantime.
    assert!(status.image.is_some());
}

/// spec.md §6's "OS 설치 미완료" row: an unreachable guest is a wait, not a
/// failure, and never a ready image.
#[tokio::test]
async fn a_guest_that_never_answers_stays_awaiting_install() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    f.utm.set_fail_on("connect");

    let mut messages = Vec::new();
    let err = profile_build::build(&f.ctx, "kali-arm64", &mut |m| messages.push(m))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("connect"), "{err}");

    let record = f.record("kali-arm64").await;
    assert_eq!(record.phase, BuildPhase::AwaitingInstall);
    assert!(record.ready_image().is_none());
    assert!(
        record.installer_vm.is_some(),
        "the installer VM is preserved"
    );
    let printed = messages.join("\n");
    assert!(
        printed.contains("wargame profile console kali-arm64"),
        "{printed}"
    );
}

/// AC-09: a failed verification preserves the previous image, records the
/// failing command, and never marks the profile ready.
#[tokio::test]
async fn a_failed_verification_preserves_the_previous_image() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let first = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let good_image = first.ready_image().unwrap().clone();

    f.utm.transport.set_exec_responder(|argv| {
        if argv == ["python3".to_string(), "--version".to_string()] {
            wargame_core::ports::ExecResult {
                status: 127,
                stdout: String::new(),
                stderr: "bash: python3: command not found".to_string(),
            }
        } else {
            wargame_core::ports::ExecResult {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            }
        }
    });

    let err = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("python3 --version"), "{message}");
    assert!(message.contains("127"), "{message}");

    let record = f.record("kali-arm64").await;
    assert_eq!(
        record.phase,
        BuildPhase::Failed {
            stage: BuildStage::Verifying,
            error: message,
        }
    );
    assert_eq!(
        record.ready_image(),
        Some(&good_image),
        "the last good image must survive a failed rebuild"
    );
    assert_eq!(record.verification.last().unwrap().exit_code, 127);
    assert!(
        f.utm.discarded.lock().unwrap().is_empty(),
        "nothing may be deleted when the build failed"
    );
    assert_eq!(
        f.utm.published.lock().unwrap().len(),
        1,
        "a failed build must not publish"
    );
}

/// AC-09 / docs/utm-compatibility.md T12: a VM that will not stop fails the
/// build in the publishing stage, records the cleanup failure and leaves the
/// previous image alone.
#[tokio::test]
async fn a_stop_that_fails_records_a_cleanup_failure_and_keeps_the_old_image() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let first = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let good_image = first.ready_image().unwrap().clone();

    f.utm.set_fail_on("shutdown");
    f.utm
        .set_cleanup_failure("qemu-aarch64-softmmu (pid 4242) still holds the VM's files");
    let err = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("shutdown"), "{err}");

    let record = f.record("kali-arm64").await;
    assert!(matches!(
        record.phase,
        BuildPhase::Failed {
            stage: BuildStage::Publishing,
            ..
        }
    ));
    assert_eq!(record.ready_image(), Some(&good_image));
    assert_eq!(record.cleanup_failures.len(), 1);
    assert!(record.cleanup_failures[0].contains("pid 4242"));
    assert!(f.utm.discarded.lock().unwrap().is_empty());
}

/// A build that was interrupted mid-phase re-runs against the installation
/// VM the record already owns instead of creating a second one.
#[tokio::test]
async fn an_interrupted_build_resumes_without_duplicating_vms() {
    let f = fixture();
    let created = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    // Simulate a process that died between `connect` and `publish`.
    let mut interrupted = created.record.clone();
    interrupted.set_phase(BuildPhase::Provisioning);
    f.builds.insert(interrupted);

    let record = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    assert_eq!(record.phase, BuildPhase::Ready);
    assert_eq!(
        record.installer_vm, created.record.installer_vm,
        "the same installation VM must be reused"
    );
    assert_eq!(
        f.utm
            .calls()
            .iter()
            .filter(|c| *c == "create_installer_vm")
            .count(),
        1
    );
}

/// The previously published image is deleted only after the new one is
/// written down, and a failure to delete it is recorded rather than lost.
#[tokio::test]
async fn a_rebuild_replaces_the_image_and_only_then_deletes_the_old_one() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let first = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let old = first.ready_image().unwrap().clone();

    let second = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let new = second.ready_image().unwrap().clone();
    assert_ne!(new.id, old.id);
    assert_eq!(second.previous_image, None, "the old image was discarded");
    assert_eq!(
        f.utm.discarded.lock().unwrap().clone(),
        vec![old.id.clone()]
    );

    let calls = f.utm.calls();
    let publish_idx = calls.iter().position(|c| c == "publish").unwrap();
    let discard_idx = calls
        .iter()
        .position(|c| c.starts_with("discard_image"))
        .unwrap();
    assert!(
        publish_idx < discard_idx,
        "the replacement must exist before the old image is deleted"
    );
    assert_eq!(
        f.utm.image_at_discard.lock().unwrap().clone(),
        Some(new.id),
        "the new image must already be written down when the old one is deleted"
    );
}

#[tokio::test]
async fn a_failed_discard_of_the_old_image_is_recorded_not_swallowed() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    f.utm.set_fail_on("discard_image");

    let record = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    assert_eq!(
        record.phase,
        BuildPhase::Ready,
        "the new image is still good"
    );
    assert_eq!(
        record.leaked_images.len(),
        1,
        "the old image still exists, so it stays tracked"
    );
    assert_eq!(record.previous_image, None);
    assert_eq!(record.cleanup_failures.len(), 1);
    assert!(record.cleanup_failures[0].contains("could not delete"));
}

/// A Windows profile now builds like any other, with every guest-side
/// command phrased for PowerShell (spec.md §3 FR-3/FR-4).
#[tokio::test]
async fn a_windows_build_provisions_through_powershell() {
    let f = fixture();
    profile_build::create(&f.ctx, "windows-x86_64", iso_source(&f.iso))
        .await
        .unwrap();

    let record = profile_build::build(&f.ctx, "windows-x86_64", &mut noop)
        .await
        .expect("a Windows profile provisions");
    assert_eq!(record.phase, BuildPhase::Ready);

    let mut argvs: Vec<Vec<String>> = Vec::new();
    let mut scripts: Vec<String> = Vec::new();
    for call in f.utm.transport.calls() {
        match call {
            TransportCall::Exec { endpoint, argv } => {
                assert_eq!(
                    endpoint.shell,
                    GuestShell::PowerShell,
                    "a Windows guest is reached as a PowerShell endpoint"
                );
                argvs.push(argv);
            }
            TransportCall::ExecScript { endpoint, script } => {
                assert_eq!(endpoint.shell, GuestShell::PowerShell);
                scripts.push(script);
            }
            TransportCall::WaitReady { .. } | TransportCall::CopyIn { .. } => {}
        }
    }

    let flat: Vec<String> = argvs.iter().flatten().cloned().collect();
    // Cmdlets arrive as trusted scripts, with their parameter names bare and
    // only their data quoted -- PowerShell binds a parameter from nothing
    // else.
    assert!(
        scripts
            .iter()
            .any(|s| s == "New-Item -ItemType Directory -Force -Path 'wg-provision' | Out-Null"),
        "the provisioning directory is made with New-Item, not mkdir: {scripts:?}"
    );
    assert!(
        scripts
            .iter()
            .any(|s| s.starts_with("Remove-Item -Recurse -Force")),
        "the cleanup uses Remove-Item, not rm: {scripts:?}"
    );
    assert!(
        scripts.iter().any(|s| s.starts_with("Write-VolumeCache")),
        "the flush uses Write-VolumeCache, not sync: {scripts:?}"
    );
    for script in &scripts {
        assert!(
            !script.contains("'-"),
            "a quoted parameter name would bind positionally: {script:?}"
        );
    }
    assert!(
        !flat
            .iter()
            .any(|a| a == "mkdir" || a == "bash" || a == "sync"),
        "no POSIX command may reach a Windows guest: {argvs:?}"
    );
    assert!(
        argvs
            .iter()
            .any(|argv| argv[0] == "powershell" && argv.iter().any(|a| a.ends_with("common.ps1"))),
        "the profile's .ps1 must be run through PowerShell: {argvs:?}"
    );
    assert!(
        argvs
            .iter()
            .any(|argv| argv.iter().any(|a| a.contains("wg-provision\\scripts"))),
        "guest paths use the Windows separator: {argvs:?}"
    );
}

/// A build whose guest shell has no wired transport is refused before the
/// VM is touched at all, rather than half-provisioned.
#[tokio::test]
async fn a_build_without_a_transport_for_the_guest_shell_is_refused_early() {
    let mut f = fixture();
    f.ctx.transports = TransportRegistry::new(Arc::new(FakeTransport::new()));
    profile_build::create(&f.ctx, "windows-x86_64", iso_source(&f.iso))
        .await
        .unwrap();

    let err = profile_build::build(&f.ctx, "windows-x86_64", &mut noop)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("powershell"), "{err}");

    let record = f.record("windows-x86_64").await;
    assert_eq!(record.phase, BuildPhase::AwaitingInstall);
    assert!(record.ready_image().is_none());
    assert!(
        !f.utm.calls().iter().any(|c| c == "connect"),
        "the guest must not be touched: {:?}",
        f.utm.calls()
    );
}

#[tokio::test]
async fn a_windows_create_prints_the_manual_tpm_and_secure_boot_step() {
    let f = fixture();
    let outcome = profile_build::create(&f.ctx, "windows-x86_64", iso_source(&f.iso))
        .await
        .unwrap();
    let guidance = outcome.guidance.join("\n");
    assert!(guidance.contains("TPM 2.0"), "{guidance}");
    assert!(guidance.contains("Secure Boot"), "{guidance}");
    assert!(guidance.contains("OpenSSH Server"), "{guidance}");
    // The baseline script installs a Windows capability and writes the
    // machine PATH, both of which need an elevated token, and refuses
    // without one -- so the account has to be made an administrator during
    // the install, not after the first failed build.
    assert!(guidance.contains("Administrators"), "{guidance}");
    // Nothing about sudo on a Windows guest.
    assert!(!guidance.contains("sudoers"), "{guidance}");
}

/// The VMs a build record owns have no session, but they are not orphans:
/// `prune` must leave them alone (spec.md §3 FR-5).
#[tokio::test]
async fn prune_never_deletes_a_vm_a_build_record_owns() {
    let f = fixture();
    let vm_utm = f.ctx.backends.get("utm").unwrap();
    // Two VMs exist on the backend: one this app's build record owns, one
    // nobody claims.
    let record = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap()
        .record;
    let installer = record.installer_vm.clone().unwrap();
    vm_utm
        .create(&wargame_core::ports::VmSpec::new(
            installer.id.clone(),
            wargame_core::ports::ImageRef("src".to_string()),
            f.ctx.profiles.get("kali-arm64").unwrap().resources,
        ))
        .await
        .unwrap();
    vm_utm
        .create(&wargame_core::ports::VmSpec::new(
            "wg-orphan",
            wargame_core::ports::ImageRef("src".to_string()),
            f.ctx.profiles.get("kali-arm64").unwrap().resources,
        ))
        .await
        .unwrap();

    let report = wargame_core::app::prune(&f.ctx, true).await.unwrap();
    // The installer VM is owned by a record, so it is never even a
    // candidate; the VM nobody here claims is reported, not deleted.
    assert!(report.orphans.is_empty(), "{report:?}");
    assert_eq!(report.unmanaged_names(), vec!["utm/wg-orphan"]);
}

// --- the Tart path stays what it was --------------------------------------

#[tokio::test]
async fn a_tart_build_needs_no_record_and_writes_one() {
    let f = fixture();
    let record = profile_build::build(&f.ctx, "macos-re-arm64", &mut noop)
        .await
        .unwrap();
    assert_eq!(record.backend, "tart");
    assert!(matches!(record.source, BuildSource::TartBase { .. }));
    assert_eq!(record.phase, BuildPhase::Ready);
    assert!(record.installer_vm.is_none(), "Tart has no installation VM");
    let steps: Vec<String> = f
        .tart
        .calls()
        .into_iter()
        .map(|c| c.split(':').next().unwrap().to_string())
        .collect();
    assert_eq!(
        steps,
        vec![
            "prepare",
            "clone_for_build",
            "boot",
            "connect",
            "shutdown",
            "publish"
        ]
    );

    // Tart still launches from its own image name, not from the record.
    let profile = f.ctx.profiles.get("macos-re-arm64").unwrap();
    assert_eq!(
        profile_build::launch_image(&f.ctx, &profile)
            .await
            .unwrap()
            .0,
        "wargame-profile-macos-re-arm64"
    );
}

/// spec.md §3 FR-3: "설치 중 재부팅 필요 상태를 처리하고 다시 연결한다".
/// A script that exits 3010 is not a failure: the build waits for SSH to
/// drop and come back, then carries on.
#[tokio::test]
async fn a_script_that_asks_for_a_reboot_waits_for_the_guest_and_continues() {
    let f = fixture();
    let guest = Arc::new(RebootingTransport::new(1));
    f.utm
        .set_transport(guest.clone() as Arc<dyn GuestTransport>);
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();

    let mut messages = Vec::new();
    let record = profile_build::build(&f.ctx, "kali-arm64", &mut |m| messages.push(m))
        .await
        .unwrap();

    assert_eq!(record.phase, BuildPhase::Ready);
    assert_eq!(
        *guest.waits.lock().unwrap(),
        1,
        "the build must wait for SSH again after the reboot"
    );
    let printed = messages.join("\n");
    assert!(printed.contains("reboot"), "{printed}");
    // The verify command still ran, i.e. the flow continued past the reboot.
    assert_eq!(record.verification.len(), 1);
    let execs = guest.execs.lock().unwrap().clone();
    assert!(
        execs
            .iter()
            .any(|argv| argv == &["python3".to_string(), "--version".to_string()]),
        "{execs:?}"
    );
}

/// The Windows half of the same requirement, end to end through the *real*
/// transport: the guest's provisioning script exits 3010, `ssh` can only
/// exit with the low byte of that (194), and the build must still see 3010,
/// wait for the guest and carry on.
#[tokio::test]
async fn a_windows_script_that_asks_for_a_reboot_survives_sshs_8_bit_exit_status() {
    use wargame_core::infra::{powershell, SshGuestTransport};

    let f = fixture();
    let runner = Arc::new(FakeRunner::new());
    let rebooted = Arc::new(Mutex::new(false));
    let probes = Arc::new(Mutex::new(0usize));
    let seen = probes.clone();
    let done = rebooted.clone();
    runner.set_responder(move |cmd| {
        // The remote command is always the last argv element: base64 of the
        // wrapper script the transport built.
        let script = cmd
            .args
            .last()
            .and_then(|last| powershell::decode_command(last))
            .unwrap_or_default();

        if script.contains("common.ps1") {
            // The installer asks for a reboot. powershell.exe exits 3010,
            // which `ssh` truncates to 194 -- so the wrapper reports the
            // true code on its marker line and exits 255 instead.
            return Ok(wargame_core::infra::Output {
                status: 255,
                stdout: String::new(),
                stderr: format!(
                    "a component needs a restart\n{}3010\n",
                    powershell::EXIT_CODE_MARKER
                ),
            });
        }
        if script.contains("wargame-ready") {
            // The "is the guest still up?" probe: down once, then back.
            let mut count = seen.lock().unwrap();
            *count += 1;
            if !*done.lock().unwrap() {
                *done.lock().unwrap() = true;
                return Ok(wargame_core::infra::Output {
                    status: 255,
                    stdout: String::new(),
                    stderr: "ssh: connect to host 10.0.0.9 port 22: Connection refused".to_string(),
                });
            }
        }
        Ok(wargame_core::infra::Output {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    let transport = Arc::new(SshGuestTransport::new(
        runner.clone(),
        PathBuf::from("/keys/id_ed25519"),
    ));
    f.utm.set_transport(transport as Arc<dyn GuestTransport>);
    profile_build::create(&f.ctx, "windows-x86_64", iso_source(&f.iso))
        .await
        .unwrap();

    let mut messages = Vec::new();
    let record = profile_build::build(&f.ctx, "windows-x86_64", &mut |m| messages.push(m))
        .await
        .expect("a reboot-required script is not a build failure");

    assert_eq!(record.phase, BuildPhase::Ready);
    let printed = messages.join("\n");
    assert!(
        printed.contains("reboot"),
        "the build must report the reboot it waited for: {printed}"
    );
    assert!(
        *probes.lock().unwrap() > 0,
        "the build must probe the guest while it is down"
    );
    // Nothing downstream ever saw the marker line.
    assert!(!printed.contains(powershell::EXIT_CODE_MARKER), "{printed}");
}

/// A backend whose image id is stable across builds -- Tart's image *is*
/// `wargame-profile-<name>` -- republishes the same id, so there is no old
/// image to delete. Deleting it would delete the one this build just made,
/// leaving a record that says `ready` and a `launch` that fails with
/// `ImageMissing` (spec.md §3 FR-3's "사용자 스크립트를 추가하고 build를
/// 재실행하면 …", AC-01's existing macOS profiles).
#[tokio::test]
async fn rebuilding_an_image_whose_id_never_changes_keeps_it() {
    let f = fixture();
    let first = profile_build::build(&f.ctx, "macos-re-arm64", &mut noop)
        .await
        .unwrap();
    let image = first
        .ready_image()
        .expect("the first build publishes")
        .clone();
    assert_eq!(image.id, "wargame-profile-macos-re-arm64");

    let second = profile_build::build(&f.ctx, "macos-re-arm64", &mut noop)
        .await
        .unwrap();

    assert_eq!(second.phase, BuildPhase::Ready);
    assert_eq!(
        second.ready_image().map(|i| i.id.clone()),
        Some(image.id.clone())
    );
    assert_eq!(
        second.previous_image, None,
        "the same image was republished; there is no earlier one to delete"
    );
    assert!(
        f.tart.discarded.lock().unwrap().is_empty(),
        "the republished image must never be deleted: {:?}",
        f.tart.calls()
    );
    assert!(
        f.tart.has_image(&image.id),
        "the image the record points at must still exist on the backend"
    );
    assert!(second.leaked_images.is_empty());

    // And it is still what a launch would clone.
    let profile = f.ctx.profiles.get("macos-re-arm64").unwrap();
    assert_eq!(
        profile_build::launch_image(&f.ctx, &profile)
            .await
            .unwrap()
            .0,
        image.id
    );
}

/// An image that could not be deleted still exists, so the record keeps
/// pointing at it: `prune` must not treat it as an orphan, and the next
/// build retries the deletion.
#[tokio::test]
async fn an_image_that_could_not_be_deleted_stays_tracked_and_is_retried() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let first = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let old = first.ready_image().unwrap().clone();

    f.utm.set_fail_on("discard_image");
    let second = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    assert_eq!(
        second
            .leaked_images
            .iter()
            .map(|i| i.id.clone())
            .collect::<Vec<_>>(),
        vec![old.id.clone()],
        "a failed deletion must not lose the reference"
    );
    assert!(second
        .owned_vm_ids()
        .contains(&("utm".to_string(), old.id.clone())));
    // Whatever the bookkeeping, the currently published image is never a
    // deletion candidate.
    let current = second.ready_image().unwrap().id.clone();
    assert!(f.utm.has_image(&current));

    // The next build retries it, and this time it works.
    *f.utm.fail_on.lock().unwrap() = None;
    let third = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    assert!(
        third.leaked_images.is_empty(),
        "the retry cleared the leak: {:?}",
        third.leaked_images
    );
    assert!(!f.utm.has_image(&old.id));
    assert!(f.utm.has_image(&third.ready_image().unwrap().id));
}

/// A build killed halfway must leave the phase it reached, not the phase of
/// the run before it.
#[tokio::test]
async fn each_stage_is_written_down_as_the_build_reaches_it() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();

    // A rebuild that dies while verifying leaves `verifying` behind, not the
    // `ready` of the previous run.
    f.utm.transport.set_exec_responder(|argv| {
        if argv == ["python3".to_string(), "--version".to_string()] {
            wargame_core::ports::ExecResult {
                status: 1,
                stdout: String::new(),
                stderr: "boom".to_string(),
            }
        } else {
            wargame_core::ports::ExecResult {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            }
        }
    });
    let _ = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap_err();
    let record = f.record("kali-arm64").await;
    assert!(matches!(
        record.phase,
        BuildPhase::Failed {
            stage: BuildStage::Verifying,
            ..
        }
    ));

    // The phases really were persisted one at a time, not only at the end.
    assert!(
        f.builds.saves() >= 6,
        "expected a save per stage transition, got {}",
        f.builds.saves()
    );
}

/// A run that died between "the new image is recorded" and "the old one is
/// deleted" leaves `previous_image` naming an image that still exists. The
/// next build must not overwrite that reference with its own replacement:
/// the image would stop being deleted, and `prune` would stop protecting it.
#[tokio::test]
async fn a_stale_previous_image_from_a_killed_run_is_not_lost() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    let first = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    let current = first.ready_image().unwrap().clone();

    // As if the process had been killed just after the `Ready` save of an
    // earlier build: an image it replaced is still named and still exists.
    let stale = PublishedImage::new("STALE-IMAGE", "wg-build-image-kali-arm64");
    let mut interrupted = f.record("kali-arm64").await;
    interrupted.previous_image = Some(stale.clone());
    f.builds.insert(interrupted);

    let second = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();

    let discarded = f.utm.discarded.lock().unwrap().clone();
    assert!(
        discarded.contains(&stale.id),
        "the stale image must still be deleted: {discarded:?}"
    );
    assert!(
        discarded.contains(&current.id),
        "so must the one this build replaced: {discarded:?}"
    );
    assert!(
        second.leaked_images.is_empty(),
        "{:?}",
        second.leaked_images
    );
    assert_eq!(second.previous_image, None);
    assert!(f.utm.has_image(&second.ready_image().unwrap().id));
}

// --- the installer CD (phase A, D3) ---------------------------------------

/// `profile create` attaches an installer ISO, so the record says the media
/// is on; a build that ejects it says so too. That is what tells a later
/// `profile console` -- and the next reader of the record -- whether the VM
/// still boots the installer.
#[tokio::test]
async fn the_record_tracks_whether_the_installer_media_is_still_attached() {
    let f = fixture();
    let created = profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    assert_eq!(created.record.installer_media, InstallerMedia::Attached);

    let record = profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap();
    assert_eq!(record.installer_media, InstallerMedia::Detached);
    assert_eq!(
        f.record("kali-arm64").await.installer_media,
        InstallerMedia::Detached,
        "and it is persisted, not only returned"
    );
}

/// The eject happens before the boot, so even a build that then fails has
/// left the VM bootable from its own disk -- and the record says so, or the
/// next run would have no idea.
#[tokio::test]
async fn a_build_that_fails_after_the_eject_still_records_it() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    f.utm.set_fail_on("connect");
    profile_build::build(&f.ctx, "kali-arm64", &mut noop)
        .await
        .unwrap_err();

    let record = f.record("kali-arm64").await;
    assert_eq!(record.phase, BuildPhase::AwaitingInstall);
    assert_eq!(record.installer_media, InstallerMedia::Detached);
}

/// An installation VM that is still running cannot have its configuration
/// written (UTM refuses that), so the eject is skipped rather than failing
/// the build -- and the record must not claim the CD is gone.
#[tokio::test]
async fn a_blocked_eject_does_not_stop_the_build_and_does_not_claim_the_media_is_gone() {
    let f = fixture();
    profile_build::create(&f.ctx, "kali-arm64", iso_source(&f.iso))
        .await
        .unwrap();
    f.utm
        .set_eject(EjectOutcome::Blocked("the VM is started".to_string()));
    let mut messages = Vec::new();
    let record = profile_build::build(&f.ctx, "kali-arm64", &mut |m| messages.push(m))
        .await
        .unwrap();
    assert_eq!(record.installer_media, InstallerMedia::Attached);
    assert!(
        messages.iter().any(|m| m.contains("started")),
        "the user has to be told why it was skipped: {messages:?}"
    );
}

/// A Tart profile has no installation media at all: nothing is ejected and
/// nothing is claimed about it.
#[tokio::test]
async fn a_tart_build_ejects_nothing() {
    let f = fixture();
    profile_build::build(&f.ctx, "macos-re-arm64", &mut noop)
        .await
        .unwrap();
    assert!(
        !f.tart.calls().iter().any(|c| c.starts_with("eject")),
        "{:?}",
        f.tart.calls()
    );
    assert_eq!(
        f.record("macos-re-arm64").await.installer_media,
        InstallerMedia::Unknown
    );
}
