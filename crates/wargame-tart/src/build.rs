//! The Tart half of the common image build (`wargame_core::app::profile_build`).
//!
//! Every step that is not Tart-specific -- copying and running the profile's
//! provisioning scripts, the `tools.verify` commands, the provisioning log,
//! the build record -- lives in core. What is left here is exactly what only
//! Tart can do: make sure the base image is local, clone and size a VM from
//! it, boot it headless on NAT, install the host's SSH public key over a
//! one-time password login (`sshpass`, secret only ever in the `SSHPASS` env
//! var -- never argv, never logged), stop the VM and confirm it really
//! stopped.
//!
//! Tart's "publish" is a no-op by construction: the built VM *is* the image
//! (`wargame-profile-<name>`), which is why `prepare` clears a previous build
//! of the same name before cloning.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use wargame_core::domain::build::PublishedImage;
use wargame_core::domain::{GuestEndpoint, GuestShell, NetworkMode, Profile, VmRef};
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::process::{Cmd, ProcessRunner};
use wargame_core::ports::image_builder::{BuildRequest, GuestLink, Progress};
use wargame_core::ports::{
    GuestTransport, ImageRef, ProfileImageBuilder, StartOptions, VmProvider, VmSpec, VmStatus,
};

use crate::TartProvider;

/// How often the password-based SSH probe (installing the host's public key)
/// is retried while the guest's `sshd` finishes coming up.
const KEY_INSTALL_RETRY: Duration = Duration::from_secs(5);
/// Total time budget for that probe before giving up.
const KEY_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// How often, after `tart stop` returns, the build polls the VM's status to
/// confirm it actually stopped -- a bulk Homebrew install can leave hundreds
/// of MB of dirty pages that don't finish flushing within `tart stop`'s own
/// (now-extended) grace period, in which case Tart force-kills the guest and
/// the late writes are lost (DESIGN.md §5).
const STOP_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Total time budget for that confirmation poll before giving up.
const STOP_POLL_TIMEOUT: Duration = Duration::from_secs(60);

/// Builds a [`GuestTransport`], deferred until after the host's SSH key has
/// actually been installed (see [`TartImageBuilder::connect`]).
/// The guest user is no longer part of the transport -- it travels in the
/// [`GuestEndpoint`] each call is addressed with (spec.md §3 FR-4).
pub type TransportFactory = Box<dyn Fn() -> Arc<dyn GuestTransport> + Send + Sync>;

pub struct TartImageBuilder {
    runner: Arc<dyn ProcessRunner>,
    tart: Arc<TartProvider>,
    transport_factory: TransportFactory,
    /// Where the host's ed25519 public key lives
    /// (`$WARGAME_HOME/keys/id_ed25519.pub`). It is read when a build
    /// actually connects, not when the builder is wired, so a key generated
    /// between the two is still picked up.
    pubkey_path: PathBuf,
}

impl TartImageBuilder {
    pub fn new(
        runner: Arc<dyn ProcessRunner>,
        tart: Arc<TartProvider>,
        transport_factory: TransportFactory,
        pubkey_path: PathBuf,
    ) -> Self {
        TartImageBuilder {
            runner,
            tart,
            transport_factory,
            pubkey_path,
        }
    }

    /// The host's public key, installed into the guest's `authorized_keys`
    /// so future sessions never need the build-time password again.
    fn pubkey(&self) -> Result<String> {
        std::fs::read_to_string(&self.pubkey_path)
            .map(|key| key.trim().to_string())
            .map_err(|e| {
                WargameError::Other(format!(
                    "cannot read the host's SSH public key {}: {e}",
                    self.pubkey_path.display()
                ))
            })
    }

    /// Installs `pubkey` into the guest's `authorized_keys` over a
    /// password-authenticated SSH connection (`sshpass`), retrying while the
    /// guest's `sshd` finishes starting. `SSHPASS` carries the secret only
    /// through the environment -- it is never part of argv, so it never
    /// appears in a process listing or in anything that logs a `Cmd`'s argv.
    async fn install_pubkey(&self, profile: &Profile, ip: String) -> Result<()> {
        let password =
            profile
                .guest
                .password
                .clone()
                .ok_or_else(|| WargameError::InvalidProfile {
                    name: profile.name.clone(),
                    reason: "guest.password is required to build a profile image".to_string(),
                })?;

        let pubkey = self.pubkey()?;
        let remote_command = format!(
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh && printf '%s\\n' '{pubkey}' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys"
        );
        let cmd = Cmd {
            program: "sshpass".to_string(),
            args: vec![
                "-e".to_string(),
                "ssh".to_string(),
                "-o".to_string(),
                "StrictHostKeyChecking=no".to_string(),
                "-o".to_string(),
                "UserKnownHostsFile=/dev/null".to_string(),
                format!("{}@{ip}", profile.guest.user),
                remote_command,
            ],
            env: vec![("SSHPASS".to_string(), password)],
            stdin: None,
            cwd: None,
        };

        let deadline = Instant::now() + KEY_INSTALL_TIMEOUT;
        loop {
            let out = self.runner.run(&cmd).await?;
            if out.status == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(WargameError::GuestBootstrapFailed(format!(
                    "could not install the SSH key within {KEY_INSTALL_TIMEOUT:?}: {}",
                    out.stderr
                )));
            }
            tokio::time::sleep(KEY_INSTALL_RETRY).await;
        }
    }
}

#[async_trait]
impl ProfileImageBuilder for TartImageBuilder {
    fn backend(&self) -> &str {
        "tart"
    }

    /// Tart clones a ready-made base image; there is no OS to install by
    /// hand, so no installation VM and no `profile create`.
    fn uses_installer_vm(&self) -> bool {
        false
    }

    async fn prepare(&self, req: &BuildRequest<'_>, progress: Progress<'_>) -> Result<()> {
        // The key is not needed until `connect`, but a host that cannot
        // produce one would otherwise pull a base image, clone it and boot
        // it before finding that out (spec.md §6: fail before creating).
        self.pubkey()?;

        let base = ImageRef(req.profile.image.base.clone());
        progress(format!("checking for base image {}", base.0));
        if !self.tart.image_exists(&base).await? {
            progress(format!("pulling base image {}", base.0));
            self.tart.pull_image(&base).await?;
        }

        // Tart's image *is* the VM, so a previous build of this profile has
        // to go before a new one can take its name.
        let target = req.profile.image_name();
        if self.tart.image_exists(&ImageRef(target.clone())).await? {
            progress(format!("removing previous build of {target}"));
            self.tart.delete(&VmRef::new("tart", target)).await?;
        }
        Ok(())
    }

    async fn clone_for_build(
        &self,
        req: &BuildRequest<'_>,
        progress: Progress<'_>,
    ) -> Result<VmRef> {
        let base = req.profile.image.base.clone();
        let target = req.profile.image_name();
        progress(format!("cloning {base} -> {target}"));
        self.tart
            .create(&VmSpec {
                name: target,
                image: ImageRef(base),
                resources: req.profile.resources,
                execution: req.execution.clone(),
                // A build installs packages, so it always runs on NAT
                // (spec.md §3 FR-6); the launched sessions are isolated.
                network: NetworkMode::Nat,
            })
            .await
    }

    async fn boot(&self, req: &BuildRequest<'_>, vm: &VmRef, progress: Progress<'_>) -> Result<()> {
        progress("starting the VM headless on NAT".to_string());
        self.tart
            .start(
                vm,
                &StartOptions {
                    network: NetworkMode::Nat,
                    headless: true,
                    log_path: req.log_path.clone(),
                },
            )
            .await?;
        Ok(())
    }

    async fn connect(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        progress: Progress<'_>,
    ) -> Result<GuestLink> {
        progress("waiting for a DHCP lease".to_string());
        let ip = self.tart.wait_ip(vm, req.boot_timeout).await?;

        progress("installing the host's SSH key (password login)".to_string());
        self.install_pubkey(req.profile, ip.to_string()).await?;

        let transport = (self.transport_factory)();
        // Tart only runs macOS guests here, so this is always a POSIX
        // endpoint -- but the shell comes from the profile's OS all the
        // same, so the invariant holds by construction rather than by
        // convention (spec.md §5 item 3).
        let endpoint = GuestEndpoint {
            shell: GuestShell::for_os(req.profile.guest.os),
            ..GuestEndpoint::new(ip, req.profile.guest.user.clone())
        };

        progress("waiting for SSH to accept the new key".to_string());
        transport.wait_ready(&endpoint, req.boot_timeout).await?;
        Ok(GuestLink {
            endpoint,
            transport,
        })
    }

    async fn shutdown(
        &self,
        _req: &BuildRequest<'_>,
        vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<()> {
        self.tart.stop(vm).await?;

        let deadline = tokio::time::Instant::now() + STOP_POLL_TIMEOUT;
        loop {
            match self.tart.status(vm).await {
                Ok(VmStatus::Running) => {}
                Ok(_) => return Ok(()),
                Err(err) => {
                    tracing::warn!(
                        vm = %vm.name,
                        error = %err,
                        "failed to check VM status while confirming it stopped"
                    );
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(WargameError::VmCreationFailed(
                    "profile VM did not stop cleanly".to_string(),
                ));
            }
            tokio::time::sleep(STOP_POLL_INTERVAL).await;
        }
    }

    /// The provisioned, stopped VM already *is* the image Tart launches
    /// from, so publishing is only naming it.
    async fn publish(
        &self,
        req: &BuildRequest<'_>,
        vm: &VmRef,
        _progress: Progress<'_>,
    ) -> Result<PublishedImage> {
        let _ = vm;
        let name = req.profile.image_name();
        Ok(PublishedImage::new(name.clone(), name))
    }

    async fn discard_image(&self, image: &PublishedImage) -> Result<()> {
        self.tart
            .delete(&VmRef::new("tart", image.name.clone()))
            .await
    }

    /// NFR-02 (DESIGN.md §5): a build that failed partway through must not
    /// leave a `wargame-profile-<name>` image behind for a later `launch` to
    /// mistake for a finished one, so the partially built VM is stopped and
    /// deleted.
    async fn cleanup_failed_build(&self, _req: &BuildRequest<'_>, vm: &VmRef) -> Vec<String> {
        let mut failures = Vec::new();
        if let Err(err) = self.tart.stop(vm).await {
            tracing::warn!("failed to stop VM {} after a failed build: {err}", vm.name);
            failures.push(format!(
                "could not stop {} after the failure: {err}",
                vm.name
            ));
        }
        if let Err(err) = self.tart.delete(vm).await {
            tracing::warn!(
                "failed to delete VM {} after a failed build: {err}",
                vm.name
            );
            failures.push(format!(
                "could not delete the partially built image {}: {err}",
                vm.name
            ));
        }
        failures
    }
}

/// Where the Tart backend writes its console log while a build runs.
pub fn default_build_log(logs_dir: &std::path::Path) -> PathBuf {
    logs_dir.join("profile-build.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use wargame_core::app::profile_build::{provision_log_path, run_build_flow};
    use wargame_core::domain::build::{BuildRecord, BuildSource};
    use wargame_core::domain::{
        compat, GuestArch, GuestOs, GuestSpec, ImageSpec, ProvisionSpec, Resources, ToolsSpec,
        VmSettings, WorkspaceSpec,
    };
    use wargame_core::infra::process::Output;
    use wargame_core::testing::{FakeRunner, FakeTransport, TransportCall};

    use crate::TartConfig;

    fn sample_profile() -> Profile {
        Profile {
            name: "macos-minimal-arm64".to_string(),
            description: String::new(),
            vm: VmSettings::default(),
            guest: GuestSpec {
                os: GuestOs::Macos,
                arch: GuestArch::Arm64,
                distribution: None,
                user: "admin".to_string(),
                password: Some("s3cr3t".to_string()),
            },
            image: ImageSpec {
                base: "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest".to_string(),
            },
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 50,
            },
            provision: ProvisionSpec {
                scripts: vec![
                    "scripts/macos/common.sh".to_string(),
                    "scripts/macos/reversing.sh".to_string(),
                ],
            },
            tools: ToolsSpec {
                verify: vec!["python3 --version".to_string(), "uv --version".to_string()],
            },
            workspace: WorkspaceSpec {
                root: "/Users/admin/wargame".to_string(),
            },
        }
    }

    /// A `FakeRunner` responder that answers `tart list` with `"[]"` (so no
    /// image ever appears to already exist) and any other `tart`/`sshpass`
    /// invocation with a plain success.
    fn install_happy_responder(cmd: &Cmd) -> Result<Output> {
        if cmd.program == "tart" && cmd.args.first().map(String::as_str) == Some("list") {
            return Ok(Output {
                status: 0,
                stdout: "[]".to_string(),
                stderr: String::new(),
            });
        }
        if cmd.program == "tart" && cmd.args.first().map(String::as_str) == Some("ip") {
            return Ok(Output {
                status: 0,
                stdout: "10.0.0.7\n".to_string(),
                stderr: String::new(),
            });
        }
        Ok(Output::default())
    }

    /// A builder plus the temporary key directory it reads its public key
    /// from (kept alive for the test's duration).
    struct Fixture {
        builder: TartImageBuilder,
        _keys: tempfile::TempDir,
    }

    impl std::ops::Deref for Fixture {
        type Target = TartImageBuilder;

        fn deref(&self) -> &TartImageBuilder {
            &self.builder
        }
    }

    fn builder(runner: Arc<FakeRunner>, transport: Arc<FakeTransport>) -> Fixture {
        let keys = tempfile::tempdir().unwrap();
        let pubkey_path = keys.path().join("id_ed25519.pub");
        std::fs::write(&pubkey_path, "ssh-ed25519 AAAA... wargame-vm\n").unwrap();
        let tart = Arc::new(TartProvider::new(runner.clone(), TartConfig::default()));
        Fixture {
            builder: TartImageBuilder::new(
                runner,
                tart,
                Box::new(move || transport.clone() as Arc<dyn GuestTransport>),
                pubkey_path,
            ),
            _keys: keys,
        }
    }

    fn record(profile: &Profile) -> BuildRecord {
        BuildRecord::new(
            profile,
            "tart",
            BuildSource::TartBase {
                image: profile.image.base.clone(),
            },
        )
    }

    fn request<'a>(
        profile: &'a Profile,
        record: &'a BuildRecord,
        log_dir: &Path,
    ) -> BuildRequest<'a> {
        BuildRequest {
            profile,
            record,
            execution: compat::resolve(profile).unwrap(),
            scripts_dir: PathBuf::from("/repo/profiles"),
            log_path: log_dir.join("profile-build.log"),
            boot_timeout: Duration::from_secs(300),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn happy_path_runs_steps_in_order_and_keeps_the_password_out_of_argv() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner.clone(), transport.clone());
        let profile = sample_profile();
        let record = record(&profile);
        let req = request(&profile, &record, Path::new("/tmp"));
        let mut messages = Vec::new();

        let outcome = run_build_flow(&*b, &req, &mut |m| messages.push(m), &())
            .await
            .unwrap_or_else(|e| panic!("build failed: {}", e.error));
        assert_eq!(outcome.image.name, "wargame-profile-macos-minimal-arm64");
        assert_eq!(outcome.image.id, "wargame-profile-macos-minimal-arm64");
        assert_eq!(outcome.verification.len(), 2);
        assert!(!messages.is_empty());

        let calls = runner.calls();
        let idx = |program: &str, first_arg: Option<&str>| -> usize {
            calls
                .iter()
                .position(|c| {
                    c.program == program && c.args.first().map(String::as_str) == first_arg
                })
                .unwrap_or_else(|| panic!("no {program} {first_arg:?} call recorded: {calls:?}"))
        };

        let clone_idx = idx("tart", Some("clone"));
        let set_idx = idx("tart", Some("set"));
        let run_idx = idx("tart", Some("run"));
        let ip_idx = idx("tart", Some("ip"));
        let sshpass_idx = calls
            .iter()
            .position(|c| c.program == "sshpass")
            .expect("sshpass call recorded");
        let stop_idx = idx("tart", Some("stop"));

        assert!(clone_idx < set_idx, "clone must precede set");
        assert!(set_idx < run_idx, "set must precede run");
        assert!(run_idx < ip_idx, "run must precede ip");
        assert!(ip_idx < sshpass_idx, "ip must precede the sshpass install");
        assert!(sshpass_idx < stop_idx, "install must precede stop");
        assert!(
            calls[stop_idx].args.contains(&"--timeout".to_string()),
            "tart stop must pass an explicit --timeout: {:?}",
            calls[stop_idx]
        );
        // Nothing but the post-stop status confirmation poll (`tart list`)
        // may follow `stop` (DESIGN.md §5).
        let calls_after_stop = &calls[stop_idx + 1..];
        assert!(
            !calls_after_stop.is_empty(),
            "expected a status poll after stop to confirm it stopped cleanly"
        );
        assert!(
            calls_after_stop
                .iter()
                .all(|c| c.program == "tart" && c.args.first().map(String::as_str) == Some("list")),
            "only status polls should follow stop: {calls_after_stop:?}"
        );

        let sshpass_cmd = &calls[sshpass_idx];
        assert_eq!(sshpass_cmd.args[0], "-e");
        assert_eq!(sshpass_cmd.args[1], "ssh");
        assert_eq!(
            sshpass_cmd.env,
            vec![("SSHPASS".to_string(), "s3cr3t".to_string())]
        );
        for arg in &sshpass_cmd.args {
            assert!(
                !arg.contains("s3cr3t"),
                "password leaked into sshpass argv: {arg}"
            );
        }
        for call in &calls {
            for arg in &call.args {
                assert!(!arg.contains("s3cr3t"), "password leaked into argv: {arg}");
            }
        }

        // Scripts run in profile order, then verify commands, then cleanup.
        let transport_calls = transport.calls();
        let TransportCall::WaitReady { endpoint } = &transport_calls[0] else {
            panic!("expected the SSH readiness wait first");
        };
        // The guest user comes from the profile, not from the transport.
        assert_eq!(endpoint.user, "admin");
        assert_eq!(endpoint.host.to_string(), "10.0.0.7");
        let TransportCall::Exec { argv, .. } = &transport_calls[1] else {
            panic!("expected the mkdir -p exec before the copy");
        };
        assert_eq!(argv, &["mkdir", "-p", "wg-provision"]);
        assert!(matches!(transport_calls[2], TransportCall::CopyIn { .. }));
        let TransportCall::Exec { argv, .. } = &transport_calls[3] else {
            panic!("expected the first provisioning exec");
        };
        assert_eq!(argv, &["bash", "wg-provision/scripts/macos/common.sh"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[4] else {
            panic!("expected the second provisioning exec");
        };
        assert_eq!(argv, &["bash", "wg-provision/scripts/macos/reversing.sh"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[5] else {
            panic!("expected the first verify exec");
        };
        assert_eq!(argv, &["python3", "--version"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[6] else {
            panic!("expected the second verify exec");
        };
        assert_eq!(argv, &["uv", "--version"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[7] else {
            panic!("expected the cleanup exec");
        };
        assert_eq!(argv, &["rm", "-rf", "wg-provision"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[8] else {
            panic!("expected the pre-stop sync exec");
        };
        assert_eq!(argv, &["sync"]);
        assert_eq!(transport_calls.len(), 9);

        // The scripts directory was copied in as a whole, from where the
        // per-script exec paths expect to find it.
        let TransportCall::CopyIn { src, dest, .. } = &transport_calls[2] else {
            unreachable!()
        };
        assert_eq!(src, &Path::new("/repo/profiles/scripts"));
        assert_eq!(dest, "wg-provision/");
    }

    /// If `tart stop` returns but the VM never reports as stopped within the
    /// poll window, the build must fail loudly rather than silently hand
    /// back an image that was built from a guest killed mid-flush
    /// (DESIGN.md §5).
    #[tokio::test(start_paused = true)]
    async fn provision_fails_when_the_vm_does_not_stop_cleanly() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|cmd| {
            if cmd.program == "tart" && cmd.args.first().map(String::as_str) == Some("list") {
                // Always reports running, including after `stop` -- the VM
                // never actually stops within the poll window.
                return Ok(Output {
                    status: 0,
                    stdout: r#"[{"Name":"wargame-profile-macos-minimal-arm64","Source":"local","State":"running","Running":true,"Size":1,"Disk":1}]"#.to_string(),
                    stderr: String::new(),
                });
            }
            if cmd.program == "tart" && cmd.args.first().map(String::as_str) == Some("ip") {
                return Ok(Output {
                    status: 0,
                    stdout: "10.0.0.7\n".to_string(),
                    stderr: String::new(),
                });
            }
            Ok(Output::default())
        });
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner, transport);
        let mut profile = sample_profile();
        profile.provision.scripts.clear();
        let record = record(&profile);
        let req = request(&profile, &record, Path::new("/tmp"));

        let failure = run_build_flow(&*b, &req, &mut |_| {}, &())
            .await
            .err()
            .expect("the build must fail");
        assert!(
            matches!(failure.error, WargameError::VmCreationFailed(ref msg) if msg.contains("did not stop cleanly")),
            "expected a VmCreationFailed about not stopping cleanly, got {:?}",
            failure.error
        );
        assert!(
            !failure.awaiting_install,
            "Tart has no manual install to wait for"
        );
    }

    #[tokio::test]
    async fn verify_failure_is_invalid_profile_not_bootstrap_failed() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        transport.set_exec_status(1);
        let b = builder(runner, transport);

        let mut profile = sample_profile();
        profile.provision.scripts.clear();
        profile.tools.verify = vec!["test -x /nonexistent".to_string()];
        let record = record(&profile);
        let req = request(&profile, &record, Path::new("/tmp"));

        let failure = run_build_flow(&*b, &req, &mut |_| {}, &())
            .await
            .err()
            .unwrap();
        assert!(
            matches!(failure.error, WargameError::InvalidProfile { ref name, .. } if name == "macos-minimal-arm64")
        );
        assert_eq!(failure.verification.len(), 1);
        assert_eq!(failure.verification[0].exit_code, 1);
    }

    /// NFR-02 (DESIGN.md §5): a build that fails partway through must not
    /// leave a `wargame-profile-<name>` image behind for a later `launch` to
    /// mistake for a finished one, so a failing verify must still stop and
    /// delete the VM before the error is returned.
    #[tokio::test]
    async fn verify_failure_stops_and_deletes_the_partially_built_vm() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        transport.set_exec_status(1);
        let b = builder(runner.clone(), transport);

        let mut profile = sample_profile();
        profile.provision.scripts.clear();
        profile.tools.verify = vec!["test -x /nonexistent".to_string()];
        let record = record(&profile);
        let req = request(&profile, &record, Path::new("/tmp"));

        let failure = run_build_flow(&*b, &req, &mut |_| {}, &())
            .await
            .err()
            .unwrap();
        assert!(
            matches!(failure.error, WargameError::InvalidProfile { ref name, .. } if name == "macos-minimal-arm64")
        );

        let calls = runner.calls();
        let stop_idx = calls
            .iter()
            .position(|c| c.program == "tart" && c.args.first().map(String::as_str) == Some("stop"))
            .expect("cleanup must stop the VM after a failed build");
        let delete_idx = calls
            .iter()
            .position(|c| {
                c.program == "tart"
                    && c.args.first().map(String::as_str) == Some("delete")
                    && c.args.get(1).map(String::as_str)
                        == Some("wargame-profile-macos-minimal-arm64")
            })
            .expect("cleanup must delete the profile image after a failed build");
        assert!(
            stop_idx < delete_idx,
            "the VM must be stopped before it is deleted"
        );
    }

    /// A host with no `$WARGAME_HOME/keys/id_ed25519.pub` cannot install a
    /// key into the guest, and must find that out before pulling, cloning or
    /// booting anything.
    #[tokio::test]
    async fn a_missing_host_key_is_refused_before_anything_is_created() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let tart = Arc::new(TartProvider::new(runner.clone(), TartConfig::default()));
        let b = TartImageBuilder::new(
            runner.clone(),
            tart,
            Box::new(move || transport.clone() as Arc<dyn GuestTransport>),
            PathBuf::from("/no/such/dir/id_ed25519.pub"),
        );
        let profile = sample_profile();
        let record = record(&profile);
        let req = request(&profile, &record, Path::new("/tmp"));

        let failure = run_build_flow(&b, &req, &mut |_| {}, &())
            .await
            .err()
            .expect("a missing host key must fail the build");
        assert_eq!(
            failure.stage,
            wargame_core::domain::build::BuildStage::Preparing
        );
        assert!(
            failure.error.to_string().contains("id_ed25519.pub"),
            "{}",
            failure.error
        );
        assert!(
            runner.calls().is_empty(),
            "nothing may run before the key is known: {:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn missing_password_is_rejected_before_any_guest_transport_use() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner, transport.clone());

        let mut profile = sample_profile();
        profile.guest.password = None;
        let record = record(&profile);
        let req = request(&profile, &record, Path::new("/tmp"));

        let failure = run_build_flow(&*b, &req, &mut |_| {}, &())
            .await
            .err()
            .unwrap();
        assert!(matches!(failure.error, WargameError::InvalidProfile { .. }));
        assert!(transport.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn happy_path_logs_every_provisioning_command_to_the_provision_log() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner, transport);
        let profile = sample_profile();
        let record = record(&profile);
        let log_dir = tempfile::tempdir().unwrap();
        let req = request(&profile, &record, log_dir.path());

        run_build_flow(&*b, &req, &mut |_| {}, &())
            .await
            .unwrap_or_else(|e| panic!("build failed: {}", e.error));

        let log_path = provision_log_path(&req.log_path, &profile.name);
        let log = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("expected a provisioning log at {log_path:?}: {e}"));

        assert!(
            log.contains("==== bash wg-provision/scripts/macos/common.sh (exit 0) ===="),
            "log missing common.sh header: {log}"
        );
        assert!(
            log.contains("==== bash wg-provision/scripts/macos/reversing.sh (exit 0) ===="),
            "log missing reversing.sh header: {log}"
        );
        assert!(
            log.contains("==== python3 --version (exit 0) ===="),
            "log missing python3 verify header: {log}"
        );
        assert!(
            log.contains("==== uv --version (exit 0) ===="),
            "log missing uv verify header: {log}"
        );
    }

    #[tokio::test]
    async fn failing_verify_error_includes_the_guest_stderr() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        transport.set_exec_responder(|argv| {
            if argv == ["uv".to_string(), "--version".to_string()] {
                wargame_core::ports::ExecResult {
                    status: 127,
                    stdout: String::new(),
                    stderr: "zsh:1: command not found: uv".to_string(),
                }
            } else {
                wargame_core::ports::ExecResult {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                }
            }
        });
        let log_dir = tempfile::tempdir().unwrap();
        let b = builder(runner, transport);
        let profile = sample_profile();
        let record = record(&profile);
        let req = request(&profile, &record, log_dir.path());

        let failure = run_build_flow(&*b, &req, &mut |_| {}, &())
            .await
            .err()
            .unwrap();

        let WargameError::InvalidProfile { reason, .. } = failure.error else {
            panic!("expected InvalidProfile");
        };
        assert!(
            reason.contains("zsh:1: command not found: uv"),
            "error reason should surface the guest's stderr: {reason}"
        );

        let log_path = provision_log_path(&req.log_path, &profile.name);
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log.contains("==== uv --version (exit 127) ===="),
            "log missing failing verify header: {log}"
        );
        assert!(log.contains("zsh:1: command not found: uv"));
    }
}
