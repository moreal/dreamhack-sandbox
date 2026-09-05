//! Compile-time embedded profile YAMLs and the guest bootstrap script
//! (DESIGN.md §6, §7; PLAN.md Task 7). These are baked into the `wargame`
//! binary itself so `nix build`'s output needs no separate `profiles/` or
//! `guest/` directory alongside it; [`crate::wiring::build_context`] also
//! copies the YAMLs out to `$WARGAME_HOME/profiles/` on first run so users
//! can edit or add to them.

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

/// The trusted guest bootstrap script (`guest/bootstrap.sh`), handed to
/// `wargame-core::app::launch` via [`wargame_core::app::AppContext::bootstrap_script`].
pub const BOOTSTRAP_SH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../guest/bootstrap.sh"
));

/// `(filename, content)` for every built-in profile YAML, in a stable
/// order -- used by [`crate::wiring::build_context`] to seed
/// `$WARGAME_HOME/profiles/` on first run.
pub fn builtin_profile_yamls() -> Vec<(&'static str, &'static str)> {
    vec![
        ("macos-minimal-arm64.yaml", MACOS_MINIMAL_ARM64_YAML),
        ("macos-re-arm64.yaml", MACOS_RE_ARM64_YAML),
    ]
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

    #[test]
    fn both_builtin_profiles_parse() {
        let profiles = builtin_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"macos-minimal-arm64"));
        assert!(names.contains(&"macos-re-arm64"));
    }

    #[test]
    fn bootstrap_script_is_embedded() {
        assert!(BOOTSTRAP_SH.starts_with("#!/usr/bin/env bash"));
    }
}
