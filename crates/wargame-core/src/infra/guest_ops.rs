//! The one place a guest command's OS shape is decided (spec.md §5 item 3:
//! "endpoint 및 명령 전략은 OS/세션별로 정한다").
//!
//! Application services (`app::launch`, `app::profile_build`,
//! `app::lifecycle`) ask [`GuestOps`] for "make these directories", "run
//! this provisioning script", "flush the filesystem" and get back a
//! [`GuestCommand`] phrased for the endpoint's own shell. They never test
//! the guest OS themselves, and what they get for a POSIX endpoint is byte
//! for byte what the pre-multi-OS code sent -- the macOS path
//! `docs/verification.md` records as working against a real VM is not
//! re-derived here.
//!
//! Nothing here builds a *host* shell string: a [`GuestCommand::Argv`] is
//! quoted or encoded by the transport, and a [`GuestCommand::Script`] is a
//! template this crate owns whose data is already quoted for the guest's
//! shell.

use crate::domain::{GuestEndpoint, GuestShell};
use crate::error::Result;
use crate::infra::powershell;
use crate::ports::guest_transport::{ExecResult, GuestTransport};

/// One command to run in a guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestCommand {
    /// A program and its arguments. The transport quotes (POSIX) or encodes
    /// (PowerShell) every element as a literal, so this is the right shape
    /// for anything whose arguments are plain strings -- every POSIX
    /// command, and Windows *native* commands like `powershell.exe`.
    Argv(Vec<String>),
    /// A trusted script in the guest's own shell language, with every piece
    /// of data in it already quoted for that shell.
    ///
    /// PowerShell *cmdlets* need this: a cmdlet binds a parameter only from
    /// a bare `-Name` token, so `New-Item` cannot be assembled from quoted
    /// argv (`'-Force'` would arrive as a positional value). The parameter
    /// names are written out in the template and only the data is quoted.
    Script(String),
}

impl GuestCommand {
    fn argv(elements: &[&str]) -> Self {
        GuestCommand::Argv(elements.iter().map(|s| s.to_string()).collect())
    }

    /// The argv, if this is an [`GuestCommand::Argv`].
    pub fn as_argv(&self) -> Option<&[String]> {
        match self {
            GuestCommand::Argv(argv) => Some(argv),
            GuestCommand::Script(_) => None,
        }
    }

    /// A one-line rendering for the provisioning log.
    pub fn describe(&self) -> String {
        match self {
            GuestCommand::Argv(argv) => argv.join(" "),
            GuestCommand::Script(script) => script.replace('\n', "; "),
        }
    }

    /// Runs this command over `transport`.
    pub async fn run(
        &self,
        transport: &dyn GuestTransport,
        endpoint: &GuestEndpoint,
    ) -> Result<ExecResult> {
        match self {
            GuestCommand::Argv(argv) => transport.exec(endpoint, argv).await,
            GuestCommand::Script(script) => transport.exec_script(endpoint, script).await,
        }
    }
}

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
    /// POSIX takes them all in one `mkdir -p`; PowerShell gets one
    /// `New-Item` per directory, because `New-Item`'s second positional
    /// argument is `-Value`, not another path.
    pub fn mkdir_p(&self, dirs: &[String]) -> Vec<GuestCommand> {
        match self.shell {
            GuestShell::Posix => {
                let mut argv = vec!["mkdir".to_string(), "-p".to_string()];
                argv.extend(dirs.iter().cloned());
                vec![GuestCommand::Argv(argv)]
            }
            GuestShell::PowerShell => dirs
                .iter()
                .map(|dir| {
                    GuestCommand::Script(format!(
                        "New-Item -ItemType Directory -Force -Path {} | Out-Null",
                        powershell::quote(dir)
                    ))
                })
                .collect(),
        }
    }

    /// Removes a directory tree and everything under it. Never fatal for
    /// callers: both forms succeed when the path is already gone.
    pub fn remove_tree(&self, path: &str) -> GuestCommand {
        match self.shell {
            GuestShell::Posix => GuestCommand::argv(&["rm", "-rf", path]),
            GuestShell::PowerShell => GuestCommand::Script(format!(
                "Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath {}",
                powershell::quote(path)
            )),
        }
    }

    /// Flushes the guest's filesystem before a stop, so a stop that outruns
    /// its graceful-shutdown window does not lose dirty pages
    /// (DESIGN.md §5). `Write-VolumeCache` is the Windows equivalent of
    /// `sync`; a guest too old to have it just fails the command, which
    /// every caller treats as non-fatal.
    pub fn flush(&self) -> GuestCommand {
        match self.shell {
            GuestShell::Posix => GuestCommand::argv(&["sync"]),
            GuestShell::PowerShell => {
                GuestCommand::Script("Write-VolumeCache -DriveLetter C".to_string())
            }
        }
    }

    /// A command that does nothing and succeeds: the "is the guest still
    /// answering?" probe.
    pub fn probe(&self) -> GuestCommand {
        match self.shell {
            GuestShell::Posix => GuestCommand::argv(&["true"]),
            GuestShell::PowerShell => {
                GuestCommand::Script("Write-Output 'wargame-ready'".to_string())
            }
        }
    }

    /// Runs one provisioning script (spec.md §3 FR-3: "macOS/Kali는 Bash,
    /// Windows는 PowerShell로 실행한다"). The Windows form starts a *nested*
    /// `powershell.exe` -- a native command, so its arguments are ordinary
    /// literals -- with an explicit execution policy, and the script's exit
    /// code (3010 included) propagates through `$LASTEXITCODE`.
    pub fn run_script(&self, path: &str) -> GuestCommand {
        match self.shell {
            GuestShell::Posix => GuestCommand::argv(&["bash", path]),
            GuestShell::PowerShell => GuestCommand::argv(&[
                "powershell",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                path,
            ]),
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
    ///
    /// `require_digests` is what turns a missing `incoming/.wargame-sha256`
    /// into an error instead of silently skipping verification: the host
    /// always stages that file, so its absence means the transfer did not
    /// land, and a launch must not carry on unverified (spec.md §3 FR-4).
    pub fn run_bootstrap(
        &self,
        script: &str,
        workspace: &str,
        extract: bool,
        require_digests: bool,
    ) -> GuestCommand {
        match self.shell {
            GuestShell::Posix => {
                let mut argv = vec![
                    "bash".to_string(),
                    script.to_string(),
                    workspace.to_string(),
                    format!("--extract={extract}"),
                ];
                if require_digests {
                    argv.push("--require-digests".to_string());
                }
                GuestCommand::Argv(argv)
            }
            GuestShell::PowerShell => {
                let mut argv = match self.run_script(script) {
                    GuestCommand::Argv(argv) => argv,
                    GuestCommand::Script(_) => unreachable!("run_script is argv on Windows"),
                };
                argv.push("-Workspace".to_string());
                argv.push(workspace.to_string());
                argv.push("-Extract".to_string());
                argv.push(extract.to_string());
                if require_digests {
                    argv.push("-RequireDigests".to_string());
                }
                GuestCommand::Argv(argv)
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
        assert_eq!(
            commands[0],
            GuestCommand::Script(
                "New-Item -ItemType Directory -Force -Path 'a' | Out-Null".to_string()
            )
        );
    }
}
