//! Login flow (DESIGN.md §4): `GET /` for a `csrftoken` cookie, then
//! `POST /api/v1/auth/login/`; on success the resulting cookie jar
//! (session cookie included) is persisted to the [`CredentialStore`].

use reqwest::cookie::Jar;
use reqwest::StatusCode;
use secrecy::ExposeSecret;
use serde_json::json;
use url::Url;
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::{CredentialStore, Secret};

use crate::client::{cookie_value, persist_jar};

const PROVIDER: &str = "dreamhack";

/// Runs the full login flow and, on success, persists the resulting cookies
/// (including `sessionid`) to `store` under [`crate::CREDENTIAL_KEY`].
pub async fn login(
    client: &reqwest::Client,
    jar: &Jar,
    base: &Url,
    store: &dyn CredentialStore,
    email: &str,
    password: Secret<String>,
) -> Result<()> {
    // Best-effort: prime the CSRF cookie. Django sets it on any GET; if this
    // fails we still attempt the POST (some deployments seed it elsewhere).
    let _ = client.get(base.clone()).send().await;

    let login_url = base
        .join("/api/v1/auth/login/")
        .map_err(|e| unavailable(e.to_string()))?;
    let csrf = cookie_value(jar, base, "csrftoken");

    let mut req = client
        .post(login_url)
        .header("Referer", base.as_str())
        .json(&json!({ "email": email, "password": password.expose_secret() }));
    if let Some(csrf) = &csrf {
        req = req.header("X-CSRFToken", csrf);
    }

    let resp = req.send().await.map_err(|e| unavailable(e.to_string()))?;
    let status = resp.status();
    if status.is_success() {
        persist_jar(store, jar, base)?;
        return Ok(());
    }

    let body = resp.text().await.unwrap_or_default();
    let reason = extract_detail(&body).unwrap_or_else(|| {
        if status == StatusCode::UNAUTHORIZED {
            "invalid credentials".to_string()
        } else {
            format!("HTTP {status}")
        }
    });
    Err(WargameError::AuthenticationFailed { reason })
}

fn extract_detail(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("detail")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn unavailable(reason: impl Into<String>) -> WargameError {
    WargameError::ProviderUnavailable {
        provider: PROVIDER.to_string(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_detail_from_401_fixture() {
        let body = include_str!("../fixtures/login_401.json");
        assert_eq!(
            extract_detail(body),
            Some("Failed to login with given credentials.".to_string())
        );
    }
}
