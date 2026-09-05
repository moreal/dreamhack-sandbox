//! Both shipped profile definitions parse via `Profile::from_yaml` (PLAN.md
//! Task 6; DESIGN.md §6, MVP.md §13).

use std::path::PathBuf;

use wargame_core::domain::profile::Profile;

fn read_profile(filename: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles")
        .join(filename);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn macos_minimal_arm64_parses() {
    let profile = Profile::from_yaml(&read_profile("macos-minimal-arm64.yaml")).unwrap();
    assert_eq!(profile.name, "macos-minimal-arm64");
    assert_eq!(profile.provision.scripts, vec!["scripts/macos/common.sh"]);
    assert_eq!(profile.image_name(), "wargame-profile-macos-minimal-arm64");
}

#[test]
fn macos_re_arm64_parses() {
    let profile = Profile::from_yaml(&read_profile("macos-re-arm64.yaml")).unwrap();
    assert_eq!(profile.name, "macos-re-arm64");
    assert_eq!(profile.provision.scripts.len(), 2);
    assert_eq!(
        profile.provision.scripts,
        vec!["scripts/macos/common.sh", "scripts/macos/reversing.sh"]
    );
    assert_eq!(profile.tools.verify.len(), 3);
    assert_eq!(profile.image_name(), "wargame-profile-macos-re-arm64");
}
