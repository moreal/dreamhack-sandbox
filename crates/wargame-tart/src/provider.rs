//! `TartProvider`: `VmProvider` implemented over the `tart` CLI (DESIGN.md
//! §5), driven entirely through `ProcessRunner` argv arrays — never a shell
//! string, and no secret ever appears in one of these commands' argv.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
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

/// The remediation text for a host where isolated networking can't run
/// (DESIGN.md §5/§7). Reuses `wargame-core`'s wording verbatim so the
/// message is identical everywhere it's shown (CLI error, `doctor`).
pub fn isolated_network_hint(softnet_path: Option<&Path>) -> String {
    wargame_core::error::isolated_network_hint(softnet_path)
}

/// How many of the run log's trailing non-empty lines to include when a VM
/// exits during boot (DESIGN.md §5) -- enough to show the actual failure
/// (e.g. Softnet's "root privileges are required...") without dumping the
/// whole log.
const RUN_LOG_TAIL_LINES: usize = 5;

/// The last `RUN_LOG_TAIL_LINES` non-empty lines of `log_path`, tolerating a
/// missing or unreadable file (in which case a placeholder is returned so
/// the resulting error message still says *something*).
fn tail_run_log(log_path: &Path) -> String {
    match std::fs::read_to_string(log_path) {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = lines.len().saturating_sub(RUN_LOG_TAIL_LINES);
            lines[start..].join("\n")
        }
        Err(_) => format!("(no run log at {})", log_path.display()),
    }
}

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
    /// The `run.log` path passed to the most recent `start()` for each VM
    /// name, so `wait_ip` can show its tail when the runner process died
    /// during boot (DESIGN.md §5).
    run_logs: Mutex<HashMap<String, PathBuf>>,
}

impl TartProvider {
    pub fn new(runner: Arc<dyn ProcessRunner>, cfg: TartConfig) -> Self {
        TartProvider {
            runner,
            cfg,
            run_logs: Mutex::new(HashMap::new()),
        }
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

    /// Whether isolated networking (`--net-host`, backed by Tart's Softnet)
    /// can actually run on this host: Softnet needs root, and Tart invokes
    /// it with non-interactive `sudo` (DESIGN.md §5/§7), so this resolves
    /// `softnet` on `PATH` and probes non-interactive sudo against it.
    /// Never runs anything through a shell string -- always argv.
    async fn isolated_network_available(&self) -> bool {
        let Some(softnet_path) = self.runner.which("softnet") else {
            return false;
        };
        let cmd = Cmd::new(
            "sudo",
            vec![
                "-n".to_string(),
                softnet_path.display().to_string(),
                "--help".to_string(),
            ],
        );
        matches!(self.runner.run(&cmd).await, Ok(out) if out.status == 0)
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
            Ok(out) if out.status == 0 => {
                let isolated_network = Some(self.isolated_network_available().await);
                Ok(BackendStatus {
                    available: true,
                    version: Some(out.stdout.trim().to_string()),
                    detail: "ok".to_string(),
                    isolated_network,
                })
            }
            Ok(out) => Ok(BackendStatus {
                available: false,
                version: None,
                detail: out.stderr,
                isolated_network: None,
            }),
            Err(e) => Ok(BackendStatus {
                available: false,
                version: None,
                detail: e.to_string(),
                isolated_network: None,
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

        // Tart rejects `--disk-size` unless it's strictly larger than the
        // clone's *current* disk (clones inherit the base image's disk), so
        // look up the clone's current size and only request a resize when
        // it would actually grow (DESIGN.md §5).
        let current_disk_gib = match self.list_vms().await {
            Ok(vms) => match vms.iter().find(|v| v.name == spec.name) {
                Some(v) => Some(v.disk_gib),
                None => {
                    tracing::warn!(
                        vm = %spec.name,
                        "tart list did not report the newly cloned VM; treating current disk size as unknown"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    vm = %spec.name,
                    error = %e,
                    "tart list failed while checking the clone's current disk size; treating it as unknown"
                );
                None
            }
        };
        let disk_gib = match current_disk_gib {
            Some(current) if spec.resources.disk_gib > current => Some(spec.resources.disk_gib),
            Some(_) => None,
            None => Some(spec.resources.disk_gib),
        };

        let set_out = self
            .runner
            .run(&self.cmd(args::set_args_with_disk(
                &spec.name,
                &spec.resources,
                disk_gib,
            )))
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
        self.run_logs
            .lock()
            .unwrap()
            .insert(vm.name.clone(), opts.log_path.clone());
        Ok(RuntimeInfo { pid: Some(pid) })
    }

    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut first_iteration = true;
        loop {
            let out = self.runner.run(&self.cmd(args::ip_args(&vm.name))).await?;
            if out.status == 0 {
                if let Ok(ip) = IpAddr::from_str(out.stdout.trim()) {
                    return Ok(ip);
                }
            }

            // The `tart run` process can die (e.g. Softnet needing root)
            // long before the boot timeout elapses; polling `tart ip` alone
            // would otherwise spin for the whole timeout with no useful
            // error (DESIGN.md §5). Skip this on the very first iteration so
            // a VM that simply hasn't reported `Running` yet isn't flagged.
            if !first_iteration {
                if let Ok(vms) = self.list_vms().await {
                    if let Some(v) = vms.iter().find(|v| v.name == vm.name) {
                        if !v.running {
                            let log_path = self
                                .run_logs
                                .lock()
                                .unwrap()
                                .get(&vm.name)
                                .cloned()
                                .unwrap_or_else(|| PathBuf::from("/dev/null"));
                            let tail = tail_run_log(&log_path);
                            return Err(WargameError::VmCreationFailed(format!(
                                "VM exited during boot: {tail}"
                            )));
                        }
                    }
                }
            }
            first_iteration = false;

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
    async fn healthcheck_isolated_network_false_when_softnet_missing() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success("tart 2.0.0\n"); // --version
                                             // `which("softnet")` returns None: no set_which() call.
        let p = provider(runner.clone());

        let status = p.healthcheck().await.unwrap();
        assert!(status.available);
        assert_eq!(status.isolated_network, Some(false));
    }

    #[tokio::test]
    async fn healthcheck_isolated_network_false_when_sudo_n_fails() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_which("softnet", PathBuf::from("/opt/homebrew/bin/softnet"));
        runner.push_success("tart 2.0.0\n"); // --version
        runner.push_failure(1, "sudo: a password is required"); // sudo -n softnet --help
        let p = provider(runner.clone());

        let status = p.healthcheck().await.unwrap();
        assert!(status.available);
        assert_eq!(status.isolated_network, Some(false));

        let calls = runner.calls();
        let sudo_call = calls
            .iter()
            .find(|c| c.program == "sudo")
            .expect("expected a sudo invocation");
        assert_eq!(sudo_call.args[0], "-n");
        assert_eq!(sudo_call.args[1], "/opt/homebrew/bin/softnet");
        assert_eq!(sudo_call.args[2], "--help");
    }

    #[tokio::test]
    async fn healthcheck_isolated_network_true_when_sudo_n_succeeds() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_which("softnet", PathBuf::from("/opt/homebrew/bin/softnet"));
        runner.push_success("tart 2.0.0\n"); // --version
        runner.push_success(""); // sudo -n softnet --help
        let p = provider(runner.clone());

        let status = p.healthcheck().await.unwrap();
        assert!(status.available);
        assert_eq!(status.isolated_network, Some(true));
    }

    #[test]
    fn isolated_network_hint_mentions_sudoers_and_nat_fallback() {
        let hint = isolated_network_hint(Some(Path::new("/opt/homebrew/bin/softnet")));
        assert!(hint.contains("/opt/homebrew/bin/softnet"));
        assert!(hint.contains("/etc/sudoers.d/wargame-softnet"));
        assert!(hint.contains("--network nat"));

        let default_hint = isolated_network_hint(None);
        assert!(default_hint.contains("/opt/homebrew/bin/softnet"));
    }

    /// A `tart list --format json` response reporting a single VM named
    /// `name` with the given current disk size in GiB.
    fn list_json_with_disk(name: &str, disk_gib: u64) -> String {
        format!(
            r#"[{{"Name":"{name}","Source":"local","State":"stopped","Running":false,"Size":1,"Disk":{disk_gib}}}]"#
        )
    }

    #[tokio::test]
    async fn create_clones_then_sets_and_returns_vm_ref() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success(""); // clone
        runner.push_success(list_json_with_disk("wg-dh-out-of-bound-a1b2c3", 50)); // list
        runner.push_success(""); // set
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
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].program, "tart");
        assert_eq!(calls[0].args[0], "clone");
        assert_eq!(calls[1].args[0], "list");
        assert_eq!(calls[2].args[0], "set");
        // requested 60 > current 50, so the resize is still requested.
        assert!(calls[2].args.contains(&"--disk-size".to_string()));
    }

    #[tokio::test]
    async fn create_deletes_the_clone_when_set_fails() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success(""); // clone
        runner.push_success(list_json_with_disk("wg-x", 50)); // list
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
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].args[0], "clone");
        assert_eq!(calls[1].args[0], "list");
        assert_eq!(calls[2].args[0], "set");
        assert_eq!(calls[3].args[0], "delete");
        assert_eq!(calls[3].args[1], "wg-x");
    }

    #[tokio::test]
    async fn create_omits_disk_size_when_requested_disk_not_larger_than_current() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success(""); // clone
        runner.push_success(list_json_with_disk("wg-x", 50)); // list: current disk 50 GiB
        runner.push_success(""); // set
        let p = provider(runner.clone());
        let spec = VmSpec {
            name: "wg-x".to_string(),
            image: ImageRef("base".to_string()),
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 40, // <= current disk: must not pass --disk-size
            },
        };

        p.create(&spec).await.unwrap();

        let calls = runner.calls();
        let set_call = &calls[2];
        assert_eq!(set_call.args[0], "set");
        assert!(!set_call.args.contains(&"--disk-size".to_string()));
    }

    #[tokio::test]
    async fn create_passes_disk_size_when_requested_disk_larger_than_current() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success(""); // clone
        runner.push_success(list_json_with_disk("wg-x", 50)); // list: current disk 50 GiB
        runner.push_success(""); // set
        let p = provider(runner.clone());
        let spec = VmSpec {
            name: "wg-x".to_string(),
            image: ImageRef("base".to_string()),
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 60, // > current disk: must pass --disk-size 60
            },
        };

        p.create(&spec).await.unwrap();

        let calls = runner.calls();
        let set_call = &calls[2];
        assert_eq!(set_call.args[0], "set");
        let disk_flag_idx = set_call
            .args
            .iter()
            .position(|a| a == "--disk-size")
            .expect("expected --disk-size to be passed");
        assert_eq!(set_call.args[disk_flag_idx + 1], "60");
    }

    #[tokio::test]
    async fn create_keeps_disk_size_when_list_lookup_fails_to_find_the_clone() {
        let runner = Arc::new(FakeRunner::new());
        runner.push_success(""); // clone
                                 // list succeeds but doesn't report our VM (lookup failure case).
        runner.push_success(list_json_with_disk("some-other-vm", 50));
        runner.push_success(""); // set
        let p = provider(runner.clone());
        let spec = VmSpec {
            name: "wg-x".to_string(),
            image: ImageRef("base".to_string()),
            resources: Resources {
                cpu: 4,
                memory_mib: 8192,
                disk_gib: 40,
            },
        };

        p.create(&spec).await.unwrap();

        let calls = runner.calls();
        let set_call = &calls[2];
        assert_eq!(set_call.args[0], "set");
        let disk_flag_idx = set_call
            .args
            .iter()
            .position(|a| a == "--disk-size")
            .expect("current disk unknown: --disk-size should still be passed");
        assert_eq!(set_call.args[disk_flag_idx + 1], "40");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_ip_retries_until_an_ip_appears() {
        let runner = Arc::new(FakeRunner::new());
        let ip_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ip_calls_responder = ip_calls.clone();
        runner.set_responder(move |cmd| {
            match cmd.args.first().map(String::as_str) {
                Some("ip") => {
                    let n = ip_calls_responder.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 2 {
                        Ok(Output {
                            status: 1,
                            stdout: String::new(),
                            stderr: "no ip".to_string(),
                        })
                    } else {
                        Ok(Output {
                            status: 0,
                            stdout: "10.0.0.5\n".to_string(),
                            stderr: String::new(),
                        })
                    }
                }
                // Still booting, but running -- must not be mistaken for an
                // exited VM (DESIGN.md §5).
                Some("list") => Ok(Output {
                    status: 0,
                    stdout: r#"[{"Name":"wg-x","Source":"local","State":"running","Running":true,"Size":1,"Disk":1}]"#
                        .to_string(),
                    stderr: String::new(),
                }),
                _ => Ok(Output::default()),
            }
        });
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };

        let ip = p.wait_ip(&vm, Duration::from_secs(30)).await.unwrap();
        assert_eq!(ip, IpAddr::from_str("10.0.0.5").unwrap());
        assert_eq!(ip_calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_ip_fails_fast_when_the_vm_exited_during_boot() {
        let log_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            log_file.path(),
            "some earlier line\n\
             root privileges are required to run and passwordless sudo was not available\n\
             RuntimeFailed(why: \"Softnet process terminated prematurely\")\n",
        )
        .unwrap();
        let log_path = log_file.path().to_path_buf();

        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(move |cmd| match cmd.args.first().map(String::as_str) {
            Some("ip") => Ok(Output {
                status: 1,
                stdout: String::new(),
                stderr: "no ip".to_string(),
            }),
            Some("list") => Ok(Output {
                status: 0,
                stdout: r#"[{"Name":"wg-x","Source":"local","State":"stopped","Running":false,"Size":1,"Disk":1}]"#
                    .to_string(),
                stderr: String::new(),
            }),
            _ => Ok(Output::default()),
        });
        let p = provider(runner.clone());
        let vm = VmRef {
            backend: "tart".to_string(),
            name: "wg-x".to_string(),
        };
        p.start(
            &vm,
            &StartOptions {
                network: NetworkMode::Isolated,
                headless: true,
                log_path: log_path.clone(),
            },
        )
        .await
        .unwrap();

        let err = p.wait_ip(&vm, Duration::from_secs(300)).await.unwrap_err();
        let WargameError::VmCreationFailed(msg) = err else {
            panic!("expected VmCreationFailed, got {err:?}");
        };
        assert!(msg.contains("Softnet"), "message was: {msg}");
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
