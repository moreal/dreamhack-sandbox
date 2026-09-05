//! Raw Dreamhack endpoint calls, returning [`crate::model`] structs
//! (DESIGN.md §4). Response-shape interpretation lives in `normalize.rs`;
//! this module only knows HTTP.

use reqwest::{Response, StatusCode};
use url::Url;
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::SearchQuery;

use crate::model::{ChallengeDetail, SearchResponse};

const PROVIDER: &str = "dreamhack";

fn unavailable(reason: impl Into<String>) -> WargameError {
    WargameError::ProviderUnavailable {
        provider: PROVIDER.to_string(),
        reason: reason.into(),
    }
}

fn changed(what: impl Into<String>) -> WargameError {
    WargameError::ProviderChanged {
        provider: PROVIDER.to_string(),
        what: what.into(),
    }
}

fn join(base: &Url, path: &str) -> Result<Url> {
    base.join(path)
        .map_err(|e| unavailable(format!("bad url {path}: {e}")))
}

/// `GET /api/v1/wargame/challenges/?search=...&page=...&page_size=...[&category=...]`.
pub async fn search(
    client: &reqwest::Client,
    base: &Url,
    q: &SearchQuery,
) -> Result<SearchResponse> {
    let mut url = join(base, "/api/v1/wargame/challenges/")?;
    {
        let mut qp = url.query_pairs_mut();
        if !q.text.is_empty() {
            qp.append_pair("search", &q.text);
        }
        if let Some(category) = &q.category {
            qp.append_pair("category", category);
        }
        qp.append_pair("page", &q.page.to_string());
        qp.append_pair("page_size", &q.page_size.to_string());
    }
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| unavailable(e.to_string()))?;
    parse_json(resp, "search response shape").await
}

/// `GET /api/v1/wargame/challenges/<id>/`.
pub async fn detail(client: &reqwest::Client, base: &Url, id: &str) -> Result<ChallengeDetail> {
    let url = join(base, &format!("/api/v1/wargame/challenges/{id}/"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| unavailable(e.to_string()))?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Err(WargameError::ChallengeNotFound(id.to_string()));
    }
    parse_json(resp, "detail response shape").await
}

async fn parse_json<T: serde::de::DeserializeOwned>(resp: Response, what: &str) -> Result<T> {
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(WargameError::AuthenticationRequired {
            provider: PROVIDER.to_string(),
        });
    }
    if !status.is_success() {
        return Err(unavailable(format!("HTTP {status}")));
    }
    let bytes = resp.bytes().await.map_err(|e| unavailable(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|_| changed(what.to_string()))
}
