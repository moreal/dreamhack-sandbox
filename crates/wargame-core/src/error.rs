//! User-facing errors (MVP.md §23) and the crate-wide `Result` alias.

use crate::domain::session::SessionState;

/// The single error type returned from every fallible operation in `wargame-core`
/// and the crates that build on it.
///
/// Every variant is meant to be shown to the user; use [`WargameError::hint`] to
/// surface an actionable next step alongside the message.
#[derive(Debug, thiserror::Error)]
pub enum WargameError {
    #[error("authentication required for {provider}")]
    AuthenticationRequired { provider: String },
    #[error("authentication failed: {reason}")]
    AuthenticationFailed { reason: String },
    #[error("provider {provider} unavailable: {reason}")]
    ProviderUnavailable { provider: String, reason: String },
    #[error("provider {provider} response changed: {what}")]
    ProviderChanged { provider: String, what: String },
    #[error("challenge {0} not found")]
    ChallengeNotFound(String),
    #[error("download failed for {filename}: {reason}")]
    DownloadFailed { filename: String, reason: String },
    #[error("hash or write failure: {0}")]
    HashOrWrite(String),
    #[error("incompatible environment: {0}")]
    Incompatible(String),
    #[error("image {image} missing for profile {profile}")]
    ImageMissing { profile: String, image: String },
    #[error("insufficient resources: {0}")]
    InsufficientResources(String),
    #[error("VM backend {backend} unavailable: {reason}")]
    BackendUnavailable { backend: String, reason: String },
    #[error("VM creation failed: {0}")]
    VmCreationFailed(String),
    #[error("guest boot timed out after {0:?}")]
    GuestBootTimeout(std::time::Duration),
    #[error("guest bootstrap failed: {0}")]
    GuestBootstrapFailed(String),
    #[error("copy into guest failed: {0}")]
    CopyFailed(String),
    #[error("network unavailable: {0}")]
    NetworkUnavailable(String),
    #[error("cleanup failed: {0}")]
    CleanupFailed(String),
    #[error("session {0} not found")]
    SessionNotFound(String),
    #[error("session reference {needle:?} is ambiguous; it matches: {}", candidates.join(", "))]
    AmbiguousSession {
        needle: String,
        candidates: Vec<String>,
    },
    #[error("no sessions exist yet")]
    NoSessions,
    #[error("session {0} is marked keep")]
    SessionKept(String),
    #[error("session is {from} and cannot become {to}")]
    InvalidTransition {
        from: SessionState,
        to: SessionState,
    },
    #[error("profile {0} not found")]
    ProfileNotFound(String),
    #[error("invalid profile {name}: {reason}")]
    InvalidProfile { name: String, reason: String },
    #[error("configuration error: {0}")]
    Config(String),
    #[error("credential store error: {0}")]
    CredentialStore(String),
    /// macOS refused an Apple Event to `app` (TCC "Automation" grant
    /// missing), e.g. AppleScript error -1743 (spec.md §6's "UTM
    /// 미설치·자동화 권한 거부" row).
    #[error("automation permission denied for {app}: {detail}")]
    AutomationDenied { app: String, detail: String },
    /// A backend process the app did not spawn is still holding a VM's
    /// files after it was stopped (docs/utm-compatibility.md T12). We never
    /// signal it -- the stop is reported incomplete and the user decides.
    #[error("{detail}")]
    OrphanProcess { pid: u32, detail: String },
    /// The guest never reported an address before the boot timeout. The
    /// message carries the elapsed time and what was being waited for, the
    /// hint the log path and how to raise the timeout (spec.md §6's "긴
    /// emulate 부팅" row).
    #[error("{detail}")]
    GuestAddressTimeout { detail: String, hint: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// The remediation text shown whenever isolated (host-only) networking
/// can't actually run on this host -- Tart implements `--net-host` via
/// Softnet, which requires root (DESIGN.md §5/§7). `softnet_path`, when
/// known, is substituted into the sudoers line so the command can be
/// copy-pasted as-is; otherwise a common Homebrew install path is used as a
/// placeholder. Shared between [`WargameError::hint`] / `doctor`'s
/// `network:isolated` check (which have no path to give, so pass `None`)
/// and `wargame-tart`'s `isolated_network_hint`, which reuses this verbatim
/// so the message is identical everywhere it's shown.
pub fn isolated_network_hint(softnet_path: Option<&std::path::Path>) -> String {
    let path = softnet_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "/opt/homebrew/bin/softnet".to_string());
    format!(
        "Isolated networking uses Tart's Softnet, which needs root. Either allow passwordless sudo for it:\n  echo \"$(whoami) ALL=(root) NOPASSWD: {path}\" | sudo tee /etc/sudoers.d/wargame-softnet\nor launch with NAT networking instead: --network nat"
    )
}

/// The recovery steps for a denied macOS Automation (Apple Events) grant
/// (docs/utm-compatibility.md §1): the grant is per *responsible app*, so
/// the terminal or IDE that runs `wargame` is what has to be allowed, and
/// it is toggled in System Settings, not by anything wargame may do on the
/// user's behalf.
pub fn automation_permission_hint(app: &str) -> String {
    format!(
        "Allow this terminal to control {app}: System Settings > Privacy & Security > Automation > (your terminal app) > {app}.\nThe grant belongs to the app that runs wargame, so Terminal, iTerm and an IDE each need it once. Then re-run: wargame doctor"
    )
}

impl WargameError {
    /// A short, actionable next step to show under the error message, if any
    /// (MVP.md §23's "Try:" lines).
    pub fn hint(&self) -> Option<String> {
        match self {
            WargameError::NetworkUnavailable(_) => Some(isolated_network_hint(None)),
            WargameError::ImageMissing { profile, .. } => {
                Some(format!("wargame profile build {profile}"))
            }
            WargameError::AuthenticationRequired { .. } => {
                Some("wargame auth login".to_string())
            }
            WargameError::InsufficientResources(_) => Some(
                "wargame sessions   # then delete an old session to free space".to_string(),
            ),
            WargameError::BackendUnavailable { .. } => Some("wargame doctor".to_string()),
            WargameError::AutomationDenied { app, .. } => Some(automation_permission_hint(app)),
            WargameError::OrphanProcess { pid, .. } => Some(format!(
                "A backend process wargame did not start is still holding this VM. Check it, then clear it yourself:\n  ps -o pid,ppid,command -p {pid}\n  kill -9 {pid}\nwargame never signals a process it did not spawn."
            )),
            WargameError::GuestAddressTimeout { hint, .. } => Some(hint.clone()),
            WargameError::ProfileNotFound(_) => Some("wargame profiles".to_string()),
            WargameError::SessionNotFound(_) => Some("wargame sessions".to_string()),
            WargameError::AmbiguousSession { .. } => {
                Some("Use a longer prefix, or the full id from: wargame sessions".to_string())
            }
            WargameError::NoSessions => Some(
                "wargame search <query>   # then: wargame launch <provider:id>".to_string(),
            ),
            WargameError::SessionKept(id) => Some(format!(
                "wargame delete {id} --force   # or unmark it first: wargame keep {id} --off"
            )),
            WargameError::InvalidTransition { from, .. } => Some(match from {
                SessionState::Stopped => "wargame resume <id>   # then try again".to_string(),
                SessionState::Failed => {
                    "wargame session inspect <id>   # see why it failed; wargame delete <id> to discard it"
                        .to_string()
                }
                SessionState::Ready | SessionState::Running => {
                    "wargame sessions   # the session is already up; wargame ssh <id> to connect".to_string()
                }
                _ => "wargame sessions".to_string(),
            }),
            WargameError::Incompatible(_) => Some("wargame profiles".to_string()),
            WargameError::InvalidProfile { name, .. } => Some(format!(
                "Check the provisioning log: $WARGAME_HOME/logs/provision-{name}.log"
            )),
            WargameError::GuestBootstrapFailed(_) => Some(
                "Check the provisioning log: $WARGAME_HOME/logs/provision-<profile>.log"
                    .to_string(),
            ),
            _ => None,
        }
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, WargameError>;
