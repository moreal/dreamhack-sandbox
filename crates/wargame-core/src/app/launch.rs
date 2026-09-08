//! The launch pipeline (DESIGN.md §3.4; MVP.md §14-15): preflight checks
//! run before any session exists, a session record persisted before any VM
//! exists (NFR-02), VM creation, asset download (staged, hashed, never
//! executed host-side), boot, and guest-side bootstrap.

use std::sync::Arc;

use time::OffsetDateTime;
use tokio::sync::mpsc::Sender;

use crate::domain::{
    Challenge, ChallengeRef, GuestConnection, GuestEndpoint, GuestShell, KeepPolicy, NetworkMode,
    Profile, ResolvedBackend, Resources, Session, SessionAsset, SessionState,
};
use crate::error::{Result, WargameError};
use crate::infra::GuestOps;
use crate::ports::{
    AuthState, DownloadedFile, GuestTransport, ImageRef, StartOptions, VmProvider, VmSpec,
};
use crate::security::StagingDir;

use super::backends::provider_for_profile;
use super::AppContext;

/// A request to launch one challenge into a fresh session (DESIGN.md §3.4).
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub challenge: ChallengeRef,
    pub profile: String,
    pub network: NetworkMode,
    pub resources: Option<Resources>,
    pub extract: bool,
    pub gui: bool,
}

/// Progress events emitted by [`launch`] (DESIGN.md §3.4): the CLI prints
/// each on its own line and the TUI's launch screen renders them as a log.
#[derive(Debug, Clone)]
pub enum LaunchEvent {
    Preflight(String),
    SessionCreated {
        id: String,
    },
    VmCreated {
        name: String,
    },
    Downloading {
        filename: String,
    },
    Downloaded {
        filename: String,
        sha256: String,
        size: u64,
    },
    Booting,
    GuestIp(std::net::IpAddr),
    Bootstrapping,
    Ready {
        session_id: String,
        ssh_hint: String,
    },
    Failed {
        session_id: Option<String>,
        error: String,
        hint: Option<String>,
    },
}

const GIB: u64 = 1024 * 1024 * 1024;
/// A Tart clone is copy-on-write, so it does not need a profile's *entire*
/// declared disk size free up front — DESIGN.md §3.4 asks for a quarter of
/// it as headroom.
const MIN_DISK_FRACTION: f64 = 0.25;

async fn emit(events: &Sender<LaunchEvent>, event: LaunchEvent) {
    // The receiver may already be gone (e.g. a CLI command that only cares
    // about the final `Result`); progress is best-effort.
    let _ = events.send(event).await;
}

/// A short prefix for session ids, e.g. `"dh"` for `"dreamhack"`
/// (DESIGN.md §3.4's `"dh-<slug>-<rand6>"` example).
fn provider_short_code(provider_name: &str) -> String {
    if provider_name == "dreamhack" {
        return "dh".to_string();
    }
    let lower: String = provider_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if lower.len() >= 2 {
        lower[..2].to_string()
    } else if lower.is_empty() {
        "xx".to_string()
    } else {
        lower
    }
}

fn rand_hex6() -> String {
    let bytes: [u8; 3] = rand::random();
    hex::encode(bytes)
}

/// Runs the full launch pipeline (DESIGN.md §3.4).
///
/// Preflight checks (profile/compat, VM backend, image presence, disk
/// space, challenge lookup, auth) all happen before any session record is
/// written. Once the session exists, every later failure moves it to
/// `Failed` (with `last_error` set) and is still returned as `Err` — the VM
/// (if one was created) is left in place so `wargame delete <id>` can clean
/// it up.
pub async fn launch(
    ctx: &AppContext,
    req: LaunchRequest,
    events: Sender<LaunchEvent>,
) -> Result<Session> {
    emit(
        &events,
        LaunchEvent::Preflight("loading profile".to_string()),
    )
    .await;
    let profile = ctx.profiles.get(&req.profile)?;

    // Resolution first: it is pure data, so an unsupported OS/arch/mode
    // combination is refused before any backend is even contacted, let alone
    // asked to create, clone or start something (spec.md §3 FR-1).
    let (execution, vm) = provider_for_profile(ctx, &profile)?;
    // Say what `backend: auto` and the profile's guest actually resolved to
    // before anything is created, so an emulated x86_64 boot's long wait is
    // never a surprise (spec.md §3 FR-1, §6's "긴 emulate 부팅").
    emit(
        &events,
        LaunchEvent::Preflight(format!(
            "profile {}: {} on {}",
            profile.name,
            execution.guest_label(),
            execution.backend_label(),
        )),
    )
    .await;
    let capabilities = vm.capabilities();
    crate::domain::compat::check(&profile, vm.name(), &capabilities)?;
    if !capabilities.supports_network(req.network) {
        return Err(WargameError::NetworkUnavailable(format!(
            "the {} backend cannot run a session on the {} network",
            vm.name(),
            req.network,
        )));
    }

    // A guest whose shell has no wired transport can never be provisioned,
    // so refuse it here rather than after a VM has been created and booted
    // (spec.md §3 FR-4; §6's "VM/세션 생성 전 실패").
    ctx.transports
        .for_shell(GuestShell::for_os(profile.guest.os))?;

    emit(
        &events,
        LaunchEvent::Preflight("checking VM backend".to_string()),
    )
    .await;
    let backend = vm.healthcheck().await?;
    if !backend.available {
        return Err(WargameError::BackendUnavailable {
            backend: vm.name().to_string(),
            reason: backend.detail,
        });
    }
    if req.network == NetworkMode::Isolated && backend.isolated_network == Some(false) {
        return Err(WargameError::NetworkUnavailable(format!(
            "isolated networking is not available on this host: {}",
            backend.detail
        )));
    }

    // Which image this profile launches from is a per-backend question:
    // Tart names a local image, UTM names the UUID of the VM its build
    // record published (spec.md §5 item 5). An unbuilt profile is refused
    // here, before anything is created.
    let image = crate::app::profile_build::launch_image(ctx, &profile).await?;
    if !vm.image_exists(&image).await? {
        return Err(WargameError::ImageMissing {
            profile: profile.name.clone(),
            image: image.0.clone(),
        });
    }
    emit(
        &events,
        LaunchEvent::Preflight(format!("image ready: {}", image.0)),
    )
    .await;

    let required_bytes =
        (profile.resources.disk_gib as f64 * MIN_DISK_FRACTION * GIB as f64) as u64;
    let free_bytes = ctx.disk.free_bytes(&ctx.dirs.root)?;
    if free_bytes < required_bytes {
        return Err(WargameError::InsufficientResources(format!(
            "profile {:?} needs at least {:.1} GiB free, only {:.1} GiB available at {}",
            profile.name,
            required_bytes as f64 / GIB as f64,
            free_bytes as f64 / GIB as f64,
            ctx.dirs.root.display(),
        )));
    }

    emit(
        &events,
        LaunchEvent::Preflight("loading challenge".to_string()),
    )
    .await;
    let challenge = ctx.wargame.get_challenge(&req.challenge.id).await?;

    if !challenge.assets.is_empty() {
        let auth = ctx.wargame.auth_state().await?;
        if matches!(auth, AuthState::Anonymous) {
            return Err(WargameError::AuthenticationRequired {
                provider: ctx.wargame.name().to_string(),
            });
        }
    }

    // --- Preflight is done: from here a session record exists. ---

    let resources = req.resources.unwrap_or(profile.resources);
    let session_id = Session::new_id(
        &provider_short_code(ctx.wargame.name()),
        &challenge.summary.slug,
        &rand_hex6(),
    );
    let mut session = Session {
        id: session_id.clone(),
        display_name: challenge.summary.title.clone(),
        challenge: challenge.summary.clone(),
        challenge_snapshot: challenge.clone(),
        vm_provider: execution.backend.as_str().to_string(),
        profile: profile.name.clone(),
        guest_os: profile.guest.os,
        guest_arch: profile.guest.arch,
        guest_distribution: execution.distribution.clone(),
        vm_mode: execution.mode,
        network: req.network,
        resources,
        created_at: OffsetDateTime::now_utc(),
        state: SessionState::Creating,
        vm_ref: None,
        assets: Vec::new(),
        keep: KeepPolicy::Discard,
        last_error: None,
        runner_pid: None,
        guest_ip: None,
        // Snapshotted now so `resume`/`ssh` reconnect the way this launch
        // did even if the profile is edited afterwards (FR-5).
        connection: GuestConnection::new(profile.guest.user.clone(), profile.guest.os),
        workspace_root: profile.workspace.root.clone(),
    };
    ctx.repo.save(&session).await?;
    emit(
        &events,
        LaunchEvent::SessionCreated {
            id: session_id.clone(),
        },
    )
    .await;

    match run_launch(
        ctx,
        &req,
        &profile,
        &image,
        &challenge,
        &mut session,
        &execution,
        &vm,
        &events,
    )
    .await
    {
        Ok(()) => Ok(session),
        Err(err) => Err(fail_session(ctx, &mut session, err, &events).await),
    }
}

/// Moves `session` to `Failed`, records `last_error`, persists it (VM ref
/// included, if it was ever set) and emits `LaunchEvent::Failed`. Returns
/// `err` unchanged so callers can `return Err(fail_session(...).await)`.
async fn fail_session(
    ctx: &AppContext,
    session: &mut Session,
    err: WargameError,
    events: &Sender<LaunchEvent>,
) -> WargameError {
    let hint = err.hint();
    let message = err.to_string();
    let _ = session.transition(SessionState::Failed);
    session.last_error = Some(message.clone());
    let _ = ctx.repo.save(session).await;
    emit(
        events,
        LaunchEvent::Failed {
            session_id: Some(session.id.clone()),
            error: message,
            hint,
        },
    )
    .await;
    err
}

/// Steps 3-8 of DESIGN.md §3.4: VM creation through to `Ready`. `session`
/// is saved after every state change so a crash mid-pipeline leaves an
/// accurate on-disk record.
#[allow(clippy::too_many_arguments)]
async fn run_launch(
    ctx: &AppContext,
    req: &LaunchRequest,
    profile: &Profile,
    // The already-resolved image for this profile's backend.
    image: &ImageRef,
    challenge: &Challenge,
    session: &mut Session,
    execution: &ResolvedBackend,
    vm: &Arc<dyn VmProvider>,
    events: &Sender<LaunchEvent>,
) -> Result<()> {
    let boot_timeout = ctx
        .config
        .vm
        .boot_timeout(execution.backend, execution.mode);
    let vm_spec = VmSpec {
        // The prefix belongs to the backend the profile resolved to, so a
        // UTM VM is never named from `[vm.tart]` (spec.md §3 FR-5).
        name: format!(
            "{}{}",
            ctx.config.vm.vm_prefix(execution.backend),
            session.id
        ),
        image: image.clone(),
        resources: session.resources,
        execution: execution.clone(),
        network: session.network,
    };
    let vm_ref = vm.create(&vm_spec).await?;
    session.vm_ref = Some(vm_ref.clone());
    ctx.repo.save(session).await?;
    emit(
        events,
        LaunchEvent::VmCreated {
            name: vm_ref.name.clone(),
        },
    )
    .await;

    session.transition(SessionState::Downloading)?;
    ctx.repo.save(session).await?;

    let staging = StagingDir::create(&ctx.dirs.cache_dir(), ctx.config.debug.keep_staging)?;
    let mut downloaded_files: Vec<DownloadedFile> = Vec::new();
    for asset in &challenge.assets {
        emit(
            events,
            LaunchEvent::Downloading {
                filename: asset.filename.clone(),
            },
        )
        .await;
        let mut sink = staging.sink(&asset.id);
        let result = ctx
            .wargame
            .download_asset(challenge, asset, &mut sink)
            .await?;
        for file in result.files {
            session.assets.push(SessionAsset {
                filename: file.filename.clone(),
                sha256: file.sha256.clone(),
                size: file.size,
            });
            emit(
                events,
                LaunchEvent::Downloaded {
                    filename: file.filename.clone(),
                    sha256: file.sha256.clone(),
                    size: file.size,
                },
            )
            .await;
            downloaded_files.push(file);
        }
    }
    ctx.repo.save(session).await?;

    session.transition(SessionState::Booting)?;
    ctx.repo.save(session).await?;
    emit(events, LaunchEvent::Booting).await;

    let start_opts = StartOptions {
        network: session.network,
        headless: !req.gui,
        log_path: ctx.repo.session_dir(&session.id).join("run.log"),
    };
    let runtime = vm.start(&vm_ref, &start_opts).await?;
    session.runner_pid = runtime.pid;
    ctx.repo.save(session).await?;

    let ip = vm.wait_ip(&vm_ref, boot_timeout).await?;
    session.guest_ip = Some(ip);
    ctx.repo.save(session).await?;
    emit(events, LaunchEvent::GuestIp(ip)).await;

    let endpoint = session.connection.endpoint(ip);
    let transport = ctx.transports.for_endpoint(&endpoint)?;
    transport.wait_ready(&endpoint, boot_timeout).await?;

    session.transition(SessionState::Provisioning)?;
    ctx.repo.save(session).await?;
    emit(events, LaunchEvent::Bootstrapping).await;

    provision_guest(
        ctx,
        req,
        profile,
        challenge,
        &endpoint,
        transport.as_ref(),
        &staging,
        &downloaded_files,
    )
    .await?;

    session.transition(SessionState::Ready)?;
    ctx.repo.save(session).await?;
    emit(
        events,
        LaunchEvent::Ready {
            session_id: session.id.clone(),
            ssh_hint: format!("wargame ssh {}", session.id),
        },
    )
    .await;

    Ok(())
}

/// Writes the trusted bootstrap script and per-launch metadata into the
/// guest workspace (MVP.md §15's `<root>/<slug>/{incoming,work,notes,
/// output}/` layout, created by the bootstrap script itself) and runs it.
///
/// Every command and path here comes from [`GuestOps`], so a Windows guest
/// gets `New-Item`, backslash paths and `bootstrap.ps1` while a macOS or
/// Kali guest gets exactly the `mkdir -p` / `bash bootstrap.sh` it always
/// did (spec.md §3 FR-4). Nothing is extracted or executed host-side — only
/// `copy_in` (scp) and one fixed bootstrap invocation happen here, and the
/// challenge files themselves are never run.
#[allow(clippy::too_many_arguments)]
async fn provision_guest(
    ctx: &AppContext,
    req: &LaunchRequest,
    profile: &Profile,
    challenge: &Challenge,
    endpoint: &GuestEndpoint,
    transport: &dyn GuestTransport,
    staging: &StagingDir,
    downloaded_files: &[DownloadedFile],
) -> Result<()> {
    let ops = GuestOps::for_endpoint(endpoint);
    let workspace_root = ops.trim_root(&profile.workspace.root);
    let workspace = ops.join(&workspace_root, &challenge.summary.slug);
    let bootstrap_dir = ops.join(&workspace_root, ".wg");
    let incoming_dir = ops.join(&workspace, "incoming");

    for command in ops.mkdir_p(&[bootstrap_dir.clone(), incoming_dir.clone()]) {
        let mkdir = command.run(transport, endpoint).await?;
        if mkdir.status != 0 {
            return Err(WargameError::CopyFailed(format!(
                "creating the guest workspace failed ({}): {}",
                command.describe(),
                mkdir.stderr
            )));
        }
    }

    let bootstrap_name = ops.bootstrap_name();
    let bootstrap_path = staging.path.join(bootstrap_name);
    std::fs::write(&bootstrap_path, ctx.bootstrap.for_shell(endpoint.shell))?;
    transport
        .copy_in(endpoint, &bootstrap_path, &ops.dir_dest(&bootstrap_dir))
        .await?;

    for file in downloaded_files {
        transport
            .copy_in(endpoint, &file.path, &ops.dir_dest(&incoming_dir))
            .await?;
    }

    // The digests the guest checks its copies against (spec.md §3 FR-4:
    // "게스트 파일 해시는 호스트 staging 해시와 같아야 한다"), in `sha256sum`
    // layout so neither bootstrap script has to parse JSON.
    let digest_list: String = downloaded_files
        .iter()
        .map(|file| format!("{}  {}\n", file.sha256, file.filename))
        .collect();
    let digest_path = staging.path.join(".wargame-sha256");
    std::fs::write(&digest_path, digest_list)?;
    transport
        .copy_in(endpoint, &digest_path, &ops.dir_dest(&incoming_dir))
        .await?;

    let mut download_sha256 = serde_json::Map::new();
    for file in downloaded_files {
        download_sha256.insert(
            file.filename.clone(),
            serde_json::Value::String(file.sha256.clone()),
        );
    }
    let metadata = serde_json::json!({
        "provider": challenge.summary.r#ref.provider,
        "challenge_id": challenge.summary.r#ref.id,
        "title": challenge.summary.title,
        "download_sha256": download_sha256,
    });
    let metadata_path = staging.path.join(".wargame.json");
    let metadata_bytes =
        serde_json::to_vec_pretty(&metadata).map_err(|e| WargameError::Other(e.to_string()))?;
    std::fs::write(&metadata_path, metadata_bytes)?;
    transport
        .copy_in(endpoint, &metadata_path, &ops.dir_dest(&incoming_dir))
        .await?;

    // `require_digests`: the host has just staged `.wargame-sha256`, so the
    // guest must refuse to carry on if it is not there rather than quietly
    // skipping verification (spec.md §3 FR-4).
    let bootstrap = ops.run_bootstrap(
        &ops.join(&bootstrap_dir, bootstrap_name),
        &workspace,
        req.extract,
        true,
    );
    let out = bootstrap.run(transport, endpoint).await?;
    if out.status != 0 {
        return Err(WargameError::GuestBootstrapFailed(format!(
            "bootstrap exited with status {}: {}",
            out.status, out.stderr
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::app::BuilderRegistry;
    use crate::app::{BackendRegistry, TransportRegistry};
    use crate::domain::{
        GuestArch, GuestOs, GuestSpec, ImageSpec, ProvisionSpec, ToolsSpec, VmSettings,
        WorkspaceSpec,
    };
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
    use crate::infra::HostDiskSpace;
    use crate::testing::fake_transport::TransportCall;
    use crate::testing::MemoryBuildRepo;
    use crate::testing::{
        FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
        FIXTURE_ASSET_BYTES,
    };
    use std::sync::Arc;

    fn tmp_path() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    fn test_profile() -> Profile {
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
                base: "base-image".to_string(),
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

    struct Fixture {
        ctx: AppContext,
        vm: Arc<FakeVmProvider>,
        wargame: Arc<FakeWargameProvider>,
        transport: Arc<FakeTransport>,
    }

    fn fixture() -> Fixture {
        let vm = Arc::new(FakeVmProvider::new());
        // The launch pipeline checks for the already-*built* profile image
        // (`wargame profile build`), not the raw base OS image.
        vm.add_image(test_profile().image_name());
        let wargame = Arc::new(FakeWargameProvider::new());
        let transport = Arc::new(FakeTransport::new());
        let profiles_dir = tmp_path();
        let ctx = AppContext {
            config: Default::default(),
            dirs: AppDirs { root: tmp_path() },
            wargame: wargame.clone(),
            backends: BackendRegistry::new(vec![vm.clone() as Arc<dyn VmProvider>]),
            transports: TransportRegistry::new(transport.clone()),
            repo: Arc::new(MemoryRepo::new()),
            builds: Arc::new(MemoryBuildRepo::new()),
            builders: BuilderRegistry::default(),
            disk: Arc::new(HostDiskSpace),
            profiles: ProfileStore::new(vec![test_profile()], profiles_dir),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap: crate::app::BootstrapScripts::new("#!/bin/bash\necho hi\n", "# ps\n"),
        };
        Fixture {
            ctx,
            vm,
            wargame,
            transport,
        }
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
    async fn happy_path_emits_events_in_order_and_reaches_ready() {
        let Fixture {
            ctx,
            wargame,
            transport,
            ..
        } = fixture();
        wargame.set_authenticated("tester");
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);

        let session = launch(&ctx, request(), tx).await.unwrap();
        assert_eq!(session.state, SessionState::Ready);
        assert_eq!(session.assets.len(), 1);

        let expected_sha256 = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(FIXTURE_ASSET_BYTES);
            hex::encode(hasher.finalize())
        };
        assert_eq!(session.assets[0].sha256, expected_sha256);

        // Persisted, not just returned.
        let loaded = ctx.repo.load(&session.id).await.unwrap();
        assert_eq!(loaded.state, SessionState::Ready);

        let mut saw_session_created = false;
        let mut saw_ready = false;
        let mut order = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match &event {
                LaunchEvent::SessionCreated { .. } => saw_session_created = true,
                LaunchEvent::Ready { .. } => saw_ready = true,
                _ => {}
            }
            order.push(event);
        }
        assert!(saw_session_created);
        assert!(saw_ready);
        // SessionCreated must precede Ready.
        let created_idx = order
            .iter()
            .position(|e| matches!(e, LaunchEvent::SessionCreated { .. }))
            .unwrap();
        let ready_idx = order
            .iter()
            .position(|e| matches!(e, LaunchEvent::Ready { .. }))
            .unwrap();
        assert!(created_idx < ready_idx);

        // The asset and the bootstrap script were both copied in, and the
        // bootstrap was executed with the requested --extract flag.
        let calls = transport.calls();
        let copy_in_dests: Vec<String> = calls
            .iter()
            .filter_map(|c| match c {
                TransportCall::CopyIn { dest, .. } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        assert!(copy_in_dests.iter().any(|d| d.contains(".wg")));
        assert!(copy_in_dests.iter().any(|d| d.contains("incoming")));

        let exec_argvs: Vec<Vec<String>> = calls
            .iter()
            .filter_map(|c| match c {
                TransportCall::Exec { argv, .. } => Some(argv.clone()),
                _ => None,
            })
            .collect();
        assert!(exec_argvs
            .iter()
            .any(|argv| argv.iter().any(|a| a == "--extract=true")));

        // The per-launch staging directory (created under cache/staging/)
        // must not survive the launch.
        let staging_root = ctx.dirs.cache_dir().join("staging");
        let remaining = std::fs::read_dir(&staging_root)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn image_missing_creates_no_session() {
        let Fixture { ctx, .. } = fixture();
        // Don't add "base-image" this time.
        let ctx = AppContext {
            backends: BackendRegistry::new(vec![
                Arc::new(FakeVmProvider::new()) as Arc<dyn crate::ports::VmProvider>
            ]),
            ..ctx
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = launch(&ctx, request(), tx).await.unwrap_err();
        assert!(matches!(err, WargameError::ImageMissing { .. }));
        assert!(ctx.repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn compat_rejection_creates_no_session() {
        let Fixture { ctx, .. } = fixture();
        let mut incompatible = test_profile();
        incompatible.guest.os = GuestOs::Windows;
        incompatible.guest.arch = GuestArch::X86_64;
        let ctx = AppContext {
            profiles: ProfileStore::new(vec![incompatible], tmp_path()),
            ..ctx
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let err = launch(&ctx, request(), tx).await.unwrap_err();
        assert!(matches!(err, WargameError::Incompatible(_)));
        assert!(ctx.repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn vm_create_failure_fails_session_without_vm_ref() {
        let Fixture {
            ctx, vm, wargame, ..
        } = fixture();
        wargame.set_authenticated("tester");
        vm.set_fail_on("create");
        let (tx, _rx) = tokio::sync::mpsc::channel(64);

        let err = launch(&ctx, request(), tx).await.unwrap_err();
        assert!(matches!(err, WargameError::VmCreationFailed(_)));

        let sessions = ctx.repo.list().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Failed);
        assert!(sessions[0].vm_ref.is_none());
        assert!(sessions[0].last_error.is_some());
    }

    #[tokio::test]
    async fn boot_timeout_fails_session_with_vm_ref_set() {
        let Fixture {
            ctx, vm, wargame, ..
        } = fixture();
        wargame.set_authenticated("tester");
        vm.set_fail_on("wait_ip");
        let (tx, _rx) = tokio::sync::mpsc::channel(64);

        let err = launch(&ctx, request(), tx).await.unwrap_err();
        assert!(matches!(err, WargameError::GuestBootTimeout(_)));

        let sessions = ctx.repo.list().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Failed);
        assert!(
            sessions[0].vm_ref.is_some(),
            "vm_ref must survive so `wargame delete` can clean up the VM"
        );
    }

    #[tokio::test]
    async fn isolated_network_unavailable_fails_before_any_session() {
        let Fixture {
            ctx, vm, wargame, ..
        } = fixture();
        wargame.set_authenticated("tester");
        vm.set_isolated_network(Some(false));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);

        let err = launch(&ctx, request(), tx).await.unwrap_err();
        assert!(matches!(err, WargameError::NetworkUnavailable(_)));
        assert!(ctx.repo.list().await.unwrap().is_empty());
        assert!(
            vm.calls().iter().all(|c| c != "create"),
            "no VM may be created when isolated networking is unavailable"
        );
    }

    #[tokio::test]
    async fn nat_network_proceeds_even_when_isolated_is_unavailable() {
        let Fixture {
            ctx, vm, wargame, ..
        } = fixture();
        wargame.set_authenticated("tester");
        vm.set_isolated_network(Some(false));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);

        let mut req = request();
        req.network = NetworkMode::Nat;
        let session = launch(&ctx, req, tx).await.unwrap();
        assert_eq!(session.state, SessionState::Ready);
    }

    #[tokio::test]
    async fn isolated_network_unknown_proceeds() {
        let Fixture {
            ctx, wargame, vm, ..
        } = fixture();
        wargame.set_authenticated("tester");
        vm.set_isolated_network(None);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);

        let session = launch(&ctx, request(), tx).await.unwrap();
        assert_eq!(session.state, SessionState::Ready);
    }

    #[tokio::test]
    async fn anonymous_provider_with_assets_requires_auth_before_any_vm() {
        let Fixture { ctx, vm, .. } = fixture();
        // wargame provider stays Anonymous (default).
        let (tx, _rx) = tokio::sync::mpsc::channel(64);

        let err = launch(&ctx, request(), tx).await.unwrap_err();
        assert!(matches!(err, WargameError::AuthenticationRequired { .. }));
        assert!(ctx.repo.list().await.unwrap().is_empty());
        assert!(
            vm.calls().iter().all(|c| c != "create"),
            "no VM may be created before authentication is confirmed"
        );
    }
}
