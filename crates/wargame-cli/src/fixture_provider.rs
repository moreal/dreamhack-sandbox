//! `FixtureProvider`: an in-memory [`WargameProvider`] with exactly one
//! challenge (`fixture:1`, "hello-fixture"), wired in when `WARGAME_PROVIDER`
//! is `"fixture"` (PLAN.md Task 7). Used by `crates/wargame-cli/tests/cli.rs`
//! and `scripts/e2e-smoke.sh` so neither needs a Dreamhack account.

use std::sync::Mutex;

use async_trait::async_trait;

use wargame_core::domain::challenge::{Challenge, ChallengeAsset, ChallengeRef, ChallengeSummary};
use wargame_core::error::Result;
use wargame_core::ports::{
    AssetSink, AuthState, DownloadResult, ProviderStatus, SearchPage, SearchQuery, Secret,
    WargameProvider,
};

const PROVIDER_NAME: &str = "fixture";
const CHALLENGE_ID: &str = "1";
const CHALLENGE_TITLE: &str = "hello-fixture";
const ASSET_FILENAME: &str = "hello-fixture.zip";
const ZIP_ENTRY_NAME: &str = "hello.txt";
const ZIP_ENTRY_CONTENT: &[u8] = b"hello wargame\n";

/// A provider with a single, always-available challenge -- no network
/// access, no credentials, no Dreamhack account required.
pub struct FixtureProvider {
    auth: Mutex<AuthState>,
}

impl FixtureProvider {
    pub fn new() -> Self {
        FixtureProvider {
            auth: Mutex::new(AuthState::Authenticated {
                display: "fixture-user".to_string(),
            }),
        }
    }

    fn challenge(&self) -> Challenge {
        let summary = ChallengeSummary {
            r#ref: ChallengeRef {
                provider: PROVIDER_NAME.to_string(),
                id: CHALLENGE_ID.to_string(),
            },
            slug: CHALLENGE_TITLE.to_string(),
            title: CHALLENGE_TITLE.to_string(),
            categories: vec!["fixture".to_string()],
            difficulty: None,
        };
        Challenge {
            summary: summary.clone(),
            description: "A local fixture challenge; no network access needed.".to_string(),
            tags: vec!["fixture".to_string()],
            assets: vec![ChallengeAsset {
                id: CHALLENGE_ID.to_string(),
                filename: ASSET_FILENAME.to_string(),
                size: Some(fixture_zip_bytes().len() as u64),
                descriptor: serde_json::json!({"kind": "fixture"}),
                expected_sha256: None,
            }],
            remote_endpoints: vec![],
            source_url: "fixture://hello-fixture".to_string(),
        }
    }
}

impl Default for FixtureProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WargameProvider for FixtureProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    async fn search(&self, q: &SearchQuery) -> Result<SearchPage> {
        let challenge = self.challenge();
        let matches = q.text.trim().is_empty()
            || challenge
                .summary
                .title
                .to_ascii_lowercase()
                .contains(&q.text.to_ascii_lowercase());
        let items = if matches {
            vec![challenge.summary]
        } else {
            vec![]
        };
        let total = items.len() as u64;
        Ok(SearchPage {
            items,
            next_page: None,
            total,
        })
    }

    async fn get_challenge(&self, id: &str) -> Result<Challenge> {
        let mut challenge = self.challenge();
        challenge.summary.r#ref.id = id.to_string();
        Ok(challenge)
    }

    async fn auth_state(&self) -> Result<AuthState> {
        Ok(self.auth.lock().unwrap().clone())
    }

    async fn login(&self, username: &str, _password: Secret<String>) -> Result<()> {
        *self.auth.lock().unwrap() = AuthState::Authenticated {
            display: username.to_string(),
        };
        Ok(())
    }

    async fn download_asset(
        &self,
        _ch: &Challenge,
        a: &ChallengeAsset,
        sink: &mut dyn AssetSink,
    ) -> Result<DownloadResult> {
        let bytes = fixture_zip_bytes();
        sink.begin(&a.filename).await?;
        sink.write(&bytes).await?;
        let file = sink.finish().await?;
        Ok(DownloadResult { files: vec![file] })
    }

    async fn healthcheck(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            reachable: true,
            auth: self.auth.lock().unwrap().clone(),
            detail: "fixture provider is always reachable".to_string(),
        })
    }
}

/// Builds a minimal, stored (uncompressed) ZIP archive by hand -- one entry,
/// `hello.txt`, containing `hello wargame\n` -- so this crate needs neither
/// a `zip` binary nor a zip-writing dependency for a single test fixture
/// (PLAN.md Task 7).
pub fn fixture_zip_bytes() -> Vec<u8> {
    build_stored_zip(ZIP_ENTRY_NAME, ZIP_ENTRY_CONTENT)
}

fn build_stored_zip(name: &str, content: &[u8]) -> Vec<u8> {
    let crc = crc32(content);
    let name_bytes = name.as_bytes();

    // Local file header (PK\x03\x04) + the raw (stored, i.e. uncompressed)
    // file data.
    let mut local = Vec::new();
    local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    local.extend_from_slice(&20u16.to_le_bytes()); // version needed to extract
    local.extend_from_slice(&0u16.to_le_bytes()); // general purpose bit flag
    local.extend_from_slice(&0u16.to_le_bytes()); // compression method: stored
    local.extend_from_slice(&0u16.to_le_bytes()); // last mod file time
    local.extend_from_slice(&0u16.to_le_bytes()); // last mod file date
    local.extend_from_slice(&crc.to_le_bytes());
    local.extend_from_slice(&(content.len() as u32).to_le_bytes()); // compressed size
    local.extend_from_slice(&(content.len() as u32).to_le_bytes()); // uncompressed size
    local.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    local.extend_from_slice(&0u16.to_le_bytes()); // extra field length
    local.extend_from_slice(name_bytes);
    local.extend_from_slice(content);

    // Central directory header (PK\x01\x02): one entry, pointing back at
    // the local header above (offset 0, since this archive holds only it).
    let mut central = Vec::new();
    central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    central.extend_from_slice(&20u16.to_le_bytes()); // version made by
    central.extend_from_slice(&20u16.to_le_bytes()); // version needed to extract
    central.extend_from_slice(&0u16.to_le_bytes()); // flags
    central.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    central.extend_from_slice(&0u16.to_le_bytes()); // time
    central.extend_from_slice(&0u16.to_le_bytes()); // date
    central.extend_from_slice(&crc.to_le_bytes());
    central.extend_from_slice(&(content.len() as u32).to_le_bytes());
    central.extend_from_slice(&(content.len() as u32).to_le_bytes());
    central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes()); // extra length
    central.extend_from_slice(&0u16.to_le_bytes()); // comment length
    central.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
    central.extend_from_slice(&0u32.to_le_bytes()); // local header offset
    central.extend_from_slice(name_bytes);

    let cd_offset = local.len() as u32;
    let cd_size = central.len() as u32;

    // End of central directory record (PK\x05\x06).
    let mut eocd = Vec::new();
    eocd.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // this disk
    eocd.extend_from_slice(&0u16.to_le_bytes()); // disk with the start of the cd
    eocd.extend_from_slice(&1u16.to_le_bytes()); // entries on this disk
    eocd.extend_from_slice(&1u16.to_le_bytes()); // entries total
    eocd.extend_from_slice(&cd_size.to_le_bytes());
    eocd.extend_from_slice(&cd_offset.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // comment length

    let mut out = Vec::with_capacity(local.len() + central.len() + eocd.len());
    out.extend_from_slice(&local);
    out.extend_from_slice(&central);
    out.extend_from_slice(&eocd);
    out
}

/// Standard (reflected, `0xEDB88320`-polynomial) CRC-32, as used by ZIP and
/// gzip -- avoids depending on a `crc32`/`zip` crate for one test fixture.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // The standard CRC-32 (ISO-HDLC) check value for the ASCII string
        // "123456789" is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_bytes_have_a_valid_header_and_eocd_signature() {
        let bytes = fixture_zip_bytes();
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
        // No comment, so the EOCD record is a fixed 22 bytes at the tail.
        assert_eq!(&bytes[bytes.len() - 22..bytes.len() - 18], b"PK\x05\x06");
    }

    #[tokio::test]
    async fn search_matches_case_insensitively_and_falls_back_to_all() {
        let provider = FixtureProvider::new();
        let page = provider
            .search(&SearchQuery {
                text: "HELLO".to_string(),
                category: None,
                page: 1,
                page_size: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].r#ref.provider, "fixture");
        assert_eq!(page.items[0].r#ref.id, "1");

        let empty = provider
            .search(&SearchQuery {
                text: "no-such-thing".to_string(),
                category: None,
                page: 1,
                page_size: 10,
            })
            .await
            .unwrap();
        assert!(empty.items.is_empty());
    }

    #[tokio::test]
    async fn download_asset_writes_the_stored_zip_and_hashes_it() {
        let provider = FixtureProvider::new();
        let challenge = provider.get_challenge("1").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let staging = wargame_core::security::StagingDir::create(dir.path(), true).unwrap();
        let mut sink = staging.sink("1");

        let result = provider
            .download_asset(&challenge, &challenge.assets[0], &mut sink)
            .await
            .unwrap();
        assert_eq!(result.files.len(), 1);
        let bytes = std::fs::read(&result.files[0].path).unwrap();
        assert_eq!(bytes, fixture_zip_bytes());
        assert!(!result.files[0].sha256.is_empty());
    }
}
