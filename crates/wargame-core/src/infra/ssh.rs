//! `ssh`/`scp`-backed [`GuestTransport`] (DESIGN.md §3.3, §3.6).
//!
//! Every invocation goes through [`ProcessRunner`] with an explicit argv —
//! no shell strings, and the same hardened options every time (host key
//! checking off, since guests are disposable; short connect timeout; batch
//! mode so a stuck credential prompt cannot hang the CLI).
//!
//! One transport serves every guest: the [`GuestEndpoint`]'s
//! [`GuestShell`] decides how a command is phrased on the wire
//! (spec.md §3 FR-4). A POSIX endpoint produces exactly the argv the
//! pre-multi-OS transport produced — the macOS path `docs/verification.md`
//! records as working against a real VM — and a PowerShell endpoint gets a
//! base64 `-EncodedCommand` instead, so no POSIX quoting ever reaches a
//! Windows guest.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::domain::session::{GuestShell, DEFAULT_GUEST_PORT};
use crate::error::{Result, WargameError};
use crate::infra::powershell::{self, PowerShellCommand};
use crate::infra::process::{Cmd, Output, ProcessRunner};
use crate::ports::guest_transport::{ExecResult, GuestEndpoint, GuestTransport};

const WAIT_READY_POLL: Duration = Duration::from_secs(3);

/// Which guest-side service a readiness probe found missing (spec.md §6's
/// "SSH/guest agent 미준비: 필요한 서비스를 구분하고 단계·timeout 표시").
///
/// The guest *agent* is not in this list on purpose: [`GuestTransport::wait_ready`]
/// only runs once an address is known, and on UTM that address comes from
/// the QEMU guest agent itself, so by then the agent has answered. A guest
/// that never reports an address fails earlier, with
/// [`WargameError::GuestAddressTimeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestService {
    /// Nothing accepted a TCP/SSH connection at all.
    Ssh,
    /// SSH answered but refused the key.
    Authentication,
    /// SSH authenticated, but the guest could not run the command — a
    /// Windows guest whose default SSH shell is not PowerShell, say.
    Shell,
}

impl GuestService {
    pub fn as_str(self) -> &'static str {
        match self {
            GuestService::Ssh => "SSH",
            GuestService::Authentication => "SSH authentication",
            GuestService::Shell => "the guest shell",
        }
    }
}

impl std::fmt::Display for GuestService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The SSH transport. The user, port, key and shell come from the
/// [`GuestEndpoint`] the caller passes -- the profile's `guest.user`/OS at
/// launch, the session's saved connection afterwards -- so nothing here
/// assumes one host-wide guest account or one guest OS (spec.md §3 FR-4).
///
/// `default_key_path` is only the fallback for an endpoint that names no
/// key of its own (the host's `$WARGAME_HOME/keys/id_ed25519`).
pub struct SshGuestTransport {
    runner: Arc<dyn ProcessRunner>,
    default_key_path: PathBuf,
}

impl SshGuestTransport {
    pub fn new(runner: Arc<dyn ProcessRunner>, default_key_path: PathBuf) -> Self {
        SshGuestTransport {
            runner,
            default_key_path,
        }
    }

    /// The `-i <key>`, `StrictHostKeyChecking=no`, etc. options common to
    /// every `ssh`/`scp` invocation. `port_flag` differs between the two
    /// tools (`ssh -p`, `scp -P`), and is only added for a non-default port
    /// so existing command lines are unchanged.
    fn base_args(&self, endpoint: &GuestEndpoint, port_flag: &str) -> Vec<String> {
        let key_path = endpoint
            .key_path
            .as_deref()
            .unwrap_or(&self.default_key_path);
        let mut args = vec![
            "-i".to_string(),
            key_path.display().to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            "UserKnownHostsFile=/dev/null".to_string(),
            "-o".to_string(),
            "ConnectTimeout=5".to_string(),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
        ];
        if endpoint.port != DEFAULT_GUEST_PORT {
            args.push(port_flag.to_string());
            args.push(endpoint.port.to_string());
        }
        args
    }

    fn target(&self, endpoint: &GuestEndpoint) -> String {
        format!("{}@{}", endpoint.user, endpoint.host)
    }

    /// The remote command for `argv`, i.e. everything after `user@host`.
    ///
    /// POSIX: unchanged from the pre-multi-OS transport -- `--` and one
    /// single-quoted command string for the guest's login shell.
    /// PowerShell: `powershell -NoProfile -NonInteractive -EncodedCommand
    /// <base64>`, so quoting, spaces, Korean text and newlines never pass
    /// through a shell's word splitting at all.
    fn remote_command(&self, endpoint: &GuestEndpoint, argv: &[String]) -> Vec<String> {
        match endpoint.shell {
            GuestShell::Posix => {
                let joined = argv
                    .iter()
                    .map(|a| shell_quote(a))
                    .collect::<Vec<_>>()
                    .join(" ");
                vec!["--".to_string(), joined]
            }
            GuestShell::PowerShell => PowerShellCommand::from_argv(argv).remote_argv(),
        }
    }

    /// Runs an already-phrased remote command and restores the guest's true
    /// exit code.
    ///
    /// The local `ssh` process can only exit with the low byte of the remote
    /// status, so a Windows script's 3010 ("reboot required") would arrive as
    /// 194. The PowerShell wrapper prints out-of-range codes on a marker line
    /// and exits 255; here that line is turned back into the real status and
    /// removed from stderr, so no caller -- or log -- ever sees either the
    /// truncation or the marker.
    async fn run_remote(
        &self,
        endpoint: &GuestEndpoint,
        remote: Vec<String>,
    ) -> Result<ExecResult> {
        let mut args = self.base_args(endpoint, "-p");
        args.push(self.target(endpoint));
        args.extend(remote);

        let cmd = Cmd::new("ssh", args);
        let out = self.runner.run(&cmd).await?;
        let (status, stderr) = restore_exit_code(out.status, out.stderr);
        Ok(ExecResult {
            status,
            stdout: out.stdout,
            stderr,
        })
    }

    /// The command that answers "is the guest ready?" in its own shell.
    fn readiness_probe(&self, endpoint: &GuestEndpoint) -> Vec<String> {
        match endpoint.shell {
            // Byte-identical to the pre-multi-OS probe.
            GuestShell::Posix => vec!["true".to_string()],
            GuestShell::PowerShell => PowerShellCommand::from_script("exit 0").remote_argv(),
        }
    }

    /// Classifies one failed readiness probe. `ssh` reports every failure of
    /// its own with status 255, so the distinction between "nothing is
    /// listening" and "the key was refused" is in stderr; any other status
    /// came from the guest, which means SSH itself worked and the *shell*
    /// did not.
    fn classify(outcome: &std::result::Result<Output, String>) -> (GuestService, String) {
        match outcome {
            Err(error) => (GuestService::Ssh, error.clone()),
            Ok(out) => {
                let stderr = out.stderr.trim().to_string();
                let lowered = stderr.to_ascii_lowercase();
                if out.status == 255 {
                    if lowered.contains("permission denied")
                        || lowered.contains("authentication")
                        || lowered.contains("publickey")
                    {
                        (GuestService::Authentication, stderr)
                    } else {
                        (GuestService::Ssh, stderr)
                    }
                } else {
                    let detail = if stderr.is_empty() {
                        format!("the probe exited with status {}", out.status)
                    } else {
                        stderr
                    };
                    (GuestService::Shell, detail)
                }
            }
        }
    }

    fn not_ready(
        endpoint: &GuestEndpoint,
        service: GuestService,
        detail: &str,
        elapsed: Duration,
        timeout: Duration,
    ) -> WargameError {
        let detail = format!(
            "{} is not ready on {endpoint} after {:.1} s (timeout {} s): {}",
            service,
            elapsed.as_secs_f64(),
            timeout.as_secs(),
            if detail.is_empty() {
                "the guest gave no reason".to_string()
            } else {
                detail.to_string()
            },
        );
        let prepare = match (service, endpoint.shell) {
            (GuestService::Authentication, _) => format!(
                "Append the host's public key ($WARGAME_HOME/keys/id_ed25519.pub) to {}'s authorized_keys in the guest.",
                endpoint.user
            ),
            (_, GuestShell::PowerShell) =>
                "In the guest: install OpenSSH Server (Settings > Optional features), start the sshd service, make PowerShell its default SSH shell (`New-ItemProperty -Path 'HKLM:\\SOFTWARE\\OpenSSH' -Name DefaultShell -Value 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe'`), and install the UTM guest tools so the address stays discoverable.".to_string(),
            (_, GuestShell::Posix) => format!(
                "In the guest: make sure the SSH server is running, the user {:?} exists, and (on UTM) `qemu-guest-agent` is installed so the address stays discoverable.",
                endpoint.user
            ),
        };
        WargameError::GuestNotReady {
            service: service.as_str().to_string(),
            detail,
            hint: format!(
                "{prepare}\nOpen a console with `wargame gui <session>` (or `wargame profile console <profile>`) to fix it, then retry.\nRaise the wait with `[vm.utm] boot_timeout_secs` (emulate) / `virtualize_boot_timeout_secs`, or `[vm.tart] boot_timeout_secs`, in $WARGAME_HOME/config.toml."
            ),
        }
    }
}

#[async_trait]
impl GuestTransport for SshGuestTransport {
    async fn wait_ready(&self, endpoint: &GuestEndpoint, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let deadline = started + timeout;
        let probe = self.readiness_probe(endpoint);
        loop {
            let mut args = self.base_args(endpoint, "-p");
            args.push(self.target(endpoint));
            args.extend(probe.iter().cloned());
            let cmd = Cmd::new("ssh", args);
            let outcome = self
                .runner
                .run(&cmd)
                .await
                .map_err(|e: WargameError| e.to_string());
            if let Ok(out) = &outcome {
                if out.status == 0 {
                    return Ok(());
                }
            }
            let now = Instant::now();
            if now >= deadline {
                // A missing service is never success: report which one, how
                // long we waited and the timeout that was used.
                let (service, detail) = Self::classify(&outcome);
                return Err(Self::not_ready(
                    endpoint,
                    service,
                    &detail,
                    now - started,
                    timeout,
                ));
            }
            tokio::time::sleep((deadline - now).min(WAIT_READY_POLL)).await;
        }
    }

    async fn exec(&self, endpoint: &GuestEndpoint, argv: &[String]) -> Result<ExecResult> {
        let remote = self.remote_command(endpoint, argv);
        self.run_remote(endpoint, remote).await
    }

    async fn exec_script(&self, endpoint: &GuestEndpoint, script: &str) -> Result<ExecResult> {
        let remote = match endpoint.shell {
            // A POSIX guest never needs this path (`GuestOps` gives it argv),
            // but the port has to work for every endpoint: hand the script to
            // the guest's own `bash -c`, quoted like any other argument.
            GuestShell::Posix => self.remote_command(
                endpoint,
                &["bash".to_string(), "-c".to_string(), script.to_string()],
            ),
            GuestShell::PowerShell => PowerShellCommand::from_script(script).remote_argv(),
        };
        self.run_remote(endpoint, remote).await
    }

    async fn copy_in(&self, endpoint: &GuestEndpoint, src: &Path, dest: &str) -> Result<()> {
        // `scp` spells the port `-P`, not `-p` (which means "preserve times").
        //
        // `dest` is passed exactly as the guest will see it, with no quoting
        // of its own: modern `scp` speaks the SFTP protocol (the default
        // since OpenSSH 9.0; `-O` would opt back into the legacy protocol,
        // and is deliberately never passed), so the remote path is
        // transferred as data and no guest shell ever re-parses it. That is
        // what lets a Windows destination keep its drive letter,
        // backslashes, spaces, quotes and Korean characters.
        let mut args = self.base_args(endpoint, "-P");
        args.push("-r".to_string());
        args.push(src.display().to_string());
        args.push(format!("{}:{}", self.target(endpoint), dest));

        let cmd = Cmd::new("scp", args);
        let out = self.runner.run(&cmd).await?;
        if out.status != 0 {
            return Err(WargameError::CopyFailed(out.stderr));
        }
        Ok(())
    }

    fn interactive_shell_cmd(&self, endpoint: &GuestEndpoint) -> StdCommand {
        let mut cmd = StdCommand::new("ssh");
        cmd.arg("-t");
        for arg in self.base_args(endpoint, "-p") {
            cmd.arg(arg);
        }
        cmd.arg(self.target(endpoint));
        // A Windows guest's default SSH shell may well be `cmd.exe`; `wargame
        // ssh` into a Windows session must still land in PowerShell
        // (spec.md §3 FR-4).
        if endpoint.shell == GuestShell::PowerShell {
            cmd.arg("powershell");
            cmd.arg("-NoLogo");
        }
        cmd
    }
}

/// POSIX single-quote escaping: wraps `s` in single quotes, replacing any
/// embedded `'` with `'\''` (close quote, escaped literal quote, reopen
/// quote). Only correct for [`crate::domain::GuestShell::Posix`] endpoints;
/// a PowerShell guest is quoted by [`crate::infra::powershell::quote`]
/// instead (spec.md §3 FR-4: "Unix shell quoting을 Windows에 재사용하지
/// 않는다").
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Puts back an exit code that could not survive `ssh`'s 8-bit exit status,
/// and removes the marker line that carried it.
///
/// Only `ssh`'s own 255 is examined, and only when the wrapper's marker is
/// actually present, so a genuine 255 (or a genuine 194) is never
/// reinterpreted.
fn restore_exit_code(status: i32, stderr: String) -> (i32, String) {
    if status != 255 {
        return (status, stderr);
    }
    match powershell::take_exit_code_marker(&stderr) {
        Some((code, cleaned)) => (code, cleaned),
        None => (status, stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{GuestShell, DEFAULT_GUEST_PORT};
    use crate::testing::FakeRunner;
    use std::net::IpAddr;
    use std::str::FromStr;

    fn endpoint(user: &str) -> GuestEndpoint {
        GuestEndpoint::new(IpAddr::from_str("10.0.0.5").unwrap(), user)
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[tokio::test]
    async fn exec_argv_has_base_options() {
        let runner = Arc::new(FakeRunner::new());
        let transport = SshGuestTransport::new(runner.clone(), PathBuf::from("/keys/id_ed25519"));

        transport
            .exec(
                &endpoint("admin"),
                &["echo".to_string(), "hi there".to_string()],
            )
            .await
            .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        let cmd = &calls[0];
        assert_eq!(cmd.program, "ssh");
        assert_eq!(cmd.args[0], "-i");
        assert_eq!(cmd.args[1], "/keys/id_ed25519");
        assert!(cmd.args.contains(&"StrictHostKeyChecking=no".to_string()));
        assert!(cmd
            .args
            .contains(&"UserKnownHostsFile=/dev/null".to_string()));
        assert!(cmd.args.contains(&"ConnectTimeout=5".to_string()));
        assert!(cmd.args.contains(&"admin@10.0.0.5".to_string()));
        assert!(
            !cmd.args.contains(&"-p".to_string()),
            "the default port stays implicit"
        );
        assert!(cmd.args.last().unwrap().contains("echo"));
        assert!(cmd.args.last().unwrap().contains("'hi there'"));
    }

    #[tokio::test]
    async fn the_endpoint_supplies_user_port_and_key() {
        let runner = Arc::new(FakeRunner::new());
        let transport = SshGuestTransport::new(runner.clone(), PathBuf::from("/keys/id_ed25519"));
        let endpoint = GuestEndpoint {
            host: IpAddr::from_str("127.0.0.1").unwrap(),
            port: 22022,
            user: "kali".to_string(),
            key_path: Some(PathBuf::from("/keys/kali_ed25519")),
            shell: GuestShell::Posix,
        };

        transport
            .exec(&endpoint, &["id".to_string()])
            .await
            .unwrap();
        transport
            .copy_in(&endpoint, Path::new("/tmp/a.zip"), "incoming/")
            .await
            .unwrap();

        let calls = runner.calls();
        assert_eq!(calls[0].program, "ssh");
        assert_eq!(calls[0].args[1], "/keys/kali_ed25519");
        assert!(calls[0].args.contains(&"-p".to_string()));
        assert!(calls[0].args.contains(&"22022".to_string()));
        assert!(calls[0].args.contains(&"kali@127.0.0.1".to_string()));

        // `scp` spells the port `-P`.
        assert_eq!(calls[1].program, "scp");
        assert!(calls[1].args.contains(&"-P".to_string()));
        assert!(!calls[1].args.contains(&"-p".to_string()));
        assert!(calls[1]
            .args
            .contains(&"kali@127.0.0.1:incoming/".to_string()));
        assert_eq!(DEFAULT_GUEST_PORT, 22);
    }

    #[tokio::test]
    async fn copy_in_uses_scp_with_recursive_flag() {
        let runner = Arc::new(FakeRunner::new());
        let transport = SshGuestTransport::new(runner.clone(), PathBuf::from("/keys/id_ed25519"));
        let src = PathBuf::from("/tmp/asset.zip");

        transport
            .copy_in(&endpoint("admin"), &src, "~/wargame/incoming/")
            .await
            .unwrap();

        let calls = runner.calls();
        assert_eq!(calls[0].program, "scp");
        assert!(calls[0].args.contains(&"-r".to_string()));
        assert!(calls[0]
            .args
            .contains(&"admin@10.0.0.5:~/wargame/incoming/".to_string()));
    }

    #[tokio::test]
    async fn wait_ready_polls_until_success() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_response(crate::infra::process::Output {
            status: 255,
            stdout: String::new(),
            stderr: String::new(),
        });
        // FakeRunner falls back to a default `status: 0` response once the queue is empty.
        let transport = SshGuestTransport::new(runner.clone(), PathBuf::from("/keys/id_ed25519"));

        transport
            .wait_ready(&endpoint("admin"), Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(runner.calls().len(), 2);
    }

    #[tokio::test]
    async fn wait_ready_times_out() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|_| {
            Ok(crate::infra::process::Output {
                status: 255,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let transport = SshGuestTransport::new(runner.clone(), PathBuf::from("/keys/id_ed25519"));

        let err = transport
            .wait_ready(&endpoint("admin"), Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(matches!(err, WargameError::GuestNotReady { .. }));
        assert!(err.to_string().contains("SSH"), "{err}");
    }
}
