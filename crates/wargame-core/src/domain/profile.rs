//! Declarative environment profiles (MVP.md §13; DESIGN.md §6).

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
    pub user: String,
    pub password: Option<String>,
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
        Ok(Profile {
            name: raw.metadata.name,
            description: raw.metadata.description,
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
        format!("wargame-profile-{}", self.name)
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

    #[test]
    fn rejects_wrong_api_version() {
        let bad = SAMPLE.replacen("wargame.vm/v1", "wargame.vm/v2", 1);
        let err = Profile::from_yaml(&bad).unwrap_err();
        assert!(matches!(err, WargameError::InvalidProfile { .. }));
    }
}
