//! `TartProvider`: `VmProvider` implemented over the `tart` CLI (DESIGN.md
//! §5), driven entirely through `ProcessRunner` argv arrays — never a shell
//! string, and no secret ever appears in one of these commands' argv.

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use wargame_core::domain::{GuestArch, GuestOs, ProviderCapabilities, VmRef};
use wargame_core::error::{Result, WargameError};
use wargame_core::infra::process::{Cmd, ProcessRunner};
use wargame_core::ports::{
    BackendStatus, ConsoleDescriptor, ImageRef, RuntimeInfo, StartOptions, VmProvider, VmSpec,
    VmStatus,
};

use crate::{args, console, image, list::TartVm};

/// Static configuration for [`TartProvider`] (DESIGN.md §3.5's `[vm.tart]`
/// section, resolved into the shapes the provider actually needs).
#[derive(Debug, Clone)]
pub struct TartConfig {
    pub binary: String,
    pub vm_prefix: String,
    pub boot_timeout: Duration,
}

impl Default for TartConfig {
    fn default() -> Self {
        TartConfig {
            binary: "tart".to_string(),
            vm_prefix: "wg-".to_string(),
            boot_timeout: Duration::from_secs(300),
        }
    }
}

/// How often `wait_ip` polls `tart ip` (DESIGN.md §5).
const IP_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct TartProvider {
    runner: Arc<dyn ProcessRunner>,
    cfg: TartConfig,
}

impl TartProvider {
    pub fn new(runner: Arc<dyn ProcessRunner>, cfg: TartConfig) -> Self {
        TartProvider { runner, cfg }
    }

    /// The Tart VM name for a session id, e.g. `"wg-dh-out-of-bound-a1b2c3"`.
    pub fn vm_name(&self, session_id: &str) -> String {
        format!("{}{session_id}", self.cfg.vm_prefix)
    }

    /// The configured boot timeout (used for `wait_ip`/`wait_ready` waits by
    /// callers outside this module, e.g. [`crate::build::ProfileImageBuilder`]).
    pub fn boot_timeout(&self) -> Duration {
        self.cfg.boot_timeout
    }

    fn cmd(&self, args: Vec<String>) -> Cmd {
        Cmd::new(self.cfg.binary.clone(), args)
    }

    async fn list_vms(&self) -> Result<Vec<TartVm>> {
        let out = self.runner.run(&self.cmd(args::list_args())).await?;
        if out.status != 0 {
            return Err(WargameError::BackendUnavailable {
                backend: "tart".to_string(),
                reason: out.stderr,
            });
        }
        crate::list::parse_list(&out.stdout)
    }
}

#[async_trait]
impl VmProvider for TartProvider {
    fn name(&self) -> &str {
        "tart"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            guest_os: vec![GuestOs::Macos],
            guest_arch: vec![GuestArch::Arm64],
            graphical_console: true,
            ssh: true,
            snapshots: false,
            copy_in: true,
            remote: false,
        }
    }

    async fn healthcheck(&self) -> Result<BackendStatus> {
        let cmd = self.cmd(vec!["--version".to_string()]);
        match self.runner.run(&cmd).await {
            Ok(out) if out.status == 0 => Ok(BackendStatus {
                available: true,
                version: Some(out.stdout.trim().to_string()),
                detail: "ok".to_string(),
            }),
            Ok(out) => Ok(BackendStatus {
                available: false,
                version: None,
                detail: out.stderr,
            }),
            Err(e) => Ok(BackendStatus {
                available: false,
                version: None,
                detail: e.to_string(),
            }),
        }
    }

    async fn image_exists(&self, image: &ImageRef) -> Result<bool> {
        let vms = self.list_vms().await?;
        Ok(image::contains_image(&vms, &image.0))
    }

    async fn pull_image(&self, image: &ImageRef) -> Result<()> {
        let out = self
            .runner
            .run(&self.cmd(args::pull_args(&image.0)))
            .await?;
        if out.status != 0 {
            return Err(WargameError::VmCreationFailed(format!(
                "tart pull {} failed: {}",
                image.0, out.stderr
            )));
        }
        Ok(())
    }

    async fn create(&self, spec: &VmSpec) -> Result<VmRef> {
        let clone_out = self
            .runner
            .run(&self.cmd(args::clone_args(&spec.image.0, &spec.name)))
            .await?;
        if clone_out.status != 0 {
            return Err(WargameError::VmCreationFailed(format!(
                "tart clone {} {} failed: {}",
                spec.image.0, spec.name, clone_out.stderr
            )));
        }

        let set_out = self
            .runner
            .run(&self.cmd(args::set_args(&spec.name, &spec.resources)))
            .await?;
        if set_out.status != 0 {
            // Don't leave a half-configured clone behind (DESIGN.md §5).
            let _ = self
                .runner
                .run(&self.cmd(args::delete_args(&spec.name)))
                .await;
            return Err(WargameError::VmCreationFailed(format!(
                "tart set {} failed: {}",
                spec.name, set_out.stderr
            )));
        }

        Ok(VmRef {
            backend: "tart".to_string(),
            name: spec.name.clone(),
        })
    }

    async fn start(&self, vm: &VmRef, opts: &StartOptions) -> Result<RuntimeInfo> {
        let cmd = self.cmd(args::run_args(&vm.name, opts));
        let pid = self.runner.spawn_detached(&cmd, &opts.log_path).await?;
        Ok(RuntimeInfo { pid: Some(pid) })
    }

    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let out = self.runner.run(&self.cmd(args::ip_args(&vm.name))).await?;
            if out.status == 0 {
                if let Ok(ip) = IpAddr::from_str(out.stdout.trim()) {
                    return Ok(ip);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(WargameError::GuestBootTimeout(timeout));
            }
            tokio::time::sleep(IP_POLL_INTERVAL).await;
        }
    }

    async fn stop(&self, vm: &VmRef) -> Result<()> {
        let out = self
            .runner
            .run(&self.cmd(args::stop_args(&vm.name)))
            .await?;
        if out.status != 0 {
            return Err(WargameError::Other(format!(
                "tart stop {} failed: {}",
                vm.name, out.stderr
            )));
        }
        Ok(())
    }

    async fn delete(&self, vm: &VmRef) -> Result<()> {
        if let VmStatus::Running = self.status(vm).await? {
            // Best-effort: `tart delete` on a stopped VM still succeeds.
            let _ = self.stop(vm).await;
        }
        let out = self
            .runner
            .run(&self.cmd(args::delete_args(&vm.name)))
            .await?;
        if out.status != 0 {
            return Err(WargameError::CleanupFailed(format!(
                "tart delete {} failed: {}",
                vm.name, out.stderr
            )));
        }
        Ok(())
    }

    async fn status(&self, vm: &VmRef) -> Result<VmStatus> {
        let vms = self.list_vms().await?;
        Ok(match vms.iter().find(|v| v.name == vm.name) {
            None => VmStatus::Missing,
            Some(v) if v.running => VmStatus::Running,
            Some(_) => VmStatus::Stopped,
        })
    }

    async fn list(&self) -> Result<Vec<VmRef>> {
        let vms = self.list_vms().await?;
        Ok(vms
            .into_iter()
            .filter(|v| v.name.starts_with(&self.cfg.vm_prefix))
            .map(|v| VmRef {
                backend: "tart".to_string(),
                name: v.name,
            })
            .collect())
    }

    fn console(&self, vm: &VmRef) -> ConsoleDescriptor {
        console::console(&self.cfg.binary, &vm.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wargame_core::domain::{NetworkMode, Resources};
    use wargame_core::infra::process::Output;
    use wargame_core::testing::FakeRunner;

    fn resources() -> Resources {
        Resources {
            cpu: 4,
            memory_mib: 8192,
            disk_gib: 60,
        }
    }

    fn provider(runner: Arc<FakeRunner>) -> TartProvider {
        TartProvider::new(runner, TartConfig::default())
    }

    #[test]
    fn vm_name_uses_configured_prefix() {
        let p = TartProvider::new(
            Arc::new(FakeRunner::new()),
            TartConfig {
                vm_prefix: "wg-".to_string(),
                ..TartConfig::default()
            },
        );
        assert_eq!(
            p.vm_name("dh-out-of-bound-a1b2c3"),
            "wg-dh-out-of-bound-a1b2c3"
        );
    }

    #[tokio::test]
    async fn create_clones_then_sets_and_returns_vm_ref() {
        let runner = Arc::new(FakeRunner::new());
        let p = provider(runner.clone());
        let spec = VmSpec {
            name: "wg-dh-out-of-bound-a1b2c3".to_string(),
            image: ImageRef("wargame-profile-macos-re-arm64".to_string()),
            resources: resources(),
        };

        let vm = p.create(&spec).await.unwrap();
        assert_eq!(vm.backend, "tart");
        assert_eq!(vm.name, "wg-dh-out-of-bound-a1b2c3");

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].program, "tart");
        assert_eq!(calls[0].args[0], "clone");
        assert_eq!(calls[1].args[0], "set");
    }

    #[tokio::test]
    async fn create_deletes_the_clone_when_set_fails() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success(""); // clone
        runner.push_failure(1, "disk too small"); // set
        runner.push_success(""); // delete
        let p = provider(runner.clone());
        let spec = VmSpec {
            name: "wg-x".to_string(),
            image: ImageRef("base".to_string()),
            resources: resources(),
        };

        let err = p.create(&spec).await.unwrap_err();
        assert!(matches!(err, WargameError::VmCreationFailed(_)));

        let calls = runner.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].args[0], "clone");
        assert_eq!(calls[1].args[0], "set");
        assert_eq!(calls[2].args[0], "delete");
        assert_eq!(calls[2].args[1], "wg-x");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_ip_retries_until_an_ip_appears() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_failure(1, "no ip"); // still booting
        runner.push_failure(1, "no ip");
        runner.push_success("10.0.0.5\n");
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };

        let ip = p.wait_ip(&vm, Duration::from_secs(30)).await.unwrap();
        assert_eq!(ip, IpAddr::from_str("10.0.0.5").unwrap());
        assert_eq!(runner.calls().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_ip_times_out() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|_| {
            Ok(Output {
                status: 1,
                stdout: String::new(),
                stderr: "no ip".to_string(),
            })
        });
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };

        let err = p.wait_ip(&vm, Duration::from_millis(10)).await.unwrap_err();
        assert!(matches!(err, WargameError::GuestBootTimeout(_)));
    }

    #[tokio::test]
    async fn start_passes_net_host_only_for_isolated_and_no_graphics_only_headless() {
        let runner = Arc::new(FakeRunner::new());
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };
        let opts = StartOptions {
            network: NetworkMode::Isolated,
            headless: true,
            log_path: PathBuf::from("/tmp/run.log"),
        };

        let info = p.start(&vm, &opts).await.unwrap();
        assert!(info.pid.is_some());
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].args.contains(&"--no-graphics".to_string()));
        assert!(calls[0].args.contains(&"--net-host".to_string()));

        let gui_nat_opts = StartOptions {
            network: NetworkMode::Nat,
            headless: false,
            log_path: PathBuf::from("/tmp/run.log"),
        };
        p.start(&vm, &gui_nat_opts).await.unwrap();
        let calls = runner.calls();
        assert!(!calls[1].args.contains(&"--no-graphics".to_string()));
        assert!(!calls[1].args.contains(&"--net-host".to_string()));
    }

    #[tokio::test]
    async fn status_is_missing_for_unknown_vm() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success("[]");
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-unknown".to_string(),
        };
        assert_eq!(p.status(&vm).await.unwrap(), VmStatus::Missing);
    }

    #[tokio::test]
    async fn status_running_and_stopped() {
        let runner = Arc::new(FakeRunner::new());
        let list_json = r#"[{"Name":"wg-x","Source":"local","State":"running","Running":true,"Size":1,"Disk":1}]"#;
        runner.push_success(list_json);
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };
        assert_eq!(p.status(&vm).await.unwrap(), VmStatus::Running);
    }

    #[tokio::test]
    async fn delete_stops_a_running_vm_first() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|cmd| match cmd.args.first().map(String::as_str) {
            Some("list") => Ok(Output {
                status: 0,
                stdout: r#"[{"Name":"wg-x","Source":"local","State":"running","Running":true,"Size":1,"Disk":1}]"#.to_string(),
                stderr: String::new(),
            }),
            _ => Ok(Output::default()),
        });
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };

        p.delete(&vm).await.unwrap();
        let calls = runner.calls();
        assert_eq!(calls[0].args[0], "list");
        assert_eq!(calls[1].args[0], "stop");
        assert_eq!(calls[2].args[0], "delete");
    }

    #[tokio::test]
    async fn delete_skips_stop_for_a_stopped_vm() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|cmd| match cmd.args.first().map(String::as_str) {
            Some("list") => Ok(Output {
                status: 0,
                stdout: r#"[{"Name":"wg-x","Source":"local","State":"stopped","Running":false,"Size":1,"Disk":1}]"#.to_string(),
                stderr: String::new(),
            }),
            _ => Ok(Output::default()),
        });
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };

        p.delete(&vm).await.unwrap();
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args[0], "list");
        assert_eq!(calls[1].args[0], "delete");
    }

    #[tokio::test]
    async fn list_filters_by_prefix() {
        let runner = Arc::new(FakeRunner::new());
        let list_json = r#"[
            {"Name":"wg-a","Source":"local","State":"stopped","Running":false,"Size":1,"Disk":1},
            {"Name":"wargame-profile-macos-re-arm64","Source":"local","State":"stopped","Running":false,"Size":1,"Disk":1},
            {"Name":"wg-b","Source":"local","State":"running","Running":true,"Size":1,"Disk":1}
        ]"#;
        runner.push_success(list_json);
        let p = provider(runner.clone());
        let refs = p.list().await.unwrap();
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|r| r.name.starts_with("wg-")));
    }

    #[tokio::test]
    async fn image_exists_true_and_false() {
        let runner = Arc::new(FakeRunner::new());
        let list_json = r#"[{"Name":"wargame-profile-macos-re-arm64","Source":"local","State":"stopped","Running":false,"Size":1,"Disk":1}]"#;
        runner.push_success(list_json);
        let p = provider(runner.clone());
        assert!(p
            .image_exists(&ImageRef("wargame-profile-macos-re-arm64".to_string()))
            .await
            .unwrap());

        runner.push_success(list_json);
        assert!(!p
            .image_exists(&ImageRef("wargame-profile-other".to_string()))
            .await
            .unwrap());
    }

    #[test]
    fn capabilities_match_design() {
        let p = provider(Arc::new(FakeRunner::new()));
        let caps = p.capabilities();
        assert_eq!(caps.guest_os, vec![GuestOs::Macos]);
        assert_eq!(caps.guest_arch, vec![GuestArch::Arm64]);
        assert!(caps.graphical_console);
        assert!(caps.ssh);
        assert!(!caps.snapshots);
        assert!(caps.copy_in);
        assert!(!caps.remote);
    }

    #[test]
    fn console_delegates_to_console_module() {
        let p = provider(Arc::new(FakeRunner::new()));
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };
        let ConsoleDescriptor::Window { argv } = p.console(&vm) else {
            panic!("expected Window descriptor");
        };
        assert_eq!(argv[0], "tart");
        assert_eq!(argv[2], "wg-x");
    }
}
