//! Environment diagnostics (DESIGN.md §3.1's `app/doctor.rs`; DESIGN.md
//! §7's check list): a flat list of independent checks. No individual
//! check failing stops the others from running, and [`doctor`] itself
//! never returns `Err` — a failed check is just a `Check { ok: false, .. }`
//! row for the CLI/TUI to render.

use crate::ports::ImageRef;

use super::AppContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub hint: Option<String>,
}

fn binary_check(ctx: &AppContext, name: &str, program: &str, hint: Option<&str>) -> Check {
    match ctx.runner.which(program) {
        Some(path) => Check {
            name: name.to_string(),
            ok: true,
            detail: format!("found at {}", path.display()),
            hint: None,
        },
        None => Check {
            name: name.to_string(),
            ok: false,
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
        ok: cfg!(target_os = "macos") && std::env::consts::ARCH == "aarch64",
        detail: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        hint: if cfg!(target_os = "macos") {
            None
        } else {
            Some("wargame-vm requires macOS on Apple Silicon".to_string())
        },
    });

    let healthcheck = ctx.vm.healthcheck().await;
    checks.push(match &healthcheck {
        Ok(status) => Check {
            name: format!("vm-backend:{}", ctx.vm.name()),
            ok: status.available,
            detail: status
                .version
                .clone()
                .unwrap_or_else(|| status.detail.clone()),
            hint: if status.available {
                None
            } else {
                Some(format!("Install/start the {} VM backend", ctx.vm.name()))
            },
        },
        Err(e) => Check {
            name: format!("vm-backend:{}", ctx.vm.name()),
            ok: false,
            detail: e.to_string(),
            hint: e.hint(),
        },
    });

    checks.push(match &healthcheck {
        Ok(status) => match status.isolated_network {
            Some(true) => Check {
                name: "network:isolated".to_string(),
                ok: true,
                detail: status.detail.clone(),
                hint: None,
            },
            Some(false) => Check {
                name: "network:isolated".to_string(),
                ok: false,
                detail: status.detail.clone(),
                hint: Some(crate::error::isolated_network_hint(None)),
            },
            None => Check {
                name: "network:isolated".to_string(),
                ok: true,
                detail: "unknown".to_string(),
                hint: None,
            },
        },
        Err(_) => Check {
            name: "network:isolated".to_string(),
            ok: true,
            detail: "unknown".to_string(),
            hint: None,
        },
    });

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
                ok: free_gib >= MIN_FREE_GIB,
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
            ok: false,
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
        ok: home_writable,
        detail: ctx.dirs.root.display().to_string(),
        hint: if home_writable {
            None
        } else {
            Some("Check permissions on $WARGAME_HOME".to_string())
        },
    });

    for profile in ctx.profiles.list() {
        let image = ImageRef(profile.image_name());
        let ok = ctx.vm.image_exists(&image).await.unwrap_or(false);
        checks.push(Check {
            name: format!("profile-image:{}", profile.name),
            ok,
            detail: image.0.clone(),
            hint: if ok {
                None
            } else {
                Some(format!("wargame profile build {}", profile.name))
            },
        });
    }

    checks.push(match ctx.wargame.healthcheck().await {
        Ok(status) => Check {
            name: format!("provider:{}", ctx.wargame.name()),
            ok: status.reachable,
            detail: status.detail,
            hint: None,
        },
        Err(e) => Check {
            name: format!("provider:{}", ctx.wargame.name()),
            ok: false,
            detail: e.to_string(),
            hint: e.hint(),
        },
    });

    let key_path = ctx.dirs.keys_dir().join("id_ed25519");
    let key_present = key_path.exists();
    checks.push(Check {
        name: "guest-key".to_string(),
        ok: key_present,
        detail: key_path.display().to_string(),
        hint: if key_present {
            None
        } else {
            Some("wargame profile build <name>".to_string())
        },
    });

    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
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
            vm,
            transport: Arc::new(FakeTransport::new()),
            repo: Arc::new(MemoryRepo::new()),
            profiles: ProfileStore::new(vec![], tmp_path()),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap_script: "#!/bin/bash\n",
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
        assert!(!sshpass.ok);
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
            .find(|c| c.name == "network:isolated")
            .unwrap();
        assert!(check.ok);
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
            .find(|c| c.name == "network:isolated")
            .unwrap();
        assert!(!check.ok);
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
            .find(|c| c.name == "network:isolated")
            .unwrap();
        assert!(check.ok);
        assert_eq!(check.detail, "unknown");
    }

    #[tokio::test]
    async fn doctor_reports_missing_profile_images() {
        use crate::domain::{
            GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, Resources, WorkspaceSpec,
        };
        let mut ctx = fixture();
        ctx.profiles = ProfileStore::new(
            vec![Profile {
                name: "macos-minimal-arm64".to_string(),
                description: String::new(),
                guest: GuestSpec {
                    os: GuestOs::Macos,
                    arch: GuestArch::Arm64,
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
        assert!(!profile_check.ok);
    }
}
