//! Provider-neutral domain model (MVP.md §8, §13, §16; DESIGN.md §3.2).

pub mod challenge;
pub mod compat;
pub mod profile;
pub mod session;

pub use challenge::{slugify, Challenge, ChallengeAsset, ChallengeRef, ChallengeSummary};
pub use compat::{
    check, resolve, Backend, ProviderCapabilities, ResolvedBackend, SupportedCombination, SUPPORTED,
};
pub use profile::{
    BackendChoice, ExecutionMode, GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, ProvisionSpec,
    Resources, ToolsSpec, VmSettings, WorkspaceSpec,
};
pub use session::{
    GuestConnection, GuestEndpoint, GuestShell, KeepPolicy, NetworkMode, Session, SessionAsset,
    SessionState, VmRef, DEFAULT_GUEST_PORT, DEFAULT_GUEST_USER,
};
