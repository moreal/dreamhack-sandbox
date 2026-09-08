//! Integration tests for `wargame_core::app::launch` (PLAN.md Task 5, Step
//! 1): the full pipeline against `wargame_core::testing`'s fakes, exercised
//! as an external crate would use it (the public `AppContext` API only).

use std::sync::Arc;
use wargame_core::app::BuilderRegistry;
use wargame_core::infra::HostDiskSpace;
use wargame_core::testing::MemoryBuildRepo;

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
        builds: Arc::new(MemoryBuildRepo::new()),
        builders: BuilderRegistry::default(),
        disk: Arc::new(HostDiskSpace),
        profiles: ProfileStore::new(vec![profile()], tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap: wargame_core::app::BootstrapScripts::new("#!/bin/bash\necho hi\n", "# ps\n"),
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

// --- OS-aware provisioning (spec.md §3 FR-4, §7 AC-07) ----------------------

/// A macOS launch must still copy in and run exactly what it always did,
/// plus the digest list the guest now checks itself against.
#[tokio::test]
async fn a_posix_launch_bootstraps_the_way_it_always_did() {
    let (ctx, _vm, wargame) = context();
    wargame.set_authenticated("tester");
    let transport = Arc::new(FakeTransport::new());
    let mut ctx = ctx;
    ctx.transports = TransportRegistry::new(transport.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    launch(&ctx, request(), tx).await.unwrap();

    let (execs, copies) = transport_calls(&transport);
    assert!(
        execs
            .iter()
            .any(|argv| argv[0] == "mkdir" && argv[1] == "-p"),
        "{execs:?}"
    );
    let bootstrap = execs
        .iter()
        .find(|argv| argv[0] == "bash")
        .expect("the bootstrap is run with bash");
    assert!(bootstrap[1].ends_with("/.wg/bootstrap.sh"), "{bootstrap:?}");
    assert_eq!(bootstrap[2], "/Users/admin/wargame/hello-fixture");
    assert_eq!(bootstrap[3], "--extract=true");
    assert_eq!(
        bootstrap[4], "--require-digests",
        "a missing digest list must fail the guest, not be skipped"
    );

    assert!(
        copies
            .iter()
            .any(|(src, dest)| src.ends_with("bootstrap.sh")
                && dest == "/Users/admin/wargame/.wg/"),
        "{copies:?}"
    );
    assert!(
        copies
            .iter()
            .any(|(src, dest)| src.ends_with(".wargame-sha256")
                && dest == "/Users/admin/wargame/hello-fixture/incoming/"),
        "the host's digests must reach the guest: {copies:?}"
    );
}

/// The same launch against a Windows guest speaks PowerShell and Windows
/// paths throughout, with no POSIX command or separator anywhere.
#[tokio::test]
async fn a_windows_launch_provisions_with_powershell_and_windows_paths() {
    use wargame_core::domain::build::{BuildRecord, BuildSource, PublishedImage};
    use wargame_core::domain::{
        BackendChoice, ExecutionMode, ProviderCapabilities, VmSettings, WorkspaceSpec,
    };

    let mut windows = profile();
    windows.name = "windows-x86_64".to_string();
    windows.vm = VmSettings {
        backend: BackendChoice::Utm,
        mode: ExecutionMode::Emulate,
    };
    windows.guest.os = GuestOs::Windows;
    windows.guest.arch = GuestArch::X86_64;
    windows.guest.user = "wargame".to_string();
    windows.image.base = String::new();
    windows.workspace = WorkspaceSpec {
        root: r"C:\wargame".to_string(),
    };

    let utm = Arc::new(FakeVmProvider::with_backend("utm"));
    utm.set_capabilities(ProviderCapabilities {
        guest_os: vec![GuestOs::Windows, GuestOs::Linux],
        guest_arch: vec![GuestArch::Arm64, GuestArch::X86_64],
        modes: vec![ExecutionMode::Virtualize, ExecutionMode::Emulate],
        networks: vec![NetworkMode::Isolated, NetworkMode::Nat],
        graphical_console: true,
        ssh: true,
        snapshots: false,
        copy_in: true,
        remote: false,
    });
    utm.add_image("published-windows-image");

    // A UTM profile launches from the image its build record published.
    let mut record = BuildRecord::new(
        &windows,
        "utm",
        BuildSource::UtmVm {
            uuid: "installer".to_string(),
        },
    );
    record.image = Some(PublishedImage::new("published-windows-image", "wg-windows"));
    record.phase = wargame_core::domain::build::BuildPhase::Ready;
    let builds = Arc::new(MemoryBuildRepo::new());
    builds.insert(record);

    let wargame = Arc::new(FakeWargameProvider::new());
    wargame.set_authenticated("tester");
    let transport = Arc::new(FakeTransport::new());
    let ctx = AppContext {
        config: Default::default(),
        dirs: AppDirs { root: tmp_path() },
        wargame,
        backends: BackendRegistry::new(vec![utm as Arc<dyn VmProvider>]),
        transports: TransportRegistry::new(transport.clone()).with_powershell(transport.clone()),
        repo: Arc::new(MemoryRepo::new()),
        builds,
        builders: BuilderRegistry::default(),
        disk: Arc::new(HostDiskSpace),
        profiles: ProfileStore::new(vec![windows], tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap: wargame_core::app::BootstrapScripts::new("#!/bin/bash\n", "# ps1\n"),
    };

    let mut req = request();
    req.profile = "windows-x86_64".to_string();
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let session = launch(&ctx, req, tx).await.expect("a Windows launch");
    assert_eq!(session.state, SessionState::Ready);
    assert_eq!(session.connection.shell.as_str(), "powershell");
    assert_eq!(session.workspace_root, r"C:\wargame");

    let (execs, copies) = transport_calls(&transport);
    let flat: Vec<String> = execs.iter().flatten().cloned().collect();
    // The workspace is made with a cmdlet, which reaches the guest as a
    // trusted script: parameter names bare, only the path quoted.
    assert!(
        flat.iter()
            .any(|a| a == r"New-Item -ItemType Directory -Force -Path 'C:\wargame\.wg' | Out-Null"),
        "{execs:?}"
    );
    assert!(
        !flat.iter().any(|a| a == "mkdir" || a == "bash"),
        "no POSIX command may reach a Windows guest: {execs:?}"
    );
    let bootstrap = execs
        .iter()
        .find(|argv| argv.iter().any(|a| a.ends_with("bootstrap.ps1")))
        .expect("the Windows bootstrap runs");
    assert_eq!(bootstrap[0], "powershell");
    assert!(
        bootstrap.contains(&"-Workspace".to_string()),
        "{bootstrap:?}"
    );
    assert!(
        bootstrap.contains(&r"C:\wargame\hello-fixture".to_string()),
        "{bootstrap:?}"
    );
    assert!(
        bootstrap.contains(&"-Extract".to_string()) && bootstrap.contains(&"true".to_string()),
        "{bootstrap:?}"
    );
    assert!(
        !bootstrap.iter().any(|a| a.contains('/')),
        "Windows guest paths never use POSIX separators: {bootstrap:?}"
    );
    assert!(
        bootstrap.contains(&"-RequireDigests".to_string()),
        "a missing digest list must fail the guest, not be skipped: {bootstrap:?}"
    );

    assert!(
        copies
            .iter()
            .any(|(src, dest)| src.ends_with("bootstrap.ps1") && dest == r"C:\wargame\.wg\"),
        "{copies:?}"
    );
    assert!(
        copies
            .iter()
            .any(|(src, dest)| src.ends_with(".wargame-sha256")
                && dest == r"C:\wargame\hello-fixture\incoming\"),
        "{copies:?}"
    );
}

/// The exec argv and copy_in (src, dest) pairs a transport saw, in order.
fn transport_calls(transport: &Arc<FakeTransport>) -> (Vec<Vec<String>>, Vec<(String, String)>) {
    use wargame_core::testing::TransportCall;
    let mut execs = Vec::new();
    let mut copies = Vec::new();
    for call in transport.calls() {
        match call {
            TransportCall::Exec { argv, .. } => execs.push(argv),
            TransportCall::CopyIn { src, dest, .. } => {
                copies.push((src.display().to_string(), dest))
            }
            TransportCall::ExecScript { script, .. } => execs.push(vec![script]),
            TransportCall::WaitReady { .. } => {}
        }
    }
    (execs, copies)
}
