//! Guest OS/arch/backend compatibility checking (MVP.md §25).

use serde::{Deserialize, Serialize};

use super::profile::{GuestArch, GuestOs, Profile};
use crate::error::{Result, WargameError};

/// What a `VmProvider` backend can run (MVP.md §11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub guest_os: Vec<GuestOs>,
    pub guest_arch: Vec<GuestArch>,
    pub graphical_console: bool,
    pub ssh: bool,
    pub snapshots: bool,
    pub copy_in: bool,
    pub remote: bool,
}

fn os_label(os: GuestOs) -> &'static str {
    match os {
        GuestOs::Macos => "macOS",
        GuestOs::Linux => "Linux",
        GuestOs::Windows => "Windows",
    }
}

fn arch_label(arch: GuestArch) -> &'static str {
    match arch {
        GuestArch::Arm64 => "arm64",
        GuestArch::X86_64 => "x86_64",
    }
}

/// Checks whether `profile`'s guest OS/architecture is launchable on the
/// `backend_name` backend advertising `caps`.
pub fn check(profile: &Profile, backend_name: &str, caps: &ProviderCapabilities) -> Result<()> {
    let os_ok = caps.guest_os.contains(&profile.guest.os);
    let arch_ok = caps.guest_arch.contains(&profile.guest.arch);
    if os_ok && arch_ok {
        return Ok(());
    }

    let supported: Vec<String> = caps
        .guest_os
        .iter()
        .flat_map(|os| caps.guest_arch.iter().map(move |arch| (os, arch)))
        .map(|(os, arch)| format!("  - {}/{}", os_label(*os), arch_label(*arch)))
        .collect();
    let supported = if supported.is_empty() {
        "  (none)".to_string()
    } else {
        supported.join("\n")
    };

    let message = format!(
        "This environment requires {}/{}.\n\nThe current backend {:?} supports:\n{}",
        os_label(profile.guest.os),
        arch_label(profile.guest.arch),
        backend_name,
        supported,
    );
    Err(WargameError::Incompatible(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::profile::{
        GuestSpec, ImageSpec, ProvisionSpec, Resources, ToolsSpec, WorkspaceSpec,
    };

    fn profile_with(os: GuestOs, arch: GuestArch) -> Profile {
        Profile {
            name: "test".to_string(),
            description: String::new(),
            guest: GuestSpec {
                os,
                arch,
                user: "admin".to_string(),
                password: None,
            },
            image: ImageSpec {
                base: "base".to_string(),
            },
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 60,
            },
            provision: ProvisionSpec::default(),
            tools: ToolsSpec::default(),
            workspace: WorkspaceSpec {
                root: "/Users/admin/wargame".to_string(),
            },
        }
    }

    fn macos_arm64_caps() -> ProviderCapabilities {
        ProviderCapabilities {
            guest_os: vec![GuestOs::Macos],
            guest_arch: vec![GuestArch::Arm64],
            graphical_console: true,
            ssh: true,
            snapshots: false,
            copy_in: true,
            remote: false,
        }
    }

    #[test]
    fn accepts_matching_profile() {
        let profile = profile_with(GuestOs::Macos, GuestArch::Arm64);
        assert!(check(&profile, "local-macos", &macos_arm64_caps()).is_ok());
    }

    #[test]
    fn rejects_mismatched_profile_with_helpful_message() {
        let profile = profile_with(GuestOs::Windows, GuestArch::X86_64);
        let err = check(&profile, "local-macos", &macos_arm64_caps()).unwrap_err();
        let WargameError::Incompatible(msg) = &err else {
            panic!("expected Incompatible, got {err:?}");
        };
        assert!(msg.contains("supports:"));
        assert!(msg.contains("Windows/x86_64"));
        assert!(msg.contains("macOS/arm64"));
    }
}
