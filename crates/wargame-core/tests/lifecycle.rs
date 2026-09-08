//! Integration tests for `wargame_core::app::lifecycle` (PLAN.md Task 5,
//! Step 1): stop/resume/delete/keep/prune against `wargame_core::testing`'s
//! fakes, exercised through the public `AppContext` API only.

use std::sync::Arc;
use wargame_core::app::BuilderRegistry;
use wargame_core::infra::HostDiskSpace;
use wargame_core::testing::MemoryBuildRepo;

use time::OffsetDateTime;

use wargame_core::app::profiles::ProfileStore;
use wargame_core::app::{
    delete, inspect, keep, list_sessions, open_gui, prune, resume, ssh_command, stop, AppContext,
    BackendRegistry, TransportRegistry,
};
use wargame_core::domain::{
    Challenge, ChallengeRef, ChallengeSummary, ExecutionMode, GuestArch, GuestConnection, GuestOs,
    KeepPolicy, NetworkMode, Resources, Session, SessionState,
};
use wargame_core::error::WargameError;
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::ProcessRunner;
use wargame_core::infra::JsonSessionRepository;
use wargame_core::ports::{ImageRef, StartOptions, VmProvider, VmSpec};
use wargame_core::testing::{FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider};

fn tmp_path() -> std::path::PathBuf {
    tempfile::tempdir().unwrap().keep()
}

fn context() -> (AppContext, Arc<FakeVmProvider>) {
    let (ctx, tart, _utm) = mixed_context();
    (ctx, tart)
}

/// An [`AppContext`] with *both* backends wired, which is what a host that
/// has run a macOS profile and a Kali one looks like (spec.md §3 FR-5's
/// "혼합 백엔드 세션").
fn mixed_context() -> (AppContext, Arc<FakeVmProvider>, Arc<FakeVmProvider>) {
    let tart = Arc::new(FakeVmProvider::new());
    let utm = Arc::new(FakeVmProvider::with_backend("utm"));
    let repo_dir = tmp_path();
    let ctx = AppContext {
        config: Default::default(),
        dirs: AppDirs { root: tmp_path() },
        wargame: Arc::new(FakeWargameProvider::new()),
        backends: BackendRegistry::new(vec![
            tart.clone() as Arc<dyn VmProvider>,
            utm.clone() as Arc<dyn VmProvider>,
        ]),
        transports: TransportRegistry::new(Arc::new(FakeTransport::new())),
        repo: Arc::new(JsonSessionRepository::new(repo_dir)),
        builds: Arc::new(MemoryBuildRepo::new()),
        builders: BuilderRegistry::default(),
        disk: Arc::new(HostDiskSpace),
        profiles: ProfileStore::new(vec![], tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap: wargame_core::app::BootstrapScripts::new("#!/bin/bash\n", "# ps\n"),
    };
    (ctx, tart, utm)
}

fn small_resources() -> Resources {
    Resources {
        cpu: 1,
        memory_mib: 1024,
        disk_gib: 10,
    }
}

fn session(id: &str, state: SessionState, keep: KeepPolicy) -> Session {
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
        resources: small_resources(),
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

#[tokio::test]
async fn delete_refuses_a_kept_session_without_force_then_succeeds_with_it() {
    let (ctx, _vm) = context();
    let s = session("keep-me", SessionState::Ready, KeepPolicy::Keep);
    ctx.repo.save(&s).await.unwrap();

    let err = delete(&ctx, "keep-me", false).await.unwrap_err();
    assert!(matches!(err, WargameError::SessionKept(ref id) if id == "keep-me"));
    assert!(ctx.repo.load("keep-me").await.is_ok());

    delete(&ctx, "keep-me", true).await.unwrap();
    assert!(matches!(
        ctx.repo.load("keep-me").await.unwrap_err(),
        WargameError::SessionNotFound(_)
    ));
}

#[tokio::test]
async fn delete_is_fine_when_the_vm_is_already_missing() {
    let (ctx, _vm) = context();
    let mut s = session("no-vm", SessionState::Failed, KeepPolicy::Discard);
    s.vm_ref = Some(wargame_core::domain::VmRef::new("tart", "wg-no-vm"));
    ctx.repo.save(&s).await.unwrap();

    // The fake VM provider never had "wg-no-vm" created, so status() reports
    // Missing; delete() must still succeed and remove the session.
    delete(&ctx, "no-vm", false).await.unwrap();
    assert!(ctx.repo.load("no-vm").await.is_err());
}

#[tokio::test]
async fn keep_toggles_the_policy_on_a_stored_session() {
    let (ctx, _vm) = context();
    let s = session("s", SessionState::Ready, KeepPolicy::Discard);
    ctx.repo.save(&s).await.unwrap();

    let kept = keep(&ctx, "s", true).await.unwrap();
    assert_eq!(kept.keep, KeepPolicy::Keep);
    assert_eq!(
        keep(&ctx, "s", false).await.unwrap().keep,
        KeepPolicy::Discard
    );
}

#[tokio::test]
async fn stop_then_resume_round_trips_through_the_fake_vm() {
    let (ctx, vm) = context();
    let vm_ref = vm
        .create(&VmSpec::new(
            "wg-s".to_string(),
            ImageRef("img".to_string()),
            small_resources(),
        ))
        .await
        .unwrap();
    let mut s = session("s", SessionState::Ready, KeepPolicy::Discard);
    s.vm_ref = Some(vm_ref);
    ctx.repo.save(&s).await.unwrap();

    let stopped = stop(&ctx, "s").await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);

    let resumed = resume(&ctx, "s").await.unwrap();
    assert_eq!(resumed.state, SessionState::Running);
    assert!(resumed.guest_ip.is_some());
}

#[tokio::test]
async fn prune_finds_an_orphan_vm_but_leaves_a_tracked_one_alone() {
    let (ctx, vm) = context();
    vm.create(&VmSpec::new(
        "wg-orphan".to_string(),
        ImageRef("img".to_string()),
        small_resources(),
    ))
    .await
    .unwrap();
    let tracked_ref = vm
        .create(&VmSpec::new(
            "wg-tracked".to_string(),
            ImageRef("img".to_string()),
            small_resources(),
        ))
        .await
        .unwrap();
    let mut tracked = session("tracked", SessionState::Ready, KeepPolicy::Discard);
    tracked.vm_ref = Some(tracked_ref);
    ctx.repo.save(&tracked).await.unwrap();

    let dry_run = prune(&ctx, true).await.unwrap();
    assert_eq!(dry_run, vec!["tart/wg-orphan".to_string()]);
    assert_eq!(vm.vm_names().len(), 2, "dry run must not delete");

    let orphans = prune(&ctx, false).await.unwrap();
    assert_eq!(orphans, vec!["tart/wg-orphan".to_string()]);
    assert_eq!(vm.vm_names(), vec!["wg-tracked".to_string()]);
}

// --- mixed backends and pre-multi-OS records (spec.md §7 AC-08) -----------

/// A session record as the pre-multi-OS build wrote it: no
/// `guest_distribution`, `vm_mode`, `connection` or `workspace_root`, and a
/// `vm_ref` naming a VM but carrying no stable id.
const LEGACY_SESSION_JSON: &str = r#"{
  "id": "dh-legacy-000001",
  "display_name": "legacy",
  "challenge": {
    "ref": {"provider": "dreamhack", "id": "1"},
    "slug": "legacy", "title": "legacy", "categories": [], "difficulty": null
  },
  "challenge_snapshot": {
    "summary": {
      "ref": {"provider": "dreamhack", "id": "1"},
      "slug": "legacy", "title": "legacy", "categories": [], "difficulty": null
    },
    "description": "", "tags": [], "assets": [], "remote_endpoints": [],
    "source_url": "https://example.invalid"
  },
  "vm_provider": "tart",
  "profile": "macos-re-arm64",
  "guest_os": "macos",
  "guest_arch": "arm64",
  "network": "isolated",
  "resources": {"cpu": 2, "memory_mib": 4096, "disk_gib": 20},
  "created_at": "2026-01-01T00:00:00Z",
  "state": "ready",
  "vm_ref": {"backend": "tart", "name": "wg-dh-legacy-000001"},
  "assets": [],
  "keep": "discard",
  "last_error": null,
  "runner_pid": null,
  "guest_ip": null
}"#;

/// Creates a VM on `vm` and stores a session pointing at it.
async fn session_on(
    ctx: &AppContext,
    vm: &Arc<FakeVmProvider>,
    id: &str,
    state: SessionState,
) -> Session {
    let vm_ref = vm
        .create(&VmSpec::new(
            format!("wg-{id}"),
            ImageRef("img".to_string()),
            small_resources(),
        ))
        .await
        .unwrap();
    if state.is_up() {
        // `reconcile` trusts the backend over the record, so a session that
        // says it is up needs a VM that is actually running.
        vm.start(
            &vm_ref,
            &StartOptions {
                network: NetworkMode::Isolated,
                headless: true,
                log_path: tmp_path().join("run.log"),
            },
        )
        .await
        .unwrap();
    }
    let mut s = session(id, state, KeepPolicy::Discard);
    s.vm_provider = vm_ref.backend.clone();
    if vm_ref.backend == "utm" {
        s.guest_os = GuestOs::Linux;
        s.guest_distribution = Some("kali".to_string());
        s.vm_mode = ExecutionMode::Emulate;
        s.guest_arch = GuestArch::X86_64;
    }
    s.vm_ref = Some(vm_ref);
    ctx.repo.save(&s).await.unwrap();
    s
}

/// Every lifecycle verb routes on the *session's* backend, so a Tart and a
/// UTM session on the same host never touch each other's VMs.
#[tokio::test]
async fn stop_resume_and_delete_act_only_on_the_sessions_own_backend() {
    let (ctx, tart, utm) = mixed_context();
    session_on(&ctx, &tart, "mac-1", SessionState::Ready).await;
    session_on(&ctx, &utm, "kali-1", SessionState::Ready).await;

    let listed = list_sessions(&ctx).await.unwrap();
    assert_eq!(listed.len(), 2);
    let mut backends: Vec<&str> = listed.iter().map(|s| s.backend_name()).collect();
    backends.sort_unstable();
    assert_eq!(backends, vec!["tart", "utm"]);

    // Stopping the UTM session leaves the Tart one running.
    assert_eq!(
        stop(&ctx, "kali-1").await.unwrap().state,
        SessionState::Stopped
    );
    assert_eq!(
        inspect(&ctx, "mac-1").await.unwrap().state,
        SessionState::Ready
    );
    assert_eq!(
        resume(&ctx, "kali-1").await.unwrap().state,
        SessionState::Running
    );

    // ... and deleting it removes exactly one VM, from one backend.
    delete(&ctx, "kali-1", false).await.unwrap();
    assert!(utm.vm_names().is_empty(), "{:?}", utm.vm_names());
    assert_eq!(tart.vm_names(), vec!["wg-mac-1".to_string()]);
    assert_eq!(list_sessions(&ctx).await.unwrap().len(), 1);
}

/// `prune` asks each wired backend for its own VMs and only deletes the ones
/// no session claims -- on that backend (spec.md §3 FR-5: "다른 백엔드의 같은
/// VM 이름을 혼동하지 않는다").
#[tokio::test]
async fn prune_keeps_tracked_vms_on_both_backends_and_names_the_backend() {
    let (ctx, tart, utm) = mixed_context();
    session_on(&ctx, &tart, "mac-1", SessionState::Ready).await;
    session_on(&ctx, &utm, "kali-1", SessionState::Ready).await;
    // The same VM *name* on both backends: only the untracked one goes.
    for provider in [&tart, &utm] {
        provider
            .create(&VmSpec::new(
                "wg-orphan".to_string(),
                ImageRef("img".to_string()),
                small_resources(),
            ))
            .await
            .unwrap();
    }

    let mut orphans = prune(&ctx, false).await.unwrap();
    orphans.sort();
    assert_eq!(
        orphans,
        vec!["tart/wg-orphan".to_string(), "utm/wg-orphan".to_string()]
    );
    assert_eq!(tart.vm_names(), vec!["wg-mac-1".to_string()]);
    assert_eq!(utm.vm_names(), vec!["wg-kali-1".to_string()]);
}

/// spec.md §3 FR-5: "과거 세션의 누락값은 기존 Tart/arm64/virtualize/admin
/// 동작으로 복원한다" -- and every verb still works on such a record.
#[tokio::test]
async fn a_pre_multi_os_session_record_still_runs_the_whole_lifecycle() {
    let (ctx, tart, _utm) = mixed_context();
    tart.create(&VmSpec::new(
        "wg-dh-legacy-000001".to_string(),
        ImageRef("img".to_string()),
        small_resources(),
    ))
    .await
    .unwrap();
    let legacy: Session = serde_json::from_str(LEGACY_SESSION_JSON).unwrap();
    tart.start(
        legacy.vm_ref.as_ref().unwrap(),
        &StartOptions {
            network: NetworkMode::Isolated,
            headless: true,
            log_path: tmp_path().join("run.log"),
        },
    )
    .await
    .unwrap();
    ctx.repo.save(&legacy).await.unwrap();

    let loaded = inspect(&ctx, "dh-legacy-000001").await.unwrap();
    assert_eq!(loaded.backend_name(), "tart");
    assert_eq!(loaded.vm_mode, ExecutionMode::Virtualize);
    assert_eq!(loaded.connection, GuestConnection::default());
    assert_eq!(loaded.connection.user, "admin");
    assert_eq!(
        loaded.vm_ref.as_ref().unwrap().id,
        "wg-dh-legacy-000001",
        "a record with no VM id addresses the VM by name, as Tart does"
    );
    assert!(loaded.workspace_root.is_empty());

    assert_eq!(list_sessions(&ctx).await.unwrap().len(), 1);
    assert_eq!(
        stop(&ctx, "dh-legacy-000001").await.unwrap().state,
        SessionState::Stopped
    );
    let resumed = resume(&ctx, "dh-legacy-000001").await.unwrap();
    assert_eq!(resumed.state, SessionState::Running);
    assert!(resumed.guest_ip.is_some());

    // `gui` and `ssh` both go through the session's saved connection.
    open_gui(&ctx, "dh-legacy-000001").await.unwrap();
    let cmd = ssh_command(&ctx, "dh-legacy-000001").await.unwrap();
    let rendered = format!("{cmd:?}");
    assert!(rendered.contains("admin@"), "{rendered}");

    delete(&ctx, "dh-legacy-000001", false).await.unwrap();
    assert!(tart.vm_names().is_empty());
    assert!(list_sessions(&ctx).await.unwrap().is_empty());
}

/// `gui` and `ssh` on a UTM session go to the UTM backend, addressing it by
/// the stable id rather than the display name.
#[tokio::test]
async fn gui_and_ssh_use_the_utm_sessions_own_backend_and_connection() {
    let (ctx, _tart, utm) = mixed_context();
    let mut s = session_on(&ctx, &utm, "kali-1", SessionState::Ready).await;
    s.connection = GuestConnection::new("kali", GuestOs::Linux);
    ctx.repo.save(&s).await.unwrap();

    open_gui(&ctx, "kali-1").await.unwrap();
    assert!(
        utm.calls().iter().any(|c| c.starts_with("start")),
        "{:?}",
        utm.calls()
    );
    let rendered = format!("{:?}", ssh_command(&ctx, "kali-1").await.unwrap());
    assert!(rendered.contains("kali@"), "{rendered}");
}
