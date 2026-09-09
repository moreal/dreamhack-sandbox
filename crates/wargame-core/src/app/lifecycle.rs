//! Session lifecycle operations (DESIGN.md §3.1's `app/lifecycle.rs`;
//! MVP.md §17-21, §37): list/inspect/stop/resume/delete/keep/prune. Built
//! entirely on [`crate::ports::VmProvider`] and
//! [`crate::ports::SessionRepository`], so it runs the same way against a
//! real Tart backend or `crate::testing`'s fakes.

use std::collections::{HashMap, HashSet};

use crate::domain::{KeepPolicy, Session, SessionState};
use crate::error::{Result, WargameError};
use crate::ports::{StartOptions, VmStatus};

use super::backends::{provider_for_session, transport_for_session};
use super::AppContext;

/// How long a session's own VM may take to boot, from the backend and mode
/// it was created with -- not from the profile, which may have changed
/// (spec.md §3 FR-5).
fn boot_timeout(ctx: &AppContext, session: &Session) -> Result<std::time::Duration> {
    Ok(ctx
        .config
        .vm
        .boot_timeout(session.backend()?, session.vm_mode))
}

/// One row of [`list_sessions`]: the session as it should be shown, plus
/// whatever stopped its backend from being asked.
///
/// The note exists so a backend problem is never silently swallowed *and*
/// never fails the whole list: an uninstalled UTM or a denied Automation
/// grant would otherwise hide every Tart session too (spec.md §6 row 1's
/// "UTM 미설치·자동화 권한 거부 -> 원인·복구 절차, Tart 작업은 계속 가능").
/// It serializes flattened, so `sessions --json` keeps the session's own
/// shape and merely gains the key.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionListing {
    #[serde(flatten)]
    pub session: Session,
    /// Why this row may be out of date: the backend could not be asked for
    /// the VM's real status. `None` when it answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconcile_error: Option<String>,
}

pub async fn list_sessions(ctx: &AppContext) -> Result<Vec<SessionListing>> {
    let mut sessions = Vec::new();
    for session in ctx.repo.list().await? {
        let (session, reconcile_error) = reconcile_reporting(ctx, session).await?;
        sessions.push(SessionListing {
            session,
            reconcile_error,
        });
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
pub async fn reconcile(ctx: &AppContext, session: Session) -> Result<Session> {
    Ok(reconcile_reporting(ctx, session).await?.0)
}

/// [`reconcile`], also saying why the backend could not be asked.
///
/// A backend that cannot answer -- UTM is not installed, the Automation
/// grant was denied -- is not a reason to fail the caller: the record is
/// returned exactly as it stands, together with the error, so a list can
/// show the problem on that row and keep every other session visible. The
/// error is never dropped: callers that *act* on the session (`stop`,
/// `resume`, `delete`) reach the backend again straight afterwards and
/// surface its real failure there.
async fn reconcile_reporting(
    ctx: &AppContext,
    mut session: Session,
) -> Result<(Session, Option<String>)> {
    if !session.state.is_up() {
        return Ok((session, None));
    }
    let Some(vm_ref) = &session.vm_ref else {
        return Ok((session, None));
    };
    // A backend this build has no adapter for is treated like one that
    // cannot answer: the record is left exactly as it is rather than
    // failing every `wargame sessions` that happens to include it.
    let vm = match provider_for_session(ctx, &session) {
        Ok(vm) => vm,
        Err(e) => {
            tracing::debug!(
                session = %session.id,
                backend = %session.backend_name(),
                error = %e,
                "no adapter for this session's backend; leaving the record untouched"
            );
            return Ok((session, Some(e.to_string())));
        }
    };
    let vm_ref = vm_ref.clone();
    let status = match vm.status(&vm_ref).await {
        Ok(status) => status,
        Err(e) => {
            // The backend itself is unreachable. Failing here would take
            // every other session down with it, so the record is left as it
            // is and the reason travels with the row instead.
            tracing::warn!(
                session = %session.id,
                backend = %session.backend_name(),
                error = %e,
                "backend could not report this VM's status; leaving the record untouched"
            );
            return Ok((session, Some(e.to_string())));
        }
    };
    if status != VmStatus::Stopped {
        return Ok((session, None));
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
    Ok((session, None))
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
    // Best-effort: flush the guest's filesystem before asking the backend to
    // stop it, so a stop that outruns its own graceful-shutdown window
    // doesn't lose writes that were still dirty in page cache
    // (DESIGN.md §5). A failure here must never block the stop itself.
    match transport_for_session(ctx, &session) {
        Ok((endpoint, transport)) => {
            let flush = crate::infra::GuestOps::for_endpoint(&endpoint).flush();
            if let Err(err) = flush.run(transport.as_ref(), &endpoint).await {
                tracing::debug!(session = %id, error = %err, "sync before stop failed; stopping anyway");
            }
        }
        Err(err) => {
            tracing::debug!(session = %id, error = %err, "no guest transport before stop; stopping anyway");
        }
    }
    if let Some(vm_ref) = &session.vm_ref {
        provider_for_session(ctx, &session)?.stop(vm_ref).await?;
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

    // Everything here comes from the session's own record: its backend, its
    // execution mode's boot timeout, its network, its guest user and port.
    let vm = provider_for_session(ctx, &session)?;
    let boot_timeout = boot_timeout(ctx, &session)?;
    let start_opts = StartOptions {
        network: session.network,
        headless: true,
        log_path: ctx.repo.session_dir(id).join("run.log"),
    };
    let runtime = vm.start(&vm_ref, &start_opts).await?;
    session.runner_pid = runtime.pid;

    let ip = vm.wait_ip(&vm_ref, boot_timeout).await?;
    session.guest_ip = Some(ip);
    let (endpoint, transport) = transport_for_session(ctx, &session)?;
    transport.wait_ready(&endpoint, boot_timeout).await?;

    session.transition(SessionState::Running)?;
    ctx.repo.save(&session).await?;
    Ok(session)
}

/// Deletes a session's VM (if any) and its metadata. A `Keep`-policy
/// session refuses unless `force` is set (NFR-07).
///
/// This is the last resort, so it works from whatever state a crash left
/// behind. A record that is already `Deleting` is an *interrupted* delete:
/// it is continued rather than re-entered, which would be an illegal
/// same-state transition and would leave the user with no way to finish the
/// cleanup that `prune` points them at.
pub async fn delete(ctx: &AppContext, id: &str, force: bool) -> Result<()> {
    let mut session = ctx.repo.load(id).await?;
    if session.keep == KeepPolicy::Keep && !force {
        return Err(WargameError::SessionKept(session.id.clone()));
    }
    if !matches!(
        session.state,
        SessionState::Deleting | SessionState::Deleted
    ) {
        session.transition(SessionState::Deleting)?;
        ctx.repo.save(&session).await?;
    }

    if let Some(vm_ref) = &session.vm_ref {
        let vm = provider_for_session(ctx, &session)?;
        // `status()` failing is treated as "not missing" so we still try the
        // delete rather than silently leaving a VM behind.
        let status = vm.status(vm_ref).await.unwrap_or(VmStatus::Stopped);
        if status != VmStatus::Missing {
            vm.delete(vm_ref).await?;
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

/// One leftover VM `prune` found running. Never deleted: `prune` does not
/// stop guests, so the user is told which session to deal with instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningLeftover {
    /// `<backend>/<name>`.
    pub vm: String,
    /// The id of the session record that owns it.
    pub session: String,
}

/// One VM inside the configured name prefix that no record in this home
/// owns. `prune` will never touch it, so the caller has what it needs to
/// name the manual remedy on the right backend instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmanagedVm {
    /// `<backend>/<name>`.
    pub vm: String,
    /// The backend it lives on (`"tart"`, `"utm"`).
    pub backend: String,
    /// Its display name on that backend.
    pub name: String,
}

/// One backend `prune` could not sweep at all.
///
/// A host that never installed UTM answers every `utm` call with
/// [`crate::error::WargameError::BackendUnavailable`], and a denied
/// Automation grant behaves the same way. That must not fail the whole
/// sweep -- the Tart VMs are still there to clean up -- and it must not be
/// hidden either, or the user is told "no orphaned VMs" about a backend
/// nobody looked at (spec.md §6 row 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBackend {
    /// The backend that could not be asked (`"utm"`, `"tart"`).
    pub backend: String,
    /// Its own error message, verbatim.
    pub error: String,
    /// The error's next step, when it has one.
    pub hint: Option<String>,
}

/// What one [`prune`] pass found. Every VM is named `<backend>/<name>`,
/// because a name alone is not unique across backends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Leftover VMs this home owns and `prune` deleted -- or, on a dry run,
    /// would delete.
    pub orphans: Vec<String>,
    /// Leftovers this home owns that are still running, and so were left
    /// alone.
    pub running: Vec<RunningLeftover>,
    /// VMs that only *look* like ours: they carry the configured name
    /// prefix, but no record in this `WARGAME_HOME` owns them -- another
    /// home's VMs, or the user's own. Reported so they are not a mystery,
    /// never touched (spec.md §3 FR-5).
    pub unmanaged: Vec<UnmanagedVm>,
    /// Backends that could not be swept, with the reason. Their VMs were
    /// neither examined nor touched.
    pub skipped: Vec<SkippedBackend>,
}

impl PruneReport {
    /// The `<backend>/<name>` of every VM that carries this app's prefix but
    /// belongs to no record here.
    pub fn unmanaged_names(&self) -> Vec<&str> {
        self.unmanaged.iter().map(|u| u.vm.as_str()).collect()
    }

    /// Whether there is nothing at all to report. A skipped backend counts:
    /// "no orphaned VMs found" would claim a sweep that never ran.
    pub fn is_empty(&self) -> bool {
        self.orphans.is_empty()
            && self.running.is_empty()
            && self.unmanaged.is_empty()
            && self.skipped.is_empty()
    }
}

/// Whether a session record still justifies its VM's existence.
///
/// `Deleting`/`Deleted` mean a delete did not finish; `Failed` means the
/// session will never run again. Every other state -- including a merely
/// `Stopped` session the user will `resume` -- is live, and its VM is not a
/// leftover.
fn is_leftover(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Failed | SessionState::Deleting | SessionState::Deleted
    )
}

/// Cleans up the VMs *this* `WARGAME_HOME` left behind (MVP.md §37 /
/// NFR-02).
///
/// Ownership is decided by this home's records, never by a name prefix
/// (spec.md §3 FR-5: "앱 기록의 UUID와 소유권을 대조한다. 이름 prefix만으로
/// 외부 VM을 삭제하지 않는다"). A VM is a deletion candidate only when a
/// session record here owns it -- by the stable `(backend, id)` it wrote
/// down, or by the `<prefix><session id>` name a launch that died before
/// saving that ref would have used -- *and* that record no longer justifies
/// it ([`is_leftover`]). Everything else is left alone: VMs a build record
/// owns, VMs of live or kept sessions, and prefix-matching VMs no record
/// here knows about, which are reported as unmanaged instead of silently
/// hidden.
///
/// A running VM is never deleted, even when owned.
pub async fn prune(ctx: &AppContext, dry_run: bool) -> Result<PruneReport> {
    let sessions = ctx.repo.list().await?;
    // Two record-derived keys point at the same sessions: the stable
    // `(backend, id)` a launch wrote down, and the `(backend, name)` it
    // would have written down. An identically named VM on another backend is
    // a different VM, so the backend is part of both keys (FR-5).
    let mut by_id: HashMap<(&str, &str), &Session> = HashMap::new();
    let mut by_name: HashMap<(String, String), &Session> = HashMap::new();
    for session in &sessions {
        if let Some(vm) = &session.vm_ref {
            by_id.insert((vm.backend.as_str(), vm.id.as_str()), session);
        }
        if let Ok(backend) = session.backend() {
            by_name.insert(
                (
                    backend.as_str().to_string(),
                    format!("{}{}", ctx.config.vm.vm_prefix(backend), session.id),
                ),
                session,
            );
        }
    }
    // Build records own VMs that no session ever will: the installation VM
    // a profile is created with and the image VMs it publishes. They carry
    // the build prefix rather than the session prefix, but ownership is
    // decided by the record, not by a name (spec.md §3 FR-5).
    let mut build_owned: HashSet<(String, String)> = HashSet::new();
    for record in ctx.builds.list().await? {
        build_owned.extend(record.owned_vm_ids());
    }

    let mut report = PruneReport::default();
    for vm in ctx.backends.all() {
        // One backend being unreachable is not a reason to abandon the
        // others: it is recorded and the sweep goes on (spec.md §6 row 1,
        // "Tart 작업은 계속 가능").
        let mut skip = |error: WargameError| {
            tracing::warn!(
                backend = %vm.name(),
                error = %error,
                "backend could not be swept; leaving its VMs alone"
            );
            report.skipped.push(SkippedBackend {
                backend: vm.name().to_string(),
                hint: error.hint(),
                error: error.to_string(),
            });
        };
        let listed = match vm.list().await {
            Ok(listed) => listed,
            Err(e) => {
                skip(e);
                continue;
            }
        };
        for vm_ref in listed {
            let label = format!("{}/{}", vm_ref.backend, vm_ref.name);
            if build_owned.contains(&(vm_ref.backend.clone(), vm_ref.id.clone())) {
                continue;
            }
            let owner = by_id
                .get(&(vm_ref.backend.as_str(), vm_ref.id.as_str()))
                .or_else(|| by_name.get(&(vm_ref.backend.clone(), vm_ref.name.clone())))
                .copied();
            let Some(session) = owner else {
                report.unmanaged.push(UnmanagedVm {
                    vm: label,
                    backend: vm_ref.backend.clone(),
                    name: vm_ref.name.clone(),
                });
                continue;
            };
            if session.keep == KeepPolicy::Keep || !is_leftover(session.state) {
                continue;
            }
            // A backend that stops answering mid-sweep is the same problem
            // as one that never answered: report it and move to the next
            // backend rather than failing the whole prune.
            let status = match vm.status(&vm_ref).await {
                Ok(status) => status,
                Err(e) => {
                    skip(e);
                    break;
                }
            };
            match status {
                VmStatus::Running => report.running.push(RunningLeftover {
                    vm: label,
                    session: session.id.clone(),
                }),
                // Already gone between `list` and now: nothing to do, and
                // nothing worth telling the user about.
                VmStatus::Missing => {}
                VmStatus::Stopped => {
                    if !dry_run {
                        if let Err(e) = vm.delete(&vm_ref).await {
                            skip(e);
                            break;
                        }
                        settle_record(ctx, session).await?;
                    }
                    report.orphans.push(label);
                }
            }
        }
    }
    Ok(report)
}

/// Brings the owning record in line with the VM `prune` has just deleted.
///
/// A record that was already `Deleting` is an interrupted delete, and
/// removing the VM was the part that was left: the record goes too, rather
/// than haunting `wargame sessions` as a session with nothing behind it. A
/// failed session's record is kept -- the failure is still worth reading --
/// but it must stop pointing at a VM that no longer exists.
async fn settle_record(ctx: &AppContext, session: &Session) -> Result<()> {
    match session.state {
        SessionState::Deleting | SessionState::Deleted => ctx.repo.delete(&session.id).await,
        _ => {
            let mut session = session.clone();
            session.vm_ref = None;
            session.runner_pid = None;
            session.guest_ip = None;
            ctx.repo.save(&session).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::app::BuilderRegistry;
    use crate::app::{BackendRegistry, TransportRegistry};
    use crate::domain::{
        Challenge, ChallengeRef, ChallengeSummary, ExecutionMode, GuestArch, GuestConnection,
        GuestOs, NetworkMode, Resources,
    };
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
    use crate::infra::HostDiskSpace;
    use crate::ports::{ImageRef, VmProvider, VmSpec};
    use crate::testing::MemoryBuildRepo;
    use crate::testing::{
        FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
    };
    use std::sync::Arc;
    use std::time::Duration;
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
            backends: BackendRegistry::new(vec![vm.clone() as Arc<dyn VmProvider>]),
            transports: TransportRegistry::new(transport.clone()),
            repo: Arc::new(MemoryRepo::new()),
            builds: Arc::new(MemoryBuildRepo::new()),
            builders: BuilderRegistry::default(),
            disk: Arc::new(HostDiskSpace),
            profiles: ProfileStore::new(vec![], tmp_path()),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap: crate::app::BootstrapScripts::new("#!/bin/bash\n", "# ps\n"),
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
            guest_distribution: None,
            vm_mode: ExecutionMode::Virtualize,
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
            connection: GuestConnection::default(),
            workspace_root: "/Users/admin/wargame".to_string(),
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
            f.vm.create(&VmSpec::new(
                format!("wg-{id}"),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
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
        assert_eq!(listed[0].session.state, SessionState::Stopped);
        assert_eq!(listed[0].reconcile_error, None);
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
        session.vm_ref = Some(crate::domain::VmRef::new("tart", "wg-gone"));
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
            .create(&VmSpec::new(
                "wg-s".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
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
            .create(&VmSpec::new(
                "wg-s".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
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
                |c| matches!(c, crate::testing::TransportCall::Exec { endpoint, argv }
                    if endpoint.host == ip && argv == &["sync".to_string()])
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
            .create(&VmSpec::new(
                "wg-s".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
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
            .create(&VmSpec::new(
                "wg-s".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
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
    async fn prune_deletes_the_leftover_it_owns_and_ignores_the_rest() {
        let Fixture { ctx, vm, .. } = fixture();
        let resources = Resources {
            cpu: 1,
            memory_mib: 1024,
            disk_gib: 10,
        };
        // A prefix-matching VM no record here owns: somebody else's.
        vm.create(&VmSpec::new(
            "wg-not-ours".to_string(),
            ImageRef("img".to_string()),
            resources,
        ))
        .await
        .unwrap();
        // A VM a live session owns.
        let tracked_ref = vm
            .create(&VmSpec::new(
                "wg-tracked".to_string(),
                ImageRef("img".to_string()),
                resources,
            ))
            .await
            .unwrap();
        let mut tracked = sample_session("tracked", SessionState::Ready, KeepPolicy::Discard);
        tracked.vm_ref = Some(tracked_ref);
        ctx.repo.save(&tracked).await.unwrap();
        // A VM whose session record says its delete never finished.
        let leftover_ref = vm
            .create(&VmSpec::new(
                "wg-leftover".to_string(),
                ImageRef("img".to_string()),
                resources,
            ))
            .await
            .unwrap();
        let mut leftover = sample_session("leftover", SessionState::Deleting, KeepPolicy::Discard);
        leftover.vm_ref = Some(leftover_ref);
        ctx.repo.save(&leftover).await.unwrap();

        // VMs are reported `<backend>/<name>`: a name alone is not unique
        // across backends.
        let dry = prune(&ctx, true).await.unwrap();
        assert_eq!(dry.orphans, vec!["tart/wg-leftover".to_string()]);
        assert_eq!(dry.unmanaged_names(), vec!["tart/wg-not-ours"]);
        assert_eq!(vm.vm_names().len(), 3, "dry run must not delete anything");

        let report = prune(&ctx, false).await.unwrap();
        assert_eq!(report.orphans, vec!["tart/wg-leftover".to_string()]);
        assert_eq!(
            vm.vm_names(),
            vec!["wg-not-ours".to_string(), "wg-tracked".to_string()]
        );
    }

    /// Ownership is `(backend, id)`, not the display name: with the same VM
    /// name registered on two backends, only the one a record here owns can
    /// be deleted (spec.md §3 FR-5's "다른 백엔드의 같은 VM 이름을 혼동하지
    /// 않는다").
    #[tokio::test]
    async fn prune_keys_ownership_on_backend_and_id_not_on_the_name() {
        let Fixture { mut ctx, vm, .. } = fixture();
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        ctx.backends = BackendRegistry::new(vec![
            vm.clone() as Arc<dyn VmProvider>,
            utm.clone() as Arc<dyn VmProvider>,
        ]);
        let spec = || {
            VmSpec::new(
                "wg-same-name".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            )
        };
        vm.create(&spec()).await.unwrap();
        let utm_ref = utm.create(&spec()).await.unwrap();

        // Only the UTM VM is claimed by a record, and that record's delete
        // never finished.
        let mut session = sample_session("leftover", SessionState::Deleting, KeepPolicy::Discard);
        session.vm_provider = "utm".to_string();
        session.vm_ref = Some(utm_ref);
        ctx.repo.save(&session).await.unwrap();

        let report = prune(&ctx, false).await.unwrap();
        assert_eq!(report.orphans, vec!["utm/wg-same-name".to_string()]);
        assert_eq!(report.unmanaged_names(), vec!["tart/wg-same-name"]);
        assert_eq!(
            vm.vm_names(),
            vec!["wg-same-name".to_string()],
            "the Tart VM no record owns must survive"
        );
        assert!(utm.vm_names().is_empty());
    }

    /// A macOS-only host has no UTM, so every `utm` call fails before it
    /// reaches the app. `prune` must still sweep Tart and report the
    /// backend it could not ask, rather than exiting non-zero for every
    /// user who never installed UTM (spec.md §6 row 1: "UTM 미설치·자동화
    /// 권한 거부 -> 원인·복구 절차, Tart 작업은 계속 가능").
    #[tokio::test]
    async fn prune_skips_a_backend_it_cannot_list_and_still_sweeps_the_others() {
        let Fixture { mut ctx, vm, .. } = fixture();
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        utm.set_fail_on("list");
        utm.set_fail_on("status");
        ctx.backends = BackendRegistry::new(vec![
            vm.clone() as Arc<dyn VmProvider>,
            utm.clone() as Arc<dyn VmProvider>,
        ]);
        let leftover_ref = vm
            .create(&VmSpec::new(
                "wg-leftover".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
            .await
            .unwrap();
        let mut leftover = sample_session("leftover", SessionState::Deleting, KeepPolicy::Discard);
        leftover.vm_ref = Some(leftover_ref);
        ctx.repo.save(&leftover).await.unwrap();

        let report = prune(&ctx, false).await.unwrap();

        assert_eq!(report.orphans, vec!["tart/wg-leftover".to_string()]);
        assert!(vm.vm_names().is_empty(), "the Tart leftover must be gone");
        let skipped = &report.skipped;
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].backend, "utm");
        assert!(
            skipped[0].error.contains("utm"),
            "the skip must carry the backend's own error: {skipped:?}"
        );
        assert_eq!(skipped[0].hint.as_deref(), Some("wargame doctor"));
        assert!(!report.is_empty(), "a skipped backend is worth reporting");
    }

    /// A `prune` that found nothing but could not ask one backend is not an
    /// empty report: saying "no orphaned VMs found" would claim a sweep
    /// that never happened.
    #[tokio::test]
    async fn a_prune_report_with_only_a_skip_is_not_empty() {
        let Fixture { mut ctx, .. } = fixture();
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        utm.set_fail_on("list");
        ctx.backends = BackendRegistry::new(vec![utm as Arc<dyn VmProvider>]);

        let report = prune(&ctx, true).await.unwrap();
        assert!(report.orphans.is_empty());
        assert!(!report.is_empty());
        assert_eq!(report.skipped.len(), 1);
    }

    /// One unreachable backend must not make `wargame sessions` fail
    /// wholesale: the Tart sessions still list, the UTM row keeps its
    /// recorded state, and the backend problem is attached to that row
    /// instead of being swallowed (spec.md §6 row 1).
    #[tokio::test]
    async fn list_sessions_survives_a_backend_that_cannot_answer_and_says_so() {
        let Fixture { mut ctx, vm, .. } = fixture();
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        utm.set_fail_on("status");
        ctx.backends = BackendRegistry::new(vec![
            vm.clone() as Arc<dyn VmProvider>,
            utm.clone() as Arc<dyn VmProvider>,
        ]);
        let mut on_utm = sample_session("utm-one", SessionState::Ready, KeepPolicy::Discard);
        on_utm.vm_provider = "utm".to_string();
        on_utm.vm_ref = Some(crate::domain::VmRef::new("utm", "wg-utm-one"));
        ctx.repo.save(&on_utm).await.unwrap();
        let mut on_tart = sample_session("tart-one", SessionState::Ready, KeepPolicy::Discard);
        on_tart.vm_ref = Some(crate::domain::VmRef::new("tart", "wg-tart-one"));
        ctx.repo.save(&on_tart).await.unwrap();

        let listed = list_sessions(&ctx).await.unwrap();

        assert_eq!(listed.len(), 2, "both sessions must still be visible");
        let utm_row = listed.iter().find(|l| l.session.id == "utm-one").unwrap();
        assert_eq!(
            utm_row.session.state,
            SessionState::Ready,
            "an unanswerable backend leaves the record untouched"
        );
        assert!(
            utm_row
                .reconcile_error
                .as_deref()
                .is_some_and(|e| e.contains("utm")),
            "the backend problem must reach the row: {utm_row:?}"
        );
        let tart_row = listed.iter().find(|l| l.session.id == "tart-one").unwrap();
        assert_eq!(tart_row.reconcile_error, None);
    }

    /// `inspect` follows the same rule -- it must not fail wholesale --
    /// while an *explicit* `stop`/`resume`/`delete` on that session still
    /// returns the backend's real error rather than pretending to work.
    #[tokio::test]
    async fn explicit_operations_still_fail_on_an_unreachable_backend() {
        let Fixture { mut ctx, .. } = fixture();
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        for step in ["status", "stop", "start", "delete"] {
            utm.set_fail_on(step);
        }
        ctx.backends = BackendRegistry::new(vec![utm as Arc<dyn VmProvider>]);
        let mut session = sample_session("utm-one", SessionState::Ready, KeepPolicy::Discard);
        session.vm_provider = "utm".to_string();
        session.vm_ref = Some(crate::domain::VmRef::new("utm", "wg-utm-one"));
        ctx.repo.save(&session).await.unwrap();

        assert_eq!(
            inspect(&ctx, "utm-one").await.unwrap().state,
            SessionState::Ready,
            "inspect must not fail on a backend that cannot answer"
        );
        let stop_err = stop(&ctx, "utm-one").await.unwrap_err().to_string();
        assert!(stop_err.contains("fake stop failure"), "{stop_err}");
        let delete_err = delete(&ctx, "utm-one", false)
            .await
            .unwrap_err()
            .to_string();
        assert!(delete_err.contains("fake delete failure"), "{delete_err}");
        let mut stopped = ctx.repo.load("utm-one").await.unwrap();
        stopped.state = SessionState::Stopped;
        ctx.repo.save(&stopped).await.unwrap();
        let resume_err = resume(&ctx, "utm-one").await.unwrap_err().to_string();
        assert!(resume_err.contains("fake start failure"), "{resume_err}");
    }

    /// A session's backend comes from the VM ref it actually created on,
    /// even when the (older, coarser) `vm_provider` field disagrees.
    #[tokio::test]
    async fn session_operations_route_on_the_vm_refs_backend() {
        let Fixture { mut ctx, vm, .. } = fixture();
        let utm = Arc::new(FakeVmProvider::with_backend("utm"));
        ctx.backends = BackendRegistry::new(vec![
            vm.clone() as Arc<dyn VmProvider>,
            utm.clone() as Arc<dyn VmProvider>,
        ]);
        let utm_ref = utm
            .create(&VmSpec::new(
                "wg-u".to_string(),
                ImageRef("img".to_string()),
                Resources {
                    cpu: 1,
                    memory_mib: 1024,
                    disk_gib: 10,
                },
            ))
            .await
            .unwrap();
        let mut session = sample_session("s", SessionState::Ready, KeepPolicy::Discard);
        session.vm_provider = "tart".to_string();
        session.vm_ref = Some(utm_ref.clone());
        assert_eq!(session.backend_name(), "utm");
        ctx.repo.save(&session).await.unwrap();

        delete(&ctx, "s", false).await.unwrap();
        assert!(utm.vm_names().is_empty(), "the UTM VM should be deleted");
        assert!(vm.calls().iter().all(|c| c != "delete"));
    }
}
