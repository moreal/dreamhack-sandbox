//! Sessions screen (MVP.md §7.3, DESIGN.md §7's `tui/sessions.rs`): a table
//! of local sessions with stop/resume/delete (delete asks `y/N`).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use super::app::App;

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| {
            ListItem::new(format!(
                "{:<24} {:<20} {:<12} {}",
                s.id,
                s.display_name,
                format!("{:?}", s.state).to_lowercase(),
                s.profile
            ))
        })
        .collect();
    let mut list_state = ListState::default();
    if !app.sessions.is_empty() {
        list_state.select(Some(app.sessions_selected));
    }
    let title = if app.pending_delete.is_some() {
        "Sessions -- delete this session? [y/N]"
    } else {
        "Sessions"
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut list_state);
}
