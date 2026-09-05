//! An in-memory [`SessionRepository`], for tests that need one without
//! touching disk.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::domain::session::Session;
use crate::error::{Result, WargameError};
use crate::ports::SessionRepository;

#[derive(Default)]
pub struct MemoryRepo {
    sessions: Mutex<BTreeMap<String, Session>>,
}

impl MemoryRepo {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionRepository for MemoryRepo {
    async fn save(&self, s: &Session) -> Result<()> {
        self.sessions
            .lock()
            .unwrap()
            .insert(s.id.clone(), s.clone());
        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Session> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| WargameError::SessionNotFound(id.to_string()))
    }

    async fn list(&self) -> Result<Vec<Session>> {
        let mut all: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        Ok(all)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.sessions.lock().unwrap().remove(id);
        Ok(())
    }

    fn session_dir(&self, id: &str) -> PathBuf {
        PathBuf::from(format!("/memory/sessions/{id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::challenge::{Challenge, ChallengeRef, ChallengeSummary};
    use crate::domain::profile::{GuestArch, GuestOs, Resources};
    use crate::domain::session::{KeepPolicy, NetworkMode, SessionState};
    use time::OffsetDateTime;

    fn sample(id: &str) -> Session {
        let summary = ChallengeSummary {
            r#ref: ChallengeRef::parse("dreamhack:1").unwrap(),
            slug: "s".to_string(),
            title: "s".to_string(),
            categories: vec![],
            difficulty: None,
        };
        Session {
            id: id.to_string(),
            display_name: "s".to_string(),
            challenge: summary.clone(),
            challenge_snapshot: Challenge {
                summary,
                description: String::new(),
                tags: vec![],
                assets: vec![],
                remote_endpoints: vec![],
                source_url: "https://example.invalid".to_string(),
            },
            vm_provider: "tart".to_string(),
            profile: "p".to_string(),
            guest_os: GuestOs::Macos,
            guest_arch: GuestArch::Arm64,
            network: NetworkMode::Isolated,
            resources: Resources {
                cpu: 1,
                memory_mib: 1024,
                disk_gib: 10,
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

    #[tokio::test]
    async fn round_trips_and_deletes() {
        let repo = MemoryRepo::new();
        let s = sample("id-1");
        repo.save(&s).await.unwrap();
        assert_eq!(repo.load("id-1").await.unwrap().id, "id-1");
        assert_eq!(repo.list().await.unwrap().len(), 1);
        repo.delete("id-1").await.unwrap();
        assert!(matches!(
            repo.load("id-1").await.unwrap_err(),
            WargameError::SessionNotFound(_)
        ));
    }
}
