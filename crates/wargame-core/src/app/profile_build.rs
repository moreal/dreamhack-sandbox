//! `profile init | create | console | build`: the reusable-image use cases
//! (spec.md §3 FR-2/FR-3, §4, §5 items 4-5).
//!
//! Two layers live here, deliberately separated:
//!
//! * [`run_build_flow`] is the **common build flow** in spec.md §5 item 4's
//!   order -- prepare → clone → boot → connect → provision → verify →
//!   graceful shutdown → publish. It talks only to a
//!   [`ProfileImageBuilder`] and a [`GuestTransport`], so the Tart and UTM
//!   adapters share every step that is not backend-specific, and neither of
//!   them knows what a build record is.
//! * The `init`/`create`/`console`/`build` functions own the **build record**
//!   state machine around that flow: what a re-run reuses, what a failure
//!   preserves, and when a profile may finally be called ready. Nothing here
//!   calls `utmctl`, `tart` or AppleScript -- that is what the port is for
//!   (spec.md §5 item 3).
//!
//! The contract the flow exists to enforce: an image becomes launchable only
//! after every verification passed *and* the guest shut down cleanly *and*
//! the image was published; a failure never takes the previous good image
//! away.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::domain::build::{
    profile_hash, scripts_hash, BuildPhase, BuildRecord, BuildSource, BuildStage, InstallerMedia,
    PublishedImage, VerificationResult,
};
use crate::domain::compat::Backend;
use crate::domain::{GuestOs, GuestShell, Profile, VmRef};
use crate::error::{Result, WargameError};
use crate::infra::GuestOps;
use crate::ports::image_builder::{
    BuildObserver, BuildRequest, EjectOutcome, GuestLink, ImageRegistrar, InstallerRequest,
    Progress,
};
use crate::ports::{BuildRecordRepository, ImageRef, ProfileImageBuilder, VmProvider};
use crate::security::hash::sha256_file;

use super::backends::resolve_backend;
use super::AppContext;

const GIB: u64 = 1024 * 1024 * 1024;

/// "You asked for the wrong thing, and here is the right thing" -- a
/// refusal whose *message* already names the next command.
///
/// Deliberately not [`WargameError::InvalidProfile`]: that variant's hint
/// sends the user to the provisioning log, which is exactly where the answer
/// is *not* when a profile is fine and the command simply does not apply to
/// it (spec.md §6 wants the recovery step, not a wrong one).
pub(super) fn refuse(profile: &str, reason: impl std::fmt::Display) -> WargameError {
    WargameError::Other(format!("profile {profile}: {reason}"))
}

/// The exit code a provisioning script uses to say "reboot me, then carry
/// on with the next script" -- the Windows Update/MSI convention, reused for
/// every guest so one profile can mix scripts that need it (spec.md §3 FR-3:
/// "설치 중 재부팅 필요 상태를 처리하고 다시 연결한다").
pub const REBOOT_REQUIRED_EXIT_CODE: i32 = 3010;

/// Every wired [`ProfileImageBuilder`], keyed by backend name -- the build
/// counterpart of [`super::BackendRegistry`].
#[derive(Clone, Default)]
pub struct BuilderRegistry {
    builders: BTreeMap<String, Arc<dyn ProfileImageBuilder>>,
    order: Vec<String>,
}

impl BuilderRegistry {
    pub fn new(builders: Vec<Arc<dyn ProfileImageBuilder>>) -> Self {
        let mut registry = BuilderRegistry::default();
        for builder in builders {
            let name = builder.backend().to_string();
            if !registry.builders.contains_key(&name) {
                registry.order.push(name.clone());
            }
            registry.builders.insert(name, builder);
        }
        registry
    }

    pub fn get(&self, backend: &str) -> Result<Arc<dyn ProfileImageBuilder>> {
        self.builders
            .get(backend)
            .cloned()
            .ok_or_else(|| WargameError::BackendUnavailable {
                backend: backend.to_string(),
                reason: format!(
                    "no {backend:?} image builder is wired into this build (available: {})",
                    if self.order.is_empty() {
                        "none".to_string()
                    } else {
                        self.order.join(", ")
                    }
                ),
            })
    }

    pub fn names(&self) -> Vec<String> {
        self.order.clone()
    }
}

impl std::fmt::Debug for BuilderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuilderRegistry")
            .field("backends", &self.order)
            .finish()
    }
}

// --- profile init ---------------------------------------------------------

/// What [`init`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    pub profile: Profile,
    /// The new `<name>.yaml`.
    pub yaml_path: PathBuf,
    /// The provisioning scripts copied for the new profile, in profile order.
    pub scripts: Vec<PathBuf>,
}

/// Copies a template profile under a new name (spec.md §4's `profile init
/// kali-x86_64 --name kali-custom`).
///
/// The copy gets its **own** scripts directory (`scripts/<new name>/`) and
/// its `provision.scripts` are rewritten to point at it, so the user's edits
/// are never overwritten when a new binary re-seeds the built-in scripts
/// (spec.md §3 FR-3's "사용자 편집 프로필·사용자 스크립트를 내장 파일 배포
/// 과정에서 덮어쓰지 않는다").
pub async fn init(ctx: &AppContext, template: &str, new_name: &str) -> Result<InitOutcome> {
    let new_name = new_name.trim();
    if new_name.is_empty()
        || !new_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(refuse(
            new_name,
            "a profile name may only contain letters, digits, '-', '_' and '.'",
        ));
    }
    if new_name == template {
        return Err(refuse(
            new_name,
            format!("{template} is the template; pick a different --name"),
        ));
    }
    if ctx.profiles.get(new_name).is_ok() {
        return Err(refuse(
            new_name,
            "a profile with this name already exists; pick another --name or edit the existing one",
        ));
    }

    let template_profile = ctx.profiles.get(template)?;
    let yaml = ctx.profiles.yaml_for(template)?;
    let profiles_dir = ctx.profiles.user_dir().to_path_buf();
    let yaml_path = profiles_dir.join(format!("{new_name}.yaml"));
    if yaml_path.exists() {
        return Err(refuse(
            new_name,
            format!("{} already exists", yaml_path.display()),
        ));
    }

    // Work the whole copy out in memory first: nothing is written until the
    // result parses as the profile the user asked for, so a template this
    // cannot rewrite leaves no half-made profile behind.
    let mut new_yaml = rename_in_yaml(&yaml, &template_profile.name, new_name)?;
    let mut to_copy = Vec::new();
    for script in &template_profile.provision.scripts {
        let file_name = Path::new(script)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("provision.sh");
        let new_rel = format!("scripts/{new_name}/{file_name}");
        let source = profiles_dir.join(script);
        let contents = std::fs::read(&source).map_err(|e| WargameError::InvalidProfile {
            name: template.to_string(),
            reason: format!(
                "its provisioning script {} is missing ({e}); cannot copy the template",
                source.display()
            ),
        })?;
        new_yaml = repoint_script(&new_yaml, script, &new_rel);
        to_copy.push((profiles_dir.join(&new_rel), contents));
    }

    let profile = Profile::from_yaml(&new_yaml)?;
    if profile.name != new_name {
        return Err(refuse(
            new_name,
            format!(
                "the copied template still calls itself {:?}; nothing was written",
                profile.name
            ),
        ));
    }

    let mut scripts = Vec::new();
    for (dest, contents) in to_copy {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // An existing file here is the user's; the new profile just points
        // at it.
        if !dest.exists() {
            std::fs::write(&dest, contents)?;
        }
        scripts.push(dest);
    }
    std::fs::write(&yaml_path, &new_yaml)?;

    Ok(InitOutcome {
        profile,
        yaml_path,
        scripts,
    })
}

/// Repoints one `provision.scripts` entry at the copy's own script.
///
/// Only the sequence item itself is rewritten -- a whole-document replace
/// would also edit any comment that happens to mention the path, and the
/// template's comments are what make it a template.
fn repoint_script(yaml: &str, old_path: &str, new_path: &str) -> String {
    let mut out = String::with_capacity(yaml.len());
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        let is_entry = trimmed
            .strip_prefix("- ")
            .map(|value| unquote(value.trim()) == old_path)
            .unwrap_or(false);
        if is_entry {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(&format!("{indent}- {new_path}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// A YAML scalar without its surrounding quotes, if it had any.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Replaces `metadata.name` in a profile YAML while leaving every comment
/// and every other line exactly as the template wrote them.
fn rename_in_yaml(yaml: &str, old_name: &str, new_name: &str) -> Result<String> {
    let mut out = String::with_capacity(yaml.len());
    let mut in_metadata = false;
    let mut renamed = false;
    for line in yaml.lines() {
        if !renamed {
            if line.starts_with("metadata:") {
                in_metadata = true;
            } else if in_metadata {
                let trimmed = line.trim_start();
                if trimmed.starts_with("name:") {
                    let indent = &line[..line.len() - trimmed.len()];
                    out.push_str(&format!("{indent}name: {new_name}\n"));
                    renamed = true;
                    continue;
                }
                // A new top-level key ends the metadata block.
                if !line.starts_with(' ') && !line.trim().is_empty() {
                    in_metadata = false;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !renamed {
        return Err(WargameError::InvalidProfile {
            name: old_name.to_string(),
            reason: "its YAML has no metadata.name line to rename".to_string(),
        });
    }
    Ok(out)
}

// --- profile create -------------------------------------------------------

/// Where a new UTM profile's image comes from. Exactly one of spec.md §4's
/// mutually exclusive `--iso` / `--from-vm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateSource {
    Iso {
        path: PathBuf,
        /// A SHA-256 the user vouches for; verified before any VM exists.
        sha256: Option<String>,
    },
    FromVm {
        uuid: String,
    },
}

/// The result of [`create`], including the guidance the CLI/TUI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutcome {
    pub record: BuildRecord,
    /// True when an installation VM already existed for this source and was
    /// reused rather than created again (spec.md §3 FR-2: "같은 명령을 다시
    /// 실행하면 설치 VM을 재사용·안내하고 중복 VM/디스크를 만들지 않는다").
    pub reused: bool,
    pub guidance: Vec<String>,
}

/// Creates (or re-uses) the installation VM for a profile.
pub async fn create(
    ctx: &AppContext,
    profile_name: &str,
    source: CreateSource,
) -> Result<CreateOutcome> {
    let profile = ctx.profiles.get(profile_name)?;
    let execution = resolve_backend(&profile)?;
    let builder = ctx.builders.get(execution.backend.as_str())?;
    if !builder.uses_installer_vm() {
        return Err(refuse(
            &profile.name,
            format!(
                "{} profiles build from `image.base`, not from an installation VM. `profile create` is for UTM profiles that install an OS from an ISO or clone an installed VM; run `wargame profile build {}` instead",
                execution.backend, profile.name
            ),
        ));
    }

    // Everything that can be judged from data alone happens before any VM
    // is created or cloned (spec.md §6: "불가 조합·ISO 없음·해시 불일치 →
    // VM/세션 생성 전 실패").
    let source = validate_source(source)?;

    if let Some(existing) = ctx.builds.load(&profile.name).await? {
        if !existing.source.same_as(&source) {
            return Err(refuse(
                &profile.name,
                format!(
                    "this profile is already attached to {} (installation VM {}). A profile has exactly one image source; to build from {} instead, make a new profile: `wargame profile init {} --name <new-name>`",
                    existing.source.describe(),
                    existing
                        .installer_vm
                        .as_ref()
                        .map(|vm| format!("{} ({})", vm.name, vm.id))
                        .unwrap_or_else(|| "<none>".to_string()),
                    source.describe(),
                    profile.name,
                ),
            ));
        }
        let guidance = create_guidance(ctx, &profile, &existing, true);
        return Ok(CreateOutcome {
            record: existing,
            reused: true,
            guidance,
        });
    }

    let space = check_free_space(ctx, &profile, execution.backend.as_str())?;

    let name = format!(
        "{}{}",
        ctx.config.vm.build_prefix(execution.backend),
        profile.name
    );
    let vm = builder
        .create_installer_vm(&InstallerRequest {
            profile: &profile,
            name,
            execution: execution.clone(),
            source: &source,
        })
        .await?;

    let mut record = BuildRecord::new(&profile, execution.backend.as_str(), source);
    // An ISO source is attached to the new VM as a removable drive, and the
    // record has to know: the guest boots it in preference to its own disk
    // until something ejects it (D3).
    if matches!(record.source, BuildSource::Iso { .. }) {
        record.installer_media = InstallerMedia::Attached;
    }
    record.installer_vm = Some(vm);
    // The OS still has to be installed (or, for a clone, its SSH and guest
    // agent still have to answer), so the honest phase is the same for both
    // sources: waiting on the guest, not building.
    record.set_phase(BuildPhase::AwaitingInstall);
    ctx.builds.save(&record).await?;

    let mut guidance = create_guidance(ctx, &profile, &record, false);
    guidance.splice(1..1, space.warning);
    Ok(CreateOutcome {
        record,
        reused: false,
        guidance,
    })
}

/// The least free space a guest install can work in at all. Well under any
/// profile's nominal disk, because that number is not what gets allocated:
/// a UTM guest disk is a sparse qcow2 -- a fresh one occupies nothing and a
/// finished minimal Kali install about 2.3 GiB of a nominal 20
/// (task-7-phase-a-report.md §5 D1). Refusing on the nominal size blocked
/// both shipped Kali profiles on any host with less than 80 GiB free, for
/// VMs that would have used single-digit GiB.
///
/// 8 GiB is "enough to install a Linux guest and provision it, with room to
/// notice before the volume fills".
pub const MIN_FREE_GIB: u64 = 8;

/// What [`check_free_space`] found.
struct SpaceCheck {
    /// The one line to show the user when there is less room than the
    /// profile's nominal disk. Empty otherwise.
    warning: Vec<String>,
}

/// Refuses a create the host has no room for at all, and warns about one it
/// merely has less room for than the profile asks.
///
/// Measures the volume the *backend* writes disks to, which for UTM is its
/// own sandbox container rather than `$WARGAME_HOME`. If that directory's
/// free space cannot be read (it is `drwx------` inside an app sandbox), the
/// wargame home is measured instead rather than refusing to create
/// anything.
fn check_free_space(ctx: &AppContext, profile: &Profile, backend: &str) -> Result<SpaceCheck> {
    let provider = ctx.backends.get(backend)?;
    let (storage, free) = free_space_for(ctx, provider.as_ref())?;
    let nominal = profile.resources.disk_gib.saturating_mul(GIB);
    let floor = MIN_FREE_GIB * GIB;

    if free < floor {
        return Err(WargameError::InsufficientResources {
            detail: format!(
                "only {:.1} GiB is free on the volume that holds {}, and an OS install needs at least {MIN_FREE_GIB} GiB there",
                free as f64 / GIB as f64,
                storage.display(),
            ),
            hint: format!(
                "Free space on that volume, then run this again.
A guest disk grows as it is used, so profile {:?}'s `disk_gib` is a ceiling rather than a reservation -- lowering it in {} will not help until the volume itself has room.",
                profile.name,
                ctx.profiles.user_dir().join(format!("{}.yaml", profile.name)).display(),
            ),
        });
    }

    let warning = if free < nominal {
        vec![format!(
                "! only {:.1} GiB is free on the volume that holds {}, less than this profile's {} GiB disk. That is allowed -- the guest disk is sparse and grows as it is used (a finished minimal Kali install is a few GiB) -- but the guest will fail once the volume fills. Lower `resources.disk_gib`, or free space, if that is close.",
                free as f64 / GIB as f64,
                storage.display(),
            profile.resources.disk_gib,
        )]
    } else {
        Vec::new()
    };
    Ok(SpaceCheck { warning })
}

/// The directory whose volume `provider` writes VM disks to, and the bytes
/// free on it.
///
/// Falls back to `$WARGAME_HOME` both when the backend does not know where
/// it stores disks and when that directory cannot be read -- a sandboxed
/// container this process may not stat is not a reason to refuse a create.
pub fn free_space_for(ctx: &AppContext, provider: &dyn VmProvider) -> Result<(PathBuf, u64)> {
    if let Some(storage) = provider.storage_dir() {
        match ctx.disk.free_bytes(&storage) {
            Ok(free) => return Ok((storage, free)),
            Err(e) => tracing::debug!(
                path = %storage.display(),
                error = %e,
                "could not read the backend's storage volume; measuring the wargame home instead"
            ),
        }
    }
    let free = ctx.disk.free_bytes(&ctx.dirs.root)?;
    Ok((ctx.dirs.root.clone(), free))
}

/// Checks an ISO exists, is readable and (if the user gave one) matches its
/// SHA-256. An unverified ISO is never recorded as verified, and the OS/arch
/// inside it is never claimed to be checked (spec.md §3 FR-2).
fn validate_source(source: CreateSource) -> Result<BuildSource> {
    match source {
        CreateSource::FromVm { uuid } => {
            let uuid = uuid.trim().to_string();
            if uuid.is_empty() {
                return Err(WargameError::VmNotFound(
                    "--from-vm needs the source VM's UUID (see `utmctl list`)".to_string(),
                ));
            }
            Ok(BuildSource::UtmVm { uuid })
        }
        CreateSource::Iso { path, sha256 } => {
            let path = std::fs::canonicalize(&path).map_err(|e| {
                WargameError::Other(format!(
                    "cannot read the installer ISO {}: {e}",
                    path.display()
                ))
            })?;
            if !path.is_file() {
                return Err(WargameError::Other(format!(
                    "the installer ISO {} is not a file",
                    path.display()
                )));
            }
            std::fs::File::open(&path).map_err(|e| {
                WargameError::Other(format!(
                    "cannot read the installer ISO {}: {e}",
                    path.display()
                ))
            })?;

            let mut verified = false;
            if let Some(expected) = sha256.as_deref() {
                let expected = expected.trim().to_ascii_lowercase();
                let (actual, _size) = sha256_file(&path)?;
                if actual != expected {
                    return Err(WargameError::HashOrWrite(format!(
                        "the installer ISO {} does not match --sha256:\n  expected {expected}\n  actual   {actual}\nNothing was created.",
                        path.display()
                    )));
                }
                verified = true;
            }
            Ok(BuildSource::Iso {
                path,
                sha256: sha256.map(|s| s.trim().to_ascii_lowercase()),
                verified_sha256: verified,
            })
        }
    }
}

/// What the user has to do next, printed by `profile create` on both a first
/// run and a re-run (spec.md §3 FR-2's "설치용 UUID, 콘솔 여는 방법, OS 설치 후
/// SSH/guest tools 준비, build 재개 명령을 출력한다").
fn create_guidance(
    ctx: &AppContext,
    profile: &Profile,
    record: &BuildRecord,
    reused: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(vm) = &record.installer_vm {
        lines.push(if reused {
            format!(
                "Re-using the installation VM {} ({}) this profile already owns; nothing was created.",
                vm.name, vm.id
            )
        } else {
            format!("Installation VM {} ({}) created.", vm.name, vm.id)
        });
    }
    lines.push(format!("Source: {}", record.source.describe()));
    if let BuildSource::Iso {
        sha256: Some(_),
        verified_sha256,
        ..
    } = &record.source
    {
        lines.push(if *verified_sha256 {
            "SHA-256 verified against the file.".to_string()
        } else {
            "SHA-256 recorded but NOT verified.".to_string()
        });
    }

    if profile.guest.os == GuestOs::Windows {
        lines.push(
            "Windows 11 needs TPM 2.0 and Secure Boot, and UTM 4.7.5 cannot set them from a script (docs/utm-compatibility.md §6). Before installing, open the VM in UTM > Settings > QEMU and enable \"UEFI Boot\", \"TPM 2.0 Device\" and \"Reset UEFI Variables\"."
                .to_string(),
        );
    }
    lines.push(format!(
        "1. Open the console:  wargame profile console {}",
        profile.name
    ));
    lines.push(match &record.source {
        BuildSource::UtmVm { .. } => {
            "2. The clone already has an OS. Log in through the console and make sure the guest is ready (below).".to_string()
        }
        _ => "2. Install the OS in that window. Create the account below; the installer's user must match the profile.".to_string(),
    });
    lines.push(format!(
        "3. In the guest, prepare: user {:?}, an SSH server on port 22, and the QEMU guest agent (UTM reads the guest's address from it -- without it the build cannot find the VM).",
        profile.guest.user
    ));
    if profile.guest.os == GuestOs::Windows {
        lines.push(
            "   On Windows that is OpenSSH Server (Settings > Optional features) plus PowerShell as the SSH shell, and the UTM guest tools for the agent."
                .to_string(),
        );
    } else {
        lines.push(
            "   On Debian/Kali: `sudo apt install openssh-server qemu-guest-agent && sudo systemctl enable --now ssh qemu-guest-agent`."
                .to_string(),
        );
        // Kali's guest-agent unit has no [Install] section (it is activated
        // by udev), so the obvious check reports something that looks like
        // a failure and is not (task-7-phase-a-report.md §5 D8).
        lines.push(
            "   To check it afterwards use `systemctl is-active qemu-guest-agent`, not `is-enabled`: on Kali that unit is activated by udev and answers `static`, which is correct."
                .to_string(),
        );
    }
    // The baseline provisioning scripts need more than an account and a key,
    // and refuse without it. Asking here costs a paste; finding out at the
    // first `profile build` costs an install cycle (spec.md §6's "OS 설치
    // 미완료: 설치·SSH 준비·build 재개 명령 안내").
    lines.extend(privilege_guidance(profile));
    lines.push(format!(
        "5. Authorize this host's key: append {} to ~{}/.ssh/authorized_keys in the guest.",
        ctx.dirs.keys_dir().join("id_ed25519.pub").display(),
        profile.guest.user,
    ));
    lines.push(format!(
        "6. Then build the reusable image:  wargame profile build {}",
        profile.name
    ));
    // The defaults are deliberately left alone -- lowering them silently
    // would change what every existing profile builds -- but two installs
    // at once on one host is what made phase A's emulated guest crawl, and
    // that is worth one line here (task-7-phase-a-report.md §5 D8).
    lines.push(format!(
        "This profile asks for {} vCPU. An emulated guest uses every core it is given, so an install or build running at the same time as another one on this host will make both crawl -- do them one at a time, or lower `resources.cpu` in the profile.",
        profile.resources.cpu
    ));
    lines
}

/// Step 4 of [`create_guidance`]: the elevated access `profile build` needs
/// in the guest, phrased for the guest's OS with the profile's own user
/// already substituted.
///
/// A POSIX build runs `apt-get` through `sudo -n`, which cannot answer a
/// password prompt over a non-interactive SSH command; a Windows build
/// installs a Windows capability and writes the machine PATH, both of which
/// need the elevated token OpenSSH grants a local administrator.
fn privilege_guidance(profile: &Profile) -> Vec<String> {
    let user = &profile.guest.user;
    match profile.guest.os {
        GuestOs::Windows => vec![format!(
            "4. Make {user:?} a local administrator (Computer Management > Local Users and Groups > Groups > Administrators). The build installs a Windows capability and edits the machine PATH, so an unelevated account is refused."
        )],
        _ => vec![
            format!(
                "4. Give {user:?} passwordless sudo -- the build installs packages over a non-interactive SSH command, which cannot answer a password prompt. In the guest:"
            ),
            format!("     echo '{user} ALL=(ALL) NOPASSWD:ALL' | sudo tee /etc/sudoers.d/wargame"),
            "     sudo chmod 0440 /etc/sudoers.d/wargame".to_string(),
        ],
    }
}

// --- profile console ------------------------------------------------------

/// What [`console`] opened, and what to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleOutcome {
    pub vm: VmRef,
    /// Printed under the "opened" line: this verb *starts* the VM and
    /// leaves it running, which nothing used to say (D4).
    pub guidance: Vec<String>,
}

/// Opens the installation VM's window (spec.md §4's `profile console`).
pub async fn console(ctx: &AppContext, profile_name: &str) -> Result<ConsoleOutcome> {
    let profile = ctx.profiles.get(profile_name)?;
    let execution = resolve_backend(&profile)?;
    let builder = ctx.builders.get(execution.backend.as_str())?;
    if !builder.uses_installer_vm() {
        return Err(refuse(
            &profile.name,
            format!(
                "`profile console` opens the installation VM of a UTM profile; {} profiles build from `image.base` and have no installation VM. Use `wargame gui <session>` for a session's console",
                execution.backend
            ),
        ));
    }
    let record = require_record(ctx, &profile).await?;
    let vm = record
        .installer_vm
        .clone()
        .ok_or_else(|| no_installer_vm(&profile.name))?;
    builder.open_console(&vm).await?;
    Ok(ConsoleOutcome {
        guidance: console_guidance(&profile.name, &record),
        vm,
    })
}

/// What to say after the console window is up.
///
/// It has to say that the VM is now *running*, because it is: `profile
/// console` starts it, and before this the CLI printed only "Opened",
/// leaving the user with no wargame verb that could stop it again
/// (task-7-phase-a-report.md §5 D4).
fn console_guidance(profile: &str, record: &BuildRecord) -> Vec<String> {
    let mut lines = vec![format!(
        "The installation VM is now running, and stays up until you stop it: wargame profile stop {profile}"
    )];
    if record.installer_media != InstallerMedia::Detached {
        lines.push(format!(
            "Once the OS is installed, take the installer disc out -- otherwise the machine boots the installer again, not the system you just installed:  wargame profile eject {profile}"
        ));
    }
    lines.push(format!(
        "When the guest is ready (SSH, guest agent, the host key), build the image:  wargame profile build {profile}"
    ));
    lines
}

async fn require_record(ctx: &AppContext, profile: &Profile) -> Result<BuildRecord> {
    ctx.builds
        .load(&profile.name)
        .await?
        .ok_or_else(|| no_installer_vm(&profile.name))
}

pub(super) fn no_installer_vm(profile: &str) -> WargameError {
    WargameError::ImageNotBuilt {
        profile: profile.to_string(),
        detail: "it has no installation VM yet".to_string(),
        hint: format!("wargame profile create {profile} --iso <path>   # or --from-vm <utm-uuid>"),
    }
}

// --- profile build --------------------------------------------------------

/// Builds (or rebuilds) a profile's reusable image and updates its build
/// record. Returns the record as it was saved.
pub async fn build(
    ctx: &AppContext,
    profile_name: &str,
    progress: Progress<'_>,
) -> Result<BuildRecord> {
    let profile = ctx.profiles.get(profile_name)?;
    let execution = resolve_backend(&profile)?;
    let builder = ctx.builders.get(execution.backend.as_str())?;

    // A guest whose shell has no wired transport could never be
    // provisioned, so refuse before the VM is touched at all rather than
    // half-provisioning it (spec.md §3 FR-4).
    ctx.transports
        .for_shell(GuestShell::for_os(profile.guest.os))
        .map_err(|e| {
            refuse(
                &profile.name,
                format!(
                    "its guest is driven through {}, and {e}",
                    GuestShell::for_os(profile.guest.os)
                ),
            )
        })?;

    let record = match ctx.builds.load(&profile.name).await? {
        Some(record) => record,
        None if builder.uses_installer_vm() => return Err(no_installer_vm(&profile.name)),
        // Tart needs no `profile create`: its source is `image.base`.
        None => BuildRecord::new(
            &profile,
            execution.backend.as_str(),
            BuildSource::TartBase {
                image: profile.image.base.clone(),
            },
        ),
    };
    if record.backend != execution.backend.as_str() {
        return Err(refuse(
            &profile.name,
            format!(
                "its build record was made for the {} backend but the profile now resolves to {}. Make a new profile (`wargame profile init <template> --name <new>`) rather than re-pointing this one",
                record.backend, execution.backend
            ),
        ));
    }

    let scripts_dir = ctx.profiles.user_dir().to_path_buf();
    let new_profile_hash = profile_hash(&profile);
    let new_scripts_hash = scripts_hash(&scripts_dir, &profile.provision.scripts);

    let snapshot = record.clone();
    let request = BuildRequest {
        profile: &profile,
        record: &snapshot,
        execution: execution.clone(),
        scripts_dir,
        log_path: ctx.dirs.logs_dir().join("profile-build.log"),
        boot_timeout: ctx
            .config
            .vm
            .boot_timeout(execution.backend, execution.mode),
    };

    // The flow persists the phase it reaches, so a build killed halfway
    // leaves an accurate record rather than the phase of the previous run.
    let phases = RecordPhases::new(ctx.builds.clone(), record);
    let outcome = run_build_flow(builder.as_ref(), &request, progress, &phases, &phases).await;
    let mut record = phases.into_record();

    match outcome {
        Ok(outcome) => {
            record.verification = outcome.verification;
            for failure in outcome.cleanup_failures {
                record.record_cleanup_failure(failure);
            }
            if builder.uses_installer_vm() {
                record.installer_vm = Some(outcome.vm);
            }
            if outcome.media.is_detached() {
                record.installer_media = InstallerMedia::Detached;
            }
            // A run that died between recording a new image and deleting the
            // one it replaced left that image behind and still named here.
            // It exists, so it stays tracked rather than being overwritten
            // by this build's own replacement.
            if let Some(stale) = record.previous_image.take() {
                record.track_leaked_image(stale);
            }
            // The new image is written down *before* the old one is deleted:
            // a crash in between leaves two images, never none (FR-3).
            //
            // A backend whose image id is stable across builds (Tart's image
            // *is* `wargame-profile-<name>`) republishes the same id, and
            // then there is no old image to delete -- deleting it would
            // delete the one just built.
            let replaced = record.image.replace(outcome.image.clone());
            record.previous_image = replaced.filter(|old| old.id != outcome.image.id);
            record.profile_hash = new_profile_hash;
            record.scripts_hash = new_scripts_hash;
            record.last_error = None;
            record.set_phase(BuildPhase::Ready);
            ctx.builds.save(&record).await?;

            discard_replaced_images(ctx, builder.as_ref(), &mut record).await?;
            Ok(record)
        }
        Err(failure) => {
            if !failure.verification.is_empty() {
                record.verification = failure.verification;
            }
            for cleanup_failure in failure.cleanup_failures {
                record.record_cleanup_failure(cleanup_failure);
            }
            if builder.uses_installer_vm() {
                if let Some(vm) = failure.vm {
                    record.installer_vm = Some(vm);
                }
            }
            // The eject happens before the boot, so a build that failed
            // later still left the CD out; the record must say so or the
            // next run has no idea what state the VM is in.
            if failure.media.as_ref().map(EjectOutcome::is_detached) == Some(true) {
                record.installer_media = InstallerMedia::Detached;
            }
            if failure.awaiting_install {
                // Not a failed build: the guest simply is not installed or
                // reachable yet, so the profile goes back to waiting rather
                // than being marked broken (spec.md §6's "OS 설치 미완료" row).
                record.set_phase(BuildPhase::AwaitingInstall);
                record.last_error = Some(failure.error.to_string());
            } else {
                record.set_phase(BuildPhase::Failed {
                    stage: failure.stage,
                    error: failure.error.to_string(),
                });
            }
            ctx.builds.save(&record).await?;
            Err(failure.error)
        }
    }
}

/// Deletes the image this build replaced, and retries any earlier deletion
/// that failed.
///
/// Only ever called after the record naming the *new* image was saved, so a
/// failure here can lose nothing. An image that still cannot be deleted
/// stays tracked in `leaked_images`: it exists on the backend, so `prune`
/// must keep skipping it, and the next build tries again.
async fn discard_replaced_images(
    ctx: &AppContext,
    builder: &dyn ProfileImageBuilder,
    record: &mut BuildRecord,
) -> Result<()> {
    let mut pending: Vec<PublishedImage> = record.previous_image.take().into_iter().collect();
    pending.append(&mut record.leaked_images);
    if pending.is_empty() {
        return Ok(());
    }

    let published = record.image.as_ref().map(|image| image.id.clone());
    let mut leaked: Vec<PublishedImage> = Vec::new();
    for image in pending {
        // Never delete what the record currently points at, whatever the
        // bookkeeping says.
        if published.as_deref() == Some(image.id.as_str()) {
            continue;
        }
        // The same image can be named twice (a stale `previous_image` that
        // was also tracked as leaked); deleting it once is enough.
        if leaked.iter().any(|done| done.id == image.id) {
            continue;
        }
        if let Err(e) = builder.discard_image(&image).await {
            record.record_cleanup_failure(format!(
                "could not delete the replaced image {} ({}): {e}",
                image.name, image.id
            ));
            leaked.push(image);
        }
    }
    record.leaked_images = leaked;
    ctx.builds.save(record).await
}

/// The [`BuildObserver`] that writes each stage the flow reaches into the
/// build record (spec.md §3 FR-3's "단계 ... 를 로그에 남긴다", and what makes
/// a half-finished build report where it actually stopped).
///
/// It owns the record while the flow runs -- the flow is the only thing
/// touching it then -- and hands it back afterwards.
struct RecordPhases {
    builds: Arc<dyn BuildRecordRepository>,
    record: Mutex<BuildRecord>,
}

impl RecordPhases {
    fn new(builds: Arc<dyn BuildRecordRepository>, record: BuildRecord) -> Self {
        RecordPhases {
            builds,
            record: Mutex::new(record),
        }
    }

    fn into_record(self) -> BuildRecord {
        self.record.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl BuildObserver for RecordPhases {
    async fn stage_started(&self, stage: BuildStage) -> Result<()> {
        // The stages before the guest answers say nothing a record should
        // claim: a UTM profile is still `awaiting-install` until it does,
        // and a rebuild's previous image is still the launchable one.
        let phase = match stage {
            BuildStage::Provisioning => BuildPhase::Provisioning,
            BuildStage::Verifying => BuildPhase::Verifying,
            BuildStage::Publishing => BuildPhase::Publishing,
            BuildStage::Preparing
            | BuildStage::Cloning
            | BuildStage::Booting
            | BuildStage::Connecting => return Ok(()),
        };
        let snapshot = {
            let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
            record.set_phase(phase);
            record.clone()
        };
        self.builds.save(&snapshot).await
    }
}

/// The publish step's new VMs land in the record as **leaked images** the
/// instant the backend knows their ids, and are saved before the publish
/// carries on.
///
/// That looks backwards for the one that is about to become the profile's
/// image, and is deliberate: a build interrupted between "UTM registered a
/// copy" and "the record names it as the image" would otherwise leave a
/// virtual machine nothing knows about -- which is exactly what a failed
/// publish did (task-7-b2b-report.md D5). Carried as leaked, it is skipped
/// by `prune` and deleted by the next successful build; and when this build
/// does succeed, `discard_replaced_images` drops the new image from that
/// list rather than deleting it, because the record now points at it.
#[async_trait]
impl ImageRegistrar for RecordPhases {
    async fn image_created(&self, image: &PublishedImage) -> Result<()> {
        let snapshot = {
            let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
            record.track_leaked_image(image.clone());
            record.clone()
        };
        self.builds.save(&snapshot).await
    }
}

// --- image readiness (launch / doctor / profiles) -------------------------

/// What a profile has to launch from right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileImageStatus {
    pub backend: Backend,
    /// The image `launch` would clone. `None` when nothing is published.
    pub image: Option<ImageRef>,
    /// The build record's phase, or `None` when the profile was never
    /// `profile create`d / built.
    pub phase: Option<BuildPhase>,
    /// The profile or its scripts changed since this image was built.
    pub rebuild_needed: bool,
    /// One line for `doctor`/`profiles`.
    pub detail: String,
}

/// Reads a profile's build record and says what it has to launch from.
///
/// Tart keeps its images in Tart's own store, so its image name is always
/// the answer and the backend's `image_exists` is what decides; UTM has no
/// registry at all, so the record *is* the registry (spec.md §5 item 5).
pub async fn image_status(ctx: &AppContext, profile: &Profile) -> Result<ProfileImageStatus> {
    let execution = resolve_backend(profile)?;
    let record = ctx.builds.load(&profile.name).await?;
    let rebuild_needed = record
        .as_ref()
        .map(|r| {
            r.needs_rebuild(
                &profile_hash(profile),
                &scripts_hash(ctx.profiles.user_dir(), &profile.provision.scripts),
            )
        })
        .unwrap_or(false);

    let (image, detail) = match execution.backend {
        Backend::Tart => (Some(ImageRef(profile.image_name())), profile.image_name()),
        Backend::Utm => match record.as_ref().and_then(|r| r.ready_image()) {
            Some(published) => (
                Some(ImageRef(published.id.clone())),
                format!("{} ({})", published.name, published.id),
            ),
            None => (
                None,
                match record.as_ref().map(|r| &r.phase) {
                    Some(phase) => format!("not published yet ({phase})"),
                    None => "no build record: `wargame profile create` has not run".to_string(),
                },
            ),
        },
    };

    Ok(ProfileImageStatus {
        backend: execution.backend,
        image,
        phase: record.map(|r| r.phase),
        rebuild_needed,
        detail: if rebuild_needed {
            format!("{detail} (rebuild needed: the profile or its scripts changed)")
        } else {
            detail
        },
    })
}

/// The image a launch of `profile` must clone from, or an error that names
/// the command which would produce one.
pub async fn launch_image(ctx: &AppContext, profile: &Profile) -> Result<ImageRef> {
    let status = image_status(ctx, profile).await?;
    status.image.ok_or_else(|| WargameError::ImageNotBuilt {
        profile: profile.name.clone(),
        detail: status.detail,
        hint: match status.phase {
            None => format!(
                "wargame profile create {} --iso <path>   # then: wargame profile build {0}",
                profile.name
            ),
            Some(_) => format!("wargame profile build {}", profile.name),
        },
    })
}

// --- the common build flow ------------------------------------------------

/// What a finished [`run_build_flow`] produced.
pub struct BuildFlowOutcome {
    /// The VM that was provisioned (the installation VM, or Tart's clone).
    pub vm: VmRef,
    pub image: PublishedImage,
    pub verification: Vec<VerificationResult>,
    pub cleanup_failures: Vec<String>,
    /// What the eject step did to the installation VM's installer media.
    pub media: EjectOutcome,
}

/// Where a build stopped and what it left behind.
pub struct BuildFlowError {
    pub stage: BuildStage,
    /// The VM the flow was working on, once it exists.
    pub vm: Option<VmRef>,
    pub error: WargameError,
    pub verification: Vec<VerificationResult>,
    pub cleanup_failures: Vec<String>,
    /// The guest was never reachable on a backend that installs its OS by
    /// hand: a wait, not a failure.
    pub awaiting_install: bool,
    /// What the eject step did before the failure, when it got that far.
    /// The CD is out whatever happens afterwards, and the record has to say
    /// so or the next run cannot tell.
    pub media: Option<EjectOutcome>,
}

impl BuildFlowError {
    fn new(stage: BuildStage, error: WargameError) -> Self {
        BuildFlowError {
            stage,
            vm: None,
            error,
            verification: Vec::new(),
            cleanup_failures: Vec::new(),
            awaiting_install: false,
            media: None,
        }
    }
}

/// spec.md §5 item 4's order, once, for every backend:
/// prepare → clone → boot → connect → provision → verify → graceful
/// shutdown → publish.
///
/// Provisioning and verification are the same everywhere (copy the profile's
/// scripts into the guest, run them, run `tools.verify`), so they live here;
/// everything that differs is a [`ProfileImageBuilder`] call.
pub async fn run_build_flow(
    builder: &dyn ProfileImageBuilder,
    req: &BuildRequest<'_>,
    progress: Progress<'_>,
    observer: &dyn BuildObserver,
    registrar: &dyn ImageRegistrar,
) -> std::result::Result<BuildFlowOutcome, BuildFlowError> {
    let profile = req.profile;

    stage(observer, BuildStage::Preparing).await?;
    progress(format!("preparing the {} build", builder.backend()));
    builder
        .prepare(req, progress)
        .await
        .map_err(|e| BuildFlowError::new(BuildStage::Preparing, e))?;

    stage(observer, BuildStage::Cloning).await?;
    let vm = builder
        .clone_for_build(req, progress)
        .await
        .map_err(|e| BuildFlowError::new(BuildStage::Cloning, e))?;

    // Before the boot, not after it: an installed guest whose installer CD
    // is still attached boots the CD, so the build would wait out its whole
    // boot timeout and the image it published would carry the CD too
    // (task-7-phase-a-report.md §5 D3). Idempotent, so a rebuild is free.
    //
    // Except on a VM this app never put media into. `profile create
    // --from-vm` copies a VM the *user* installed: whatever removable
    // drives it has are theirs, put there for their own reasons, and
    // "detach every removable drive" is not something to do to it uninvited.
    let media = if matches!(req.record.source, BuildSource::UtmVm { .. }) {
        progress(format!(
            "{} was cloned from a VM you already had, so wargame attached no installer media to it and detaches nothing",
            vm.name
        ));
        EjectOutcome::NotApplicable
    } else {
        eject_installer_media(builder, req, &vm, progress).await?
    };

    // From here every failure cleans up after itself before propagating.
    let outcome = build_from(builder, req, &vm, progress, observer, registrar).await;
    match outcome {
        Ok(mut outcome) => {
            outcome.media = media;
            Ok(outcome)
        }
        Err(mut failure) => {
            failure.media = Some(media);
            progress("build failed; cleaning up".to_string());
            failure
                .cleanup_failures
                .extend(builder.cleanup_failed_build(req, &vm).await);
            failure.vm = Some(vm);
            failure.awaiting_install =
                builder.uses_installer_vm() && failure.stage == BuildStage::Connecting;
            if failure.awaiting_install {
                progress(format!(
                    "the guest never answered. Finish the OS install and the guest setup through `wargame profile console {}`, then run `wargame profile build {0}` again.",
                    profile.name
                ));
            }
            Err(failure)
        }
    }
}

/// Steps 3-8, split out of [`run_build_flow`] so every failure after the
/// clone gets the same cleanup.
async fn build_from(
    builder: &dyn ProfileImageBuilder,
    req: &BuildRequest<'_>,
    vm: &VmRef,
    progress: Progress<'_>,
    observer: &dyn BuildObserver,
    registrar: &dyn ImageRegistrar,
) -> std::result::Result<BuildFlowOutcome, BuildFlowError> {
    stage(observer, BuildStage::Booting).await?;
    builder
        .boot(req, vm, progress)
        .await
        .map_err(|e| BuildFlowError::new(BuildStage::Booting, e))?;

    stage(observer, BuildStage::Connecting).await?;
    let link = builder
        .connect(req, vm, progress)
        .await
        .map_err(|e| BuildFlowError::new(BuildStage::Connecting, e))?;

    stage(observer, BuildStage::Provisioning).await?;
    provision(req, &link, progress)
        .await
        .map_err(|e| BuildFlowError::new(BuildStage::Provisioning, e))?;

    stage(observer, BuildStage::Verifying).await?;
    let verification = verify(req, &link, progress).await.map_err(|(results, e)| {
        let mut failure = BuildFlowError::new(BuildStage::Verifying, e);
        failure.verification = results;
        failure
    })?;

    // Everything from here is "make the image": a guest that does not stop
    // cleanly must never be published (DESIGN.md §5).
    stage(observer, BuildStage::Publishing).await?;
    flush_guest(&link, progress).await;
    builder.shutdown(req, vm, progress).await.map_err(|e| {
        let mut failure = BuildFlowError::new(BuildStage::Publishing, e);
        failure.verification = verification.clone();
        failure
    })?;

    let image = builder
        .publish(req, vm, progress, registrar)
        .await
        .map_err(|e| {
            let mut failure = BuildFlowError::new(BuildStage::Publishing, e);
            failure.verification = verification.clone();
            failure
        })?;

    Ok(BuildFlowOutcome {
        vm: vm.clone(),
        image,
        verification,
        cleanup_failures: Vec::new(),
        // Filled in by `run_build_flow`, which owns the eject step.
        media: EjectOutcome::NotApplicable,
    })
}

/// The eject step, with its two outcomes that are *not* failures: a backend
/// with no installation media, and a VM that is not stopped (its
/// configuration cannot be written then, and the answer is a recovery step
/// rather than a dead build).
async fn eject_installer_media(
    builder: &dyn ProfileImageBuilder,
    req: &BuildRequest<'_>,
    vm: &VmRef,
    progress: Progress<'_>,
) -> std::result::Result<EjectOutcome, BuildFlowError> {
    let media = builder
        .eject_installer_media(vm, progress)
        .await
        .map_err(|e| {
            // Attributed to the boot: this is what makes the boot land in
            // the installed system rather than in the installer.
            let mut failure = BuildFlowError::new(BuildStage::Booting, e);
            failure.vm = Some(vm.clone());
            failure
        })?;
    match &media {
        EjectOutcome::Detached(n) => progress(format!(
            "detached the installer media from {} ({n} removable drive{})",
            vm.name,
            if *n == 1 { "" } else { "s" }
        )),
        EjectOutcome::NothingAttached => progress(format!(
            "{} has no installer media attached; nothing to eject",
            vm.name
        )),
        EjectOutcome::Blocked(reason) => progress(format!(
            "leaving the installer media attached: {reason}. Stop it and build again -- `wargame profile stop {0}` then `wargame profile build {0}` -- if this build cannot reach the guest.",
            req.profile.name
        )),
        // Not a failure, and not something stopping the VM would fix: the
        // build carries on and the reason is on the record's own terms
        // (nothing is claimed to have been detached).
        EjectOutcome::EveryDriveRemovable(reason) => progress(format!(
            "refusing to detach anything: {reason}. Take the installer drive out by hand in UTM if the guest boots it instead of its own disk."
        )),
        EjectOutcome::NotApplicable => {}
    }
    Ok(media)
}

/// Announces a stage to the observer, turning a refusal to record it into a
/// failure *at* that stage.
async fn stage(
    observer: &dyn BuildObserver,
    stage: BuildStage,
) -> std::result::Result<(), BuildFlowError> {
    observer
        .stage_started(stage)
        .await
        .map_err(|e| BuildFlowError::new(stage, e))
}

/// Copies the profile's `scripts/` tree into the guest and runs each
/// `provision.scripts` entry there (spec.md §3 FR-3: "설치 스크립트는 반드시
/// 게스트에서 실행한다", "macOS/Kali는 Bash, Windows는 PowerShell로 실행한다").
///
/// The OS decides every command's shape through [`GuestOps`]; nothing here
/// tests the guest's operating system.
async fn provision(req: &BuildRequest<'_>, link: &GuestLink, progress: Progress<'_>) -> Result<()> {
    let profile = req.profile;
    let ops = GuestOps::for_endpoint(&link.endpoint);
    let log_path = provision_log_path(&req.log_path, &profile.name);
    progress(format!("provisioning log: {}", log_path.display()));

    progress("copying provisioning scripts into the guest".to_string());
    // `~` is never expanded in `GuestTransport::exec` argv (every argument
    // is quoted or encoded before being sent to the guest shell), so all
    // guest-side paths are relative to the SSH user's home directory. `scp
    // -r <src>/scripts wg-provision` would, on a guest where `wg-provision`
    // does not already exist, create `wg-provision` *as* a copy of
    // `scripts` -- so make the destination first and copy into it with a
    // trailing separator.
    for command in ops.mkdir_p(&[PROVISION_DIR.to_string()]) {
        command.run(link.transport.as_ref(), &link.endpoint).await?;
    }
    link.transport
        .copy_in(
            &link.endpoint,
            &req.scripts_dir.join("scripts"),
            &ops.dir_dest(PROVISION_DIR),
        )
        .await?;

    for script in &profile.provision.scripts {
        progress(format!("provisioning: {script}"));
        let command = ops.run_script(&ops.join(PROVISION_DIR, script));
        let out = command.run(link.transport.as_ref(), &link.endpoint).await?;
        log_exec(
            &log_path,
            &command.describe(),
            out.status,
            &out.stdout,
            &out.stderr,
        );
        if out.status == REBOOT_REQUIRED_EXIT_CODE {
            progress(format!(
                "{script} asked for a reboot (exit {REBOOT_REQUIRED_EXIT_CODE}); waiting for the guest to come back"
            ));
            wait_for_reboot(req, link, progress).await?;
            continue;
        }
        if out.status != 0 {
            return Err(WargameError::GuestBootstrapFailed(format!(
                "provisioning script {script} failed (exit {}): {}",
                out.status,
                tail_lines(&combined_output(&out.stdout, &out.stderr), 15)
            )));
        }
    }
    Ok(())
}

/// Waits for the guest's SSH to drop and come back after a script asked for
/// a reboot. A guest that reboots faster than the first probe is fine: what
/// matters is that the *next* script runs on a guest that answers.
async fn wait_for_reboot(
    req: &BuildRequest<'_>,
    link: &GuestLink,
    progress: Progress<'_>,
) -> Result<()> {
    let probe = GuestOps::for_endpoint(&link.endpoint).probe();
    let deadline = tokio::time::Instant::now() + REBOOT_DROP_TIMEOUT;
    loop {
        match probe.run(link.transport.as_ref(), &link.endpoint).await {
            Err(_) => break,
            Ok(out) if out.status != 0 => break,
            Ok(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                "the guest still answered {REBOOT_DROP_TIMEOUT:?} after a reboot request; continuing"
            );
            break;
        }
        tokio::time::sleep(REBOOT_POLL_INTERVAL).await;
    }
    progress("the guest went down; waiting for SSH again".to_string());
    link.transport
        .wait_ready(&link.endpoint, req.boot_timeout)
        .await
}

/// Where the profile's `scripts/` tree lands in the guest's home directory.
const PROVISION_DIR: &str = "wg-provision";

/// How long to wait for SSH to actually drop after a reboot-required script.
const REBOOT_DROP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const REBOOT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// Runs `tools.verify` in the guest, recording every command and its exit
/// code. The first failure ends the build (spec.md §6's "도구 검증 실패:
/// 명령·exit code·로그 표시, 이전 이미지 보존").
#[allow(clippy::type_complexity)]
async fn verify(
    req: &BuildRequest<'_>,
    link: &GuestLink,
    progress: Progress<'_>,
) -> std::result::Result<Vec<VerificationResult>, (Vec<VerificationResult>, WargameError)> {
    let profile = req.profile;
    let log_path = provision_log_path(&req.log_path, &profile.name);
    let mut results = Vec::new();
    for check in &profile.tools.verify {
        let argv: Vec<String> = check.split_whitespace().map(str::to_string).collect();
        if argv.is_empty() {
            continue;
        }
        progress(format!("verifying: {check}"));
        let out = match link.transport.exec(&link.endpoint, &argv).await {
            Ok(out) => out,
            Err(e) => return Err((results, e)),
        };
        log_exec(
            &log_path,
            &argv.join(" "),
            out.status,
            &out.stdout,
            &out.stderr,
        );
        results.push(VerificationResult {
            command: check.clone(),
            exit_code: out.status,
        });
        if out.status != 0 {
            let error = WargameError::InvalidProfile {
                name: profile.name.clone(),
                reason: format!(
                    "verify command {check:?} failed (exit {}): {}",
                    out.status,
                    tail_lines(&combined_output(&out.stdout, &out.stderr), 15)
                ),
            };
            return Err((results, error));
        }
    }
    Ok(results)
}

/// Removes the provisioning files and flushes the guest's filesystem before
/// the shutdown. Never fatal: a guest that cannot `sync` still gets a
/// graceful stop, and the stop itself is what the build actually depends on.
async fn flush_guest(link: &GuestLink, progress: Progress<'_>) {
    let ops = GuestOps::for_endpoint(&link.endpoint);
    progress("cleaning up provisioning files".to_string());
    if let Err(e) = ops
        .remove_tree(PROVISION_DIR)
        .run(link.transport.as_ref(), &link.endpoint)
        .await
    {
        tracing::warn!(error = %e, "could not remove {PROVISION_DIR} from the guest");
    }
    progress("flushing guest filesystem and shutting down".to_string());
    match ops
        .flush()
        .run(link.transport.as_ref(), &link.endpoint)
        .await
    {
        Ok(out) if out.status != 0 => {
            tracing::warn!(
                status = out.status,
                stderr = %out.stderr,
                "sync failed on the guest before stop; proceeding anyway"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "could not run sync on the guest before stop; proceeding anyway");
        }
    }
}

/// Where provisioning output for `profile` is logged: `log_path`'s
/// directory (typically `$WARGAME_HOME/logs`) with the file name replaced by
/// `provision-<profile>.log`, so it never clobbers the backend's own console
/// log and survives across builds of different profiles.
pub fn provision_log_path(log_path: &Path, profile: &str) -> PathBuf {
    let dir = log_path.parent().map(Path::to_path_buf).unwrap_or_default();
    dir.join(format!("provision-{profile}.log"))
}

/// Combines stdout and stderr for the "last N lines" shown in an error.
fn combined_output(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    combined.push_str(stdout);
    if !stdout.is_empty() && !stderr.is_empty() && !stdout.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(stderr);
    combined
}

/// The last `n` lines of `text`, for embedding a bounded amount of guest
/// output in an error message.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Appends one command's output to the provisioning log as a
/// `==== <command> (exit N) ====` header followed by its output. Never fails
/// the build: a log write failure is only `tracing::warn`.
fn log_exec(log_path: &Path, command: &str, status: i32, stdout: &str, stderr: &str) {
    let mut body = format!("==== {command} (exit {status}) ====\n");
    for stream in [stdout, stderr] {
        if !stream.is_empty() {
            body.push_str(stream);
            if !stream.ends_with('\n') {
                body.push('\n');
            }
        }
    }

    if let Some(parent) = log_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "failed to create provisioning log directory {}: {err}",
                parent.display()
            );
            return;
        }
    }
    match OpenOptions::new().create(true).append(true).open(log_path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(body.as_bytes()) {
                tracing::warn!(
                    "failed to write provisioning log {}: {err}",
                    log_path.display()
                );
            }
        }
        Err(err) => tracing::warn!(
            "failed to open provisioning log {}: {err}",
            log_path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"apiVersion: wargame.vm/v1
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

    #[test]
    fn renaming_a_template_keeps_every_other_line_including_comments() {
        let renamed = rename_in_yaml(TEMPLATE, "kali-arm64", "kali-custom").unwrap();
        assert!(renamed.contains("  name: kali-custom\n"));
        assert!(renamed.contains("  description: \"Kali template\""));
        assert!(renamed.contains("distribution: kali"));
        let profile = Profile::from_yaml(&renamed).unwrap();
        assert_eq!(profile.name, "kali-custom");
    }

    #[test]
    fn renaming_only_touches_the_metadata_name() {
        // `guest.user` is also a `name`-adjacent scalar; only metadata.name
        // may change, and only once.
        let yaml = TEMPLATE.replace("  user: kali", "  user: kali\n  name: not-metadata");
        let renamed = rename_in_yaml(&yaml, "kali-arm64", "kali-custom").unwrap();
        assert!(renamed.contains("  name: not-metadata"));
        assert_eq!(renamed.matches("kali-custom").count(), 1);
    }

    /// A whole-document replace would rewrite the template's comments too,
    /// which is where the instructions live.
    #[test]
    fn repointing_a_script_only_touches_its_list_entry() {
        let yaml = "# see scripts/kali/common.sh for what runs\nprovision:\n  scripts:\n    - scripts/kali/common.sh\n    - scripts/kali/extra.sh\n";
        let out = repoint_script(yaml, "scripts/kali/common.sh", "scripts/mine/common.sh");
        assert!(
            out.contains("# see scripts/kali/common.sh for what runs"),
            "{out}"
        );
        assert!(out.contains("    - scripts/mine/common.sh"), "{out}");
        assert!(out.contains("    - scripts/kali/extra.sh"), "{out}");
        assert!(!out.contains("    - scripts/kali/common.sh"), "{out}");
    }

    #[test]
    fn repointing_handles_a_quoted_entry() {
        let yaml = "  scripts:\n    - \"scripts/kali/common.sh\"\n";
        let out = repoint_script(yaml, "scripts/kali/common.sh", "scripts/mine/common.sh");
        assert!(out.contains("    - scripts/mine/common.sh"), "{out}");
    }

    #[test]
    fn a_yaml_without_metadata_name_is_refused() {
        assert!(rename_in_yaml("kind: Profile\n", "x", "y").is_err());
    }

    #[test]
    fn the_provision_log_sits_next_to_the_build_log() {
        assert_eq!(
            provision_log_path(Path::new("/home/logs/profile-build.log"), "kali-custom"),
            PathBuf::from("/home/logs/provision-kali-custom.log")
        );
    }

    #[test]
    fn tail_lines_never_panics_on_short_output() {
        assert_eq!(tail_lines("a\nb", 15), "a\nb");
        assert_eq!(tail_lines("", 3), "");
    }
}
