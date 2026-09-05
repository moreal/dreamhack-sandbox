//! Host-side infrastructure adapters (DESIGN.md §3.1).
//!
//! Only the pieces needed so far are present; the rest of Task 2's file map
//! (`fs`, `keychain`, `json_repo`, `ssh`) lands alongside its security
//! helpers.

pub mod process;

pub use process::{Cmd, Output, ProcessRunner, RealRunner};
