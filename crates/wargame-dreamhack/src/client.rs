//! `reqwest::Client` construction, cookie jar plumbing, and
//! `CredentialStore` <-> cookie-jar (de)serialization (DESIGN.md §4, §3.6).

use std::sync::Arc;
use std::time::Duration;

use reqwest::cookie::{CookieStore, Jar};
use reqwest::Client;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use url::Url;
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::CredentialStore as CoreCredentialStore;
use wargame_core::ports::Secret;

use crate::CREDENTIAL_KEY;

/// One cookie as persisted in the credential store (DESIGN.md §3.6's
/// `{cookies: [{name,value,domain,path}], created_at}` blob).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub cookies: Vec<StoredCookie>,
    pub created_at: u64,
}

fn provider_err(reason: String) -> WargameError {
    WargameError::ProviderUnavailable {
        provider: "dreamhack".to_string(),
        reason,
    }
}

/// Builds the shared `reqwest::Client`: rustls TLS, a 30s timeout, the given
/// user agent, and cookies routed through `jar`.
pub fn build_http_client(jar: Arc<Jar>, user_agent: &str) -> Result<Client> {
    Client::builder()
        .cookie_provider(jar)
        .user_agent(user_agent.to_string())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| provider_err(e.to_string()))
}

/// Reads whatever session was persisted under [`CREDENTIAL_KEY`] and seeds a
/// fresh cookie jar scoped to `base_url` with it. A missing or unparsable
/// stored value simply yields an empty jar (anonymous session).
pub fn load_jar(store: &dyn CoreCredentialStore, base_url: &Url) -> Result<Arc<Jar>> {
    let jar = Arc::new(Jar::default());
    if let Some(secret) = store
        .get(CREDENTIAL_KEY)
        .map_err(|e| WargameError::CredentialStore(e.to_string()))?
    {
        if let Ok(stored) = serde_json::from_str::<StoredSession>(secret.expose_secret()) {
            for c in &stored.cookies {
                jar.add_cookie_str(&format!("{}={}", c.name, c.value), base_url);
            }
        }
    }
    Ok(jar)
}

/// Serializes every cookie the jar holds for `base_url` and writes it to the
/// credential store under [`CREDENTIAL_KEY`].
pub fn persist_jar(store: &dyn CoreCredentialStore, jar: &Jar, base_url: &Url) -> Result<()> {
    let domain = base_url.host_str().unwrap_or("").to_string();
    let cookies = cookie_pairs(jar, base_url)
        .into_iter()
        .map(|(name, value)| StoredCookie {
            name,
            value,
            domain: domain.clone(),
            path: "/".to_string(),
        })
        .collect();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stored = StoredSession {
        cookies,
        created_at,
    };
    let json = serde_json::to_string(&stored).map_err(|e| provider_err(e.to_string()))?;
    store
        .set(CREDENTIAL_KEY, &Secret::new(Box::new(json)))
        .map_err(|e| WargameError::CredentialStore(e.to_string()))
}

/// Every `(name, value)` cookie pair the jar holds for `url`, in
/// `Cookie:`-header order.
pub fn cookie_pairs(jar: &Jar, url: &Url) -> Vec<(String, String)> {
    let Some(header) = jar.cookies(url) else {
        return Vec::new();
    };
    let Ok(s) = header.to_str() else {
        return Vec::new();
    };
    s.split(';')
        .filter_map(|part| {
            let part = part.trim();
            part.split_once('=')
                .map(|(n, v)| (n.to_string(), v.to_string()))
        })
        .collect()
}

/// The value of a single named cookie the jar holds for `url`, if any.
pub fn cookie_value(jar: &Jar, url: &Url, name: &str) -> Option<String> {
    cookie_pairs(jar, url)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
}

/// Cookie names Dreamhack may use for the CSRF token, most specific first.
/// Live dreamhack.io sets `csrf_token` (verified 2026-09-06); `csrftoken`
/// is Django's default and is kept as a fallback.
pub const CSRF_COOKIE_NAMES: &[&str] = &["csrf_token", "csrftoken"];

/// The CSRF token to echo back in `X-CSRFToken`, from whichever of
/// [`CSRF_COOKIE_NAMES`] the jar holds for `url`.
pub fn csrf_token(jar: &Jar, url: &Url) -> Option<String> {
    CSRF_COOKIE_NAMES
        .iter()
        .find_map(|name| cookie_value(jar, url, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_token_prefers_dreamhacks_csrf_token_cookie() {
        let url = Url::parse("https://dreamhack.io").unwrap();
        let jar = Jar::default();
        assert_eq!(csrf_token(&jar, &url), None);
        jar.add_cookie_str("csrftoken=django", &url);
        assert_eq!(csrf_token(&jar, &url), Some("django".to_string()));
        jar.add_cookie_str("csrf_token=live", &url);
        assert_eq!(csrf_token(&jar, &url), Some("live".to_string()));
    }

    #[test]
    fn cookie_value_reads_from_jar() {
        let url = Url::parse("https://dreamhack.io").unwrap();
        let jar = Jar::default();
        jar.add_cookie_str("csrftoken=abc123", &url);
        jar.add_cookie_str("sessionid=xyz789", &url);
        assert_eq!(
            cookie_value(&jar, &url, "csrftoken"),
            Some("abc123".to_string())
        );
        assert_eq!(
            cookie_value(&jar, &url, "sessionid"),
            Some("xyz789".to_string())
        );
        assert_eq!(cookie_value(&jar, &url, "missing"), None);
    }

    #[test]
    fn load_jar_round_trips_persist_jar() {
        struct MemStore(std::sync::Mutex<std::collections::HashMap<String, String>>);
        impl CoreCredentialStore for MemStore {
            fn get(&self, key: &str) -> Result<Option<Secret<String>>> {
                Ok(self
                    .0
                    .lock()
                    .unwrap()
                    .get(key)
                    .cloned()
                    .map(|v| Secret::new(Box::new(v))))
            }
            fn set(&self, key: &str, value: &Secret<String>) -> Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), value.expose_secret().clone());
                Ok(())
            }
            fn delete(&self, key: &str) -> Result<()> {
                self.0.lock().unwrap().remove(key);
                Ok(())
            }
        }

        let store = MemStore(std::sync::Mutex::new(std::collections::HashMap::new()));
        let url = Url::parse("https://dreamhack.io").unwrap();
        let jar = Jar::default();
        jar.add_cookie_str("sessionid=xyz789", &url);
        persist_jar(&store, &jar, &url).unwrap();

        let loaded = load_jar(&store, &url).unwrap();
        assert_eq!(
            cookie_value(&loaded, &url, "sessionid"),
            Some("xyz789".to_string())
        );
    }
}
