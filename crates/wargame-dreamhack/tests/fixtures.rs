//! Parsing/normalization tests against the sanitized fixtures under
//! `fixtures/` (Task 3, PLAN.md Step 1). No live network involved.

use wargame_dreamhack::model::{ChallengeDetail, SearchResponse};
use wargame_dreamhack::normalize;

const SEARCH: &str = include_str!("../fixtures/search.json");
const SEARCH_PAGE: &str = include_str!("../fixtures/search_page.json");
const DETAIL: &str = include_str!("../fixtures/detail.json");
const LOGIN_401: &str = include_str!("../fixtures/login_401.json");
const DOWNLOAD_401: &str = include_str!("../fixtures/download_401.json");

#[test]
fn search_fixture_parses_and_normalizes() {
    let resp: SearchResponse = serde_json::from_str(SEARCH).unwrap();
    assert_eq!(resp.results.len(), 1);
    let page = normalize::search_page(&resp);
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(item.r#ref.id, "11");
    assert_eq!(item.r#ref.provider, "dreamhack");
    assert_eq!(item.title, "out_of_bound");
    assert_eq!(item.slug, "out-of-bound");
    assert_eq!(item.difficulty.as_deref(), Some("Bronze 4"));
    assert_eq!(item.categories, vec!["pwnable".to_string()]);
    assert_eq!(page.next_page, None);
    assert_eq!(page.total, 1);
}

#[test]
fn search_page_fixture_yields_next_page_two() {
    let resp: SearchResponse = serde_json::from_str(SEARCH_PAGE).unwrap();
    let page = normalize::search_page(&resp);
    assert_eq!(page.next_page, Some(2));
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.total, 1829);
}

#[test]
fn detail_fixture_normalizes() {
    let detail: ChallengeDetail = serde_json::from_str(DETAIL).unwrap();
    let challenge = normalize::challenge("https://dreamhack.io", &detail);
    assert!(!challenge.description.is_empty());
    assert_eq!(challenge.assets.len(), 1);
    assert_eq!(
        challenge.assets[0]
            .descriptor
            .get("kind")
            .and_then(|v| v.as_str()),
        Some("dreamhack-download")
    );
    assert_eq!(
        challenge.assets[0]
            .descriptor
            .get("challenge_id")
            .and_then(|v| v.as_u64()),
        Some(11)
    );
    assert!(challenge.remote_endpoints.is_empty());
    assert_eq!(
        challenge.source_url,
        "https://dreamhack.io/wargame/challenges/11"
    );
    assert_eq!(challenge.summary.slug, "out-of-bound");
}

#[test]
fn login_401_fixture_maps_to_authentication_failed() {
    let value: serde_json::Value = serde_json::from_str(LOGIN_401).unwrap();
    assert_eq!(
        value.get("detail").and_then(|v| v.as_str()),
        Some("Failed to login with given credentials.")
    );
}

#[test]
fn download_401_fixture_has_a_detail_message() {
    let value: serde_json::Value = serde_json::from_str(DOWNLOAD_401).unwrap();
    assert!(value.get("detail").is_some());
}
