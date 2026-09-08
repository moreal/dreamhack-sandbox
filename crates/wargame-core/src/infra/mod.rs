//! Infrastructure adapters: the concrete, OS-touching implementations of
//! `wargame-core`'s ports (DESIGN.md §3.1). Everything here is behind a
//! trait so tests use `crate::testing`'s fakes instead.

pub mod fs;
pub mod json_build_repo;
pub mod json_repo;
pub mod keychain;
pub mod process;
pub mod ssh;

pub use fs::{disk_free_bytes, AppDirs, HostDiskSpace};
pub use json_build_repo::JsonBuildRepository;
pub use json_repo::JsonSessionRepository;
pub use keychain::{FileStore, KeychainStore};
pub use process::{Cmd, Output, ProcessRunner, RealRunner};
pub use ssh::{shell_quote, SshGuestTransport};
