//! Sessions screen (MVP.md §7.3, DESIGN.md §7's `tui/sessions.rs`): a table
//! of local sessions with stop/resume/keep/delete/ssh/gui (delete asks `y/N`).

use ratatui::layout::{Constraint, Layout, Rect};
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

/// One line per session whose backend could not be asked, so an
/// uninstalled UTM (or a denied Automation grant) is visible as a *stale
/// row*, not as an empty list or a silently wrong state (spec.md §6 row 1).
fn backend_notes(app: &App) -> Vec<String> {
    app.sessions
        .iter()
        .filter_map(|l| {
            let error = l.reconcile_error.as_deref()?;
            Some(format!(
                "! {} ({}): {error} — the state shown is the last one recorded",
                l.session.id,
                l.session.backend_name()
            ))
        })
        .collect()
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = if app.pending_delete.is_some() {
        "Sessions — delete this session? [y/N]"
    } else {
        "Sessions"
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    // The notes take the bottom of the pane, never the whole of it: the
    // table is what the user came for.
    let notes = backend_notes(app);
    let (area, notes_area) = if notes.is_empty() || area.height < 4 {
        (area, None)
    } else {
        let lines = (notes.len() as u16).min(area.height / 3).max(1);
        let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(lines)]).split(area);
        (parts[0], Some(parts[1]))
    };
    if let Some(notes_area) = notes_area {
        frame.render_widget(
            Paragraph::new(notes.join("\n")).style(Style::default().fg(Color::Yellow)),
            notes_area,
        );
    }

    if app.sessions.is_empty() {
        let empty =
            Paragraph::new("No sessions yet. Press Esc to search for a challenge and launch one.")
                .style(Style::default().add_modifier(Modifier::DIM))
                .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let now = OffsetDateTime::now_utc();
    // `backend` is per session, not per profile: Tart and UTM sessions live
    // side by side and every action here routes on the session's own backend,
    // including one written before sessions carried a VM id (spec.md §3
    // FR-5).
    let header = Row::new([
        "id", "title", "state", "backend", "network", "profile", "keep", "age",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .sessions
        .iter()
        .map(|listing| {
            let s = &listing.session;
            Row::new(vec![
                // A row whose backend could not be asked is marked here and
                // explained underneath, rather than reading as confirmed.
                Cell::from(match listing.reconcile_error {
                    Some(_) => format!("! {}", s.id),
                    None => s.id.clone(),
                }),
                Cell::from(s.display_name.clone()),
                Cell::from(s.state.to_string()).style(state_style(s.state)),
                Cell::from(s.backend_name().to_string()),
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
        Constraint::Length(7),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::widgets::buffer_text;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use wargame_core::app::SessionListing;
    use wargame_core::domain::{NetworkMode, Session};

    /// A session record as the pre-multi-OS build wrote it: no
    /// `guest_distribution`, `vm_mode`, `connection` or `workspace_root`, and
    /// a `vm_ref` with a name but no stable id (spec.md §3 FR-5's "과거 세션
    /// JSON에 migration/default를 정의한다").
    const LEGACY_SESSION_JSON: &str = r#"{
      "id": "dh-legacy-000001",
      "display_name": "legacy",
      "challenge": {
        "ref": {"provider": "dreamhack", "id": "1"},
        "slug": "legacy", "title": "legacy", "categories": [], "difficulty": null
      },
      "challenge_snapshot": {
        "summary": {
          "ref": {"provider": "dreamhack", "id": "1"},
          "slug": "legacy", "title": "legacy", "categories": [], "difficulty": null
        },
        "description": "", "tags": [], "assets": [], "remote_endpoints": [],
        "source_url": "https://example.invalid"
      },
      "vm_provider": "tart",
      "profile": "macos-re-arm64",
      "guest_os": "macos",
      "guest_arch": "arm64",
      "network": "isolated",
      "resources": {"cpu": 2, "memory_mib": 4096, "disk_gib": 20},
      "created_at": "2026-01-01T00:00:00Z",
      "state": "stopped",
      "vm_ref": {"backend": "tart", "name": "wg-dh-legacy-000001"},
      "assets": [],
      "keep": "discard",
      "last_error": null,
      "runner_pid": null,
      "guest_ip": null
    }"#;

    fn legacy_session() -> Session {
        serde_json::from_str(LEGACY_SESSION_JSON).expect("a pre-multi-OS record still loads")
    }

    fn utm_session() -> Session {
        let mut session = legacy_session();
        session.id = "dh-kali-000002".to_string();
        session.display_name = "kali one".to_string();
        session.profile = "kali-custom".to_string();
        session.state = SessionState::Ready;
        session.vm_provider = "utm".to_string();
        session.vm_ref = Some(wargame_core::domain::VmRef {
            backend: "utm".to_string(),
            id: "634CF941-A83B-4795-8EF7-1B8D5846F202".to_string(),
            name: "wg-dh-kali-000002".to_string(),
        });
        session
    }

    fn render(sessions: Vec<Session>) -> String {
        render_listings(
            sessions
                .into_iter()
                .map(|session| SessionListing {
                    session,
                    reconcile_error: None,
                })
                .collect(),
        )
    }

    fn render_listings(sessions: Vec<SessionListing>) -> String {
        let mut app = App::new("macos-re-arm64".to_string(), NetworkMode::Isolated, vec![]);
        app.sessions = sessions;
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), &app))
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    /// spec.md §3 FR-5: the list has to work with Tart and UTM sessions side
    /// by side, and say which is which -- the backend is what every
    /// lifecycle action routes on.
    #[test]
    fn the_sessions_table_names_each_sessions_backend() {
        let rendered = render(vec![utm_session(), legacy_session()]);
        assert!(rendered.contains("backend"), "{rendered}");
        assert!(rendered.contains("utm"), "{rendered}");
        assert!(rendered.contains("tart"), "{rendered}");
        assert!(rendered.contains("dh-kali-000002"), "{rendered}");
        assert!(rendered.contains("dh-legacy-000001"), "{rendered}");
    }

    /// A record written before sessions carried a VM id or a mode still
    /// renders, reading its backend off the ref the old build did write.
    #[test]
    fn a_legacy_session_still_renders_with_its_backend() {
        let session = legacy_session();
        assert_eq!(session.backend_name(), "tart");
        assert_eq!(
            session.vm_ref.as_ref().unwrap().id,
            "wg-dh-legacy-000001",
            "a missing id falls back to the name"
        );
        assert!(render(vec![session]).contains("stopped"));
    }

    /// One backend being unreachable must not blank the table: every
    /// session is still listed, the affected row is marked, and the reason
    /// is on screen (spec.md §6 row 1: "Tart 작업은 계속 가능").
    #[test]
    fn a_session_whose_backend_could_not_be_asked_is_marked_and_explained() {
        let rendered = render_listings(vec![
            SessionListing {
                session: utm_session(),
                reconcile_error: Some("VM backend utm unavailable: utmctl not found".to_string()),
            },
            SessionListing {
                session: legacy_session(),
                reconcile_error: None,
            },
        ]);
        assert!(rendered.contains("dh-legacy-000001"), "{rendered}");
        assert!(rendered.contains("! dh-kali-000002"), "{rendered}");
        assert!(rendered.contains("utmctl not found"), "{rendered}");
    }

    #[test]
    fn an_empty_list_says_so_instead_of_drawing_a_table() {
        assert!(render(vec![]).contains("No sessions yet"));
    }
}
