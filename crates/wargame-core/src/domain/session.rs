//! Session model and lifecycle state machine (MVP.md §16; DESIGN.md §3.2).

use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::challenge::{Challenge, ChallengeSummary};
use super::profile::{GuestArch, GuestOs, Resources};
use crate::error::{Result, WargameError};

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAsset {
    pub filename: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRef {
    pub backend: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub display_name: String,
    pub challenge: ChallengeSummary,
    pub challenge_snapshot: Challenge,
    pub vm_provider: String,
    pub profile: String,
    pub guest_os: GuestOs,
    pub guest_arch: GuestArch,
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
}

impl Session {
    /// Builds a session id of the form `"<provider_short>-<slug>-<rand6>"`,
    /// e.g. `"dh-out-of-bound-a1b2c3"`.
    pub fn new_id(provider_short: &str, slug: &str, rand6: &str) -> String {
        format!("{provider_short}-{slug}-{rand6}")
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
    fn session_round_trips_json() {
        let s = sample_session();
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.state, s.state);
    }
}
