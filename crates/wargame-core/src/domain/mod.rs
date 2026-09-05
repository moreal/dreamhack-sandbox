//! Provider-neutral domain model (MVP.md §8, §13, §16; DESIGN.md §3.2).

pub mod challenge;
pub mod compat;
pub mod profile;
pub mod session;

pub use challenge::{slugify, Challenge, ChallengeAsset, ChallengeRef, ChallengeSummary};
pub use compat::{check, ProviderCapabilities};
pub use profile::{
    GuestArch, GuestOs, GuestSpec, ImageSpec, Profile, ProvisionSpec, Resources, ToolsSpec,
    WorkspaceSpec,
};
pub use session::{KeepPolicy, NetworkMode, Session, SessionAsset, SessionState, VmRef};
