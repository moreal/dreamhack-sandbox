//! Compile-time embedded profile YAMLs, their provisioning scripts, and the
//! guest bootstrap script (DESIGN.md §6, §7; PLAN.md Task 7). These are
//! baked into the `wargame` binary itself so `nix build`'s output needs no
//! separate `profiles/` or `guest/` directory alongside it;
//! [`crate::wiring::build_context`] also copies the YAMLs out to
//! `$WARGAME_HOME/profiles/` on first run so users can edit or add to them,
//! and (re)writes the scripts under `$WARGAME_HOME/profiles/scripts/` on
//! every run so `profile build` always provisions with the scripts that
//! match the running binary.

use wargame_core::domain::Profile;
use wargame_core::error::Result;

const MACOS_MINIMAL_ARM64_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../profiles/macos-minimal-arm64.yaml"
));
const MACOS_RE_ARM64_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../profiles/macos-re-arm64.yaml"
));
const KALI_ARM64_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../profiles/kali-arm64.yaml"
));
const KALI_X86_64_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../profiles/kali-x86_64.yaml"
));
const WINDOWS_X86_64_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../profiles/windows-x86_64.yaml"
));

/// `(relative path under `$WARGAME_HOME/profiles/`, content)` for every
/// built-in provisioning script, i.e. the paths a profile's
/// `provision.scripts` entries refer to.
pub fn builtin_provision_scripts() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "scripts/macos/common.sh",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/scripts/macos/common.sh"
            )),
        ),
        (
            "scripts/macos/reversing.sh",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/scripts/macos/reversing.sh"
            )),
        ),
        (
            "scripts/kali/common.sh",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/scripts/kali/common.sh"
            )),
        ),
        (
            "scripts/windows/common.ps1",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/scripts/windows/common.ps1"
            )),
        ),
        // Not run by any built-in profile: the commented starting point
        // spec.md §4's flow tells users to copy when they add their own
        // tools.
        (
            "scripts/custom/my-tools.sh",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/scripts/custom/my-tools.sh"
            )),
        ),
        (
            "scripts/custom/my-tools.ps1",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/scripts/custom/my-tools.ps1"
            )),
        ),
    ]
}

/// The trusted guest bootstrap scripts (`guest/bootstrap.sh` for macOS and
/// Kali, `guest/bootstrap.ps1` for Windows), handed to
/// `wargame-core::app::launch` via [`wargame_core::app::AppContext::bootstrap`].
pub const BOOTSTRAP_SH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../guest/bootstrap.sh"
));

pub const BOOTSTRAP_PS1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../guest/bootstrap.ps1"
));

/// `(filename, content)` for every built-in profile YAML, in a stable
/// order -- used by [`crate::wiring::build_context`] to seed
/// `$WARGAME_HOME/profiles/` on first run.
pub fn builtin_profile_yamls() -> Vec<(&'static str, &'static str)> {
    vec![
        ("macos-minimal-arm64.yaml", MACOS_MINIMAL_ARM64_YAML),
        ("macos-re-arm64.yaml", MACOS_RE_ARM64_YAML),
        ("kali-arm64.yaml", KALI_ARM64_YAML),
        ("kali-x86_64.yaml", KALI_X86_64_YAML),
        ("windows-x86_64.yaml", WINDOWS_X86_64_YAML),
    ]
}

/// `(profile name, YAML text)` for every built-in profile, for
/// [`wargame_core::app::ProfileStore::with_builtin_yaml`] -- what `profile
/// init` copies a template from.
pub fn builtin_profile_sources() -> Result<Vec<(String, String)>> {
    builtin_profile_yamls()
        .into_iter()
        .map(|(_, yaml)| Profile::from_yaml(yaml).map(|p| (p.name, yaml.to_string())))
        .collect()
}

/// The built-in profiles, parsed once at wiring time and handed to
/// [`wargame_core::app::ProfileStore::new`] as the always-present base set.
pub fn builtin_profiles() -> Result<Vec<Profile>> {
    builtin_profile_yamls()
        .into_iter()
        .map(|(_, yaml)| Profile::from_yaml(yaml))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use wargame_core::domain::{BackendChoice, ExecutionMode, GuestArch, GuestOs, GuestShell};

    #[test]
    fn every_builtin_profile_parses() {
        let profiles = builtin_profiles().unwrap();
        assert_eq!(profiles.len(), 5);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"macos-minimal-arm64"));
        assert!(names.contains(&"macos-re-arm64"));
        assert!(names.contains(&"kali-arm64"));
        assert!(names.contains(&"kali-x86_64"));
        assert!(names.contains(&"windows-x86_64"));
    }

    /// spec.md §4's template values, which `profile init` copies verbatim.
    #[test]
    fn the_multi_os_templates_carry_the_specs_values() {
        let by_name = |name: &str| {
            builtin_profiles()
                .unwrap()
                .into_iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} is not a built-in profile"))
        };

        let kali_arm = by_name("kali-arm64");
        assert_eq!(kali_arm.vm.backend, BackendChoice::Utm);
        assert_eq!(kali_arm.vm.mode, ExecutionMode::Virtualize);
        assert_eq!(kali_arm.guest.os, GuestOs::Linux);
        assert_eq!(kali_arm.guest.arch, GuestArch::Arm64);
        assert_eq!(kali_arm.guest.distribution_key().as_deref(), Some("kali"));
        assert_eq!(kali_arm.guest.user, "kali");
        assert_eq!(kali_arm.workspace.root, "/home/kali/wargame");
        assert_eq!(kali_arm.resources.cpu, 4);
        assert_eq!(kali_arm.resources.memory_mib, 8192);
        assert_eq!(kali_arm.resources.disk_gib, 80);
        assert!(kali_arm.image.base.is_empty(), "UTM has no base image");

        let kali_x86 = by_name("kali-x86_64");
        assert_eq!(kali_x86.vm.mode, ExecutionMode::Emulate);
        assert_eq!(kali_x86.guest.arch, GuestArch::X86_64);

        let windows = by_name("windows-x86_64");
        assert_eq!(windows.vm.mode, ExecutionMode::Emulate);
        assert_eq!(windows.guest.os, GuestOs::Windows);
        assert_eq!(windows.guest.arch, GuestArch::X86_64);
        assert_eq!(windows.guest.user, "wargame");
        assert_eq!(windows.workspace.root, "C:\\wargame");
        assert_eq!(
            windows.provision.scripts,
            vec!["scripts/windows/common.ps1".to_string()]
        );

        // Every template resolves to a backend, i.e. is a supported
        // combination (spec.md §3 FR-1).
        for profile in builtin_profiles().unwrap() {
            wargame_core::domain::compat::resolve(&profile)
                .unwrap_or_else(|e| panic!("{} does not resolve: {e}", profile.name));
        }
    }

    #[test]
    fn builtin_profile_sources_are_keyed_by_profile_name() {
        let sources = builtin_profile_sources().unwrap();
        assert_eq!(sources.len(), 5);
        let (_, yaml) = sources
            .iter()
            .find(|(name, _)| name == "kali-arm64")
            .unwrap();
        assert!(yaml.contains("name: kali-arm64"));
    }

    #[test]
    fn every_builtin_profile_script_is_embedded() {
        let embedded: Vec<&str> = builtin_provision_scripts()
            .into_iter()
            .map(|(path, content)| {
                if path.ends_with(".ps1") {
                    assert!(content.starts_with("# wargame-vm"), "{path}");
                } else {
                    assert!(content.starts_with("#!/usr/bin/env bash"), "{path}");
                }
                path
            })
            .collect();
        for profile in builtin_profiles().unwrap() {
            for script in &profile.provision.scripts {
                assert!(
                    embedded.contains(&script.as_str()),
                    "{}: {script} is not embedded",
                    profile.name
                );
            }
        }
    }

    /// spec.md §3 FR-3 + the Task 5 review: `tools.verify` entries are argv,
    /// and a PowerShell guest runs argv through
    /// [`wargame_core::infra::PowerShellCommand::from_argv`], which quotes
    /// *every* token. A native executable takes those as plain strings, but
    /// a **cmdlet** binds a parameter only from a bare `-Name` token, so a
    /// quoted `'-Path'` would be bound positionally and the command would
    /// fail for a reason no log makes obvious. Every Windows verify line we
    /// ship therefore has to start with a native executable.
    #[test]
    fn windows_verify_commands_are_native_executables_not_cmdlets() {
        for profile in builtin_profiles().unwrap() {
            if GuestShell::for_os(profile.guest.os) != GuestShell::PowerShell {
                continue;
            }
            assert!(
                !profile.tools.verify.is_empty(),
                "{}: a Windows profile must verify its tools",
                profile.name
            );
            for check in &profile.tools.verify {
                let argv: Vec<&str> = check.split_whitespace().collect();
                let program = *argv
                    .first()
                    .unwrap_or_else(|| panic!("{}: empty verify entry", profile.name));
                assert!(
                    !looks_like_cmdlet(program),
                    "{}: {check:?} starts with the cmdlet/alias {program:?}; \
                     use a native executable (PowerShell binds a quoted \
                     parameter name positionally)",
                    profile.name
                );
            }
        }
    }

    /// Whether `name` would resolve to a cmdlet or one of its aliases rather
    /// than an executable on `PATH`.
    ///
    /// Two rules, both from PowerShell's own conventions: a cmdlet is named
    /// `Verb-Noun`, and Windows PowerShell ships a fixed set of Unix-looking
    /// aliases (`curl` and `wget` are `Invoke-WebRequest`, `sc` is
    /// `Set-Content`, ...) that shadow the executables of the same name.
    fn looks_like_cmdlet(name: &str) -> bool {
        const SHADOWED_BY_AN_ALIAS: &[&str] = &[
            "cat", "cd", "cp", "curl", "diff", "dir", "echo", "kill", "ls", "man", "mount", "mv",
            "ps", "pwd", "rm", "rmdir", "sc", "set", "sleep", "sort", "tee", "type", "wget",
            "where", "write",
        ];
        if SHADOWED_BY_AN_ALIAS.contains(&name.to_ascii_lowercase().as_str()) {
            return true;
        }
        // `Verb-Noun`, e.g. `Get-ChildItem`: capitalised on both sides of a
        // single hyphen, letters only.
        match name.split_once('-') {
            Some((verb, noun)) => {
                !verb.is_empty()
                    && !noun.is_empty()
                    && verb.chars().all(|c| c.is_ascii_alphabetic())
                    && noun.chars().all(|c| c.is_ascii_alphabetic())
                    && verb.starts_with(|c: char| c.is_ascii_uppercase())
                    && noun.starts_with(|c: char| c.is_ascii_uppercase())
            }
            None => false,
        }
    }

    #[test]
    fn the_cmdlet_heuristic_tells_executables_and_cmdlets_apart() {
        for cmdlet in ["Get-ChildItem", "Set-Content", "sc", "curl", "LS"] {
            assert!(looks_like_cmdlet(cmdlet), "{cmdlet}");
        }
        for native in [
            "python",
            "git",
            "7z",
            "sc.exe",
            "ssh",
            "powershell",
            "python3",
        ] {
            assert!(!looks_like_cmdlet(native), "{native}");
        }
    }

    #[test]
    fn bootstrap_scripts_are_embedded() {
        assert!(BOOTSTRAP_SH.starts_with("#!/usr/bin/env bash"));
        assert!(BOOTSTRAP_PS1.starts_with("# wargame-vm"));
        // Both halves of the workspace contract, so a Windows guest gets the
        // same layout a POSIX one does (spec.md §3 FR-4).
        for dir in ["incoming", "work", "notes", "output"] {
            assert!(BOOTSTRAP_SH.contains(dir), "bootstrap.sh: {dir}");
            assert!(BOOTSTRAP_PS1.contains(dir), "bootstrap.ps1: {dir}");
        }
    }
}
