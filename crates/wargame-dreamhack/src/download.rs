//! `POST /api/v1/wargame/challenges/<id>/download/` and the two response
//! shapes it may return (DESIGN.md §1, §4): a JSON body pointing at one or
//! more file URLs, or the file bytes directly.

use futures_util::StreamExt;
use reqwest::cookie::Jar;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use reqwest::{Response, StatusCode};
use url::Url;
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::{AssetSink, DownloadResult, DownloadedFile};

use crate::client::cookie_value;

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
    sink: &mut dyn AssetSink,
) -> Result<DownloadResult> {
    let url = join(
        base,
        &format!("/api/v1/wargame/challenges/{challenge_id}/download/"),
    )?;
    let referer = join(base, &format!("/wargame/challenges/{challenge_id}/"))?;
    let csrf = cookie_value(jar, base, "csrftoken");

    let mut req = client.post(url).header("Referer", referer.as_str());
    if let Some(csrf) = &csrf {
        req = req.header("X-CSRFToken", csrf);
    }
    let resp = req.send().await.map_err(|e| unavailable(e.to_string()))?;

    match resp.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(WargameError::AuthenticationRequired {
                provider: PROVIDER.to_string(),
            })
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
        return download_from_json(client, &body, sink).await;
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

async fn download_from_json(
    client: &reqwest::Client,
    body: &serde_json::Value,
    sink: &mut dyn AssetSink,
) -> Result<DownloadResult> {
    if let Some(arr) = body.get("files").and_then(|v| v.as_array()) {
        let mut files = Vec::new();
        for item in arr {
            let Some(url) = item
                .get("url")
                .or_else(|| item.get("file"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let name = item
                .get("name")
                .or_else(|| item.get("filename"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            files.push(fetch_one(client, url, name, sink).await?);
        }
        if files.is_empty() {
            return Err(changed("download response shape"));
        }
        return Ok(DownloadResult { files });
    }

    let url = ["url", "file", "download_url"]
        .iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()));
    let Some(url) = url else {
        return Err(changed("download response shape"));
    };
    let file = fetch_one(client, url, None, sink).await?;
    Ok(DownloadResult { files: vec![file] })
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    name: Option<String>,
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
    let filename = name
        .or_else(|| {
            resp.headers()
                .get(CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_disposition)
        })
        .unwrap_or_else(|| url_filename(resp.url()));
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
}
