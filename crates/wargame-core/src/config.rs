//! Application configuration (DESIGN.md §3.5, MVP.md §22).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::session::NetworkMode;
use crate::error::{Result, WargameError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub default_provider: String,
    pub default_profile: String,
    pub default_network: NetworkMode,
    pub extract_archives: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        GeneralConfig {
            default_provider: "dreamhack".to_string(),
            default_profile: "macos-re-arm64".to_string(),
            default_network: NetworkMode::Isolated,
            extract_archives: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TartConfig {
    pub binary: String,
    pub vm_prefix: String,
    pub boot_timeout_secs: u64,
}

impl Default for TartConfig {
    fn default() -> Self {
        TartConfig {
            binary: "tart".to_string(),
            vm_prefix: "wg-".to_string(),
            boot_timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VmConfig {
    pub tart: TartConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DreamhackConfig {
    pub base_url: String,
    /// `"keychain"` (macOS Keychain) or `"file"` (plaintext JSON; tests/CI only).
    pub credential_store: String,
}

impl Default for DreamhackConfig {
    fn default() -> Self {
        DreamhackConfig {
            base_url: "https://dreamhack.io".to_string(),
            credential_store: "keychain".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WargameConfig {
    pub dreamhack: DreamhackConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugConfig {
    pub keep_staging: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub vm: VmConfig,
    pub wargame: WargameConfig,
    pub debug: DebugConfig,
}

impl Config {
    /// Loads config from `path`. A missing path (or `None`) yields
    /// [`Config::default`]; every field is `#[serde(default)]` so a partial
    /// file only overrides what it mentions.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let Some(path) = path else {
            return Ok(Config::default());
        };
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content).map_err(|e| WargameError::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[general]
default_provider = "dreamhack"
default_profile  = "macos-re-arm64"
default_network  = "isolated"
extract_archives = true

[vm.tart]
binary = "tart"
vm_prefix = "wg-"
boot_timeout_secs = 300

[wargame.dreamhack]
base_url = "https://dreamhack.io"
credential_store = "keychain"

[debug]
keep_staging = false
"#;

    #[test]
    fn load_none_gives_defaults() {
        let cfg = Config::load(None).unwrap();
        assert_eq!(cfg.general.default_network, NetworkMode::Isolated);
        assert_eq!(cfg.general.default_provider, "dreamhack");
        assert_eq!(cfg.vm.tart.binary, "tart");
    }

    #[test]
    fn load_missing_path_gives_defaults() {
        let cfg = Config::load(Some(Path::new("/nonexistent/wargame-config.toml"))).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn parses_design_sample() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.general.default_provider, "dreamhack");
        assert_eq!(cfg.general.default_profile, "macos-re-arm64");
        assert_eq!(cfg.general.default_network, NetworkMode::Isolated);
        assert!(cfg.general.extract_archives);
        assert_eq!(cfg.vm.tart.binary, "tart");
        assert_eq!(cfg.vm.tart.vm_prefix, "wg-");
        assert_eq!(cfg.vm.tart.boot_timeout_secs, 300);
        assert_eq!(cfg.wargame.dreamhack.base_url, "https://dreamhack.io");
        assert_eq!(cfg.wargame.dreamhack.credential_store, "keychain");
        assert!(!cfg.debug.keep_staging);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let cfg: Config = toml::from_str("[general]\ndefault_network = \"nat\"\n").unwrap();
        assert_eq!(cfg.general.default_network, NetworkMode::Nat);
        assert_eq!(cfg.general.default_provider, "dreamhack");
        assert_eq!(cfg.vm.tart.vm_prefix, "wg-");
    }
}
