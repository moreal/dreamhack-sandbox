//! `GuestTransport`: how the host talks to a running guest (DESIGN.md §3.3).

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::Result;

pub use crate::domain::session::{GuestEndpoint, GuestShell};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Every method is keyed by a [`GuestEndpoint`] rather than a bare address:
/// the user, port, key and shell all come from the profile at launch and
/// from the saved session afterwards, so nothing here may assume a
/// host-wide default user (spec.md §3 FR-4).
#[async_trait]
pub trait GuestTransport: Send + Sync {
    /// Blocks until the guest accepts commands, or `timeout` elapses
    /// (`GuestBootTimeout`).
    async fn wait_ready(&self, endpoint: &GuestEndpoint, timeout: Duration) -> Result<()>;
    async fn exec(&self, endpoint: &GuestEndpoint, argv: &[String]) -> Result<ExecResult>;
    /// Copies `src` (host path) into `dest` (a guest path) recursively.
    async fn copy_in(&self, endpoint: &GuestEndpoint, src: &Path, dest: &str) -> Result<()>;
    /// Builds (but does not spawn) an interactive shell command for
    /// `wargame ssh`; the caller `exec`s it so it replaces the current
    /// process's terminal.
    fn interactive_shell_cmd(&self, endpoint: &GuestEndpoint) -> Command;
}
