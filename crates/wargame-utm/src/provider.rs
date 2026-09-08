//! `UtmProvider`: the core `VmProvider` port over UTM 4.7.5, plus the two
//! build-time operations `profile create` needs.
//!
//! Everything it runs goes through [`crate::automation::Automation`], and
//! every command it issues is a verified row of
//! `docs/utm-compatibility.md`'s operation matrix. The rules that shape
//! this file:
//!
//! * **The UUID is the identity** ([`VmRef::id`]); the display name is only
//!   ever shown or prefix-checked. UTM display names are not unique and
//!   `virtual machine named` silently returns the first match (T3).
//! * **Ownership before mutation**: a VM is stopped or deleted only when it
//!   is registered in UTM *and* its name is inside this app's prefix, so a
//!   UUID that points at somebody else's VM is refused rather than acted on
//!   (spec.md §3 FR-5).
//! * **`stopped` is not "free"**: after every stop, an orphaned
//!   `qemu-*-softmmu` holding this VM's `-uuid` fails the stop -- reported,
//!   never signalled (T12).

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::{sleep, Instant};

use wargame_core::domain::{
    ExecutionMode, GuestArch, GuestOs, NetworkMode, ProviderCapabilities, ResolvedBackend,
    Resources, VmRef,
};
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::process::ProcessRunner;
use wargame_core::ports::{
    BackendCheck, BackendStatus, ConsoleDescriptor, ImageRef, RuntimeInfo, StartOptions,
    VmProvider, VmSpec, VmStatus,
};

use crate::automation::{Automation, MakeVmParams, OrphanQemu, StopMethod, UtmStatus, UtmVm};
use crate::config::{
    architecture, display_hardware, meets_minimum, network_mode, parse_version, UtmConfig,
    BUNDLED_UTMCTL, ISOLATED_NETWORK_DETAIL, MIN_UTM_VERSION, VERIFIED_UTM_VERSION,
};
use crate::console;

/// Which reusable images exist for this backend.
///
/// UTM has no image registry of its own: a launchable image is a VM this
/// app built and recorded, so the answer lives in the app's build records
/// (spec.md §5 item 5), which are a later task. Until one is wired,
/// [`NoBuildRecords`] answers "not ready" -- the honest state, and the one
/// that sends the user to `profile create`/`profile build` instead of
/// launching from something that does not exist.
#[async_trait]
pub trait ImageCatalog: Send + Sync {
    async fn image_ready(&self, image: &ImageRef) -> Result<bool>;
}

/// The default [`ImageCatalog`]: nothing is ready yet.
pub struct NoBuildRecords;

#[async_trait]
impl ImageCatalog for NoBuildRecords {
    async fn image_ready(&self, _image: &ImageRef) -> Result<bool> {
        Ok(false)
    }
}

/// How long to wait for a resumed VM to leave `paused` before stopping it.
const RESUME_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for `stopped` after escalating to `stop ... by kill`.
const KILL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct UtmProvider {
    automation: Automation,
    cfg: UtmConfig,
    images: Arc<dyn ImageCatalog>,
    /// The run log passed to the most recent `start()` of each VM, so
    /// `wait_ip`'s timeout can point at it (spec.md §6).
    run_logs: Mutex<HashMap<String, PathBuf>>,
}

impl UtmProvider {
    pub fn new(runner: Arc<dyn ProcessRunner>, cfg: UtmConfig) -> Self {
        UtmProvider {
            automation: Automation::new(runner, cfg.clone()),
            cfg,
            images: Arc::new(NoBuildRecords),
            run_logs: Mutex::new(HashMap::new()),
        }
    }

    /// Replaces the "which images are ready" answer (the build-record store
    /// of a later task).
    pub fn with_image_catalog(mut self, images: Arc<dyn ImageCatalog>) -> Self {
        self.images = images;
        self
    }

    /// The UTM automation surface, for the build-time flows that need more
    /// than the `VmProvider` port exposes.
    pub fn automation(&self) -> &Automation {
        &self.automation
    }

    pub fn config(&self) -> &UtmConfig {
        &self.cfg
    }

    /// Where `utmctl` is (FR-6's discovery order), or an error naming every
    /// place that was tried.
    pub fn utmctl_path(&self) -> Result<String> {
        self.automation.utmctl_path()
    }

    /// The boot timeout for an execution mode, for callers outside the port.
    pub fn boot_timeout(&self, mode: ExecutionMode) -> Duration {
        self.cfg.boot_timeout_for(mode)
    }

    // --- build-time operations (spec.md §4's `profile create`) -------------

    /// Creates the installation VM for a profile: a blank disk, the
    /// installer ISO on a removable drive, UEFI, and a display so the user
    /// can actually run the installer (§3 -- a scripted VM gets none).
    ///
    /// Windows gets UEFI but **no TPM 2.0 and no Secure Boot**: neither is
    /// in UTM 4.7.5's dictionary at all (§6), and there is no way to set
    /// them from a script. That is left silently alone here; the
    /// `profile create` flow prints the manual step.
    pub async fn create_installer_vm(
        &self,
        name: &str,
        os: GuestOs,
        arch: GuestArch,
        mode: ExecutionMode,
        resources: &Resources,
        iso_path: &Path,
    ) -> Result<VmRef> {
        let hypervisor = hypervisor_for(arch, mode)?;
        let (id, created_name) = self
            .automation
            .make_vm(&MakeVmParams {
                name,
                architecture: architecture(arch),
                memory_mib: resources.memory_mib,
                cpu_cores: resources.cpu,
                hypervisor,
                // Kali's installer and every supported Windows build boot
                // UEFI; `uefi:true` is verified for both architectures.
                uefi: true,
                disk_mib: resources.disk_gib.saturating_mul(1024),
                iso: Some(iso_path),
            })
            .await?;
        if os == GuestOs::Windows {
            tracing::warn!(
                vm = %id,
                "UTM 4.7.5 cannot script TPM 2.0 or Secure Boot; enable them in the VM's QEMU settings before installing Windows 11"
            );
        }
        // The installer VM is built for a console session, so it always gets
        // NAT (the installer downloads packages) and a display.
        self.automation
            .set_network_mode(&id, network_mode(NetworkMode::Nat))
            .await?;
        self.automation
            .set_display(&id, display_hardware(arch))
            .await?;
        Ok(VmRef {
            backend: "utm".to_string(),
            id,
            name: created_name,
        })
    }

    /// Copies an existing VM, returning the copy's UUID.
    ///
    /// `duplicate` returns nothing at all (T2), so the new UUID is found by
    /// comparing the registry before and after -- by set difference, with
    /// the name only as a tie-breaker, because names are not unique (T3).
    pub async fn clone_vm(&self, source_id: &str, new_name: &str) -> Result<VmRef> {
        let before = self.automation.list().await?;
        let Some(source) = before.iter().find(|vm| vm.id == source_id) else {
            return Err(WargameError::VmCreationFailed(format!(
                "no UTM virtual machine with id {source_id} to clone from (it is the source recorded for this profile's image)"
            )));
        };
        // Copying a VM that is still running would capture a torn disk, and
        // the copy inherits everything including that state.
        if source.status.is_live() {
            return Err(WargameError::VmCreationFailed(format!(
                "the source VM {} ({}) is {}; stop it in UTM before cloning from it",
                source.name, source_id, source.status
            )));
        }
        self.automation.duplicate(source_id, new_name).await?;

        let after = self.automation.list().await?;
        let new: Vec<&UtmVm> = after
            .iter()
            .filter(|vm| !before.iter().any(|old| old.id == vm.id))
            .collect();
        let created = match new.as_slice() {
            [only] => *only,
            [] => {
                return Err(WargameError::VmCreationFailed(format!(
                    "UTM registered no new virtual machine after duplicating {source_id} as {new_name:?}"
                )))
            }
            many => many
                .iter()
                .find(|vm| vm.name == new_name)
                .copied()
                .ok_or_else(|| {
                    WargameError::VmCreationFailed(format!(
                        "UTM registered {} new virtual machines while duplicating {source_id} as {new_name:?}; none of them carries that name",
                        many.len()
                    ))
                })?,
        };
        Ok(VmRef {
            backend: "utm".to_string(),
            id: created.id.clone(),
            name: created.name.clone(),
        })
    }

    /// Adds a second removable drive backed by `iso` -- the Windows
    /// guest-tools CD (§2). The VM must be stopped.
    pub async fn attach_iso(&self, vm: &VmRef, iso: &Path) -> Result<()> {
        self.automation.attach_iso(&vm.id, iso).await
    }

    /// Points the VM's first removable drive at another ISO. There is no
    /// verified way to *remove* a drive, so an installer CD is swapped, not
    /// detached (§2 lists "Attach a second ISO" and "Replace an ISO"; no
    /// detach was reproduced).
    pub async fn replace_iso(&self, vm: &VmRef, iso: &Path) -> Result<()> {
        self.automation.replace_iso(&vm.id, iso).await
    }

    /// Shows the VM's window: starts it if it is not running (which
    /// launches and foregrounds UTM) and otherwise just activates the app.
    /// Used by `profile console`.
    pub async fn open_console(&self, vm: &VmRef) -> Result<()> {
        match self.automation.status(&vm.id).await? {
            UtmStatus::Missing => Err(WargameError::VmNotFound(format!(
                "UTM has no virtual machine with id {}",
                vm.id
            ))),
            status if status.is_live() => self.automation.activate_app().await,
            _ => {
                self.automation.start(&vm.id).await?;
                self.automation.activate_app().await
            }
        }
    }

    // --- internals ---------------------------------------------------------

    /// The registry entry for a VM this app is allowed to act on.
    ///
    /// `Ok(None)` means the UUID is not registered -- gone, which every
    /// caller treats as "nothing to do". A UUID that *is* registered but
    /// whose display name is outside our prefix belongs to the user, and is
    /// refused (FR-5: "이름 prefix만으로 외부 VM을 삭제하지 않는다" -- and
    /// never a VM we were not given from a record either).
    async fn owned_vm(&self, vm: &VmRef) -> Result<Option<UtmVm>> {
        let Some(entry) = self.automation.find(&vm.id).await? else {
            return Ok(None);
        };
        if !self.cfg.owns_name(&entry.name) {
            return Err(WargameError::CleanupFailed(format!(
                "refusing to touch UTM virtual machine {} ({:?}): its name is outside this app's {:?} prefix, so wargame did not create it",
                entry.id, entry.name, self.cfg.vm_prefix
            )));
        }
        Ok(Some(entry))
    }

    /// Polls `status` until `predicate` holds, or `timeout` elapses.
    /// Returns the last status seen.
    async fn wait_for_status(
        &self,
        id: &str,
        timeout: Duration,
        predicate: impl Fn(UtmStatus) -> bool,
    ) -> Result<UtmStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.automation.status(id).await?;
            if predicate(status) {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Ok(status);
            }
            sleep(self.cfg.status_poll_interval).await;
        }
    }

    /// T12: after a stop, refuse to call it done while a QEMU process still
    /// carries this VM's `-uuid`. We never signal it: the process belongs to
    /// UTM's `QEMUHelper`, and killing something we did not spawn is exactly
    /// what FR-5 forbids.
    async fn refuse_on_orphan(&self, vm: &VmRef) -> Result<()> {
        let Some(OrphanQemu { pid, program }) = self.automation.find_orphan_qemu(&vm.id).await?
        else {
            return Ok(());
        };
        Err(WargameError::OrphanProcess {
            pid,
            detail: format!(
                "UTM reports {} ({}) as stopped, but its QEMU process {program} (pid {pid}) is still running and holding the VM's files; starting, cloning, reconfiguring or deleting it now fails with \"Is another process using the image\"",
                vm.name, vm.id
            ),
        })
    }
}

/// UTM's `hypervisor` flag for an architecture/mode pair, or a refusal.
///
/// T6: UTM accepts `hypervisor:true` for a guest architecture that does not
/// match the host and silently emulates anyway -- the configuration reads
/// back `true` while QEMU still runs `-accel tcg`. Since spec.md §1 forbids
/// changing a requested mode silently, `virtualize` on x86_64 is refused
/// here as well as in the resolver: a defensive check, because the value
/// UTM would report back cannot be trusted.
fn hypervisor_for(arch: GuestArch, mode: ExecutionMode) -> Result<bool> {
    match (arch, mode) {
        (GuestArch::X86_64, ExecutionMode::Virtualize) => Err(WargameError::Incompatible(
            "requested: x86_64 guest in virtualize mode on UTM.\nAn x86_64 guest cannot be virtualized on an Apple Silicon host: UTM accepts `hypervisor:true` for it and silently emulates anyway (docs/utm-compatibility.md T6).\nUse mode: emulate for x86_64 guests, or arch: arm64 to virtualize."
                .to_string(),
        )),
        (GuestArch::Arm64, ExecutionMode::Virtualize) => Ok(true),
        // Emulation always means the hypervisor is off, on either arch.
        (_, ExecutionMode::Emulate) => Ok(false),
    }
}

/// The execution settings of a [`VmSpec`], checked before anything runs.
fn validate_execution(execution: &ResolvedBackend) -> Result<bool> {
    hypervisor_for(execution.arch, execution.mode)
}

#[async_trait]
impl VmProvider for UtmProvider {
    fn name(&self) -> &str {
        "utm"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // macOS guests stay on Tart: UTM's QEMU backend does not run
            // them, and the existing macOS path is deliberately untouched.
            guest_os: vec![GuestOs::Linux, GuestOs::Windows],
            guest_arch: vec![GuestArch::Arm64, GuestArch::X86_64],
            modes: vec![ExecutionMode::Virtualize, ExecutionMode::Emulate],
            // Both are configurable; `isolated` maps to UTM's `host` mode,
            // whose guest-side isolation is still unverified -- see
            // `healthcheck`'s detail, which says so in as many words.
            networks: vec![NetworkMode::Isolated, NetworkMode::Nat],
            graphical_console: true,
            ssh: true,
            snapshots: false,
            copy_in: true,
            remote: false,
        }
    }

    async fn healthcheck(&self) -> Result<BackendStatus> {
        let unavailable = |detail: String| BackendStatus {
            available: false,
            version: None,
            detail,
            isolated_network: None,
        };

        // 1. Is `utmctl` anywhere? Do this first: on a host with no UTM,
        //    nothing may be launched and a Tart profile must be unaffected.
        let utmctl = match self.automation.utmctl_located() {
            Ok((path, _source)) => path,
            Err(e) => return Ok(unavailable(e.to_string())),
        };

        // 2. The app handshake of T8, then the version.
        if let Err(e) = self.automation.ensure_app_running().await {
            return Ok(unavailable(e.to_string()));
        }
        let version = match self.automation.version().await {
            Ok(version) => version,
            Err(e) => return Ok(unavailable(e.to_string())),
        };
        let parsed = parse_version(&version);
        match parsed {
            Some(v) if !meets_minimum(v) => {
                return Ok(unavailable(format!(
                    "UTM {version} at {utmctl} is too old: {}.{} or newer is required (scripted VM creation arrived in 4.2, the Windows 11 TPM/Secure Boot toggles in 4.3)",
                    MIN_UTM_VERSION.0, MIN_UTM_VERSION.1
                )));
            }
            None => {
                return Ok(unavailable(format!(
                    "`utmctl version` printed {version:?}, which is not a version number"
                )))
            }
            Some(_) => {}
        }

        // 3. Can this process actually drive the app? `utmctl` lives inside
        //    UTM.app, so it needs no grant; `osascript` is out-of-process
        //    and is what a denied Automation permission shows up on (§1).
        if let Err(e) = self.automation.automation_probe().await {
            return Ok(unavailable(e.to_string()));
        }

        let mut detail = ISOLATED_NETWORK_DETAIL.to_string();
        if version != VERIFIED_UTM_VERSION {
            detail =
                format!("{detail}; UTM {version} is untested (verified: {VERIFIED_UTM_VERSION})");
        }
        Ok(BackendStatus {
            available: true,
            version: Some(version),
            // UTM holds the vmnet entitlements itself, so `host` mode needs
            // no sudo and no helper -- unlike Tart's Softnet path.
            isolated_network: Some(true),
            detail,
        })
    }

    async fn diagnostics(&self) -> Vec<BackendCheck> {
        let mut checks = Vec::new();
        let located = self.automation.utmctl_located();
        match &located {
            Ok((path, source)) => checks.push(BackendCheck {
                name: "utm:utmctl".to_string(),
                ok: true,
                detail: format!("{path} (from {})", source.as_str()),
                hint: None,
            }),
            Err(e) => {
                checks.push(BackendCheck {
                    name: "utm:utmctl".to_string(),
                    ok: false,
                    detail: e.to_string(),
                    hint: Some(
                        "Install UTM (`brew install --cask utm`), or set [vm.utm].utmctl to its path"
                            .to_string(),
                    ),
                });
                return checks;
            }
        }

        match self.automation.version().await {
            Ok(version) => {
                let parsed = parse_version(&version);
                let ok = parsed.map(meets_minimum).unwrap_or(false);
                checks.push(BackendCheck {
                    name: "utm:version".to_string(),
                    ok,
                    detail: if version == VERIFIED_UTM_VERSION {
                        format!("UTM {version} (verified)")
                    } else {
                        format!(
                            "UTM {version} (untested; verified against {VERIFIED_UTM_VERSION}, minimum {}.{})",
                            MIN_UTM_VERSION.0, MIN_UTM_VERSION.1
                        )
                    },
                    hint: (!ok).then(|| {
                        format!(
                            "Update UTM to {}.{} or newer",
                            MIN_UTM_VERSION.0, MIN_UTM_VERSION.1
                        )
                    }),
                });
            }
            Err(e) => checks.push(BackendCheck {
                name: "utm:version".to_string(),
                ok: false,
                detail: e.to_string(),
                hint: e.hint(),
            }),
        }

        match self.automation.automation_probe().await {
            Ok(version) => checks.push(BackendCheck {
                name: "utm:automation".to_string(),
                ok: true,
                detail: format!("osascript can control UTM (answered {version})"),
                hint: None,
            }),
            Err(e) => checks.push(BackendCheck {
                name: "utm:automation".to_string(),
                ok: false,
                detail: e.to_string(),
                hint: Some(e.hint().unwrap_or_else(|| {
                    wargame_core::error::automation_permission_hint(crate::config::UTM_APP)
                })),
            }),
        }
        checks
    }

    async fn image_exists(&self, image: &ImageRef) -> Result<bool> {
        self.images.image_ready(image).await
    }

    /// UTM has nothing to pull: an image is a VM this app built and
    /// recorded, and building one is `profile create` + `profile build`.
    async fn pull_image(&self, image: &ImageRef) -> Result<()> {
        Err(WargameError::Other(format!(
            "UTM has no image registry to pull {} from: a UTM image is a VM this app installed and recorded. Create the installation VM with `wargame profile create <profile> --iso <path>`, install the OS through its console, then `wargame profile build <profile>`",
            image.0
        )))
    }

    /// Clones the profile's source VM into a fresh one and applies the
    /// session's resources and network.
    ///
    /// The source is `spec.image`, which for UTM is the **UUID** of the VM
    /// the profile's build record points at -- UTM cannot be asked to
    /// provision from a base image, only to copy an existing VM (FR-2).
    async fn create(&self, spec: &VmSpec) -> Result<VmRef> {
        // Refuse an impossible execution configuration before contacting
        // UTM at all (spec.md §6: fail before anything is created).
        let hypervisor = validate_execution(&spec.execution)?;

        let vm = self.clone_vm(&spec.image.0, &spec.name).await?;

        // A clone inherits the source's configuration, so only what this
        // session decides is written -- while it is still stopped, which is
        // the only state `update configuration` accepts (T9).
        self.automation
            .set_hardware(
                &vm.id,
                spec.resources.cpu,
                spec.resources.memory_mib,
                hypervisor,
            )
            .await?;
        self.automation
            .set_network_mode(&vm.id, network_mode(spec.network))
            .await?;
        // The clone may come from a VM the user built without a display;
        // give it one so `wargame gui` and `profile console` have a window.
        self.automation
            .set_display(&vm.id, display_hardware(spec.execution.arch))
            .await?;
        // UTM's dictionary has no way to resize an existing drive
        // (`guest size` is write-only at creation, T5), so the disk stays
        // whatever the source image was built with.
        if spec.resources.disk_gib > 0 {
            tracing::debug!(
                vm = %vm.id,
                disk_gib = spec.resources.disk_gib,
                "UTM cannot resize a cloned disk from a script; keeping the source image's disk size"
            );
        }
        Ok(vm)
    }

    async fn start(&self, vm: &VmRef, opts: &StartOptions) -> Result<RuntimeInfo> {
        if opts.headless {
            // T7: `--hide` emits -10004 and hides nothing; there is no
            // headless start. The window is part of the deal.
            tracing::debug!(vm = %vm.id, "UTM has no headless start; the VM window will open");
        }
        let status = self.automation.status(&vm.id).await?;
        if status == UtmStatus::Missing {
            return Err(WargameError::VmNotFound(format!(
                "UTM has no virtual machine with id {} ({})",
                vm.id, vm.name
            )));
        }
        // Enforce the session's network on every start, while the VM is
        // stopped -- the only state UTM accepts a configuration write in.
        // An `isolated` session must never come back up on NAT because
        // something changed the VM in the app (FR-6).
        if status == UtmStatus::Stopped {
            self.automation
                .set_network_mode(&vm.id, network_mode(opts.network))
                .await?;
        } else {
            tracing::debug!(
                vm = %vm.id,
                status = %status,
                "VM is not stopped; leaving its network configuration alone"
            );
        }
        self.automation.start(&vm.id).await?;
        self.run_logs
            .lock()
            .unwrap()
            .insert(vm.id.clone(), opts.log_path.clone());
        // UTM runs QEMU under its own XPC service: there is no child of
        // ours to report.
        Ok(RuntimeInfo { pid: None })
    }

    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr> {
        let deadline = Instant::now() + timeout;
        let mut last_reason;
        loop {
            match self.automation.guest_ip(&vm.id).await? {
                Ok(ip) => return Ok(ip),
                Err(reason) => last_reason = reason,
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(self.cfg.ip_poll_interval).await;
        }

        let log_path = self
            .run_logs
            .lock()
            .unwrap()
            .get(&vm.id)
            .cloned()
            .unwrap_or_else(|| PathBuf::from("$WARGAME_HOME/sessions/<id>/run.log"));
        Err(WargameError::GuestAddressTimeout {
            detail: format!(
                "{} ({}) did not report an address within {} s: {last_reason}. UTM reads the guest address from the QEMU guest agent, so the guest needs `qemu-guest-agent` installed and running -- before that, an installing or half-provisioned guest never answers",
                vm.name,
                vm.id,
                timeout.as_secs(),
            ),
            hint: format!(
                "Watch the VM's UTM window (an emulated x86_64 boot is roughly 3x slower than a virtualized one), or the run log at {}.\nIf it is only slow, raise [vm.utm].boot_timeout_secs (emulate) or [vm.utm].virtualize_boot_timeout_secs in $WARGAME_HOME/config.toml.",
                log_path.display()
            ),
        })
    }

    async fn stop(&self, vm: &VmRef) -> Result<()> {
        let Some(entry) = self.owned_vm(vm).await? else {
            tracing::info!(
                vm = %vm.id,
                "UTM no longer knows this VM; nothing to stop"
            );
            return Ok(());
        };

        // `stop` refuses a paused VM ("The virtual machine is not running",
        // exit 0), so resume it first (§2).
        let mut status = self.automation.status(&vm.id).await?;
        if matches!(status, UtmStatus::Paused | UtmStatus::Pausing) {
            tracing::debug!(vm = %vm.id, "resuming a paused VM so it can be stopped");
            self.automation.start(&vm.id).await?;
            status = self
                .wait_for_status(&vm.id, RESUME_TIMEOUT, |s| {
                    !matches!(s, UtmStatus::Paused | UtmStatus::Pausing)
                })
                .await?;
        }

        if status.is_live() {
            self.automation.stop(&vm.id, StopMethod::Force).await?;
            // Poll for `stopped` itself: `stop` is an Apple Event with T1's
            // exit-code defect, and the VM passes through `stopping`, which
            // can persist (T9).
            let mut status = self
                .wait_for_status(&vm.id, self.cfg.stop_timeout, |s| {
                    matches!(s, UtmStatus::Stopped | UtmStatus::Missing)
                })
                .await?;
            if !matches!(status, UtmStatus::Stopped | UtmStatus::Missing) {
                tracing::warn!(
                    vm = %vm.id,
                    name = %entry.name,
                    status = %status,
                    "graceful stop did not finish in time; terminating the backend"
                );
                self.automation.stop(&vm.id, StopMethod::Kill).await?;
                status = self
                    .wait_for_status(&vm.id, KILL_TIMEOUT, |s| {
                        matches!(s, UtmStatus::Stopped | UtmStatus::Missing)
                    })
                    .await?;
            }
            if !matches!(status, UtmStatus::Stopped | UtmStatus::Missing) {
                return Err(WargameError::CleanupFailed(format!(
                    "UTM virtual machine {} ({}) is still {status} after a forced stop and a kill",
                    entry.name, vm.id
                )));
            }
        }

        // T12: `stopped` does not mean the VM's files are free.
        self.refuse_on_orphan(vm).await
    }

    async fn delete(&self, vm: &VmRef) -> Result<()> {
        let Some(entry) = self.owned_vm(vm).await? else {
            tracing::info!(vm = %vm.id, "UTM no longer knows this VM; nothing to delete");
            return Ok(());
        };
        if entry.status.is_live() {
            self.stop(vm).await?;
        } else {
            // Even an already-stopped VM can have T12's orphan holding its
            // files; deleting under that is how bundles get corrupted.
            self.refuse_on_orphan(vm).await?;
        }
        self.automation.delete(&vm.id).await
    }

    async fn status(&self, vm: &VmRef) -> Result<VmStatus> {
        Ok(match self.automation.status(&vm.id).await? {
            // `starting`/`started`/`pausing`/`paused`/`resuming` all have a
            // live backend; a paused VM is still occupying its resources.
            status if status.is_live() => VmStatus::Running,
            // `stopping` is reported as stopped-in-progress: nothing may be
            // written to the configuration in that state either.
            UtmStatus::Stopped | UtmStatus::Stopping => VmStatus::Stopped,
            UtmStatus::Missing => VmStatus::Missing,
            // `is_live` covers the rest; this keeps the match total.
            _ => VmStatus::Stopped,
        })
    }

    async fn list(&self) -> Result<Vec<VmRef>> {
        Ok(self
            .automation
            .list()
            .await?
            .into_iter()
            // Only this app's namespace: everything else is the user's, and
            // `prune` must never even see it (FR-5).
            .filter(|vm| self.cfg.owns_name(&vm.name))
            .map(|vm| VmRef {
                backend: "utm".to_string(),
                id: vm.id,
                name: vm.name,
            })
            .collect())
    }

    fn console(&self, vm: &VmRef) -> ConsoleDescriptor {
        let utmctl = self
            .automation
            .utmctl_path()
            .unwrap_or_else(|_| BUNDLED_UTMCTL.to_string());
        console::console(&utmctl, &vm.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypervisor_is_on_only_for_a_virtualized_arm64_guest() {
        assert!(hypervisor_for(GuestArch::Arm64, ExecutionMode::Virtualize).unwrap());
        assert!(!hypervisor_for(GuestArch::Arm64, ExecutionMode::Emulate).unwrap());
        assert!(!hypervisor_for(GuestArch::X86_64, ExecutionMode::Emulate).unwrap());

        let err = hypervisor_for(GuestArch::X86_64, ExecutionMode::Virtualize).unwrap_err();
        assert!(matches!(err, WargameError::Incompatible(_)));
        let message = err.to_string();
        assert!(message.contains("x86_64"), "{message}");
        assert!(message.contains("emulate"), "{message}");
    }
}
