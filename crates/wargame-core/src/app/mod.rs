//! Application services (DESIGN.md §3.1's `app/`): the use-case layer the
//! CLI and TUI both call through. Every function here is built entirely on
//! ports (`WargameProvider`, `VmProvider`, `GuestTransport`,
//! `SessionRepository`, `ProcessRunner`), so it runs identically against a
//! real Tart/Dreamhack/SSH stack or against `crate::testing`'s fakes.

pub mod backends;
pub mod console;
pub mod doctor;
pub mod launch;
pub mod lifecycle;
pub mod profile_build;
pub mod profiles;
pub mod search;

use std::sync::Arc;

use crate::config::Config;
use crate::infra::fs::AppDirs;
use crate::infra::process::ProcessRunner;
use crate::ports::{BuildRecordRepository, DiskSpace, SessionRepository, WargameProvider};

pub use backends::{
    provider_for_profile, provider_for_session, resolve_backend, transport_for_session,
    BackendRegistry, TransportRegistry,
};
pub use console::{open_gui, ssh_command};
pub use doctor::{doctor, doctor_profile, Check};
pub use launch::{launch, LaunchEvent, LaunchRequest};
pub use lifecycle::{delete, inspect, keep, list_sessions, prune, resolve_session, resume, stop};
pub use profile_build::{
    build as build_profile_image, console as open_profile_console, create as create_profile_vm,
    image_status, init as init_profile, launch_image, BuilderRegistry, CreateOutcome, CreateSource,
    InitOutcome, ProfileImageStatus,
};
pub use profiles::{
    overview as profile_overview, overviews as profile_overviews, ImageReadiness, ProfileOverview,
    ProfileStore,
};
pub use search::{open, search};

/// Everything an application service needs to do its work (DESIGN.md §7's
/// composition root builds exactly one of these; the CLI's commands and the
/// TUI's effect runner both call the functions in this module through it).
pub struct AppContext {
    pub config: Config,
    pub dirs: AppDirs,
    pub wargame: Arc<dyn WargameProvider>,
    /// Every wired VM backend. A launch picks one from the profile
    /// (`app::backends::provider_for_profile`); everything else picks the
    /// one the session recorded (`app::backends::provider_for_session`).
    pub backends: BackendRegistry,
    /// The guest transports, selected per endpoint shell.
    pub transports: TransportRegistry,
    pub repo: Arc<dyn SessionRepository>,
    /// The app-owned build records (`$WARGAME_HOME/builds/<profile>`): what
    /// `profile create`/`profile build` produced, and the only place a UTM
    /// profile's launchable image is written down (spec.md §5 item 5).
    pub builds: Arc<dyn BuildRecordRepository>,
    /// The per-backend image builders `profile build` runs through.
    pub builders: BuilderRegistry,
    /// Free-space lookups, behind a port so the checks that refuse a build
    /// or a launch can be tested without filling a disk.
    pub disk: Arc<dyn DiskSpace>,
    pub profiles: ProfileStore,
    pub runner: Arc<dyn ProcessRunner>,
    /// The trusted guest bootstrap scripts, embedded into the `wargame`
    /// binary by the CLI crate via `include_str!` and handed down here so
    /// `app::launch` never has to read them from disk.
    pub bootstrap: BootstrapScripts,
}

/// The trusted guest bootstrap scripts, one per guest shell
/// (`guest/bootstrap.sh` and `guest/bootstrap.ps1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapScripts {
    pub posix: &'static str,
    pub powershell: &'static str,
}

impl BootstrapScripts {
    pub fn new(posix: &'static str, powershell: &'static str) -> Self {
        BootstrapScripts { posix, powershell }
    }

    /// The script a guest with this shell runs.
    pub fn for_shell(&self, shell: crate::domain::GuestShell) -> &'static str {
        match shell {
            crate::domain::GuestShell::Posix => self.posix,
            crate::domain::GuestShell::PowerShell => self.powershell,
        }
    }
}
