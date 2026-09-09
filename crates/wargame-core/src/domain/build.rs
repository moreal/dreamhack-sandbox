//! The app-owned **build record**: what `profile create` started, how far
//! `profile build` got, and which reusable image (if any) came out of it
//! (spec.md §3 FR-2/FR-3, §5 item 5).
//!
//! A build record is deliberately *not* a session record: it outlives every
//! challenge session, it owns VMs (the installer VM and the published image
//! VM) that `prune` must never touch, and it is the only place that knows
//! whether a UTM profile has anything to launch from at all -- UTM has no
//! image registry, so "the image" is a stopped VM this app built and wrote
//! down here (docs/utm-compatibility.md §2).
//!
//! Nothing secret ever lands in a record: `guest.password` is used at
//! runtime and never serialized here (spec.md §5 item 7).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::domain::profile::{ExecutionMode, GuestArch, GuestOs, Profile};
use crate::domain::session::VmRef;

/// Where the image being built comes from (spec.md §3 FR-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildSource {
    /// A local installation ISO the user supplies. `sha256` is whatever the
    /// user passed on the command line; `verified_sha256` says whether it
    /// was actually checked against the file (an unverified ISO is never
    /// reported as verified -- FR-2).
    Iso {
        path: PathBuf,
        #[serde(default)]
        sha256: Option<String>,
        #[serde(default)]
        verified_sha256: bool,
    },
    /// An already-installed local UTM VM, addressed by its UUID. The source
    /// is only ever cloned, never started or provisioned (FR-2).
    UtmVm { uuid: String },
    /// Tart's existing OCI/base image input (`image.base`), kept so the Tart
    /// path writes the same kind of record as the UTM one.
    TartBase { image: String },
}

impl BuildSource {
    /// A one-line description for the "you asked for a different source"
    /// conflict message.
    pub fn describe(&self) -> String {
        match self {
            BuildSource::Iso { path, .. } => format!("ISO {}", path.display()),
            BuildSource::UtmVm { uuid } => format!("UTM VM {uuid}"),
            BuildSource::TartBase { image } => format!("Tart base image {image}"),
        }
    }

    /// Whether two sources name the same thing. The verification flags are
    /// deliberately ignored: re-running `profile create` with the same ISO
    /// and no `--sha256` is the same source, not a new one.
    pub fn same_as(&self, other: &BuildSource) -> bool {
        match (self, other) {
            (BuildSource::Iso { path: a, .. }, BuildSource::Iso { path: b, .. }) => a == b,
            (BuildSource::UtmVm { uuid: a }, BuildSource::UtmVm { uuid: b }) => a == b,
            (BuildSource::TartBase { image: a }, BuildSource::TartBase { image: b }) => a == b,
            _ => false,
        }
    }
}

/// The step a build was in when it failed, so a failure says *where*
/// (spec.md §5 item 4's flow order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStage {
    Preparing,
    Cloning,
    Booting,
    Connecting,
    Provisioning,
    Verifying,
    Publishing,
}

impl BuildStage {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildStage::Preparing => "preparing",
            BuildStage::Cloning => "cloning",
            BuildStage::Booting => "booting",
            BuildStage::Connecting => "connecting",
            BuildStage::Provisioning => "provisioning",
            BuildStage::Verifying => "verifying",
            BuildStage::Publishing => "publishing",
        }
    }
}

impl std::fmt::Display for BuildStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How far this profile's image has got.
///
/// `Ready` is reached only after verification *and* a graceful shutdown
/// *and* publishing (spec.md's 산출 계약: "build 성공은 검증된 정지 이미지
/// 게시이며 실패·설치 대기를 ready로 표시하지 않는다"). A guest that never
/// finished its OS installation stays in [`BuildPhase::AwaitingInstall`] --
/// it is not a failure, it is a wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum BuildPhase {
    /// The record exists; the installer VM has not been made yet.
    Created,
    /// The installer VM exists and the guest is not reachable yet: the user
    /// installs the OS and prepares SSH/guest tools through the console.
    AwaitingInstall,
    Provisioning,
    Verifying,
    Publishing,
    Ready,
    Failed {
        stage: BuildStage,
        error: String,
    },
}

impl BuildPhase {
    /// The short word shown in `profiles`/`doctor`.
    pub fn as_str(&self) -> &'static str {
        match self {
            BuildPhase::Created => "created",
            BuildPhase::AwaitingInstall => "awaiting-install",
            BuildPhase::Provisioning => "provisioning",
            BuildPhase::Verifying => "verifying",
            BuildPhase::Publishing => "publishing",
            BuildPhase::Ready => "ready",
            BuildPhase::Failed { .. } => "failed",
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, BuildPhase::Ready)
    }
}

impl std::fmt::Display for BuildPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildPhase::Failed { stage, error } => write!(f, "failed while {stage}: {error}"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// Whether the installation VM still has its installer CD in the drive.
///
/// It matters because UEFI boots the removable drive first: an installed
/// guest with the CD still attached comes back up in the installer, never
/// reaches SSH, and hands a copy of itself the same CD
/// (task-7-phase-a-report.md §5 D3). `profile create` attaches one;
/// `profile build` and `profile eject` take it out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerMedia {
    /// Never established -- an older record, a backend with no installation
    /// VM, or a VM cloned from one the user already had.
    #[default]
    Unknown,
    /// A removable drive with installer media is attached.
    Attached,
    /// No removable drive: the guest boots from its own disk.
    Detached,
}

impl InstallerMedia {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallerMedia::Unknown => "unknown",
            InstallerMedia::Attached => "attached",
            InstallerMedia::Detached => "detached",
        }
    }
}

impl std::fmt::Display for InstallerMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A published, launchable image: the stopped VM (UTM) or local image
/// (Tart) a finished build produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedImage {
    /// The backend's stable id -- a UTM UUID, a Tart image name.
    pub id: String,
    /// The display name, for messages.
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
}

impl PublishedImage {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        PublishedImage {
            id: id.into(),
            name: name.into(),
            published_at: OffsetDateTime::now_utc(),
        }
    }
}

/// One `tools.verify` command and how it exited (spec.md §3 FR-3: "단계·종료
/// 코드·실패한 검증 명령을 로그에 남긴다").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub command: String,
    pub exit_code: i32,
}

impl VerificationResult {
    pub fn passed(&self) -> bool {
        self.exit_code == 0
    }
}

/// Everything the app knows about one profile's image build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecord {
    pub profile: String,
    /// The backend that owns every VM named in this record (`"utm"`, `"tart"`).
    pub backend: String,
    pub source: BuildSource,
    pub os: GuestOs,
    pub arch: GuestArch,
    pub mode: ExecutionMode,
    /// The long-lived installation VM (UTM only). Preserved across failures
    /// so the user can resume the install (spec.md §3 FR-5).
    #[serde(default)]
    pub installer_vm: Option<VmRef>,
    /// Whether that VM still has its installer CD attached. Written by
    /// `profile create` (which attaches one), by the build's eject step and
    /// by `profile eject`.
    #[serde(default)]
    pub installer_media: InstallerMedia,
    pub phase: BuildPhase,
    /// The image a finished build published, and what `launch` clones from.
    #[serde(default)]
    pub image: Option<PublishedImage>,
    /// The image that was published before the current one, while it still
    /// exists. Set by a successful publish so a crash between "the new image
    /// is recorded" and "the old one is deleted" leaves two images rather
    /// than none (FR-3).
    #[serde(default)]
    pub previous_image: Option<PublishedImage>,
    /// Images this profile published that could not be deleted when they
    /// were replaced. They still exist on the backend, so they stay tracked:
    /// `prune` must not treat them as orphans, and the next build retries
    /// deleting them (spec.md §3 FR-5's "정리 실패를 기록한다").
    #[serde(default)]
    pub leaked_images: Vec<PublishedImage>,
    /// Hash of the profile definition the current image was built from.
    #[serde(default)]
    pub profile_hash: String,
    /// Hash of the provisioning scripts the current image was built from.
    #[serde(default)]
    pub scripts_hash: String,
    #[serde(default)]
    pub verification: Vec<VerificationResult>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Steps that could not be cleaned up after a failure, kept so the user
    /// is told rather than left with silent leftovers (spec.md §3 FR-5:
    /// "실패 시 단계별 정리와 정리 실패를 기록한다").
    #[serde(default)]
    pub cleanup_failures: Vec<String>,
}

impl BuildRecord {
    /// A fresh record in [`BuildPhase::Created`].
    pub fn new(profile: &Profile, backend: impl Into<String>, source: BuildSource) -> Self {
        let now = OffsetDateTime::now_utc();
        BuildRecord {
            profile: profile.name.clone(),
            backend: backend.into(),
            source,
            os: profile.guest.os,
            arch: profile.guest.arch,
            mode: profile.vm.mode,
            installer_vm: None,
            installer_media: InstallerMedia::Unknown,
            phase: BuildPhase::Created,
            image: None,
            previous_image: None,
            leaked_images: Vec::new(),
            profile_hash: String::new(),
            scripts_hash: String::new(),
            verification: Vec::new(),
            created_at: now,
            updated_at: now,
            last_error: None,
            cleanup_failures: Vec::new(),
        }
    }

    /// Moves to `phase` and stamps `updated_at`.
    pub fn set_phase(&mut self, phase: BuildPhase) {
        if let BuildPhase::Failed { stage, error } = &phase {
            self.last_error = Some(format!("{stage}: {error}"));
        }
        self.phase = phase;
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// Remembers an image that still exists on the backend but is no longer
    /// the published one, so `prune` keeps skipping it and the next build
    /// retries deleting it. Recording the same image twice is a no-op.
    pub fn track_leaked_image(&mut self, image: PublishedImage) {
        if self.image.as_ref().map(|i| i.id == image.id) == Some(true) {
            return;
        }
        if self.leaked_images.iter().any(|known| known.id == image.id) {
            return;
        }
        self.leaked_images.push(image);
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// Records a step that could not be undone after a failure.
    pub fn record_cleanup_failure(&mut self, detail: impl Into<String>) {
        self.cleanup_failures.push(detail.into());
        self.updated_at = OffsetDateTime::now_utc();
    }

    /// The image `launch` may clone from.
    ///
    /// `image` is written **only** by a successful publish -- after every
    /// verification passed and the guest shut down cleanly -- so its mere
    /// presence is the "ready" signal (spec.md's 산출 계약: 설치 대기나 실패는
    /// ready가 아니다). It deliberately survives a later *failed* rebuild:
    /// the last good image stays launchable (FR-3, AC-09).
    pub fn ready_image(&self) -> Option<&PublishedImage> {
        self.image.as_ref()
    }

    /// Whether the profile or its scripts changed since the current image
    /// was built (spec.md §3 FR-3: "프로필 변경과 이미지 불일치는 재빌드를
    /// 안내한다"). Only meaningful once something has been published.
    pub fn needs_rebuild(&self, profile_hash: &str, scripts_hash: &str) -> bool {
        self.image.is_some()
            && (self.profile_hash != profile_hash || self.scripts_hash != scripts_hash)
    }

    /// Every `(backend, vm id)` this record owns -- the installer VM, the
    /// published image, the image it replaced, and any image a failed
    /// deletion left behind. `prune` skips these: they have no session, but
    /// they are not orphans (spec.md §3 FR-5).
    pub fn owned_vm_ids(&self) -> Vec<(String, String)> {
        let mut owned = Vec::new();
        if let Some(vm) = &self.installer_vm {
            owned.push((vm.backend.clone(), vm.id.clone()));
        }
        for image in [self.image.as_ref(), self.previous_image.as_ref()]
            .into_iter()
            .flatten()
            .chain(self.leaked_images.iter())
        {
            owned.push((self.backend.clone(), image.id.clone()));
        }
        owned
    }
}

/// A stable hash of everything about a profile that changes what gets
/// built. `guest.password` is deliberately excluded: it is a secret, it
/// never belongs in a record, and it does not change the resulting image
/// (spec.md §5 item 7).
pub fn profile_hash(profile: &Profile) -> String {
    let mut hasher = Sha256::new();
    let mut field = |key: &str, value: &str| {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    };
    field("name", &profile.name);
    field("backend", profile.vm.backend.as_str());
    field("mode", profile.vm.mode.as_str());
    field("os", &format!("{:?}", profile.guest.os));
    field("arch", &format!("{:?}", profile.guest.arch));
    field(
        "distribution",
        profile.guest.distribution_key().as_deref().unwrap_or(""),
    );
    field("user", &profile.guest.user);
    field("image.base", &profile.image.base);
    field("cpu", &profile.resources.cpu.to_string());
    field("memory_mib", &profile.resources.memory_mib.to_string());
    field("disk_gib", &profile.resources.disk_gib.to_string());
    for script in &profile.provision.scripts {
        field("script", script);
    }
    for check in &profile.tools.verify {
        field("verify", check);
    }
    field("workspace", &profile.workspace.root);
    hex::encode(hasher.finalize())
}

/// A stable hash of the *contents* of a profile's provisioning scripts, so
/// editing a script (without touching the YAML) still asks for a rebuild
/// (spec.md §3 FR-3). A script that cannot be read hashes as `<missing>`
/// rather than failing: the build itself will report that properly.
pub fn scripts_hash(scripts_dir: &Path, scripts: &[String]) -> String {
    let mut hasher = Sha256::new();
    for script in scripts {
        hasher.update(script.as_bytes());
        hasher.update(b"\0");
        match std::fs::read(scripts_dir.join(script)) {
            Ok(bytes) => hasher.update(&bytes),
            Err(_) => hasher.update(b"<missing>"),
        }
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::profile::{
        GuestSpec, ImageSpec, ProvisionSpec, Resources, ToolsSpec, VmSettings, WorkspaceSpec,
    };

    fn profile() -> Profile {
        Profile {
            name: "kali-custom".to_string(),
            description: String::new(),
            vm: VmSettings::default(),
            guest: GuestSpec {
                os: GuestOs::Linux,
                arch: GuestArch::X86_64,
                distribution: Some("kali".to_string()),
                user: "kali".to_string(),
                password: Some("s3cr3t".to_string()),
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

    fn record() -> BuildRecord {
        BuildRecord::new(
            &profile(),
            "utm",
            BuildSource::Iso {
                path: PathBuf::from("/iso/kali.iso"),
                sha256: None,
                verified_sha256: false,
            },
        )
    }

    #[test]
    fn a_new_record_is_created_and_has_nothing_to_launch_from() {
        let record = record();
        assert_eq!(record.phase, BuildPhase::Created);
        assert!(record.ready_image().is_none());
        assert!(record.owned_vm_ids().is_empty());
    }

    #[test]
    fn only_a_published_image_is_launchable() {
        let mut record = record();
        // Nothing published yet: awaiting install and mid-publish are both
        // "there is nothing to launch from".
        record.set_phase(BuildPhase::AwaitingInstall);
        assert!(record.ready_image().is_none());
        record.set_phase(BuildPhase::Publishing);
        assert!(record.ready_image().is_none());

        record.image = Some(PublishedImage::new("UUID-1", "wg-build-image-kali-custom"));
        record.set_phase(BuildPhase::Ready);
        assert_eq!(record.ready_image().unwrap().id, "UUID-1");

        // A later failed rebuild must not take the last good image away.
        record.set_phase(BuildPhase::Failed {
            stage: BuildStage::Verifying,
            error: "python3 --version exited 127".to_string(),
        });
        assert_eq!(record.ready_image().unwrap().id, "UUID-1");
        assert!(record.last_error.as_deref().unwrap().contains("verifying"));
    }

    #[test]
    fn owned_vm_ids_cover_the_installer_and_both_images() {
        let mut record = record();
        record.installer_vm = Some(VmRef {
            backend: "utm".to_string(),
            id: "INSTALLER".to_string(),
            name: "wg-build-kali-custom".to_string(),
        });
        record.image = Some(PublishedImage::new("NEW", "wg-build-image-kali-custom"));
        record.previous_image = Some(PublishedImage::new("OLD", "wg-build-image-kali-custom"));
        record
            .leaked_images
            .push(PublishedImage::new("LEAKED", "wg-build-image-kali-custom"));
        let owned = record.owned_vm_ids();
        assert!(owned.contains(&("utm".to_string(), "INSTALLER".to_string())));
        assert!(owned.contains(&("utm".to_string(), "NEW".to_string())));
        assert!(owned.contains(&("utm".to_string(), "OLD".to_string())));
        assert!(
            owned.contains(&("utm".to_string(), "LEAKED".to_string())),
            "an image whose deletion failed still exists, so prune must skip it"
        );
    }

    #[test]
    fn sources_compare_by_identity_not_by_verification_state() {
        let a = BuildSource::Iso {
            path: PathBuf::from("/iso/kali.iso"),
            sha256: None,
            verified_sha256: false,
        };
        let b = BuildSource::Iso {
            path: PathBuf::from("/iso/kali.iso"),
            sha256: Some("abc".to_string()),
            verified_sha256: true,
        };
        assert!(a.same_as(&b));
        assert!(!a.same_as(&BuildSource::UtmVm {
            uuid: "UUID".to_string()
        }));
    }

    #[test]
    fn the_profile_hash_ignores_the_password_but_not_the_scripts() {
        let base = profile();
        let mut other = base.clone();
        other.guest.password = Some("different".to_string());
        assert_eq!(profile_hash(&base), profile_hash(&other));

        let mut changed = base.clone();
        changed
            .provision
            .scripts
            .push("scripts/mine.sh".to_string());
        assert_ne!(profile_hash(&base), profile_hash(&changed));

        let mut resized = base;
        resized.resources.cpu = 8;
        assert_ne!(profile_hash(&profile()), profile_hash(&resized));
    }

    #[test]
    fn the_scripts_hash_follows_the_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("scripts/kali")).unwrap();
        let script = dir.path().join("scripts/kali/common.sh");
        std::fs::write(&script, "echo one\n").unwrap();
        let scripts = vec!["scripts/kali/common.sh".to_string()];

        let first = scripts_hash(dir.path(), &scripts);
        std::fs::write(&script, "echo two\n").unwrap();
        assert_ne!(first, scripts_hash(dir.path(), &scripts));

        // A missing script hashes to something stable rather than panicking.
        let missing = scripts_hash(dir.path(), &["scripts/nope.sh".to_string()]);
        assert_eq!(
            missing,
            scripts_hash(dir.path(), &["scripts/nope.sh".to_string()])
        );
    }

    #[test]
    fn a_leaked_image_is_tracked_once_and_never_the_published_one() {
        let mut record = record();
        record.image = Some(PublishedImage::new("CURRENT", "wg-build-image-kali-custom"));
        record.track_leaked_image(PublishedImage::new("OLD", "wg-build-image-kali-custom"));
        record.track_leaked_image(PublishedImage::new("OLD", "wg-build-image-kali-custom"));
        record.track_leaked_image(PublishedImage::new("CURRENT", "wg-build-image-kali-custom"));
        assert_eq!(
            record
                .leaked_images
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["OLD"]
        );
    }

    #[test]
    fn needs_rebuild_only_once_something_was_published() {
        let mut record = record();
        record.profile_hash = "a".to_string();
        record.scripts_hash = "b".to_string();
        assert!(!record.needs_rebuild("changed", "b"));

        record.image = Some(PublishedImage::new("UUID", "name"));
        assert!(record.needs_rebuild("changed", "b"));
        assert!(record.needs_rebuild("a", "changed"));
        assert!(!record.needs_rebuild("a", "b"));
    }

    #[test]
    fn a_record_round_trips_through_json_without_any_secret() {
        let mut record = record();
        record.installer_vm = Some(VmRef {
            backend: "utm".to_string(),
            id: "UUID-1".to_string(),
            name: "wg-build-kali-custom".to_string(),
        });
        record.set_phase(BuildPhase::AwaitingInstall);
        record.verification.push(VerificationResult {
            command: "python3 --version".to_string(),
            exit_code: 0,
        });

        let json = serde_json::to_string_pretty(&record).unwrap();
        assert!(!json.contains("s3cr3t"), "no secret may reach a record");
        let back: BuildRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }
}
