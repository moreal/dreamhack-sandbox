//! Redaction of secret-shaped substrings before they ever reach a log line
//! (DESIGN.md §3.6).
//!
//! [`redact`] is a pure string transform so it is trivially unit-testable;
//! [`init_tracing`] wires it into a `tracing` subscriber so every event's
//! rendered line is passed through it before being written anywhere.

use std::fmt::Write as _;
use std::io::Write as IoWrite;
use std::path::Path;
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

const REDACTED: &str = "<redacted>";

/// Field names (DESIGN.md §3.6) whose *value* is always replaced wholesale,
/// regardless of how it is formatted (Display, Debug-quoted, etc.), before
/// the line is even assembled.
const SENSITIVE_FIELD_NAMES: &[&str] = &[
    "cookie",
    "authorization",
    "password",
    "sessionid",
    "csrftoken",
];

/// Masks secret-shaped substrings in `s`:
/// - `sessionid=...`, `csrftoken=...`, `password=...` key/value pairs
/// - `Bearer <token>` authorization schemes
/// - the value following a `Cookie:` or `Authorization:` header (to end of line)
/// - `-----BEGIN ... PRIVATE KEY-----...-----END ... PRIVATE KEY-----` PEM blocks
///
/// Everything else passes through unchanged.
pub fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for key in ["sessionid=", "csrftoken=", "password="] {
        out = redact_value_after(&out, key, |c| {
            c.is_whitespace() || matches!(c, ';' | '&' | '"' | '\'')
        });
    }
    out = redact_value_after(&out, "Bearer ", |c| c.is_whitespace());
    out = redact_header_line(&out, "Cookie:");
    out = redact_header_line(&out, "Authorization:");
    out = redact_pem_private_keys(&out);
    out
}

/// Case-insensitive `find`, returning a byte offset valid in `haystack`.
///
/// ASCII-lowercasing preserves both string length and byte offsets for
/// non-ASCII bytes (they are left untouched), so the offset found in the
/// lowercased copy is valid in the original.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

/// Replaces every value that follows (case-insensitive) `key` with
/// `<redacted>`, where the value ends at the first character matching `stop`
/// (or at the end of the string).
///
/// If the value is quoted (`key"..."` or `key'...'`, as e.g. a Debug-rendered
/// `&str` field would render), the quotes are preserved but the *entire*
/// quoted body is treated as the value -- a quote character is deliberately
/// excluded from `stop` handling here so a value like `"hunter2"` is not cut
/// off at its own opening quote.
fn redact_value_after(s: &str, key: &str, stop: impl Fn(char) -> bool + Copy) -> String {
    let mut result = String::new();
    let mut rest = s;
    loop {
        match find_ci(rest, key) {
            None => {
                result.push_str(rest);
                break;
            }
            Some(idx) => {
                let value_start = idx + key.len();
                result.push_str(&rest[..value_start]);
                let value_rest = &rest[value_start..];
                match value_rest.chars().next().filter(|&c| c == '"' || c == '\'') {
                    Some(quote) => {
                        let after_quote = &value_rest[quote.len_utf8()..];
                        result.push(quote);
                        result.push_str(REDACTED);
                        match after_quote.find(quote) {
                            Some(p) => {
                                result.push(quote);
                                rest = &value_rest[quote.len_utf8() + p + quote.len_utf8()..];
                            }
                            None => {
                                rest = &value_rest[value_rest.len()..];
                            }
                        }
                    }
                    None => {
                        let end = value_rest.find(stop).unwrap_or(value_rest.len());
                        result.push_str(REDACTED);
                        rest = &value_rest[end..];
                    }
                }
            }
        }
    }
    result
}

/// Replaces the rest of the line following a `key:` header (e.g.
/// `Cookie:`/`Authorization:`) with `<redacted>`, preserving a single
/// leading space if present.
fn redact_header_line(s: &str, key: &str) -> String {
    let mut result = String::new();
    let mut rest = s;
    loop {
        match find_ci(rest, key) {
            None => {
                result.push_str(rest);
                break;
            }
            Some(idx) => {
                let value_start = idx + key.len();
                result.push_str(&rest[..value_start]);
                let mut value_rest = &rest[value_start..];
                if let Some(stripped) = value_rest.strip_prefix(' ') {
                    result.push(' ');
                    value_rest = stripped;
                }
                let end = value_rest.find('\n').unwrap_or(value_rest.len());
                result.push_str(REDACTED);
                rest = &value_rest[end..];
            }
        }
    }
    result
}

/// Replaces `-----BEGIN ... PRIVATE KEY-----...-----END ... PRIVATE KEY-----`
/// blocks wholesale. A `-----BEGIN...-----END` span that does not mention
/// "PRIVATE KEY" is left untouched.
fn redact_pem_private_keys(s: &str) -> String {
    const BEGIN: &str = "-----BEGIN";
    const END: &str = "-----END";

    let mut result = String::new();
    let mut rest = s;
    loop {
        let Some(start) = find_ci(rest, BEGIN) else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end_rel) = find_ci(tail, END) else {
            result.push_str(tail);
            break;
        };
        let after_end_marker = end_rel + END.len();
        let close_rel = tail[after_end_marker..]
            .find("-----")
            .map(|o| after_end_marker + o + 5)
            .unwrap_or(tail.len());
        let block = &tail[..close_rel];
        if find_ci(block, "private key").is_some() {
            write!(
                result,
                "-----BEGIN PRIVATE KEY-----{REDACTED}-----END PRIVATE KEY-----"
            )
            .expect("String writes never fail");
        } else {
            result.push_str(block);
        }
        rest = &tail[close_rel..];
    }
    result
}

struct RedactingVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl RedactingVisitor {
    /// Records `rendered` for `field`, masking it wholesale first if its name
    /// is one of [`SENSITIVE_FIELD_NAMES`] (DESIGN.md §3.6) -- this runs
    /// before any Debug-quoting or pattern matching, so a quoted secret value
    /// can never leak past it.
    fn record(&mut self, field: &Field, rendered: String) {
        if field.name() == "message" {
            self.message = rendered;
            return;
        }
        let name = field.name();
        let masked = SENSITIVE_FIELD_NAMES
            .iter()
            .any(|sensitive| name.eq_ignore_ascii_case(sensitive));
        let value = if masked {
            REDACTED.to_string()
        } else {
            rendered
        };
        self.fields.push((name.to_string(), value));
    }
}

impl Visit for RedactingVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }
}

/// A minimal `tracing_subscriber` layer whose formatted output is always
/// passed through [`redact`] before being written.
struct RedactingWriterLayer {
    writer: Mutex<Box<dyn IoWrite + Send>>,
}

impl<S: Subscriber> Layer<S> for RedactingWriterLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = RedactingVisitor {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut visitor);

        let mut line = format!("{} {}", event.metadata().level(), visitor.message);
        for (k, v) in &visitor.fields {
            let _ = write!(line, " {k}={v}");
        }
        let redacted = redact(&line);

        if let Ok(mut w) = self.writer.lock() {
            let _ = writeln!(w, "{redacted}");
        }
    }
}

/// Installs the global `tracing` subscriber. `RUST_LOG` (or `debug`/`info`
/// depending on `verbose` if unset) controls verbosity; `log_file`, if
/// given, receives redacted lines instead of stderr. Safe to call more than
/// once (later calls are ignored).
pub fn init_tracing(verbose: bool, log_file: Option<&Path>) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(if verbose { "debug" } else { "info" }));

    let writer: Box<dyn IoWrite + Send> = match log_file {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                Ok(f) => Box::new(f),
                Err(_) => Box::new(std::io::stderr()),
            }
        }
        None => Box::new(std::io::stderr()),
    };

    let layer = RedactingWriterLayer {
        writer: Mutex::new(writer),
    };

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_sessionid_and_csrftoken() {
        let input = "Cookie header had sessionid=abc123XYZ; csrftoken=deadbeef and more";
        let out = redact(input);
        assert!(!out.contains("abc123XYZ"));
        assert!(!out.contains("deadbeef"));
        assert!(out.contains("sessionid=<redacted>"));
        assert!(out.contains("csrftoken=<redacted>"));
        assert!(out.contains("and more"));
    }

    #[test]
    fn masks_password() {
        let out = redact("login body: password=hunter2&remember=1");
        assert!(!out.contains("hunter2"));
        assert!(out.contains("password=<redacted>"));
        assert!(out.contains("remember=1"));
    }

    #[test]
    fn masks_bearer_token() {
        let out = redact("Authorization: Bearer eyJabc.def.ghi\nnext line unaffected");
        assert!(!out.contains("eyJabc"));
        // Authorization: header masking also applies; check the bearer form alone too.
        let out2 = redact("sent Bearer sometoken123 to the API");
        assert!(!out2.contains("sometoken123"));
        assert!(out2.contains("Bearer <redacted>"));
    }

    #[test]
    fn masks_cookie_and_authorization_headers() {
        let out = redact("Cookie: sessionid=xyz; csrftoken=abc\nAuthorization: Bearer tok\nGET /x");
        assert!(!out.contains("xyz"));
        assert!(!out.contains("tok"));
        assert!(out.contains("GET /x"));
    }

    #[test]
    fn masks_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBogsecret\n-----END RSA PRIVATE KEY-----";
        let out = redact(pem);
        assert!(!out.contains("MIIBogsecret"));
        assert!(out.contains("PRIVATE KEY"));
    }

    #[test]
    fn leaves_normal_text_untouched() {
        let input = "GET /api/v1/wargame/challenges/11/ 200 OK in 42ms";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn init_tracing_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("log.txt");
        init_tracing(true, Some(&log_path));
        tracing::info!(cookie = "sessionid=abc", "hello");
    }

    #[test]
    fn masks_quoted_value_in_free_text() {
        // A value can be quoted even outside a tracing field (e.g. copied
        // from an HTTP request dump); the closing quote must not be treated
        // as the end of the redacted run, and the value itself must vanish.
        let out = redact(r#"login attempt password="hunter2""#);
        assert!(!out.contains("hunter2"));
        assert_eq!(out, r#"login attempt password="<redacted>""#);

        let out = redact("session cookie sessionid='abc123XYZ' seen");
        assert!(!out.contains("abc123XYZ"));
        assert_eq!(out, "session cookie sessionid='<redacted>' seen");
    }

    /// Constructs a [`RedactingWriterLayer`] writing into an in-memory buffer
    /// and returns `(subscriber, buffer)` so a test can drive real
    /// `tracing::info!` calls through it and inspect exactly what would have
    /// been written to disk.
    fn layer_with_buffer() -> (
        tracing_subscriber::layer::Layered<RedactingWriterLayer, tracing_subscriber::Registry>,
        std::sync::Arc<Mutex<Vec<u8>>>,
    ) {
        struct SharedBuf(std::sync::Arc<Mutex<Vec<u8>>>);
        impl IoWrite for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buf = std::sync::Arc::new(Mutex::new(Vec::new()));
        let layer = RedactingWriterLayer {
            writer: Mutex::new(Box::new(SharedBuf(buf.clone()))),
        };
        (tracing_subscriber::registry().with(layer), buf)
    }

    /// End-to-end regression test for the review finding: going through the
    /// installed layer (not just calling [`redact`] directly), a Debug-quoted
    /// `&str` field named `password`/`cookie`/etc. must never leak its value,
    /// and the DESIGN §3.6 field-name masking must actually fire.
    #[test]
    fn layer_masks_sensitive_fields_end_to_end() {
        let (subscriber, buf) = layer_with_buffer();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(password = "hunter2", "login attempt");
            tracing::info!(sessionid = "abc", "session set");
            tracing::info!(cookie = "flavor=choc; other=1", "cookie set");
        });

        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(!out.contains("hunter2"), "leaked password: {out}");
        assert!(!out.contains("flavor=choc"), "leaked cookie: {out}");
        assert!(out.contains("password=<redacted>"));
        assert!(out.contains("sessionid=<redacted>"));
        assert!(out.contains("cookie=<redacted>"));
    }
}
