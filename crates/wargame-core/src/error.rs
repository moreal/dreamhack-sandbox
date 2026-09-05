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
    #[error("invalid session state transition {from:?} -> {to:?}")]
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

impl WargameError {
    /// A short, actionable next step to show under the error message, if any
    /// (MVP.md §23's "Try:" lines).
    pub fn hint(&self) -> Option<String> {
        match self {
            WargameError::NetworkUnavailable(_) => Some(isolated_network_hint(None)),
            WargameError::ImageMissing { profile, .. } => {
                Some(format!("Run: wargame profile build {profile}"))
            }
            WargameError::AuthenticationRequired { .. } => {
                Some("Run: wargame auth login".to_string())
            }
            WargameError::InsufficientResources(_) => Some(
                "Run: wargame sessions   # then delete an old session to free space".to_string(),
            ),
            WargameError::BackendUnavailable { .. } => Some("Run: wargame doctor".to_string()),
            WargameError::ProfileNotFound(_) => Some("Run: wargame profiles".to_string()),
            WargameError::SessionNotFound(_) => Some("Run: wargame sessions".to_string()),
            WargameError::Incompatible(_) => Some("Run: wargame profiles".to_string()),
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
