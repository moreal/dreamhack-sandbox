//! The `wargame` TUI (MVP.md §6, DESIGN.md §7): an Elm-style loop over the
//! exact same [`wargame_core::app`] services the CLI calls. `app.rs` holds
//! the pure reducer (unit-tested without a terminal); this module owns the
//! terminal lifecycle, the crossterm key-reader thread, and dispatching
//! each [`app::Effect`] to an async application-service call.

mod app;
mod launch;
mod problem;
mod search;
mod sessions;
mod widgets;

use std::io::{self, IsTerminal, Stdout};
use std::sync::Arc;

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use wargame_core::app::{
    delete, launch as launch_challenge, list_sessions, open, open_gui, resume,
    search as search_challenges, ssh_command, stop, AppContext, LaunchEvent,
};
use wargame_core::error::{Result, WargameError};

use app::{App, Effect, Msg, Screen};

type Backend = CrosstermBackend<Stdout>;

/// Runs the TUI to completion (until `q`/`Ctrl+C`-equivalent quits it),
/// restoring the terminal on every exit path -- including a panic that
/// unwinds through here, since `restore_terminal` also runs from the
/// ordinary `?`/return paths below. Returns the process exit code (`0`).
pub async fn run(ctx: AppContext) -> Result<i32> {
    // Entering raw mode / the alternate screen on a non-tty stdin or stdout
    // (piped output, CI, `wargame < /dev/null`) is not something crossterm
    // can do meaningfully; fail fast with a clear message instead of a raw
    // ioctl error (PLAN.md Task 8's note: reducer tests cover the logic
    // when no pty is available to smoke-run against).
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        println!("wargame's TUI needs an interactive terminal. Try `wargame --help` for the CLI.");
        return Ok(2);
    }

    let default_profile = ctx.config.general.default_profile.clone();
    let default_network = ctx.config.general.default_network;
    let available_profiles: Vec<String> = ctx.profiles.list().into_iter().map(|p| p.name).collect();
    let ctx = Arc::new(ctx);

    let mut terminal = setup_terminal()?;
    let mut app = App::new(default_profile, default_network, available_profiles);

    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    spawn_key_reader(tx.clone());

    let outcome = event_loop(&mut terminal, &mut app, &ctx, &tx, &mut rx).await;

    // Always try to leave the terminal usable, even if the loop errored.
    let restore = restore_terminal(&mut terminal);
    outcome?;
    restore?;
    Ok(0)
}

async fn event_loop(
    terminal: &mut Terminal<Backend>,
    app: &mut App,
    ctx: &Arc<AppContext>,
    tx: &mpsc::UnboundedSender<Msg>,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
) -> Result<()> {
    loop {
        terminal
            .draw(|frame| draw(frame, app))
            .map_err(WargameError::Io)?;
        let Some(msg) = rx.recv().await else {
            return Ok(());
        };
        for effect in app.update(msg) {
            match effect {
                Effect::Quit => return Ok(()),
                Effect::ExecSsh(id) => run_ssh(terminal, ctx, &id, tx).await,
                other => spawn_effect(ctx.clone(), other, tx.clone()),
            }
        }
    }
}

/// Reads crossterm key events on a dedicated OS thread (crossterm's
/// `event::read` blocks) and forwards them into the same channel the async
/// application-service calls report back on. The thread exits once nothing
/// is listening any more (the app has quit).
fn spawn_key_reader(tx: mpsc::UnboundedSender<Msg>) {
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(key)) => {
                if tx.send(Msg::Key(key)).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(_) => return,
        }
    });
}

/// Dispatches every [`Effect`] but [`Effect::Quit`] (handled by the loop)
/// and [`Effect::ExecSsh`] (needs the terminal, handled by [`run_ssh`]) to
/// its application-service call on a spawned task, reporting the result
/// back through `tx` as a [`Msg`].
fn spawn_effect(ctx: Arc<AppContext>, effect: Effect, tx: mpsc::UnboundedSender<Msg>) {
    match effect {
        Effect::Search(query) => {
            tokio::spawn(async move {
                match search_challenges(&ctx, query).await {
                    Ok(page) => {
                        let _ = tx.send(Msg::SearchResults(page));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e.to_string()));
                    }
                }
            });
        }
        Effect::Open(r) => {
            tokio::spawn(async move {
                match open(&ctx, &r).await {
                    Ok(challenge) => {
                        let _ = tx.send(Msg::Challenge(challenge));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e.to_string()));
                    }
                }
            });
        }
        Effect::Launch(req) => {
            tokio::spawn(async move {
                let (etx, mut erx) = tokio::sync::mpsc::channel::<LaunchEvent>(64);
                let forward_tx = tx.clone();
                let forward = tokio::spawn(async move {
                    while let Some(event) = erx.recv().await {
                        if forward_tx.send(Msg::Launch(event)).is_err() {
                            break;
                        }
                    }
                });
                if let Err(e) = launch_challenge(&ctx, req, etx).await {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
                let _ = forward.await;
            });
        }
        Effect::LoadSessions => {
            tokio::spawn(async move { refresh_sessions(&ctx, &tx).await });
        }
        Effect::Stop(id) => {
            tokio::spawn(async move {
                match stop(&ctx, &id).await {
                    Ok(_) => refresh_sessions(&ctx, &tx).await,
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e.to_string()));
                    }
                }
            });
        }
        Effect::Resume(id) => {
            tokio::spawn(async move {
                match resume(&ctx, &id).await {
                    Ok(_) => refresh_sessions(&ctx, &tx).await,
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e.to_string()));
                    }
                }
            });
        }
        Effect::Delete(id) => {
            tokio::spawn(async move {
                // The Sessions screen's own `y`/`N` prompt is the deliberate
                // action NFR-07 asks for, so this always forces past a
                // `Keep`-policy session.
                match delete(&ctx, &id, true).await {
                    Ok(()) => refresh_sessions(&ctx, &tx).await,
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e.to_string()));
                    }
                }
            });
        }
        Effect::OpenGui(id) => {
            tokio::spawn(async move {
                if let Err(e) = open_gui(&ctx, &id).await {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
            });
        }
        Effect::Quit | Effect::ExecSsh(_) => {
            // Handled by the caller before `spawn_effect` is reached.
        }
    }
}

async fn refresh_sessions(ctx: &AppContext, tx: &mpsc::UnboundedSender<Msg>) {
    match list_sessions(ctx).await {
        Ok(sessions) => {
            let _ = tx.send(Msg::Sessions(sessions));
        }
        Err(e) => {
            let _ = tx.send(Msg::Error(e.to_string()));
        }
    }
}

/// Runs `wargame ssh <id>` interactively (DESIGN.md §7): leaves the
/// alternate screen so the guest's shell draws directly to the real
/// terminal, waits for it to exit, then re-enters and forces a full
/// redraw.
async fn run_ssh(
    terminal: &mut Terminal<Backend>,
    ctx: &AppContext,
    id: &str,
    tx: &mpsc::UnboundedSender<Msg>,
) {
    let cmd = match ssh_command(ctx, id).await {
        Ok(cmd) => cmd,
        Err(e) => {
            let _ = tx.send(Msg::Error(e.to_string()));
            return;
        }
    };
    if let Err(e) = leave_tui_mode() {
        let _ = tx.send(Msg::Error(format!("failed to suspend TUI: {e}")));
        return;
    }
    let mut cmd = cmd;
    let status = tokio::task::spawn_blocking(move || cmd.status()).await;
    if let Err(e) = enter_tui_mode() {
        let _ = tx.send(Msg::Error(format!("failed to resume TUI: {e}")));
    }
    let _ = terminal.clear();
    match status {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            let _ = tx.send(Msg::Error(format!("ssh failed: {e}")));
        }
        Err(e) => {
            let _ = tx.send(Msg::Error(format!("ssh task failed: {e}")));
        }
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());

    match app.screen {
        Screen::Search => search::draw(frame, chunks[0], &app.search),
        Screen::Problem => {
            if let Some(problem) = &app.problem {
                problem::draw(frame, chunks[0], problem);
            }
        }
        Screen::Launching => {
            if let Some(launch_state) = &app.launch {
                launch::draw(frame, chunks[0], launch_state);
            }
        }
        Screen::Sessions => sessions::draw(frame, chunks[0], app),
    }
    widgets::status_bar(frame, chunks[1], &app.status, app.help_text());
}

fn enter_tui_mode() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)
}

fn leave_tui_mode() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)
}

fn setup_terminal() -> Result<Terminal<Backend>> {
    enter_tui_mode().map_err(WargameError::Io)?;
    Terminal::new(CrosstermBackend::new(io::stdout())).map_err(WargameError::Io)
}

fn restore_terminal(terminal: &mut Terminal<Backend>) -> Result<()> {
    leave_tui_mode().map_err(WargameError::Io)?;
    terminal.show_cursor().map_err(WargameError::Io)
}
