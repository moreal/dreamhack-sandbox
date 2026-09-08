//! Application configuration (DESIGN.md §3.5, MVP.md §22).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::domain::compat::Backend;
use crate::domain::profile::ExecutionMode;
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

/// `[vm.utm]` (spec.md §3 FR-5/FR-6). The adapter itself lands in a later
/// task; what config owns here is where to find `utmctl` and how long a UTM
/// guest may take to boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UtmConfig {
    /// An explicit `utmctl` path, tried first (FR-6's discovery order:
    /// config -> `PATH` -> `/Applications/UTM.app/Contents/MacOS/utmctl`).
    pub utmctl: Option<String>,
    /// Display-name prefix for the VMs this app owns on UTM. Kept separate
    /// from `[vm.tart].vm_prefix` so the two backends can be namespaced
    /// independently.
    pub vm_prefix: String,
    /// Boot timeout for `mode: emulate` guests. Full x86_64 emulation is
    /// orders of magnitude slower than virtualization, hence FR-5's 1800 s.
    pub boot_timeout_secs: u64,
    /// Boot timeout for `mode: virtualize` guests, i.e. the existing 300 s.
    pub virtualize_boot_timeout_secs: u64,
}

impl Default for UtmConfig {
    fn default() -> Self {
        UtmConfig {
            utmctl: None,
            vm_prefix: "wg-".to_string(),
            boot_timeout_secs: 1800,
            virtualize_boot_timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VmConfig {
    pub tart: TartConfig,
    pub utm: UtmConfig,
}

impl VmConfig {
    /// How long a freshly started VM may take to answer, derived from the
    /// resolved backend and execution mode (spec.md §3 FR-5's "emulate 부팅
    /// 기본값은 1800초, virtualize는 기존 300초"). Kept in core so both
    /// adapters and every application service agree on one number.
    /// The display-name prefix VMs of `backend` are created with. Every
    /// caller that has to name a VM goes through this, so a UTM launch is
    /// never named from `[vm.tart]`.
    pub fn vm_prefix(&self, backend: Backend) -> &str {
        match backend {
            Backend::Tart => &self.tart.vm_prefix,
            Backend::Utm => &self.utm.vm_prefix,
        }
    }

    pub fn boot_timeout(&self, backend: Backend, mode: ExecutionMode) -> Duration {
        let secs = match (backend, mode) {
            // Tart only ever virtualizes (see the support matrix).
            (Backend::Tart, _) => self.tart.boot_timeout_secs,
            (Backend::Utm, ExecutionMode::Emulate) => self.utm.boot_timeout_secs,
            (Backend::Utm, ExecutionMode::Virtualize) => self.utm.virtualize_boot_timeout_secs,
        };
        Duration::from_secs(secs)
    }
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
    fn utm_section_defaults_and_overrides() {
        let cfg = Config::default();
        assert_eq!(cfg.vm.utm.utmctl, None);
        assert_eq!(cfg.vm.utm.vm_prefix, "wg-");
        assert_eq!(cfg.vm.utm.boot_timeout_secs, 1800);
        assert_eq!(cfg.vm.utm.virtualize_boot_timeout_secs, 300);

        let cfg: Config = toml::from_str(
            "[vm.utm]\nutmctl = \"/Applications/UTM.app/Contents/MacOS/utmctl\"\nboot_timeout_secs = 60\n",
        )
        .unwrap();
        assert_eq!(
            cfg.vm.utm.utmctl.as_deref(),
            Some("/Applications/UTM.app/Contents/MacOS/utmctl")
        );
        assert_eq!(cfg.vm.utm.boot_timeout_secs, 60);
        // Untouched fields keep their defaults.
        assert_eq!(cfg.vm.utm.virtualize_boot_timeout_secs, 300);
        assert_eq!(cfg.vm.tart.boot_timeout_secs, 300);
    }

    #[test]
    fn vm_prefix_is_per_backend() {
        let cfg: Config = toml::from_str("[vm.utm]\nvm_prefix = \"wgu-\"\n").unwrap();
        assert_eq!(cfg.vm.vm_prefix(Backend::Tart), "wg-");
        assert_eq!(cfg.vm.vm_prefix(Backend::Utm), "wgu-");
    }

    #[test]
    fn boot_timeout_is_per_backend_and_mode() {
        let cfg = Config::default();
        assert_eq!(
            cfg.vm
                .boot_timeout(Backend::Tart, ExecutionMode::Virtualize),
            Duration::from_secs(300)
        );
        assert_eq!(
            cfg.vm.boot_timeout(Backend::Utm, ExecutionMode::Emulate),
            Duration::from_secs(1800)
        );
        assert_eq!(
            cfg.vm.boot_timeout(Backend::Utm, ExecutionMode::Virtualize),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn partial_file_fills_defaults() {
        let cfg: Config = toml::from_str("[general]\ndefault_network = \"nat\"\n").unwrap();
        assert_eq!(cfg.general.default_network, NetworkMode::Nat);
        assert_eq!(cfg.general.default_provider, "dreamhack");
        assert_eq!(cfg.vm.tart.vm_prefix, "wg-");
    }
}
