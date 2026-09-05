//! `POST /api/v1/wargame/challenges/<id>/download/` and the response
//! shapes it may return (DESIGN.md §1, §4). Live Dreamhack (verified against
//! the frontend's challenge-page chunk on 2026-09-06) answers with a bare
//! JSON string holding a signed file URL; JSON objects with a URL field and
//! direct attachments are accepted as fallbacks.

use futures_util::StreamExt;
use reqwest::cookie::Jar;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use reqwest::{Response, StatusCode};
use url::Url;
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::{AssetSink, DownloadResult, DownloadedFile};

use crate::client::csrf_token;

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

/// Parses an RFC 6266 `Content-Disposition` header value, preferring the
/// extended `filename*=<charset>''<percent-encoded>` form over the plain
/// `filename="..."` form when both are present.
pub fn parse_content_disposition(header: &str) -> Option<String> {
    let mut plain = None;
    for part in header.split(';').skip(1) {
        let part = part.trim();
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim().trim_matches('"');
        if key.eq_ignore_ascii_case("filename*") {
            if let Some((_charset_and_lang, encoded)) = val.split_once("''") {
                if let Ok(decoded) = percent_encoding::percent_decode_str(encoded).decode_utf8() {
                    return Some(decoded.into_owned());
                }
            }
        } else if key.eq_ignore_ascii_case("filename") && plain.is_none() && !val.is_empty() {
            plain = Some(val.to_string());
        }
    }
    plain
}

/// True when a 403 body is Django REST framework's CSRF rejection
/// (`{"detail": "CSRF Failed: ..."}`) rather than a permission denial.
pub fn is_csrf_failure(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("detail")?.as_str().map(|d| d.contains("CSRF")))
        .unwrap_or(false)
}

fn url_filename(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut segs| segs.next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or("download")
        .to_string()
}

fn join(base: &Url, path: &str) -> Result<Url> {
    base.join(path)
        .map_err(|e| unavailable(format!("bad url {path}: {e}")))
}

/// `POST /api/v1/wargame/challenges/<id>/download/` with the CSRF/Referer
/// headers Dreamhack requires, then follows whichever of the two documented
/// response shapes comes back.
pub async fn download_asset(
    client: &reqwest::Client,
    jar: &Jar,
    base: &Url,
    challenge_id: &str,
    preferred_name: Option<&str>,
    sink: &mut dyn AssetSink,
) -> Result<DownloadResult> {
    let url = join(
        base,
        &format!("/api/v1/wargame/challenges/{challenge_id}/download/"),
    )?;
    let referer = join(base, &format!("/wargame/challenges/{challenge_id}/"))?;
    let csrf = csrf_token(jar, base);

    let mut req = client.post(url).header("Referer", referer.as_str());
    if let Some(csrf) = &csrf {
        req = req.header("X-CSRFToken", csrf);
    }
    let resp = req.send().await.map_err(|e| unavailable(e.to_string()))?;

    match resp.status() {
        StatusCode::UNAUTHORIZED => {
            return Err(WargameError::AuthenticationRequired {
                provider: PROVIDER.to_string(),
            })
        }
        StatusCode::FORBIDDEN => {
            // Django answers a missing/stale CSRF token with 403 too; that is
            // an adapter problem, not a login problem, so tell them apart.
            let body = resp.text().await.unwrap_or_default();
            if is_csrf_failure(&body) {
                return Err(changed(format!(
                    "download rejected the CSRF token (cookie {:?} sent: {})",
                    crate::client::CSRF_COOKIE_NAMES,
                    csrf.is_some()
                )));
            }
            return Err(WargameError::AuthenticationRequired {
                provider: PROVIDER.to_string(),
            });
        }
        StatusCode::NOT_FOUND => {
            return Err(WargameError::ChallengeNotFound(challenge_id.to_string()))
        }
        s if !s.is_success() => return Err(unavailable(format!("HTTP {s}"))),
        _ => {}
    }

    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let content_disposition = resp
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    if content_type.contains("json") {
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| changed("download response shape"))?;
        return download_from_json(client, &body, preferred_name, sink).await;
    }

    if content_disposition.is_some()
        || content_type.contains("octet-stream")
        || content_type.contains("zip")
    {
        let filename = content_disposition
            .as_deref()
            .and_then(parse_content_disposition)
            .unwrap_or_else(|| url_filename(resp.url()));
        let file = stream_to_sink(resp, &filename, sink).await?;
        return Ok(DownloadResult { files: vec![file] });
    }

    Err(changed("download response shape"))
}

/// One file reference pulled out of a JSON download response: the URL to GET
/// and, when the server named it, the filename to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub url: String,
    pub name: Option<String>,
}

/// Extracts the file URL(s) from a JSON download response. Dreamhack's
/// frontend (`onDownload` in its challenge page chunk) assigns the whole
/// response body to the anchor `href`, so the live shape is a **bare JSON
/// string** holding the URL. Object shapes (`url`/`file`/`download_url`,
/// or `files: [{url|file, name|filename}]`) are kept as defensive fallbacks.
pub fn file_refs_from_json(body: &serde_json::Value) -> Result<Vec<FileRef>> {
    if let Some(url) = body.as_str() {
        if !url.is_empty() {
            return Ok(vec![FileRef {
                url: url.to_string(),
                name: None,
            }]);
        }
        return Err(changed("download response shape"));
    }

    if let Some(arr) = body.get("files").and_then(|v| v.as_array()) {
        let files: Vec<FileRef> = arr
            .iter()
            .filter_map(|item| {
                let url = item
                    .get("url")
                    .or_else(|| item.get("file"))
                    .and_then(|v| v.as_str())?;
                let name = item
                    .get("name")
                    .or_else(|| item.get("filename"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                Some(FileRef {
                    url: url.to_string(),
                    name,
                })
            })
            .collect();
        if files.is_empty() {
            return Err(changed("download response shape"));
        }
        return Ok(files);
    }

    let url = ["url", "file", "download_url"]
        .iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()));
    match url {
        Some(url) => Ok(vec![FileRef {
            url: url.to_string(),
            name: None,
        }]),
        None => Err(changed("download response shape")),
    }
}

async fn download_from_json(
    client: &reqwest::Client,
    body: &serde_json::Value,
    preferred_name: Option<&str>,
    sink: &mut dyn AssetSink,
) -> Result<DownloadResult> {
    let refs = file_refs_from_json(body)?;
    // The placeholder name only makes sense when there is exactly one file.
    let preferred = if refs.len() == 1 {
        preferred_name
    } else {
        None
    };
    let mut files = Vec::new();
    for file in refs {
        files.push(fetch_one(client, &file.url, file.name, preferred, sink).await?);
    }
    Ok(DownloadResult { files })
}

/// True for names like `4482bbb1-7ec4-4a42-8325-d23253b84453.zip`: a UUID
/// stem carries no information for the person working on the challenge, so
/// the asset's placeholder (`<repository>.zip`) is preferred over it.
fn is_opaque_name(name: &str) -> bool {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let groups: Vec<&str> = stem.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn extension(name: &str) -> Option<&str> {
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext)
}

/// Picks the filename for a fetched file: the server's explicit name first,
/// then the asset's placeholder when it has the same extension as the URL
/// (live Dreamhack serves `<uuid>.zip` from object storage with no
/// `Content-Disposition`, so the placeholder `<repository>.zip` is the only
/// human-readable name available), else the URL's last path segment.
pub fn choose_filename(server_name: Option<String>, preferred: Option<&str>, url: &Url) -> String {
    if let Some(name) = server_name.filter(|n| !is_opaque_name(n)) {
        return name;
    }
    let from_url = url_filename(url);
    match preferred {
        Some(p)
            if extension(p).map(str::to_ascii_lowercase)
                == extension(&from_url).map(str::to_ascii_lowercase) =>
        {
            p.to_string()
        }
        _ => from_url,
    }
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    name: Option<String>,
    preferred: Option<&str>,
    sink: &mut dyn AssetSink,
) -> Result<DownloadedFile> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| unavailable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(unavailable(format!("HTTP {}", resp.status())));
    }
    let server_name = name.or_else(|| {
        resp.headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_disposition)
    });
    let filename = choose_filename(server_name, preferred, resp.url());
    stream_to_sink(resp, &filename, sink).await
}

async fn stream_to_sink(
    resp: Response,
    filename: &str,
    sink: &mut dyn AssetSink,
) -> Result<DownloadedFile> {
    sink.begin(filename).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| WargameError::DownloadFailed {
            filename: filename.to_string(),
            reason: e.to_string(),
        })?;
        sink.write(&chunk).await?;
    }
    sink.finish().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_filename() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"a.zip\""),
            Some("a.zip".to_string())
        );
    }

    #[test]
    fn parses_extended_filename_star() {
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''%ED%95%9C.zip"),
            Some("한.zip".to_string())
        );
    }

    #[test]
    fn prefers_extended_over_plain_regardless_of_order() {
        assert_eq!(
            parse_content_disposition(
                "attachment; filename=\"fallback.zip\"; filename*=UTF-8''%ED%95%9C.zip"
            ),
            Some("한.zip".to_string())
        );
    }

    #[test]
    fn url_filename_takes_last_path_segment() {
        let url = Url::parse("https://cdn.example.com/files/challenge.zip?x=1").unwrap();
        assert_eq!(url_filename(&url), "challenge.zip");
    }

    #[test]
    fn choose_filename_prefers_server_then_placeholder_then_url() {
        let url = Url::parse("https://cdn/x/4482bbb1-7ec4.zip?sig=1").unwrap();
        assert_eq!(
            choose_filename(Some("named.zip".into()), Some("rev-basic-1.zip"), &url),
            "named.zip"
        );
        assert_eq!(
            choose_filename(None, Some("rev-basic-1.zip"), &url),
            "rev-basic-1.zip"
        );
        // Extension mismatch: do not mislabel the file.
        assert_eq!(
            choose_filename(
                None,
                Some("rev-basic-1.zip"),
                &Url::parse("https://cdn/x/a.tar.gz").unwrap()
            ),
            "a.tar.gz"
        );
        assert_eq!(choose_filename(None, None, &url), "4482bbb1-7ec4.zip");
    }

    #[test]
    fn uuid_server_name_yields_to_placeholder() {
        let url = Url::parse("https://cdn/x/4482bbb1-7ec4-4a42-8325-d23253b84453.zip").unwrap();
        let uuid = "4482bbb1-7ec4-4a42-8325-d23253b84453.zip".to_string();
        assert!(is_opaque_name(&uuid));
        assert!(!is_opaque_name("rev-basic-1.zip"));
        assert_eq!(
            choose_filename(Some(uuid.clone()), Some("rev-basic-1.zip"), &url),
            "rev-basic-1.zip"
        );
        // No placeholder: the UUID name is still better than nothing.
        assert_eq!(choose_filename(Some(uuid.clone()), None, &url), uuid);
    }

    #[test]
    fn csrf_failure_body_is_detected() {
        assert!(is_csrf_failure(
            r#"{"detail":"CSRF Failed: CSRF token missing."}"#
        ));
        assert!(!is_csrf_failure(
            r#"{"detail":"You do not have permission to perform this action."}"#
        ));
        assert!(!is_csrf_failure("not json"));
    }

    #[test]
    fn bare_json_string_is_the_download_url() {
        let body = serde_json::json!("https://cdn.example.com/signed/rev-basic-1.zip?sig=abc");
        assert_eq!(
            file_refs_from_json(&body).unwrap(),
            vec![FileRef {
                url: "https://cdn.example.com/signed/rev-basic-1.zip?sig=abc".to_string(),
                name: None,
            }]
        );
    }

    #[test]
    fn empty_json_string_is_provider_changed() {
        let body = serde_json::json!("");
        assert!(matches!(
            file_refs_from_json(&body),
            Err(WargameError::ProviderChanged { .. })
        ));
    }

    #[test]
    fn object_with_url_field_still_accepted() {
        let body = serde_json::json!({"url": "https://x/a.zip"});
        assert_eq!(
            file_refs_from_json(&body).unwrap()[0].url,
            "https://x/a.zip"
        );
    }

    #[test]
    fn files_array_keeps_names_and_skips_bad_items() {
        let body = serde_json::json!({"files": [
            {"url": "https://x/a.zip", "name": "a.zip"},
            {"nope": 1},
            {"file": "https://x/b.zip", "filename": "b.zip"}
        ]});
        let refs = file_refs_from_json(&body).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name.as_deref(), Some("a.zip"));
        assert_eq!(refs[1].url, "https://x/b.zip");
    }

    #[test]
    fn unknown_object_is_provider_changed() {
        let body = serde_json::json!({"detail": "ok"});
        assert!(matches!(
            file_refs_from_json(&body),
            Err(WargameError::ProviderChanged { .. })
        ));
    }
}
