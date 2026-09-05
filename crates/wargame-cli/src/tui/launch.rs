//! Launch progress screen (DESIGN.md §7's `tui/launch.rs`): renders the
//! `LaunchEvent` log the launch pipeline (DESIGN.md §3.4) emits, and once
//! it reaches `Ready` offers `[s] ssh`, `[g] gui`, `[b] back`.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
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
    let lines: Vec<Line<'static>> = state.events.iter().map(event_line).collect();
    let title = if state.failed {
        "Failed".to_string()
    } else if state.ready {
        "Ready -- [s] ssh  [g] gui  [b] back".to_string()
    } else {
        "Launching...".to_string()
    };
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

fn event_line(event: &LaunchEvent) -> Line<'static> {
    let text = match event {
        LaunchEvent::Preflight(msg) => format!("... {msg}"),
        LaunchEvent::SessionCreated { id } => format!("[ok] session created: {id}"),
        LaunchEvent::VmCreated { name } => format!("[ok] VM created: {name}"),
        LaunchEvent::Downloading { filename } => format!("... downloading {filename}"),
        LaunchEvent::Downloaded {
            filename,
            sha256,
            size,
        } => format!("[ok] downloaded {filename} ({size} bytes, sha256 {sha256})"),
        LaunchEvent::Booting => "... booting".to_string(),
        LaunchEvent::GuestIp(ip) => format!("[ok] guest ip: {ip}"),
        LaunchEvent::Bootstrapping => "... bootstrapping".to_string(),
        LaunchEvent::Ready {
            session_id,
            ssh_hint,
        } => format!("[ok] Ready: {session_id} ({ssh_hint})"),
        LaunchEvent::Failed {
            session_id,
            error,
            hint,
        } => {
            let mut s = format!("[FAIL] {error}");
            if let Some(id) = session_id {
                s.push_str(&format!(" (session {id})"));
            }
            if let Some(h) = hint {
                s.push_str(&format!(" -- try: {h}"));
            }
            s
        }
    };
    Line::from(text)
}
