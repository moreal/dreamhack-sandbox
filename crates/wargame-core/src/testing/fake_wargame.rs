//! An in-memory [`WargameProvider`] serving a single fixture challenge whose
//! one asset is the bytes `b"PK\x03\x04fake"` (a minimal fake zip header).

use std::sync::Mutex;

use async_trait::async_trait;

use crate::domain::challenge::{Challenge, ChallengeAsset, ChallengeRef, ChallengeSummary};
use crate::error::{Result, WargameError};
use crate::ports::wargame_provider::{
    AssetSink, AuthState, DownloadResult, ProviderStatus, SearchPage, SearchQuery, WargameProvider,
};
use crate::ports::Secret;

pub const FIXTURE_ASSET_BYTES: &[u8] = b"PK\x03\x04fake";

pub struct FakeWargameProvider {
    name: String,
    auth: Mutex<AuthState>,
    fail_on: Mutex<Option<&'static str>>,
    calls: Mutex<Vec<String>>,
}

impl FakeWargameProvider {
    pub fn new() -> Self {
        FakeWargameProvider {
            name: "dreamhack".to_string(),
            auth: Mutex::new(AuthState::Anonymous),
            fail_on: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn set_authenticated(&self, display: impl Into<String>) {
        *self.auth.lock().unwrap() = AuthState::Authenticated {
            display: display.into(),
        };
    }

    /// Steps: `"get_challenge"`, `"download_asset"`.
    pub fn set_fail_on(&self, step: &'static str) {
        *self.fail_on.lock().unwrap() = Some(step);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn should_fail(&self, step: &str) -> bool {
        self.fail_on
            .lock()
            .unwrap()
            .map(|s| s == step)
            .unwrap_or(false)
    }

    /// The one challenge this fake ever serves, `<name>:1`.
    pub fn fixture_challenge(&self) -> Challenge {
        let r#ref = ChallengeRef {
            provider: self.name.clone(),
            id: "1".to_string(),
        };
        let summary = ChallengeSummary {
            r#ref,
            slug: "hello-fixture".to_string(),
            title: "hello-fixture".to_string(),
            categories: vec!["misc".to_string()],
            difficulty: Some("Bronze 1".to_string()),
        };
        Challenge {
            summary: summary.clone(),
            description: "A fixture challenge used by tests.".to_string(),
            tags: vec!["fixture".to_string()],
            assets: vec![ChallengeAsset {
                id: "1".to_string(),
                filename: "hello-fixture.zip".to_string(),
                size: Some(FIXTURE_ASSET_BYTES.len() as u64),
                descriptor: serde_json::json!({"kind": "fixture"}),
                expected_sha256: None,
            }],
            remote_endpoints: vec![],
            source_url: "https://example.invalid/fixture/1".to_string(),
        }
    }
}

impl Default for FakeWargameProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WargameProvider for FakeWargameProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn search(&self, _q: &SearchQuery) -> Result<SearchPage> {
        self.calls.lock().unwrap().push("search".to_string());
        let challenge = self.fixture_challenge();
        Ok(SearchPage {
            items: vec![challenge.summary],
            next_page: None,
            total: 1,
        })
    }

    async fn get_challenge(&self, id: &str) -> Result<Challenge> {
        self.calls.lock().unwrap().push("get_challenge".to_string());
        if self.should_fail("get_challenge") {
            return Err(WargameError::ChallengeNotFound(id.to_string()));
        }
        let mut challenge = self.fixture_challenge();
        challenge.summary.r#ref.id = id.to_string();
        Ok(challenge)
    }

    async fn auth_state(&self) -> Result<AuthState> {
        Ok(self.auth.lock().unwrap().clone())
    }

    async fn login(&self, _username: &str, _password: Secret<String>) -> Result<()> {
        self.set_authenticated("fixture-user");
        Ok(())
    }

    async fn download_asset(
        &self,
        _ch: &Challenge,
        a: &ChallengeAsset,
        sink: &mut dyn AssetSink,
    ) -> Result<DownloadResult> {
        self.calls
            .lock()
            .unwrap()
            .push("download_asset".to_string());
        if self.should_fail("download_asset") {
            return Err(WargameError::DownloadFailed {
                filename: a.filename.clone(),
                reason: "fake download failure".to_string(),
            });
        }
        sink.begin(&a.filename).await?;
        sink.write(FIXTURE_ASSET_BYTES).await?;
        let file = sink.finish().await?;
        Ok(DownloadResult { files: vec![file] })
    }

    async fn healthcheck(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            reachable: true,
            auth: self.auth.lock().unwrap().clone(),
            detail: "ok".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::StagingDir;

    #[tokio::test]
    async fn search_returns_the_fixture_challenge() {
        let provider = FakeWargameProvider::new();
        let page = provider
            .search(&SearchQuery {
                text: "hello".to_string(),
                category: None,
                page: 1,
                page_size: 20,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].r#ref.id, "1");
    }

    #[tokio::test]
    async fn anonymous_by_default_then_authenticated_after_login() {
        let provider = FakeWargameProvider::new();
        assert_eq!(provider.auth_state().await.unwrap(), AuthState::Anonymous);
        provider
            .login("user", Secret::new(Box::new("pw".to_string())))
            .await
            .unwrap();
        assert!(matches!(
            provider.auth_state().await.unwrap(),
            AuthState::Authenticated { .. }
        ));
    }

    #[tokio::test]
    async fn download_asset_writes_the_fixture_bytes() {
        let provider = FakeWargameProvider::new();
        let challenge = provider.fixture_challenge();
        let dir = tempfile::tempdir().unwrap();
        let staging = StagingDir::create(dir.path(), true).unwrap();
        let mut sink = staging.sink("1");

        let result = provider
            .download_asset(&challenge, &challenge.assets[0], &mut sink)
            .await
            .unwrap();
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].size, FIXTURE_ASSET_BYTES.len() as u64);
        let bytes = std::fs::read(&result.files[0].path).unwrap();
        assert_eq!(bytes, FIXTURE_ASSET_BYTES);
    }
}
