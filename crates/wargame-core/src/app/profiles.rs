//! Profile discovery (DESIGN.md §3.1's `app/profiles.rs`; DESIGN.md §6):
//! a fixed built-in set (embedded into the `wargame` binary via
//! `include_str!` by the CLI crate) plus user overrides dropped into
//! `$WARGAME_HOME/profiles/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::domain::compat::ResolvedBackend;
use crate::domain::{BuildPhase, Profile};
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
        for (_, content) in self.user_yaml_files() {
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
        for (path, content) in self.user_yaml_files() {
            match Profile::from_yaml(&content) {
                Ok(profile) => out.push(profile),
                // Skipped, but never in silence: `--verbose` says which file
                // and why, and `get` repeats it for the name it claimed.
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping an unusable profile")
                }
            }
        }
        out
    }

    /// Every readable `*.yaml` directly under `user_dir`, as
    /// `(path, contents)`.
    fn user_yaml_files(&self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.user_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push((path, content));
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
        if let Some(profile) = self.merged().remove(name) {
            return Ok(profile);
        }
        // A user file that *declares* this name but was rejected is not a
        // missing profile: say what is wrong with it instead.
        self.user_profile_error(name)
            .unwrap_or_else(|| Err(WargameError::ProfileNotFound(name.to_string())))
    }

    /// The error a user `*.yaml` claiming `metadata.name == name` was
    /// rejected with, if there is one.
    fn user_profile_error(&self, name: &str) -> Option<Result<Profile>> {
        for (path, content) in self.user_yaml_files() {
            if declared_name(&content).as_deref() != Some(name) {
                continue;
            }
            if let Err(e) = Profile::from_yaml(&content) {
                tracing::debug!(path = %path.display(), error = %e, "rejected user profile");
                return Some(Err(e));
            }
        }
        None
    }

    pub fn list(&self) -> Vec<Profile> {
        self.merged().into_values().collect()
    }
}

/// The `metadata.name` a YAML document claims, without validating anything
/// else about it -- so a rejected profile can still be recognised by the
/// name the user typed on the command line.
fn declared_name(yaml: &str) -> Option<String> {
    serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .ok()?
        .get("metadata")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// Whether a profile has an image to launch from, and what to say about it.
///
/// One decision, made once: `profiles`, the TUI's picker and `doctor` all
/// render this rather than each re-deriving "the record names an image, the
/// backend still has it, and nothing has changed since".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReadiness {
    /// `"ready"`, `"rebuild needed"`, `"missing"`, `"not built"`,
    /// `"not created"`, or the phase a half-finished build stopped at -- the
    /// word a table shows.
    ///
    /// `"missing"` is the one that means something is wrong rather than
    /// undone: a build finished here and published an image, and the backend
    /// does not have it any more -- deleted in UTM or Tart behind wargame's
    /// back. It reads `ready` no longer (task-7-b2b-report.md D7).
    pub state: String,
    /// The longer line `doctor` shows: the image, or why there is none.
    pub detail: String,
    /// There is an image, the backend still has it, and neither the profile
    /// nor its scripts changed since it was built.
    pub ready: bool,
    /// Nothing has been set up for this profile at all -- no build record,
    /// so `profile create` is genuinely the next command.
    ///
    /// **Not** the same as "there is no image": a profile whose
    /// installation VM exists and whose OS install is unfinished has no
    /// image either, and `profile create` would refuse it with a source
    /// conflict (task-7-b2b-report.md D9). [`ImageReadiness::installer_vm`]
    /// is what that case has instead.
    pub needs_create: bool,
    /// The installation VM this profile already owns, `<name> (<id>)`, when
    /// `profile create` has run and there is still an OS to install in it.
    pub installer_vm: Option<String>,
    /// Nothing has been built for this profile in this home yet, and no
    /// build failed trying: there is no build record at all, or one that
    /// has not got past the manual OS install. It means "you have not set
    /// this up", not "something is broken" -- which is how `doctor` tells
    /// an untouched Kali template apart from a build that fell over.
    pub never_built: bool,
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
    ///
    /// `"unknown"` is the honest answer when the backend could not be asked
    /// at all -- a denied Automation grant, a UTM that will not launch. It
    /// is deliberately not `"missing"`: the image may be sitting there
    /// untouched, and telling the user to rebuild it would be worse than
    /// telling them nothing. [`ProfileOverview::image_error`] carries what
    /// the backend actually said.
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
    // "Nothing has been started here": no record at all, or one that is
    // still waiting for the user to finish the OS install by hand -- which
    // the domain calls a wait, not a failure.
    let never_built = matches!(
        status.phase,
        None | Some(BuildPhase::Created) | Some(BuildPhase::AwaitingInstall)
    );
    let installer_vm = status
        .installer_vm
        .as_ref()
        .map(|vm| format!("{} ({})", vm.name, vm.id));
    let Some(image) = &status.image else {
        return Ok(ImageReadiness {
            state: status
                .phase
                .as_ref()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "not created".to_string()),
            detail: match &installer_vm {
                Some(vm) => format!("{} -- installation VM {vm}", status.detail),
                None => status.detail,
            },
            ready: false,
            // Only a profile with no build record at all: `profile create`
            // refuses one that already has an installation VM.
            needs_create: status.phase.is_none(),
            installer_vm,
            never_built,
        });
    };
    // A name is not an image: ask the backend whether it is actually there.
    //
    // **"No" and "I cannot tell you" are different answers.** A denied
    // Automation grant, or a UTM that will not launch, used to be swallowed
    // into `false` -- which read as `missing` and told the user to rebuild
    // an image that was sitting there untouched. An unreadable backend is
    // reported as such instead: the state word becomes `unknown`, the
    // detail carries what the backend said, and the hint sends the user to
    // `doctor` rather than to a destructive rebuild.
    let backend = match ctx.backends.get(status.backend.as_str()) {
        Ok(vm) => vm,
        Err(e) => return Err((e.to_string(), e.hint())),
    };
    let exists = match backend.image_exists(image).await {
        Ok(exists) => exists,
        Err(e) => {
            let name = status.backend;
            return Err((
                format!(
                    "could not ask the {name} backend whether {} is still there: {e}",
                    status.detail
                ),
                Some(match e.hint() {
                    Some(hint) => {
                        format!("{hint}\nwargame doctor   # what the {name} backend reports")
                    }
                    None => format!("wargame doctor   # what the {name} backend reports"),
                }),
            ));
        }
    };
    Ok(ImageReadiness {
        state: match (exists, status.rebuild_needed) {
            // Built here and gone from the backend since: not something the
            // user has yet to do, something that has to be rebuilt.
            (false, _) if !never_built => "missing",
            (false, _) => "not built",
            (true, true) => "rebuild needed",
            (true, false) => "ready",
        }
        .to_string(),
        detail: if exists {
            status.detail
        } else if never_built {
            format!("{} is not ready", status.detail)
        } else {
            format!(
                "{} -- the {} backend no longer has it",
                status.detail, status.backend
            )
        },
        ready: exists && !status.rebuild_needed,
        needs_create: false,
        installer_vm,
        // A Tart profile always has an image *name*, so "not built" here
        // can equally mean "never built" or "built and then deleted"; the
        // build record is what tells them apart.
        never_built: !exists && never_built,
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

    /// A user profile that is rejected (an unusable `guest.user`, say) must
    /// say *why* rather than vanish from the store and turn every command
    /// that names it into "profile not found".
    #[test]
    fn a_rejected_user_profile_explains_itself_instead_of_going_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("broken.yaml"),
            r#"
apiVersion: wargame.vm/v1
kind: Profile
metadata:
  name: broken
guest:
  os: macos
  arch: arm64
  user: "ad'min"
image:
  base: base
resources:
  cpu: 1
  memory_gib: 1
  disk_gib: 1
workspace:
  root: /Users/admin/wargame
"#,
        )
        .unwrap();
        let store = ProfileStore::new(vec![], dir.path().to_path_buf());

        assert!(
            !store.list().iter().any(|p| p.name == "broken"),
            "an invalid profile is still not launchable"
        );
        let err = store.get("broken").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("guest.user"), "{msg}");
        assert!(msg.contains("broken"), "{msg}");
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
