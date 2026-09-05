//! Console access (DESIGN.md §3.1's `app/console.rs`; DESIGN.md §5,
//! §7): interactive SSH into a session, and reopening its VM with a
//! graphical window.

use std::time::Duration;

use crate::domain::SessionState;
use crate::error::{Result, WargameError};
use crate::ports::{StartOptions, VmStatus};

use super::AppContext;

/// Builds (but does not spawn) the interactive `ssh` command for a session
/// that is up (`Ready`/`Running`) and has a known guest IP. The caller
/// `exec`s it so it replaces the current process (DESIGN.md §7).
pub async fn ssh_command(ctx: &AppContext, id: &str) -> Result<std::process::Command> {
    let session = ctx.repo.load(id).await?;
    if !session.state.is_up() {
        return Err(WargameError::InvalidTransition {
            from: session.state,
            to: SessionState::Running,
        });
    }
    let ip = session.guest_ip.ok_or_else(|| {
        WargameError::NetworkUnavailable("session has no guest IP yet".to_string())
    })?;
    Ok(ctx.transport.interactive_shell_cmd(ip))
}

/// Reopens a session's VM with a graphical console (DESIGN.md §5's
/// "delegate to Tart" decision): stops the current (headless) run if it is
/// active, restarts it with a window, and waits for the guest to answer
/// again before returning.
pub async fn open_gui(ctx: &AppContext, id: &str) -> Result<()> {
    let mut session = ctx.repo.load(id).await?;
    if !session.state.is_up() {
        return Err(WargameError::InvalidTransition {
            from: session.state,
            to: SessionState::Running,
        });
    }
    let vm_ref = session
        .vm_ref
        .clone()
        .ok_or_else(|| WargameError::VmCreationFailed("session has no VM".to_string()))?;

    if ctx.vm.status(&vm_ref).await.unwrap_or(VmStatus::Stopped) == VmStatus::Running {
        ctx.vm.stop(&vm_ref).await?;
    }

    let boot_timeout = Duration::from_secs(ctx.config.vm.tart.boot_timeout_secs);
    let start_opts = StartOptions {
        network: session.network,
        headless: false,
        log_path: ctx.repo.session_dir(id).join("run.log"),
    };
    let runtime = ctx.vm.start(&vm_ref, &start_opts).await?;
    session.runner_pid = runtime.pid;

    let ip = ctx.vm.wait_ip(&vm_ref, boot_timeout).await?;
    session.guest_ip = Some(ip);
    ctx.transport.wait_ready(ip, boot_timeout).await?;

    if session.state == SessionState::Ready {
        session.transition(SessionState::Running)?;
    }
    ctx.repo.save(&session).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::domain::{
        Challenge, ChallengeRef, ChallengeSummary, GuestArch, GuestOs, KeepPolicy, NetworkMode,
        Resources, Session,
    };
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
    use crate::ports::{ImageRef, VmProvider, VmSpec};
    use crate::testing::{
        FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
    };
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::sync::Arc;
    use time::OffsetDateTime;

    fn tmp_path() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    fn sample_session(id: &str, state: SessionState) -> Session {
        let summary = ChallengeSummary {
            r#ref: ChallengeRef::parse("dreamhack:1").unwrap(),
            slug: "s".to_string(),
            title: "s".to_string(),
            categories: vec![],
            difficulty: None,
        };
        Session {
            id: id.to_string(),
            display_name: "s".to_string(),
            challenge: summary.clone(),
            challenge_snapshot: Challenge {
                summary,
                description: String::new(),
                tags: vec![],
                assets: vec![],
                remote_endpoints: vec![],
                source_url: "https://example.invalid".to_string(),
            },
            vm_provider: "tart".to_string(),
            profile: "p".to_string(),
            guest_os: GuestOs::Macos,
            guest_arch: GuestArch::Arm64,
            network: NetworkMode::Isolated,
            resources: Resources {
                cpu: 1,
                memory_mib: 1024,
                disk_gib: 10,
            },
            created_at: OffsetDateTime::now_utc(),
            state,
            vm_ref: None,
            assets: vec![],
            keep: KeepPolicy::Discard,
            last_error: None,
            runner_pid: None,
            guest_ip: None,
        }
    }

    fn fixture() -> (AppContext, Arc<FakeVmProvider>) {
        let vm = Arc::new(FakeVmProvider::new());
        let ctx = AppContext {
            config: Default::default(),
            dirs: AppDirs { root: tmp_path() },
            wargame: Arc::new(FakeWargameProvider::new()),
            vm: vm.clone(),
            transport: Arc::new(FakeTransport::new()),
            repo: Arc::new(MemoryRepo::new()),
            profiles: ProfileStore::new(vec![], tmp_path()),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap_script: "#!/bin/bash\n",
        };
        (ctx, vm)
    }

    #[tokio::test]
    async fn ssh_command_requires_up_and_ip() {
        let (ctx, _vm) = fixture();
        let mut session = sample_session("s", SessionState::Ready);
        session.guest_ip = None;
        ctx.repo.save(&session).await.unwrap();
        assert!(matches!(
            ssh_command(&ctx, "s").await.unwrap_err(),
            WargameError::NetworkUnavailable(_)
        ));

        session.guest_ip = Some(IpAddr::from_str("10.0.0.5").unwrap());
        ctx.repo.save(&session).await.unwrap();
        assert!(ssh_command(&ctx, "s").await.is_ok());
    }

    #[tokio::test]
    async fn ssh_command_rejects_stopped_session() {
        let (ctx, _vm) = fixture();
        let session = sample_session("s", SessionState::Stopped);
        ctx.repo.save(&session).await.unwrap();
        assert!(matches!(
            ssh_command(&ctx, "s").await.unwrap_err(),
            WargameError::InvalidTransition { .. }
        ));
    }

    #[tokio::test]
    async fn open_gui_restarts_with_window_and_updates_ip() {
        let (ctx, vm) = fixture();
        let vm_ref = vm
            .create(&VmSpec {
                name: "wg-s".to_string(),
                image: ImageRef("img".to_string()),
                resources: Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            })
            .await
            .unwrap();
        let mut session = sample_session("s", SessionState::Ready);
        session.vm_ref = Some(vm_ref);
        ctx.repo.save(&session).await.unwrap();

        open_gui(&ctx, "s").await.unwrap();
        let after = ctx.repo.load("s").await.unwrap();
        assert_eq!(after.state, SessionState::Running);
        assert!(after.guest_ip.is_some());
    }
}
