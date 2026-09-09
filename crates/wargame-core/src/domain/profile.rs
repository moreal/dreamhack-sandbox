//! Declarative environment profiles (MVP.md §13; DESIGN.md §6).

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Result, WargameError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestOs {
    Macos,
    Linux,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuestArch {
    #[serde(rename = "arm64")]
    Arm64,
    #[serde(rename = "x86_64")]
    X86_64,
}

/// Which VM backend a profile asks for (spec.md §3 FR-1's `vm.backend`).
/// `Auto` -- the default, and what every pre-multi-OS profile YAML means --
/// lets [`crate::domain::compat::resolve`] pick from the support matrix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendChoice {
    #[default]
    Auto,
    Tart,
    Utm,
}

impl BackendChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendChoice::Auto => "auto",
            BackendChoice::Tart => "tart",
            BackendChoice::Utm => "utm",
        }
    }
}

impl std::fmt::Display for BackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Hardware virtualization vs. full software emulation (spec.md §3 FR-1's
/// `vm.mode`). A profile that omits it means `Virtualize`, which is what the
/// existing Tart/macOS path has always done; `Emulate` is never substituted
/// silently for an unavailable `Virtualize` (spec.md §1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    #[default]
    Virtualize,
    Emulate,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Virtualize => "virtualize",
            ExecutionMode::Emulate => "emulate",
        }
    }
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExecutionMode {
    type Err = WargameError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "virtualize" => Ok(ExecutionMode::Virtualize),
            "emulate" => Ok(ExecutionMode::Emulate),
            other => Err(WargameError::Other(format!(
                "invalid execution mode: {other} (expected \"virtualize\" or \"emulate\")"
            ))),
        }
    }
}

/// A profile's `vm:` section. Absent in every pre-multi-OS YAML, where the
/// defaults (`auto` + `virtualize`) reproduce the Tart behaviour exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VmSettings {
    pub backend: BackendChoice,
    pub mode: ExecutionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu: u32,
    pub memory_mib: u64,
    pub disk_gib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestSpec {
    pub os: GuestOs,
    pub arch: GuestArch,
    /// Which Linux distribution the guest is, e.g. `kali` (spec.md §3 FR-1).
    /// Only meaningful for `os: linux`; absent everywhere else.
    #[serde(default)]
    pub distribution: Option<String>,
    pub user: String,
    pub password: Option<String>,
}

impl GuestSpec {
    /// The distribution name, lowercased and trimmed, for matching against
    /// the support matrix.
    pub fn distribution_key(&self) -> Option<String> {
        self.distribution
            .as_deref()
            .map(|d| d.trim().to_ascii_lowercase())
            .filter(|d| !d.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSpec {
    pub base: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionSpec {
    #[serde(default)]
    pub scripts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsSpec {
    #[serde(default)]
    pub verify: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub vm: VmSettings,
    pub guest: GuestSpec,
    pub image: ImageSpec,
    pub resources: Resources,
    #[serde(default)]
    pub provision: ProvisionSpec,
    #[serde(default)]
    pub tools: ToolsSpec,
    pub workspace: WorkspaceSpec,
}

/// Raw on-disk YAML shape (DESIGN.md §6). Kept separate from [`Profile`] because
/// the file nests `name`/`description` under `metadata` and expresses memory in
/// GiB, while the in-memory `Profile` flattens metadata and uses MiB.
#[derive(Debug, Deserialize)]
struct RawProfileFile {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    metadata: RawMetadata,
    #[serde(default)]
    vm: VmSettings,
    guest: GuestSpec,
    image: ImageSpec,
    resources: RawResources,
    #[serde(default)]
    provision: ProvisionSpec,
    #[serde(default)]
    tools: ToolsSpec,
    workspace: WorkspaceSpec,
}

#[derive(Debug, Deserialize)]
struct RawMetadata {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct RawResources {
    cpu: u32,
    memory_gib: u64,
    disk_gib: u64,
}

const EXPECTED_API_VERSION: &str = "wargame.vm/v1";
const EXPECTED_KIND: &str = "Profile";

/// The namespace every image a build publishes lives in
/// ([`Profile::image_name`]). Kept next to the name it builds so the
/// backends that have to recognise "an image this app made" -- rather than
/// a VM the user made -- agree with it (spec.md §3 FR-5).
pub const PROFILE_IMAGE_PREFIX: &str = "wargame-profile-";

/// Refuses a `guest.user` that would not survive being pasted into a
/// command line.
///
/// The value is interpolated into the sudoers line `profile create` tells
/// the user to run **as root**
/// (`echo '<user> ALL=(ALL) NOPASSWD:ALL' | sudo tee ...`), and into the
/// `user@host` an `ssh` argv carries -- so a quote would break the root
/// command's quoting and a leading `-` would become an `ssh` option. Real
/// account names do not need any of that, so anything outside
/// `[A-Za-z0-9._-]` (starting with a letter, digit or `_`) is refused when
/// the profile is read, naming the field and the profile.
fn validate_guest_user(profile: &str, user: &str) -> Result<()> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    let starts_ok = user
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    if starts_ok && user.chars().all(allowed) {
        return Ok(());
    }
    Err(WargameError::InvalidProfile {
        name: profile.to_string(),
        reason: format!(
            "guest.user {user:?} is not a usable account name: use letters, digits, `.`, `_` or `-`, starting with a letter, digit or `_`"
        ),
    })
}

impl Profile {
    /// Parses a profile YAML document (DESIGN.md §6 schema).
    pub fn from_yaml(s: &str) -> Result<Profile> {
        let raw: RawProfileFile =
            serde_yaml::from_str(s).map_err(|e| WargameError::InvalidProfile {
                name: "<unknown>".to_string(),
                reason: e.to_string(),
            })?;
        if raw.api_version != EXPECTED_API_VERSION {
            return Err(WargameError::InvalidProfile {
                name: raw.metadata.name,
                reason: format!(
                    "unsupported apiVersion {:?}, expected {EXPECTED_API_VERSION:?}",
                    raw.api_version
                ),
            });
        }
        if raw.kind != EXPECTED_KIND {
            return Err(WargameError::InvalidProfile {
                name: raw.metadata.name,
                reason: format!(
                    "unsupported kind {:?}, expected {EXPECTED_KIND:?}",
                    raw.kind
                ),
            });
        }
        validate_guest_user(&raw.metadata.name, &raw.guest.user)?;
        Ok(Profile {
            name: raw.metadata.name,
            description: raw.metadata.description,
            vm: raw.vm,
            guest: raw.guest,
            image: raw.image,
            resources: Resources {
                cpu: raw.resources.cpu,
                memory_mib: raw.resources.memory_gib * 1024,
                disk_gib: raw.resources.disk_gib,
            },
            provision: raw.provision,
            tools: raw.tools,
            workspace: raw.workspace,
        })
    }

    /// The local Tart image name that a built profile is stored under.
    pub fn image_name(&self) -> String {
        format!("{PROFILE_IMAGE_PREFIX}{}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: macos-re-arm64
  description: "macOS reversing: Ghidra, LLVM, Python/uv"

guest:
  os: macos
  arch: arm64
  user: admin
  password: admin

image:
  base: ghcr.io/cirruslabs/macos-sequoia-vanilla:latest

resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 60

provision:
  scripts:
    - scripts/macos/common.sh
    - scripts/macos/reversing.sh

tools:
  verify:
    - python3 --version
    - uv --version
    - test -x /opt/homebrew/bin/ghidraRun

workspace:
  root: /Users/admin/wargame
"#;

    /// `guest.user` is pasted into a sudoers line the user runs as root
    /// (`profile create`'s step 4) and into the `user@host` an `ssh` argv
    /// carries. A quote, a space or a leading `-` there would break that
    /// root command line or turn into an `ssh` option, so the profile is
    /// refused when it is read.
    #[test]
    fn a_guest_user_that_would_break_a_root_command_line_is_refused() {
        for bad in [
            "ad'min",
            "ad\"min",
            "ad min",
            "admin; rm -rf /",
            "-oProxyCommand=touch$IFS/tmp/pwn",
            "admin$(id)",
            "",
        ] {
            let yaml = SAMPLE.replace("user: admin", &format!("user: {bad:?}"));
            let err = Profile::from_yaml(&yaml).expect_err("guest.user {bad:?} must be refused");
            let msg = err.to_string();
            assert!(msg.contains("guest.user"), "{bad:?}: {msg}");
            assert!(msg.contains("macos-re-arm64"), "{bad:?}: {msg}");
        }
    }

    #[test]
    fn ordinary_account_names_are_accepted() {
        for good in [
            "admin",
            "kali",
            "kali-user",
            "user.name",
            "_svc",
            "IEUser11",
        ] {
            let yaml = SAMPLE.replace("user: admin", &format!("user: {good:?}"));
            let p = Profile::from_yaml(&yaml).unwrap_or_else(|e| panic!("{good:?}: {e}"));
            assert_eq!(p.guest.user, good);
        }
    }

    #[test]
    fn parses_sample_and_converts_memory() {
        let p = Profile::from_yaml(SAMPLE).unwrap();
        assert_eq!(p.name, "macos-re-arm64");
        assert_eq!(p.resources.memory_mib, 8192);
        assert_eq!(p.resources.cpu, 4);
        assert_eq!(p.resources.disk_gib, 60);
        assert_eq!(p.guest.os, GuestOs::Macos);
        assert_eq!(p.guest.arch, GuestArch::Arm64);
        assert_eq!(p.provision.scripts.len(), 2);
        assert_eq!(p.image_name(), "wargame-profile-macos-re-arm64");
    }

    /// The `vm:` section and `guest.distribution` from spec.md §4's example.
    const KALI: &str = r#"
apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: kali-custom

vm:
  backend: utm
  mode: emulate

guest:
  os: linux
  distribution: kali
  arch: x86_64
  user: kali

image:
  base: ""

resources:
  cpu: 4
  memory_gib: 8
  disk_gib: 80

workspace:
  root: /home/kali/wargame
"#;

    #[test]
    fn a_yaml_without_a_vm_section_defaults_to_auto_virtualize() {
        let p = Profile::from_yaml(SAMPLE).unwrap();
        assert_eq!(p.vm.backend, BackendChoice::Auto);
        assert_eq!(p.vm.mode, ExecutionMode::Virtualize);
        assert_eq!(p.guest.distribution, None);
    }

    #[test]
    fn parses_the_vm_section_and_guest_distribution() {
        let p = Profile::from_yaml(KALI).unwrap();
        assert_eq!(p.vm.backend, BackendChoice::Utm);
        assert_eq!(p.vm.mode, ExecutionMode::Emulate);
        assert_eq!(p.guest.os, GuestOs::Linux);
        assert_eq!(p.guest.arch, GuestArch::X86_64);
        assert_eq!(p.guest.distribution.as_deref(), Some("kali"));
        assert_eq!(p.guest.distribution_key().as_deref(), Some("kali"));
        assert_eq!(p.guest.user, "kali");
    }

    #[test]
    fn distribution_key_is_normalized_and_empty_means_none() {
        let mut p = Profile::from_yaml(KALI).unwrap();
        p.guest.distribution = Some("  Kali ".to_string());
        assert_eq!(p.guest.distribution_key().as_deref(), Some("kali"));
        p.guest.distribution = Some(String::new());
        assert_eq!(p.guest.distribution_key(), None);
    }

    #[test]
    fn execution_mode_from_str() {
        assert_eq!(
            ExecutionMode::from_str("EMULATE").unwrap(),
            ExecutionMode::Emulate
        );
        assert_eq!(
            ExecutionMode::from_str("virtualize").unwrap(),
            ExecutionMode::Virtualize
        );
        assert!(ExecutionMode::from_str("rosetta").is_err());
    }

    #[test]
    fn rejects_wrong_api_version() {
        let bad = SAMPLE.replacen("wargame.vm/v1", "wargame.vm/v2", 1);
        let err = Profile::from_yaml(&bad).unwrap_err();
        assert!(matches!(err, WargameError::InvalidProfile { .. }));
    }
}
