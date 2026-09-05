//! Pure argv builders for every `tart` subcommand (DESIGN.md §1/§5). Kept
//! free of any `ProcessRunner` so they're trivial to unit-test and so
//! `provider.rs` never assembles a shell string — only `Vec<String>` argv.

use wargame_core::domain::{NetworkMode, Resources};
use wargame_core::ports::StartOptions;

pub fn clone_args(src: &str, dst: &str) -> Vec<String> {
    vec!["clone".to_string(), src.to_string(), dst.to_string()]
}

pub fn set_args(vm: &str, r: &Resources) -> Vec<String> {
    set_args_with_disk(vm, r, Some(r.disk_gib))
}

/// Like [`set_args`], but lets the caller omit `--disk-size` entirely.
/// Tart rejects `--disk-size` unless it is strictly larger than the VM's
/// current disk, so callers that already know the clone's current disk
/// size pass `None` when `r.disk_gib` wouldn't grow it (DESIGN.md §5).
pub fn set_args_with_disk(vm: &str, r: &Resources, disk_gib: Option<u64>) -> Vec<String> {
    let mut args = vec![
        "set".to_string(),
        vm.to_string(),
        "--cpu".to_string(),
        r.cpu.to_string(),
        "--memory".to_string(),
        r.memory_mib.to_string(),
    ];
    if let Some(disk_gib) = disk_gib {
        args.push("--disk-size".to_string());
        args.push(disk_gib.to_string());
    }
    args
}

pub fn run_args(vm: &str, o: &StartOptions) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        vm.to_string(),
        "--no-clipboard".to_string(),
        "--no-audio".to_string(),
    ];
    if o.headless {
        args.push("--no-graphics".to_string());
    }
    if o.network == NetworkMode::Isolated {
        args.push("--net-host".to_string());
    }
    args
}

pub fn ip_args(vm: &str) -> Vec<String> {
    vec!["ip".to_string(), vm.to_string()]
}

/// `tart stop` with an explicit `--timeout` (seconds): a plain `tart stop`
/// defaults to a 30s graceful-shutdown window (`tart stop --help`), which is
/// not enough for a guest with hundreds of MB of dirty pages after a bulk
/// Homebrew install -- it gets force-killed and the late writes are lost
/// (DESIGN.md §5).
pub fn stop_args(vm: &str, timeout_secs: u64) -> Vec<String> {
    vec![
        "stop".to_string(),
        vm.to_string(),
        "--timeout".to_string(),
        timeout_secs.to_string(),
    ]
}

pub fn delete_args(vm: &str) -> Vec<String> {
    vec!["delete".to_string(), vm.to_string()]
}

pub fn list_args() -> Vec<String> {
    vec![
        "list".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ]
}

pub fn pull_args(image: &str) -> Vec<String> {
    vec!["pull".to_string(), image.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn clone_args_exact() {
        assert_eq!(
            clone_args("base:latest", "wg-abc"),
            vec!["clone", "base:latest", "wg-abc"]
        );
    }

    #[test]
    fn set_args_exact() {
        let r = Resources {
            cpu: 4,
            memory_mib: 8192,
            disk_gib: 60,
        };
        assert_eq!(
            set_args("wg-abc", &r),
            vec![
                "set",
                "wg-abc",
                "--cpu",
                "4",
                "--memory",
                "8192",
                "--disk-size",
                "60"
            ]
        );
    }

    #[test]
    fn set_args_with_disk_omits_flag_when_none() {
        let r = Resources {
            cpu: 4,
            memory_mib: 8192,
            disk_gib: 60,
        };
        assert_eq!(
            set_args_with_disk("wg-abc", &r, None),
            vec!["set", "wg-abc", "--cpu", "4", "--memory", "8192"]
        );
    }

    #[test]
    fn set_args_with_disk_includes_flag_when_some() {
        let r = Resources {
            cpu: 4,
            memory_mib: 8192,
            disk_gib: 60,
        };
        assert_eq!(
            set_args_with_disk("wg-abc", &r, Some(70)),
            vec![
                "set",
                "wg-abc",
                "--cpu",
                "4",
                "--memory",
                "8192",
                "--disk-size",
                "70"
            ]
        );
    }

    fn opts(network: NetworkMode, headless: bool) -> StartOptions {
        StartOptions {
            network,
            headless,
            log_path: PathBuf::from("/tmp/run.log"),
        }
    }

    #[test]
    fn run_args_headless_isolated() {
        assert_eq!(
            run_args("wg-abc", &opts(NetworkMode::Isolated, true)),
            vec![
                "run",
                "wg-abc",
                "--no-clipboard",
                "--no-audio",
                "--no-graphics",
                "--net-host"
            ]
        );
    }

    #[test]
    fn run_args_gui_nat_has_neither_flag() {
        assert_eq!(
            run_args("wg-abc", &opts(NetworkMode::Nat, false)),
            vec!["run", "wg-abc", "--no-clipboard", "--no-audio"]
        );
    }

    #[test]
    fn run_args_headless_nat_omits_net_host() {
        assert_eq!(
            run_args("wg-abc", &opts(NetworkMode::Nat, true)),
            vec![
                "run",
                "wg-abc",
                "--no-clipboard",
                "--no-audio",
                "--no-graphics"
            ]
        );
    }

    #[test]
    fn run_args_gui_isolated_omits_no_graphics() {
        assert_eq!(
            run_args("wg-abc", &opts(NetworkMode::Isolated, false)),
            vec![
                "run",
                "wg-abc",
                "--no-clipboard",
                "--no-audio",
                "--net-host"
            ]
        );
    }

    #[test]
    fn ip_stop_delete_list_pull_exact() {
        assert_eq!(ip_args("wg-abc"), vec!["ip", "wg-abc"]);
        assert_eq!(
            stop_args("wg-abc", 180),
            vec!["stop", "wg-abc", "--timeout", "180"]
        );
        assert_eq!(delete_args("wg-abc"), vec!["delete", "wg-abc"]);
        assert_eq!(list_args(), vec!["list", "--format", "json"]);
        assert_eq!(
            pull_args("ghcr.io/cirruslabs/macos-sequoia-vanilla:latest"),
            vec!["pull", "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest"]
        );
    }
}
