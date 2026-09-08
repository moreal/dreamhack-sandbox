//! Guest OS/arch/mode/backend compatibility (MVP.md §25; spec.md §3 FR-1).
//!
//! Two related things live here:
//!
//! * the **support matrix** ([`SUPPORTED`]) and [`resolve`], which turn a
//!   profile's `vm.backend`/`vm.mode` request into exactly one backend, or
//!   into a [`WargameError::Incompatible`] naming the requested combination,
//!   the reason and every supported combination. This is pure data: it runs
//!   before any backend is contacted, so a launch is refused before anything
//!   is created, cloned or started.
//! * [`ProviderCapabilities`] and [`check`], which validate the same profile
//!   against what the *wired* provider says it can do.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::profile::{BackendChoice, ExecutionMode, GuestArch, GuestOs, Profile};
use super::session::NetworkMode;
use crate::error::{Result, WargameError};

/// A concrete VM backend -- what [`BackendChoice::Auto`] resolves *to*, and
/// what a session stores so it is never re-derived from a profile that may
/// since have changed (spec.md §3 FR-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Tart,
    Utm,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Tart => "tart",
            Backend::Utm => "utm",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Backend {
    type Err = WargameError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "tart" => Ok(Backend::Tart),
            "utm" => Ok(Backend::Utm),
            other => Err(WargameError::Other(format!(
                "unknown VM backend: {other} (expected \"tart\" or \"utm\")"
            ))),
        }
    }
}

/// One row of the support matrix (spec.md §1's release matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedCombination {
    pub backend: Backend,
    pub os: GuestOs,
    /// The `guest.distribution` this row requires, lowercased. `None` means
    /// the profile must not set one.
    pub distribution: Option<&'static str>,
    pub arch: GuestArch,
    pub mode: ExecutionMode,
}

/// Every guest/backend combination this build actually runs (spec.md §1).
/// Windows on arm64 is deliberately absent: the spec lists it as a
/// follow-up, and silently accepting it would promise a path nothing
/// implements.
pub const SUPPORTED: &[SupportedCombination] = &[
    SupportedCombination {
        backend: Backend::Tart,
        os: GuestOs::Macos,
        distribution: None,
        arch: GuestArch::Arm64,
        mode: ExecutionMode::Virtualize,
    },
    SupportedCombination {
        backend: Backend::Utm,
        os: GuestOs::Linux,
        distribution: Some("kali"),
        arch: GuestArch::Arm64,
        mode: ExecutionMode::Virtualize,
    },
    SupportedCombination {
        backend: Backend::Utm,
        os: GuestOs::Linux,
        distribution: Some("kali"),
        arch: GuestArch::X86_64,
        mode: ExecutionMode::Emulate,
    },
    SupportedCombination {
        backend: Backend::Utm,
        os: GuestOs::Windows,
        distribution: None,
        arch: GuestArch::X86_64,
        mode: ExecutionMode::Emulate,
    },
];

/// The execution configuration a profile resolves to: which backend runs it,
/// in which mode, and the guest it describes. Carried into
/// [`crate::ports::VmSpec`] and snapshotted into the session so `resume`,
/// `stop` and `delete` never have to consult the profile again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBackend {
    pub backend: Backend,
    pub mode: ExecutionMode,
    pub os: GuestOs,
    pub arch: GuestArch,
    pub distribution: Option<String>,
}

impl Default for ResolvedBackend {
    /// The combination every pre-multi-OS profile and session means:
    /// Tart, macOS, arm64, virtualize.
    fn default() -> Self {
        ResolvedBackend {
            backend: Backend::Tart,
            mode: ExecutionMode::Virtualize,
            os: GuestOs::Macos,
            arch: GuestArch::Arm64,
            distribution: None,
        }
    }
}

/// What a `VmProvider` backend can run (MVP.md §11), extended with the
/// execution modes and network modes it supports (spec.md §3 FR-1/FR-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub guest_os: Vec<GuestOs>,
    pub guest_arch: Vec<GuestArch>,
    /// The execution modes this backend implements. Defaults to
    /// `[Virtualize]` for a record written before modes existed.
    #[serde(default = "default_modes")]
    pub modes: Vec<ExecutionMode>,
    /// The network modes this backend can configure at all. A mode missing
    /// here is refused *before* anything is created or started (FR-6): an
    /// unavailable `isolated` is never silently downgraded to NAT.
    #[serde(default = "default_networks")]
    pub networks: Vec<NetworkMode>,
    pub graphical_console: bool,
    pub ssh: bool,
    pub snapshots: bool,
    pub copy_in: bool,
    pub remote: bool,
}

fn default_modes() -> Vec<ExecutionMode> {
    vec![ExecutionMode::Virtualize]
}

fn default_networks() -> Vec<NetworkMode> {
    vec![NetworkMode::Isolated, NetworkMode::Nat]
}

impl ProviderCapabilities {
    pub fn supports_network(&self, mode: NetworkMode) -> bool {
        self.networks.contains(&mode)
    }

    pub fn supports_mode(&self, mode: ExecutionMode) -> bool {
        self.modes.contains(&mode)
    }
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

/// `"Linux(kali)"` / `"macOS"` -- the guest label used in both the requested
/// line and the supported table so the two can be compared by eye.
fn guest_label(os: GuestOs, distribution: Option<&str>) -> String {
    match distribution {
        Some(distro) => format!("{}({distro})", os_label(os)),
        None => os_label(os).to_string(),
    }
}

/// The `Supported combinations:` block appended to every incompatibility
/// error (spec.md §3 FR-1: "불가 조합은 요청값·불가 이유·사용 가능한 조합을
/// 포함한 오류로 반환한다").
pub fn supported_combinations_table() -> String {
    let mut out = String::from("Supported combinations:");
    for combo in SUPPORTED {
        out.push_str(&format!(
            "\n  - backend {:<5} {:<18} {}",
            combo.backend.as_str(),
            format!(
                "{}/{}",
                guest_label(combo.os, combo.distribution),
                arch_label(combo.arch)
            ),
            combo.mode.as_str(),
        ));
    }
    out
}

/// Resolves a profile's requested `vm.backend`/`vm.mode` against the support
/// matrix (spec.md §3 FR-1).
///
/// `auto` picks the single backend that supports the guest; an explicit
/// backend must itself support it. There is no runtime fallback to another
/// backend and no silent mode change: an unsupported request is an
/// [`WargameError::Incompatible`] naming the request, the reason and the
/// full table. Pure data -- safe to call before any side effect.
pub fn resolve(profile: &Profile) -> Result<ResolvedBackend> {
    let os = profile.guest.os;
    let arch = profile.guest.arch;
    let mode = profile.vm.mode;
    let distribution = profile.guest.distribution_key();
    let distro = distribution.as_deref();

    let matches = |combo: &SupportedCombination| {
        combo.os == os && combo.distribution == distro && combo.arch == arch && combo.mode == mode
    };

    let found = match profile.vm.backend {
        BackendChoice::Auto => SUPPORTED.iter().find(|c| matches(c)),
        BackendChoice::Tart => SUPPORTED
            .iter()
            .find(|c| c.backend == Backend::Tart && matches(c)),
        BackendChoice::Utm => SUPPORTED
            .iter()
            .find(|c| c.backend == Backend::Utm && matches(c)),
    };

    match found {
        Some(combo) => Ok(ResolvedBackend {
            backend: combo.backend,
            mode,
            os,
            arch,
            distribution,
        }),
        None => Err(WargameError::Incompatible(incompatible_message(profile))),
    }
}

/// Builds the "requested / reason / supported" message for a combination the
/// matrix does not contain. The reason narrows step by step -- guest, then
/// architecture, then mode -- so it names the *specific* thing that cannot
/// be done rather than restating the whole request.
fn incompatible_message(profile: &Profile) -> String {
    let os = profile.guest.os;
    let arch = profile.guest.arch;
    let mode = profile.vm.mode;
    let distribution = profile.guest.distribution_key();
    let distro = distribution.as_deref();
    let choice = profile.vm.backend;

    let rows: Vec<&SupportedCombination> = match choice {
        BackendChoice::Auto => SUPPORTED.iter().collect(),
        BackendChoice::Tart => SUPPORTED
            .iter()
            .filter(|c| c.backend == Backend::Tart)
            .collect(),
        BackendChoice::Utm => SUPPORTED
            .iter()
            .filter(|c| c.backend == Backend::Utm)
            .collect(),
    };

    // "no backend supports ..." for `auto`, "backend \"utm\" does not
    // support ..." when the profile named one.
    let cannot = match choice {
        BackendChoice::Auto => "no backend supports".to_string(),
        other => format!("backend {:?} does not support", other.as_str()),
    };
    let guest = guest_label(os, distro);
    let guest_rows = |c: &&SupportedCombination| c.os == os && c.distribution == distro;

    let reason = if os == GuestOs::Linux
        && !rows.iter().any(guest_rows)
        && SUPPORTED.iter().any(|c| c.os == GuestOs::Linux)
    {
        // `os: linux` is kept generic and narrowed by `guest.distribution`
        // (FR-1), so say which field is wrong rather than "Linux is
        // unsupported" next to a table full of Linux rows.
        match distro {
            None => "a Linux guest must set `guest.distribution` (e.g. `kali`).".to_string(),
            Some(distro) => format!("Linux distribution {distro:?} is not supported."),
        }
    } else if !rows.iter().any(guest_rows) {
        format!("{cannot} {guest} guests.")
    } else if !rows.iter().any(|c| guest_rows(c) && c.arch == arch) {
        format!("{cannot} {guest} on {}.", arch_label(arch))
    } else {
        let modes: Vec<&str> = rows
            .iter()
            .filter(|c| guest_rows(c) && c.arch == arch)
            .map(|c| c.mode.as_str())
            .collect();
        format!(
            "{cannot} {guest}/{} in {} mode; it runs only in {}.",
            arch_label(arch),
            mode.as_str(),
            modes.join(" or "),
        )
    };

    format!(
        "Profile {:?} requests backend {}, {}/{}, mode {}.\nReason: {}\n\n{}",
        profile.name,
        choice.as_str(),
        guest,
        arch_label(arch),
        mode.as_str(),
        reason,
        supported_combinations_table(),
    )
}

/// Checks whether `profile`'s guest OS/architecture/mode is launchable on
/// the `backend_name` backend advertising `caps`. Complements [`resolve`]:
/// the matrix says which backend *should* run a profile, this says whether
/// the backend actually wired into this build agrees.
pub fn check(profile: &Profile, backend_name: &str, caps: &ProviderCapabilities) -> Result<()> {
    let os_ok = caps.guest_os.contains(&profile.guest.os);
    let arch_ok = caps.guest_arch.contains(&profile.guest.arch);
    let mode_ok = caps.supports_mode(profile.vm.mode);
    if os_ok && arch_ok && mode_ok {
        return Ok(());
    }

    let supported: Vec<String> = caps
        .guest_os
        .iter()
        .flat_map(|os| caps.guest_arch.iter().map(move |arch| (os, arch)))
        .flat_map(|(os, arch)| caps.modes.iter().map(move |mode| (os, arch, mode)))
        .map(|(os, arch, mode)| {
            format!(
                "  - {}/{} {}",
                os_label(*os),
                arch_label(*arch),
                mode.as_str()
            )
        })
        .collect();
    let supported = if supported.is_empty() {
        "  (none)".to_string()
    } else {
        supported.join("\n")
    };

    let message = format!(
        "This environment requires {}/{} {}.\n\nThe current backend {:?} supports:\n{}",
        os_label(profile.guest.os),
        arch_label(profile.guest.arch),
        profile.vm.mode.as_str(),
        backend_name,
        supported,
    );
    Err(WargameError::Incompatible(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::profile::{
        GuestSpec, ImageSpec, ProvisionSpec, Resources, ToolsSpec, VmSettings, WorkspaceSpec,
    };

    fn profile_with(os: GuestOs, arch: GuestArch) -> Profile {
        Profile {
            name: "test".to_string(),
            description: String::new(),
            vm: VmSettings::default(),
            guest: GuestSpec {
                os,
                arch,
                distribution: None,
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
            modes: vec![ExecutionMode::Virtualize],
            networks: vec![NetworkMode::Isolated, NetworkMode::Nat],
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

    #[test]
    fn rejects_a_mode_the_backend_does_not_implement() {
        let mut profile = profile_with(GuestOs::Macos, GuestArch::Arm64);
        profile.vm.mode = ExecutionMode::Emulate;
        let err = check(&profile, "tart", &macos_arm64_caps()).unwrap_err();
        assert!(matches!(err, WargameError::Incompatible(_)));
    }

    #[test]
    fn a_profile_without_a_vm_section_resolves_to_tart_virtualize() {
        let profile = profile_with(GuestOs::Macos, GuestArch::Arm64);
        let resolved = resolve(&profile).unwrap();
        assert_eq!(resolved, ResolvedBackend::default());
    }

    #[test]
    fn capabilities_report_network_support() {
        let mut caps = macos_arm64_caps();
        assert!(caps.supports_network(NetworkMode::Isolated));
        caps.networks = vec![NetworkMode::Nat];
        assert!(!caps.supports_network(NetworkMode::Isolated));
    }

    #[test]
    fn backend_round_trips_through_its_name() {
        for backend in [Backend::Tart, Backend::Utm] {
            assert_eq!(Backend::from_str(backend.as_str()).unwrap(), backend);
        }
        assert!(Backend::from_str("qemu").is_err());
    }
}
