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
pub mod profiles;
pub mod search;

use std::sync::Arc;

use crate::config::Config;
use crate::infra::fs::AppDirs;
use crate::infra::process::ProcessRunner;
use crate::ports::{SessionRepository, WargameProvider};

pub use backends::{
    provider_for_profile, provider_for_session, resolve_backend, transport_for_session,
    BackendRegistry, TransportRegistry,
};
pub use console::{open_gui, ssh_command};
pub use doctor::{doctor, Check};
pub use launch::{launch, LaunchEvent, LaunchRequest};
pub use lifecycle::{delete, inspect, keep, list_sessions, prune, resolve_session, resume, stop};
pub use profiles::ProfileStore;
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
    pub profiles: ProfileStore,
    pub runner: Arc<dyn ProcessRunner>,
    /// The trusted guest bootstrap script (`guest/bootstrap.sh`), embedded
    /// into the `wargame` binary by the CLI crate via `include_str!` and
    /// handed down here so `app::launch` never has to read it from disk.
    pub bootstrap_script: &'static str,
}
