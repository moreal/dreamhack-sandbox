//! Image-name helpers (DESIGN.md §5).

use crate::list::TartVm;

/// Whether `name` looks like an OCI reference (`registry/repo:tag`) rather
/// than a bare local Tart VM/image name.
pub fn is_oci(name: &str) -> bool {
    name.contains('/') || name.contains(':')
}

/// Whether `vms` (from `tart list`) already has an entry named exactly
/// `image` — OCI references and local names both compare by exact string
/// match, matching how `tart list` prints them.
pub fn contains_image(vms: &[TartVm], image: &str) -> bool {
    vms.iter().any(|vm| vm.name == image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(name: &str) -> TartVm {
        TartVm {
            name: name.to_string(),
            source: "local".to_string(),
            state: "stopped".to_string(),
            running: false,
            size_gib: 1,
            disk_gib: 1,
        }
    }

    #[test]
    fn is_oci_detects_registry_refs() {
        assert!(is_oci("ghcr.io/cirruslabs/macos-sequoia-vanilla:latest"));
        assert!(is_oci("localhost:5000/foo"));
        assert!(!is_oci("wargame-profile-macos-re-arm64"));
    }

    #[test]
    fn contains_image_matches_exact_name() {
        let vms = vec![vm("wargame-profile-macos-re-arm64"), vm("wg-abc")];
        assert!(contains_image(&vms, "wargame-profile-macos-re-arm64"));
        assert!(!contains_image(&vms, "wargame-profile-other"));
    }

    #[test]
    fn contains_image_compares_oci_names_exactly() {
        let vms = vec![vm("ghcr.io/cirruslabs/macos-sequoia-vanilla:latest")];
        assert!(contains_image(
            &vms,
            "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest"
        ));
        assert!(!contains_image(
            &vms,
            "ghcr.io/cirruslabs/macos-sequoia-vanilla:2026"
        ));
    }
}
