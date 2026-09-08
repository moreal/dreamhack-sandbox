//! wargame-utm: the UTM (QEMU) VM backend (spec.md §5 item 2).
//!
//! Implements `wargame_core`'s `VmProvider` port over UTM 4.7.5's scripting
//! interface, and adds the build-time operations `profile create` needs.
//! Every command it runs is a row of `docs/utm-compatibility.md`'s verified
//! operation matrix; nothing is guessed. The fixtures those rows were
//! captured from live in `tests/fixtures/`.

pub mod automation;
pub mod config;
pub mod console;
pub mod provider;

pub use automation::{Automation, MakeVmParams, OrphanQemu, StopMethod, UtmStatus, UtmVm};
pub use config::{
    architecture, display_hardware, network_mode, UtmConfig, BUNDLED_UTMCTL,
    ISOLATED_NETWORK_DETAIL, MIN_UTM_VERSION, UTM_APP, VERIFIED_UTM_VERSION,
};
pub use provider::{ImageCatalog, NoBuildRecords, UtmProvider};
