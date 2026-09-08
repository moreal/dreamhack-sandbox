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
    /// The page the current `results` came from (1-based).
    pub page: u32,
    /// The provider's next page, if there is one (`]` requests it).
    pub next_page: Option<u32>,
    /// Total matches across all pages, as reported by the provider.
    pub total: u64,
    /// Whether any search has completed yet (drives the empty-state text).
    pub searched: bool,
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
            page: 1,
            next_page: None,
            total: 0,
            searched: false,
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
    let input_title = if state.editing {
        "Search Dreamhack (Enter to search)"
    } else {
        "Search Dreamhack (/ to edit)"
    };
    let shown = if state.editing {
        format!("{}▏", state.input)
    } else {
        state.input.clone()
    };
    let input = Paragraph::new(shown)
        .style(input_style)
        .block(Block::default().borders(Borders::ALL).title(input_title));
    frame.render_widget(input, chunks[0]);

    let results_title = if state.results.is_empty() {
        "Results".to_string()
    } else {
        let first = (state.page - 1) as u64 * 20 + 1;
        let last = first + state.results.len() as u64 - 1;
        let more = if state.next_page.is_some() {
            "  ] next page"
        } else {
            ""
        };
        let back = if state.page > 1 {
            "  [ previous page"
        } else {
            ""
        };
        format!("Results {first}–{last} of {}{more}{back}", state.total)
    };
    if state.results.is_empty() {
        let text = if state.searched {
            "No challenges matched. Press / to change the query."
        } else {
            "Type a query and press Enter. Enter on a result shows its details."
        };
        let empty = Paragraph::new(text)
            .style(Style::default().add_modifier(Modifier::DIM))
            .block(Block::default().borders(Borders::ALL).title(results_title));
        frame.render_widget(empty, chunks[1]);
        return;
    }

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
    list_state.select(Some(state.selected));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(results_title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
}
