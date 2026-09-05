//! Provider-neutral challenge model (MVP.md §8).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Result, WargameError};

/// A stable reference to a challenge, e.g. `dreamhack:11`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChallengeRef {
    pub provider: String,
    pub id: String,
}

impl ChallengeRef {
    /// Parses `"provider:id"`. A bare number (or bare string with no `:`) is
    /// treated as `id` with provider defaulted to `"dreamhack"`.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            return Err(WargameError::Other(
                "challenge reference must not be empty".to_string(),
            ));
        }
        match s.split_once(':') {
            Some((provider, id)) => {
                if provider.trim().is_empty() || id.trim().is_empty() {
                    return Err(WargameError::Other(format!(
                        "invalid challenge reference: {s}"
                    )));
                }
                Ok(ChallengeRef {
                    provider: provider.trim().to_string(),
                    id: id.trim().to_string(),
                })
            }
            None => Ok(ChallengeRef {
                provider: "dreamhack".to_string(),
                id: s.to_string(),
            }),
        }
    }
}

impl fmt::Display for ChallengeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.id)
    }
}

/// Search-result-sized view of a challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeSummary {
    pub r#ref: ChallengeRef,
    pub slug: String,
    pub title: String,
    pub categories: Vec<String>,
    pub difficulty: Option<String>,
}

/// Full challenge detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub summary: ChallengeSummary,
    pub description: String,
    pub tags: Vec<String>,
    pub assets: Vec<ChallengeAsset>,
    pub remote_endpoints: Vec<String>,
    pub source_url: String,
}

/// A provider-opaque downloadable asset attached to a challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeAsset {
    pub id: String,
    pub filename: String,
    pub size: Option<u64>,
    /// Provider-specific data needed to actually perform the download later.
    /// Core never interprets this; only the provider adapter that produced it does.
    pub descriptor: serde_json::Value,
    pub expected_sha256: Option<String>,
}

/// Normalizes an arbitrary title into a filesystem/URL-safe slug.
///
/// Lowercases, keeps `[a-z0-9-]`, collapses runs of separators into a single
/// `-`, trims leading/trailing `-`, and truncates to 40 bytes. Falls back to
/// `"challenge"` if the result would be empty.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_was_dash = false;
    for ch in title.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "challenge".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_and_id() {
        let r = ChallengeRef::parse("dreamhack:11").unwrap();
        assert_eq!(r.provider, "dreamhack");
        assert_eq!(r.id, "11");
    }

    #[test]
    fn parse_bare_number_defaults_provider() {
        let r = ChallengeRef::parse("11").unwrap();
        assert_eq!(r.provider, "dreamhack");
        assert_eq!(r.id, "11");
    }

    #[test]
    fn parse_empty_errors() {
        assert!(ChallengeRef::parse("").is_err());
    }

    #[test]
    fn parse_missing_id_errors() {
        assert!(ChallengeRef::parse("dreamhack:").is_err());
    }

    #[test]
    fn display_round_trips() {
        let r = ChallengeRef::parse("dreamhack:11").unwrap();
        assert_eq!(r.to_string(), "dreamhack:11");
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Out Of Bound!!"), "out-of-bound");
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify("!!!"), "challenge");
        assert_eq!(slugify(""), "challenge");
    }

    #[test]
    fn slugify_truncates_and_trims_trailing_dash() {
        let long = "a".repeat(50);
        let s = slugify(&long);
        assert!(s.len() <= 40);
        assert!(!s.ends_with('-'));
    }
}
