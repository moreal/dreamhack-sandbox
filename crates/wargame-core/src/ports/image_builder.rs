//! The reusable-image build contract (spec.md §5 item 4: "공통 이미지
//! 빌드는 준비→복제→부팅→연결→프로비저닝→검증→정상 종료→게시 순서를 갖는다.
//! Tart 세부 처리는 Tart adapter에 남긴다").
//!
//! [`crate::app::profile_build`] owns that order and every decision made
//! along it -- what counts as ready, what is preserved on failure, what goes
//! into the build record. A backend implements [`ProfileImageBuilder`] with
//! only the steps that are actually backend-specific, and never sees the
//! record's state machine.
//!
//! [`BuildRecordRepository`] is where those records live.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::domain::build::{BuildRecord, BuildSource, BuildStage, PublishedImage};
use crate::domain::compat::ResolvedBackend;
use crate::domain::{GuestEndpoint, Profile, VmRef};
use crate::error::{Result, WargameError};
use crate::ports::GuestTransport;

/// Where a build step sends its human-readable progress. `Send` so the
/// futures an `async_trait` method returns stay `Send`.
pub type Progress<'a> = &'a mut (dyn FnMut(String) + Send);

/// Everything a build step needs that is not backend-specific state.
pub struct BuildRequest<'a> {
    pub profile: &'a Profile,
    /// The record as it stands *before* this step -- for the installer VM
    /// this build must reuse, and the source it was created from.
    pub record: &'a BuildRecord,
    pub execution: ResolvedBackend,
    /// Directory the profile's `provision.scripts` paths are relative to.
    pub scripts_dir: PathBuf,
    /// Where the backend's own console/run log goes.
    pub log_path: PathBuf,
    /// How long the guest may take to answer, from backend and mode
    /// (spec.md §3 FR-5).
    pub boot_timeout: Duration,
}

impl BuildRequest<'_> {
    /// The installer/build VM this profile already owns, if any.
    pub fn installer_vm(&self) -> Option<&VmRef> {
        self.record.installer_vm.as_ref()
    }
}

/// What `profile create` asks a backend to bring into existence.
pub struct InstallerRequest<'a> {
    pub profile: &'a Profile,
    /// The display name the VM must be created with -- already carrying the
    /// build prefix, so it is outside the session namespace.
    pub name: String,
    pub execution: ResolvedBackend,
    pub source: &'a BuildSource,
}

/// Told which stage the build has reached, before that stage runs.
///
/// This is how the record ends up saying where a build actually stopped: the
/// flow itself owns no record, so it announces its progress and whoever
/// called it decides what that means. An error aborts the build -- a stage
/// that cannot be written down is not one to carry on from.
#[async_trait]
pub trait BuildObserver: Send + Sync {
    async fn stage_started(&self, stage: BuildStage) -> Result<()>;
}

/// The observer for callers that do not keep a record (every backend's own
/// tests): notice nothing, fail nothing.
#[async_trait]
impl BuildObserver for () {
    async fn stage_started(&self, _stage: BuildStage) -> Result<()> {
        Ok(())
    }
}

/// What detaching the installation VM's installer media did.
///
/// An installed guest that still has its installer CD attached boots the CD
/// (UEFI prefers it), so it never reaches SSH and every copy taken of it
/// inherits the CD (task-7-phase-a-report.md §5 D3). Ejecting is therefore
/// part of building, not a convenience -- and it is idempotent, so both
/// [`EjectOutcome::Detached`] and [`EjectOutcome::NothingAttached`] mean
/// "this VM now boots from its own disk".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EjectOutcome {
    /// This many removable drives were detached.
    Detached(usize),
    /// There was nothing to detach.
    NothingAttached,
    /// This backend has no installation media (Tart builds from an image).
    NotApplicable,
    /// Not done, and why -- the VM has to be stopped for its configuration
    /// to be written, and it was not. Never an error on its own: the build
    /// carries on and says what it skipped.
    Blocked(String),
    /// **Refused**, and why: every drive this VM has is removable, so
    /// detaching them would leave it with nothing at all -- which, on a VM
    /// adopted from one the user already had, is their installed system.
    /// Nothing was written. Like [`EjectOutcome::Blocked`] this does not
    /// fail a build; unlike it, no amount of stopping the VM changes it.
    EveryDriveRemovable(String),
}

impl EjectOutcome {
    /// Whether the VM is now free of installer media.
    pub fn is_detached(&self) -> bool {
        matches!(
            self,
            EjectOutcome::Detached(_) | EjectOutcome::NothingAttached
        )
    }
}

/// A live connection to the guest being provisioned.
pub struct GuestLink {
    pub endpoint: GuestEndpoint,
    pub transport: Arc<dyn GuestTransport>,
}

/// The backend half of the common build flow.
///
/// Ordering, record keeping, provisioning and verification all live in
/// [`crate::app::profile_build`]; an implementation only has to know how
/// *its* backend prepares, clones, boots, connects, shuts down and
/// publishes.
#[async_trait]
pub trait ProfileImageBuilder: Send + Sync {
    /// The backend name this builder implements (`"tart"`, `"utm"`), matching
    /// [`crate::ports::VmProvider::name`].
    fn backend(&self) -> &str;

    // --- `profile create`: the installer VM (UTM). ------------------------

    /// Whether this backend builds through a long-lived installation VM the
    /// user installs an OS into. Tart clones a ready-made base image
    /// instead, and answers `false`.
    fn uses_installer_vm(&self) -> bool {
        false
    }

    /// Creates the installation VM (blank disk + installer ISO), or clones
    /// the source VM named by the request's [`BuildSource`].
    async fn create_installer_vm(&self, req: &InstallerRequest<'_>) -> Result<VmRef> {
        Err(not_supported(
            self.backend(),
            "create an installation VM",
            req.profile,
        ))
    }

    /// Opens the installer VM's graphical console.
    async fn open_console(&self, _vm: &VmRef) -> Result<()> {
        Err(WargameError::Other(format!(
            "the {} backend has no installation console: its images are built from a base image, not installed by hand",
            self.backend()
        )))
    }

    // --- `profile build`: the common flow, in order. ----------------------

    /// Step 1 -- prepare: make sure everything the clone needs exists (Tart
    /// pulls the base image; UTM checks the installer VM is still there).
    async fn prepare(&self, req: &BuildRequest<'_>, progress: Progress<'_>) -> Result<()>;

    /// Step 2 -- clone: the VM this build provisions. For a backend with an
    /// installer VM this *is* that VM (nothing is copied until publish);
    /// for Tart it is a fresh clone of the base image.
    async fn clone_for_build(
        &self,
        req: &BuildRequest<'_>,
        progress: Progress<'_>,
    ) -> Result<VmRef>;

    /// Step 2.5 -- eject: detach the installer media before the VM boots.
    ///
    /// Runs on every build, and must be idempotent: a VM whose CD is
    /// already gone answers [`EjectOutcome::NothingAttached`]. A backend
    /// that has no installation media at all leaves this alone.
    async fn eject_installer_media(
        &self,
        _vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<EjectOutcome> {
        Ok(EjectOutcome::NotApplicable)
    }

    /// Step 3 -- boot: start `vm` on NAT (spec.md §3 FR-6: builds install
    /// packages, so they never run isolated).
    async fn boot(&self, req: &BuildRequest<'_>, vm: &VmRef, progress: Progress<'_>) -> Result<()>;

    /// Step 4 -- connect: wait for an address and a usable guest shell.
    async fn connect(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<GuestLink>;

    /// Step 7 -- graceful shutdown. Must fail rather than report success on
    /// a VM that did not actually stop: an image taken from a guest killed
    /// mid-flush is corrupt.
    async fn shutdown(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<()>;

    /// Step 8 -- publish: turn the stopped, provisioned VM into the
    /// launchable image and hand back what to record.
    async fn publish(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<PublishedImage>;

    /// Deletes an image that has been replaced. Only ever called *after* the
    /// new image is recorded, so a failure here loses nothing.
    async fn discard_image(&self, image: &PublishedImage) -> Result<()>;

    /// Best-effort teardown of whatever the failed build left running.
    /// Returns one line per step that could not be undone; those go into the
    /// record's `cleanup_failures` (spec.md §3 FR-5).
    async fn cleanup_failed_build(&self, req: &BuildRequest<'_>, vm: &VmRef) -> Vec<String>;
}

fn not_supported(backend: &str, what: &str, profile: &Profile) -> WargameError {
    WargameError::InvalidProfile {
        name: profile.name.clone(),
        reason: format!("the {backend} backend cannot {what}"),
    }
}

/// Persistence for [`BuildRecord`]s, keyed by profile name (spec.md §5 item
/// 5: "설치 build record와 문제 session record를 분리한다").
#[async_trait]
pub trait BuildRecordRepository: Send + Sync {
    async fn save(&self, record: &BuildRecord) -> Result<()>;
    /// `None` when this profile has never been `profile create`d.
    async fn load(&self, profile: &str) -> Result<Option<BuildRecord>>;
    async fn list(&self) -> Result<Vec<BuildRecord>>;
    async fn delete(&self, profile: &str) -> Result<()>;
}
