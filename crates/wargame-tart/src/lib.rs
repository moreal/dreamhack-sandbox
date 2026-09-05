//! wargame-tart: Tart VM backend adapter (DESIGN.md §5).

pub mod args;
pub mod build;
pub mod console;
pub mod image;
pub mod list;
pub mod provider;

pub use build::{BuildOptions, ProfileImageBuilder, TransportFactory};
pub use list::TartVm;
pub use provider::{TartConfig, TartProvider};
