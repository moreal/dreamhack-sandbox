//! `model::*` -> `wargame_core::domain::*` (DESIGN.md §4).

use serde_json::json;
use url::Url;
use wargame_core::domain::{slugify, Challenge, ChallengeAsset, ChallengeRef, ChallengeSummary};
use wargame_core::ports::SearchPage;

use crate::model::{ChallengeDetail, ChallengeListItem, SearchResponse};

const PROVIDER: &str = "dreamhack";

/// `ChallengeListItem` -> `ChallengeSummary`.
///
/// `slug` prefers the server-provided `repository` (Dreamhack's stable
/// per-challenge slug) and falls back to slugifying the title. `difficulty`
/// is `tier_display` verbatim, `categories` is `tags` verbatim.
pub fn summary(item: &ChallengeListItem) -> ChallengeSummary {
    let slug = item
        .repository
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| slugify(&item.title));
    ChallengeSummary {
        r#ref: ChallengeRef {
            provider: PROVIDER.to_string(),
            id: item.id.to_string(),
        },
        slug,
        title: item.title.clone(),
        categories: item.tags.clone(),
        difficulty: item.tier_display.clone(),
    }
}

/// Words in a Korean/English challenge description that indicate a
/// downloadable file bundle exists, even when `needs_vm` is false.
const FILE_HINTS: &[&str] = &["file", "파일", "binary", "바이너리"];

fn mentions_file(description: &str) -> bool {
    let lower = description.to_lowercase();
    FILE_HINTS.iter().any(|hint| lower.contains(hint))
}

/// True if the trimmed line is *only* a `host:port`-style `nc` invocation,
/// e.g. `nc pwnable.dreamhack.io 12345`.
fn is_nc_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("nc ") else {
        return false;
    };
    let parts: Vec<&str> = rest.split_whitespace().collect();
    parts.len() == 2 && !parts[0].is_empty() && parts[1].chars().all(|c| c.is_ascii_digit())
}

/// True if the trimmed line is *only* a bare `http(s)://...` URL (as opposed
/// to, say, a markdown link `[text](https://...)` embedded in prose).
fn is_bare_url_line(line: &str) -> bool {
    (line.starts_with("http://") || line.starts_with("https://"))
        && !line.contains(char::is_whitespace)
}

/// Extracts remote-service endpoints (`nc host port` lines, bare URLs) from
/// a challenge description, one per matching line.
fn remote_endpoints(description: &str) -> Vec<String> {
    description
        .lines()
        .map(str::trim)
        .filter(|line| is_nc_line(line) || is_bare_url_line(line))
        .map(str::to_string)
        .collect()
}

/// `SearchResponse` -> `SearchPage`. `next_page` is pulled out of the
/// `next` URL's `page` query parameter (Dreamhack echoes the full next-page
/// URL rather than a bare page number).
pub fn search_page(r: &SearchResponse) -> SearchPage {
    let next_page = r.next.as_deref().and_then(|next| {
        Url::parse(next).ok().and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "page")
                .and_then(|(_, v)| v.parse::<u32>().ok())
        })
    });
    SearchPage {
        items: r.results.iter().map(summary).collect(),
        next_page,
        total: r.count,
    }
}

/// `ChallengeDetail` -> `Challenge`.
pub fn challenge(base_url: &str, d: &ChallengeDetail) -> Challenge {
    let base_url = base_url.trim_end_matches('/');
    let sum = summary(&ChallengeListItem {
        id: d.id,
        title: d.title.clone(),
        tags: d.tags.clone(),
        tier_display: d.tier_display.clone(),
        repository: d.repository.clone(),
        needs_vm: d.needs_vm,
        is_public: d.is_public,
    });
    let mut assets = Vec::new();
    if d.needs_vm || mentions_file(&d.description) {
        assets.push(ChallengeAsset {
            id: d.id.to_string(),
            filename: format!("{}.zip", sum.slug),
            size: None,
            descriptor: json!({"kind": "dreamhack-download", "challenge_id": d.id}),
            expected_sha256: None,
        });
    }
    Challenge {
        description: d.description.clone(),
        tags: d.tags.clone(),
        assets,
        remote_endpoints: remote_endpoints(&d.description),
        source_url: format!("{base_url}/wargame/challenges/{}", d.id),
        summary: sum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nc_line_matches() {
        assert!(is_nc_line("nc pwnable.dreamhack.io 12345"));
        assert!(!is_nc_line("run nc pwnable.dreamhack.io 12345"));
        assert!(!is_nc_line("nc pwnable.dreamhack.io"));
    }

    #[test]
    fn bare_url_matches_only_standalone_lines() {
        assert!(is_bare_url_line("https://dreamhack.io/x"));
        assert!(!is_bare_url_line(
            "[Out-of-bounds](https://dreamhack.io/lecture/units/out-of-bounds)"
        ));
    }
}
