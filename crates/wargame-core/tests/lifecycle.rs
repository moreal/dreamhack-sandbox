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
    delete, keep, prune, resume, stop, AppContext, BackendRegistry, TransportRegistry,
};
use wargame_core::domain::{
    Challenge, ChallengeRef, ChallengeSummary, ExecutionMode, GuestArch, GuestConnection, GuestOs,
    KeepPolicy, NetworkMode, Resources, Session, SessionState,
};
use wargame_core::error::WargameError;
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::ProcessRunner;
use wargame_core::ports::{ImageRef, VmProvider, VmSpec};
use wargame_core::testing::{
    FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
};

fn tmp_path() -> std::path::PathBuf {
    tempfile::tempdir().unwrap().keep()
}

fn context() -> (AppContext, Arc<FakeVmProvider>) {
    let vm = Arc::new(FakeVmProvider::new());
    let ctx = AppContext {
        config: Default::default(),
        dirs: AppDirs { root: tmp_path() },
        wargame: Arc::new(FakeWargameProvider::new()),
        backends: BackendRegistry::new(vec![vm.clone() as Arc<dyn VmProvider>]),
        transports: TransportRegistry::new(Arc::new(FakeTransport::new())),
        repo: Arc::new(MemoryRepo::new()),
        builds: Arc::new(MemoryBuildRepo::new()),
        builders: BuilderRegistry::default(),
        disk: Arc::new(HostDiskSpace),
        profiles: ProfileStore::new(vec![], tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap: wargame_core::app::BootstrapScripts::new("#!/bin/bash\n", "# ps\n"),
    };
    (ctx, vm)
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
