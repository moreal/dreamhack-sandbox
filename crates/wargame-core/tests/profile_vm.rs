//! `profile eject | stop | delete` over fakes: the lifecycle verbs for the
//! VMs a build record owns (spec.md §3 FR-5, §4; task-7-phase-a-report.md
//! §5 D3/D4).
//!
//! Nothing here touches UTM or Tart. What is being checked is the ownership
//! rule -- every VM is reached through the build record, never by name --
//! and the refusals: a VM that is still running is not deleted, a VM the
//! record does not own is not touched, and a record whose VM could not be
//! removed is kept so the user can try again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use wargame_core::app::profile_vm;
use wargame_core::app::{
    AppContext, BackendRegistry, BuilderRegistry, ProfileStore, TransportRegistry,
};
use wargame_core::domain::build::{
    BuildPhase, BuildRecord, BuildSource, InstallerMedia, PublishedImage,
};
use wargame_core::domain::VmRef;
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::fs::AppDirs;
use wargame_core::ports::image_builder::{
    BuildRequest, EjectOutcome, GuestLink, ImageRegistrar, InstallerRequest, Progress,
};
use wargame_core::ports::{
    BuildRecordRepository, DiskSpace, GuestTransport, ProfileImageBuilder, VmProvider, VmStatus,
};
use wargame_core::testing::{
    FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryBuildRepo, MemoryRepo,
};

const INSTALLER_VM: &str = "wg-build-kali-arm64";
const IMAGE_VM: &str = "wg-build-image-kali-arm64";
/// A VM no record here owns -- the user's own, or another home's.
const SOMEONE_ELSES_VM: &str = "kindle-vm";

// --- fakes ----------------------------------------------------------------

/// A build backend that only has to answer the eject: these verbs never run
/// a build, so every other step refuses rather than pretending.
struct StubBuilder {
    backend: String,
    installer: bool,
    eject: Mutex<Result<EjectOutcome>>,
    ejected: Mutex<Vec<String>>,
}

impl StubBuilder {
    fn utm() -> Self {
        StubBuilder {
            backend: "utm".to_string(),
            installer: true,
            eject: Mutex::new(Ok(EjectOutcome::Detached(1))),
            ejected: Mutex::new(Vec::new()),
        }
    }

    fn tart() -> Self {
        StubBuilder {
            backend: "tart".to_string(),
            installer: false,
            eject: Mutex::new(Ok(EjectOutcome::NotApplicable)),
            ejected: Mutex::new(Vec::new()),
        }
    }

    fn set_eject(&self, outcome: Result<EjectOutcome>) {
        *self.eject.lock().unwrap() = outcome;
    }

    fn ejected(&self) -> Vec<String> {
        self.ejected.lock().unwrap().clone()
    }
}

fn unused(what: &str) -> WargameError {
    WargameError::Other(format!("the stub builder was asked to {what}"))
}

#[async_trait]
impl ProfileImageBuilder for StubBuilder {
    fn backend(&self) -> &str {
        &self.backend
    }

    fn uses_installer_vm(&self) -> bool {
        self.installer
    }

    async fn eject_installer_media(
        &self,
        vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<EjectOutcome> {
        self.ejected.lock().unwrap().push(vm.id.clone());
        match &*self.eject.lock().unwrap() {
            Ok(outcome) => Ok(outcome.clone()),
            Err(e) => Err(WargameError::Other(e.to_string())),
        }
    }

    async fn prepare(&self, _req: &BuildRequest<'_>, _progress: Progress<'_>) -> Result<()> {
        Err(unused("prepare"))
    }

    async fn clone_for_build(
        &self,
        _req: &BuildRequest<'_>,
        _progress: Progress<'_>,
    ) -> Result<VmRef> {
        Err(unused("clone"))
    }

    async fn boot(
        &self,
        _req: &BuildRequest<'_>,
        _vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<()> {
        Err(unused("boot"))
    }

    async fn connect(
        &self,
        _req: &BuildRequest<'_>,
        _vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<GuestLink> {
        Err(unused("connect"))
    }

    async fn shutdown(
        &self,
        _req: &BuildRequest<'_>,
        _vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<()> {
        Err(unused("shut down"))
    }

    async fn publish(
        &self,
        _req: &BuildRequest<'_>,
        _vm: &VmRef,
        _progress: Progress<'_>,
        _registrar: &dyn ImageRegistrar,
    ) -> Result<PublishedImage> {
        Err(unused("publish"))
    }

    async fn discard_image(&self, _image: &PublishedImage) -> Result<()> {
        Err(unused("discard an image"))
    }

    async fn cleanup_failed_build(&self, _req: &BuildRequest<'_>, _vm: &VmRef) -> Vec<String> {
        Vec::new()
    }

    async fn create_installer_vm(&self, _req: &InstallerRequest<'_>) -> Result<VmRef> {
        Err(unused("create an installation VM"))
    }
}

struct FakeDisk;

impl DiskSpace for FakeDisk {
    fn free_bytes(&self, _path: &Path) -> Result<u64> {
        Ok(500 * 1024 * 1024 * 1024)
    }
}

const KALI_YAML: &str = r#"apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: kali-arm64

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

workspace:
  root: /home/kali/wargame
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

workspace:
  root: /Users/admin/wargame
"#;

struct Fixture {
    ctx: AppContext,
    utm_builder: Arc<StubBuilder>,
    utm_vms: Arc<FakeVmProvider>,
    builds: Arc<MemoryBuildRepo>,
    #[allow(dead_code)]
    home: PathBuf,
}

impl Fixture {
    async fn record(&self, profile: &str) -> Option<BuildRecord> {
        self.builds.load(profile).await.unwrap()
    }

    /// A `kali-arm64` record that owns an installation VM and a published
    /// image, both registered with the fake backend as `stopped`.
    async fn with_built_profile(&self) -> BuildRecord {
        let profile = self.ctx.profiles.get("kali-arm64").unwrap();
        let mut record = BuildRecord::new(
            &profile,
            "utm",
            BuildSource::Iso {
                path: PathBuf::from("/iso/kali-arm64.iso"),
                sha256: None,
                verified_sha256: false,
            },
        );
        record.installer_vm = Some(VmRef {
            backend: "utm".to_string(),
            id: "INSTALLER-UUID".to_string(),
            name: INSTALLER_VM.to_string(),
        });
        record.installer_media = InstallerMedia::Attached;
        record.image = Some(PublishedImage::new("IMAGE-UUID", IMAGE_VM));
        record.set_phase(BuildPhase::Ready);
        self.builds.save(&record).await.unwrap();
        self.utm_vms.set_vm(INSTALLER_VM, VmStatus::Stopped);
        self.utm_vms.set_vm(IMAGE_VM, VmStatus::Stopped);
        self.utm_vms.set_vm(SOMEONE_ELSES_VM, VmStatus::Stopped);
        record
    }
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap().keep();
    let dirs = AppDirs { root: home.clone() };
    dirs.ensure_layout().unwrap();
    let profiles_dir = dirs.profiles_dir();
    std::fs::write(profiles_dir.join("kali-arm64.yaml"), KALI_YAML).unwrap();
    std::fs::write(profiles_dir.join("macos-re-arm64.yaml"), MACOS_YAML).unwrap();
    for rel in ["scripts/kali/common.sh", "scripts/macos/common.sh"] {
        let path = profiles_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/usr/bin/env bash\n").unwrap();
    }

    let utm_builder = Arc::new(StubBuilder::utm());
    let tart_builder = Arc::new(StubBuilder::tart());
    let utm_vms = Arc::new(FakeVmProvider::with_backend("utm"));
    let builds = Arc::new(MemoryBuildRepo::new());
    let transport = Arc::new(FakeTransport::new()) as Arc<dyn GuestTransport>;
    let ctx = AppContext {
        config: Default::default(),
        dirs,
        wargame: Arc::new(FakeWargameProvider::new()),
        backends: BackendRegistry::new(vec![
            Arc::new(FakeVmProvider::new()) as Arc<dyn VmProvider>,
            utm_vms.clone() as Arc<dyn VmProvider>,
        ]),
        transports: TransportRegistry::new(transport.clone()).with_powershell(transport),
        repo: Arc::new(MemoryRepo::new()),
        builds: builds.clone(),
        builders: BuilderRegistry::new(vec![
            utm_builder.clone() as Arc<dyn ProfileImageBuilder>,
            tart_builder as Arc<dyn ProfileImageBuilder>,
        ]),
        disk: Arc::new(FakeDisk),
        profiles: ProfileStore::new(Vec::new(), profiles_dir),
        runner: Arc::new(FakeRunner::new()),
        bootstrap: wargame_core::app::BootstrapScripts::new("", ""),
    };
    Fixture {
        ctx,
        utm_builder,
        utm_vms,
        builds,
        home,
    }
}

// --- profile eject ---------------------------------------------------------

#[tokio::test]
async fn eject_detaches_the_media_of_the_recorded_installation_vm_and_writes_it_down() {
    let f = fixture();
    f.with_built_profile().await;

    let report = profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
        .await
        .unwrap();
    assert_eq!(report.outcome, EjectOutcome::Detached(1));
    assert_eq!(report.vm.name, INSTALLER_VM);
    assert_eq!(
        f.utm_builder.ejected(),
        vec!["INSTALLER-UUID".to_string()],
        "the VM comes from the record, by id"
    );
    assert_eq!(
        f.record("kali-arm64").await.unwrap().installer_media,
        InstallerMedia::Detached
    );
}

#[tokio::test]
async fn ejecting_twice_is_harmless_and_still_says_the_media_is_gone() {
    let f = fixture();
    f.with_built_profile().await;
    profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
        .await
        .unwrap();
    f.utm_builder.set_eject(Ok(EjectOutcome::NothingAttached));

    let report = profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
        .await
        .unwrap();
    assert_eq!(report.outcome, EjectOutcome::NothingAttached);
    assert_eq!(
        f.record("kali-arm64").await.unwrap().installer_media,
        InstallerMedia::Detached
    );
}

/// A running VM cannot have its configuration written. That is a refusal
/// with a way out, not a silent no-op -- and the record must not start
/// claiming the media is gone.
#[tokio::test]
async fn ejecting_a_running_installation_vm_names_profile_stop() {
    let f = fixture();
    f.with_built_profile().await;
    f.utm_builder.set_eject(Ok(EjectOutcome::Blocked(
        "wg-build-kali-arm64 is started".to_string(),
    )));

    let err = profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
        .await
        .unwrap_err();
    let message = format!("{err}{}", err.hint().unwrap_or_default());
    assert!(message.contains("is started"), "{message}");
    assert!(
        message.contains("wargame profile stop kali-arm64"),
        "the way out has to be named: {message}"
    );
    assert_eq!(
        f.record("kali-arm64").await.unwrap().installer_media,
        InstallerMedia::Attached
    );
}

/// Stopping the VM would not help: `removable` is fixed when a drive is
/// created, so the only way out is by hand in UTM. The hint must not send
/// the user to `profile stop`.
#[tokio::test]
async fn ejecting_a_vm_whose_every_drive_is_removable_is_an_error_that_does_not_say_stop() {
    let f = fixture();
    f.with_built_profile().await;
    f.utm_builder
        .set_eject(Ok(EjectOutcome::EveryDriveRemovable(
            "every drive of wg-build-kali-arm64 is marked removable".to_string(),
        )));

    let err = profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
        .await
        .unwrap_err();
    let message = err.to_string();
    let hint = err.hint().unwrap_or_default();
    assert!(message.contains("marked removable"), "{message}");
    assert!(
        !hint.contains("profile stop"),
        "stopping it changes nothing: {hint}"
    );
    assert!(hint.contains("UTM"), "{hint}");
    assert_eq!(
        f.record("kali-arm64").await.unwrap().installer_media,
        InstallerMedia::Attached,
        "nothing was detached, so nothing may be recorded as detached"
    );
}

/// These verbs act on what the *record* owns, so a profile whose backend was
/// edited afterwards is refused rather than routed to a backend that never
/// made anything.
#[tokio::test]
async fn a_record_from_another_backend_is_refused_by_every_verb() {
    for verb in ["eject", "stop", "delete"] {
        let f = fixture();
        let mut record = f.with_built_profile().await;
        record.backend = "tart".to_string();
        f.builds.save(&record).await.unwrap();

        let err = match verb {
            "eject" => profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
                .await
                .unwrap_err(),
            "stop" => profile_vm::stop(&f.ctx, "kali-arm64").await.unwrap_err(),
            _ => profile_vm::delete(&f.ctx, "kali-arm64").await.unwrap_err(),
        };
        let message = err.to_string();
        assert!(message.contains("tart"), "{verb}: {message}");
        assert!(message.contains("utm"), "{verb}: {message}");
        assert!(f.utm_builder.ejected().is_empty(), "{verb}");
        assert!(
            !f.utm_vms
                .calls()
                .iter()
                .any(|c| c == "delete" || c == "stop"),
            "{verb}: {:?}",
            f.utm_vms.calls()
        );
    }
}

#[tokio::test]
async fn ejecting_a_profile_nobody_created_names_profile_create() {
    let f = fixture();
    let err = profile_vm::eject(&f.ctx, "kali-arm64", &mut |_| {})
        .await
        .unwrap_err();
    assert!(
        err.hint().unwrap_or_default().contains("profile create"),
        "{err:?}"
    );
}

#[tokio::test]
async fn ejecting_a_tart_profile_is_refused_before_anything_runs() {
    let f = fixture();
    let err = profile_vm::eject(&f.ctx, "macos-re-arm64", &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.to_string().contains("installation VM"), "{err}");
    assert!(f.utm_builder.ejected().is_empty());
}

// --- profile stop ----------------------------------------------------------

#[tokio::test]
async fn stop_stops_the_installation_vm_through_the_backend_the_record_names() {
    let f = fixture();
    f.with_built_profile().await;
    f.utm_vms.set_vm(INSTALLER_VM, VmStatus::Running);

    let report = profile_vm::stop(&f.ctx, "kali-arm64").await.unwrap();
    assert!(report.was_running);
    assert_eq!(report.vm.name, INSTALLER_VM);
    assert!(f.utm_vms.calls().iter().any(|c| c == "stop"));
    assert_eq!(
        f.utm_vms.calls().iter().filter(|c| *c == "stop").count(),
        1,
        "only the installation VM is stopped, not the image"
    );
}

#[tokio::test]
async fn stopping_an_already_stopped_installation_vm_is_a_no_op() {
    let f = fixture();
    f.with_built_profile().await;

    let report = profile_vm::stop(&f.ctx, "kali-arm64").await.unwrap();
    assert!(!report.was_running);
    assert!(
        !f.utm_vms.calls().iter().any(|c| c == "stop"),
        "{:?}",
        f.utm_vms.calls()
    );
}

#[tokio::test]
async fn stopping_a_vm_utm_no_longer_has_is_reported_not_an_error() {
    let f = fixture();
    let mut record = f.with_built_profile().await;
    record.installer_vm = Some(VmRef {
        backend: "utm".to_string(),
        id: "GONE".to_string(),
        name: "wg-build-gone".to_string(),
    });
    f.builds.save(&record).await.unwrap();

    let report = profile_vm::stop(&f.ctx, "kali-arm64").await.unwrap();
    assert_eq!(report.status, VmStatus::Missing);
    assert!(!report.was_running);
}

/// The backend's own T12 refusal -- a QEMU process wargame never spawned is
/// still holding the VM -- has to reach the user with its pid, not be
/// swallowed into "stopped".
#[tokio::test]
async fn stop_passes_an_orphaned_backend_process_through_with_its_pid() {
    let f = fixture();
    f.with_built_profile().await;
    f.utm_vms.set_vm(INSTALLER_VM, VmStatus::Running);
    f.utm_vms.set_orphan_on_stop(58445);

    let err = profile_vm::stop(&f.ctx, "kali-arm64").await.unwrap_err();
    assert!(
        matches!(err, WargameError::OrphanProcess { pid: 58445, .. }),
        "{err:?}"
    );
    assert!(err.hint().unwrap_or_default().contains("58445"), "{err:?}");
}

#[tokio::test]
async fn stopping_a_tart_profile_is_refused() {
    let f = fixture();
    let err = profile_vm::stop(&f.ctx, "macos-re-arm64")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("installation VM"), "{err}");
}

// --- profile delete --------------------------------------------------------

#[tokio::test]
async fn delete_removes_the_installation_vm_the_image_and_the_record() {
    let f = fixture();
    f.with_built_profile().await;

    let plan = profile_vm::plan_delete(&f.ctx, "kali-arm64").await.unwrap();
    let planned: Vec<&str> = plan.vms.iter().map(|v| v.vm.name.as_str()).collect();
    assert_eq!(planned, vec![INSTALLER_VM, IMAGE_VM]);

    let report = profile_vm::delete(&f.ctx, "kali-arm64").await.unwrap();
    let deleted: Vec<&str> = report.deleted.iter().map(|v| v.vm.name.as_str()).collect();
    assert_eq!(deleted, vec![INSTALLER_VM, IMAGE_VM]);
    assert!(
        f.record("kali-arm64").await.is_none(),
        "the record goes too"
    );
    assert_eq!(
        f.utm_vms.vm_names(),
        vec![SOMEONE_ELSES_VM.to_string()],
        "a VM no record owns is never touched"
    );
}

/// FR-5: a running VM is not deleted out from under the user. The refusal
/// names the verb that stops it.
#[tokio::test]
async fn delete_refuses_while_a_vm_the_record_owns_is_running() {
    let f = fixture();
    f.with_built_profile().await;
    f.utm_vms.set_vm(INSTALLER_VM, VmStatus::Running);

    let err = profile_vm::delete(&f.ctx, "kali-arm64").await.unwrap_err();
    let message = format!("{err}{}", err.hint().unwrap_or_default());
    assert!(message.contains(INSTALLER_VM), "{message}");
    assert!(
        message.contains("wargame profile stop kali-arm64"),
        "{message}"
    );
    assert!(
        !f.utm_vms.calls().iter().any(|c| c == "delete"),
        "nothing may be deleted: {:?}",
        f.utm_vms.calls()
    );
    assert!(f.record("kali-arm64").await.is_some(), "the record stays");
}

/// A VM that could not be removed leaves the record in place: it is the
/// only thing that knows the VM exists, so throwing it away would strand
/// the VM outside every verb that can reach it.
#[tokio::test]
async fn delete_keeps_the_record_when_a_vm_could_not_be_deleted() {
    let f = fixture();
    f.with_built_profile().await;
    f.utm_vms.set_fail_on("delete");

    let err = profile_vm::delete(&f.ctx, "kali-arm64").await.unwrap_err();
    assert!(err.to_string().contains(INSTALLER_VM), "{err}");
    assert!(f.record("kali-arm64").await.is_some());
}

#[tokio::test]
async fn deleting_a_profile_whose_vms_are_already_gone_still_removes_the_record() {
    let f = fixture();
    let record = f.with_built_profile().await;
    for name in [INSTALLER_VM, IMAGE_VM] {
        f.ctx
            .backends
            .get("utm")
            .unwrap()
            .delete(&VmRef {
                backend: "utm".to_string(),
                id: "x".to_string(),
                name: name.to_string(),
            })
            .await
            .unwrap();
    }

    let report = profile_vm::delete(&f.ctx, &record.profile).await.unwrap();
    assert!(report.deleted.is_empty());
    assert_eq!(report.already_gone.len(), 2);
    assert!(f.record("kali-arm64").await.is_none());
}

#[tokio::test]
async fn deleting_a_profile_nobody_created_names_profile_create() {
    let f = fixture();
    let err = profile_vm::delete(&f.ctx, "kali-arm64").await.unwrap_err();
    assert!(
        err.hint().unwrap_or_default().contains("profile create"),
        "{err:?}"
    );
}

/// Every image the record still knows about is removed, including one an
/// earlier build could not delete -- those exist on the backend and are
/// tracked precisely so they are not left behind.
#[tokio::test]
async fn delete_covers_the_images_a_previous_build_left_behind() {
    let f = fixture();
    let mut record = f.with_built_profile().await;
    record.previous_image = Some(PublishedImage::new("OLD-UUID", "wg-build-image-old"));
    record
        .leaked_images
        .push(PublishedImage::new("LEAKED-UUID", "wg-build-image-leaked"));
    f.builds.save(&record).await.unwrap();
    f.utm_vms.set_vm("wg-build-image-old", VmStatus::Stopped);
    f.utm_vms.set_vm("wg-build-image-leaked", VmStatus::Stopped);

    let report = profile_vm::delete(&f.ctx, "kali-arm64").await.unwrap();
    let deleted: Vec<&str> = report.deleted.iter().map(|v| v.vm.name.as_str()).collect();
    assert!(deleted.contains(&"wg-build-image-old"), "{deleted:?}");
    assert!(deleted.contains(&"wg-build-image-leaked"), "{deleted:?}");
    assert_eq!(f.utm_vms.vm_names(), vec![SOMEONE_ELSES_VM.to_string()]);
}
