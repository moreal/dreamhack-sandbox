//! `SessionRepository`: local session metadata persistence (DESIGN.md §3.3,
//! §21).

use std::path::PathBuf;

use async_trait::async_trait;

use crate::domain::session::Session;
use crate::error::Result;

#[async_trait]
pub trait SessionRepository: Send + Sync {
    async fn save(&self, s: &Session) -> Result<()>;
    /// Errs with `SessionNotFound` if `id` does not exist.
    async fn load(&self, id: &str) -> Result<Session>;
    /// All known sessions, newest `created_at` first.
    async fn list(&self) -> Result<Vec<Session>>;
    async fn delete(&self, id: &str) -> Result<()>;
    /// Where this session's on-disk artifacts (metadata, logs) live.
    fn session_dir(&self, id: &str) -> PathBuf;
}
