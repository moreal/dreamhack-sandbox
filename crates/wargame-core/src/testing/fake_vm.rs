//! An in-memory [`VmProvider`] for exercising the launch/lifecycle
//! application services without Tart.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::domain::compat::ProviderCapabilities;
use crate::domain::profile::{ExecutionMode, GuestArch, GuestOs};
use crate::domain::session::{NetworkMode, VmRef};
use crate::error::{Result, WargameError};
use crate::ports::vm_provider::{
    BackendStatus, ConsoleDescriptor, ImageRef, RuntimeInfo, StartOptions, VmProvider, VmSpec,
    VmStatus,
};

#[derive(Debug, Clone)]
struct VmEntry {
    status: VmStatus,
    ip: Option<IpAddr>,
}

pub struct FakeVmProvider {
    name: String,
    images: Mutex<Vec<String>>,
    vms: Mutex<BTreeMap<String, VmEntry>>,
    fail_on: Mutex<Option<&'static str>>,
    calls: Mutex<Vec<String>>,
    specs: Mutex<Vec<VmSpec>>,
    wait_ip_timeouts: Mutex<Vec<Duration>>,
    capabilities: Mutex<ProviderCapabilities>,
    default_ip: IpAddr,
    isolated_network: Mutex<Option<bool>>,
}

impl FakeVmProvider {
    /// A Tart-shaped backend: macOS/arm64, virtualize only.
    pub fn new() -> Self {
        Self::with_backend("tart")
    }

    /// The same fake registered under another backend name, so a test can
    /// wire two of them into one `BackendRegistry` and assert which one an
    /// operation reached.
    pub fn with_backend(name: impl Into<String>) -> Self {
        FakeVmProvider {
            name: name.into(),
            images: Mutex::new(Vec::new()),
            vms: Mutex::new(BTreeMap::new()),
            fail_on: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            specs: Mutex::new(Vec::new()),
            wait_ip_timeouts: Mutex::new(Vec::new()),
            capabilities: Mutex::new(ProviderCapabilities {
                guest_os: vec![GuestOs::Macos],
                guest_arch: vec![GuestArch::Arm64],
                modes: vec![ExecutionMode::Virtualize],
                networks: vec![NetworkMode::Isolated, NetworkMode::Nat],
                graphical_console: true,
                ssh: true,
                snapshots: false,
                copy_in: true,
                remote: false,
            }),
            default_ip: IpAddr::from_str("10.0.0.5").unwrap(),
            isolated_network: Mutex::new(None),
        }
    }

    pub fn add_image(&self, name: impl Into<String>) {
        self.images.lock().unwrap().push(name.into());
    }

    /// Replaces what this backend claims it can run.
    pub fn set_capabilities(&self, capabilities: ProviderCapabilities) {
        *self.capabilities.lock().unwrap() = capabilities;
    }

    /// Every `VmSpec` handed to `create`, in order.
    pub fn specs(&self) -> Vec<VmSpec> {
        self.specs.lock().unwrap().clone()
    }

    /// Every timeout `wait_ip` was called with, in order -- so a test can
    /// check the per-mode boot timeout actually reached the backend.
    pub fn wait_ip_timeouts(&self) -> Vec<Duration> {
        self.wait_ip_timeouts.lock().unwrap().clone()
    }

    /// Configures the `isolated_network` field `healthcheck()` reports
    /// (`None` = "can't tell", the default).
    pub fn set_isolated_network(&self, value: Option<bool>) {
        *self.isolated_network.lock().unwrap() = value;
    }

    /// Steps: `"healthcheck"`, `"create"`, `"start"`, `"wait_ip"`.
    pub fn set_fail_on(&self, step: &'static str) {
        *self.fail_on.lock().unwrap() = Some(step);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn vm_names(&self) -> Vec<String> {
        self.vms.lock().unwrap().keys().cloned().collect()
    }

    fn should_fail(&self, step: &str) -> bool {
        self.fail_on
            .lock()
            .unwrap()
            .map(|s| s == step)
            .unwrap_or(false)
    }

    fn record(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_string());
    }
}

impl Default for FakeVmProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VmProvider for FakeVmProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.lock().unwrap().clone()
    }

    async fn healthcheck(&self) -> Result<BackendStatus> {
        self.record("healthcheck");
        if self.should_fail("healthcheck") {
            return Err(WargameError::BackendUnavailable {
                backend: self.name.clone(),
                reason: "fake failure".to_string(),
            });
        }
        Ok(BackendStatus {
            available: true,
            version: Some("fake-1.0".to_string()),
            detail: "ok".to_string(),
            isolated_network: *self.isolated_network.lock().unwrap(),
        })
    }

    async fn image_exists(&self, image: &ImageRef) -> Result<bool> {
        self.record("image_exists");
        Ok(self.images.lock().unwrap().iter().any(|i| i == &image.0))
    }

    async fn pull_image(&self, image: &ImageRef) -> Result<()> {
        self.record("pull_image");
        self.images.lock().unwrap().push(image.0.clone());
        Ok(())
    }

    async fn create(&self, spec: &VmSpec) -> Result<VmRef> {
        self.record("create");
        self.specs.lock().unwrap().push(spec.clone());
        if self.should_fail("create") {
            return Err(WargameError::VmCreationFailed(
                "fake create failure".to_string(),
            ));
        }
        self.vms.lock().unwrap().insert(
            spec.name.clone(),
            VmEntry {
                status: VmStatus::Stopped,
                ip: None,
            },
        );
        Ok(VmRef::new(self.name.clone(), spec.name.clone()))
    }

    async fn start(&self, vm: &VmRef, _opts: &StartOptions) -> Result<RuntimeInfo> {
        self.record("start");
        if self.should_fail("start") {
            return Err(WargameError::VmCreationFailed(
                "fake start failure".to_string(),
            ));
        }
        let mut vms = self.vms.lock().unwrap();
        let entry = vms.entry(vm.name.clone()).or_insert(VmEntry {
            status: VmStatus::Stopped,
            ip: None,
        });
        entry.status = VmStatus::Running;
        entry.ip = Some(self.default_ip);
        Ok(RuntimeInfo { pid: Some(4242) })
    }

    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr> {
        self.record("wait_ip");
        self.wait_ip_timeouts.lock().unwrap().push(timeout);
        if self.should_fail("wait_ip") {
            return Err(WargameError::GuestBootTimeout(timeout));
        }
        let ip = self
            .vms
            .lock()
            .unwrap()
            .get(&vm.name)
            .and_then(|e| e.ip)
            .unwrap_or(self.default_ip);
        Ok(ip)
    }

    async fn stop(&self, vm: &VmRef) -> Result<()> {
        self.record("stop");
        if let Some(entry) = self.vms.lock().unwrap().get_mut(&vm.name) {
            entry.status = VmStatus::Stopped;
        }
        Ok(())
    }

    async fn delete(&self, vm: &VmRef) -> Result<()> {
        self.record("delete");
        self.vms.lock().unwrap().remove(&vm.name);
        Ok(())
    }

    async fn status(&self, vm: &VmRef) -> Result<VmStatus> {
        self.record("status");
        Ok(self
            .vms
            .lock()
            .unwrap()
            .get(&vm.name)
            .map(|e| e.status)
            .unwrap_or(VmStatus::Missing))
    }

    async fn list(&self) -> Result<Vec<VmRef>> {
        self.record("list");
        Ok(self
            .vms
            .lock()
            .unwrap()
            .keys()
            .map(|name| VmRef::new(self.name.clone(), name.clone()))
            .collect())
    }

    fn console(&self, vm: &VmRef) -> ConsoleDescriptor {
        ConsoleDescriptor::Window {
            argv: vec!["tart".to_string(), "run".to_string(), vm.name.clone()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::profile::Resources;

    fn spec() -> VmSpec {
        VmSpec::new(
            "wg-test",
            ImageRef("wargame-profile-macos-minimal-arm64".to_string()),
            Resources {
                cpu: 2,
                memory_mib: 4096,
                disk_gib: 40,
            },
        )
    }

    #[tokio::test]
    async fn create_then_status_running_after_start() {
        let vm = FakeVmProvider::new();
        let vm_ref = vm.create(&spec()).await.unwrap();
        assert_eq!(vm_ref.name, "wg-test");
        assert_eq!(vm.status(&vm_ref).await.unwrap(), VmStatus::Stopped);

        vm.start(
            &vm_ref,
            &StartOptions {
                network: NetworkMode::Isolated,
                headless: true,
                log_path: "/dev/null".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(vm.status(&vm_ref).await.unwrap(), VmStatus::Running);
    }

    #[tokio::test]
    async fn status_missing_for_unknown_vm() {
        let vm = FakeVmProvider::new();
        let vm_ref = VmRef::new("tart", "nope");
        assert_eq!(vm.status(&vm_ref).await.unwrap(), VmStatus::Missing);
    }

    #[tokio::test]
    async fn with_backend_names_the_provider_and_its_vm_refs() {
        let vm = FakeVmProvider::with_backend("utm");
        assert_eq!(vm.name(), "utm");
        let vm_ref = vm.create(&spec()).await.unwrap();
        assert_eq!(vm_ref.backend, "utm");
    }

    #[tokio::test]
    async fn set_fail_on_create_errors() {
        let vm = FakeVmProvider::new();
        vm.set_fail_on("create");
        assert!(vm.create(&spec()).await.is_err());
    }

    #[tokio::test]
    async fn image_exists_reflects_add_image() {
        let vm = FakeVmProvider::new();
        let image = ImageRef("base".to_string());
        assert!(!vm.image_exists(&image).await.unwrap());
        vm.add_image("base");
        assert!(vm.image_exists(&image).await.unwrap());
    }
}
