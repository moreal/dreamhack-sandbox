//! Small pieces of chrome shared by every TUI screen (DESIGN.md §7's
//! `tui/widgets.rs`): currently just the bottom status/help line.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Renders `status` (a transient message set by the reducer, e.g. the NAT
/// warning or an error) over `help` (the static key map for the current
/// screen) when there is nothing more specific to say.
pub fn status_bar(frame: &mut Frame, area: Rect, status: &str, help: &str) {
    let both = format!("{status}   ({help})");
    let text = if status.is_empty() {
        help.to_string()
    } else if both.chars().count() <= area.width as usize {
        both
    } else {
        // Not enough room for both: the transient message wins, the key
        // map comes back as soon as it clears.
        status.to_string()
    };
    let paragraph = Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM));
    frame.render_widget(paragraph, area);
}
