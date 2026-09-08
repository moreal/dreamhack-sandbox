//! An in-memory [`BuildRecordRepository`] for exercising `profile
//! create`/`profile build` without touching `$WARGAME_HOME`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::domain::build::BuildRecord;
use crate::error::Result;
use crate::ports::BuildRecordRepository;

#[derive(Default)]
pub struct MemoryBuildRepo {
    records: Mutex<BTreeMap<String, BuildRecord>>,
    /// How many times `save` was called, so a test can assert a record was
    /// persisted at each step rather than only at the end.
    saves: Mutex<usize>,
}

impl MemoryBuildRepo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a record without going through the port (a build that was
    /// interrupted, say).
    pub fn insert(&self, record: BuildRecord) {
        self.records
            .lock()
            .unwrap()
            .insert(record.profile.clone(), record);
    }

    pub fn saves(&self) -> usize {
        *self.saves.lock().unwrap()
    }
}

#[async_trait]
impl BuildRecordRepository for MemoryBuildRepo {
    async fn save(&self, record: &BuildRecord) -> Result<()> {
        *self.saves.lock().unwrap() += 1;
        self.insert(record.clone());
        Ok(())
    }

    async fn load(&self, profile: &str) -> Result<Option<BuildRecord>> {
        Ok(self.records.lock().unwrap().get(profile).cloned())
    }

    async fn list(&self) -> Result<Vec<BuildRecord>> {
        Ok(self.records.lock().unwrap().values().cloned().collect())
    }

    async fn delete(&self, profile: &str) -> Result<()> {
        self.records.lock().unwrap().remove(profile);
        Ok(())
    }
}
