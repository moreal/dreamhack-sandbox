//! `wargame` CLI surface (DESIGN.md §7): every subcommand's exact shape.
//! `main.rs` owns dispatch; this module only owns parsing.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use wargame_core::domain::NetworkMode;

const AFTER_HELP: &str = "\
Quick start:
  wargame doctor                       check the host (Tart, ssh, disk, auth)
  wargame auth login                   store a Dreamhack session in the Keychain
  wargame profile build                build the default profile's VM image (once)
  wargame search rev                   find a challenge
  wargame launch dreamhack:15          boot it in a fresh isolated VM
  wargame ssh                          shell into the most recent session

Session ids can be shortened: any unique prefix or substring of the id or the
challenge title works (`wargame ssh out_of`), and omitting the id means the
most recent session. Run `wargame` with no arguments for the TUI.";

#[derive(Debug, Parser)]
#[command(
    name = "wargame",
    version,
    about = "Search a wargame challenge and launch it into a disposable VM.",
    after_help = AFTER_HELP
)]
pub struct Cli {
    /// Verbose (debug-level) logging.
    #[arg(long, global = true, display_order = 900)]
    pub verbose: bool,
    /// Path to config.toml (default: $WARGAME_HOME/config/config.toml).
    #[arg(long, global = true, value_name = "FILE", display_order = 901)]
    pub config: Option<PathBuf>,
    /// Overrides `$WARGAME_HOME` (default: ~/Library/Application Support/wargame-vm).
    #[arg(long, global = true, value_name = "DIR", display_order = 902)]
    pub home: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// `--network` values, mirrored from [`NetworkMode`] so clap can list and
/// complete them (`wargame-core` does not depend on clap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NetworkArg {
    /// Host-only vmnet: SSH from this Mac works, no LAN or internet (needs Softnet sudo).
    Isolated,
    /// Tart's shared networking: outbound internet access. Asks for confirmation.
    Nat,
}

impl From<NetworkArg> for NetworkMode {
    fn from(value: NetworkArg) -> Self {
        match value {
            NetworkArg::Isolated => NetworkMode::Isolated,
            NetworkArg::Nat => NetworkMode::Nat,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search challenges on the configured provider.
    #[command(visible_alias = "find")]
    Search {
        /// Text to search for (matched against titles).
        query: String,
        /// Only show challenges in this category (e.g. pwnable, reversing, web).
        #[arg(long)]
        category: Option<String>,
        /// Result page to show (20 per page).
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Print the raw result page as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one challenge's description and files.
    #[command(visible_aliases = ["show", "info"])]
    Open {
        /// `provider:id`, or a bare id (defaults to the configured provider).
        r#ref: String,
        /// Print the challenge as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Launch a challenge into a fresh disposable VM.
    #[command(visible_alias = "run")]
    Launch {
        /// `provider:id`, or a bare id (defaults to the configured provider).
        r#ref: String,
        /// Profile (VM image) to launch into; see `wargame profiles`
        /// [default: general.default_profile from config, initially macos-re-arm64].
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
        /// Guest networking [default: general.default_network from config, initially isolated].
        #[arg(long, value_enum, ignore_case = true)]
        network: Option<NetworkArg>,
        /// Override the profile's CPU count.
        #[arg(long, value_name = "N")]
        cpu: Option<u32>,
        /// Override the profile's memory, in GiB.
        #[arg(long = "memory-gib", value_name = "GIB")]
        memory_gib: Option<u64>,
        /// Copy challenge archives into the guest as-is instead of unpacking them there.
        #[arg(long = "no-extract")]
        no_extract: bool,
        /// Boot with a graphical console window instead of headless.
        #[arg(long)]
        gui: bool,
        /// Skip the interactive NAT confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// List local sessions.
    #[command(visible_aliases = ["ls", "ps"])]
    Sessions {
        /// Print the sessions as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Session subcommands.
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },
    /// SSH into a session's VM. Replaces this process.
    Ssh {
        /// Session id, a unique prefix/substring of it, or the challenge title
        /// [default: the most recent session].
        id: Option<String>,
    },
    /// Reopen a session's VM with a graphical console.
    Gui {
        /// Session id, a unique prefix/substring of it, or the challenge title
        /// [default: the most recent session].
        id: Option<String>,
    },
    /// Stop a session's VM (keeps its disk; `resume` brings it back).
    Stop {
        /// Session id, a unique prefix/substring of it, or the challenge title
        /// [default: the most recent session].
        id: Option<String>,
    },
    /// Resume a stopped session's VM.
    #[command(visible_alias = "start")]
    Resume {
        /// Session id, a unique prefix/substring of it, or the challenge title
        /// [default: the most recent session].
        id: Option<String>,
    },
    /// Mark a session to keep (kept sessions survive `prune` and need `delete --force`).
    Keep {
        /// Session id, a unique prefix/substring of it, or the challenge title
        /// [default: the most recent session].
        id: Option<String>,
        /// Turn keep off instead of on.
        #[arg(long)]
        off: bool,
    },
    /// Delete a session and its VM.
    #[command(visible_alias = "rm")]
    Delete {
        /// Session id, a unique prefix/substring of it, or the challenge title.
        id: String,
        /// Required to delete a `keep`-flagged session.
        #[arg(long, short = 'f')]
        force: bool,
    },
    /// Delete Tart VMs with no corresponding session record.
    Prune {
        /// Only list what would be deleted.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Diagnose the host environment.
    Doctor {
        /// Print the checks as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List available profiles.
    Profiles {
        /// Print the profiles as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Profile subcommands.
    Profile {
        #[command(subcommand)]
        action: ProfileCommand,
    },
    /// Dreamhack authentication.
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
    /// Print a shell completion script (e.g. `wargame completions zsh > ~/.zfunc/_wargame`).
    Completions {
        /// Shell to generate for.
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Show one session's full detail.
    #[command(visible_alias = "show")]
    Inspect {
        /// Session id, a unique prefix/substring of it, or the challenge title
        /// [default: the most recent session].
        id: Option<String>,
        /// Print the session as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Build (or rebuild) a profile's local Tart image. Slow (tens of minutes) the first time.
    Build {
        /// Profile name; see `wargame profiles` [default: general.default_profile from config].
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Log in with an email/password (the password is prompted, never stored).
    Login {
        /// Dreamhack account email (prompted if omitted).
        #[arg(long)]
        email: Option<String>,
    },
    /// Show the current authentication state.
    Status,
    /// Forget the stored session.
    Logout,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn aliases_parse_to_their_commands() {
        assert!(matches!(
            Cli::parse_from(["wargame", "ls"]).command,
            Some(Command::Sessions { .. })
        ));
        assert!(matches!(
            Cli::parse_from(["wargame", "rm", "x"]).command,
            Some(Command::Delete { .. })
        ));
        assert!(matches!(
            Cli::parse_from(["wargame", "show", "dreamhack:1"]).command,
            Some(Command::Open { .. })
        ));
        assert!(matches!(
            Cli::parse_from(["wargame", "ssh"]).command,
            Some(Command::Ssh { id: None })
        ));
    }

    #[test]
    fn network_flag_is_an_enum() {
        let cli = Cli::parse_from(["wargame", "launch", "dreamhack:1", "--network", "NAT"]);
        assert!(matches!(
            cli.command,
            Some(Command::Launch {
                network: Some(NetworkArg::Nat),
                ..
            })
        ));
        assert!(
            Cli::try_parse_from(["wargame", "launch", "dreamhack:1", "--network", "bogus"])
                .is_err()
        );
    }
}
