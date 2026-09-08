//! Backend resolution, endpoint plumbing and on-disk compatibility
//! (spec.md §3 FR-1/FR-4/FR-5; PLAN task 2).
//!
//! `OLD_SESSION_JSON` and `OLD_PROFILE_YAML` below are verbatim copies of
//! records the pre-multi-OS build produced (the `Session` serde shape and
//! `profiles/macos-re-arm64.yaml` as of the commit before this task): they
//! must keep loading, and must resolve to exactly the behaviour that build
//! had -- tart / arm64 / virtualize / user `admin` / port 22 / POSIX shell.

use std::sync::Arc;
use std::time::Duration;
use wargame_core::app::BuilderRegistry;
use wargame_core::infra::HostDiskSpace;
use wargame_core::testing::MemoryBuildRepo;

use time::OffsetDateTime;

use wargame_core::app::backends::{
    provider_for_session, resolve_backend, BackendRegistry, TransportRegistry,
};
use wargame_core::app::profiles::ProfileStore;
use wargame_core::app::{list_sessions, prune, resume, stop, AppContext};
use wargame_core::config::Config;
use wargame_core::domain::{
    Backend, BackendChoice, Challenge, ChallengeRef, ChallengeSummary, ExecutionMode, GuestArch,
    GuestConnection, GuestOs, GuestShell, GuestSpec, ImageSpec, KeepPolicy, NetworkMode, Profile,
    ProvisionSpec, Resources, Session, SessionState, ToolsSpec, VmRef, WorkspaceSpec,
};
use wargame_core::error::WargameError;
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::ProcessRunner;
use wargame_core::ports::{ImageRef, VmProvider, VmSpec, VmStatus};
use wargame_core::testing::fake_transport::TransportCall;
use wargame_core::testing::{
    FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
};

/// A session record exactly as the pre-multi-OS build wrote it: no
/// `vm_mode`, no `connection`, no `workspace_root`, and a `vm_ref` with no
/// `id`.
const OLD_SESSION_JSON: &str = r#"{
  "id": "dh-out-of-bound-a1b2c3",
  "display_name": "out_of_bound",
  "challenge": {
    "ref": {
      "provider": "dreamhack",
      "id": "11"
    },
    "slug": "out-of-bound",
    "title": "out_of_bound",
    "categories": [
      "pwnable"
    ],
    "difficulty": null
  },
  "challenge_snapshot": {
    "summary": {
      "ref": {
        "provider": "dreamhack",
        "id": "11"
      },
      "slug": "out-of-bound",
      "title": "out_of_bound",
      "categories": [
        "pwnable"
      ],
      "difficulty": null
    },
    "description": "",
    "tags": [],
    "assets": [],
    "remote_endpoints": [],
    "source_url": "https://dreamhack.io"
  },
  "vm_provider": "tart",
  "profile": "macos-re-arm64",
  "guest_os": "macos",
  "guest_arch": "arm64",
  "network": "isolated",
  "resources": {
    "cpu": 4,
    "memory_mib": 8192,
    "disk_gib": 60
  },
  "created_at": "2026-09-05T12:34:56Z",
  "state": "ready",
  "vm_ref": {
    "backend": "tart",
    "name": "wg-dh-out-of-bound-a1b2c3"
  },
  "assets": [
    {
      "filename": "prob.zip",
      "sha256": "8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4",
      "size": 1234
    }
  ],
  "keep": "discard",
  "last_error": null,
  "runner_pid": 4242,
  "guest_ip": "192.168.64.7"
}"#;

/// `profiles/macos-re-arm64.yaml`, verbatim, from before `vm:` and
/// `guest.distribution` existed.
const OLD_PROFILE_YAML: &str = r#"apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: macos-re-arm64
  description: "macOS reversing: Ghidra, LLVM, Python/uv"

guest:
  os: macos
  arch: arm64
  user: admin
  password: admin # only used while building the profile image (DESIGN.md §5)

image:
  base: ghcr.io/cirruslabs/macos-sequoia-vanilla:latest

resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 60

provision:
  scripts:
    - scripts/macos/common.sh
    - scripts/macos/reversing.sh

tools:
  verify:
    - git --version
    - python3 --version
    - uv --version

workspace:
  root: /Users/admin/wargame
"#;

// --- FR-5: old records keep their old meaning ------------------------------

#[test]
fn old_session_json_deserializes_with_legacy_defaults() {
    let session: Session = serde_json::from_str(OLD_SESSION_JSON).expect("old record still loads");

    assert_eq!(session.vm_provider, "tart");
    assert_eq!(session.guest_arch, GuestArch::Arm64);
    assert_eq!(session.vm_mode, ExecutionMode::Virtualize);
    assert_eq!(session.guest_distribution, None);
    assert_eq!(session.connection.user, "admin");
    assert_eq!(session.connection.port, 22);
    assert_eq!(session.connection.shell, GuestShell::Posix);
    assert_eq!(session.connection.key_path, None);
    assert_eq!(session.workspace_root, "");

    // A pre-change `vm_ref` has no stable id; the display name stands in for
    // it so lifecycle operations still address the same VM.
    let vm_ref = session.vm_ref.as_ref().unwrap();
    assert_eq!(vm_ref.backend, "tart");
    assert_eq!(vm_ref.name, "wg-dh-out-of-bound-a1b2c3");
    assert_eq!(vm_ref.id, "wg-dh-out-of-bound-a1b2c3");

    // The endpoint rebuilt from that record is the pre-change SSH target.
    let endpoint = session.endpoint().expect("a session with an IP has one");
    assert_eq!(endpoint.user, "admin");
    assert_eq!(endpoint.port, 22);
    assert_eq!(endpoint.host.to_string(), "192.168.64.7");
    assert_eq!(endpoint.shell, GuestShell::Posix);
}

#[test]
fn old_profile_yaml_parses_and_resolves_to_tart_virtualize() {
    let profile = Profile::from_yaml(OLD_PROFILE_YAML).expect("old YAML still parses");
    assert_eq!(profile.name, "macos-re-arm64");
    assert_eq!(profile.resources.memory_mib, 8192);
    assert_eq!(profile.vm.backend, BackendChoice::Auto);
    assert_eq!(profile.vm.mode, ExecutionMode::Virtualize);
    assert_eq!(profile.guest.distribution, None);

    let resolved = resolve_backend(&profile).expect("macOS/arm64 resolves");
    assert_eq!(resolved.backend, Backend::Tart);
    assert_eq!(resolved.mode, ExecutionMode::Virtualize);
}

// --- FR-1: the support matrix ----------------------------------------------

/// One requested `(vm.backend, vm.mode, guest)` combination.
#[derive(Debug, Clone, Copy)]
struct Request {
    backend: BackendChoice,
    mode: ExecutionMode,
    os: GuestOs,
    distribution: Option<&'static str>,
    arch: GuestArch,
}

fn req(
    backend: BackendChoice,
    mode: ExecutionMode,
    os: GuestOs,
    distribution: Option<&'static str>,
    arch: GuestArch,
) -> Request {
    Request {
        backend,
        mode,
        os,
        distribution,
        arch,
    }
}

fn profile_for(request: Request) -> Profile {
    Profile {
        name: "under-test".to_string(),
        description: String::new(),
        vm: wargame_core::domain::VmSettings {
            backend: request.backend,
            mode: request.mode,
        },
        guest: GuestSpec {
            os: request.os,
            arch: request.arch,
            distribution: request.distribution.map(str::to_string),
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
        provision: ProvisionSpec::default(),
        tools: ToolsSpec::default(),
        workspace: WorkspaceSpec {
            root: "/w".to_string(),
        },
    }
}

#[test]
fn support_matrix_resolves_every_supported_combination() {
    use BackendChoice::{Auto, Tart, Utm};
    use ExecutionMode::{Emulate, Virtualize};
    use GuestArch::{Arm64, X86_64};
    use GuestOs::{Linux, Macos, Windows};

    // spec.md §1's release matrix, reached both via `auto` and by naming the
    // backend explicitly.
    let cases: &[(Request, Backend)] = &[
        (req(Auto, Virtualize, Macos, None, Arm64), Backend::Tart),
        (req(Tart, Virtualize, Macos, None, Arm64), Backend::Tart),
        (
            req(Auto, Virtualize, Linux, Some("kali"), Arm64),
            Backend::Utm,
        ),
        (
            req(Utm, Virtualize, Linux, Some("kali"), Arm64),
            Backend::Utm,
        ),
        (
            req(Auto, Emulate, Linux, Some("kali"), X86_64),
            Backend::Utm,
        ),
        (req(Utm, Emulate, Linux, Some("kali"), X86_64), Backend::Utm),
        (req(Auto, Emulate, Windows, None, X86_64), Backend::Utm),
        (req(Utm, Emulate, Windows, None, X86_64), Backend::Utm),
    ];

    for (request, expected) in cases {
        let profile = profile_for(*request);
        let resolved =
            resolve_backend(&profile).unwrap_or_else(|e| panic!("{request:?} should resolve: {e}"));
        assert_eq!(resolved.backend, *expected, "{request:?}");
        assert_eq!(resolved.mode, request.mode);
        assert_eq!(resolved.os, request.os);
        assert_eq!(resolved.arch, request.arch);
        assert_eq!(
            resolved.distribution.as_deref(),
            request.distribution,
            "{request:?}"
        );
    }
}

#[test]
fn support_matrix_rejects_unsupported_combinations_before_anything_happens() {
    use BackendChoice::{Auto, Tart, Utm};
    use ExecutionMode::{Emulate, Virtualize};
    use GuestArch::{Arm64, X86_64};
    use GuestOs::{Linux, Macos, Windows};

    // `(request, a fragment of the request the message must name back)`.
    let cases: &[(Request, &str)] = &[
        // Explicit `utm` with macOS.
        (req(Utm, Virtualize, Macos, None, Arm64), "macOS"),
        // Explicit `tart` with Linux / Windows.
        (req(Tart, Virtualize, Linux, Some("kali"), Arm64), "Linux"),
        (req(Tart, Emulate, Windows, None, X86_64), "Windows"),
        // x86_64 with virtualize (no hardware path on an Apple Silicon host).
        (req(Auto, Virtualize, Linux, Some("kali"), X86_64), "x86_64"),
        (req(Auto, Virtualize, Windows, None, X86_64), "x86_64"),
        // macOS with emulate.
        (req(Auto, Emulate, Macos, None, Arm64), "macOS"),
        (req(Tart, Emulate, Macos, None, Arm64), "macOS"),
        // Windows on arm64 is a follow-up, not a release combination.
        (req(Auto, Virtualize, Windows, None, Arm64), "Windows"),
        // `os: linux` without a known distribution.
        (req(Auto, Virtualize, Linux, None, Arm64), "Linux"),
    ];

    for (request, requested_fragment) in cases {
        let profile = profile_for(*request);
        let err = resolve_backend(&profile).expect_err(&format!("{request:?} must be rejected"));
        let WargameError::Incompatible(message) = &err else {
            panic!("expected Incompatible, got {err:?}");
        };
        // The requested combination...
        assert!(
            message.contains(requested_fragment),
            "{message} should name the requested {requested_fragment}"
        );
        assert!(
            message.contains(request.mode.as_str()),
            "{message} names the mode"
        );
        // ...a reason...
        assert!(
            message.to_lowercase().contains("reason"),
            "{message} states a reason"
        );
        // ...and the whole supported table.
        assert!(
            message.contains("Supported combinations"),
            "{message} lists the supported combinations"
        );
        assert!(message.contains("Linux(kali)/x86_64"), "{message}");
        assert!(message.contains("macOS/arm64"), "{message}");
    }
}

// --- FR-5: boot timeouts are derived per mode ------------------------------

#[test]
fn boot_timeout_is_derived_per_backend_and_mode() {
    let config = Config::default();
    assert_eq!(
        config
            .vm
            .boot_timeout(Backend::Tart, ExecutionMode::Virtualize),
        Duration::from_secs(300)
    );
    assert_eq!(
        config
            .vm
            .boot_timeout(Backend::Utm, ExecutionMode::Virtualize),
        Duration::from_secs(300)
    );
    assert_eq!(
        config.vm.boot_timeout(Backend::Utm, ExecutionMode::Emulate),
        Duration::from_secs(1800)
    );

    let overridden: Config = toml::from_str("[vm.utm]\nboot_timeout_secs = 60\n").unwrap();
    assert_eq!(
        overridden
            .vm
            .boot_timeout(Backend::Utm, ExecutionMode::Emulate),
        Duration::from_secs(60)
    );
}

// --- FR-5: lifecycle routes by the *session*, never the profile ------------

fn tmp_path() -> std::path::PathBuf {
    tempfile::tempdir().unwrap().keep()
}

struct Fixture {
    ctx: AppContext,
    tart: Arc<FakeVmProvider>,
    utm: Arc<FakeVmProvider>,
    transport: Arc<FakeTransport>,
}

fn utm_capabilities() -> wargame_core::domain::ProviderCapabilities {
    wargame_core::domain::ProviderCapabilities {
        guest_os: vec![GuestOs::Linux, GuestOs::Windows],
        guest_arch: vec![GuestArch::Arm64, GuestArch::X86_64],
        modes: vec![ExecutionMode::Virtualize, ExecutionMode::Emulate],
        networks: vec![NetworkMode::Isolated, NetworkMode::Nat],
        graphical_console: true,
        ssh: true,
        snapshots: false,
        copy_in: true,
        remote: false,
    }
}

fn fixture(profiles: Vec<Profile>) -> Fixture {
    let tart = Arc::new(FakeVmProvider::new());
    let utm = Arc::new(FakeVmProvider::with_backend("utm"));
    utm.set_capabilities(utm_capabilities());
    let transport = Arc::new(FakeTransport::new());
    let ctx = AppContext {
        config: Config::default(),
        dirs: AppDirs { root: tmp_path() },
        wargame: Arc::new(FakeWargameProvider::new()),
        backends: BackendRegistry::new(vec![
            tart.clone() as Arc<dyn VmProvider>,
            utm.clone() as Arc<dyn VmProvider>,
        ]),
        transports: TransportRegistry::new(transport.clone()),
        repo: Arc::new(MemoryRepo::new()),
        builds: Arc::new(MemoryBuildRepo::new()),
        builders: BuilderRegistry::default(),
        disk: Arc::new(HostDiskSpace),
        profiles: ProfileStore::new(profiles, tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap_script: "#!/bin/bash\n",
    };
    Fixture {
        ctx,
        tart,
        utm,
        transport,
    }
}

#[allow(clippy::too_many_arguments)]
fn session(id: &str, backend: &str, vm_name: &str, vm_id: &str, state: SessionState) -> Session {
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
        vm_provider: backend.to_string(),
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
        vm_ref: Some(VmRef {
            backend: backend.to_string(),
            id: vm_id.to_string(),
            name: vm_name.to_string(),
        }),
        assets: vec![],
        keep: KeepPolicy::Discard,
        last_error: None,
        runner_pid: None,
        guest_ip: Some("10.0.0.5".parse().unwrap()),
        connection: GuestConnection::default(),
        workspace_root: "/Users/admin/wargame".to_string(),
    }
}

async fn make_vm(provider: &FakeVmProvider, name: &str) {
    let spec = VmSpec::new(
        name.to_string(),
        ImageRef("img".to_string()),
        Resources {
            cpu: 1,
            memory_mib: 1024,
            disk_gib: 10,
        },
    );
    let vm_ref = provider.create(&spec).await.unwrap();
    provider
        .start(
            &vm_ref,
            &wargame_core::ports::StartOptions {
                network: NetworkMode::Isolated,
                headless: true,
                log_path: "/dev/null".into(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn two_backends_with_the_same_vm_name_are_not_confused() {
    let Fixture { ctx, tart, utm, .. } = fixture(vec![]);

    // The *same* VM name exists on both backends.
    make_vm(&tart, "wg-shared-name").await;
    make_vm(&utm, "wg-shared-name").await;

    let tart_session = session(
        "s-tart",
        "tart",
        "wg-shared-name",
        "wg-shared-name",
        SessionState::Running,
    );
    let utm_session = session(
        "s-utm",
        "utm",
        "wg-shared-name",
        "0C4D5D8E-1111-2222-3333-444455556666",
        SessionState::Running,
    );
    ctx.repo.save(&tart_session).await.unwrap();
    ctx.repo.save(&utm_session).await.unwrap();

    // Routing comes from the session, not from a global default.
    assert_eq!(
        provider_for_session(&ctx, &utm_session).unwrap().name(),
        "utm"
    );
    assert_eq!(
        provider_for_session(&ctx, &tart_session).unwrap().name(),
        "tart"
    );

    stop(&ctx, "s-utm").await.unwrap();

    let vm_ref = utm_session.vm_ref.clone().unwrap();
    assert_eq!(utm.status(&vm_ref).await.unwrap(), VmStatus::Stopped);
    let tart_ref = tart_session.vm_ref.clone().unwrap();
    assert_eq!(
        tart.status(&tart_ref).await.unwrap(),
        VmStatus::Running,
        "the identically named Tart VM must be untouched"
    );
    assert!(
        !tart.calls().iter().any(|c| c == "stop"),
        "the Tart backend must never see a UTM session's stop"
    );
}

#[tokio::test]
async fn prune_keeps_identically_named_vms_on_other_backends() {
    let Fixture { ctx, tart, utm, .. } = fixture(vec![]);
    make_vm(&tart, "wg-shared-name").await;
    make_vm(&utm, "wg-shared-name").await;

    // Only the UTM VM has a session record; the Tart one is a real orphan.
    // Ownership is keyed by `(backend, provider id)`, and both fakes mint a
    // Tart-style id (the VM name), so this is the hardest case: identical
    // names *and* identical ids on two different backends.
    ctx.repo
        .save(&session(
            "s-utm",
            "utm",
            "wg-shared-name",
            "wg-shared-name",
            SessionState::Running,
        ))
        .await
        .unwrap();

    let orphans = prune(&ctx, true).await.unwrap();
    assert_eq!(orphans.len(), 1, "exactly one orphan: {orphans:?}");
    assert!(orphans[0].contains("tart"), "{orphans:?}");

    prune(&ctx, false).await.unwrap();
    assert!(
        utm.vm_names().contains(&"wg-shared-name".to_string()),
        "the UTM VM with a live session must survive prune"
    );
    assert!(tart.vm_names().is_empty(), "the Tart orphan is deleted");
}

#[tokio::test]
async fn resume_uses_the_saved_backend_and_connection_after_the_profile_changed() {
    // The profile the session was launched from now says something else
    // entirely (a Tart/macOS profile renamed onto the same key).
    let mut edited = profile_for(req(
        BackendChoice::Tart,
        ExecutionMode::Virtualize,
        GuestOs::Macos,
        None,
        GuestArch::Arm64,
    ));
    edited.name = "p".to_string();
    edited.guest.user = "admin".to_string();

    let Fixture {
        ctx,
        tart,
        utm,
        transport,
    } = fixture(vec![edited]);

    make_vm(&utm, "wg-kali").await;
    let mut stopped = session(
        "s-kali",
        "utm",
        "wg-kali",
        "0C4D5D8E-1111-2222-3333-444455556666",
        SessionState::Stopped,
    );
    stopped.guest_os = GuestOs::Linux;
    stopped.guest_distribution = Some("kali".to_string());
    stopped.guest_arch = GuestArch::X86_64;
    stopped.vm_mode = ExecutionMode::Emulate;
    stopped.connection = GuestConnection {
        user: "kali".to_string(),
        port: 22022,
        shell: GuestShell::Posix,
        key_path: None,
    };
    ctx.repo.save(&stopped).await.unwrap();

    let resumed = resume(&ctx, "s-kali").await.unwrap();
    assert_eq!(resumed.state, SessionState::Running);
    assert_eq!(resumed.vm_provider, "utm");

    assert!(utm.calls().iter().any(|c| c == "start"));
    assert!(
        !tart.calls().iter().any(|c| c == "start"),
        "the edited profile's backend must not be used to resume"
    );

    // The emulate boot timeout, not the virtualize one, was used.
    assert_eq!(
        utm.wait_ip_timeouts().first().copied(),
        Some(Duration::from_secs(1800))
    );

    // The transport was addressed with the *saved* user and port.
    let endpoints: Vec<_> = transport
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            TransportCall::WaitReady { endpoint } => Some(endpoint),
            _ => None,
        })
        .collect();
    assert!(!endpoints.is_empty(), "resume waits for the guest");
    assert_eq!(endpoints[0].user, "kali");
    assert_eq!(endpoints[0].port, 22022);
}

#[tokio::test]
async fn sessions_with_distinct_users_and_ports_get_distinct_endpoints() {
    let Fixture {
        ctx,
        tart,
        utm,
        transport,
    } = fixture(vec![]);
    make_vm(&tart, "wg-macos").await;
    make_vm(&utm, "wg-kali").await;

    let mut macos = session(
        "s-macos",
        "tart",
        "wg-macos",
        "wg-macos",
        SessionState::Running,
    );
    macos.connection = GuestConnection {
        user: "admin".to_string(),
        port: 22,
        shell: GuestShell::Posix,
        key_path: None,
    };
    let mut kali = session(
        "s-kali",
        "utm",
        "wg-kali",
        "0C4D5D8E-1111-2222-3333-444455556666",
        SessionState::Running,
    );
    kali.connection = GuestConnection {
        user: "kali".to_string(),
        port: 22022,
        shell: GuestShell::Posix,
        key_path: None,
    };
    ctx.repo.save(&macos).await.unwrap();
    ctx.repo.save(&kali).await.unwrap();

    stop(&ctx, "s-macos").await.unwrap();
    stop(&ctx, "s-kali").await.unwrap();

    let mut seen: Vec<(String, u16)> = transport
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            TransportCall::Exec { endpoint, .. } => Some((endpoint.user, endpoint.port)),
            _ => None,
        })
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen,
        vec![("admin".to_string(), 22), ("kali".to_string(), 22022)]
    );
}

#[tokio::test]
async fn a_session_whose_backend_is_not_wired_reports_it_by_name() {
    let tart = Arc::new(FakeVmProvider::new());
    let ctx = AppContext {
        config: Config::default(),
        dirs: AppDirs { root: tmp_path() },
        wargame: Arc::new(FakeWargameProvider::new()),
        backends: BackendRegistry::new(vec![tart as Arc<dyn VmProvider>]),
        transports: TransportRegistry::new(Arc::new(FakeTransport::new())),
        repo: Arc::new(MemoryRepo::new()),
        builds: Arc::new(MemoryBuildRepo::new()),
        builders: BuilderRegistry::default(),
        disk: Arc::new(HostDiskSpace),
        profiles: ProfileStore::new(vec![], tmp_path()),
        runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
        bootstrap_script: "#!/bin/bash\n",
    };
    let orphaned = session(
        "s-utm",
        "utm",
        "wg-kali",
        "0C4D5D8E-1111-2222-3333-444455556666",
        SessionState::Running,
    );
    ctx.repo.save(&orphaned).await.unwrap();

    let message = provider_for_session(&ctx, &orphaned)
        .err()
        .expect("an unwired backend is an error")
        .to_string();
    assert!(message.contains("utm"), "{message}");

    // Listing must still work, though: an unreachable backend leaves its
    // session records exactly as they are rather than failing the whole
    // command.
    let listed = list_sessions(&ctx).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, SessionState::Running);
}
