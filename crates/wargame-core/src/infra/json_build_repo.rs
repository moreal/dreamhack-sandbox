//! JSON-file-backed [`BuildRecordRepository`]:
//! `builds/<profile>/record.json`, written atomically via a temp file +
//! rename so a crash mid-write never leaves a half-written record -- the
//! same shape and the same guarantee as
//! [`crate::infra::json_repo::JsonSessionRepository`] (DESIGN.md §21).
//!
//! A record survives every failed build on purpose: it is what lets
//! `profile create` be re-run without making a second installation VM, and
//! what keeps the previously published image findable.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::domain::build::BuildRecord;
use crate::error::{Result, WargameError};
use crate::ports::BuildRecordRepository;

pub struct JsonBuildRepository {
    root: PathBuf,
}

impl JsonBuildRepository {
    pub fn new(root: PathBuf) -> Self {
        JsonBuildRepository { root }
    }

    /// `<root>/<profile>` -- the directory holding one profile's record.
    pub fn build_dir(&self, profile: &str) -> PathBuf {
        self.root.join(profile)
    }

    fn record_path(&self, profile: &str) -> PathBuf {
        self.build_dir(profile).join("record.json")
    }
}

#[async_trait]
impl BuildRecordRepository for JsonBuildRepository {
    async fn save(&self, record: &BuildRecord) -> Result<()> {
        let dir = self.build_dir(&record.profile);
        std::fs::create_dir_all(&dir)?;
        let json =
            serde_json::to_string_pretty(record).map_err(|e| WargameError::Other(e.to_string()))?;
        let tmp = dir.join(format!(".record.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.record_path(&record.profile))?;
        Ok(())
    }

    async fn load(&self, profile: &str) -> Result<Option<BuildRecord>> {
        let path = self.record_path(profile);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content)
            .map(Some)
            .map_err(|e| WargameError::Other(format!("{}: {e}", path.display())))
    }

    async fn list(&self) -> Result<Vec<BuildRecord>> {
        let mut records = Vec::new();
        if self.root.exists() {
            for entry in std::fs::read_dir(&self.root)? {
                let entry = entry?;
                if !entry.path().is_dir() {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(entry.path().join("record.json")) else {
                    continue;
                };
                // A record this build cannot parse is skipped, not fatal:
                // one unreadable profile must not hide every other one.
                if let Ok(record) = serde_json::from_str::<BuildRecord>(&content) {
                    records.push(record);
                }
            }
        }
        records.sort_by(|a, b| a.profile.cmp(&b.profile));
        Ok(records)
    }

    async fn delete(&self, profile: &str) -> Result<()> {
        let dir = self.build_dir(profile);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::build::{BuildPhase, BuildSource};
    use crate::domain::profile::{
        GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, Resources, VmSettings, WorkspaceSpec,
    };

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            description: String::new(),
            vm: VmSettings::default(),
            guest: GuestSpec {
                os: GuestOs::Linux,
                arch: GuestArch::X86_64,
                distribution: Some("kali".to_string()),
                user: "kali".to_string(),
                password: None,
            },
            image: ImageSpec {
                base: String::new(),
            },
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 80,
            },
            provision: Default::default(),
            tools: Default::default(),
            workspace: WorkspaceSpec {
                root: "/home/kali/wargame".to_string(),
            },
        }
    }

    fn record(name: &str) -> BuildRecord {
        BuildRecord::new(
            &profile(name),
            "utm",
            BuildSource::UtmVm {
                uuid: "UUID-1".to_string(),
            },
        )
    }

    #[tokio::test]
    async fn round_trips_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonBuildRepository::new(dir.path().to_path_buf());
        let mut record = record("kali-custom");
        record.set_phase(BuildPhase::AwaitingInstall);

        repo.save(&record).await.unwrap();
        let loaded = repo.load("kali-custom").await.unwrap().unwrap();
        assert_eq!(loaded, record);
        assert!(dir.path().join("kali-custom/record.json").is_file());
    }

    #[tokio::test]
    async fn a_profile_with_no_record_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonBuildRepository::new(dir.path().to_path_buf());
        assert!(repo.load("never-created").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_skips_unparsable_records_and_sorts_by_profile() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonBuildRepository::new(dir.path().to_path_buf());
        repo.save(&record("zeta")).await.unwrap();
        repo.save(&record("alpha")).await.unwrap();
        std::fs::create_dir_all(dir.path().join("broken")).unwrap();
        std::fs::write(dir.path().join("broken/record.json"), "{ not json").unwrap();

        let all = repo.list().await.unwrap();
        assert_eq!(
            all.iter().map(|r| r.profile.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }

    #[tokio::test]
    async fn delete_removes_the_record_directory() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonBuildRepository::new(dir.path().to_path_buf());
        repo.save(&record("kali-custom")).await.unwrap();
        repo.delete("kali-custom").await.unwrap();
        assert!(repo.load("kali-custom").await.unwrap().is_none());
        assert!(!dir.path().join("kali-custom").exists());
    }
}
