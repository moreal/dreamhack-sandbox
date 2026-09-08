//! `wargame` binary: CLI + TUI composition root (DESIGN.md §7).

mod cli;
mod fixture_provider;
mod output;
mod profiles_builtin;
mod tui;
mod wiring;

use std::io::{IsTerminal, Write};
use std::time::Instant;

use clap::{CommandFactory, Parser};

use wargame_core::app::{
    delete, doctor, inspect, keep, launch, list_sessions, open, open_gui, prune, resolve_session,
    resume, search, ssh_command, stop, AppContext, LaunchEvent, LaunchRequest,
};
use wargame_core::domain::{ChallengeRef, KeepPolicy, NetworkMode, SessionState};
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

    if let Some(Command::Completions { shell }) = command {
        // Needs no context (and must work before any config/home exists).
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "wargame",
            &mut std::io::stdout(),
        );
        return 0;
    }

    let ctx = match wiring::build_context(verbose, config, home) {
        Ok(ctx) => ctx,
        Err(e) => {
            print_error(&e);
            return 1;
        }
    };

    let Some(command) = command else {
        // No subcommand -> the TUI (PLAN.md Task 8), over the same
        // AppContext and application services every subcommand below uses.
        return match tui::run(ctx).await {
            Ok(code) => code,
            Err(e) => {
                print_error(&e);
                1
            }
        };
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

/// Resolves a (possibly shortened or omitted) session reference, telling
/// the user on stderr which session was picked whenever it is not
/// literally what they typed.
async fn resolve(ctx: &AppContext, needle: Option<&str>) -> Result<String> {
    let id = resolve_session(ctx, needle).await?;
    if needle != Some(id.as_str()) {
        eprintln!("(session {id})");
    }
    Ok(id)
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
                Some(n) => NetworkMode::from(n),
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

            println!(
                "Launching {} into profile {} ({} network)…",
                req.challenge, req.profile, req.network
            );
            let started = Instant::now();
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
                    let id = &session.id;
                    println!();
                    println!("Ready in {}s: {id}", started.elapsed().as_secs());
                    println!("Next:");
                    println!("  wargame ssh {id}      # open a shell in the VM");
                    println!("  wargame gui {id}      # open a graphical console");
                    println!("  wargame stop {id}     # pause it (resume later)");
                    println!("  wargame delete {id}   # discard it");
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
            let id = resolve(ctx, id.as_deref()).await?;
            let session = inspect(ctx, &id).await?;
            let log = ctx.repo.session_dir(&id).join("run.log");
            output::print_session(&session, Some(&log), json);
            Ok(0)
        }

        Command::Ssh { id } => {
            let id = resolve(ctx, id.as_deref()).await?;
            let cmd = ssh_command(ctx, &id).await?;
            exec_replacing_process(cmd)
        }

        Command::Gui { id } => {
            let id = resolve(ctx, id.as_deref()).await?;
            println!("Opening a graphical console for {id}… (SSH keeps working; closing the window stops the VM)");
            open_gui(ctx, &id).await?;
            println!("Opened. Next: wargame ssh {id}   # or: wargame stop {id}");
            Ok(0)
        }

        Command::Stop { id } => {
            let id = resolve(ctx, id.as_deref()).await?;
            let current = inspect(ctx, &id).await?;
            if current.state == SessionState::Stopped {
                println!("{id} is already stopped. Next: wargame resume {id}");
                return Ok(0);
            }
            let session = stop(ctx, &id).await?;
            println!(
                "Stopped {}. Next: wargame resume {0}   # or: wargame delete {0}",
                session.id
            );
            Ok(0)
        }

        Command::Resume { id } => {
            let id = resolve(ctx, id.as_deref()).await?;
            let current = inspect(ctx, &id).await?;
            if current.state.is_up() {
                println!("{id} is already running. Next: wargame ssh {id}");
                return Ok(0);
            }
            println!("Resuming {id}… (waiting for the guest to boot)");
            let session = resume(ctx, &id).await?;
            println!("Resumed {}. Next: wargame ssh {0}", session.id);
            Ok(0)
        }

        Command::Keep { id, off } => {
            let id = resolve(ctx, id.as_deref()).await?;
            let session = keep(ctx, &id, !off).await?;
            match session.keep {
                KeepPolicy::Keep => println!(
                    "{id} is now kept: it survives `wargame prune` and needs `wargame delete {id} --force`.\nUndo: wargame keep {id} --off"
                ),
                KeepPolicy::Discard => println!("{id} is no longer kept; `wargame delete {id}` works again."),
            }
            Ok(0)
        }

        Command::Delete { id, force } => {
            let id = resolve(ctx, Some(&id)).await?;
            delete(ctx, &id, force).await?;
            println!("Deleted {id}.");
            Ok(0)
        }

        Command::Prune { dry_run } => {
            let orphans = prune(ctx, dry_run).await?;
            if orphans.is_empty() {
                println!("No orphaned VMs found.");
                return Ok(0);
            }
            for name in &orphans {
                if dry_run {
                    println!("would delete {name}");
                } else {
                    println!("deleted {name}");
                }
            }
            if dry_run {
                println!(
                    "{} orphaned VM(s). Run `wargame prune` to delete them.",
                    orphans.len()
                );
            } else {
                println!("Deleted {} orphaned VM(s).", orphans.len());
            }
            Ok(0)
        }

        Command::Doctor { json } => {
            let checks = doctor(ctx).await;
            output::print_checks(&checks, json);
            Ok(0)
        }

        Command::Profiles { json } => {
            output::print_profiles(
                &ctx.profiles.list(),
                &ctx.config.general.default_profile,
                json,
            );
            Ok(0)
        }

        Command::Profile {
            action: ProfileCommand::Build { name },
        } => {
            let name = name.unwrap_or_else(|| ctx.config.general.default_profile.clone());
            let profile = ctx.profiles.get(&name)?;
            println!(
                "Building image for profile {name} (this takes tens of minutes the first time; NAT networking is used for provisioning)…"
            );
            let started = Instant::now();
            let (builder, opts) = wiring::build_profile_image_builder(ctx).await?;
            let image = builder
                .build_profile(&profile, &opts, &mut |msg| println!("… {msg}"))
                .await?;
            println!(
                "✓ built {} in {}s. Next: wargame launch <provider:id> --profile {name}",
                image.0,
                started.elapsed().as_secs()
            );
            Ok(0)
        }

        Command::Auth { action } => dispatch_auth(ctx, action).await,

        Command::Completions { .. } => unreachable!("handled before the context is built"),
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
            println!("Logged in as {email}. Next: wargame search <query>");
            Ok(0)
        }
        AuthCommand::Status => {
            match ctx.wargame.auth_state().await? {
                AuthState::Anonymous => {
                    println!("Not logged in. Searching works anonymously; downloading challenge files needs `wargame auth login`.")
                }
                AuthState::Authenticated { display } => {
                    println!("Logged in ({display}). Next: wargame search <query>")
                }
            }
            Ok(0)
        }
        AuthCommand::Logout => {
            let store = wiring::build_credential_store(&ctx.config, &ctx.dirs, &ctx.runner);
            store
                .delete(wargame_dreamhack::CREDENTIAL_KEY)
                .map_err(|e| WargameError::CredentialStore(e.to_string()))?;
            println!("Logged out; the stored session was removed from the credential store.");
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

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / KIB / KIB)
    } else {
        format!("{:.2} GiB", b / KIB / KIB / KIB)
    }
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
        } => println!(
            "✓ downloaded {filename} ({}, sha256 {}…)",
            human_size(*size),
            &sha256[..sha256.len().min(12)]
        ),
        LaunchEvent::Booting => println!("… booting the VM (up to a few minutes on first boot)"),
        LaunchEvent::GuestIp(ip) => println!("✓ guest ip: {ip}"),
        LaunchEvent::Bootstrapping => {
            println!("… copying files into the guest and unpacking them there")
        }
        LaunchEvent::Ready { .. } => {
            // The summary block printed by `launch` covers this.
        }
        LaunchEvent::Failed {
            session_id,
            error,
            hint,
        } => {
            println!("✗ Failed: {error}");
            if let Some(id) = session_id {
                println!("  session {id} was left in place; `wargame session inspect {id}` shows the details, `wargame delete {id}` discards it.");
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
