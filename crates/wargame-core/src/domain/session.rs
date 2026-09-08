//! Session model and lifecycle state machine (MVP.md §16; DESIGN.md §3.2).

use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::challenge::{Challenge, ChallengeSummary};
use super::profile::{ExecutionMode, GuestArch, GuestOs, Resources};
use crate::error::{Result, WargameError};

/// The guest user every pre-multi-OS session connected as: the CLI wired
/// exactly one SSH user, and both built-in macOS profiles provision it
/// (DESIGN.md §6). It is only a *default* now -- the real user comes from
/// `profile.guest.user` at launch and from the session afterwards -- but a
/// record written before [`GuestConnection`] existed means this.
pub const DEFAULT_GUEST_USER: &str = "admin";
/// The guest SSH port a session uses unless a backend allocated a forwarded
/// one (spec.md §3 FR-4).
pub const DEFAULT_GUEST_PORT: u16 = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    Isolated,
    Nat,
}

impl FromStr for NetworkMode {
    type Err = WargameError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "isolated" => Ok(NetworkMode::Isolated),
            "nat" => Ok(NetworkMode::Nat),
            other => Err(WargameError::Other(format!(
                "invalid network mode: {other} (expected \"isolated\" or \"nat\")"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepPolicy {
    Discard,
    Keep,
}

/// Session lifecycle state (MVP.md §16). Only [`SessionState::Deleted`] is
/// terminal; everything else can still move (see [`SessionState::can_transition_to`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Creating,
    Downloading,
    Booting,
    Provisioning,
    Ready,
    Running,
    Stopped,
    Failed,
    Deleting,
    Deleted,
}

impl SessionState {
    /// Whether moving from `self` to `to` is a legal transition
    /// (DESIGN.md §3.2's transition table).
    pub fn can_transition_to(self, to: SessionState) -> bool {
        use SessionState::*;
        if self == to {
            return false;
        }
        match (self, to) {
            (Creating, Downloading) => true,
            (Downloading, Booting) => true,
            (Booting, Provisioning) => true,
            (Provisioning, Ready) => true,
            (Ready, Running) => true,
            (Running, Stopped) => true,
            (Stopped, Running) => true,
            (Stopped, Booting) => true,
            (Ready, Deleting) | (Running, Deleting) | (Stopped, Deleting) | (Failed, Deleting) => {
                true
            }
            (Deleting, Deleted) => true,
            // Any non-terminal state may fail.
            (_, Failed) if self != Deleted => true,
            _ => false,
        }
    }

    /// The VM is up and reachable for `ssh`/`gui` (Ready or Running).
    pub fn is_up(self) -> bool {
        matches!(self, SessionState::Ready | SessionState::Running)
    }

    /// No further transitions are possible.
    pub fn is_terminal(self) -> bool {
        matches!(self, SessionState::Deleted)
    }

    /// The lowercase name used everywhere a state is shown to the user
    /// (tables, status lines, error messages); matches the serde form.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Creating => "creating",
            SessionState::Downloading => "downloading",
            SessionState::Booting => "booting",
            SessionState::Provisioning => "provisioning",
            SessionState::Ready => "ready",
            SessionState::Running => "running",
            SessionState::Stopped => "stopped",
            SessionState::Failed => "failed",
            SessionState::Deleting => "deleting",
            SessionState::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl NetworkMode {
    /// The lowercase name used in tables and status lines.
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkMode::Isolated => "isolated",
            NetworkMode::Nat => "nat",
        }
    }
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which shell language a guest command has to be written in (spec.md §3
/// FR-4). Windows needs PowerShell encoding rather than POSIX quoting; the
/// PowerShell transport itself is a later task, this is the plumbing that
/// tells it apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestShell {
    #[default]
    Posix,
    #[serde(rename = "powershell")]
    PowerShell,
}

impl GuestShell {
    /// The shell a guest OS is driven through.
    pub fn for_os(os: GuestOs) -> GuestShell {
        match os {
            GuestOs::Windows => GuestShell::PowerShell,
            GuestOs::Macos | GuestOs::Linux => GuestShell::Posix,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GuestShell::Posix => "posix",
            GuestShell::PowerShell => "powershell",
        }
    }
}

impl std::fmt::Display for GuestShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How to reach a session's guest, minus the address (which only exists once
/// the VM is up): the snapshot spec.md §3 FR-5 asks a session to store so a
/// later `resume`/`ssh` reconnects the way the launch did, whatever the
/// profile says now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuestConnection {
    pub user: String,
    pub port: u16,
    pub shell: GuestShell,
    /// The private key to authenticate with. `None` means the host's
    /// standard guest key (`$WARGAME_HOME/keys/id_ed25519`), which is what
    /// every pre-multi-OS session used.
    pub key_path: Option<PathBuf>,
}

impl Default for GuestConnection {
    fn default() -> Self {
        GuestConnection {
            user: DEFAULT_GUEST_USER.to_string(),
            port: DEFAULT_GUEST_PORT,
            shell: GuestShell::Posix,
            key_path: None,
        }
    }
}

impl GuestConnection {
    /// The connection settings a launch of `os`/`user` starts with.
    pub fn new(user: impl Into<String>, os: GuestOs) -> Self {
        GuestConnection {
            user: user.into(),
            port: DEFAULT_GUEST_PORT,
            shell: GuestShell::for_os(os),
            key_path: None,
        }
    }

    /// Completes these settings with a host address.
    pub fn endpoint(&self, host: IpAddr) -> GuestEndpoint {
        GuestEndpoint {
            host,
            port: self.port,
            user: self.user.clone(),
            key_path: self.key_path.clone(),
            shell: self.shell,
        }
    }
}

/// A fully addressed guest: everything [`crate::ports::GuestTransport`]
/// needs to run a command, with no host-wide default standing in for the
/// user (spec.md §3 FR-4: "admin을 고정하지 않는다").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestEndpoint {
    pub host: IpAddr,
    pub port: u16,
    pub user: String,
    pub key_path: Option<PathBuf>,
    pub shell: GuestShell,
}

impl GuestEndpoint {
    /// A POSIX endpoint on the default SSH port with the transport's own
    /// key -- the shape of every pre-multi-OS connection.
    pub fn new(host: IpAddr, user: impl Into<String>) -> Self {
        GuestEndpoint {
            host,
            port: DEFAULT_GUEST_PORT,
            user: user.into(),
            key_path: None,
            shell: GuestShell::Posix,
        }
    }
}

impl std::fmt::Display for GuestEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}:{}", self.user, self.host, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAsset {
    pub filename: String,
    pub sha256: String,
    pub size: u64,
}

/// A VM as its backend knows it: which backend owns it, its stable
/// provider-side id (a UTM UUID, a Tart VM name) and the display name.
///
/// Deserializing a record written before `id` existed fills it from `name`,
/// which is exactly what Tart addresses a VM by -- so old sessions keep
/// pointing at the same VM (spec.md §3 FR-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "VmRefRepr")]
pub struct VmRef {
    pub backend: String,
    pub id: String,
    pub name: String,
}

impl VmRef {
    /// A VM whose stable id *is* its name (Tart).
    pub fn new(backend: impl Into<String>, name: impl Into<String>) -> Self {
        let name = name.into();
        VmRef {
            backend: backend.into(),
            id: name.clone(),
            name,
        }
    }
}

#[derive(Deserialize)]
struct VmRefRepr {
    backend: String,
    #[serde(default)]
    id: Option<String>,
    name: String,
}

impl From<VmRefRepr> for VmRef {
    fn from(raw: VmRefRepr) -> VmRef {
        VmRef {
            backend: raw.backend,
            id: raw.id.unwrap_or_else(|| raw.name.clone()),
            name: raw.name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub display_name: String,
    pub challenge: ChallengeSummary,
    pub challenge_snapshot: Challenge,
    /// The backend that owns this session's VM. Every lifecycle operation
    /// routes on this, never on the profile or a global default (FR-5).
    pub vm_provider: String,
    pub profile: String,
    pub guest_os: GuestOs,
    pub guest_arch: GuestArch,
    /// `kali` for a Linux guest; absent for macOS/Windows and for every
    /// pre-multi-OS record.
    #[serde(default)]
    pub guest_distribution: Option<String>,
    /// The execution mode the VM was created with. Old records have none and
    /// mean `virtualize`, which is what Tart has always done.
    #[serde(default)]
    pub vm_mode: ExecutionMode,
    pub network: NetworkMode,
    pub resources: Resources,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub state: SessionState,
    pub vm_ref: Option<VmRef>,
    pub assets: Vec<SessionAsset>,
    pub keep: KeepPolicy,
    pub last_error: Option<String>,
    pub runner_pid: Option<u32>,
    pub guest_ip: Option<IpAddr>,
    /// How to reconnect to the guest. Old records default to the single
    /// hardcoded connection the pre-multi-OS build used: `admin`, port 22,
    /// POSIX, host key.
    #[serde(default)]
    pub connection: GuestConnection,
    /// The guest-side workspace root the challenge was placed under. Empty
    /// in pre-multi-OS records, which always took it from the profile.
    #[serde(default)]
    pub workspace_root: String,
}

impl Session {
    /// Builds a session id of the form `"<provider_short>-<slug>-<rand6>"`,
    /// e.g. `"dh-out-of-bound-a1b2c3"`.
    pub fn new_id(provider_short: &str, slug: &str, rand6: &str) -> String {
        format!("{provider_short}-{slug}-{rand6}")
    }

    /// The guest endpoint to talk to this session, once it has an address.
    /// Built from the *saved* connection settings, so a profile edited after
    /// launch cannot change where or as whom we reconnect (FR-5).
    pub fn endpoint(&self) -> Option<GuestEndpoint> {
        self.guest_ip.map(|ip| self.connection.endpoint(ip))
    }

    /// Moves the session to `to` if that is a legal transition, updating
    /// `self.state` in place.
    pub fn transition(&mut self, to: SessionState) -> Result<()> {
        if !self.state.can_transition_to(to) {
            return Err(WargameError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::challenge::ChallengeRef;

    fn sample_challenge() -> Challenge {
        let summary = ChallengeSummary {
            r#ref: ChallengeRef::parse("dreamhack:11").unwrap(),
            slug: "out-of-bound".to_string(),
            title: "out_of_bound".to_string(),
            categories: vec!["pwnable".to_string()],
            difficulty: None,
        };
        Challenge {
            summary,
            description: String::new(),
            tags: vec![],
            assets: vec![],
            remote_endpoints: vec![],
            source_url: "https://dreamhack.io".to_string(),
        }
    }

    fn sample_session() -> Session {
        let challenge = sample_challenge();
        Session {
            id: Session::new_id("dh", &challenge.summary.slug, "a1b2c3"),
            display_name: challenge.summary.title.clone(),
            challenge: challenge.summary.clone(),
            challenge_snapshot: challenge,
            vm_provider: "tart".to_string(),
            profile: "macos-re-arm64".to_string(),
            guest_os: GuestOs::Macos,
            guest_arch: GuestArch::Arm64,
            guest_distribution: None,
            vm_mode: ExecutionMode::Virtualize,
            network: NetworkMode::Isolated,
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 60,
            },
            created_at: OffsetDateTime::now_utc(),
            state: SessionState::Creating,
            vm_ref: None,
            assets: vec![],
            keep: KeepPolicy::Discard,
            last_error: None,
            runner_pid: None,
            guest_ip: None,
            connection: GuestConnection::default(),
            workspace_root: "/Users/admin/wargame".to_string(),
        }
    }

    #[test]
    fn new_id_format() {
        assert_eq!(
            Session::new_id("dh", "out-of-bound", "a1b2c3"),
            "dh-out-of-bound-a1b2c3"
        );
    }

    #[test]
    fn happy_path_transitions_all_pass() {
        let mut s = sample_session();
        use SessionState::*;
        for to in [Downloading, Booting, Provisioning, Ready, Running, Stopped] {
            s.transition(to).unwrap();
        }
        assert_eq!(s.state, Stopped);
    }

    #[test]
    fn resume_from_stopped_to_booting() {
        let mut s = sample_session();
        s.state = SessionState::Stopped;
        assert!(s.transition(SessionState::Booting).is_ok());
    }

    #[test]
    fn running_and_stopped_are_bidirectional() {
        let mut s = sample_session();
        s.state = SessionState::Running;
        assert!(s.transition(SessionState::Stopped).is_ok());
        assert!(s.transition(SessionState::Running).is_ok());
    }

    #[test]
    fn any_non_terminal_can_fail() {
        for from in [
            SessionState::Creating,
            SessionState::Downloading,
            SessionState::Booting,
            SessionState::Provisioning,
            SessionState::Ready,
            SessionState::Running,
            SessionState::Stopped,
            SessionState::Deleting,
        ] {
            assert!(
                from.can_transition_to(SessionState::Failed),
                "{from:?} -> Failed"
            );
        }
        assert!(!SessionState::Deleted.can_transition_to(SessionState::Failed));
    }

    #[test]
    fn terminal_states_can_delete() {
        for from in [
            SessionState::Ready,
            SessionState::Running,
            SessionState::Stopped,
            SessionState::Failed,
        ] {
            assert!(from.can_transition_to(SessionState::Deleting));
        }
        assert!(SessionState::Deleting.can_transition_to(SessionState::Deleted));
    }

    #[test]
    fn ready_to_creating_is_invalid() {
        let mut s = sample_session();
        s.state = SessionState::Ready;
        let err = s.transition(SessionState::Creating).unwrap_err();
        assert!(matches!(err, WargameError::InvalidTransition { .. }));
    }

    #[test]
    fn is_up_and_is_terminal() {
        assert!(SessionState::Ready.is_up());
        assert!(SessionState::Running.is_up());
        assert!(!SessionState::Stopped.is_up());
        assert!(SessionState::Deleted.is_terminal());
        assert!(!SessionState::Failed.is_terminal());
    }

    #[test]
    fn network_mode_from_str() {
        assert_eq!(
            NetworkMode::from_str("isolated").unwrap(),
            NetworkMode::Isolated
        );
        assert_eq!(NetworkMode::from_str("NAT").unwrap(), NetworkMode::Nat);
        assert!(NetworkMode::from_str("bogus").is_err());
    }

    #[test]
    fn vm_ref_without_an_id_falls_back_to_its_name() {
        let old: VmRef = serde_json::from_str(r#"{"backend":"tart","name":"wg-x"}"#).unwrap();
        assert_eq!(old.id, "wg-x");
        let new: VmRef =
            serde_json::from_str(r#"{"backend":"utm","id":"UUID-1","name":"wg-x"}"#).unwrap();
        assert_eq!(new.id, "UUID-1");
    }

    #[test]
    fn guest_shell_follows_the_guest_os() {
        assert_eq!(GuestShell::for_os(GuestOs::Macos), GuestShell::Posix);
        assert_eq!(GuestShell::for_os(GuestOs::Linux), GuestShell::Posix);
        assert_eq!(GuestShell::for_os(GuestOs::Windows), GuestShell::PowerShell);
    }

    #[test]
    fn endpoint_is_built_from_the_saved_connection() {
        let mut s = sample_session();
        assert!(s.endpoint().is_none(), "no address yet");
        s.guest_ip = Some(IpAddr::from_str("10.0.0.5").unwrap());
        s.connection = GuestConnection {
            user: "kali".to_string(),
            port: 22022,
            shell: GuestShell::Posix,
            key_path: None,
        };
        let endpoint = s.endpoint().unwrap();
        assert_eq!(endpoint.user, "kali");
        assert_eq!(endpoint.port, 22022);
        assert_eq!(endpoint.to_string(), "kali@10.0.0.5:22022");
    }

    #[test]
    fn session_round_trips_json() {
        let s = sample_session();
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.state, s.state);
    }
}
