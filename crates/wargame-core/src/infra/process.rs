//! Subprocess execution (DESIGN.md §3.6's "always argv, never a shell
//! string" rule lives here): [`ProcessRunner`] is the seam every adapter
//! that shells out (Tart, ssh/scp, `security`) goes through, so tests can
//! substitute [`crate::testing::FakeRunner`] instead of touching the host.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tracing::debug;

use crate::error::{Result, WargameError};

/// A subprocess invocation: always `program` + argv, never an interpolated
/// shell string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
}

impl Cmd {
    /// Convenience constructor for the common case of no env/stdin/cwd.
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Cmd {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            stdin: None,
            cwd: None,
        }
    }
}

/// Captured output of a completed (non-detached) process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The seam between core/adapters and the operating system's process API.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Runs `cmd` to completion, capturing both streams.
    async fn run(&self, cmd: &Cmd) -> Result<Output>;
    /// Spawns `cmd` detached from this process (own session), appending its
    /// stdout/stderr to `log_path`. Returns the child's pid.
    async fn spawn_detached(&self, cmd: &Cmd, log_path: &Path) -> Result<u32>;
    /// Resolves `program` against `PATH`, if it exists there.
    fn which(&self, program: &str) -> Option<PathBuf>;
    /// Whether a process with this pid currently exists (best-effort).
    fn pid_alive(&self, pid: u32) -> bool;
}

/// The real, OS-backed [`ProcessRunner`].
pub struct RealRunner;

#[async_trait]
impl ProcessRunner for RealRunner {
    async fn run(&self, cmd: &Cmd) -> Result<Output> {
        debug!(program = %cmd.program, args = ?cmd.args, "running command");

        let mut command = tokio::process::Command::new(&cmd.program);
        command.args(&cmd.args);
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        command.stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command
            .spawn()
            .map_err(|e| WargameError::Other(format!("failed to spawn {}: {e}", cmd.program)))?;

        if let Some(input) = &cmd.stdin {
            use tokio::io::AsyncWriteExt;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(input).await.map_err(|e| {
                    WargameError::Other(format!("failed writing stdin to {}: {e}", cmd.program))
                })?;
            }
        }

        let out = child
            .wait_with_output()
            .await
            .map_err(|e| WargameError::Other(format!("failed waiting for {}: {e}", cmd.program)))?;

        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    async fn spawn_detached(&self, cmd: &Cmd, log_path: &Path) -> Result<u32> {
        debug!(program = %cmd.program, args = ?cmd.args, log = %log_path.display(), "spawning detached command");

        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log_out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        let log_err = log_out.try_clone()?;

        let mut command = std::process::Command::new(&cmd.program);
        command.args(&cmd.args);
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::null());
        command.stdout(Stdio::from(log_out));
        command.stderr(Stdio::from(log_err));

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid() is async-signal-safe and takes no arguments
            // that could dangle; this only detaches the child into its own
            // session so it survives this process exiting.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let child = command
            .spawn()
            .map_err(|e| WargameError::Other(format!("failed to spawn {}: {e}", cmd.program)))?;
        Ok(child.id())
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let candidate = Path::new(program);
        if candidate.is_absolute() || program.contains('/') {
            return candidate.is_file().then(|| candidate.to_path_buf());
        }
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(program))
            .find(|p| p.is_file())
    }

    fn pid_alive(&self, pid: u32) -> bool {
        #[cfg(unix)]
        {
            // kill(pid, 0) performs no signal delivery, only existence/permission checks.
            unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_captures_stdout_and_status() {
        let runner = RealRunner;
        let cmd = Cmd::new("true", Vec::<String>::new());
        let out = runner.run(&cmd).await.unwrap();
        assert_eq!(out.status, 0);
    }

    #[tokio::test]
    async fn run_feeds_stdin() {
        let runner = RealRunner;
        let mut cmd = Cmd::new("cat", Vec::<String>::new());
        cmd.stdin = Some(b"hello from stdin".to_vec());
        let out = runner.run(&cmd).await.unwrap();
        assert_eq!(out.stdout, "hello from stdin");
    }

    #[test]
    fn which_finds_a_common_binary() {
        let runner = RealRunner;
        assert!(runner.which("sh").is_some());
        assert!(runner.which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn pid_alive_true_for_self() {
        let runner = RealRunner;
        assert!(runner.pid_alive(std::process::id()));
    }
}
