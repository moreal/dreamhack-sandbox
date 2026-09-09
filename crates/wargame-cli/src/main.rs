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
    build_profile_image, create_profile_vm, delete, doctor, doctor_profile, init_profile, inspect,
    keep, launch, list_sessions, open, open_gui, open_profile_console, profile_overviews,
    profile_vm, prune, resolve_session, resume, search, ssh_command, stop, AppContext,
    CreateSource, LaunchEvent, LaunchRequest, UnmanagedVm,
};
use wargame_core::domain::{ChallengeRef, KeepPolicy, NetworkMode, SessionState};
use wargame_core::error::{Result, WargameError};
use wargame_core::ports::image_builder::EjectOutcome;
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
            let report = prune(ctx, dry_run).await?;
            if report.is_empty() {
                println!("No orphaned VMs found.");
                return Ok(0);
            }
            for name in &report.orphans {
                if dry_run {
                    println!("would delete {name}");
                } else {
                    println!("deleted {name}");
                }
            }
            // `prune` never stops a guest, so a running leftover is reported
            // with the one command that does finish it off. `delete`
            // continues an interrupted delete, so this works from the state
            // that produced the leftover.
            for leftover in &report.running {
                println!(
                    "still running, left alone: {} — remove it with: wargame delete {}",
                    leftover.vm, leftover.session
                );
            }
            // Never deleted, but never hidden either: the user still gets to
            // learn these VMs exist, and what to do if one really is theirs
            // to remove (spec.md §3 FR-5).
            if !report.unmanaged.is_empty() {
                println!(
                    "{} VM(s) carry this app's name prefix but no record here owns them — another wargame home's, or your own — so they were left alone:",
                    report.unmanaged.len()
                );
                for vm in &report.unmanaged {
                    println!("  {} — {}", vm.vm, manual_removal_hint(vm));
                }
            }
            if !report.orphans.is_empty() {
                if dry_run {
                    println!(
                        "{} orphaned VM(s). Run `wargame prune` to delete them.",
                        report.orphans.len()
                    );
                } else {
                    println!("Deleted {} orphaned VM(s).", report.orphans.len());
                }
            } else {
                println!("No orphaned VMs to delete.");
            }
            Ok(0)
        }

        Command::Doctor { json, profile } => {
            let checks = match &profile {
                Some(name) => doctor_profile(ctx, name).await,
                None => doctor(ctx).await,
            };
            output::print_checks(&checks, json);
            Ok(0)
        }

        Command::Profiles { json } => {
            // The same overview the TUI's profile picker renders, so the two
            // cannot disagree about what a profile resolves to or whether it
            // has an image (spec.md §3 FR-1).
            let overviews = profile_overviews(ctx).await;
            output::print_profiles(&overviews, &ctx.config.general.default_profile, json);
            Ok(0)
        }

        Command::Profile { action } => dispatch_profile(ctx, action).await,

        Command::Auth { action } => dispatch_auth(ctx, action).await,

        Command::Completions { .. } => unreachable!("handled before the context is built"),
    }
}

/// What to run to remove a VM `wargame` will not touch, on the backend it
/// actually lives on. `prune` deliberately never deletes these, so saying
/// "ignored" without saying this would leave the user stuck.
fn manual_removal_hint(vm: &UnmanagedVm) -> String {
    match vm.backend.as_str() {
        "tart" => format!("if you want it gone: tart delete {}", vm.name),
        "utm" => format!(
            "if you want it gone: delete {:?} in the UTM app (right-click the VM ▸ Delete)",
            vm.name
        ),
        other => format!("if you want it gone, remove it with {other}'s own tools"),
    }
}

/// Generates the host's guest SSH key if it is missing, warning rather than
/// failing when that is not possible (no `ssh-keygen` on `PATH`, say) --
/// every command that calls this has real work to report first.
async fn ensure_host_key_or_warn(ctx: &AppContext) {
    if let Err(e) = wiring::ensure_host_key(ctx).await {
        eprintln!(
            "Warning: could not prepare the host's SSH key: {e}\nGenerate it yourself with:\n  ssh-keygen -t ed25519 -N \"\" -f {}",
            ctx.dirs.keys_dir().join("id_ed25519").display()
        );
    }
}

/// `wargame profile init|create|console|build` (spec.md §4).
async fn dispatch_profile(ctx: &AppContext, action: ProfileCommand) -> Result<i32> {
    match action {
        ProfileCommand::Init { template, name } => {
            let outcome = init_profile(ctx, &template, &name).await?;
            println!("✓ created profile {name} from {template}:");
            println!("  {}", outcome.yaml_path.display());
            for script in &outcome.scripts {
                println!("  {}", script.display());
            }
            println!("Edit those, then:");
            if outcome.profile.image.base.is_empty() {
                println!(
                    "  wargame profile create {name} --iso <path>   # or --from-vm <utm-uuid>"
                );
            } else {
                println!("  wargame profile build {name}");
            }
            Ok(0)
        }

        ProfileCommand::Create {
            name,
            iso,
            from_vm,
            sha256,
        } => {
            // The guest is told to authorize this key, so it should exist
            // before the instructions point at it -- but a host without
            // `ssh-keygen` must still get the real answer about its profile
            // and its ISO, so this never fails the command.
            ensure_host_key_or_warn(ctx).await;
            let source = match (iso, from_vm) {
                (Some(path), None) => CreateSource::Iso { path, sha256 },
                (None, Some(uuid)) => CreateSource::FromVm { uuid },
                // clap's argument group already made this unreachable.
                _ => {
                    return Err(WargameError::Other(
                        "pass exactly one of --iso or --from-vm".to_string(),
                    ))
                }
            };
            let outcome = create_profile_vm(ctx, &name, source).await?;
            if outcome.reused {
                println!("Installation VM for {name} already exists.");
            } else {
                println!("✓ installation VM for {name} created.");
            }
            for line in &outcome.guidance {
                println!("  {line}");
            }
            Ok(0)
        }

        ProfileCommand::Console { name } => {
            println!("Starting the installation VM for {name} and opening its window…");
            let opened = open_profile_console(ctx, &name).await?;
            println!("✓ {} ({}) is open.", opened.vm.name, opened.vm.id);
            for line in &opened.guidance {
                println!("  {line}");
            }
            Ok(0)
        }

        ProfileCommand::Eject { name } => {
            let report = profile_vm::eject(ctx, &name, &mut |msg| println!("… {msg}")).await?;
            match report.outcome {
                EjectOutcome::Detached(n) => println!(
                    "✓ took the installer disc out of {} ({n} removable drive{}). It now boots from its own disk.",
                    report.vm.name,
                    if n == 1 { "" } else { "s" }
                ),
                EjectOutcome::NothingAttached => println!(
                    "{} has no installer disc attached; nothing to do.",
                    report.vm.name
                ),
                // `profile eject` refuses a backend with no installation
                // media before it gets here, and a blocked eject is an
                // error -- neither can reach this arm.
                EjectOutcome::NotApplicable | EjectOutcome::Blocked(_) => {
                    println!("{} has no installer disc attached.", report.vm.name)
                }
            }
            println!("Next: wargame profile build {name}");
            Ok(0)
        }

        ProfileCommand::Stop { name } => {
            let report = profile_vm::stop(ctx, &name).await?;
            if report.was_running {
                println!("✓ stopped {} ({}).", report.vm.name, report.vm.id);
            } else {
                println!(
                    "{} ({}) is already {}; nothing to stop.",
                    report.vm.name,
                    report.vm.id,
                    status_word(report.status)
                );
            }
            Ok(0)
        }

        ProfileCommand::Delete { name, force } => {
            let plan = profile_vm::plan_delete(ctx, &name).await?;
            if plan.vms.is_empty() {
                println!("Profile {name} owns no VMs; removing its build record.");
            } else {
                println!("This deletes, permanently:");
                for owned in &plan.vms {
                    println!("  {} [{}]", owned.describe(), status_word(owned.status));
                }
                println!(
                    "  and the build record of {name}. The OS installed in that VM goes with it."
                );
            }
            if !confirm_or_abort(force, &format!("Delete the VMs of {name}?")) {
                println!("Nothing was deleted.");
                return Ok(1);
            }
            let report = profile_vm::delete(ctx, &name).await?;
            for owned in &report.deleted {
                println!("deleted {}", owned.describe());
            }
            for owned in &report.already_gone {
                println!("{} was already gone", owned.describe());
            }
            println!(
                "✓ profile {name} has no VMs left. Start again with: wargame profile create {name} --iso <path>"
            );
            Ok(0)
        }

        ProfileCommand::Build { name } => {
            let name = name.unwrap_or_else(|| ctx.config.general.default_profile.clone());
            // Both backends want the host key: Tart installs it into the
            // guest, UTM expects the user to have authorized it. A failure
            // here is reported, not fatal: the build itself says precisely
            // what it could not read when it gets there.
            ensure_host_key_or_warn(ctx).await;
            println!(
                "Building the image for profile {name} (this takes tens of minutes the first time; NAT networking is used for provisioning)…"
            );
            let started = Instant::now();
            let record = build_profile_image(ctx, &name, &mut |msg| println!("… {msg}")).await?;
            let image = record
                .ready_image()
                .map(|i| i.name.clone())
                .unwrap_or_else(|| name.clone());
            println!(
                "✓ built {image} in {}s. Next: wargame launch <provider:id> --profile {name}",
                started.elapsed().as_secs()
            );
            for failure in &record.cleanup_failures {
                println!("! {failure}");
            }
            Ok(0)
        }
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

/// The word for a VM's state in a message.
fn status_word(status: wargame_core::ports::VmStatus) -> &'static str {
    match status {
        wargame_core::ports::VmStatus::Running => "running",
        wargame_core::ports::VmStatus::Stopped => "stopped",
        wargame_core::ports::VmStatus::Missing => "already gone",
    }
}

/// `question [y/N]` on the TTY, skipped by `--force`. With no TTY to answer
/// on it refuses, rather than hanging or destroying something unasked.
fn confirm_or_abort(force: bool, question: &str) -> bool {
    if force {
        return true;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    if !std::io::stdin().is_terminal() {
        println!("\nNot a terminal: re-run with --force if that is what you meant.");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unmanaged(backend: &str, name: &str) -> UnmanagedVm {
        UnmanagedVm {
            vm: format!("{backend}/{name}"),
            backend: backend.to_string(),
            name: name.to_string(),
        }
    }

    /// `prune` never deletes a VM no record here owns, so the row that says
    /// so has to name the way out on the backend it actually lives on.
    #[test]
    fn an_ignored_vm_names_the_manual_remedy_for_its_backend() {
        let tart = manual_removal_hint(&unmanaged("tart", "wg-other"));
        assert!(tart.contains("tart delete wg-other"), "{tart}");

        let utm = manual_removal_hint(&unmanaged("utm", "wg-build-kali"));
        assert!(utm.contains("UTM app"), "{utm}");
        assert!(utm.contains("wg-build-kali"), "{utm}");

        // An unknown backend still says something actionable rather than
        // nothing at all.
        let other = manual_removal_hint(&unmanaged("qemu", "wg-x"));
        assert!(other.contains("qemu"), "{other}");
    }
}
