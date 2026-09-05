//! `ssh`/`scp`-backed [`GuestTransport`] (DESIGN.md §3.3, §3.6).
//!
//! Every invocation goes through [`ProcessRunner`] with an explicit argv —
//! no shell strings, and the same hardened options every time (host key
//! checking off, since guests are disposable; short connect timeout; batch
//! mode so a stuck credential prompt cannot hang the CLI).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::error::{Result, WargameError};
use crate::infra::process::{Cmd, ProcessRunner};
use crate::ports::guest_transport::{ExecResult, GuestTransport};

const WAIT_READY_POLL: Duration = Duration::from_secs(3);

pub struct SshGuestTransport {
    runner: Arc<dyn ProcessRunner>,
    user: String,
    key_path: PathBuf,
}

impl SshGuestTransport {
    pub fn new(runner: Arc<dyn ProcessRunner>, user: impl Into<String>, key_path: PathBuf) -> Self {
        SshGuestTransport {
            runner,
            user: user.into(),
            key_path,
        }
    }

    /// The `-i <key>`, `StrictHostKeyChecking=no`, etc. options common to
    /// every `ssh`/`scp` invocation.
    fn base_args(&self) -> Vec<String> {
        vec![
            "-i".to_string(),
            self.key_path.display().to_string(),
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
        ]
    }

    fn target(&self, ip: IpAddr) -> String {
        format!("{}@{}", self.user, ip)
    }
}

#[async_trait]
impl GuestTransport for SshGuestTransport {
    async fn wait_ready(&self, ip: IpAddr, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut args = self.base_args();
            args.push(self.target(ip));
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

    async fn exec(&self, ip: IpAddr, argv: &[String]) -> Result<ExecResult> {
        let mut args = self.base_args();
        args.push(self.target(ip));
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

    async fn copy_in(&self, ip: IpAddr, src: &Path, dest: &str) -> Result<()> {
        let mut args = self.base_args();
        args.push("-r".to_string());
        args.push(src.display().to_string());
        args.push(format!("{}:{}", self.target(ip), dest));

        let cmd = Cmd::new("scp", args);
        let out = self.runner.run(&cmd).await?;
        if out.status != 0 {
            return Err(WargameError::CopyFailed(out.stderr));
        }
        Ok(())
    }

    fn interactive_shell_cmd(&self, ip: IpAddr) -> StdCommand {
        let mut cmd = StdCommand::new("ssh");
        cmd.arg("-t");
        for arg in self.base_args() {
            cmd.arg(arg);
        }
        cmd.arg(self.target(ip));
        cmd
    }
}

/// POSIX single-quote escaping: wraps `s` in single quotes, replacing any
/// embedded `'` with `'\''` (close quote, escaped literal quote, reopen
/// quote).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeRunner;
    use std::str::FromStr;

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[tokio::test]
    async fn exec_argv_has_base_options() {
        let runner = Arc::new(FakeRunner::new());
        let transport =
            SshGuestTransport::new(runner.clone(), "admin", PathBuf::from("/keys/id_ed25519"));
        let ip = IpAddr::from_str("10.0.0.5").unwrap();

        transport
            .exec(ip, &["echo".to_string(), "hi there".to_string()])
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
        assert!(cmd.args.last().unwrap().contains("echo"));
        assert!(cmd.args.last().unwrap().contains("'hi there'"));
    }

    #[tokio::test]
    async fn copy_in_uses_scp_with_recursive_flag() {
        let runner = Arc::new(FakeRunner::new());
        let transport =
            SshGuestTransport::new(runner.clone(), "admin", PathBuf::from("/keys/id_ed25519"));
        let ip = IpAddr::from_str("10.0.0.5").unwrap();
        let src = PathBuf::from("/tmp/asset.zip");

        transport
            .copy_in(ip, &src, "~/wargame/incoming/")
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
        let transport =
            SshGuestTransport::new(runner.clone(), "admin", PathBuf::from("/keys/id_ed25519"));
        let ip = IpAddr::from_str("10.0.0.5").unwrap();

        transport
            .wait_ready(ip, Duration::from_secs(30))
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
        let transport =
            SshGuestTransport::new(runner.clone(), "admin", PathBuf::from("/keys/id_ed25519"));
        let ip = IpAddr::from_str("10.0.0.5").unwrap();

        let err = transport
            .wait_ready(ip, Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(matches!(err, WargameError::GuestBootTimeout(_)));
    }
}
