//! `WargameProvider`: the challenge-source port (DESIGN.md §3.3, MVP.md §9).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::Secret;
use crate::domain::challenge::{Challenge, ChallengeAsset, ChallengeSummary};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub category: Option<String>,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    pub items: Vec<ChallengeSummary>,
    pub next_page: Option<u32>,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthState {
    Anonymous,
    Authenticated { display: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub reachable: bool,
    pub auth: AuthState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedFile {
    pub filename: String,
    pub path: std::path::PathBuf,
    pub sha256: String,
    pub size: u64,
}

/// One asset may unpack into several files server-side (DESIGN.md §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadResult {
    pub files: Vec<DownloadedFile>,
}

/// A staging-file writer handed to a provider so it never has to know where
/// (or how safely) bytes end up on disk — see `security::staging`.
#[async_trait]
pub trait AssetSink: Send {
    async fn begin(&mut self, server_filename: &str) -> Result<()>;
    async fn write(&mut self, chunk: &[u8]) -> Result<()>;
    async fn finish(&mut self) -> Result<DownloadedFile>;
}

#[async_trait]
pub trait WargameProvider: Send + Sync {
    /// Short provider identifier, e.g. `"dreamhack"`.
    fn name(&self) -> &str;
    async fn search(&self, q: &SearchQuery) -> Result<SearchPage>;
    async fn get_challenge(&self, id: &str) -> Result<Challenge>;
    async fn auth_state(&self) -> Result<AuthState>;
    async fn login(&self, username: &str, password: Secret<String>) -> Result<()>;
    async fn download_asset(
        &self,
        ch: &Challenge,
        a: &ChallengeAsset,
        sink: &mut dyn AssetSink,
    ) -> Result<DownloadResult>;
    async fn healthcheck(&self) -> Result<ProviderStatus>;
}
