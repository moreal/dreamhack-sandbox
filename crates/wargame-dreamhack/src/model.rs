//! Serde structs mirroring the Dreamhack API responses we actually use
//! (DESIGN.md §4). Only fields the adapter needs are declared; everything
//! else is dropped by serde's default "ignore unknown fields" behavior.

use serde::{Deserialize, Serialize};

/// One entry of `GET /api/v1/wargame/challenges/?search=...`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeListItem {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tier_display: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub needs_vm: bool,
    #[serde(default)]
    pub is_public: bool,
}

/// `GET /api/v1/wargame/challenges/?search=...` response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub count: u64,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub previous: Option<String>,
    #[serde(default)]
    pub results: Vec<ChallengeListItem>,
}

/// `GET /api/v1/wargame/challenges/<id>/` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeDetail {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tier_display: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub needs_vm: bool,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub description: String,
}
