//! `profile eject | stop | delete`: the lifecycle verbs for the VMs a build
//! record owns (spec.md §3 FR-5's "설치용 VM은 재개할 수 있도록 보존한다"
//! and its ownership rule; §4's CLI contract).
//!
//! The installation VM is long-lived by design -- the user's OS install
//! lives in it and every rebuild starts from it -- which is exactly why it
//! needs verbs of its own. `wargame stop` and `wargame delete` are *session*
//! verbs and refuse it, so before these existed the only way to stop or
//! remove an installation VM was `utmctl` by hand
//! (task-7-phase-a-report.md §5 D4).
//!
//! Two rules shape everything here:
//!
//! * **Ownership comes from the build record, never from a name.** Each VM
//!   is addressed by the `(backend, uuid)` the record wrote down, and the
//!   backend adapter refuses a UUID whose VM is outside this app's prefix.
//!   A VM no record names is never seen, let alone acted on.
//! * **Stopping is the backend's job.** `profile stop` goes through the same
//!   [`VmProvider::stop`] a session stop does, so the UTM path keeps its
//!   whole contract: poll for `stopped` rather than trusting the verb (T9),
//!   escalate to a kill only after the timeout, and fail -- naming the pid
//!   -- when a QEMU process this app never spawned is still holding the VM
//!   (T12).

use std::sync::Arc;

use crate::domain::build::{BuildRecord, InstallerMedia};
use crate::domain::{Profile, VmRef};
use crate::error::{Result, WargameError};
use crate::ports::image_builder::{EjectOutcome, Progress};
use crate::ports::{ProfileImageBuilder, VmProvider, VmStatus};

use super::backends::resolve_backend;
use super::profile_build::refuse;
use super::AppContext;

/// What one of a record's VMs is for, so a message can say which one it is
/// talking about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmRole {
    /// The long-lived VM the OS was installed into.
    Installer,
    /// A VM a build published as a launchable image -- the current one, the
    /// one it replaced, or one an earlier deletion failed to remove.
    Image,
}

impl VmRole {
    pub fn as_str(self) -> &'static str {
        match self {
            VmRole::Installer => "installation VM",
            VmRole::Image => "published image",
        }
    }
}

impl std::fmt::Display for VmRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One VM a build record owns, with the status the backend reports for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedVm {
    pub role: VmRole,
    pub vm: VmRef,
    pub status: VmStatus,
}

impl OwnedVm {
    /// `"installation VM wg-build-kali (UUID)"`, for messages.
    pub fn describe(&self) -> String {
        format!("{} {} ({})", self.role, self.vm.name, self.vm.id)
    }
}

/// What [`eject`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EjectReport {
    pub vm: VmRef,
    pub outcome: EjectOutcome,
}

/// What [`stop`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    pub vm: VmRef,
    /// Whether the VM was up when the command started -- so the CLI can say
    /// "stopped it" apart from "it was already stopped".
    pub was_running: bool,
    /// The status after the stop.
    pub status: VmStatus,
}

/// Everything [`delete`] would remove, for the confirmation the CLI asks
/// before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePlan {
    pub profile: String,
    pub vms: Vec<OwnedVm>,
}

impl DeletePlan {
    /// The VMs that are up, and so block the delete.
    pub fn running(&self) -> Vec<&OwnedVm> {
        self.vms
            .iter()
            .filter(|owned| owned.status == VmStatus::Running)
            .collect()
    }
}

/// What [`delete`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReport {
    pub profile: String,
    pub deleted: Vec<OwnedVm>,
    /// VMs the record named that the backend no longer has.
    pub already_gone: Vec<OwnedVm>,
}

// --- profile eject ---------------------------------------------------------

/// Detaches the installer media from a profile's installation VM
/// (spec.md §4's `profile eject`, task-7-phase-a-report.md §5 D3).
///
/// `profile build` does this on its own before every boot; this verb is for
/// doing it deliberately -- right after the OS install, so the next
/// `profile console` lands in the installed system instead of the installer.
/// Running it twice is harmless.
pub async fn eject(
    ctx: &AppContext,
    profile_name: &str,
    progress: Progress<'_>,
) -> Result<EjectReport> {
    let (profile, builder, mut record) = installer_profile(ctx, profile_name, "eject").await?;
    let vm = installer_vm(&profile, &record)?;

    let outcome = builder.eject_installer_media(&vm, progress).await?;
    if let EjectOutcome::Blocked(reason) = &outcome {
        return Err(refuse_with_hint(
            &profile.name,
            format!("its installer media is still attached -- {reason}"),
            format!(
                "wargame profile stop {0}   # then: wargame profile eject {0}",
                profile.name
            ),
        ));
    }
    if outcome.is_detached() && record.installer_media != InstallerMedia::Detached {
        record.installer_media = InstallerMedia::Detached;
        ctx.builds.save(&record).await?;
    }
    Ok(EjectReport { vm, outcome })
}

// --- profile stop ----------------------------------------------------------

/// Stops a profile's installation VM (spec.md §4's `profile stop`).
///
/// Everything the stop has to get right -- polling for `stopped` instead of
/// trusting the verb, escalating to a kill only after the timeout, refusing
/// while an orphaned backend process still holds the VM -- belongs to the
/// backend, and this is the same [`VmProvider::stop`] a session stop uses.
pub async fn stop(ctx: &AppContext, profile_name: &str) -> Result<StopReport> {
    let (profile, _builder, record) = installer_profile(ctx, profile_name, "stop").await?;
    let vm = installer_vm(&profile, &record)?;
    let provider = provider_for(ctx, &record)?;

    let before = provider.status(&vm).await?;
    if before != VmStatus::Running {
        return Ok(StopReport {
            vm,
            was_running: false,
            status: before,
        });
    }
    provider.stop(&vm).await?;
    let status = provider.status(&vm).await?;
    Ok(StopReport {
        vm,
        was_running: true,
        status,
    })
}

// --- profile delete --------------------------------------------------------

/// What [`delete`] would remove, so the caller can show it and ask first.
pub async fn plan_delete(ctx: &AppContext, profile_name: &str) -> Result<DeletePlan> {
    let (profile, _builder, record) = installer_profile(ctx, profile_name, "delete").await?;
    Ok(DeletePlan {
        profile: profile.name.clone(),
        vms: owned_vms(ctx, &record).await?,
    })
}

/// Deletes a profile's installation VM, every image its builds published,
/// and the build record itself (spec.md §4's `profile delete`).
///
/// Refuses while any of them is running: an installation VM is where the
/// user's OS install lives, and deleting one out from under a running guest
/// is how a disk gets corrupted. Nothing outside the record is ever
/// touched, and the record is removed **last** -- it is the only thing that
/// knows these VMs exist, so a VM that could not be deleted must stay
/// reachable through it.
pub async fn delete(ctx: &AppContext, profile_name: &str) -> Result<DeleteReport> {
    let (profile, _builder, record) = installer_profile(ctx, profile_name, "delete").await?;
    let provider = provider_for(ctx, &record)?;
    let owned = owned_vms(ctx, &record).await?;

    let running: Vec<String> = owned
        .iter()
        .filter(|o| o.status == VmStatus::Running)
        .map(OwnedVm::describe)
        .collect();
    if !running.is_empty() {
        return Err(refuse_with_hint(
            &profile.name,
            format!(
                "{} is still running; nothing was deleted",
                running.join(", ")
            ),
            format!(
                "wargame profile stop {0}   # then: wargame profile delete {0}",
                profile.name
            ),
        ));
    }

    let mut deleted = Vec::new();
    let mut already_gone = Vec::new();
    for owned in owned {
        if owned.status == VmStatus::Missing {
            already_gone.push(owned);
            continue;
        }
        provider.delete(&owned.vm).await.map_err(|e| {
            // The record is deliberately still there: it is what the retry
            // needs.
            WargameError::CleanupFailed(format!(
                "could not delete the {} of profile {}: {e}. The build record was kept, so `wargame profile delete {}` can be run again once that is sorted out",
                owned.describe(),
                profile.name,
                profile.name
            ))
        })?;
        deleted.push(owned);
    }

    ctx.builds.delete(&profile.name).await?;
    Ok(DeleteReport {
        profile: profile.name.clone(),
        deleted,
        already_gone,
    })
}

// --- shared plumbing -------------------------------------------------------

/// The profile, its build backend and its record -- refusing a backend that
/// has no installation VM at all, and a profile nobody has created one for.
async fn installer_profile(
    ctx: &AppContext,
    profile_name: &str,
    verb: &str,
) -> Result<(Profile, Arc<dyn ProfileImageBuilder>, BuildRecord)> {
    let profile = ctx.profiles.get(profile_name)?;
    let execution = resolve_backend(&profile)?;
    let builder = ctx.builders.get(execution.backend.as_str())?;
    if !builder.uses_installer_vm() {
        return Err(refuse(
            &profile.name,
            format!(
                "`profile {verb}` acts on the installation VM of a UTM profile; {} profiles build from `image.base` and have no installation VM. Use `wargame stop`/`wargame delete` for a session's VM",
                execution.backend
            ),
        ));
    }
    let record = ctx
        .builds
        .load(&profile.name)
        .await?
        .ok_or_else(|| no_installation_vm(&profile.name))?;
    Ok((profile, builder, record))
}

/// [`super::profile_build`]'s `refuse`, plus the command that makes the
/// refused verb work.
fn refuse_with_hint(
    profile: &str,
    reason: impl std::fmt::Display,
    hint: impl Into<String>,
) -> WargameError {
    WargameError::Refused {
        detail: format!("profile {profile}: {reason}"),
        hint: hint.into(),
    }
}

fn installer_vm(profile: &Profile, record: &BuildRecord) -> Result<VmRef> {
    record
        .installer_vm
        .clone()
        .ok_or_else(|| no_installation_vm(&profile.name))
}

/// These verbs act on the installation VM, so the refusal says *that* --
/// not "no built image", which is the answer to a different question
/// (`launch`'s).
fn no_installation_vm(profile: &str) -> WargameError {
    WargameError::Refused {
        detail: format!(
            "profile {profile} has no installation VM: nothing has been created for it yet"
        ),
        hint: format!("wargame profile create {profile} --iso <path>   # or --from-vm <utm-uuid>"),
    }
}

fn provider_for(ctx: &AppContext, record: &BuildRecord) -> Result<Arc<dyn VmProvider>> {
    ctx.backends.get(&record.backend)
}

/// Every VM the record owns, in the order they should be dealt with: the
/// installation VM first, then the images. The status comes from the
/// backend, so a VM deleted in the app behind our back is `Missing` rather
/// than an error.
async fn owned_vms(ctx: &AppContext, record: &BuildRecord) -> Result<Vec<OwnedVm>> {
    let provider = provider_for(ctx, record)?;
    let mut refs: Vec<(VmRole, VmRef)> = Vec::new();
    if let Some(vm) = &record.installer_vm {
        refs.push((VmRole::Installer, vm.clone()));
    }
    for image in [record.image.as_ref(), record.previous_image.as_ref()]
        .into_iter()
        .flatten()
        .chain(record.leaked_images.iter())
    {
        if refs.iter().any(|(_, vm)| vm.id == image.id) {
            continue;
        }
        refs.push((
            VmRole::Image,
            VmRef {
                backend: record.backend.clone(),
                id: image.id.clone(),
                name: image.name.clone(),
            },
        ));
    }

    let mut owned = Vec::with_capacity(refs.len());
    for (role, vm) in refs {
        let status = provider.status(&vm).await?;
        owned.push(OwnedVm { role, vm, status });
    }
    Ok(owned)
}
