//! Launch progress screen (DESIGN.md §7's `tui/launch.rs`): renders the
//! `LaunchEvent` log the launch pipeline (DESIGN.md §3.4) emits, and once
//! it reaches `Ready` offers `[s] ssh`, `[g] gui`, `[b] sessions`.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use wargame_core::app::LaunchEvent;

#[derive(Debug, Clone, Default)]
pub struct LaunchState {
    pub events: Vec<LaunchEvent>,
    pub ready: bool,
    pub failed: bool,
    pub session_id: Option<String>,
}

pub fn draw(frame: &mut Frame, area: Rect, state: &LaunchState) {
    let mut lines: Vec<Line<'static>> = state.events.iter().map(event_line).collect();
    let (title, color) = if state.failed {
        (
            "Launch failed — [b] sessions to inspect or delete it",
            Color::Red,
        )
    } else if state.ready {
        ("Ready — [s] ssh  [g] gui  [b] sessions", Color::Green)
    } else {
        lines.push(Line::from("…").style(Style::default().add_modifier(Modifier::DIM)));
        ("Launching…", Color::Yellow)
    };
    // Keep the newest lines in view once the log outgrows the box.
    let inner_height = area.height.saturating_sub(2) as usize;
    let scroll = lines.len().saturating_sub(inner_height) as u16;
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(color)),
        );
    frame.render_widget(paragraph, area);
}

fn ok(text: String) -> Line<'static> {
    Line::from(format!("✓ {text}")).style(Style::default().fg(Color::Green))
}

fn pending(text: String) -> Line<'static> {
    Line::from(format!("… {text}"))
}

fn event_line(event: &LaunchEvent) -> Line<'static> {
    match event {
        LaunchEvent::Preflight(msg) => pending(msg.clone()),
        LaunchEvent::SessionCreated { id } => ok(format!("session created: {id}")),
        LaunchEvent::VmCreated { name } => ok(format!("VM created: {name}")),
        LaunchEvent::Downloading { filename } => pending(format!("downloading {filename}")),
        LaunchEvent::Downloaded {
            filename,
            sha256,
            size,
        } => ok(format!(
            "downloaded {filename} ({size} bytes, sha256 {}…)",
            &sha256[..sha256.len().min(12)]
        )),
        LaunchEvent::Booting => pending("booting the VM (up to a few minutes)".to_string()),
        LaunchEvent::GuestIp(ip) => ok(format!("guest ip: {ip}")),
        LaunchEvent::Bootstrapping => {
            pending("copying files into the guest and unpacking them there".to_string())
        }
        LaunchEvent::Ready {
            session_id,
            ssh_hint,
        } => ok(format!(
            "Ready: {session_id}  (outside the TUI: {ssh_hint})"
        )),
        LaunchEvent::Failed {
            session_id,
            error,
            hint,
        } => {
            let mut s = format!("✗ {error}");
            if let Some(id) = session_id {
                s.push_str(&format!("  (session {id} kept for inspection)"));
            }
            if let Some(h) = hint {
                s.push_str(&format!("\n  try: {h}"));
            }
            Line::from(s).style(Style::default().fg(Color::Red))
        }
    }
}
