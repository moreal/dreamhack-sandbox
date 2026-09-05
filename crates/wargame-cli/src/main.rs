//! `wargame` binary: CLI + TUI composition root (DESIGN.md §7).

mod cli;
mod fixture_provider;
mod output;
mod profiles_builtin;
mod wiring;

use std::io::{IsTerminal, Write};

use clap::Parser;

use wargame_core::app::{
    delete, doctor, inspect, keep, launch, list_sessions, open, open_gui, prune, resume, search,
    ssh_command, stop, AppContext, LaunchEvent, LaunchRequest,
};
use wargame_core::domain::{ChallengeRef, NetworkMode};
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::{AuthState, SearchQuery, Secret};

use cli::{AuthCommand, Cli, Command, ProfileCommand, SessionCommand};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cli = Cli::parse();
    let code = run(cli).await;
    std::process::exit(code);
}

async fn run(cli: Cli) -> i32 {
    let Cli {
        verbose,
        config,
        home,
        command,
    } = cli;

    let Some(command) = command else {
        // Task 8 builds the TUI; until then this is the documented
        // placeholder (PLAN.md Task 7's note).
        println!("TUI not built yet");
        return 2;
    };

    let ctx = match wiring::build_context(verbose, config, home) {
        Ok(ctx) => ctx,
        Err(e) => {
            print_error(&e);
            return 1;
        }
    };

    match dispatch(&ctx, command).await {
        Ok(code) => code,
        Err(e) => {
            print_error(&e);
            1
        }
    }
}

fn print_error(err: &WargameError) {
    eprintln!("Error: {err}");
    if let Some(hint) = err.hint() {
        eprintln!("Try: {hint}");
    }
}

async fn dispatch(ctx: &AppContext, command: Command) -> Result<i32> {
    match command {
        Command::Search {
            query,
            category,
            page,
            json,
        } => {
            let result = search(
                ctx,
                SearchQuery {
                    text: query,
                    category,
                    page,
                    page_size: 20,
                },
            )
            .await?;
            output::print_search_page(&result, json);
            Ok(0)
        }

        Command::Open { r#ref, json } => {
            let reference = ChallengeRef::parse(&r#ref)?;
            let challenge = open(ctx, &reference).await?;
            output::print_challenge(&challenge, json);
            Ok(0)
        }

        Command::Launch {
            r#ref,
            profile,
            network,
            cpu,
            memory_gib,
            no_extract,
            gui,
            yes,
        } => {
            let challenge_ref = ChallengeRef::parse(&r#ref)?;
            let network_mode = match network {
                Some(n) => n.parse::<NetworkMode>()?,
                None => ctx.config.general.default_network,
            };

            if network_mode == NetworkMode::Nat && !confirm_nat_or_abort(yes) {
                return Ok(1);
            }

            let profile_name =
                profile.unwrap_or_else(|| ctx.config.general.default_profile.clone());
            let profile_obj = ctx.profiles.get(&profile_name)?;
            let mut resources = profile_obj.resources;
            let mut has_override = false;
            if let Some(cpu) = cpu {
                resources.cpu = cpu;
                has_override = true;
            }
            if let Some(memory_gib) = memory_gib {
                resources.memory_mib = memory_gib * 1024;
                has_override = true;
            }

            let req = LaunchRequest {
                challenge: challenge_ref,
                profile: profile_name,
                network: network_mode,
                resources: has_override.then_some(resources),
                extract: !no_extract,
                gui,
            };

            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let printer = tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    print_launch_event(&event);
                }
            });
            let outcome = launch(ctx, req, tx).await;
            let _ = printer.await;

            match outcome {
                Ok(session) => {
                    println!("session id: {}", session.id);
                    Ok(0)
                }
                Err(e) => {
                    print_error(&e);
                    Ok(1)
                }
            }
        }

        Command::Sessions { json } => {
            let sessions = list_sessions(ctx).await?;
            output::print_sessions(&sessions, json);
            Ok(0)
        }

        Command::Session {
            action: SessionCommand::Inspect { id, json },
        } => {
            let session = inspect(ctx, &id).await?;
            output::print_session(&session, json);
            Ok(0)
        }

        Command::Ssh { id } => {
            let cmd = ssh_command(ctx, &id).await?;
            exec_replacing_process(cmd)
        }

        Command::Gui { id } => {
            open_gui(ctx, &id).await?;
            println!("Opened a graphical console for {id}.");
            Ok(0)
        }

        Command::Stop { id } => {
            let session = stop(ctx, &id).await?;
            println!("Stopped {}.", session.id);
            Ok(0)
        }

        Command::Resume { id } => {
            let session = resume(ctx, &id).await?;
            println!("Resumed {}.", session.id);
            Ok(0)
        }

        Command::Keep { id, off } => {
            let session = keep(ctx, &id, !off).await?;
            println!("{}: keep = {:?}", session.id, session.keep);
            Ok(0)
        }

        Command::Delete { id, force } => {
            delete(ctx, &id, force).await?;
            println!("Deleted {id}.");
            Ok(0)
        }

        Command::Prune { dry_run } => {
            let orphans = prune(ctx, dry_run).await?;
            if orphans.is_empty() {
                println!("No orphaned VMs found.");
            }
            for name in &orphans {
                if dry_run {
                    println!("would delete {name}");
                } else {
                    println!("deleted {name}");
                }
            }
            Ok(0)
        }

        Command::Doctor { json } => {
            let checks = doctor(ctx).await;
            output::print_checks(&checks, json);
            Ok(0)
        }

        Command::Profiles { json } => {
            output::print_profiles(&ctx.profiles.list(), json);
            Ok(0)
        }

        Command::Profile {
            action: ProfileCommand::Build { name },
        } => {
            let profile = ctx.profiles.get(&name)?;
            let (builder, opts) = wiring::build_profile_image_builder(ctx).await?;
            let image = builder
                .build_profile(&profile, &opts, &mut |msg| println!("… {msg}"))
                .await?;
            println!("✓ built {}", image.0);
            Ok(0)
        }

        Command::Auth { action } => dispatch_auth(ctx, action).await,
    }
}

async fn dispatch_auth(ctx: &AppContext, action: AuthCommand) -> Result<i32> {
    match action {
        AuthCommand::Login { email } => {
            let email = match email {
                Some(e) => e,
                None => prompt_line("Email: ")?,
            };
            let password = rpassword::prompt_password("Password: ")
                .map_err(|e| WargameError::Other(e.to_string()))?;
            ctx.wargame
                .login(&email, Secret::new(Box::new(password)))
                .await?;
            println!("Logged in as {email}.");
            Ok(0)
        }
        AuthCommand::Status => {
            match ctx.wargame.auth_state().await? {
                AuthState::Anonymous => println!("anonymous"),
                AuthState::Authenticated { display } => println!("authenticated as {display}"),
            }
            Ok(0)
        }
        AuthCommand::Logout => {
            let store = wiring::build_credential_store(&ctx.config, &ctx.dirs, &ctx.runner);
            store
                .delete(wargame_dreamhack::CREDENTIAL_KEY)
                .map_err(|e| WargameError::CredentialStore(e.to_string()))?;
            println!("Logged out.");
            Ok(0)
        }
    }
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout()
        .flush()
        .map_err(|e| WargameError::Other(e.to_string()))?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| WargameError::Other(e.to_string()))?;
    Ok(line.trim().to_string())
}

/// NAT confirmation (MVP §18): without `--yes`, warns and asks
/// `Continue? [y/N]` on the TTY; with no TTY to answer on, refuses rather
/// than hanging or silently proceeding.
fn confirm_nat_or_abort(yes: bool) -> bool {
    if yes {
        return true;
    }
    println!(
        "Warning: NAT networking connects this VM directly to your network, \
         rather than isolating it (MVP.md §18)."
    );
    print!("Continue? [y/N] ");
    let _ = std::io::stdout().flush();

    if !std::io::stdin().is_terminal() {
        return false;
    }
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn print_launch_event(event: &LaunchEvent) {
    match event {
        LaunchEvent::Preflight(msg) => println!("… {msg}"),
        LaunchEvent::SessionCreated { id } => println!("✓ session created: {id}"),
        LaunchEvent::VmCreated { name } => println!("✓ VM created: {name}"),
        LaunchEvent::Downloading { filename } => println!("… downloading {filename}"),
        LaunchEvent::Downloaded {
            filename,
            sha256,
            size,
        } => println!("✓ downloaded {filename} ({size} bytes, sha256 {sha256})"),
        LaunchEvent::Booting => println!("… booting"),
        LaunchEvent::GuestIp(ip) => println!("✓ guest ip: {ip}"),
        LaunchEvent::Bootstrapping => println!("… bootstrapping"),
        LaunchEvent::Ready {
            session_id,
            ssh_hint,
        } => println!("✓ Ready: session {session_id} ({ssh_hint})"),
        LaunchEvent::Failed {
            session_id,
            error,
            hint,
        } => {
            println!("✗ Failed: {error}");
            if let Some(id) = session_id {
                println!("  session: {id}");
            }
            if let Some(hint) = hint {
                println!("  Try: {hint}");
            }
        }
    }
}

#[cfg(unix)]
fn exec_replacing_process(mut cmd: std::process::Command) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    // `exec` only returns on failure -- on success this process image is
    // gone (DESIGN.md §7's "exec replaces process").
    let err = cmd.exec();
    Err(WargameError::Other(format!("failed to exec ssh: {err}")))
}

#[cfg(not(unix))]
fn exec_replacing_process(mut cmd: std::process::Command) -> Result<i32> {
    let status = cmd
        .status()
        .map_err(|e| WargameError::Other(e.to_string()))?;
    Ok(status.code().unwrap_or(1))
}
