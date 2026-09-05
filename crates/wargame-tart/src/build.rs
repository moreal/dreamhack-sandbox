//! `ProfileImageBuilder`: turns a declarative [`Profile`] into a reusable
//! local Tart image (DESIGN.md §5/§6, MVP.md §13-14, PLAN.md Task 6).
//!
//! The pipeline: make sure the base image is local, clear out any previous
//! build of the same profile image, clone + size a fresh VM, boot it
//! headless on NAT, install the host's SSH public key over a one-time
//! password login (`sshpass`, secret only ever in the `SSHPASS` env var --
//! never argv, never logged), then run the profile's provisioning scripts
//! and verification commands over that now-keyed SSH connection before
//! stopping the VM and handing back its image name.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wargame_core::domain::{NetworkMode, Profile, VmRef};
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::process::{Cmd, ProcessRunner};
use wargame_core::ports::{GuestTransport, ImageRef, StartOptions, VmProvider, VmSpec};

use crate::TartProvider;

/// How often the password-based SSH probe (installing the host's public key)
/// is retried while the guest's `sshd` finishes coming up.
const KEY_INSTALL_RETRY: Duration = Duration::from_secs(5);
/// Total time budget for that probe before giving up.
const KEY_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Builds a [`GuestTransport`] for a given guest username, deferred until
/// after the host's SSH key has actually been installed (see
/// [`ProfileImageBuilder::build_profile`]).
pub type TransportFactory = Box<dyn Fn(&str) -> Arc<dyn GuestTransport> + Send + Sync>;

/// Inputs to [`ProfileImageBuilder::build_profile`] that aren't already part
/// of the [`Profile`] itself.
pub struct BuildOptions {
    /// Directory containing the `scripts/` subtree that a profile's
    /// `provision.scripts` paths are relative to -- the repo's `profiles/`
    /// directory, or `$WARGAME_HOME/profiles` for a user override.
    pub scripts_dir: PathBuf,
    /// The host's ed25519 public key (contents of `id_ed25519.pub`),
    /// installed into the guest's `authorized_keys` so future sessions can
    /// SSH in without ever needing the build-time password again.
    pub pubkey: String,
    pub log_path: PathBuf,
}

pub struct ProfileImageBuilder {
    runner: Arc<dyn ProcessRunner>,
    tart: Arc<TartProvider>,
    transport_factory: TransportFactory,
}

impl ProfileImageBuilder {
    pub fn new(
        runner: Arc<dyn ProcessRunner>,
        tart: Arc<TartProvider>,
        transport_factory: TransportFactory,
    ) -> Self {
        ProfileImageBuilder {
            runner,
            tart,
            transport_factory,
        }
    }

    pub async fn build_profile(
        &self,
        profile: &Profile,
        opts: &BuildOptions,
        progress: &mut dyn FnMut(String),
    ) -> Result<ImageRef> {
        let base = ImageRef(profile.image.base.clone());
        let target_name = profile.image_name();
        let target_ref = ImageRef(target_name.clone());

        progress(format!("checking for base image {}", base.0));
        if !self.tart.image_exists(&base).await? {
            progress(format!("pulling base image {}", base.0));
            self.tart.pull_image(&base).await?;
        }

        if self.tart.image_exists(&target_ref).await? {
            progress(format!("removing previous build of {target_name}"));
            self.tart
                .delete(&VmRef {
                    backend: "tart".to_string(),
                    name: target_name.clone(),
                })
                .await?;
        }

        progress(format!("cloning {} -> {target_name}", base.0));
        let vm = self
            .tart
            .create(&VmSpec {
                name: target_name.clone(),
                image: base,
                resources: profile.resources,
            })
            .await?;

        match self.provision(&vm, profile, opts, progress).await {
            Ok(()) => Ok(ImageRef(target_name)),
            Err(err) => {
                // A partially provisioned image must never satisfy
                // `image_exists` for a later `launch` (DESIGN.md §5, NFR-02),
                // so best-effort tear the VM back down before propagating the
                // original failure.
                progress("build failed; cleaning up the partially built VM".to_string());
                if let Err(stop_err) = self.tart.stop(&vm).await {
                    tracing::warn!(
                        "failed to stop VM {} after a failed build: {stop_err}",
                        vm.name
                    );
                }
                if let Err(delete_err) = self.tart.delete(&vm).await {
                    tracing::warn!(
                        "failed to delete VM {} after a failed build: {delete_err}",
                        vm.name
                    );
                }
                Err(err)
            }
        }
    }

    /// The provisioning steps after the clone: start, wait for an IP,
    /// install the host key, copy in and run the provisioning scripts, run
    /// the profile's verify commands, clean up, and stop the VM. Split out
    /// from [`Self::build_profile`] so any failure here can be met with
    /// best-effort cleanup of the partially built image.
    async fn provision(
        &self,
        vm: &VmRef,
        profile: &Profile,
        opts: &BuildOptions,
        progress: &mut dyn FnMut(String),
    ) -> Result<()> {
        progress("starting the VM headless on NAT".to_string());
        self.tart
            .start(
                vm,
                &StartOptions {
                    network: NetworkMode::Nat,
                    headless: true,
                    log_path: opts.log_path.clone(),
                },
            )
            .await?;

        progress("waiting for a DHCP lease".to_string());
        let ip = self.tart.wait_ip(vm, self.tart.boot_timeout()).await?;

        progress("installing the host's SSH key (password login)".to_string());
        self.install_pubkey(profile, ip.to_string(), &opts.pubkey)
            .await?;

        let transport = (self.transport_factory)(&profile.guest.user);

        progress("waiting for SSH to accept the new key".to_string());
        transport.wait_ready(ip, self.tart.boot_timeout()).await?;

        let log_path = provision_log_path(opts, profile);
        progress(format!("provisioning log: {}", log_path.display()));

        progress("copying provisioning scripts into the guest".to_string());
        // `~` is never expanded in `GuestTransport::exec` argv (every
        // argument is single-quoted before being sent to the guest shell),
        // so all guest-side paths here are relative to the SSH user's home
        // directory instead. `scp -r <src>/scripts wg-provision` would, on a
        // guest where `wg-provision` doesn't already exist, create
        // `wg-provision` *as* a copy of `scripts` itself -- so create the
        // destination directory first and copy into it with a trailing
        // slash, giving `wg-provision/scripts/...` as the profile's script
        // paths expect.
        transport
            .exec(
                ip,
                &[
                    "mkdir".to_string(),
                    "-p".to_string(),
                    "wg-provision".to_string(),
                ],
            )
            .await?;
        transport
            .copy_in(ip, &opts.scripts_dir.join("scripts"), "wg-provision/")
            .await?;

        for script in &profile.provision.scripts {
            progress(format!("provisioning: {script}"));
            let remote_path = format!("wg-provision/{script}");
            let argv = vec!["bash".to_string(), remote_path];
            let command_desc = argv.join(" ");
            let out = transport.exec(ip, &argv).await?;
            log_exec(
                &log_path,
                &command_desc,
                out.status,
                &out.stdout,
                &out.stderr,
            );
            if out.status != 0 {
                return Err(WargameError::GuestBootstrapFailed(format!(
                    "provisioning script {script} failed (exit {}): {}",
                    out.status,
                    tail_lines(&combined_output(&out.stdout, &out.stderr), 15)
                )));
            }
        }

        for check in &profile.tools.verify {
            progress(format!("verifying: {check}"));
            let argv: Vec<String> = check.split_whitespace().map(str::to_string).collect();
            if argv.is_empty() {
                continue;
            }
            let command_desc = argv.join(" ");
            let out = transport.exec(ip, &argv).await?;
            log_exec(
                &log_path,
                &command_desc,
                out.status,
                &out.stdout,
                &out.stderr,
            );
            if out.status != 0 {
                return Err(WargameError::InvalidProfile {
                    name: profile.name.clone(),
                    reason: format!(
                        "verify command {check:?} failed (exit {}): {}",
                        out.status,
                        tail_lines(&combined_output(&out.stdout, &out.stderr), 15)
                    ),
                });
            }
        }

        progress("cleaning up provisioning files".to_string());
        transport
            .exec(
                ip,
                &[
                    "rm".to_string(),
                    "-rf".to_string(),
                    "wg-provision".to_string(),
                ],
            )
            .await?;

        progress("stopping the VM".to_string());
        self.tart.stop(vm).await?;

        Ok(())
    }

    /// Installs `pubkey` into the guest's `authorized_keys` over a
    /// password-authenticated SSH connection (`sshpass`), retrying while the
    /// guest's `sshd` finishes starting. `SSHPASS` carries the secret only
    /// through the environment -- it is never part of argv, so it never
    /// appears in a process listing or in anything that logs a `Cmd`'s argv.
    async fn install_pubkey(&self, profile: &Profile, ip: String, pubkey: &str) -> Result<()> {
        let password =
            profile
                .guest
                .password
                .clone()
                .ok_or_else(|| WargameError::InvalidProfile {
                    name: profile.name.clone(),
                    reason: "guest.password is required to build a profile image".to_string(),
                })?;

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

/// Where provisioning output for `profile` is logged: `opts.log_path`'s
/// directory (typically `$WARGAME_HOME/logs`), with the file name replaced
/// by `provision-<profile name>.log` so it doesn't clobber
/// `profile-build.log` (the Tart `start` console log) and survives across
/// builds of different profiles.
fn provision_log_path(opts: &BuildOptions, profile: &Profile) -> PathBuf {
    let dir = opts
        .log_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    dir.join(format!("provision-{}.log", profile.name))
}

/// Combines stdout and stderr into one blob for the "last N lines" shown in
/// an error message -- interleaving isn't preserved (we only get each
/// stream as a whole from `ExecResult`), but seeing both is more useful to a
/// user debugging a failure than seeing neither.
fn combined_output(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    combined.push_str(stdout);
    if !stdout.is_empty() && !stderr.is_empty() && !stdout.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(stderr);
    combined
}

/// The last `n` lines of `text` (fewer if it has fewer), for embedding a
/// bounded amount of guest output directly in an error message.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Appends one command's output to the provisioning log as a
/// `==== <command> (exit N) ====` header followed by its stdout and stderr.
/// Never fails the build: a log write failure is only `tracing::warn`.
fn log_exec(log_path: &Path, command: &str, status: i32, stdout: &str, stderr: &str) {
    let mut body = format!("==== {command} (exit {status}) ====\n");
    if !stdout.is_empty() {
        body.push_str(stdout);
        if !stdout.ends_with('\n') {
            body.push('\n');
        }
    }
    if !stderr.is_empty() {
        body.push_str(stderr);
        if !stderr.ends_with('\n') {
            body.push('\n');
        }
    }

    if let Some(parent) = log_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "failed to create provisioning log directory {}: {err}",
                parent.display()
            );
            return;
        }
    }
    let opened = OpenOptions::new().create(true).append(true).open(log_path);
    match opened {
        Ok(mut file) => {
            if let Err(err) = file.write_all(body.as_bytes()) {
                tracing::warn!(
                    "failed to write provisioning log {}: {err}",
                    log_path.display()
                );
            }
        }
        Err(err) => {
            tracing::warn!(
                "failed to open provisioning log {}: {err}",
                log_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use wargame_core::domain::{
        GuestArch, GuestOs, GuestSpec, ImageSpec, ProvisionSpec, Resources, ToolsSpec,
        WorkspaceSpec,
    };
    use wargame_core::infra::process::Output;
    use wargame_core::testing::{FakeRunner, FakeTransport, TransportCall};

    use crate::TartConfig;

    fn sample_profile() -> Profile {
        Profile {
            name: "macos-minimal-arm64".to_string(),
            description: String::new(),
            guest: GuestSpec {
                os: GuestOs::Macos,
                arch: GuestArch::Arm64,
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

    fn builder(runner: Arc<FakeRunner>, transport: Arc<FakeTransport>) -> ProfileImageBuilder {
        let tart = Arc::new(TartProvider::new(runner.clone(), TartConfig::default()));
        ProfileImageBuilder::new(
            runner,
            tart,
            Box::new(move |_user: &str| transport.clone() as Arc<dyn GuestTransport>),
        )
    }

    fn opts() -> BuildOptions {
        BuildOptions {
            scripts_dir: PathBuf::from("/repo/profiles"),
            pubkey: "ssh-ed25519 AAAA... wargame-vm".to_string(),
            log_path: PathBuf::from("/tmp/build.log"),
        }
    }

    fn opts_with_log_dir(log_dir: &Path) -> BuildOptions {
        BuildOptions {
            scripts_dir: PathBuf::from("/repo/profiles"),
            pubkey: "ssh-ed25519 AAAA... wargame-vm".to_string(),
            log_path: log_dir.join("profile-build.log"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn happy_path_runs_steps_in_order_and_keeps_the_password_out_of_argv() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner.clone(), transport.clone());
        let profile = sample_profile();
        let mut messages = Vec::new();

        let image = b
            .build_profile(&profile, &opts(), &mut |m| messages.push(m))
            .await
            .unwrap();
        assert_eq!(image.0, "wargame-profile-macos-minimal-arm64");
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
        assert_eq!(stop_idx, calls.len() - 1, "stop must be the last call");

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
        let TransportCall::Exec { argv, .. } = &transport_calls[0] else {
            panic!("expected the mkdir -p exec before the copy");
        };
        assert_eq!(argv, &["mkdir", "-p", "wg-provision"]);
        assert!(matches!(transport_calls[1], TransportCall::CopyIn { .. }));
        let TransportCall::Exec { argv, .. } = &transport_calls[2] else {
            panic!("expected the first provisioning exec");
        };
        assert_eq!(argv, &["bash", "wg-provision/scripts/macos/common.sh"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[3] else {
            panic!("expected the second provisioning exec");
        };
        assert_eq!(argv, &["bash", "wg-provision/scripts/macos/reversing.sh"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[4] else {
            panic!("expected the first verify exec");
        };
        assert_eq!(argv, &["python3", "--version"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[5] else {
            panic!("expected the second verify exec");
        };
        assert_eq!(argv, &["uv", "--version"]);
        let TransportCall::Exec { argv, .. } = &transport_calls[6] else {
            panic!("expected the cleanup exec");
        };
        assert_eq!(argv, &["rm", "-rf", "wg-provision"]);
        assert_eq!(transport_calls.len(), 7);

        // The scripts directory was copied in as a whole, from where the
        // per-script exec paths expect to find it.
        let TransportCall::CopyIn { src, dest, .. } = &transport_calls[1] else {
            unreachable!()
        };
        assert_eq!(src, &Path::new("/repo/profiles/scripts"));
        assert_eq!(dest, "wg-provision/");
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

        let err = b
            .build_profile(&profile, &opts(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(
            matches!(err, WargameError::InvalidProfile { name, .. } if name == "macos-minimal-arm64")
        );
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

        let err = b
            .build_profile(&profile, &opts(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(
            matches!(err, WargameError::InvalidProfile { name, .. } if name == "macos-minimal-arm64")
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

    #[tokio::test]
    async fn missing_password_is_rejected_before_any_guest_transport_use() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner, transport.clone());

        let mut profile = sample_profile();
        profile.guest.password = None;

        let err = b
            .build_profile(&profile, &opts(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, WargameError::InvalidProfile { .. }));
        assert!(transport.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn happy_path_logs_every_provisioning_command_to_the_provision_log() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(install_happy_responder);
        let transport = Arc::new(FakeTransport::new());
        let b = builder(runner, transport);
        let profile = sample_profile();
        let log_dir = tempfile::tempdir().unwrap();
        let opts = opts_with_log_dir(log_dir.path());

        b.build_profile(&profile, &opts, &mut |_| {}).await.unwrap();

        let log_path = provision_log_path(&opts, &profile);
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
        let opts = opts_with_log_dir(log_dir.path());

        let err = b
            .build_profile(&profile, &opts, &mut |_| {})
            .await
            .unwrap_err();

        let WargameError::InvalidProfile { reason, .. } = err else {
            panic!("expected InvalidProfile, got {err:?}");
        };
        assert!(
            reason.contains("zsh:1: command not found: uv"),
            "error reason should surface the guest's stderr: {reason}"
        );

        let log_path = provision_log_path(&opts, &profile);
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log.contains("==== uv --version (exit 127) ===="),
            "log missing failing verify header: {log}"
        );
        assert!(log.contains("zsh:1: command not found: uv"));
    }
}
