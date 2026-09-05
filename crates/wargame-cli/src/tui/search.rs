//! Search screen (MVP.md §6's mockup, DESIGN.md §7's `tui/search.rs`): a
//! text input (`/` focuses it) over a result list (`Enter` opens the
//! selected item).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use wargame_core::domain::ChallengeSummary;

/// State for the search screen. `editing == true` means keystrokes go into
/// `input`; otherwise they navigate `results`.
#[derive(Debug, Clone)]
pub struct SearchState {
    pub input: String,
    pub editing: bool,
    pub results: Vec<ChallengeSummary>,
    pub selected: usize,
}

impl SearchState {
    /// A fresh search screen starts in editing mode (MVP.md §6: the search
    /// box is where the cursor starts).
    pub fn new() -> Self {
        SearchState {
            input: String::new(),
            editing: true,
            results: Vec::new(),
            selected: 0,
        }
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &SearchState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let input_style = if state.editing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let input = Paragraph::new(state.input.as_str())
        .style(input_style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Search Dreamhack"),
        );
    frame.render_widget(input, chunks[0]);

    let items: Vec<ListItem> = state
        .results
        .iter()
        .map(|c| {
            ListItem::new(format!(
                "{:<32} {:<16} {}",
                c.title,
                c.categories.join(","),
                c.difficulty.clone().unwrap_or_default()
            ))
        })
        .collect();
    let mut list_state = ListState::default();
    if !state.results.is_empty() {
        list_state.select(Some(state.selected));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Results"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
}
