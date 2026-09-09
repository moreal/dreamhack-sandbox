//! Problem (challenge detail) screen (MVP.md §6's mockup, DESIGN.md §7's
//! `tui/problem.rs`): description, a profile picker, and a network picker
//! (`Tab` cycles the two fields; `n` toggles network and shows the NAT
//! warning in the caller's status line).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use wargame_core::app::ProfileOverview;
use wargame_core::domain::{Challenge, NetworkMode};

/// Which field on the Problem screen `Tab`/`↑`/`↓` currently act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Profile,
    Network,
}

/// One entry in the profile picker: its name plus the state spec.md §3 FR-1
/// asks the selection screen to show. Built once at startup from the same
/// [`ProfileOverview`] the `profiles` command renders, so the CLI and the
/// TUI cannot disagree about what a profile resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileChoice {
    pub name: String,
    /// `"kali/x86_64"`.
    pub guest: String,
    /// `"utm/emulate"`.
    pub backend: String,
    /// `"ready"`, `"rebuild needed"`, `"missing"`, `"not built"`, ...
    pub image: String,
}

impl ProfileChoice {
    pub fn from_overview(overview: &ProfileOverview) -> Self {
        ProfileChoice {
            name: overview.profile.name.clone(),
            guest: overview.guest_label(),
            backend: overview.backend_label(),
            image: overview.image_label().to_string(),
        }
    }

    /// Green once there is an image to clone, red while there is not:
    /// launching a profile in any other state fails in preflight.
    fn image_color(&self) -> Color {
        match self.image.as_str() {
            "ready" => Color::Green,
            "rebuild needed" => Color::Yellow,
            _ => Color::Red,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProblemState {
    pub challenge: Challenge,
    pub profiles: Vec<ProfileChoice>,
    pub profile_index: usize,
    pub network: NetworkMode,
    pub field: Field,
}

impl ProblemState {
    pub fn new(
        challenge: Challenge,
        profiles: Vec<ProfileChoice>,
        default_profile: &str,
        default_network: NetworkMode,
    ) -> Self {
        let profile_index = profiles
            .iter()
            .position(|p| p.name == default_profile)
            .unwrap_or(0);
        ProblemState {
            challenge,
            profiles,
            profile_index,
            network: default_network,
            field: Field::Profile,
        }
    }

    pub fn selected(&self) -> Option<&ProfileChoice> {
        self.profiles.get(self.profile_index)
    }

    pub fn selected_profile(&self) -> Option<&str> {
        self.selected().map(|p| p.name.as_str())
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
    // Name, then what it resolved to and whether it can launch at all
    // (spec.md §3 FR-1: "TUI 선택 화면에 OS/arch/backend/mode와 이미지 준비
    // 상태를 표시한다").
    let profile_line = match state.selected() {
        Some(choice) => Line::from(vec![
            Span::styled(choice.name.clone(), profile_style),
            Span::raw("   ·   "),
            Span::raw(choice.guest.clone()),
            Span::raw("  ·  "),
            Span::raw(choice.backend.clone()),
            Span::raw("  ·  image: "),
            Span::styled(
                choice.image.clone(),
                Style::default().fg(choice.image_color()),
            ),
        ]),
        None => Line::from("(no profiles available)"),
    };
    let profile =
        Paragraph::new(profile_line).block(Block::default().borders(Borders::ALL).title(format!(
            "Profile (↑/↓ to change, {} of {})",
            state.profile_index + 1,
            state.profiles.len().max(1)
        )));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::widgets::buffer_text;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use wargame_core::domain::{ChallengeRef, ChallengeSummary};

    fn challenge() -> Challenge {
        Challenge {
            summary: ChallengeSummary {
                r#ref: ChallengeRef {
                    provider: "dreamhack".to_string(),
                    id: "11".to_string(),
                },
                slug: "out_of_bound".to_string(),
                title: "out_of_bound".to_string(),
                categories: vec!["pwnable".to_string()],
                difficulty: Some("Beginner".to_string()),
            },
            description: "a description".to_string(),
            tags: vec![],
            assets: vec![],
            remote_endpoints: vec![],
            source_url: "https://example.invalid".to_string(),
        }
    }

    fn render(state: &ProblemState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), state))
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    /// spec.md §3 FR-1: "profiles 및 TUI 선택 화면에 OS/arch/backend/mode와
    /// 이미지 준비 상태를 표시한다".
    #[test]
    fn the_profile_picker_shows_guest_backend_mode_and_image_state() {
        let state = ProblemState::new(
            challenge(),
            vec![
                ProfileChoice {
                    name: "kali-x86_64".to_string(),
                    guest: "kali/x86_64".to_string(),
                    backend: "utm/emulate".to_string(),
                    image: "not built".to_string(),
                },
                ProfileChoice {
                    name: "macos-re-arm64".to_string(),
                    guest: "macos/arm64".to_string(),
                    backend: "tart/virtualize".to_string(),
                    image: "ready".to_string(),
                },
            ],
            "kali-x86_64",
            NetworkMode::Isolated,
        );
        let rendered = render(&state);
        assert!(rendered.contains("kali-x86_64"), "{rendered}");
        assert!(rendered.contains("kali/x86_64"), "{rendered}");
        assert!(rendered.contains("utm/emulate"), "{rendered}");
        assert!(rendered.contains("not built"), "{rendered}");
    }

    /// The picker follows the selection, so the state on screen is always
    /// the state of the profile that would actually launch.
    #[test]
    fn moving_the_selection_shows_the_other_profiles_state() {
        let mut state = ProblemState::new(
            challenge(),
            vec![
                ProfileChoice {
                    name: "kali-x86_64".to_string(),
                    guest: "kali/x86_64".to_string(),
                    backend: "utm/emulate".to_string(),
                    image: "not built".to_string(),
                },
                ProfileChoice {
                    name: "macos-re-arm64".to_string(),
                    guest: "macos/arm64".to_string(),
                    backend: "tart/virtualize".to_string(),
                    image: "ready".to_string(),
                },
            ],
            "kali-x86_64",
            NetworkMode::Isolated,
        );
        state.profile_index = 1;
        assert_eq!(state.selected_profile(), Some("macos-re-arm64"));
        let rendered = render(&state);
        assert!(rendered.contains("tart/virtualize"), "{rendered}");
        assert!(!rendered.contains("utm/emulate"), "{rendered}");
    }

    /// A profile list that is empty still renders rather than panicking.
    #[test]
    fn no_profiles_renders_a_placeholder() {
        let state = ProblemState::new(challenge(), vec![], "none", NetworkMode::Isolated);
        assert!(render(&state).contains("no profiles available"));
    }
}
