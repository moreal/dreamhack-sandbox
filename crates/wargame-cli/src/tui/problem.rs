//! Problem (challenge detail) screen (MVP.md §6's mockup, DESIGN.md §7's
//! `tui/problem.rs`): description, a profile picker, and a network picker
//! (`Tab` cycles the two fields; `n` toggles network and shows the NAT
//! warning in the caller's status line).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use wargame_core::domain::{Challenge, NetworkMode};

/// Which field on the Problem screen `Tab`/`↑`/`↓` currently act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Profile,
    Network,
}

#[derive(Debug, Clone)]
pub struct ProblemState {
    pub challenge: Challenge,
    pub profile_names: Vec<String>,
    pub profile_index: usize,
    pub network: NetworkMode,
    pub field: Field,
}

impl ProblemState {
    pub fn new(
        challenge: Challenge,
        profile_names: Vec<String>,
        default_profile: &str,
        default_network: NetworkMode,
    ) -> Self {
        let profile_index = profile_names
            .iter()
            .position(|p| p == default_profile)
            .unwrap_or(0);
        ProblemState {
            challenge,
            profile_names,
            profile_index,
            network: default_network,
            field: Field::Profile,
        }
    }

    pub fn selected_profile(&self) -> Option<&str> {
        self.profile_names
            .get(self.profile_index)
            .map(String::as_str)
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &ProblemState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);

    let summary = &state.challenge.summary;
    let mut facts = vec![summary.r#ref.to_string()];
    if !summary.categories.is_empty() {
        facts.push(summary.categories.join(", "));
    }
    if let Some(d) = &summary.difficulty {
        facts.push(d.clone());
    }
    facts.push(match state.challenge.assets.len() {
        0 => "no files".to_string(),
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    });
    let header = Paragraph::new(format!("{}   ·   {}", summary.title, facts.join("  ·  ")))
        .block(Block::default().borders(Borders::ALL).title("Challenge"));
    frame.render_widget(header, chunks[0]);

    let desc = Paragraph::new(state.challenge.description.as_str())
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Description"));
    frame.render_widget(desc, chunks[1]);

    let profile_style = if state.field == Field::Profile {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let profile_text = state
        .selected_profile()
        .map(str::to_string)
        .unwrap_or_else(|| "(no profiles available)".to_string());
    let profile = Paragraph::new(profile_text).style(profile_style).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Profile (↑/↓ to change)"),
    );
    frame.render_widget(profile, chunks[2]);

    let network_style = if state.field == Field::Network {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let (network_label, network_color) = match state.network {
        NetworkMode::Isolated => ("isolated", Color::Green),
        NetworkMode::Nat => ("nat", Color::Red),
    };
    let network = Paragraph::new(network_label)
        .style(network_style.fg(network_color))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Network (n to toggle)   —   Enter to launch"),
        );
    frame.render_widget(network, chunks[3]);
}
