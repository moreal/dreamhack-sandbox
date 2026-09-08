//! `ssh`/`scp`-backed [`GuestTransport`] (DESIGN.md §3.3, §3.6).
//!
//! Every invocation goes through [`ProcessRunner`] with an explicit argv —
//! no shell strings, and the same hardened options every time (host key
//! checking off, since guests are disposable; short connect timeout; batch
//! mode so a stuck credential prompt cannot hang the CLI).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::domain::session::DEFAULT_GUEST_PORT;
use crate::error::{Result, WargameError};
use crate::infra::process::{Cmd, ProcessRunner};
use crate::ports::guest_transport::{ExecResult, GuestEndpoint, GuestTransport};

const WAIT_READY_POLL: Duration = Duration::from_secs(3);

/// The POSIX-guest SSH transport. The user, port and key come from the
/// [`GuestEndpoint`] the caller passes -- the profile's `guest.user` at
/// launch, the session's saved connection afterwards -- so nothing here
/// assumes one host-wide guest account (spec.md §3 FR-4).
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
}

#[async_trait]
impl GuestTransport for SshGuestTransport {
    async fn wait_ready(&self, endpoint: &GuestEndpoint, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut args = self.base_args(endpoint, "-p");
            args.push(self.target(endpoint));
            args.push("true".to_string());
            let cmd = Cmd::new("ssh", args);
            if let Ok(out) = self.runner.run(&cmd).await {
                if out.status == 0 {
                    return Ok(());
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(WargameError::GuestBootTimeout(timeout));
            }
            tokio::time::sleep((deadline - now).min(WAIT_READY_POLL)).await;
        }
    }

    async fn exec(&self, endpoint: &GuestEndpoint, argv: &[String]) -> Result<ExecResult> {
        let mut args = self.base_args(endpoint, "-p");
        args.push(self.target(endpoint));
        args.push("--".to_string());
        let remote_command = argv
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        args.push(remote_command);

        let cmd = Cmd::new("ssh", args);
        let out = self.runner.run(&cmd).await?;
        Ok(ExecResult {
            status: out.status,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    async fn copy_in(&self, endpoint: &GuestEndpoint, src: &Path, dest: &str) -> Result<()> {
        // `scp` spells the port `-P`, not `-p` (which means "preserve times").
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
        cmd
    }
}

/// POSIX single-quote escaping: wraps `s` in single quotes, replacing any
/// embedded `'` with `'\''` (close quote, escaped literal quote, reopen
/// quote). Only correct for [`crate::domain::GuestShell::Posix`] endpoints;
/// a PowerShell guest needs its own transport (spec.md §3 FR-4), which
/// [`crate::app::backends::TransportRegistry`] selects instead of this one.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
        assert!(matches!(err, WargameError::GuestBootTimeout(_)));
    }
}
