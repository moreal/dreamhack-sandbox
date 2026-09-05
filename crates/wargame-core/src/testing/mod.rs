//! Test doubles shared across crates (DESIGN.md/PLAN.md Task 2). Public and
//! feature-gate-free so `wargame-dreamhack`, `wargame-tart`, and
//! `wargame-cli` can all depend on it from their own tests.
//!
//! Only [`fake_runner`] exists so far; the rest of Task 2's fakes
//! (`FakeVmProvider`, `FakeWargameProvider`, `FakeTransport`, `MemoryStore`,
//! `MemoryRepo`) land alongside its security/infra work.

pub mod fake_runner;

pub use fake_runner::FakeRunner;
