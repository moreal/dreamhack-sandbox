//! `VmProvider`: the VM backend port (DESIGN.md §3.3, MVP.md §11).

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use crate::domain::compat::{ProviderCapabilities, ResolvedBackend};
use crate::domain::profile::Resources;
use crate::domain::session::{NetworkMode, VmRef};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef(pub String);

/// Everything a backend needs to bring one VM into existence: the image and
/// resources it always needed, plus the execution configuration the profile
/// resolved to (spec.md §3 FR-1) -- which backend, which mode, which guest,
/// and the network the VM is configured for (UTM sets that on the VM, not
/// only at start time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    pub name: String,
    pub image: ImageRef,
    pub resources: Resources,
    pub execution: ResolvedBackend,
    pub network: NetworkMode,
}

impl VmSpec {
    /// A spec for the pre-multi-OS combination (Tart / macOS / arm64 /
    /// virtualize) on an isolated network -- the defaults every existing
    /// call site meant.
    pub fn new(name: impl Into<String>, image: ImageRef, resources: Resources) -> Self {
        VmSpec {
            name: name.into(),
            image,
            resources,
            execution: ResolvedBackend::default(),
            network: NetworkMode::Isolated,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOptions {
    pub network: NetworkMode,
    pub headless: bool,
    pub log_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmStatus {
    Running,
    Stopped,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    pub available: bool,
    pub version: Option<String>,
    pub detail: String,
    /// Whether this backend can actually run isolated (host-only) networking
    /// right now -- e.g. Tart's `--net-host` depends on Softnet, which needs
    /// root (DESIGN.md §5/§7). `None` when the backend has no way to tell.
    pub isolated_network: Option<bool>,
}

/// One backend-specific `doctor` row (spec.md §3 FR-6's "필요한 backend,
/// 실제 경로·버전, 자동화 권한 ... 검사"). Everything a backend can say about
/// itself that does not fit [`BackendStatus`]'s single line -- where its
/// binary was found, whether host automation is permitted -- comes back
/// through [`VmProvider::diagnostics`] so `app/doctor.rs` never has to know
/// which backend it is talking to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub hint: Option<String>,
}

/// How the user actually gets a graphical view of the VM (MVP.md §26's
/// `ConsoleDescriptor`, trimmed to what the local Tart backend can produce
/// today).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleDescriptor {
    Window { argv: Vec<String> },
    Vnc { url: String },
    None,
}

#[async_trait]
pub trait VmProvider: Send + Sync {
    /// Short backend identifier, e.g. `"tart"`.
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn healthcheck(&self) -> Result<BackendStatus>;
    async fn image_exists(&self, image: &ImageRef) -> Result<bool>;
    async fn pull_image(&self, image: &ImageRef) -> Result<()>;
    /// Clone `spec.image` into a fresh VM and apply `spec.resources`.
    async fn create(&self, spec: &VmSpec) -> Result<VmRef>;
    /// Start the VM detached; returns the runner's pid if one is available.
    async fn start(&self, vm: &VmRef, opts: &StartOptions) -> Result<RuntimeInfo>;
    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr>;
    async fn stop(&self, vm: &VmRef) -> Result<()>;
    async fn delete(&self, vm: &VmRef) -> Result<()>;
    async fn status(&self, vm: &VmRef) -> Result<VmStatus>;
    /// All VMs this backend manages, for orphan detection (MVP.md §37/NFR-02).
    async fn list(&self) -> Result<Vec<VmRef>>;
    /// Backend-specific `doctor` rows (binary path, host automation
    /// permission, ...). Empty unless a backend has something to add.
    async fn diagnostics(&self) -> Vec<BackendCheck> {
        Vec::new()
    }
    fn console(&self, vm: &VmRef) -> ConsoleDescriptor;
}
