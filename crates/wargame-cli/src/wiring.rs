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

use wargame_core::app::{AppContext, ProfileStore};
use wargame_core::config::Config;
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::fs::AppDirs;
use wargame_core::infra::process::{Cmd, ProcessRunner, RealRunner};
use wargame_core::infra::{FileStore, JsonSessionRepository, KeychainStore, SshGuestTransport};
use wargame_core::ports::{
    CredentialStore, GuestTransport, SessionRepository, VmProvider, WargameProvider,
};
use wargame_core::security::init_tracing;
use wargame_core::testing::{FakeTransport, FakeVmProvider};

use wargame_dreamhack::{DreamhackConfig, DreamhackProvider};
use wargame_tart::{BuildOptions, ProfileImageBuilder, TartConfig, TartProvider, TransportFactory};

use crate::fixture_provider::FixtureProvider;
use crate::profiles_builtin;

/// The guest OS user every built-in macOS profile provisions (both
/// `profiles/*.yaml` files use `guest.user: admin`); the everyday
/// (post-build) SSH transport wired here is fixed to it rather than
/// threaded through per-launch, since the MVP ships exactly this one guest
/// user (DESIGN.md §6).
const DEFAULT_GUEST_USER: &str = "admin";

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
    dirs.ensure_layout()?;

    for (filename, content) in profiles_builtin::builtin_profile_yamls() {
        let dest = dirs.profiles_dir().join(filename);
        if !dest.exists() {
            std::fs::write(&dest, content)?;
        }
    }
    // Unlike the YAMLs, the built-in scripts are overwritten every run: they
    // are part of the tool, not user configuration, and a stale copy would
    // silently build images with an older provisioning recipe. Users add
    // their own scripts under other names.
    for (rel, content) in profiles_builtin::builtin_provision_scripts() {
        let dest = dirs.profiles_dir().join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&dest).ok().as_deref() != Some(content) {
            std::fs::write(&dest, content)?;
        }
    }

    let config_path = config_override.unwrap_or_else(|| dirs.config_file());
    let config = Config::load(Some(&config_path))?;

    init_tracing(verbose, Some(&dirs.logs_dir().join("wargame.log")));

    let runner: Arc<dyn ProcessRunner> = Arc::new(RealRunner);
    let builtin_profiles = profiles_builtin::builtin_profiles()?;

    let wargame = build_wargame_provider(&config, &dirs, &runner)?;
    let (vm, transport) = build_vm_backend(&config, &dirs, &runner, &builtin_profiles);

    let repo: Arc<dyn SessionRepository> =
        Arc::new(JsonSessionRepository::new(dirs.sessions_dir()));
    let profiles = ProfileStore::new(builtin_profiles, dirs.profiles_dir());

    Ok(AppContext {
        config,
        dirs,
        wargame,
        vm,
        transport,
        repo,
        profiles,
        runner,
        bootstrap_script: profiles_builtin::BOOTSTRAP_SH,
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

/// `$WARGAME_VM=fake` -> in-memory fakes (every built-in profile's image is
/// pre-registered so `wargame launch` needs no prior `profile build`);
/// anything else -> the real Tart backend over SSH.
fn build_vm_backend(
    config: &Config,
    dirs: &AppDirs,
    runner: &Arc<dyn ProcessRunner>,
    builtin_profiles: &[wargame_core::domain::Profile],
) -> (Arc<dyn VmProvider>, Arc<dyn GuestTransport>) {
    if std::env::var("WARGAME_VM").as_deref() == Ok("fake") {
        let fake_vm = Arc::new(FakeVmProvider::new());
        for profile in builtin_profiles {
            fake_vm.add_image(profile.image_name());
        }
        return (
            fake_vm as Arc<dyn VmProvider>,
            Arc::new(FakeTransport::new()) as Arc<dyn GuestTransport>,
        );
    }

    let tart = Arc::new(TartProvider::new(runner.clone(), tart_config(config)));
    let key_path = dirs.keys_dir().join("id_ed25519");
    let transport = Arc::new(SshGuestTransport::new(
        runner.clone(),
        DEFAULT_GUEST_USER,
        key_path,
    ));
    (
        tart as Arc<dyn VmProvider>,
        transport as Arc<dyn GuestTransport>,
    )
}

fn tart_config(config: &Config) -> TartConfig {
    TartConfig {
        binary: config.vm.tart.binary.clone(),
        vm_prefix: config.vm.tart.vm_prefix.clone(),
        boot_timeout: Duration::from_secs(config.vm.tart.boot_timeout_secs),
        ..TartConfig::default()
    }
}

/// Builds the (real-Tart-only) [`ProfileImageBuilder`] and its
/// [`BuildOptions`] for `wargame profile build` (DESIGN.md §5/§6): generates
/// the host's ed25519 guest key on first use (DESIGN.md §3.6) and locates
/// the provisioning `scripts/` tree.
pub async fn build_profile_image_builder(
    ctx: &AppContext,
) -> Result<(ProfileImageBuilder, BuildOptions)> {
    let key_path = ctx.dirs.keys_dir().join("id_ed25519");
    let pubkey_path = ctx.dirs.keys_dir().join("id_ed25519.pub");
    if !key_path.exists() {
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
    }
    let pubkey = std::fs::read_to_string(&pubkey_path)?.trim().to_string();

    let tart = Arc::new(TartProvider::new(
        ctx.runner.clone(),
        tart_config(&ctx.config),
    ));
    let runner = ctx.runner.clone();
    let key_path_for_factory = key_path.clone();
    let transport_factory: TransportFactory = Box::new(move |user: &str| {
        Arc::new(SshGuestTransport::new(
            runner.clone(),
            user.to_string(),
            key_path_for_factory.clone(),
        )) as Arc<dyn GuestTransport>
    });
    let builder = ProfileImageBuilder::new(ctx.runner.clone(), tart, transport_factory);

    let opts = BuildOptions {
        // `$WARGAME_HOME/profiles`, which `build_context` has just seeded
        // with the embedded `scripts/` tree; never a path from the build
        // machine (a `nix build` binary's `CARGO_MANIFEST_DIR` is a sandbox
        // path that does not exist at runtime).
        scripts_dir: ctx.dirs.profiles_dir(),
        pubkey,
        log_path: ctx.dirs.logs_dir().join("profile-build.log"),
    };
    Ok((builder, opts))
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
        assert_eq!(ctx.vm.name(), "tart");
        assert_eq!(ctx.profiles.list().len(), 2);
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
        std::env::remove_var("WARGAME_PROVIDER");
        std::env::remove_var("WARGAME_VM");
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
        // A stale copy (e.g. from an older binary) is replaced, not kept.
        std::fs::write(&script, "#!/usr/bin/env bash\necho stale\n").unwrap();
        let _ctx = build_context(false, None, Some(home.clone())).unwrap();
        assert!(!std::fs::read_to_string(&script)
            .unwrap()
            .contains("echo stale"));
        std::env::remove_var("WARGAME_PROVIDER");
        std::env::remove_var("WARGAME_VM");
    }
}
