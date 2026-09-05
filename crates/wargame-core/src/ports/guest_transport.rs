//! `GuestTransport`: how the host talks to a running guest (DESIGN.md §3.3).

use std::net::IpAddr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait GuestTransport: Send + Sync {
    /// Blocks until the guest accepts commands, or `timeout` elapses
    /// (`GuestBootTimeout`).
    async fn wait_ready(&self, ip: IpAddr, timeout: Duration) -> Result<()>;
    async fn exec(&self, ip: IpAddr, argv: &[String]) -> Result<ExecResult>;
    /// Copies `src` (host path) into `dest` (a guest path) recursively.
    async fn copy_in(&self, ip: IpAddr, src: &Path, dest: &str) -> Result<()>;
    /// Builds (but does not spawn) an interactive shell command for
    /// `wargame ssh`; the caller `exec`s it so it replaces the current
    /// process's terminal.
    fn interactive_shell_cmd(&self, ip: IpAddr) -> Command;
}
