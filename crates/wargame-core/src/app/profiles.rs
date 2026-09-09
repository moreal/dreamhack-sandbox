//! Profile discovery (DESIGN.md §3.1's `app/profiles.rs`; DESIGN.md §6):
//! a fixed built-in set (embedded into the `wargame` binary via
//! `include_str!` by the CLI crate) plus user overrides dropped into
//! `$WARGAME_HOME/profiles/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::domain::compat::ResolvedBackend;
use crate::domain::Profile;
use crate::error::{Result, WargameError};

use super::AppContext;

/// Where profile definitions come from. A `*.yaml` file in `user_dir` whose
/// `metadata.name` matches a built-in profile overrides it; anything else in
/// `user_dir` is simply an additional profile.
pub struct ProfileStore {
    builtin: Vec<Profile>,
    /// `(profile name, YAML text)` for the built-in profiles, so
    /// `profile init` can copy a template verbatim -- comments included --
    /// instead of re-serializing a parsed [`Profile`] (spec.md §4).
    builtin_yaml: Vec<(String, String)>,
    user_dir: PathBuf,
}

impl ProfileStore {
    pub fn new(builtin: Vec<Profile>, user_dir: PathBuf) -> Self {
        ProfileStore {
            builtin,
            builtin_yaml: Vec::new(),
            user_dir,
        }
    }

    /// Attaches the built-in YAML texts (the CLI crate embeds them).
    pub fn with_builtin_yaml(mut self, yamls: Vec<(String, String)>) -> Self {
        self.builtin_yaml = yamls;
        self
    }

    /// Where user profiles and their scripts live
    /// (`$WARGAME_HOME/profiles`).
    pub fn user_dir(&self) -> &Path {
        &self.user_dir
    }

    /// The YAML document a profile is defined by: the user's copy if there
    /// is one (so `profile init` templates from what the user actually
    /// sees), else the built-in text.
    pub fn yaml_for(&self, name: &str) -> Result<String> {
        let Ok(entries) = std::fs::read_dir(&self.user_dir) else {
            return self.builtin_yaml_for(name);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if matches!(Profile::from_yaml(&content), Ok(p) if p.name == name) {
                return Ok(content);
            }
        }
        self.builtin_yaml_for(name)
    }

    fn builtin_yaml_for(&self, name: &str) -> Result<String> {
        self.builtin_yaml
            .iter()
            .find(|(profile, _)| profile == name)
            .map(|(_, yaml)| yaml.clone())
            .ok_or_else(|| WargameError::ProfileNotFound(name.to_string()))
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

/// Whether a profile has an image to launch from, and what to say about it.
///
/// One decision, made once: `profiles`, the TUI's picker and `doctor` all
/// render this rather than each re-deriving "the record names an image, the
/// backend still has it, and nothing has changed since".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReadiness {
    /// `"ready"`, `"rebuild needed"`, `"not built"`, `"not created"`, or the
    /// phase a half-finished build stopped at -- the word a table shows.
    pub state: String,
    /// The longer line `doctor` shows: the image, or why there is none.
    pub detail: String,
    /// There is an image, the backend still has it, and neither the profile
    /// nor its scripts changed since it was built.
    pub ready: bool,
    /// Nothing is published for this profile, so `profile create` comes
    /// before `profile build`.
    pub needs_create: bool,
}

/// A profile plus the state spec.md §3 FR-1 asks every profile view to show:
/// what its OS, architecture, backend and execution mode resolved to, and
/// whether it has an image to launch from.
///
/// This is what `wargame profiles`, `wargame doctor` and the TUI's profile
/// picker all render, so the three cannot drift apart.
#[derive(Debug, Clone)]
pub struct ProfileOverview {
    pub profile: Profile,
    /// What the profile's OS/arch/mode resolved to. `None` when it is not a
    /// supported combination at all -- [`ProfileOverview::unsupported`] then
    /// says why.
    pub resolved: Option<ResolvedBackend>,
    unsupported: Option<String>,
    /// The image state, or `(detail, hint)` of whatever prevented reading it.
    image: std::result::Result<ImageReadiness, (String, Option<String>)>,
}

impl ProfileOverview {
    /// `"kali/x86_64"`. Falls back to what the profile itself declares when
    /// the combination does not resolve, so an unsupported profile still
    /// shows what it asked for.
    pub fn guest_label(&self) -> String {
        match &self.resolved {
            Some(resolved) => resolved.guest_label(),
            None => format!(
                "{}/{}",
                crate::domain::compat::guest_key(
                    self.profile.guest.os,
                    self.profile.guest.distribution_key().as_deref(),
                ),
                crate::domain::compat::arch_label(self.profile.guest.arch),
            ),
        }
    }

    /// `"utm/emulate"` -- the *resolved* backend, not the profile's
    /// `backend: auto`, which is what FR-1 asks to be shown.
    pub fn backend_label(&self) -> String {
        match &self.resolved {
            Some(resolved) => resolved.backend_label(),
            None => format!("{}/{}", self.profile.vm.backend, self.profile.vm.mode),
        }
    }

    /// The image state, or `None` when it could not be determined at all.
    pub fn image(&self) -> Option<&ImageReadiness> {
        self.image.as_ref().ok()
    }

    /// `(detail, hint)` of what prevented reading the image state.
    pub fn image_error(&self) -> Option<(&str, Option<&str>)> {
        self.image
            .as_ref()
            .err()
            .map(|(detail, hint)| (detail.as_str(), hint.as_deref()))
    }

    /// The image-readiness word, or `None` when it is unknown.
    pub fn image_state(&self) -> Option<&str> {
        self.image().map(|image| image.state.as_str())
    }

    /// The same, for a table cell that has to print something.
    pub fn image_label(&self) -> &str {
        self.image_state().unwrap_or("unknown")
    }

    /// Why this profile cannot run at all, when that is the reason its state
    /// is unknown.
    pub fn unsupported(&self) -> Option<&str> {
        self.unsupported.as_deref()
    }

    /// One line naming everything above, for a status bar or a hint.
    pub fn describe(&self) -> String {
        format!(
            "{}  ·  {}  ·  image: {}",
            self.guest_label(),
            self.backend_label(),
            self.image_label(),
        )
    }
}

/// Builds one profile's [`ProfileOverview`], asking the backend whether the
/// image it names is actually there (a Tart image the user deleted behind
/// wargame's back reads as "not built", not "ready").
pub async fn overview(ctx: &AppContext, profile: &Profile) -> ProfileOverview {
    let (resolved, unsupported) = match super::backends::resolve_backend(profile) {
        Ok(resolved) => (Some(resolved), None),
        Err(e) => (None, Some(e.to_string())),
    };
    ProfileOverview {
        image: image_readiness(ctx, profile).await,
        profile: profile.clone(),
        resolved,
        unsupported,
    }
}

/// Every profile the store knows, in its stable order.
pub async fn overviews(ctx: &AppContext) -> Vec<ProfileOverview> {
    let mut out = Vec::new();
    for profile in ctx.profiles.list() {
        out.push(overview(ctx, &profile).await);
    }
    out
}

async fn image_readiness(
    ctx: &AppContext,
    profile: &Profile,
) -> std::result::Result<ImageReadiness, (String, Option<String>)> {
    let status = super::profile_build::image_status(ctx, profile)
        .await
        .map_err(|e| (e.to_string(), e.hint()))?;
    let Some(image) = &status.image else {
        return Ok(ImageReadiness {
            state: status
                .phase
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "not created".to_string()),
            detail: status.detail,
            ready: false,
            needs_create: true,
        });
    };
    // A name is not an image: ask the backend whether it is actually there.
    let exists = match ctx.backends.get(status.backend.as_str()) {
        Ok(vm) => vm.image_exists(image).await.unwrap_or(false),
        Err(e) => return Err((e.to_string(), e.hint())),
    };
    Ok(ImageReadiness {
        state: match (exists, status.rebuild_needed) {
            (false, _) => "not built",
            (true, true) => "rebuild needed",
            (true, false) => "ready",
        }
        .to_string(),
        detail: if exists {
            status.detail
        } else {
            format!("{} is not ready", status.detail)
        },
        ready: exists && !status.rebuild_needed,
        needs_create: false,
    })
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
