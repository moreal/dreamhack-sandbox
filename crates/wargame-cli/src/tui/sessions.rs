//! Sessions screen (MVP.md §7.3, DESIGN.md §7's `tui/sessions.rs`): a table
//! of local sessions with stop/resume/keep/delete/ssh/gui (delete asks `y/N`).

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use time::OffsetDateTime;

use wargame_core::domain::{KeepPolicy, SessionState};

use super::app::App;

fn state_style(state: SessionState) -> Style {
    match state {
        SessionState::Ready | SessionState::Running => Style::default().fg(Color::Green),
        SessionState::Stopped => Style::default().fg(Color::Yellow),
        SessionState::Failed => Style::default().fg(Color::Red),
        _ => Style::default().add_modifier(Modifier::DIM),
    }
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = if app.pending_delete.is_some() {
        "Sessions — delete this session? [y/N]"
    } else {
        "Sessions"
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    if app.sessions.is_empty() {
        let empty =
            Paragraph::new("No sessions yet. Press Esc to search for a challenge and launch one.")
                .style(Style::default().add_modifier(Modifier::DIM))
                .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let now = OffsetDateTime::now_utc();
    let header = Row::new(["id", "title", "state", "network", "profile", "keep", "age"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .sessions
        .iter()
        .map(|s| {
            Row::new(vec![
                Cell::from(s.id.clone()),
                Cell::from(s.display_name.clone()),
                Cell::from(s.state.to_string()).style(state_style(s.state)),
                Cell::from(s.network.to_string()),
                Cell::from(s.profile.clone()),
                Cell::from(match s.keep {
                    KeepPolicy::Keep => "yes",
                    KeepPolicy::Discard => "",
                }),
                Cell::from(crate::output::age(s.created_at, now)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Min(24),
        Constraint::Min(16),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Min(14),
        Constraint::Length(4),
        Constraint::Length(8),
    ];
    let mut table_state = TableState::default();
    table_state.select(Some(app.sessions_selected));
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut table_state);
}
