//! JSON-file-backed [`SessionRepository`] (DESIGN.md §3.3, §21):
//! `sessions/<id>/metadata.json`, written atomically via a temp file + rename
//! so a crash mid-write never leaves a half-written session record.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::domain::session::Session;
use crate::error::{Result, WargameError};
use crate::ports::SessionRepository;

pub struct JsonSessionRepository {
    root: PathBuf,
}

impl JsonSessionRepository {
    pub fn new(root: PathBuf) -> Self {
        JsonSessionRepository { root }
    }

    fn metadata_path(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("metadata.json")
    }
}

#[async_trait]
impl SessionRepository for JsonSessionRepository {
    async fn save(&self, s: &Session) -> Result<()> {
        let dir = self.session_dir(&s.id);
        std::fs::create_dir_all(&dir)?;
        let json =
            serde_json::to_string_pretty(s).map_err(|e| WargameError::Other(e.to_string()))?;
        let tmp = dir.join(format!(".metadata.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.metadata_path(&s.id))?;
        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Session> {
        let path = self.metadata_path(id);
        if !path.exists() {
            return Err(WargameError::SessionNotFound(id.to_string()));
        }
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).map_err(|e| WargameError::Other(e.to_string()))
    }

    async fn list(&self) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();
        if self.root.exists() {
            for entry in std::fs::read_dir(&self.root)? {
                let entry = entry?;
                if !entry.path().is_dir() {
                    continue;
                }
                let meta_path = entry.path().join("metadata.json");
                let Ok(content) = std::fs::read_to_string(&meta_path) else {
                    continue;
                };
                if let Ok(session) = serde_json::from_str::<Session>(&content) {
                    sessions.push(session);
                }
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        Ok(sessions)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let dir = self.session_dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    fn session_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::challenge::{Challenge, ChallengeRef, ChallengeSummary};
    use crate::domain::profile::{ExecutionMode, GuestArch, GuestOs, Resources};
    use crate::domain::session::{GuestConnection, KeepPolicy, NetworkMode, SessionState};
    use std::time::Duration;
    use time::OffsetDateTime;

    fn sample_session(id: &str, created_at: OffsetDateTime) -> Session {
        let summary = ChallengeSummary {
            r#ref: ChallengeRef::parse("dreamhack:11").unwrap(),
            slug: "out-of-bound".to_string(),
            title: "out_of_bound".to_string(),
            categories: vec!["pwnable".to_string()],
            difficulty: None,
        };
        let challenge = Challenge {
            summary: summary.clone(),
            description: String::new(),
            tags: vec![],
            assets: vec![],
            remote_endpoints: vec![],
            source_url: "https://dreamhack.io".to_string(),
        };
        Session {
            id: id.to_string(),
            display_name: summary.title.clone(),
            challenge: summary,
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
            created_at,
            state: SessionState::Ready,
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

    #[tokio::test]
    async fn round_trips_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonSessionRepository::new(dir.path().to_path_buf());
        let session = sample_session("dh-a-000001", OffsetDateTime::now_utc());

        repo.save(&session).await.unwrap();
        let loaded = repo.load(&session.id).await.unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.state, session.state);

        assert!(repo.session_dir(&session.id).join("metadata.json").exists());
    }

    #[tokio::test]
    async fn load_missing_errors_session_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonSessionRepository::new(dir.path().to_path_buf());
        let err = repo.load("nope").await.unwrap_err();
        assert!(matches!(err, WargameError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn list_is_sorted_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonSessionRepository::new(dir.path().to_path_buf());
        let now = OffsetDateTime::now_utc();

        let older = sample_session("dh-a-000001", now - Duration::from_secs(60));
        let newer = sample_session("dh-b-000002", now);
        repo.save(&older).await.unwrap();
        repo.save(&newer).await.unwrap();

        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, newer.id);
        assert_eq!(all[1].id, older.id);
    }

    #[tokio::test]
    async fn delete_removes_the_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonSessionRepository::new(dir.path().to_path_buf());
        let session = sample_session("dh-a-000001", OffsetDateTime::now_utc());
        repo.save(&session).await.unwrap();

        repo.delete(&session.id).await.unwrap();
        assert!(!repo.session_dir(&session.id).exists());
        assert!(matches!(
            repo.load(&session.id).await.unwrap_err(),
            WargameError::SessionNotFound(_)
        ));
    }
}
