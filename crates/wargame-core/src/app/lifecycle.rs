//! Session lifecycle operations (DESIGN.md §3.1's `app/lifecycle.rs`;
//! MVP.md §17-21, §37): list/inspect/stop/resume/delete/keep/prune. Built
//! entirely on [`crate::ports::VmProvider`] and
//! [`crate::ports::SessionRepository`], so it runs the same way against a
//! real Tart backend or `crate::testing`'s fakes.

use std::collections::HashSet;
use std::time::Duration;

use crate::domain::{KeepPolicy, Session, SessionState};
use crate::error::{Result, WargameError};
use crate::ports::{StartOptions, VmStatus};

use super::AppContext;

pub async fn list_sessions(ctx: &AppContext) -> Result<Vec<Session>> {
    ctx.repo.list().await
}

pub async fn inspect(ctx: &AppContext, id: &str) -> Result<Session> {
    ctx.repo.load(id).await
}

/// Stops a `Ready` or `Running` session's VM.
///
/// A freshly launched session is `Ready`, and DESIGN.md §3.2's transition
/// table only allows `Running -> Stopped` directly (not `Ready ->
/// Stopped`); a `Ready` session is therefore first (validly) advanced
/// through `Running` before the final `-> Stopped` move.
pub async fn stop(ctx: &AppContext, id: &str) -> Result<Session> {
    let mut session = ctx.repo.load(id).await?;
    if !session.state.is_up() {
        return Err(WargameError::InvalidTransition {
            from: session.state,
            to: SessionState::Stopped,
        });
    }
    if let Some(vm_ref) = &session.vm_ref {
        ctx.vm.stop(vm_ref).await?;
    }
    if session.state == SessionState::Ready {
        session.transition(SessionState::Running)?;
    }
    session.transition(SessionState::Stopped)?;
    ctx.repo.save(&session).await?;
    Ok(session)
}

/// Resumes a `Stopped` session: starts its VM headless with the network
/// mode it was originally launched with, waits for a guest IP and for the
/// guest transport to answer, then marks it `Running`.
pub async fn resume(ctx: &AppContext, id: &str) -> Result<Session> {
    let mut session = ctx.repo.load(id).await?;
    if session.state != SessionState::Stopped {
        return Err(WargameError::InvalidTransition {
            from: session.state,
            to: SessionState::Running,
        });
    }
    let vm_ref = session
        .vm_ref
        .clone()
        .ok_or_else(|| WargameError::VmCreationFailed("session has no VM".to_string()))?;

    let boot_timeout = Duration::from_secs(ctx.config.vm.tart.boot_timeout_secs);
    let start_opts = StartOptions {
        network: session.network,
        headless: true,
        log_path: ctx.repo.session_dir(id).join("run.log"),
    };
    let runtime = ctx.vm.start(&vm_ref, &start_opts).await?;
    session.runner_pid = runtime.pid;

    let ip = ctx.vm.wait_ip(&vm_ref, boot_timeout).await?;
    session.guest_ip = Some(ip);
    ctx.transport.wait_ready(ip, boot_timeout).await?;

    session.transition(SessionState::Running)?;
    ctx.repo.save(&session).await?;
    Ok(session)
}

/// Deletes a session's VM (if any) and its metadata. A `Keep`-policy
/// session refuses unless `force` is set (NFR-07).
pub async fn delete(ctx: &AppContext, id: &str, force: bool) -> Result<()> {
    let mut session = ctx.repo.load(id).await?;
    if session.keep == KeepPolicy::Keep && !force {
        return Err(WargameError::Other(
            "session is kept; use --force".to_string(),
        ));
    }
    session.transition(SessionState::Deleting)?;
    ctx.repo.save(&session).await?;

    if let Some(vm_ref) = &session.vm_ref {
        // `status()` failing is treated as "not missing" so we still try the
        // delete rather than silently leaving a VM behind.
        let status = ctx.vm.status(vm_ref).await.unwrap_or(VmStatus::Stopped);
        if status != VmStatus::Missing {
            ctx.vm.delete(vm_ref).await?;
        }
    }

    ctx.repo.delete(&session.id).await?;
    Ok(())
}

pub async fn keep(ctx: &AppContext, id: &str, keep: bool) -> Result<Session> {
    let mut session = ctx.repo.load(id).await?;
    session.keep = if keep {
        KeepPolicy::Keep
    } else {
        KeepPolicy::Discard
    };
    ctx.repo.save(&session).await?;
    Ok(session)
}

/// Deletes (or, if `dry_run`, just reports) VMs the backend manages that
/// have no corresponding session record (MVP.md §37 / NFR-02). Returns the
/// names of every orphan found.
pub async fn prune(ctx: &AppContext, dry_run: bool) -> Result<Vec<String>> {
    let vms = ctx.vm.list().await?;
    let sessions = ctx.repo.list().await?;
    let known: HashSet<String> = sessions
        .iter()
        .filter_map(|s| s.vm_ref.as_ref().map(|v| v.name.clone()))
        .collect();

    let mut orphans = Vec::new();
    for vm_ref in vms {
        if known.contains(&vm_ref.name) {
            continue;
        }
        orphans.push(vm_ref.name.clone());
        if !dry_run {
            ctx.vm.delete(&vm_ref).await?;
        }
    }
    Ok(orphans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::domain::{
        Challenge, ChallengeRef, ChallengeSummary, GuestArch, GuestOs, NetworkMode, Resources,
    };
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
    use crate::ports::{ImageRef, VmProvider, VmSpec};
    use crate::testing::{
        FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
    };
    use std::sync::Arc;
    use time::OffsetDateTime;

    fn tmp_path() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    struct Fixture {
        ctx: AppContext,
        vm: Arc<FakeVmProvider>,
    }

    fn fixture() -> Fixture {
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
        Fixture { ctx, vm }
    }

    fn sample_session(id: &str, state: SessionState, keep: KeepPolicy) -> Session {
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
            keep,
            last_error: None,
            runner_pid: None,
            guest_ip: None,
        }
    }

    #[tokio::test]
    async fn stop_moves_ready_through_running_to_stopped() {
        let Fixture { ctx, vm } = fixture();
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
        let mut session = sample_session("s", SessionState::Ready, KeepPolicy::Discard);
        session.vm_ref = Some(vm_ref);
        ctx.repo.save(&session).await.unwrap();

        let stopped = stop(&ctx, "s").await.unwrap();
        assert_eq!(stopped.state, SessionState::Stopped);
    }

    #[tokio::test]
    async fn stop_on_stopped_session_is_invalid() {
        let Fixture { ctx, .. } = fixture();
        let session = sample_session("s", SessionState::Stopped, KeepPolicy::Discard);
        ctx.repo.save(&session).await.unwrap();
        assert!(matches!(
            stop(&ctx, "s").await.unwrap_err(),
            WargameError::InvalidTransition { .. }
        ));
    }

    #[tokio::test]
    async fn resume_starts_the_vm_and_marks_running() {
        let Fixture { ctx, vm } = fixture();
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
        let mut session = sample_session("s", SessionState::Stopped, KeepPolicy::Discard);
        session.vm_ref = Some(vm_ref);
        ctx.repo.save(&session).await.unwrap();

        let resumed = resume(&ctx, "s").await.unwrap();
        assert_eq!(resumed.state, SessionState::Running);
        assert!(resumed.guest_ip.is_some());
    }

    #[tokio::test]
    async fn delete_refuses_kept_session_without_force() {
        let Fixture { ctx, .. } = fixture();
        let session = sample_session("s", SessionState::Ready, KeepPolicy::Keep);
        ctx.repo.save(&session).await.unwrap();

        let err = delete(&ctx, "s", false).await.unwrap_err();
        assert!(matches!(err, WargameError::Other(_)));
        assert!(ctx.repo.load("s").await.is_ok());

        delete(&ctx, "s", true).await.unwrap();
        assert!(ctx.repo.load("s").await.is_err());
    }

    #[tokio::test]
    async fn delete_removes_vm_and_session() {
        let Fixture { ctx, vm } = fixture();
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
        let mut session = sample_session("s", SessionState::Ready, KeepPolicy::Discard);
        session.vm_ref = Some(vm_ref);
        ctx.repo.save(&session).await.unwrap();

        delete(&ctx, "s", false).await.unwrap();
        assert!(ctx.repo.load("s").await.is_err());
        assert!(vm.vm_names().is_empty());
    }

    #[tokio::test]
    async fn keep_flips_the_policy() {
        let Fixture { ctx, .. } = fixture();
        let session = sample_session("s", SessionState::Ready, KeepPolicy::Discard);
        ctx.repo.save(&session).await.unwrap();

        let kept = keep(&ctx, "s", true).await.unwrap();
        assert_eq!(kept.keep, KeepPolicy::Keep);
        let unkept = keep(&ctx, "s", false).await.unwrap();
        assert_eq!(unkept.keep, KeepPolicy::Discard);
    }

    #[tokio::test]
    async fn prune_finds_and_deletes_orphan_vms() {
        let Fixture { ctx, vm } = fixture();
        // A VM with no session record at all.
        vm.create(&VmSpec {
            name: "wg-orphan".to_string(),
            image: ImageRef("img".to_string()),
            resources: Resources {
                cpu: 1,
                memory_mib: 1024,
                disk_gib: 10,
            },
        })
        .await
        .unwrap();
        // A VM that *is* tracked by a session.
        let tracked_ref = vm
            .create(&VmSpec {
                name: "wg-tracked".to_string(),
                image: ImageRef("img".to_string()),
                resources: Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            })
            .await
            .unwrap();
        let mut session = sample_session("tracked", SessionState::Ready, KeepPolicy::Discard);
        session.vm_ref = Some(tracked_ref);
        ctx.repo.save(&session).await.unwrap();

        let dry = prune(&ctx, true).await.unwrap();
        assert_eq!(dry, vec!["wg-orphan".to_string()]);
        assert_eq!(vm.vm_names().len(), 2, "dry run must not delete anything");

        let orphans = prune(&ctx, false).await.unwrap();
        assert_eq!(orphans, vec!["wg-orphan".to_string()]);
        assert_eq!(vm.vm_names(), vec!["wg-tracked".to_string()]);
    }
}
