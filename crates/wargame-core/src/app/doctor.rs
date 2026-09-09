//! Environment diagnostics (DESIGN.md §3.1's `app/doctor.rs`; DESIGN.md
//! §7's check list): a flat list of independent checks. No individual
//! check failing stops the others from running, and [`doctor`] itself
//! never returns `Err` — a failed check is just a `Check { ok: false, .. }`
//! row for the CLI/TUI to render.

use crate::domain::{GuestShell, NetworkMode, Profile};

use super::AppContext;

/// How one [`Check`] came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Everything this check needs is there.
    Pass,
    /// A real problem: something is missing or broken.
    Fail,
    /// Nothing is broken -- this simply has not been set up here yet.
    ///
    /// A profile whose image was never built is the case that matters: a
    /// macOS-only host is not faulty for shipping Kali and Windows
    /// templates it has no installation ISO for, and three permanent `✗`
    /// rows for them teach the reader to ignore `✗` (spec.md §6's error
    /// UX). A build that was started and did not finish is *not* this: it
    /// stays a failure.
    NotSetUp,
}

impl CheckStatus {
    fn from_ok(ok: bool) -> CheckStatus {
        if ok {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        }
    }

    /// The word `doctor --json` prints.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Fail => "fail",
            CheckStatus::NotSetUp => "not-set-up",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    /// Whether the check is satisfied.
    pub fn passed(&self) -> bool {
        self.status == CheckStatus::Pass
    }

    /// Whether it found a real problem. A [`CheckStatus::NotSetUp`] row has
    /// not: it is neither passed nor failed.
    pub fn failed(&self) -> bool {
        self.status == CheckStatus::Fail
    }
}

fn binary_check(ctx: &AppContext, name: &str, program: &str, hint: Option<&str>) -> Check {
    match ctx.runner.which(program) {
        Some(path) => Check {
            name: name.to_string(),
            status: CheckStatus::from_ok(true),
            detail: format!("found at {}", path.display()),
            hint: None,
        },
        None => Check {
            name: name.to_string(),
            status: CheckStatus::from_ok(false),
            detail: format!("{program} not found on PATH"),
            hint: hint.map(str::to_string),
        },
    }
}

const MIN_FREE_GIB: f64 = 20.0;

/// Runs every host/environment check (DESIGN.md §7). Always returns the
/// full list, even when several checks fail.
pub async fn doctor(ctx: &AppContext) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(Check {
        name: "platform".to_string(),
        status: CheckStatus::from_ok(
            cfg!(target_os = "macos") && std::env::consts::ARCH == "aarch64",
        ),
        detail: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        hint: if cfg!(target_os = "macos") {
            None
        } else {
            Some("wargame-vm requires macOS on Apple Silicon".to_string())
        },
    });

    // One row per wired backend, so a host that has Tart but not UTM (or
    // the other way round) sees exactly which one is missing.
    for vm in ctx.backends.all() {
        let healthcheck = vm.healthcheck().await;
        checks.push(match &healthcheck {
            Ok(status) => Check {
                name: format!("vm-backend:{}", vm.name()),
                status: CheckStatus::from_ok(status.available),
                detail: status
                    .version
                    .clone()
                    .unwrap_or_else(|| status.detail.clone()),
                hint: if status.available {
                    None
                } else {
                    Some(format!("Install/start the {} VM backend", vm.name()))
                },
            },
            Err(e) => Check {
                name: format!("vm-backend:{}", vm.name()),
                status: CheckStatus::from_ok(false),
                detail: e.to_string(),
                hint: e.hint(),
            },
        });

        // One row per backend: with two backends wired, a single
        // `network:isolated` row would silently describe whichever one ran
        // last.
        checks.push(isolated_network_check(vm.name(), healthcheck.as_ref().ok()));
    }

    checks.push(binary_check(ctx, "ssh", "ssh", Some("Install OpenSSH")));
    checks.push(binary_check(ctx, "scp", "scp", Some("Install OpenSSH")));
    checks.push(binary_check(
        ctx,
        "ssh-keygen",
        "ssh-keygen",
        Some("Install OpenSSH"),
    ));
    checks.push(binary_check(ctx, "security", "security", None));
    checks.push(binary_check(
        ctx,
        "sshpass",
        "sshpass",
        Some("Only needed for `wargame profile build`"),
    ));

    checks.push(match crate::infra::disk_free_bytes(&ctx.dirs.root) {
        Ok(free) => {
            let free_gib = free as f64 / (1024.0 * 1024.0 * 1024.0);
            Check {
                name: "disk-space".to_string(),
                status: CheckStatus::from_ok(free_gib >= MIN_FREE_GIB),
                detail: format!("{free_gib:.1} GiB free at {}", ctx.dirs.root.display()),
                hint: if free_gib >= MIN_FREE_GIB {
                    None
                } else {
                    Some("Free up disk space".to_string())
                },
            }
        }
        Err(e) => Check {
            name: "disk-space".to_string(),
            status: CheckStatus::from_ok(false),
            detail: e.to_string(),
            hint: None,
        },
    });

    let write_probe = ctx.dirs.cache_dir().join(".doctor-write-test");
    let home_writable = std::fs::create_dir_all(ctx.dirs.cache_dir()).is_ok()
        && std::fs::write(&write_probe, b"ok").is_ok();
    if home_writable {
        let _ = std::fs::remove_file(&write_probe);
    }
    checks.push(Check {
        name: "wargame-home".to_string(),
        status: CheckStatus::from_ok(home_writable),
        detail: ctx.dirs.root.display().to_string(),
        hint: if home_writable {
            None
        } else {
            Some("Check permissions on $WARGAME_HOME".to_string())
        },
    });

    for profile in ctx.profiles.list() {
        checks.push(profile_image_check(ctx, &profile).await);
    }

    checks.push(match ctx.wargame.healthcheck().await {
        Ok(status) => Check {
            name: format!("provider:{}", ctx.wargame.name()),
            status: CheckStatus::from_ok(status.reachable),
            detail: status.detail,
            hint: None,
        },
        Err(e) => Check {
            name: format!("provider:{}", ctx.wargame.name()),
            status: CheckStatus::from_ok(false),
            detail: e.to_string(),
            hint: e.hint(),
        },
    });

    let key_path = ctx.dirs.keys_dir().join("id_ed25519");
    let key_present = key_path.exists();
    checks.push(Check {
        name: "guest-key".to_string(),
        status: CheckStatus::from_ok(key_present),
        detail: key_path.display().to_string(),
        hint: if key_present {
            None
        } else {
            Some("wargame profile build <name>".to_string())
        },
    });

    checks
}

/// The `network:isolated:<backend>` row for one backend. `status` is `None`
/// when the healthcheck itself failed -- the backend row already reports
/// that, so this one stays neutral instead of double-reporting it.
fn isolated_network_check(backend: &str, status: Option<&crate::ports::BackendStatus>) -> Check {
    let name = format!("network:isolated:{backend}");
    match status.map(|s| (s.isolated_network, s.detail.clone())) {
        Some((Some(true), detail)) => Check {
            name,
            status: CheckStatus::from_ok(true),
            detail,
            hint: None,
        },
        Some((Some(false), detail)) => Check {
            name,
            status: CheckStatus::from_ok(false),
            detail,
            hint: Some(crate::error::isolated_network_hint(None)),
        },
        _ => Check {
            name,
            status: CheckStatus::from_ok(true),
            detail: "unknown".to_string(),
            hint: None,
        },
    }
}

/// `doctor --profile <name>`: only the checks *this* profile needs
/// (spec.md §3 FR-6). A macOS/Tart profile therefore never reports UTM as a
/// problem, and a UTM profile reports the UTM app, the automation grant and
/// the emulation mode it actually asked for.
///
/// Like [`doctor`], no check aborts the rest; the only `Err` is an unknown
/// profile name.
pub async fn doctor_profile(ctx: &AppContext, name: &str) -> Vec<Check> {
    let profile = match ctx.profiles.get(name) {
        Ok(profile) => profile,
        Err(e) => {
            return vec![Check {
                name: format!("profile:{name}"),
                status: CheckStatus::from_ok(false),
                detail: e.to_string(),
                hint: e.hint(),
            }]
        }
    };

    let mut checks = Vec::new();

    // 1. Resolution. Pure data, so it answers even with nothing installed.
    let resolved = match crate::app::backends::resolve_backend(&profile) {
        Ok(resolved) => resolved,
        Err(e) => {
            checks.push(Check {
                name: format!("profile:{name}"),
                status: CheckStatus::from_ok(false),
                detail: e.to_string(),
                hint: e.hint(),
            });
            return checks;
        }
    };
    checks.push(Check {
        name: format!("profile:{name}"),
        status: CheckStatus::from_ok(true),
        detail: resolved.describe(),
        hint: None,
    });

    // 2. Is that backend wired into this build at all?
    let vm = match ctx.backends.get(resolved.backend.as_str()) {
        Ok(vm) => vm,
        Err(e) => {
            checks.push(Check {
                name: format!("vm-backend:{}", resolved.backend),
                status: CheckStatus::from_ok(false),
                detail: e.to_string(),
                hint: e.hint(),
            });
            return checks;
        }
    };

    // 3. The backend binary and version, then whatever else the backend
    //    itself wants to report (its binary path, the host automation
    //    grant, ...).
    let healthcheck = vm.healthcheck().await;
    checks.push(match &healthcheck {
        Ok(status) => Check {
            name: format!("vm-backend:{}", vm.name()),
            status: CheckStatus::from_ok(status.available),
            detail: status
                .version
                .clone()
                .unwrap_or_else(|| status.detail.clone()),
            hint: if status.available {
                None
            } else {
                Some(format!("Install/start the {} VM backend", vm.name()))
            },
        },
        Err(e) => Check {
            name: format!("vm-backend:{}", vm.name()),
            status: CheckStatus::from_ok(false),
            detail: e.to_string(),
            hint: e.hint(),
        },
    });
    for check in vm.diagnostics().await {
        checks.push(Check {
            name: check.name,
            status: CheckStatus::from_ok(check.ok),
            detail: check.detail,
            hint: check.hint,
        });
    }

    // 4. Execution mode and network, against what the backend can do.
    let capabilities = vm.capabilities();
    let mode_ok = capabilities.supports_mode(resolved.mode);
    checks.push(Check {
        name: format!("mode:{}:{}", resolved.mode, vm.name()),
        status: CheckStatus::from_ok(mode_ok),
        detail: if mode_ok {
            format!("{} supports {}", vm.name(), resolved.mode)
        } else {
            format!("{} cannot run {} guests", vm.name(), resolved.mode)
        },
        hint: (!mode_ok).then(crate::domain::compat::supported_combinations_table),
    });

    let network = ctx.config.general.default_network;
    let network_supported = capabilities.supports_network(network);
    if !network_supported {
        checks.push(Check {
            name: format!("network:{network}:{}", vm.name()),
            status: CheckStatus::from_ok(false),
            detail: format!("{} cannot configure {network} networking", vm.name()),
            hint: Some("Launch with --network nat, or pick another profile".to_string()),
        });
    } else if network == NetworkMode::Isolated {
        checks.push(isolated_network_check(vm.name(), healthcheck.as_ref().ok()));
    } else {
        checks.push(Check {
            name: format!("network:{network}:{}", vm.name()),
            status: CheckStatus::from_ok(true),
            detail: format!("{} supports {network}", vm.name()),
            hint: None,
        });
    }

    // 5. Is there an image to launch from?
    checks.push(profile_image_check(ctx, &profile).await);

    // 6. Room for this profile's guest, judged exactly the way `profile
    //    create` judges it -- same volume, same floor, same reasoning
    //    (task-7-b2b-report.md D6). Before this, `doctor` measured
    //    `$WARGAME_HOME` against the profile's *nominal* disk and reported a
    //    hard failure for a profile `profile create` accepts with a warning,
    //    with the "lower resources.disk_gib" advice that fix deliberately
    //    removed.
    checks.push(match crate::app::profile_build::free_space_for(ctx, vm.as_ref()) {
        Ok((storage, free)) => {
            let gib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            let free_gib = gib(free);
            let nominal = profile.resources.disk_gib;
            let floor = crate::app::profile_build::MIN_FREE_GIB;
            let enough = free_gib >= floor as f64;
            Check {
                name: format!("disk-space:{name}"),
                status: CheckStatus::from_ok(enough),
                detail: if !enough {
                    format!(
                        "{free_gib:.1} GiB free on the volume that holds {}, and an OS install needs at least {floor} GiB there",
                        storage.display()
                    )
                } else if free_gib < nominal as f64 {
                    format!(
                        "{free_gib:.1} GiB free on the volume that holds {}, less than this profile's {nominal} GiB disk -- allowed, because a guest disk is sparse and grows as it is used, but the guest fails once the volume fills",
                        storage.display()
                    )
                } else {
                    format!(
                        "{free_gib:.1} GiB free on the volume that holds {}, for a {nominal} GiB guest disk",
                        storage.display()
                    )
                },
                hint: (!enough).then(|| {
                    "Free space on that volume. A guest disk grows as it is used, so lowering resources.disk_gib will not help until it has room.".to_string()
                }),
            }
        }
        Err(e) => Check {
            name: format!("disk-space:{name}"),
            status: CheckStatus::from_ok(false),
            detail: e.to_string(),
            hint: None,
        },
    });

    // 7. The tools that talk to *this* guest.
    checks.push(binary_check(ctx, "ssh", "ssh", Some("Install OpenSSH")));
    checks.push(binary_check(ctx, "scp", "scp", Some("Install OpenSSH")));
    let shell = GuestShell::for_os(profile.guest.os);
    let transport = ctx.transports.for_shell(shell);
    checks.push(Check {
        name: format!("guest-shell:{shell}"),
        status: CheckStatus::from_ok(transport.is_ok()),
        detail: match &transport {
            Ok(_) => format!("{shell} transport wired for this guest"),
            Err(e) => e.to_string(),
        },
        hint: transport.err().and_then(|e| e.hint()),
    });

    checks
}

/// The `profile-image:<name>` row: which image this profile would launch
/// from, whether the backend actually has it, and whether the profile or its
/// scripts changed since it was built (spec.md §3 FR-1/FR-3).
async fn profile_image_check(ctx: &AppContext, profile: &Profile) -> Check {
    let name = profile.name.clone();
    // The same overview `profiles` and the TUI's picker render: whether an
    // image is actually launchable is one decision, made in one place.
    let overview = crate::app::profiles::overview(ctx, profile).await;
    let check = |status: CheckStatus, detail: String, hint: Option<String>| Check {
        name: format!("profile-image:{name}"),
        status,
        detail,
        hint,
    };
    let Some(image) = overview.image() else {
        // The backend could not answer at all: a real problem, not a
        // profile nobody has set up.
        let (detail, hint) = overview.image_error().unwrap_or(("unknown", None));
        return check(
            CheckStatus::Fail,
            detail.to_string(),
            hint.map(str::to_string),
        );
    };
    // A profile nobody has built here is not a broken host -- it is one you
    // have not set up. A build that *was* started and did not finish, or an
    // image that has gone missing since, stays a failure.
    let status = match (image.ready, image.never_built) {
        (true, _) => CheckStatus::Pass,
        (false, true) => CheckStatus::NotSetUp,
        (false, false) => CheckStatus::Fail,
    };
    // Every row leads with what the profile resolved to, so `doctor` names
    // the guest OS, architecture, backend and execution mode next to the
    // image state rather than only the last of those (spec.md §3 FR-1).
    check(
        status,
        format!(
            "{}, {} — {}",
            overview.guest_label(),
            overview.backend_label(),
            image.detail
        ),
        (!image.ready).then(|| match (&image.installer_vm, image.needs_create) {
            // D9: this profile *has* its installation VM and the OS in it is
            // not finished. `profile create` would refuse with a source
            // conflict, so it is the one command not to suggest.
            (Some(vm), _) if image.never_built => format!(
                "wargame profile console {name}   # finish installing the OS in {vm}, then: wargame profile build {name}"
            ),
            (_, true) => format!(
                "wargame profile create {name} --iso <path>   # then: wargame profile build {name}"
            ),
            _ => format!("wargame profile build {name}"),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::app::BuilderRegistry;
    use crate::app::{BackendRegistry, TransportRegistry};
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
    use crate::infra::HostDiskSpace;
    use crate::ports::VmProvider;
    use crate::testing::MemoryBuildRepo;
    use crate::testing::{
        FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
    };
    use std::sync::Arc;

    fn tmp_path() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    fn fixture() -> AppContext {
        fixture_with_vm(Arc::new(FakeVmProvider::new()))
    }

    fn fixture_with_vm(vm: Arc<FakeVmProvider>) -> AppContext {
        AppContext {
            config: Default::default(),
            dirs: AppDirs { root: tmp_path() },
            wargame: Arc::new(FakeWargameProvider::new()),
            backends: BackendRegistry::new(vec![vm as Arc<dyn VmProvider>]),
            transports: TransportRegistry::new(Arc::new(FakeTransport::new())),
            repo: Arc::new(MemoryRepo::new()),
            builds: Arc::new(MemoryBuildRepo::new()),
            builders: BuilderRegistry::default(),
            disk: Arc::new(HostDiskSpace),
            profiles: ProfileStore::new(vec![], tmp_path()),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap: crate::app::BootstrapScripts::new("#!/bin/bash\n", "# ps\n"),
        }
    }

    #[tokio::test]
    async fn doctor_never_panics_and_returns_checks() {
        let ctx = fixture();
        let checks = doctor(&ctx).await;
        assert!(!checks.is_empty());
        assert!(checks.iter().any(|c| c.name == "guest-key"));
    }

    #[tokio::test]
    async fn doctor_reports_missing_binaries_without_failing() {
        let ctx = fixture();
        let checks = doctor(&ctx).await;
        let sshpass = checks.iter().find(|c| c.name == "sshpass").unwrap();
        // FakeRunner has nothing registered via `which`, so this should be a
        // (non-fatal) miss with a hint, never a panic or an early return.
        assert!(!sshpass.passed());
        assert!(sshpass.hint.is_some());
    }

    #[tokio::test]
    async fn doctor_reports_network_isolated_ok_when_available() {
        let vm = Arc::new(FakeVmProvider::new());
        vm.set_isolated_network(Some(true));
        let ctx = fixture_with_vm(vm);
        let checks = doctor(&ctx).await;
        let check = checks
            .iter()
            .find(|c| c.name == "network:isolated:tart")
            .unwrap();
        assert!(check.passed());
        assert!(check.hint.is_none());
    }

    #[tokio::test]
    async fn doctor_reports_network_isolated_failing_with_hint_when_unavailable() {
        let vm = Arc::new(FakeVmProvider::new());
        vm.set_isolated_network(Some(false));
        let ctx = fixture_with_vm(vm);
        let checks = doctor(&ctx).await;
        let check = checks
            .iter()
            .find(|c| c.name == "network:isolated:tart")
            .unwrap();
        assert!(!check.passed());
        assert!(check
            .hint
            .as_deref()
            .unwrap()
            .to_lowercase()
            .contains("softnet"));
    }

    #[tokio::test]
    async fn doctor_reports_network_isolated_ok_unknown_when_backend_cant_tell() {
        let ctx = fixture();
        // Default FakeVmProvider reports `None`.
        let checks = doctor(&ctx).await;
        let check = checks
            .iter()
            .find(|c| c.name == "network:isolated:tart")
            .unwrap();
        assert!(check.passed());
        assert_eq!(check.detail, "unknown");
    }

    #[tokio::test]
    async fn doctor_reports_missing_profile_images() {
        use crate::domain::{
            GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, Resources, VmSettings, WorkspaceSpec,
        };
        let mut ctx = fixture();
        ctx.profiles = ProfileStore::new(
            vec![Profile {
                name: "macos-minimal-arm64".to_string(),
                description: String::new(),
                vm: VmSettings::default(),
                guest: GuestSpec {
                    os: GuestOs::Macos,
                    arch: GuestArch::Arm64,
                    distribution: None,
                    user: "admin".to_string(),
                    password: None,
                },
                image: ImageSpec {
                    base: "base".to_string(),
                },
                resources: Resources {
                    cpu: 2,
                    memory_mib: 4096,
                    disk_gib: 20,
                },
                provision: Default::default(),
                tools: Default::default(),
                workspace: WorkspaceSpec {
                    root: "/Users/admin/wargame".to_string(),
                },
            }],
            tmp_path(),
        );
        let checks = doctor(&ctx).await;
        let profile_check = checks
            .iter()
            .find(|c| c.name == "profile-image:macos-minimal-arm64")
            .unwrap();
        assert!(!profile_check.passed());
        // Nobody has built it, so it is not a *failure*: see
        // `an_unbuilt_profile_is_not_set_up_rather_than_broken`.
        assert_eq!(profile_check.status, CheckStatus::NotSetUp);
    }

    /// A macOS-only host is not faulty for shipping Kali and Windows
    /// templates it has no ISO for. Those rows are informational, so the
    /// `✗` rows that remain mean something (spec.md §6's error UX).
    #[tokio::test]
    async fn an_unbuilt_profile_is_not_set_up_rather_than_broken() {
        let mut ctx = fixture();
        ctx.backends = BackendRegistry::new(vec![
            Arc::new(FakeVmProvider::new()) as Arc<dyn VmProvider>,
            Arc::new(FakeVmProvider::with_backend("utm")) as Arc<dyn VmProvider>,
        ]);
        ctx.profiles = ProfileStore::new(vec![utm_profile()], tmp_path());

        let checks = doctor(&ctx).await;
        let row = checks
            .iter()
            .find(|c| c.name == "profile-image:kali-x86_64")
            .unwrap();
        assert_eq!(row.status, CheckStatus::NotSetUp, "{row:?}");
        assert!(!row.failed());
        // Still says what to do about it -- informational is not silent.
        assert!(
            row.hint.as_deref().unwrap().contains("profile create"),
            "{row:?}"
        );
        assert!(
            !checks
                .iter()
                .any(|c| c.failed() && c.name.starts_with("profile-image:")),
            "no profile row is a failure on an untouched host: {checks:?}"
        );
    }

    /// **D6.** `profile create` and `doctor --profile` have to agree.
    /// `doctor` measured `$WARGAME_HOME` against the profile's *nominal*
    /// disk, so it reported a hard failure for a profile `profile create`
    /// accepts with a warning -- and repeated the "lower resources.disk_gib"
    /// advice that fix removed (task-7-b2b-report.md D6).
    #[tokio::test]
    async fn the_profile_disk_check_measures_the_backends_volume_against_the_same_floor() {
        let profile = utm_profile(); // asks for 80 GiB
        let vm = Arc::new(FakeVmProvider::with_backend("utm"));
        vm.set_storage_dir("/Users/x/Library/Containers/com.utmapp.UTM/Data/Documents");
        let mut ctx = fixture_with_vm(vm);
        ctx.profiles = ProfileStore::new(vec![profile.clone()], tmp_path());

        // Less than the profile's disk, more than the floor: allowed, and
        // said so -- exactly what `profile create` does.
        ctx.disk = Arc::new(FixedDisk(20 * 1024 * 1024 * 1024));
        let row = doctor_profile(&ctx, "kali-x86_64")
            .await
            .into_iter()
            .find(|c| c.name == "disk-space:kali-x86_64")
            .expect("a per-profile disk row");
        assert_eq!(row.status, CheckStatus::Pass, "{row:?}");
        assert!(row.detail.contains("com.utmapp.UTM"), "{row:?}");
        assert!(row.hint.is_none(), "{row:?}");

        // Below the floor an install cannot work at all.
        ctx.disk = Arc::new(FixedDisk(4 * 1024 * 1024 * 1024));
        let row = doctor_profile(&ctx, "kali-x86_64")
            .await
            .into_iter()
            .find(|c| c.name == "disk-space:kali-x86_64")
            .expect("a per-profile disk row");
        assert_eq!(row.status, CheckStatus::Fail, "{row:?}");
        assert!(row.detail.contains("at least 8 GiB"), "{row:?}");
        // The advice B2a removed must not come back.
        assert!(
            !row.hint
                .as_deref()
                .unwrap_or_default()
                .contains("lower resources.disk_gib in the profile"),
            "{row:?}"
        );
    }

    /// **D9.** A profile whose installation VM exists and whose OS install
    /// is unfinished was told to run `wargame profile create`, the one
    /// command that refuses it (task-7-b2b-report.md D9).
    #[tokio::test]
    async fn an_unfinished_install_points_at_the_console_not_at_profile_create() {
        use crate::domain::{BuildPhase, BuildRecord, BuildSource, VmRef};

        let profile = utm_profile();
        let mut ctx = fixture_with_vm(Arc::new(FakeVmProvider::with_backend("utm")));
        ctx.profiles = ProfileStore::new(vec![profile.clone()], tmp_path());

        let mut record = BuildRecord::new(
            &profile,
            "utm",
            BuildSource::Iso {
                path: "/tmp/kali.iso".into(),
                sha256: None,
                verified_sha256: false,
            },
        );
        record.installer_vm = Some(VmRef {
            backend: "utm".to_string(),
            id: "6F78EF1A-5B93-4AFF-AAA1-775BE6C666BC".to_string(),
            name: "wg-build-kali-x86_64".to_string(),
        });
        record.set_phase(BuildPhase::AwaitingInstall);
        ctx.builds.save(&record).await.unwrap();

        let row = doctor_profile(&ctx, "kali-x86_64")
            .await
            .into_iter()
            .find(|c| c.name == "profile-image:kali-x86_64")
            .expect("an image row");
        // Still "not set up", not a broken host.
        assert_eq!(row.status, CheckStatus::NotSetUp, "{row:?}");
        let hint = row.hint.clone().unwrap_or_default();
        assert!(!hint.contains("profile create"), "{hint}");
        assert!(
            hint.contains("wargame profile console kali-x86_64"),
            "{hint}"
        );
        assert!(hint.contains("wg-build-kali-x86_64"), "{hint}");
        assert!(
            hint.contains("6F78EF1A-5B93-4AFF-AAA1-775BE6C666BC"),
            "{hint}"
        );
        assert!(hint.contains("wargame profile build kali-x86_64"), "{hint}");
    }

    /// ...but a build that *was* started and whose image is not there is a
    /// real problem, and stays one. Nothing is hidden.
    #[tokio::test]
    async fn a_built_profile_whose_image_went_missing_is_a_failure() {
        use crate::domain::{BuildPhase, BuildRecord, BuildSource, PublishedImage};

        let mut ctx = fixture();
        let profile = utm_profile();
        ctx.backends = BackendRegistry::new(vec![
            Arc::new(FakeVmProvider::new()) as Arc<dyn VmProvider>,
            Arc::new(FakeVmProvider::with_backend("utm")) as Arc<dyn VmProvider>,
        ]);
        ctx.profiles = ProfileStore::new(vec![profile.clone()], tmp_path());

        let mut record = BuildRecord::new(
            &profile,
            "utm",
            BuildSource::Iso {
                path: "/tmp/kali.iso".into(),
                sha256: None,
                verified_sha256: false,
            },
        );
        record.image = Some(PublishedImage::new("uuid-1", "wg-build-kali"));
        record.set_phase(BuildPhase::Ready);
        ctx.builds.save(&record).await.unwrap();

        let checks = doctor(&ctx).await;
        let row = checks
            .iter()
            .find(|c| c.name == "profile-image:kali-x86_64")
            .unwrap();
        assert_eq!(row.status, CheckStatus::Fail, "{row:?}");
        // D7: and it says the backend no longer has it, with the one
        // command that fixes it -- never `profile create`, which would
        // refuse.
        assert!(row.detail.contains("no longer has it"), "{row:?}");
        assert_eq!(
            row.hint.as_deref(),
            Some("wargame profile build kali-x86_64")
        );
        let overview = crate::app::profiles::overview(&ctx, &profile).await;
        assert_eq!(overview.image_state(), Some("missing"));
    }

    /// Two wired backends must not collapse into one `network:isolated`
    /// row: with a shared name the second backend's answer would silently
    /// overwrite the first's in every reader that looks a row up by name.
    #[tokio::test]
    async fn doctor_names_the_isolated_network_row_per_backend() {
        let tart = Arc::new(FakeVmProvider::new());
        tart.set_isolated_network(Some(true));
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        utm.set_isolated_network(Some(false));
        let mut ctx = fixture();
        ctx.backends = BackendRegistry::new(vec![
            tart as Arc<dyn VmProvider>,
            utm as Arc<dyn VmProvider>,
        ]);

        let checks = doctor(&ctx).await;
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"network:isolated:tart"), "{names:?}");
        assert!(names.contains(&"network:isolated:utm"), "{names:?}");
        assert!(checks
            .iter()
            .find(|c| c.name == "network:isolated:tart")
            .unwrap()
            .passed());
        assert!(!checks
            .iter()
            .find(|c| c.name == "network:isolated:utm")
            .unwrap()
            .passed());
        // Every row name is unique, so a reader can look one up by name.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate rows in {names:?}");
    }

    /// A volume with a fixed amount of room, so a disk check's verdict is
    /// the test's to choose.
    struct FixedDisk(u64);

    impl crate::ports::DiskSpace for FixedDisk {
        fn free_bytes(&self, _path: &std::path::Path) -> crate::error::Result<u64> {
            Ok(self.0)
        }
    }

    fn utm_profile() -> crate::domain::Profile {
        use crate::domain::{
            BackendChoice, ExecutionMode, GuestArch, GuestOs, GuestSpec, ImageSpec, Profile,
            Resources, VmSettings, WorkspaceSpec,
        };
        Profile {
            name: "kali-x86_64".to_string(),
            description: String::new(),
            vm: VmSettings {
                backend: BackendChoice::Utm,
                mode: ExecutionMode::Emulate,
            },
            guest: GuestSpec {
                os: GuestOs::Linux,
                arch: GuestArch::X86_64,
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
            provision: Default::default(),
            tools: Default::default(),
            workspace: WorkspaceSpec {
                root: "/home/kali/wargame".to_string(),
            },
        }
    }

    /// A Tart/macOS profile must never be told to install UTM (spec.md §3
    /// FR-6: "Tart macOS 실행에 UTM 설치를 요구하지 않는다").
    #[tokio::test]
    async fn doctor_profile_checks_only_the_backend_the_profile_resolves_to() {
        use crate::domain::{
            GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, Resources, VmSettings, WorkspaceSpec,
        };
        let mut ctx = fixture();
        ctx.profiles = ProfileStore::new(
            vec![Profile {
                name: "macos-minimal-arm64".to_string(),
                description: String::new(),
                vm: VmSettings::default(),
                guest: GuestSpec {
                    os: GuestOs::Macos,
                    arch: GuestArch::Arm64,
                    distribution: None,
                    user: "admin".to_string(),
                    password: None,
                },
                image: ImageSpec {
                    base: "base".to_string(),
                },
                resources: Resources {
                    cpu: 2,
                    memory_mib: 4096,
                    disk_gib: 20,
                },
                provision: Default::default(),
                tools: Default::default(),
                workspace: WorkspaceSpec {
                    root: "/Users/admin/wargame".to_string(),
                },
            }],
            tmp_path(),
        );

        let checks = doctor_profile(&ctx, "macos-minimal-arm64").await;
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"vm-backend:tart"), "{names:?}");
        assert!(
            !names.iter().any(|n| n.contains("utm")),
            "a macOS/Tart profile must not check UTM: {names:?}"
        );
        assert!(names.contains(&"profile:macos-minimal-arm64"), "{names:?}");
        assert!(
            names.contains(&"disk-space:macos-minimal-arm64"),
            "{names:?}"
        );
        assert!(names.contains(&"guest-shell:posix"), "{names:?}");
    }

    /// The UTM profile's rows come from the UTM backend, including whatever
    /// backend-specific diagnostics it reports.
    #[tokio::test]
    async fn doctor_profile_reports_the_utm_backend_and_its_diagnostics() {
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        utm.set_capabilities(crate::domain::ProviderCapabilities {
            guest_os: vec![crate::domain::GuestOs::Linux],
            guest_arch: vec![crate::domain::GuestArch::X86_64],
            modes: vec![crate::domain::ExecutionMode::Emulate],
            networks: vec![NetworkMode::Isolated, NetworkMode::Nat],
            graphical_console: true,
            ssh: true,
            snapshots: false,
            copy_in: true,
            remote: false,
        });
        let mut ctx = fixture();
        ctx.backends = BackendRegistry::new(vec![utm as Arc<dyn VmProvider>]);
        ctx.profiles = ProfileStore::new(vec![utm_profile()], tmp_path());

        let checks = doctor_profile(&ctx, "kali-x86_64").await;
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"vm-backend:utm"), "{names:?}");
        assert!(names.contains(&"mode:emulate:utm"), "{names:?}");
        assert!(names.contains(&"network:isolated:utm"), "{names:?}");
        assert!(names.contains(&"profile-image:kali-x86_64"), "{names:?}");
        assert!(!names.contains(&"vm-backend:tart"), "{names:?}");
    }

    /// A profile whose backend this build has no adapter for is reported as
    /// exactly that, not as a missing image or a failed healthcheck.
    #[tokio::test]
    async fn doctor_profile_names_an_unwired_backend() {
        let mut ctx = fixture();
        ctx.profiles = ProfileStore::new(vec![utm_profile()], tmp_path());
        let checks = doctor_profile(&ctx, "kali-x86_64").await;
        let backend_row = checks
            .iter()
            .find(|c| c.name == "vm-backend:utm")
            .expect("the unwired backend is reported");
        assert!(!backend_row.passed());
        assert!(backend_row.detail.contains("utm"), "{backend_row:?}");
    }

    #[tokio::test]
    async fn doctor_profile_reports_an_unknown_profile_instead_of_erroring() {
        let ctx = fixture();
        let checks = doctor_profile(&ctx, "nope").await;
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].passed());
        assert_eq!(checks[0].name, "profile:nope");
    }
}
