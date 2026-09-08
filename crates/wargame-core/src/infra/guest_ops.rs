//! The one place a guest command's OS shape is decided (spec.md §5 item 3:
//! "endpoint 및 명령 전략은 OS/세션별로 정한다").
//!
//! Application services (`app::launch`, `app::profile_build`,
//! `app::lifecycle`) ask [`GuestOps`] for "make these directories", "run
//! this provisioning script", "flush the filesystem" and get back argv for
//! the endpoint's own shell. They never test the guest OS themselves, and
//! the argv they get for a POSIX endpoint is byte for byte what the
//! pre-multi-OS code sent -- the macOS path that `docs/verification.md`
//! records as working against a real VM is not re-derived here.
//!
//! Everything below is argv, never a shell string: the transport
//! ([`crate::infra::SshGuestTransport`]) is what turns argv into a POSIX
//! command line or a PowerShell `-EncodedCommand`.

use crate::domain::{GuestEndpoint, GuestShell};

/// Command and path vocabulary for one guest shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestOps {
    shell: GuestShell,
}

impl GuestOps {
    pub fn new(shell: GuestShell) -> Self {
        GuestOps { shell }
    }

    /// The vocabulary for an endpoint's shell -- the session's saved shell
    /// at every step after launch.
    pub fn for_endpoint(endpoint: &GuestEndpoint) -> Self {
        GuestOps::new(endpoint.shell)
    }

    pub fn shell(&self) -> GuestShell {
        self.shell
    }

    /// The guest's path separator.
    pub fn separator(&self) -> char {
        match self.shell {
            GuestShell::Posix => '/',
            GuestShell::PowerShell => '\\',
        }
    }

    /// `base` with any trailing separators removed, so joining never
    /// produces a doubled separator (`C:\wargame\` -> `C:\wargame`).
    pub fn trim_root(&self, base: &str) -> String {
        let trimmed = match self.shell {
            GuestShell::Posix => base.trim_end_matches('/'),
            GuestShell::PowerShell => base.trim_end_matches(['\\', '/']),
        };
        trimmed.to_string()
    }

    /// Joins a guest path. `child` is always written with `/` by callers
    /// (it comes from profile YAML or a challenge slug), so it is rewritten
    /// to the guest's separator.
    pub fn join(&self, base: &str, child: &str) -> String {
        let base = self.trim_root(base);
        let child = match self.shell {
            GuestShell::Posix => child.to_string(),
            GuestShell::PowerShell => child.replace('/', "\\"),
        };
        format!("{base}{}{child}", self.separator())
    }

    /// A directory as a `copy_in` destination: a trailing separator, so
    /// `scp` copies *into* it rather than over it.
    pub fn dir_dest(&self, dir: &str) -> String {
        format!("{}{}", self.trim_root(dir), self.separator())
    }

    /// Creates every directory in `dirs`, parents included.
    ///
    /// POSIX takes them all in one `mkdir -p`; PowerShell's `New-Item` binds
    /// only its first positional argument to `-Path` (the second would land
    /// on `-Value`), so Windows gets one command per directory.
    pub fn mkdir_p(&self, dirs: &[String]) -> Vec<Vec<String>> {
        match self.shell {
            GuestShell::Posix => {
                let mut argv = vec!["mkdir".to_string(), "-p".to_string()];
                argv.extend(dirs.iter().cloned());
                vec![argv]
            }
            GuestShell::PowerShell => dirs
                .iter()
                .map(|dir| {
                    vec![
                        "New-Item".to_string(),
                        "-ItemType".to_string(),
                        "Directory".to_string(),
                        "-Force".to_string(),
                        "-Path".to_string(),
                        dir.clone(),
                    ]
                })
                .collect(),
        }
    }

    /// Removes a directory tree and everything under it. Never fatal for
    /// callers: both forms succeed when the path is already gone.
    pub fn remove_tree(&self, path: &str) -> Vec<String> {
        match self.shell {
            GuestShell::Posix => vec!["rm".to_string(), "-rf".to_string(), path.to_string()],
            GuestShell::PowerShell => vec![
                "Remove-Item".to_string(),
                "-Recurse".to_string(),
                "-Force".to_string(),
                "-ErrorAction".to_string(),
                "SilentlyContinue".to_string(),
                "-LiteralPath".to_string(),
                path.to_string(),
            ],
        }
    }

    /// Flushes the guest's filesystem before a stop, so a stop that outruns
    /// its graceful-shutdown window does not lose dirty pages
    /// (DESIGN.md §5). `Write-VolumeCache` is the Windows equivalent of
    /// `sync`; a guest too old to have it just fails the command, which
    /// every caller treats as non-fatal.
    pub fn flush(&self) -> Vec<String> {
        match self.shell {
            GuestShell::Posix => vec!["sync".to_string()],
            GuestShell::PowerShell => vec![
                "Write-VolumeCache".to_string(),
                "-DriveLetter".to_string(),
                "C".to_string(),
            ],
        }
    }

    /// A command that does nothing and succeeds: the "is the guest still
    /// answering?" probe.
    pub fn probe(&self) -> Vec<String> {
        match self.shell {
            GuestShell::Posix => vec!["true".to_string()],
            GuestShell::PowerShell => vec!["Write-Output".to_string(), "wargame-ready".to_string()],
        }
    }

    /// Runs one provisioning script (spec.md §3 FR-3: "macOS/Kali는 Bash,
    /// Windows는 PowerShell로 실행한다"). The Windows form starts a *nested*
    /// `powershell` so the script runs from a file with an explicit
    /// execution policy, and its exit code -- 3010 included -- propagates
    /// through `$LASTEXITCODE`.
    pub fn run_script(&self, path: &str) -> Vec<String> {
        match self.shell {
            GuestShell::Posix => vec!["bash".to_string(), path.to_string()],
            GuestShell::PowerShell => vec![
                "powershell".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                path.to_string(),
            ],
        }
    }

    /// The guest bootstrap script for this shell (`guest/bootstrap.sh` or
    /// `guest/bootstrap.ps1`).
    pub fn bootstrap_name(&self) -> &'static str {
        match self.shell {
            GuestShell::Posix => "bootstrap.sh",
            GuestShell::PowerShell => "bootstrap.ps1",
        }
    }

    /// Runs the guest bootstrap over one challenge workspace.
    pub fn run_bootstrap(&self, script: &str, workspace: &str, extract: bool) -> Vec<String> {
        match self.shell {
            GuestShell::Posix => vec![
                "bash".to_string(),
                script.to_string(),
                workspace.to_string(),
                format!("--extract={extract}"),
            ],
            GuestShell::PowerShell => {
                let mut argv = self.run_script(script);
                argv.push("-Workspace".to_string());
                argv.push(workspace.to_string());
                argv.push("-Extract".to_string());
                argv.push(extract.to_string());
                argv
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_paths_join_with_a_slash() {
        let ops = GuestOps::new(GuestShell::Posix);
        assert_eq!(
            ops.join("/home/kali/wargame/", "s/incoming"),
            "/home/kali/wargame/s/incoming"
        );
        assert_eq!(ops.trim_root("/home/kali/wargame//"), "/home/kali/wargame");
        assert_eq!(
            ops.dir_dest("/home/kali/wargame/incoming"),
            "/home/kali/wargame/incoming/"
        );
    }

    #[test]
    fn windows_paths_join_with_a_backslash() {
        let ops = GuestOps::new(GuestShell::PowerShell);
        assert_eq!(
            ops.join(r"C:\wargame", "s/incoming"),
            r"C:\wargame\s\incoming"
        );
        assert_eq!(ops.trim_root(r"C:\wargame\"), r"C:\wargame");
        assert_eq!(
            ops.dir_dest(r"C:\wargame\incoming"),
            r"C:\wargame\incoming\"
        );
    }

    #[test]
    fn a_windows_mkdir_is_one_command_per_directory() {
        let ops = GuestOps::new(GuestShell::PowerShell);
        let commands = ops.mkdir_p(&["a".to_string(), "b".to_string()]);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].last().unwrap(), "a");
        assert_eq!(commands[1].last().unwrap(), "b");
    }
}
