//! Integration tests for `wargame_core::app::launch` (PLAN.md Task 5, Step
//! 1): the full pipeline against `wargame_core::testing`'s fakes, exercised
//! as an external crate would use it (the public `AppContext` API only).

use std::sync::Arc;

use wargame_core::app::profiles::ProfileStore;
use wargame_core::app::{
    launch, AppContext, BackendRegistry, LaunchEvent, LaunchRequest, TransportRegistry,
};
use wargame_core::domain::{
    ChallengeRef, GuestArch, GuestOs, GuestSpec, ImageSpec, NetworkMode, Profile, ProvisionSpec,
    Resources, SessionState, ToolsSpec, VmSettings, WorkspaceSpec,
};
use wargame_core::error::WargameError;
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::ProcessRunner;
use wargame_core::ports::VmProvider;
use wargame_core::testing::{
    FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
};

fn tmp_path() -> std::path::PathBuf {
    tempfile::tempdir().unwrap().keep()
}

fn profile() -> Profile {
    Profile {
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
            base: "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest".to_string(),
        },
        resources: Resources {
            cpu: 2,
            memory_mib: 4096,
            disk_gib: 1,
        },
        provision: ProvisionSpec::default(),
        tools: ToolsSpec::default(),
        workspace: WorkspaceSpec {
            root: "/Users/admin/wargame".to_string(),
        },
    }
}

fn context() -> (AppContext, Arc<FakeVmProvider>, Arc<FakeWargameProvider>) {
    let vm = Arc::new(FakeVmProvider::new());
    vm.add_image(profile().image_name());
    let wargame = Arc::new(FakeWargameProvider::new());
    let ctx = AppContext {
        config: Default::default(),
        dirs: AppDirs { root: tmp_path() },
        wargame: wargame.clone(),
        backends: BackendRegistry::new(vec![vm.clone() as Arc<dyn VmProvider>]),
        transports: TransportRegistry::new(Arc::new(FakeTransport::new())),
        repo: Arc::new(MemoryRepo::new()),
        profiles: ProfileStore::new(vec![profile()], tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap_script: "#!/bin/bash\necho hi\n",
    };
    (ctx, vm, wargame)
}

fn request() -> LaunchRequest {
    LaunchRequest {
        challenge: ChallengeRef::parse("dreamhack:1").unwrap(),
        profile: "macos-minimal-arm64".to_string(),
        network: NetworkMode::Isolated,
        resources: None,
        extract: true,
        gui: false,
    }
}

#[tokio::test]
async fn happy_path_reaches_ready_and_persists_the_session() {
    let (ctx, _vm, wargame) = context();
    wargame.set_authenticated("tester");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let session = launch(&ctx, request(), tx).await.expect("launch succeeds");
    assert_eq!(session.state, SessionState::Ready);
    assert_eq!(session.assets.len(), 1);
    assert!(session.guest_ip.is_some());
    assert!(session.vm_ref.is_some());

    let persisted = ctx.repo.load(&session.id).await.unwrap();
    assert_eq!(persisted.state, SessionState::Ready);

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, LaunchEvent::SessionCreated { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, LaunchEvent::Ready { .. })));
}

#[tokio::test]
async fn authentication_required_before_any_session_or_vm() {
    let (ctx, vm, _wargame) = context();
    // Provider stays anonymous; the fixture challenge always has one asset.
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let err = launch(&ctx, request(), tx).await.unwrap_err();
    assert!(matches!(err, WargameError::AuthenticationRequired { .. }));
    assert!(ctx.repo.list().await.unwrap().is_empty());
    assert!(vm.vm_names().is_empty());
}

#[tokio::test]
async fn image_missing_is_reported_and_creates_no_session() {
    let (mut ctx, ..) = context();
    // A registry whose tart backend has no image registered.
    ctx.backends =
        BackendRegistry::new(vec![Arc::new(FakeVmProvider::new()) as Arc<dyn VmProvider>]);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let err = launch(&ctx, request(), tx).await.unwrap_err();
    assert!(matches!(err, WargameError::ImageMissing { .. }));
    assert!(err.hint().unwrap().contains("profile build"));
    assert!(ctx.repo.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn incompatible_profile_creates_no_session() {
    let (mut ctx, ..) = context();
    let mut windows_profile = profile();
    windows_profile.guest.os = GuestOs::Windows;
    windows_profile.guest.arch = GuestArch::X86_64;
    ctx.profiles = ProfileStore::new(vec![windows_profile], tmp_path());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let err = launch(&ctx, request(), tx).await.unwrap_err();
    assert!(matches!(err, WargameError::Incompatible(_)));
    assert!(ctx.repo.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_launch_keeps_its_vm_ref_so_it_can_be_deleted() {
    let (ctx, vm, wargame) = context();
    wargame.set_authenticated("tester");
    vm.set_fail_on("wait_ip");
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let err = launch(&ctx, request(), tx).await.unwrap_err();
    assert!(matches!(err, WargameError::GuestBootTimeout(_)));

    let sessions = ctx.repo.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].state, SessionState::Failed);
    assert!(sessions[0].last_error.is_some());
    assert!(sessions[0].vm_ref.is_some());
}
