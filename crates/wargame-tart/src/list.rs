//! Parsing for `tart list --format json` (DESIGN.md §1).

use serde::Deserialize;
use wargame_core::error::{Result, WargameError};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TartVm {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "Running")]
    pub running: bool,
    #[serde(rename = "Size")]
    pub size_gib: u64,
    #[serde(rename = "Disk")]
    pub disk_gib: u64,
}

/// Parses the JSON array `tart list --format json` prints. Unknown fields
/// (e.g. `Accessed`) are ignored.
pub fn parse_list(json: &str) -> Result<Vec<TartVm>> {
    serde_json::from_str(json).map_err(|e| WargameError::ProviderChanged {
        provider: "tart".to_string(),
        what: format!("`tart list --format json` output: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape per DESIGN.md §1: `[{Name, Source, State, Running, Size, Disk, Accessed}]`.
    const SAMPLE: &str = r#"[
        {
            "Name": "wargame-profile-macos-re-arm64",
            "Source": "local",
            "State": "stopped",
            "Running": false,
            "Size": 40,
            "Disk": 60,
            "Accessed": "2026-09-01T00:00:00Z"
        },
        {
            "Name": "wg-dh-out-of-bound-a1b2c3",
            "Source": "local",
            "State": "running",
            "Running": true,
            "Size": 12,
            "Disk": 60,
            "Accessed": "2026-09-05T00:00:00Z"
        },
        {
            "Name": "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest",
            "Source": "oci",
            "State": "stopped",
            "Running": false,
            "Size": 40,
            "Disk": 40,
            "Accessed": "2026-08-01T00:00:00Z"
        }
    ]"#;

    #[test]
    fn parses_three_entries_one_running() {
        let vms = parse_list(SAMPLE).unwrap();
        assert_eq!(vms.len(), 3);
        assert_eq!(vms.iter().filter(|v| v.running).count(), 1);
        assert_eq!(vms[1].name, "wg-dh-out-of-bound-a1b2c3");
        assert!(vms[1].running);
        assert_eq!(vms[1].state, "running");
    }

    #[test]
    fn empty_array_parses_to_empty_vec() {
        assert_eq!(parse_list("[]").unwrap(), Vec::new());
    }

    #[test]
    fn garbage_input_is_provider_changed() {
        let err = parse_list("not json").unwrap_err();
        assert!(matches!(err, WargameError::ProviderChanged { .. }));
    }
}
