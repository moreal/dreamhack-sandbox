//! `impl WargameProvider for DreamhackProvider` (DESIGN.md §4).

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::cookie::Jar;
use url::Url;
use wargame_core::domain::challenge::{Challenge, ChallengeAsset};
use wargame_core::error::Result;
use wargame_core::ports::{
    AssetSink, AuthState, CredentialStore, DownloadResult, ProviderStatus, SearchPage, SearchQuery,
    Secret, WargameProvider,
};

use crate::client;
use crate::{api, auth, download, normalize};

/// The account key `DreamhackProvider` reads/writes on the [`CredentialStore`]
/// (DESIGN.md §3.6): a JSON blob of the persisted cookie jar.
pub const CREDENTIAL_KEY: &str = "dreamhack.session";

#[derive(Debug, Clone)]
pub struct DreamhackConfig {
    pub base_url: Url,
    pub user_agent: String,
}

impl Default for DreamhackConfig {
    fn default() -> Self {
        DreamhackConfig {
            base_url: Url::parse("https://dreamhack.io").expect("static url"),
            user_agent: format!("wargame-vm/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

pub struct DreamhackProvider {
    client: reqwest::Client,
    jar: Arc<Jar>,
    cfg: DreamhackConfig,
    store: Arc<dyn CredentialStore>,
}

impl DreamhackProvider {
    pub fn new(cfg: DreamhackConfig, store: Arc<dyn CredentialStore>) -> Result<Self> {
        let jar = client::load_jar(store.as_ref(), &cfg.base_url)?;
        let http = client::build_http_client(jar.clone(), &cfg.user_agent)?;
        Ok(DreamhackProvider {
            client: http,
            jar,
            cfg,
            store,
        })
    }
}

#[async_trait]
impl WargameProvider for DreamhackProvider {
    fn name(&self) -> &str {
        "dreamhack"
    }

    async fn search(&self, q: &SearchQuery) -> Result<SearchPage> {
        let resp = api::search(&self.client, &self.cfg.base_url, q).await?;
        Ok(normalize::search_page(&resp))
    }

    async fn get_challenge(&self, id: &str) -> Result<Challenge> {
        let detail = api::detail(&self.client, &self.cfg.base_url, id).await?;
        Ok(normalize::challenge(self.cfg.base_url.as_str(), &detail))
    }

    async fn auth_state(&self) -> Result<AuthState> {
        match client::cookie_value(&self.jar, &self.cfg.base_url, "sessionid") {
            Some(_) => Ok(AuthState::Authenticated {
                display: "session cookie".to_string(),
            }),
            None => Ok(AuthState::Anonymous),
        }
    }

    async fn login(&self, username: &str, password: Secret<String>) -> Result<()> {
        auth::login(
            &self.client,
            &self.jar,
            &self.cfg.base_url,
            self.store.as_ref(),
            username,
            password,
        )
        .await
    }

    async fn download_asset(
        &self,
        ch: &Challenge,
        a: &ChallengeAsset,
        sink: &mut dyn AssetSink,
    ) -> Result<DownloadResult> {
        let challenge_id = a
            .descriptor
            .get("challenge_id")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| ch.summary.r#ref.id.clone());
        download::download_asset(
            &self.client,
            &self.jar,
            &self.cfg.base_url,
            &challenge_id,
            Some(a.filename.as_str()),
            sink,
        )
        .await
    }

    async fn healthcheck(&self) -> Result<ProviderStatus> {
        let q = SearchQuery {
            text: String::new(),
            category: None,
            page: 1,
            page_size: 1,
        };
        let auth = self.auth_state().await?;
        match api::search(&self.client, &self.cfg.base_url, &q).await {
            Ok(_) => Ok(ProviderStatus {
                reachable: true,
                auth,
                detail: "reachable".to_string(),
            }),
            Err(e) => Ok(ProviderStatus {
                reachable: false,
                auth,
                detail: e.to_string(),
            }),
        }
    }
}
