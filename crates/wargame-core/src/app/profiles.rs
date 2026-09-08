//! Profile discovery (DESIGN.md §3.1's `app/profiles.rs`; DESIGN.md §6):
//! a fixed built-in set (embedded into the `wargame` binary via
//! `include_str!` by the CLI crate) plus user overrides dropped into
//! `$WARGAME_HOME/profiles/`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::domain::Profile;
use crate::error::{Result, WargameError};

/// Where profile definitions come from. A `*.yaml` file in `user_dir` whose
/// `metadata.name` matches a built-in profile overrides it; anything else in
/// `user_dir` is simply an additional profile.
pub struct ProfileStore {
    builtin: Vec<Profile>,
    user_dir: PathBuf,
}

impl ProfileStore {
    pub fn new(builtin: Vec<Profile>, user_dir: PathBuf) -> Self {
        ProfileStore { builtin, user_dir }
    }

    /// Parses every `*.yaml` file directly under `user_dir`. Unreadable or
    /// unparsable files are skipped rather than failing the whole lookup —
    /// a single bad user profile should not take every profile down.
    fn user_profiles(&self) -> Vec<Profile> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.user_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(profile) = Profile::from_yaml(&content) {
                out.push(profile);
            }
        }
        out
    }

    /// Built-ins first, then user profiles overwriting by name.
    fn merged(&self) -> BTreeMap<String, Profile> {
        let mut by_name = BTreeMap::new();
        for profile in &self.builtin {
            by_name.insert(profile.name.clone(), profile.clone());
        }
        for profile in self.user_profiles() {
            by_name.insert(profile.name.clone(), profile);
        }
        by_name
    }

    pub fn get(&self, name: &str) -> Result<Profile> {
        self.merged()
            .remove(name)
            .ok_or_else(|| WargameError::ProfileNotFound(name.to_string()))
    }

    pub fn list(&self) -> Vec<Profile> {
        self.merged().into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        GuestArch, GuestOs, GuestSpec, ImageSpec, Resources, VmSettings, WorkspaceSpec,
    };

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            description: String::new(),
            vm: VmSettings::default(),
            guest: GuestSpec {
                os: GuestOs::Macos,
                arch: GuestArch::Arm64,
                distribution: None,
                user: "admin".to_string(),
                password: None,
            },
            image: ImageSpec {
                base: "base".to_string(),
            },
            resources: Resources {
                cpu: 2,
                memory_mib: 4096,
                disk_gib: 20,
            },
            provision: Default::default(),
            tools: Default::default(),
            workspace: WorkspaceSpec {
                root: "/Users/admin/wargame".to_string(),
            },
        }
    }

    #[test]
    fn lists_and_gets_builtin_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(
            vec![profile("macos-minimal-arm64"), profile("macos-re-arm64")],
            dir.path().to_path_buf(),
        );
        assert_eq!(store.list().len(), 2);
        assert_eq!(store.get("macos-re-arm64").unwrap().name, "macos-re-arm64");
    }

    #[test]
    fn unknown_profile_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(vec![], dir.path().to_path_buf());
        assert!(matches!(
            store.get("nope").unwrap_err(),
            WargameError::ProfileNotFound(_)
        ));
    }

    #[test]
    fn missing_user_dir_is_not_an_error() {
        let store = ProfileStore::new(vec![profile("p")], PathBuf::from("/no/such/dir"));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn user_profile_overrides_builtin_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut overridden = profile("macos-minimal-arm64");
        overridden.description = "user override".to_string();
        let yaml = format!(
            "apiVersion: wargame.vm/v1\nkind: Profile\nmetadata:\n  name: {}\n  description: \"user override\"\nguest:\n  os: macos\n  arch: arm64\n  user: admin\nimage:\n  base: base\nresources:\n  cpu: 2\n  memory_gib: 4\n  disk_gib: 20\nworkspace:\n  root: /Users/admin/wargame\n",
            overridden.name
        );
        std::fs::write(dir.path().join("macos-minimal-arm64.yaml"), yaml).unwrap();

        let store = ProfileStore::new(
            vec![profile("macos-minimal-arm64")],
            dir.path().to_path_buf(),
        );
        let got = store.get("macos-minimal-arm64").unwrap();
        assert_eq!(got.description, "user override");
        assert_eq!(store.list().len(), 1);
    }
}
