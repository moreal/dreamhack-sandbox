//! The UTM half of the common image build
//! (`wargame_core::app::profile_build`), plus the [`ImageCatalog`] that lets
//! `image_exists`/`doctor` see what a build published.
//!
//! What is UTM-specific, and therefore lives here:
//!
//! * **The installation VM.** UTM has no base images: the first OS install
//!   is manual, through the console, so `profile create` makes one long-lived
//!   VM (blank disk + installer ISO, or a copy of an installed VM) and the
//!   record keeps its UUID. Every later build re-uses that same VM -- it is
//!   never deleted by a failure, because the user's install lives in it
//!   (spec.md §3 FR-5: "설치용 VM은 재개할 수 있도록 보존한다").
//! * **Publishing is a clone.** The launchable image is a *stopped copy* of
//!   the provisioned installation VM (`duplicate`, T2), so a session's
//!   `create` can clone from it while the installation VM stays available
//!   for the next rebuild.
//! * **A clone's disk cannot be resized** (T5), so the image keeps whatever
//!   disk the installation VM was created with.

use std::sync::Arc;

use async_trait::async_trait;

use wargame_core::domain::build::{BuildSource, PublishedImage};
use wargame_core::domain::{GuestEndpoint, NetworkMode, VmRef};
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::image_builder::{BuildRequest, GuestLink, InstallerRequest, Progress};
use wargame_core::ports::{
    BuildRecordRepository, GuestTransport, ImageRef, ProfileImageBuilder, StartOptions, VmProvider,
    VmStatus,
};

use crate::provider::ImageCatalog;
use crate::UtmProvider;

/// The UTM build backend: the installation VM, the boot/connect wait with
/// the mode's timeout, and the publish clone.
pub struct UtmImageBuilder {
    utm: Arc<UtmProvider>,
    transport: Arc<dyn GuestTransport>,
}

impl UtmImageBuilder {
    pub fn new(utm: Arc<UtmProvider>, transport: Arc<dyn GuestTransport>) -> Self {
        UtmImageBuilder { utm, transport }
    }

    /// The display name of the image VM a build publishes for `profile`:
    /// inside the build namespace, so `prune` never sees it as a session
    /// orphan, and distinct from the installation VM's name.
    pub fn image_vm_name(&self, profile: &str) -> String {
        format!("{}image-{profile}", self.utm.config().build_prefix)
    }

    /// The installation VM a build must run against, or an error telling the
    /// user to create one.
    fn installer_vm(&self, req: &BuildRequest<'_>) -> Result<VmRef> {
        req.installer_vm().cloned().ok_or_else(|| {
            WargameError::VmNotFound(format!(
                "profile {} has no installation VM: run `wargame profile create {} --iso <path>` first",
                req.profile.name, req.profile.name
            ))
        })
    }
}

#[async_trait]
impl ProfileImageBuilder for UtmImageBuilder {
    fn backend(&self) -> &str {
        "utm"
    }

    fn uses_installer_vm(&self) -> bool {
        true
    }

    async fn create_installer_vm(&self, req: &InstallerRequest<'_>) -> Result<VmRef> {
        match req.source {
            BuildSource::Iso { path, .. } => {
                self.utm
                    .create_installer_vm(
                        &req.name,
                        req.profile.guest.os,
                        req.profile.guest.arch,
                        req.execution.mode,
                        &req.profile.resources,
                        path,
                    )
                    .await
            }
            // The source VM is only ever copied: never started, never
            // reconfigured, never deleted (spec.md §3 FR-2, AC-05).
            BuildSource::UtmVm { uuid } => self.utm.clone_vm(uuid, &req.name).await,
            BuildSource::TartBase { .. } => Err(WargameError::InvalidProfile {
                name: req.profile.name.clone(),
                reason: "a UTM installation VM is made from an ISO or from an installed UTM VM, not from a Tart base image".to_string(),
            }),
        }
    }

    async fn open_console(&self, vm: &VmRef) -> Result<()> {
        self.utm.open_console(vm).await
    }

    async fn prepare(&self, req: &BuildRequest<'_>, progress: Progress<'_>) -> Result<()> {
        let vm = self.installer_vm(req)?;
        progress(format!(
            "checking the installation VM {} ({})",
            vm.name, vm.id
        ));
        if self.utm.status(&vm).await? == VmStatus::Missing {
            // Deleted in UTM behind our back: never silently make another
            // one (spec.md §3 FR-5's "관계없는 VM 생성/삭제 금지").
            return Err(WargameError::VmNotFound(format!(
                "the installation VM {} ({}) this profile was created with no longer exists in UTM. Nothing was recreated; make a new profile with `wargame profile init <template> --name <new>` and `wargame profile create`",
                vm.name, vm.id
            )));
        }
        Ok(())
    }

    /// UTM provisions the installation VM itself -- the copy is made at
    /// publish time, so the user's install survives a failed build.
    async fn clone_for_build(
        &self,
        req: &BuildRequest<'_>,
        progress: Progress<'_>,
    ) -> Result<VmRef> {
        let vm = self.installer_vm(req)?;
        progress(format!("building in {} ({})", vm.name, vm.id));
        Ok(vm)
    }

    async fn boot(&self, req: &BuildRequest<'_>, vm: &VmRef, progress: Progress<'_>) -> Result<()> {
        // `profile console` leaves the VM running -- which is exactly how a
        // user gets from "installed the OS" to "build it", so the common
        // case is a VM that is already up. UTM answers a `start` on a
        // running VM with -2700 ("Operation not available"), so ask first.
        match self.utm.status(vm).await? {
            VmStatus::Running => {
                progress(format!(
                    "{} is already running (its console is open); building against it as it is",
                    vm.name
                ));
                return Ok(());
            }
            VmStatus::Missing => {
                return Err(WargameError::VmNotFound(format!(
                    "UTM has no virtual machine with id {} ({})",
                    vm.id, vm.name
                )))
            }
            VmStatus::Stopped => {}
        }

        progress("starting the installation VM on NAT".to_string());
        self.utm
            .start(
                vm,
                &StartOptions {
                    // A build installs packages (spec.md §3 FR-6).
                    network: NetworkMode::Nat,
                    // UTM has no headless start at all (T7); the window
                    // opening is also what lets the user watch a long
                    // emulated boot.
                    headless: false,
                    log_path: req.log_path.clone(),
                },
            )
            .await?;
        Ok(())
    }

    async fn connect(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<GuestLink> {
        progress(format!(
            "waiting up to {} s for the QEMU guest agent to report an address ({} boot)",
            req.boot_timeout.as_secs(),
            req.execution.mode
        ));
        let ip = self.utm.wait_ip(vm, req.boot_timeout).await?;
        progress(format!("guest address: {ip}"));

        let endpoint = GuestEndpoint::new(ip, req.profile.guest.user.clone());
        progress(format!("waiting for SSH as {endpoint}"));
        self.transport
            .wait_ready(&endpoint, req.boot_timeout)
            .await
            .map_err(|e| {
                WargameError::GuestAddressTimeout {
                    detail: format!(
                        "{} ({}) answers on {ip}, but SSH as {:?} did not accept a key within {} s: {e}",
                        vm.name, vm.id, endpoint.user, req.boot_timeout.as_secs()
                    ),
                    hint: format!(
                        "In the guest: install and enable an SSH server, make sure the user {:?} exists, and append the host key to its ~/.ssh/authorized_keys.\nOpen the console with `wargame profile console {}`, then run the build again.",
                        endpoint.user, req.profile.name
                    ),
                }
            })?;
        Ok(GuestLink {
            endpoint,
            transport: self.transport.clone(),
        })
    }

    async fn shutdown(
        &self,
        _req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<()> {
        progress(format!("stopping {} gracefully", vm.name));
        // `UtmProvider::stop` escalates, waits for `stopped` and refuses on
        // T12's orphaned QEMU process -- exactly what must not be published
        // over.
        self.utm.stop(vm).await?;
        if self.utm.status(vm).await? == VmStatus::Running {
            return Err(WargameError::VmCreationFailed(format!(
                "{} ({}) is still running after a graceful stop; refusing to publish an image copied from a live disk",
                vm.name, vm.id
            )));
        }
        Ok(())
    }

    /// Copies the provisioned (and now stopped) installation VM into the
    /// image VM a session launch clones from.
    async fn publish(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<PublishedImage> {
        let name = self.image_vm_name(&req.profile.name);
        progress(format!("publishing {} -> {name}", vm.name));
        let image = self.utm.clone_vm(&vm.id, &name).await?;
        Ok(PublishedImage::new(image.id, image.name))
    }

    async fn discard_image(&self, image: &PublishedImage) -> Result<()> {
        self.utm
            .delete(&VmRef {
                backend: "utm".to_string(),
                id: image.id.clone(),
                name: image.name.clone(),
            })
            .await
    }

    /// The installation VM is **never** deleted: the user's OS install lives
    /// in it and the next build resumes from it (spec.md §3 FR-5). All a
    /// failed build cleans up is leaving it running.
    async fn cleanup_failed_build(&self, _req: &BuildRequest<'_>, vm: &VmRef) -> Vec<String> {
        match self.utm.stop(vm).await {
            Ok(()) => Vec::new(),
            Err(e) => {
                tracing::warn!(vm = %vm.id, error = %e, "could not stop the installation VM after a failed build");
                vec![format!(
                    "could not stop the installation VM {} ({}) after the failure: {e}",
                    vm.name, vm.id
                )]
            }
        }
    }
}

/// The [`ImageCatalog`] over build records: a UTM image is "ready" exactly
/// when some profile's build record says it published that VM (spec.md §5
/// item 5). This is what turns `image_exists` -- and therefore `doctor`'s
/// `profile-image:` row -- from the honest "nothing is ready yet" default
/// into the real answer.
pub struct RecordImageCatalog {
    builds: Arc<dyn BuildRecordRepository>,
}

impl RecordImageCatalog {
    pub fn new(builds: Arc<dyn BuildRecordRepository>) -> Self {
        RecordImageCatalog { builds }
    }
}

#[async_trait]
impl ImageCatalog for RecordImageCatalog {
    async fn image_ready(&self, image: &ImageRef) -> Result<bool> {
        Ok(self
            .builds
            .list()
            .await?
            .iter()
            .filter(|record| record.backend == "utm")
            .any(|record| record.ready_image().map(|i| i.id == image.0) == Some(true)))
    }
}
