//! wargame-core: domain model, ports, application services, security helpers and infra adapters.
//!
//! Module layout follows DESIGN.md §3. Modules are added task by task (see PLAN.md).

pub mod config;
pub mod domain;
pub mod error;
pub mod ports;

pub use error::{Result, WargameError};
