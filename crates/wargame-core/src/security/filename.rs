//! Server-provided filename sanitisation (DESIGN.md §3.6).
//!
//! Never trust a filename that came from a remote server: it may try to
//! escape the staging directory (`../../etc/passwd`), hide itself
//! (`.bashrc`), or smuggle control characters. [`sanitize_filename`] reduces
//! any input to a safe basename, falling back to a synthetic name when
//! nothing usable survives.

const MAX_LEN: usize = 255;

/// Reduces `server_name` (as provided by a remote server) to a safe basename:
/// only the last path component survives, control characters and leading
/// dots are stripped, and the result is capped at 255 bytes. Falls back to
/// `"asset-<fallback_id>.bin"` when nothing safe remains.
pub fn sanitize_filename(server_name: &str, fallback_id: &str) -> String {
    let fallback = || format!("asset-{fallback_id}.bin");

    let last = server_name.rsplit(['/', '\\']).next().unwrap_or_default();

    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim_start_matches('.');

    if trimmed.is_empty() {
        return fallback();
    }

    let mut out = trimmed.to_string();
    if out.len() > MAX_LEN {
        let mut end = MAX_LEN;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }

    if out.is_empty() {
        fallback()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd", "7"), "passwd");
    }

    #[test]
    fn empty_falls_back() {
        assert_eq!(sanitize_filename("", "7"), "asset-7.bin");
    }

    #[test]
    fn strips_leading_dots() {
        assert_eq!(sanitize_filename(".hidden", "7"), "hidden");
    }

    #[test]
    fn dot_and_dotdot_fall_back() {
        assert_eq!(sanitize_filename(".", "7"), "asset-7.bin");
        assert_eq!(sanitize_filename("..", "7"), "asset-7.bin");
        assert_eq!(sanitize_filename("/foo/..", "7"), "asset-7.bin");
    }

    #[test]
    fn strips_control_characters() {
        assert_eq!(sanitize_filename("fo\u{0}o\u{1}.txt", "7"), "foo.txt");
    }

    #[test]
    fn truncates_to_255_bytes() {
        let long = "a".repeat(300);
        let out = sanitize_filename(&long, "7");
        assert_eq!(out.len(), 255);
    }

    #[test]
    fn windows_style_separators_are_handled() {
        assert_eq!(
            sanitize_filename("C:\\Users\\evil\\payload.exe", "7"),
            "payload.exe"
        );
    }
}
