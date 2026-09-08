//! The composition root (DESIGN.md §7): turns global CLI flags and a
//! handful of environment variables into one [`AppContext`] that every
//! subcommand runs its application-service call through.
//!
//! `$WARGAME_PROVIDER=fixture` swaps in [`FixtureProvider`] (no network, no
//! Dreamhack account) and `$WARGAME_VM=fake` swaps in
//! `wargame_core::testing`'s `FakeVmProvider`/`FakeTransport` (no Tart, no
//! real VM) -- this is what lets `crates/wargame-cli/tests/cli.rs` and
//! `scripts/e2e-smoke.sh` exercise the whole CLI without either dependency.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use url::Url;

use wargame_core::app::{
    AppContext, BackendRegistry, BootstrapScripts, BuilderRegistry, ProfileStore, TransportRegistry,
};
use wargame_core::config::Config;
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::{Cmd, ProcessRunner, RealRunner};
use wargame_core::infra::{
    FileStore, HostDiskSpace, JsonBuildRepository, JsonSessionRepository, KeychainStore,
    SshGuestTransport,
};
use wargame_core::ports::{
    BuildRecordRepository, CredentialStore, DiskSpace, GuestTransport, ProfileImageBuilder,
    SessionRepository, VmProvider, WargameProvider,
};
use wargame_core::security::init_tracing;
use wargame_core::testing::{FakeTransport, FakeVmProvider};

use wargame_dreamhack::{DreamhackConfig, DreamhackProvider};
use wargame_tart::{TartConfig, TartImageBuilder, TartProvider, TransportFactory};
use wargame_utm::{RecordImageCatalog, UtmConfig, UtmImageBuilder, UtmProvider};

use crate::fixture_provider::FixtureProvider;
use crate::profiles_builtin;

/// Writes one built-in file into `$WARGAME_HOME/profiles/<rel>`, keeping
/// whatever the user did to it (spec.md §3 FR-3: "사용자 편집 프로필·사용자
/// 스크립트를 내장 파일 배포 과정에서 덮어쓰지 않는다").
///
/// A copy the user has edited is theirs and is never touched. A copy that
/// still equals what the previous binary seeded (kept as a pristine copy
/// under `profiles/.builtin/`) is replaced, so an unedited file follows the
/// binary instead of being frozen at whatever version first ran. A file the
/// built-in set does not name at all -- anything the user added under their
/// own name -- is never written here, so it cannot be touched either.
fn seed_builtin_file(dirs: &AppDirs, rel: &str, content: &str) -> Result<()> {
    let dest = dirs.profiles_dir().join(rel);
    let pristine = dirs.profiles_dir().join(".builtin").join(rel);
    let current = std::fs::read_to_string(&dest).ok();
    let last_seeded = std::fs::read_to_string(&pristine).ok();

    let unedited = current.is_none() || current == last_seeded;
    if unedited && current.as_deref() != Some(content) {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, content)?;
    }
    if last_seeded.as_deref() != Some(content) {
        if let Some(parent) = pristine.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&pristine, content)?;
    }
    Ok(())
}

/// Seeds the built-in profile YAMLs, which [`ProfileStore`] then treats as
/// user overrides of the embedded set.
fn seed_builtin_profile_yamls(dirs: &AppDirs) -> Result<()> {
    for (filename, content) in profiles_builtin::builtin_profile_yamls() {
        seed_builtin_file(dirs, filename, content)?;
    }
    Ok(())
}

/// Seeds the built-in provisioning scripts, i.e. the paths the built-in
/// profiles' `provision.scripts` entries point at, plus the commented
/// `scripts/custom/` examples.
fn seed_builtin_scripts(dirs: &AppDirs) -> Result<()> {
    for (rel, content) in profiles_builtin::builtin_provision_scripts() {
        seed_builtin_file(dirs, rel, content)?;
    }
    Ok(())
}

/// Builds the one [`AppContext`] a CLI invocation runs through: resolves
/// `$WARGAME_HOME` (or `--home`), makes sure its subdirectories and the
/// built-in profile YAMLs exist, loads config, and wires a provider/VM
/// backend/transport pair per `$WARGAME_PROVIDER`/`$WARGAME_VM`.
pub fn build_context(
    verbose: bool,
    config_override: Option<PathBuf>,
    home_override: Option<PathBuf>,
) -> Result<AppContext> {
    let dirs = match home_override {
        Some(root) => AppDirs { root },
        None => AppDirs::resolve()?,
    };
    dirs.ensure_layout().map_err(|e| {
        WargameError::Config(format!(
            "cannot create the wargame home directory {}: {e} (set --home or $WARGAME_HOME to a writable location)",
            dirs.root.display()
        ))
    })?;

    seed_builtin_profile_yamls(&dirs)?;
    seed_builtin_scripts(&dirs)?;

    let config_path = config_override.unwrap_or_else(|| dirs.config_file());
    let config = Config::load(Some(&config_path))?;

    init_tracing(verbose, Some(&dirs.logs_dir().join("wargame.log")));

    let runner: Arc<dyn ProcessRunner> = Arc::new(RealRunner);
    let builtin_profiles = profiles_builtin::builtin_profiles()?;

    let wargame = build_wargame_provider(&config, &dirs, &runner)?;
    let builds: Arc<dyn BuildRecordRepository> =
        Arc::new(JsonBuildRepository::new(dirs.builds_dir()));
    // One adapter per backend, shared by the registry a launch picks from
    // and the builders `profile build` runs through -- they are the same
    // VMs, and a second `UtmProvider` would keep its own idea of things like
    // which run log a VM was started with.
    let providers = RealProviders::new(&config, &runner, &builds);
    let (backends, transports) = build_vm_backends(&dirs, &runner, &builtin_profiles, &providers);
    let builders = build_image_builders(&dirs, &runner, &providers);

    let repo: Arc<dyn SessionRepository> =
        Arc::new(JsonSessionRepository::new(dirs.sessions_dir()));
    let profiles = ProfileStore::new(builtin_profiles, dirs.profiles_dir())
        .with_builtin_yaml(profiles_builtin::builtin_profile_sources()?);

    Ok(AppContext {
        config,
        dirs,
        wargame,
        backends,
        transports,
        repo,
        builds,
        builders,
        disk: Arc::new(HostDiskSpace) as Arc<dyn DiskSpace>,
        profiles,
        runner,
        bootstrap: BootstrapScripts::new(
            profiles_builtin::BOOTSTRAP_SH,
            profiles_builtin::BOOTSTRAP_PS1,
        ),
    })
}

/// `$WARGAME_PROVIDER=fixture` -> [`FixtureProvider`]; anything else ->
/// [`DreamhackProvider`], backed by whichever [`CredentialStore`]
/// `config.wargame.dreamhack.credential_store` names.
fn build_wargame_provider(
    config: &Config,
    dirs: &AppDirs,
    runner: &Arc<dyn ProcessRunner>,
) -> Result<Arc<dyn WargameProvider>> {
    if std::env::var("WARGAME_PROVIDER").as_deref() == Ok("fixture") {
        return Ok(Arc::new(FixtureProvider::new()));
    }

    let store = build_credential_store(config, dirs, runner);
    let base_url = Url::parse(&config.wargame.dreamhack.base_url)
        .map_err(|e| WargameError::Config(format!("invalid [wargame.dreamhack] base_url: {e}")))?;
    let dh_config = DreamhackConfig {
        base_url,
        user_agent: format!("wargame-vm/{}", env!("CARGO_PKG_VERSION")),
    };
    Ok(Arc::new(DreamhackProvider::new(dh_config, store)?))
}

/// `"keychain"` (default) -> the macOS Keychain via `security`; `"file"` ->
/// a plaintext JSON file under `$WARGAME_HOME` (tests/CI only; warns).
pub fn build_credential_store(
    config: &Config,
    dirs: &AppDirs,
    runner: &Arc<dyn ProcessRunner>,
) -> Arc<dyn CredentialStore> {
    match config.wargame.dreamhack.credential_store.as_str() {
        "file" => Arc::new(FileStore::new(dirs.root.join("credentials.json"))),
        _ => Arc::new(KeychainStore::new(runner.clone())),
    }
}

/// Builds the backend registry and the guest transports (spec.md §5 item 1).
///
/// `$WARGAME_VM=fake` -> in-memory fakes registered under **both** backend
/// names, with every built-in profile's image pre-registered so `wargame
/// launch` needs no prior `profile build`; anything else -> the real Tart
/// and UTM backends.
///
/// Registering UTM costs nothing on a host without it: `UtmProvider` only
/// looks for `utmctl` when it is asked to do something, so a missing UTM
/// degrades the `utm` slot alone and every Tart/macOS path is untouched
/// (spec.md §3 FR-6: "Tart macOS 실행에 UTM 설치를 요구하지 않는다").
///
/// The SSH transport carries no guest user: it comes from
/// `profile.guest.user` at launch and from the session's saved connection
/// afterwards.
/// The real VM backends, built once and shared (spec.md §5 item 2).
struct RealProviders {
    tart: Arc<TartProvider>,
    utm: Arc<UtmProvider>,
}

impl RealProviders {
    fn new(
        config: &Config,
        runner: &Arc<dyn ProcessRunner>,
        builds: &Arc<dyn BuildRecordRepository>,
    ) -> Self {
        RealProviders {
            tart: Arc::new(TartProvider::new(runner.clone(), tart_config(config))),
            // The build records are what tells UTM which images exist:
            // without them every UTM profile would report "not ready"
            // forever (spec.md §5 item 5).
            utm: Arc::new(
                UtmProvider::new(runner.clone(), utm_config(config))
                    .with_image_catalog(Arc::new(RecordImageCatalog::new(builds.clone()))),
            ),
        }
    }
}

fn build_vm_backends(
    dirs: &AppDirs,
    runner: &Arc<dyn ProcessRunner>,
    builtin_profiles: &[wargame_core::domain::Profile],
    providers: &RealProviders,
) -> (BackendRegistry, TransportRegistry) {
    if std::env::var("WARGAME_VM").as_deref() == Ok("fake") {
        let fake_tart = Arc::new(FakeVmProvider::new());
        let fake_utm = Arc::new(FakeVmProvider::with_backend("utm"));
        fake_utm.set_capabilities(utm_capabilities());
        for profile in builtin_profiles {
            fake_tart.add_image(profile.image_name());
            fake_utm.add_image(profile.image_name());
        }
        let fake_transport = Arc::new(FakeTransport::new()) as Arc<dyn GuestTransport>;
        return (
            BackendRegistry::new(vec![
                fake_tart as Arc<dyn VmProvider>,
                fake_utm as Arc<dyn VmProvider>,
            ]),
            TransportRegistry::new(fake_transport.clone()).with_powershell(fake_transport),
        );
    }

    // One transport serves every guest: it phrases each command for the
    // endpoint's own shell, so the PowerShell slot is the same adapter
    // (spec.md §3 FR-4).
    let transport = Arc::new(SshGuestTransport::new(
        runner.clone(),
        dirs.keys_dir().join("id_ed25519"),
    )) as Arc<dyn GuestTransport>;
    (
        BackendRegistry::new(vec![
            providers.tart.clone() as Arc<dyn VmProvider>,
            providers.utm.clone() as Arc<dyn VmProvider>,
        ]),
        TransportRegistry::new(transport.clone()).with_powershell(transport),
    )
}

/// The per-backend image builders `wargame profile create|build` runs
/// through (spec.md §5 item 4).
///
/// Always the real ones, even under `$WARGAME_VM=fake`: a builder touches
/// `tart`/`utmctl` only when a build step actually runs, and wiring the real
/// ones keeps the "this backend cannot do that" errors (a `profile create` on
/// a Tart profile, say) exact instead of turning them into "no builder is
/// wired".
fn build_image_builders(
    dirs: &AppDirs,
    runner: &Arc<dyn ProcessRunner>,
    providers: &RealProviders,
) -> BuilderRegistry {
    let key_path = dirs.keys_dir().join("id_ed25519");
    let tart_runner = runner.clone();
    let tart_key = key_path.clone();
    let transport_factory: TransportFactory = Box::new(move || {
        Arc::new(SshGuestTransport::new(
            tart_runner.clone(),
            tart_key.clone(),
        )) as Arc<dyn GuestTransport>
    });
    let tart_builder = TartImageBuilder::new(
        runner.clone(),
        providers.tart.clone(),
        transport_factory,
        dirs.keys_dir().join("id_ed25519.pub"),
    );

    let utm_builder = UtmImageBuilder::new(
        providers.utm.clone(),
        Arc::new(SshGuestTransport::new(runner.clone(), key_path)) as Arc<dyn GuestTransport>,
    );

    BuilderRegistry::new(vec![
        Arc::new(tart_builder) as Arc<dyn ProfileImageBuilder>,
        Arc::new(utm_builder) as Arc<dyn ProfileImageBuilder>,
    ])
}

/// What the real UTM backend reports, mirrored onto the fake so
/// `WARGAME_VM=fake` can exercise a UTM profile end to end.
fn utm_capabilities() -> wargame_core::domain::ProviderCapabilities {
    UtmProvider::new(Arc::new(RealRunner), UtmConfig::default()).capabilities()
}

fn utm_config(config: &Config) -> UtmConfig {
    UtmConfig {
        utmctl: config.vm.utm.utmctl.clone(),
        vm_prefix: config.vm.utm.vm_prefix.clone(),
        build_prefix: config.vm.utm.build_prefix.clone(),
        boot_timeout: Duration::from_secs(config.vm.utm.boot_timeout_secs),
        virtualize_boot_timeout: Duration::from_secs(config.vm.utm.virtualize_boot_timeout_secs),
        ..UtmConfig::default()
    }
}

fn tart_config(config: &Config) -> TartConfig {
    TartConfig {
        binary: config.vm.tart.binary.clone(),
        vm_prefix: config.vm.tart.vm_prefix.clone(),
        boot_timeout: Duration::from_secs(config.vm.tart.boot_timeout_secs),
        ..TartConfig::default()
    }
}

/// Makes sure the host's ed25519 guest key exists (DESIGN.md §3.6), which
/// is what a Tart build installs into the guest and what every UTM guest is
/// told to authorize. Generated on first use, never overwritten.
pub async fn ensure_host_key(ctx: &AppContext) -> Result<PathBuf> {
    let key_path = ctx.dirs.keys_dir().join("id_ed25519");
    let pubkey_path = ctx.dirs.keys_dir().join("id_ed25519.pub");
    if pubkey_path.exists() {
        return Ok(pubkey_path);
    }
    std::fs::create_dir_all(ctx.dirs.keys_dir())?;
    let cmd = Cmd::new(
        "ssh-keygen",
        vec![
            "-t".to_string(),
            "ed25519".to_string(),
            "-N".to_string(),
            String::new(),
            "-f".to_string(),
            key_path.display().to_string(),
            "-C".to_string(),
            "wargame-vm".to_string(),
        ],
    );
    let out = ctx.runner.run(&cmd).await?;
    if out.status != 0 {
        return Err(WargameError::Other(format!(
            "ssh-keygen failed: {}",
            out.stderr
        )));
    }
    Ok(pubkey_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tmp_home() -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    // Both tests below mutate the same process-wide `WARGAME_PROVIDER`/
    // `WARGAME_VM` env vars and unset them again once `build_context`
    // returns. Without serializing that, cargo test's default
    // multi-threaded harness can interleave them: one test's teardown can
    // remove a var while the other is still inside `build_context`, which
    // then falls through to the real Dreamhack/Keychain path instead of the
    // fixture/fake one and fails (surfaced in `nix build`'s sandboxed
    // checkPhase, whose PATH has no `/usr/bin/security` for `KeychainStore`
    // to find). This lock makes the env-var-and-`build_context` section a
    // single critical section instead.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn build_context_with_fixture_and_fake_env_needs_no_external_tools() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("WARGAME_PROVIDER", "fixture");
        std::env::set_var("WARGAME_VM", "fake");
        let ctx = build_context(false, None, Some(tmp_home())).unwrap();
        assert_eq!(ctx.wargame.name(), "fixture");
        assert_eq!(ctx.backends.names(), vec!["tart", "utm"]);
        assert_eq!(ctx.backends.get("tart").unwrap().name(), "tart");
        assert_eq!(ctx.backends.get("utm").unwrap().name(), "utm");
        assert_eq!(
            ctx.profiles.list().len(),
            5,
            "two macOS profiles plus the Kali/Windows templates"
        );
        assert_eq!(ctx.builders.names(), vec!["tart", "utm"]);
        std::env::remove_var("WARGAME_PROVIDER");
        std::env::remove_var("WARGAME_VM");
    }

    #[test]
    fn build_context_copies_builtin_profile_yamls_into_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("WARGAME_PROVIDER", "fixture");
        std::env::set_var("WARGAME_VM", "fake");
        let home = tmp_home();
        let _ctx = build_context(false, None, Some(home.clone())).unwrap();
        assert!(home.join("profiles/macos-minimal-arm64.yaml").is_file());
        assert!(home.join("profiles/macos-re-arm64.yaml").is_file());
        assert!(home.join("profiles/kali-arm64.yaml").is_file());
        assert!(home.join("profiles/kali-x86_64.yaml").is_file());
        assert!(home.join("profiles/windows-x86_64.yaml").is_file());
        assert!(home.join("profiles/scripts/kali/common.sh").is_file());
        assert!(home.join("profiles/scripts/windows/common.ps1").is_file());
        std::env::remove_var("WARGAME_PROVIDER");
        std::env::remove_var("WARGAME_VM");
    }

    #[test]
    fn unedited_builtin_yaml_follows_the_binary_but_an_edited_one_is_kept() {
        let home = tmp_home();
        let dirs = AppDirs { root: home.clone() };
        dirs.ensure_layout().unwrap();
        let yaml = home.join("profiles/macos-re-arm64.yaml");
        let (_, current) = profiles_builtin::builtin_profile_yamls()[1];

        // Simulate an older binary having seeded a different version.
        std::fs::write(&yaml, "old: version\n").unwrap();
        std::fs::create_dir_all(home.join("profiles/.builtin")).unwrap();
        std::fs::write(
            home.join("profiles/.builtin/macos-re-arm64.yaml"),
            "old: version\n",
        )
        .unwrap();
        seed_builtin_profile_yamls(&dirs).unwrap();
        assert_eq!(std::fs::read_to_string(&yaml).unwrap(), current);

        // The user edits it: the edit survives later runs.
        let edited = format!("{current}# edited by user\n");
        std::fs::write(&yaml, &edited).unwrap();
        seed_builtin_profile_yamls(&dirs).unwrap();
        assert_eq!(std::fs::read_to_string(&yaml).unwrap(), edited);
    }

    /// spec.md §3 FR-3: "사용자 편집 프로필·사용자 스크립트를 내장 파일
    /// 배포 과정에서 덮어쓰지 않는다" -- the same rule the YAMLs follow, for
    /// the scripts. An unedited copy still follows the binary, so a stale
    /// provisioning recipe is not frozen in place.
    #[test]
    fn an_edited_builtin_script_is_kept_but_an_unedited_one_follows_the_binary() {
        let home = tmp_home();
        let dirs = AppDirs { root: home.clone() };
        dirs.ensure_layout().unwrap();
        let rel = "scripts/kali/common.sh";
        let script = home.join("profiles").join(rel);
        let current = profiles_builtin::builtin_provision_scripts()
            .into_iter()
            .find(|(path, _)| *path == rel)
            .map(|(_, content)| content)
            .unwrap();

        // An older binary seeded a different version and the user left it
        // alone: the new one replaces it.
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "#!/usr/bin/env bash\necho old\n").unwrap();
        std::fs::create_dir_all(home.join("profiles/.builtin/scripts/kali")).unwrap();
        std::fs::write(
            home.join("profiles/.builtin").join(rel),
            "#!/usr/bin/env bash\necho old\n",
        )
        .unwrap();
        seed_builtin_scripts(&dirs).unwrap();
        assert_eq!(std::fs::read_to_string(&script).unwrap(), current);

        // The user edits it: the edit survives every later run.
        let edited = format!("{current}echo 'my own step'\n");
        std::fs::write(&script, &edited).unwrap();
        seed_builtin_scripts(&dirs).unwrap();
        seed_builtin_scripts(&dirs).unwrap();
        assert_eq!(std::fs::read_to_string(&script).unwrap(), edited);
    }

    /// A script the user added under a name of their own is not part of the
    /// built-in set at all, so seeding neither writes nor removes it.
    #[test]
    fn seeding_never_touches_a_users_own_script() {
        let home = tmp_home();
        let dirs = AppDirs { root: home.clone() };
        dirs.ensure_layout().unwrap();
        let mine = home.join("profiles/scripts/custom/my-tools.sh");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, "#!/usr/bin/env bash\necho mine\n").unwrap();

        seed_builtin_scripts(&dirs).unwrap();
        seed_builtin_scripts(&dirs).unwrap();

        assert_eq!(
            std::fs::read_to_string(&mine).unwrap(),
            "#!/usr/bin/env bash\necho mine\n"
        );
    }

    #[test]
    fn build_context_seeds_and_refreshes_builtin_scripts_in_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("WARGAME_PROVIDER", "fixture");
        std::env::set_var("WARGAME_VM", "fake");
        let home = tmp_home();
        let script = home.join("profiles/scripts/macos/reversing.sh");
        let _ctx = build_context(false, None, Some(home.clone())).unwrap();
        assert!(home.join("profiles/scripts/macos/common.sh").is_file());
        assert!(script.is_file());
        // A stale copy (e.g. from an older binary, so the pristine record
        // matches it) is replaced, not kept.
        std::fs::write(&script, "#!/usr/bin/env bash\necho stale\n").unwrap();
        std::fs::write(
            home.join("profiles/.builtin/scripts/macos/reversing.sh"),
            "#!/usr/bin/env bash\necho stale\n",
        )
        .unwrap();
        let _ctx = build_context(false, None, Some(home.clone())).unwrap();
        assert!(!std::fs::read_to_string(&script)
            .unwrap()
            .contains("echo stale"));
        std::env::remove_var("WARGAME_PROVIDER");
        std::env::remove_var("WARGAME_VM");
    }
}
