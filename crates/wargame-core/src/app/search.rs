//! Challenge search/detail (DESIGN.md §3.1's `app/search.rs`): thin,
//! directly-awaited pass-throughs to the wired `WargameProvider` so the CLI
//! and TUI share exactly one code path.

use crate::domain::{Challenge, ChallengeRef};
use crate::error::Result;
use crate::ports::{SearchPage, SearchQuery};

use super::AppContext;

pub async fn search(ctx: &AppContext, q: SearchQuery) -> Result<SearchPage> {
    ctx.wargame.search(&q).await
}

pub async fn open(ctx: &AppContext, r: &ChallengeRef) -> Result<Challenge> {
    ctx.wargame.get_challenge(&r.id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::profiles::ProfileStore;
    use crate::infra::fs::AppDirs;
    use crate::infra::process::ProcessRunner;
    use crate::ports::WargameProvider;
    use crate::testing::{
        FakeRunner, FakeTransport, FakeVmProvider, FakeWargameProvider, MemoryRepo,
    };
    use std::sync::Arc;

    /// A throwaway, leaked-on-purpose temp directory path — fine for a unit
    /// test process that exits shortly after.
    fn tmp_path() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    fn ctx(wargame: Arc<dyn WargameProvider>) -> AppContext {
        AppContext {
            config: Default::default(),
            dirs: AppDirs { root: tmp_path() },
            wargame,
            vm: Arc::new(FakeVmProvider::new()),
            transport: Arc::new(FakeTransport::new()),
            repo: Arc::new(MemoryRepo::new()),
            profiles: ProfileStore::new(vec![], tmp_path()),
            runner: Arc::new(FakeRunner::new()) as Arc<dyn ProcessRunner>,
            bootstrap_script: "#!/bin/bash\n",
        }
    }

    #[tokio::test]
    async fn search_delegates_to_provider() {
        let provider: Arc<dyn WargameProvider> = Arc::new(FakeWargameProvider::new());
        let context = ctx(provider);
        let page = search(
            &context,
            SearchQuery {
                text: "hello".to_string(),
                category: None,
                page: 1,
                page_size: 20,
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1);
    }

    #[tokio::test]
    async fn open_delegates_to_provider() {
        let provider: Arc<dyn WargameProvider> = Arc::new(FakeWargameProvider::new());
        let context = ctx(provider);
        let r = ChallengeRef::parse("dreamhack:1").unwrap();
        let challenge = open(&context, &r).await.unwrap();
        assert_eq!(challenge.summary.r#ref.id, "1");
    }
}
