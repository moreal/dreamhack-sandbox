//! wargame-dreamhack: Dreamhack wargame provider adapter (DESIGN.md §4).

pub mod api;
pub mod auth;
pub mod client;
pub mod download;
pub mod model;
pub mod normalize;
pub mod provider;

pub use provider::{DreamhackConfig, DreamhackProvider, CREDENTIAL_KEY};
