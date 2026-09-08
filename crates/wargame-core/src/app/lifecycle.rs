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
    let mut sessions = Vec::new();
    for session in ctx.repo.list().await? {
        sessions.push(reconcile(ctx, session).await?);
    }
    Ok(sessions)
}

/// Turns what the user typed on the command line into a full session id.
///
/// `None` (the id was omitted) means the most recently created session --
/// the one the user almost certainly means right after `launch`. Otherwise
/// the needle matches, in order of preference: the exact id, a unique id
/// prefix, or a unique case-insensitive substring of the id or the
/// challenge title (`wargame ssh out_of_bound`). Several matches yield
/// [`WargameError::AmbiguousSession`] listing every candidate; none yields
/// [`WargameError::SessionNotFound`].
pub async fn resolve_session(ctx: &AppContext, needle: Option<&str>) -> Result<String> {
    let sessions = ctx.repo.list().await?;
    let Some(needle) = needle.map(str::trim).filter(|n| !n.is_empty()) else {
        return sessions
            .first()
            .map(|s| s.id.clone())
            .ok_or(WargameError::NoSessions);
    };
    if sessions.iter().any(|s| s.id == needle) {
        return Ok(needle.to_string());
    }
    let pick = |matching: Vec<&Session>| -> Option<Result<String>> {
        match matching.as_slice() {
            [] => None,
            [only] => Some(Ok(only.id.clone())),
            many => Some(Err(WargameError::AmbiguousSession {
                needle: needle.to_string(),
                candidates: many.iter().map(|s| s.id.clone()).collect(),
            })),
        }
    };
    if let Some(found) = pick(
        sessions
            .iter()
            .filter(|s| s.id.starts_with(needle))
            .collect(),
    ) {
        return found;
    }
    let lower = needle.to_ascii_lowercase();
    let loose: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            s.id.to_ascii_lowercase().contains(&lower)
                || s.display_name.to_ascii_lowercase().contains(&lower)
        })
        .collect();
    pick(loose).unwrap_or_else(|| Err(WargameError::SessionNotFound(needle.to_string())))
}

pub async fn inspect(ctx: &AppContext, id: &str) -> Result<Session> {
    let session = ctx.repo.load(id).await?;
    reconcile(ctx, session).await
}

/// Brings a session record back in line with what the backend reports.
///
/// The VM can stop without going through us: the user closes the Tart
/// window opened by `wargame gui`, shuts the guest down from inside, or
/// the runner dies. The record would otherwise say `Ready`/`Running`
/// forever, so `stop` fails ("VM is not running") and `resume` refuses
/// (`Running -> Running`). When an up session's VM is reported `Stopped`,
/// the record is moved to `Stopped` and saved. A `Missing` VM is a
/// different problem (delete/prune territory) and is left alone, as is a
/// backend that cannot answer.
pub async fn reconcile(ctx: &AppContext, mut session: Session) -> Result<Session> {
    if !session.state.is_up() {
        return Ok(session);
    }
    let Some(vm_ref) = &session.vm_ref else {
        return Ok(session);
    };
    if ctx.vm.status(vm_ref).await? != VmStatus::Stopped {
        return Ok(session);
    }
    tracing::info!(
        session = %session.id,
        vm = %vm_ref.name,
        "VM stopped outside of wargame; marking the session stopped"
    );
    if session.state == SessionState::Ready {
        session.transition(SessionState::Running)?;
    }
    session.transition(SessionState::Stopped)?;
    session.runner_pid = None;
    ctx.repo.save(&session).await?;
    Ok(session)
}

/// Stops a `Ready` or `Running` session's VM.
///
/// A freshly launched session is `Ready`, and DESIGN.md §3.2's transition
/// table only allows `Running -> Stopped` directly (not `Ready ->
/// Stopped`); a `Ready` session is therefore first (validly) advanced
/// through `Running` before the final `-> Stopped` move.
pub async fn stop(ctx: &AppContext, id: &str) -> Result<Session> {
    let recorded = ctx.repo.load(id).await?;
    let recorded_state = recorded.state;
    let mut session = reconcile(ctx, recorded).await?;
    if recorded_state.is_up() && session.state == SessionState::Stopped {
        // The VM had already exited (e.g. the Tart window was closed);
        // the record is now correct and there is nothing left to stop.
        return Ok(session);
    }
    if !session.state.is_up() {
        return Err(WargameError::InvalidTransition {
            from: session.state,
            to: SessionState::Stopped,
        });
    }
    if let Some(ip) = session.guest_ip {
        // Best-effort: flush the guest's filesystem before asking Tart to
        // stop it, so a stop that outruns its own graceful-shutdown window
        // doesn't lose writes that were still dirty in page cache
        // (DESIGN.md §5). A failure here must never block the stop itself.
        if let Err(err) = ctx.transport.exec(ip, &["sync".to_string()]).await {
            tracing::debug!(session = %id, error = %err, "sync before stop failed; stopping anyway");
        }
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
    let session = ctx.repo.load(id).await?;
    let mut session = reconcile(ctx, session).await?;
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
        return Err(WargameError::SessionKept(session.id.clone()));
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
        transport: Arc<FakeTransport>,
    }

    fn fixture() -> Fixture {
        let vm = Arc::new(FakeVmProvider::new());
        let transport = Arc::new(FakeTransport::new());
        let ctx = AppContext {
            config: Default::default(),
            dirs: AppDirs { root: tmp_path() },
            wargame: Arc::new(FakeWargameProvider::new()),
            vm: vm.clone(),
            transport: transport.clone(),
            repo: Arc::new(MemoryRepo::new()),
            profiles: ProfileStore::new(vec![], tmp_path()),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap_script: "#!/bin/bash\n",
        };
        Fixture { ctx, vm, transport }
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

    /// Closing the Tart window (or any out-of-band VM exit) leaves a
    /// `Ready`/`Running` record behind a VM that Tart reports stopped.
    /// The lifecycle ops must reconcile that instead of failing.
    fn drifted_fixture() -> Fixture {
        fixture()
    }

    async fn drifted_session(f: &Fixture, id: &str) {
        let vm_ref =
            f.vm.create(&VmSpec {
                name: format!("wg-{id}"),
                image: ImageRef("img".to_string()),
                resources: Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            })
            .await
            .unwrap();
        let opts = StartOptions {
            network: NetworkMode::Isolated,
            headless: true,
            log_path: tmp_path(),
        };
        f.vm.start(&vm_ref, &opts).await.unwrap();
        let mut session = sample_session(id, SessionState::Running, KeepPolicy::Discard);
        session.vm_ref = Some(vm_ref.clone());
        session.runner_pid = Some(4242);
        session.guest_ip = Some("10.0.0.9".parse().unwrap());
        f.ctx.repo.save(&session).await.unwrap();
        // The VM dies behind the record's back.
        f.vm.stop(&vm_ref).await.unwrap();
    }

    async fn seed(f: &Fixture, id: &str, title: &str, age_secs: i64) {
        let mut s = sample_session(id, SessionState::Stopped, KeepPolicy::Discard);
        s.display_name = title.to_string();
        s.created_at = OffsetDateTime::now_utc() - Duration::from_secs(age_secs as u64);
        f.ctx.repo.save(&s).await.unwrap();
    }

    #[tokio::test]
    async fn resolve_omitted_picks_the_newest_session() {
        let f = fixture();
        seed(&f, "dh-old-aaaaaa", "old", 600).await;
        seed(&f, "dh-new-bbbbbb", "new", 10).await;
        assert_eq!(
            resolve_session(&f.ctx, None).await.unwrap(),
            "dh-new-bbbbbb"
        );
        assert_eq!(
            resolve_session(&f.ctx, Some("  ")).await.unwrap(),
            "dh-new-bbbbbb"
        );
    }

    #[tokio::test]
    async fn resolve_omitted_with_no_sessions_says_so() {
        let f = fixture();
        assert!(matches!(
            resolve_session(&f.ctx, None).await,
            Err(WargameError::NoSessions)
        ));
    }

    #[tokio::test]
    async fn resolve_prefers_exact_then_prefix_then_substring() {
        let f = fixture();
        seed(&f, "dh-out-of-bound-a1b2c3", "out_of_bound", 30).await;
        seed(&f, "dh-out-of-bound-a1ffff", "out_of_bound", 20).await;
        seed(&f, "dh-rev-basic-1-9z9z9z", "rev-basic-1", 10).await;
        let r = |n: &'static str| resolve_session(&f.ctx, Some(n));

        assert_eq!(
            r("dh-out-of-bound-a1b2c3").await.unwrap(),
            "dh-out-of-bound-a1b2c3"
        );
        assert_eq!(
            r("dh-out-of-bound-a1f").await.unwrap(),
            "dh-out-of-bound-a1ffff"
        );
        assert_eq!(r("dh-rev").await.unwrap(), "dh-rev-basic-1-9z9z9z");
        // Substring of the id and of the title, case-insensitively.
        assert_eq!(r("9z9z").await.unwrap(), "dh-rev-basic-1-9z9z9z");
        assert_eq!(r("REV-BASIC").await.unwrap(), "dh-rev-basic-1-9z9z9z");
    }

    #[tokio::test]
    async fn resolve_reports_ambiguity_with_candidates() {
        let f = fixture();
        seed(&f, "dh-out-of-bound-a1b2c3", "out_of_bound", 30).await;
        seed(&f, "dh-out-of-bound-a1ffff", "out_of_bound", 20).await;
        match resolve_session(&f.ctx, Some("dh-out")).await {
            Err(WargameError::AmbiguousSession { needle, candidates }) => {
                assert_eq!(needle, "dh-out");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousSession, got {other:?}"),
        }
        assert!(matches!(
            resolve_session(&f.ctx, Some("nope")).await,
            Err(WargameError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn delete_refuses_a_kept_session_with_a_dedicated_error() {
        let f = fixture();
        f.ctx
            .repo
            .save(&sample_session(
                "k",
                SessionState::Stopped,
                KeepPolicy::Keep,
            ))
            .await
            .unwrap();
        assert!(matches!(
            delete(&f.ctx, "k", false).await,
            Err(WargameError::SessionKept(id)) if id == "k"
        ));
        assert!(f.ctx.repo.load("k").await.is_ok());
        delete(&f.ctx, "k", true).await.unwrap();
        assert!(f.ctx.repo.load("k").await.is_err());
    }

    #[tokio::test]
    async fn stop_reconciles_a_running_record_whose_vm_already_exited() {
        let f = drifted_fixture();
        drifted_session(&f, "s").await;
        let before = f.vm.calls().iter().filter(|c| *c == "stop").count();

        let stopped = stop(&f.ctx, "s").await.unwrap();
        assert_eq!(stopped.state, SessionState::Stopped);
        assert_eq!(stopped.runner_pid, None);
        assert_eq!(
            f.ctx.repo.load("s").await.unwrap().state,
            SessionState::Stopped
        );
        let after = f.vm.calls().iter().filter(|c| *c == "stop").count();
        assert_eq!(
            after, before,
            "must not ask the backend to stop a stopped VM"
        );
        assert!(
            f.transport.calls().is_empty(),
            "must not try to sync a guest that is gone"
        );
    }

    #[tokio::test]
    async fn resume_reconciles_a_running_record_whose_vm_already_exited() {
        let f = drifted_fixture();
        drifted_session(&f, "s").await;

        let resumed = resume(&f.ctx, "s").await.unwrap();
        assert_eq!(resumed.state, SessionState::Running);
        assert_eq!(
            f.vm.status(resumed.vm_ref.as_ref().unwrap()).await.unwrap(),
            VmStatus::Running
        );
    }

    #[tokio::test]
    async fn list_and_inspect_report_the_reconciled_state() {
        let f = drifted_fixture();
        drifted_session(&f, "s").await;

        let listed = list_sessions(&f.ctx).await.unwrap();
        assert_eq!(listed[0].state, SessionState::Stopped);
        assert_eq!(
            inspect(&f.ctx, "s").await.unwrap().state,
            SessionState::Stopped
        );
    }

    #[tokio::test]
    async fn reconcile_leaves_a_running_vm_alone() {
        let f = drifted_fixture();
        drifted_session(&f, "s").await;
        let vm_ref = f.ctx.repo.load("s").await.unwrap().vm_ref.unwrap();
        let opts = StartOptions {
            network: NetworkMode::Isolated,
            headless: true,
            log_path: tmp_path(),
        };
        f.vm.start(&vm_ref, &opts).await.unwrap();

        assert_eq!(
            inspect(&f.ctx, "s").await.unwrap().state,
            SessionState::Running
        );
    }

    #[tokio::test]
    async fn reconcile_leaves_a_missing_vm_alone() {
        // A missing VM is a different failure (prune/delete territory), not
        // an out-of-band stop; do not rewrite the record.
        let f = drifted_fixture();
        let mut session = sample_session("s", SessionState::Running, KeepPolicy::Discard);
        session.vm_ref = Some(crate::domain::VmRef {
            backend: "fake".to_string(),
            name: "wg-gone".to_string(),
        });
        f.ctx.repo.save(&session).await.unwrap();
        assert_eq!(
            inspect(&f.ctx, "s").await.unwrap().state,
            SessionState::Running
        );
    }

    #[tokio::test]
    async fn stop_moves_ready_through_running_to_stopped() {
        let Fixture { ctx, vm, .. } = fixture();
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
        vm.start(
            &vm_ref,
            &StartOptions {
                network: NetworkMode::Isolated,
                headless: true,
                log_path: tmp_path(),
            },
        )
        .await
        .unwrap();
        let mut session = sample_session("s", SessionState::Ready, KeepPolicy::Discard);
        session.vm_ref = Some(vm_ref);
        ctx.repo.save(&session).await.unwrap();

        let stopped = stop(&ctx, "s").await.unwrap();
        assert_eq!(stopped.state, SessionState::Stopped);
    }

    /// DESIGN.md §5: a session stop should flush the guest's filesystem
    /// (best-effort) before the VM is actually stopped, so a Tart guest with
    /// dirty pages doesn't lose writes if it's later force-killed.
    #[tokio::test]
    async fn stop_syncs_the_guest_before_stopping_when_an_ip_is_known() {
        let Fixture { ctx, vm, transport } = fixture();
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
        vm.start(
            &vm_ref,
            &StartOptions {
                network: NetworkMode::Isolated,
                headless: true,
                log_path: tmp_path(),
            },
        )
        .await
        .unwrap();
        let mut session = sample_session("s", SessionState::Ready, KeepPolicy::Discard);
        session.vm_ref = Some(vm_ref);
        let ip: std::net::IpAddr = "10.0.0.9".parse().unwrap();
        session.guest_ip = Some(ip);
        ctx.repo.save(&session).await.unwrap();

        stop(&ctx, "s").await.unwrap();

        let calls = transport.calls();
        assert!(
            calls.iter().any(
                |c| matches!(c, crate::testing::TransportCall::Exec { ip: call_ip, argv }
                    if *call_ip == ip && argv == &["sync".to_string()])
            ),
            "expected a sync exec to the guest before stop: {calls:?}"
        );
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
        let Fixture { ctx, vm, .. } = fixture();
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
    async fn delete_removes_vm_and_session() {
        let Fixture { ctx, vm, .. } = fixture();
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
        let Fixture { ctx, vm, .. } = fixture();
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
