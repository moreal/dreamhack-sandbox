//! `VmProvider`: the VM backend port (DESIGN.md §3.3, MVP.md §11).

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use crate::domain::compat::ProviderCapabilities;
use crate::domain::profile::Resources;
use crate::domain::session::{NetworkMode, VmRef};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    pub name: String,
    pub image: ImageRef,
    pub resources: Resources,
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
    fn console(&self, vm: &VmRef) -> ConsoleDescriptor;
}
