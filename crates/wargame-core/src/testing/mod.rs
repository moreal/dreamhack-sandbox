//! Test fakes for every `wargame-core` port (PLAN.md Task 2). Deliberately
//! not behind `#[cfg(test)]` so downstream crates (`wargame-dreamhack`,
//! `wargame-tart`, `wargame-cli`) can use them from their own test suites.

pub mod fake_runner;
pub mod fake_transport;
pub mod fake_vm;
pub mod fake_wargame;
pub mod memory_repo;
pub mod memory_store;

pub use fake_runner::FakeRunner;
pub use fake_transport::{FakeTransport, TransportCall};
pub use fake_vm::FakeVmProvider;
pub use fake_wargame::{FakeWargameProvider, FIXTURE_ASSET_BYTES};
pub use memory_repo::MemoryRepo;
pub use memory_store::MemoryStore;
