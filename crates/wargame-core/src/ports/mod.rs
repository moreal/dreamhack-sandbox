//! Ports: the traits every provider/backend/store adapter implements
//! (DESIGN.md §3.3). `wargame-core` depends only on these; concrete
//! implementations live in `wargame-dreamhack`, `wargame-tart`, and
//! `wargame-core::infra`.

pub mod credential_store;
pub mod disk_space;
pub mod guest_transport;
pub mod image_builder;
pub mod session_repo;
pub mod vm_provider;
pub mod wargame_provider;

pub use credential_store::CredentialStore;
pub use disk_space::DiskSpace;
pub use guest_transport::{ExecResult, GuestEndpoint, GuestShell, GuestTransport};
pub use image_builder::{
    BuildObserver, BuildRecordRepository, BuildRequest, GuestLink, InstallerRequest,
    ProfileImageBuilder, Progress,
};
pub use session_repo::SessionRepository;
pub use vm_provider::{
    BackendCheck, BackendStatus, ConsoleDescriptor, ImageRef, RuntimeInfo, StartOptions,
    VmProvider, VmSpec, VmStatus,
};
pub use wargame_provider::{
    AssetSink, AuthState, DownloadResult, DownloadedFile, ProviderStatus, SearchPage, SearchQuery,
    WargameProvider,
};

/// A boxed, zeroizing secret value (see DESIGN.md §3.6). `secrecy` 0.10 dropped
/// the old `Secret<T>` alias in favor of `SecretBox<T>`; we keep the familiar
/// name here since it is what DESIGN.md's port signatures use.
pub type Secret<T> = secrecy::SecretBox<T>;
