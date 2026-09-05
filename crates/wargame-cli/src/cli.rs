//! `wargame` CLI surface (DESIGN.md §7): every subcommand's exact shape.
//! `main.rs` owns dispatch; this module only owns parsing.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "wargame",
    version,
    about = "Search a wargame challenge and launch it into a disposable VM."
)]
pub struct Cli {
    /// Verbose (debug-level) logging.
    #[arg(long, global = true)]
    pub verbose: bool,
    /// Path to config.toml (default: $WARGAME_HOME/config/config.toml).
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Overrides `$WARGAME_HOME`.
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search challenges on the configured provider.
    Search {
        query: String,
        #[arg(long)]
        category: Option<String>,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long)]
        json: bool,
    },
    /// Look up one challenge's detail.
    Open {
        /// `provider:id`, or a bare id (defaults to the configured provider).
        r#ref: String,
        #[arg(long)]
        json: bool,
    },
    /// Launch a challenge into a fresh disposable VM.
    Launch {
        /// `provider:id`, or a bare id (defaults to the configured provider).
        r#ref: String,
        #[arg(long)]
        profile: Option<String>,
        /// `isolated` (default) or `nat`.
        #[arg(long)]
        network: Option<String>,
        #[arg(long)]
        cpu: Option<u32>,
        #[arg(long = "memory-gib")]
        memory_gib: Option<u64>,
        #[arg(long = "no-extract")]
        no_extract: bool,
        #[arg(long)]
        gui: bool,
        /// Skip the interactive NAT confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// List local sessions.
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Session subcommands.
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },
    /// SSH into a session's VM. Replaces this process.
    Ssh { id: String },
    /// Reopen a session's VM with a graphical console.
    Gui { id: String },
    /// Stop a session's VM.
    Stop { id: String },
    /// Resume a stopped session's VM.
    Resume { id: String },
    /// Flip a session's keep policy (kept sessions survive `prune`/casual delete).
    Keep {
        id: String,
        /// Turn keep off instead of on.
        #[arg(long)]
        off: bool,
    },
    /// Delete a session and its VM.
    Delete {
        id: String,
        /// Required to delete a `keep`-flagged session.
        #[arg(long)]
        force: bool,
    },
    /// Delete Tart VMs with no corresponding session record.
    Prune {
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Diagnose the host environment.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// List available profiles.
    Profiles {
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
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Show one session's full detail.
    Inspect {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Build (or rebuild) a profile's local Tart image.
    Build { name: String },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Log in with an email/password.
    Login {
        #[arg(long)]
        email: Option<String>,
    },
    /// Show the current authentication state.
    Status,
    /// Forget the stored session.
    Logout,
}
