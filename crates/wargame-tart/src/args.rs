//! Pure argv builders for every `tart` subcommand (DESIGN.md §1/§5). Kept
//! free of any `ProcessRunner` so they're trivial to unit-test and so
//! `provider.rs` never assembles a shell string — only `Vec<String>` argv.

use wargame_core::domain::{NetworkMode, Resources};
use wargame_core::ports::StartOptions;

pub fn clone_args(src: &str, dst: &str) -> Vec<String> {
    vec!["clone".to_string(), src.to_string(), dst.to_string()]
}

pub fn set_args(vm: &str, r: &Resources) -> Vec<String> {
    vec![
        "set".to_string(),
        vm.to_string(),
        "--cpu".to_string(),
        r.cpu.to_string(),
        "--memory".to_string(),
        r.memory_mib.to_string(),
        "--disk-size".to_string(),
        r.disk_gib.to_string(),
    ]
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

pub fn stop_args(vm: &str) -> Vec<String> {
    vec!["stop".to_string(), vm.to_string()]
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
        assert_eq!(stop_args("wg-abc"), vec!["stop", "wg-abc"]);
        assert_eq!(delete_args("wg-abc"), vec!["delete", "wg-abc"]);
        assert_eq!(list_args(), vec!["list", "--format", "json"]);
        assert_eq!(
            pull_args("ghcr.io/cirruslabs/macos-sequoia-vanilla:latest"),
            vec!["pull", "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest"]
        );
    }
}
