//! Subprocess execution port (DESIGN.md §3.3/§3.6): every external tool
//! (`tart`, `ssh`, `scp`, `security`, …) is invoked through this trait as an
//! argv array, never a shell string, so a secret handed through `stdin` or
//! `env` can never leak into argv, logs, or session metadata.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::{Result, WargameError};

/// One subprocess invocation. `program`/`args` are always separate argv
/// entries (DESIGN.md §3.6): callers must never build a shell string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
}

impl Cmd {
    pub fn new<S, I, A>(program: S, args: I) -> Self
    where
        S: Into<String>,
        I: IntoIterator<Item = A>,
        A: Into<String>,
    {
        Cmd {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            stdin: None,
            cwd: None,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs external processes. Implementations must never build a shell string
/// (`Command::new(bin).args([...])` only) and must never place a secret in
/// argv (use `Cmd::env`/`Cmd::stdin` instead).
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Runs `cmd` to completion, capturing both output streams.
    async fn run(&self, cmd: &Cmd) -> Result<Output>;
    /// Starts `cmd` detached in a new session, appending its stdout+stderr
    /// to `log_path`; returns its pid without waiting for it to exit.
    async fn spawn_detached(&self, cmd: &Cmd, log_path: &Path) -> Result<u32>;
    /// Resolves `program` against `PATH` (or returns it directly if it is
    /// already a path that exists), without running it.
    fn which(&self, program: &str) -> Option<PathBuf>;
    /// Whether a process with this pid is still alive.
    fn pid_alive(&self, pid: u32) -> bool;
}

/// The real `ProcessRunner`, backed by `tokio::process::Command`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRunner;

impl RealRunner {
    fn build(cmd: &Cmd) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(&cmd.program);
        c.args(&cmd.args);
        for (k, v) in &cmd.env {
            c.env(k, v);
        }
        if let Some(cwd) = &cmd.cwd {
            c.current_dir(cwd);
        }
        c
    }
}

#[async_trait]
impl ProcessRunner for RealRunner {
    async fn run(&self, cmd: &Cmd) -> Result<Output> {
        tracing::debug!(program = %cmd.program, args = ?cmd.args, "running command");
        let mut c = Self::build(cmd);
        c.stdout(std::process::Stdio::piped());
        c.stderr(std::process::Stdio::piped());
        c.stdin(if cmd.stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
        let mut child = c
            .spawn()
            .map_err(|e| WargameError::Other(format!("failed to spawn {}: {e}", cmd.program)))?;
        if let Some(bytes) = &cmd.stdin {
            use tokio::io::AsyncWriteExt;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(bytes).await?;
                drop(stdin);
            }
        }
        let output = child.wait_with_output().await?;
        Ok(Output {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn spawn_detached(&self, cmd: &Cmd, log_path: &Path) -> Result<u32> {
        tracing::debug!(program = %cmd.program, args = ?cmd.args, log = %log_path.display(), "spawning detached command");
        let log_out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        let log_err = log_out.try_clone()?;
        let mut c = Self::build(cmd);
        c.stdin(std::process::Stdio::null());
        c.stdout(std::process::Stdio::from(log_out));
        c.stderr(std::process::Stdio::from(log_err));
        #[cfg(unix)]
        {
            // `tokio::process::Command` exposes `pre_exec` directly on
            // unix (no trait import needed to call it).
            // Safety: `setsid` is async-signal-safe and takes no arguments;
            // it detaches the child into its own session so it survives us.
            unsafe {
                c.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        let child = c
            .spawn()
            .map_err(|e| WargameError::Other(format!("failed to spawn {}: {e}", cmd.program)))?;
        let pid = child
            .id()
            .ok_or_else(|| WargameError::Other("spawned process has no pid".to_string()))?;
        // Detached: we intentionally never `.wait()` on `child`. Tokio does
        // not send a kill signal on drop unless `kill_on_drop(true)` was set
        // (it wasn't), so the process keeps running independently.
        drop(child);
        Ok(pid)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            let p = PathBuf::from(program);
            return p.is_file().then_some(p);
        }
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var).find_map(|dir| {
            let candidate = dir.join(program);
            candidate.is_file().then_some(candidate)
        })
    }

    fn pid_alive(&self, pid: u32) -> bool {
        // Safety: `kill(pid, 0)` sends no signal; it only probes existence
        // and permission, which is what we want here.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn real_runner_runs_true_and_false() {
        let runner = RealRunner;
        let ok = runner
            .run(&Cmd::new("true", Vec::<String>::new()))
            .await
            .unwrap();
        assert_eq!(ok.status, 0);
        let bad = runner
            .run(&Cmd::new("false", Vec::<String>::new()))
            .await
            .unwrap();
        assert_ne!(bad.status, 0);
    }

    #[tokio::test]
    async fn real_runner_captures_stdout_and_stdin() {
        let runner = RealRunner;
        let cmd = Cmd::new("cat", Vec::<String>::new()).with_stdin(b"hello".to_vec());
        let out = runner.run(&cmd).await.unwrap();
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn which_finds_a_binary_on_path() {
        let runner = RealRunner;
        // `sh` is present on every unix CI/dev host we run tests on.
        assert!(runner.which("sh").is_some());
        assert!(runner.which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn pid_alive_is_true_for_self() {
        let runner = RealRunner;
        assert!(runner.pid_alive(std::process::id()));
    }
}
