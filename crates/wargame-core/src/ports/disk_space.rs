//! `DiskSpace`: how much room is left where wargame writes (DESIGN.md §3.5).
//!
//! Behind a port so the preflight checks that refuse a build or a launch for
//! lack of space can be exercised without filling a real disk
//! (spec.md §7 AC-09's fault injection).

use std::path::Path;

use crate::error::Result;

pub trait DiskSpace: Send + Sync {
    /// Bytes free on the filesystem containing `path`.
    fn free_bytes(&self, path: &Path) -> Result<u64>;
}
