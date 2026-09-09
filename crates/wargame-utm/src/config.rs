//! Static configuration for the UTM backend, plus the pure mappings from
//! this project's vocabulary to UTM's.
//!
//! Every value here is traceable to `docs/utm-compatibility.md`: the
//! `utmctl` discovery order and the bundled path (§1), the minimum app
//! version (§1's "Decision: `doctor` requires UTM ≥ 4.3"), the network mode
//! names (§5) and the architecture/hypervisor rules (§2, T6).

use std::path::PathBuf;
use std::time::Duration;

use wargame_core::domain::{ExecutionMode, GuestArch, NetworkMode};

/// Where `utmctl` lives inside the app bundle -- the last of FR-6's three
/// discovery locations, and the one that works without Homebrew's symlink.
pub const BUNDLED_UTMCTL: &str = "/Applications/UTM.app/Contents/MacOS/utmctl";

/// The macOS application UTM's automation (Apple Events) grant belongs to.
pub const UTM_APP: &str = "UTM";

/// Where UTM keeps the `.utm` bundles it creates, relative to `$HOME`.
///
/// UTM is sandboxed (`com.apple.security.app-sandbox`), so its VMs -- and
/// therefore their qcow2 disks -- live in its own container, never in
/// `$WARGAME_HOME` (docs/utm-compatibility.md §1). The adapter must not try
/// to *read* anything in there: the directory is `drwx------` and listing it
/// fails with `Operation not permitted`. Its **volume**, though, is the one
/// a free-space check has to measure (task-7-phase-a-report.md §5 D1).
pub const UTM_VM_DIRECTORY: &str = "Library/Containers/com.utmapp.UTM/Data/Documents";

/// [`UTM_VM_DIRECTORY`] as an absolute path, when it exists on this host --
/// it does not until UTM has run once.
pub fn utm_vm_directory() -> Option<PathBuf> {
    utm_vm_directory_in(&PathBuf::from(std::env::var_os("HOME")?))
}

/// [`utm_vm_directory`] against a given home directory. Split out so it can
/// be exercised without depending on the machine the test runs on.
pub fn utm_vm_directory_in(home: &std::path::Path) -> Option<PathBuf> {
    let dir = home.join(UTM_VM_DIRECTORY);
    dir.is_dir().then_some(dir)
}

/// The lowest UTM release this adapter supports. 4.2 is where scripting can
/// first create and configure a VM at all, but Windows 11 is a release
/// requirement and needs the TPM/Secure Boot toggles 4.3 added, so 4.3 is
/// the single number to check (docs/utm-compatibility.md §1).
pub const MIN_UTM_VERSION: (u64, u64) = (4, 3);

/// The only UTM release this adapter was verified against.
pub const VERIFIED_UTM_VERSION: &str = "4.7.5";

/// What `doctor` says about UTM's `isolated` network. UTM's `host` mode is
/// `-netdev vmnet-host`, the same primitive Tart's isolated path uses, and
/// that much is verified; whether the *guest* is actually cut off from the
/// LAN and the internet needs an installed guest OS and is not
/// (docs/utm-compatibility.md §5). Never report this as plain "ok".
pub const ISOLATED_NETWORK_DETAIL: &str =
    "mechanism verified (UTM `host` = vmnet-host, host gets .1 on the guest segment); guest isolation unverified";

/// Static configuration for the UTM adapter, resolved from `[vm.utm]`.
#[derive(Debug, Clone)]
pub struct UtmConfig {
    /// An explicit `utmctl` path, tried first (FR-6's discovery order:
    /// config -> `PATH` -> [`BUNDLED_UTMCTL`]).
    pub utmctl: Option<String>,
    /// Display-name prefix for the VMs this app owns. Nothing outside it is
    /// ever stopped or deleted (spec.md §3 FR-5).
    pub vm_prefix: String,
    /// Display-name prefix for the build-time VMs: the installation VM
    /// `profile create` makes and the image VM `profile build` publishes.
    /// It starts with [`UtmConfig::vm_prefix`] so ownership checks still
    /// pass, but it is its own namespace so `prune` -- which matches
    /// sessions, not names -- never mistakes a build VM for an orphan.
    pub build_prefix: String,
    /// Boot timeout for `mode: emulate` guests.
    pub boot_timeout: Duration,
    /// Boot timeout for `mode: virtualize` guests.
    pub virtualize_boot_timeout: Duration,
    /// How long a graceful (`stop ... by force`, i.e. ACPI) stop may sit in
    /// `stopping` before escalating to `by kill` (T9: an x86_64 TCG guest at
    /// the GRUB prompt stayed there past 60 s).
    pub stop_timeout: Duration,
    /// How long to wait for the app to answer `utmctl version` after
    /// `open -a UTM` (T8).
    pub app_launch_timeout: Duration,
    /// How often `wait_ip` asks the guest agent for an address.
    pub ip_poll_interval: Duration,
    /// How often a stop polls `status` while waiting for `stopped`.
    pub status_poll_interval: Duration,
}

impl Default for UtmConfig {
    fn default() -> Self {
        UtmConfig {
            utmctl: None,
            vm_prefix: "wg-".to_string(),
            build_prefix: "wg-build-".to_string(),
            boot_timeout: Duration::from_secs(1800),
            virtualize_boot_timeout: Duration::from_secs(300),
            stop_timeout: Duration::from_secs(120),
            app_launch_timeout: Duration::from_secs(30),
            ip_poll_interval: Duration::from_secs(5),
            status_poll_interval: Duration::from_secs(1),
        }
    }
}

impl UtmConfig {
    /// The boot timeout for one execution mode.
    pub fn boot_timeout_for(&self, mode: ExecutionMode) -> Duration {
        match mode {
            ExecutionMode::Emulate => self.boot_timeout,
            ExecutionMode::Virtualize => self.virtualize_boot_timeout,
        }
    }

    /// Whether `name` is inside this app's namespace.
    ///
    /// Either prefix counts. They nest by default ([`Self::build_prefix`]
    /// starts with [`Self::vm_prefix`]), but the two are configured
    /// independently: a user who sets `vm_prefix` alone would otherwise
    /// leave every build VM -- the installation VM and the published images
    /// -- outside the namespace, and `profile stop|eject|delete` and the
    /// build's own shutdown would refuse to touch the VM they just made.
    pub fn owns_name(&self, name: &str) -> bool {
        name.starts_with(&self.vm_prefix) || name.starts_with(&self.build_prefix)
    }
}

/// UTM's `architecture` string for a guest architecture (§2's `make`).
pub fn architecture(arch: GuestArch) -> &'static str {
    match arch {
        GuestArch::Arm64 => "aarch64",
        GuestArch::X86_64 => "x86_64",
    }
}

/// UTM's `qemu network mode` for one of ours (§5).
///
/// `isolated` -> `host` (`-netdev vmnet-host`, no WAN routing) and `nat` ->
/// `shared` (`-netdev vmnet-shared`). `emulated` (libslirp) is deliberately
/// not reachable: it is not isolated, and port forwarding is a later task.
pub fn network_mode(mode: NetworkMode) -> &'static str {
    match mode {
        NetworkMode::Isolated => "host",
        NetworkMode::Nat => "shared",
    }
}

/// The display device to give a VM so UTM opens a window for it. A VM made
/// by `make` has none at all (§3), and an unknown hardware name is silently
/// replaced by the default (T11), so only names UTM documents are used.
pub fn display_hardware(arch: GuestArch) -> &'static str {
    match arch {
        GuestArch::Arm64 => "virtio-ramfb",
        GuestArch::X86_64 => "virtio-vga",
    }
}

/// `(major, minor, patch)` from a `utmctl version` line, ignoring anything
/// after the numbers.
pub fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim();
    let numeric: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = numeric.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0)).unwrap_or(0);
    Some((major, minor, patch))
}

/// Whether a parsed version is at least [`MIN_UTM_VERSION`].
pub fn meets_minimum(version: (u64, u64, u64)) -> bool {
    (version.0, version.1) >= MIN_UTM_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_modes_map_to_the_verified_utm_names() {
        assert_eq!(network_mode(NetworkMode::Isolated), "host");
        assert_eq!(network_mode(NetworkMode::Nat), "shared");
    }

    #[test]
    fn architectures_map_to_utms_names() {
        assert_eq!(architecture(GuestArch::Arm64), "aarch64");
        assert_eq!(architecture(GuestArch::X86_64), "x86_64");
    }

    #[test]
    fn versions_parse_and_compare_against_the_4_3_floor() {
        assert_eq!(parse_version("4.7.5\n"), Some((4, 7, 5)));
        assert_eq!(parse_version("4.3"), Some((4, 3, 0)));
        assert_eq!(parse_version("5"), Some((5, 0, 0)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("not a version"), None);

        assert!(meets_minimum((4, 7, 5)));
        assert!(meets_minimum((4, 3, 0)));
        assert!(!meets_minimum((4, 2, 9)));
        assert!(!meets_minimum((3, 9, 0)));
    }

    #[test]
    fn boot_timeout_follows_the_execution_mode() {
        let cfg = UtmConfig::default();
        assert_eq!(
            cfg.boot_timeout_for(ExecutionMode::Emulate),
            Duration::from_secs(1800)
        );
        assert_eq!(
            cfg.boot_timeout_for(ExecutionMode::Virtualize),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn ownership_is_by_prefix() {
        let cfg = UtmConfig::default();
        assert!(cfg.owns_name("wg-dh-out-of-bound-a1b2c3"));
        assert!(!cfg.owns_name("kindle-vm"));
        assert!(!cfg.owns_name("Windows 11"));
    }

    /// A build VM is still ours (so it can be stopped and deleted), but it
    /// is not in the session namespace.
    #[test]
    fn build_vms_are_owned_but_outside_the_session_namespace() {
        let cfg = UtmConfig::default();
        assert!(cfg.build_prefix.starts_with(&cfg.vm_prefix));
        assert_ne!(cfg.build_prefix, cfg.vm_prefix);
        assert!(cfg.owns_name("wg-build-kali-custom"));
        assert!(cfg.owns_name("wg-build-image-kali-custom"));
    }

    /// The two prefixes are configured independently. Setting only
    /// `vm_prefix` must not put this app's own build VMs out of reach of
    /// `profile stop|eject|delete` and of the build's own shutdown.
    #[test]
    fn a_custom_session_prefix_still_owns_the_default_build_prefix() {
        let cfg = UtmConfig {
            vm_prefix: "mine-".to_string(),
            ..UtmConfig::default()
        };
        assert!(cfg.owns_name("mine-dh-out-of-bound-a1b2c3"));
        assert!(cfg.owns_name("wg-build-kali-custom"));
        assert!(cfg.owns_name("wg-build-image-kali-custom"));
        assert!(!cfg.owns_name("kindle-vm"));
        assert!(!cfg.owns_name("Windows 11"));
    }

    /// ... and the other way round: a custom build prefix outside the
    /// session one is owned too.
    #[test]
    fn a_build_prefix_outside_the_session_prefix_is_still_owned() {
        let cfg = UtmConfig {
            build_prefix: "wgb-".to_string(),
            ..UtmConfig::default()
        };
        assert!(cfg.owns_name("wgb-kali-custom"));
        assert!(cfg.owns_name("wg-dh-out-of-bound-a1b2c3"));
        assert!(!cfg.owns_name("kindle-vm"));
    }
}
