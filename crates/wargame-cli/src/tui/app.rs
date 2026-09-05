//! The TUI's Elm-style state machine (DESIGN.md §7; PLAN.md Task 8): every
//! key press and every async result flows through one [`App::update`],
//! which returns the [`Effect`]s the runtime loop (`tui::run`) should carry
//! out. The reducer itself never touches a terminal, a socket, or
//! [`AppContext`] -- that is what makes it unit-testable without a pty.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use wargame_core::app::{LaunchEvent, LaunchRequest};
use wargame_core::domain::{Challenge, ChallengeRef, NetworkMode, Session};
use wargame_core::ports::{SearchPage, SearchQuery};

use super::launch::LaunchState;
use super::problem::{Field, ProblemState};
use super::search::SearchState;

/// Which screen is currently on top (MVP.md §6-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Search,
    Problem,
    Launching,
    Sessions,
}

/// Everything that can flow into the reducer: raw key presses ride along
/// next to the async results the runtime loop feeds back from application
/// services (DESIGN.md §7: "one `update`").
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    SearchResults(SearchPage),
    Challenge(Challenge),
    Launch(LaunchEvent),
    Sessions(Vec<Session>),
    Error(String),
}

/// Everything the reducer can ask the runtime loop to do. None of these run
/// inside `update` itself -- the loop dispatches each to an `AppContext`
/// call (or, for `Quit`/`ExecSsh`, handles it directly).
#[derive(Debug, Clone)]
pub enum Effect {
    Search(SearchQuery),
    Open(ChallengeRef),
    Launch(LaunchRequest),
    LoadSessions,
    Stop(String),
    Resume(String),
    Delete(String),
    ExecSsh(String),
    OpenGui(String),
    Quit,
}

/// The whole TUI's state (MVP.md §6-7's Search -> Problem -> Launching ->
/// Sessions flow).
pub struct App {
    pub screen: Screen,
    pub search: SearchState,
    pub problem: Option<ProblemState>,
    pub launch: Option<LaunchState>,
    pub sessions: Vec<Session>,
    pub status: String,
    /// Selection cursor for the Sessions screen's list.
    pub sessions_selected: usize,
    /// Set by `d` on the Sessions screen while waiting for a `y`/`N` answer
    /// (NFR-07: destructive actions need a deliberate confirmation).
    pub pending_delete: Option<String>,
    default_profile: String,
    default_network: NetworkMode,
    available_profiles: Vec<String>,
}

impl App {
    pub fn new(
        default_profile: String,
        default_network: NetworkMode,
        available_profiles: Vec<String>,
    ) -> Self {
        App {
            screen: Screen::Search,
            search: SearchState::new(),
            problem: None,
            launch: None,
            sessions: Vec::new(),
            status: String::new(),
            sessions_selected: 0,
            pending_delete: None,
            default_profile,
            default_network,
            available_profiles,
        }
    }

    /// Help text for the current screen's key map, shown in the status bar
    /// whenever `status` itself is empty (MVP.md §6's mockup footer).
    pub fn help_text(&self) -> &'static str {
        match self.screen {
            Screen::Search => {
                if self.search.editing {
                    "Enter: search   Esc: stop editing"
                } else {
                    "/: search   Enter: open   s: sessions   q: quit"
                }
            }
            Screen::Problem => {
                "Tab: field   up/down: change   n: toggle network   l: launch   Esc: back   q: quit"
            }
            Screen::Launching => "s: ssh   g: gui   b: back   q: quit",
            Screen::Sessions => {
                "up/down: select   x: stop   r: resume   d: delete   Enter: ssh   Esc: back   q: quit"
            }
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::SearchResults(page) => {
                self.search.results = page.items;
                self.search.selected = 0;
                self.status = format!("{} result(s)", page.total);
                Vec::new()
            }
            Msg::Challenge(challenge) => {
                self.problem = Some(ProblemState::new(
                    challenge,
                    self.available_profiles.clone(),
                    &self.default_profile,
                    self.default_network,
                ));
                self.screen = Screen::Problem;
                self.status.clear();
                Vec::new()
            }
            Msg::Launch(event) => self.handle_launch_event(event),
            Msg::Sessions(sessions) => {
                self.sessions = sessions;
                if self.sessions_selected >= self.sessions.len() {
                    self.sessions_selected = self.sessions.len().saturating_sub(1);
                }
                Vec::new()
            }
            Msg::Error(err) => {
                self.status = format!("Error: {err}");
                Vec::new()
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // Some terminals (mainly Windows) report Repeat/Release too; acting
        // only on Press avoids doing everything twice.
        if key.kind != KeyEventKind::Press {
            return Vec::new();
        }
        match self.screen {
            Screen::Search => self.handle_search_key(key),
            Screen::Problem => self.handle_problem_key(key),
            Screen::Launching => self.handle_launching_key(key),
            Screen::Sessions => self.handle_sessions_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.search.editing {
            return match key.code {
                KeyCode::Char(c) => {
                    self.search.input.push(c);
                    Vec::new()
                }
                KeyCode::Backspace => {
                    self.search.input.pop();
                    Vec::new()
                }
                KeyCode::Enter => {
                    self.search.editing = false;
                    vec![Effect::Search(SearchQuery {
                        text: self.search.input.clone(),
                        category: None,
                        page: 1,
                        page_size: 20,
                    })]
                }
                KeyCode::Esc => {
                    self.search.editing = false;
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        match key.code {
            KeyCode::Char('/') => {
                self.search.editing = true;
                Vec::new()
            }
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Char('s') => {
                self.screen = Screen::Sessions;
                vec![Effect::LoadSessions]
            }
            KeyCode::Down => {
                if !self.search.results.is_empty() {
                    self.search.selected =
                        (self.search.selected + 1).min(self.search.results.len() - 1);
                }
                Vec::new()
            }
            KeyCode::Up => {
                self.search.selected = self.search.selected.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Enter => self
                .search
                .results
                .get(self.search.selected)
                .map(|item| vec![Effect::Open(item.r#ref.clone())])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn handle_problem_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let default_profile = self.default_profile.clone();
        let Some(problem) = self.problem.as_mut() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Tab => {
                problem.field = match problem.field {
                    Field::Profile => Field::Network,
                    Field::Network => Field::Profile,
                };
                Vec::new()
            }
            KeyCode::Up | KeyCode::Down
                if problem.field == Field::Profile && !problem.profile_names.is_empty() =>
            {
                let len = problem.profile_names.len();
                problem.profile_index = match key.code {
                    KeyCode::Up => (problem.profile_index + len - 1) % len,
                    _ => (problem.profile_index + 1) % len,
                };
                Vec::new()
            }
            KeyCode::Char('n') => {
                problem.network = match problem.network {
                    NetworkMode::Isolated => NetworkMode::Nat,
                    NetworkMode::Nat => NetworkMode::Isolated,
                };
                self.status = match problem.network {
                    NetworkMode::Nat => "Warning: NAT lets this VM initiate outbound connections \
                         (MVP.md \u{00a7}18). Press l to launch, n to switch back."
                        .to_string(),
                    NetworkMode::Isolated => "Network: isolated (default).".to_string(),
                };
                Vec::new()
            }
            KeyCode::Char('l') => {
                let profile = problem
                    .selected_profile()
                    .map(str::to_string)
                    .unwrap_or(default_profile);
                let req = LaunchRequest {
                    challenge: problem.challenge.summary.r#ref.clone(),
                    profile,
                    network: problem.network,
                    resources: None,
                    extract: true,
                    gui: false,
                };
                self.launch = Some(LaunchState::default());
                self.screen = Screen::Launching;
                self.status.clear();
                vec![Effect::Launch(req)]
            }
            KeyCode::Esc => {
                self.screen = Screen::Search;
                Vec::new()
            }
            KeyCode::Char('q') => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_launch_event(&mut self, event: LaunchEvent) -> Vec<Effect> {
        let state = self.launch.get_or_insert_with(LaunchState::default);
        match &event {
            LaunchEvent::Ready { session_id, .. } => {
                state.ready = true;
                state.session_id = Some(session_id.clone());
            }
            LaunchEvent::Failed { session_id, .. } => {
                state.failed = true;
                if session_id.is_some() {
                    state.session_id = session_id.clone();
                }
            }
            _ => {}
        }
        state.events.push(event);
        self.screen = Screen::Launching;
        Vec::new()
    }

    fn handle_launching_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ready = self.launch.as_ref().is_some_and(|l| l.ready);
        let session_id = self.launch.as_ref().and_then(|l| l.session_id.clone());
        match key.code {
            KeyCode::Char('s') if ready => session_id
                .map(|id| vec![Effect::ExecSsh(id)])
                .unwrap_or_default(),
            KeyCode::Char('g') if ready => session_id
                .map(|id| vec![Effect::OpenGui(id)])
                .unwrap_or_default(),
            KeyCode::Char('b') if ready => {
                self.screen = Screen::Sessions;
                vec![Effect::LoadSessions]
            }
            KeyCode::Esc => {
                self.screen = Screen::Search;
                Vec::new()
            }
            KeyCode::Char('q') => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_sessions_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(pending) = self.pending_delete.take() {
            self.status.clear();
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => vec![Effect::Delete(pending)],
                _ => Vec::new(),
            };
        }
        match key.code {
            KeyCode::Down => {
                if !self.sessions.is_empty() {
                    self.sessions_selected =
                        (self.sessions_selected + 1).min(self.sessions.len() - 1);
                }
                Vec::new()
            }
            KeyCode::Up => {
                self.sessions_selected = self.sessions_selected.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Char('x') => self
                .selected_session_id()
                .map(|id| vec![Effect::Stop(id)])
                .unwrap_or_default(),
            KeyCode::Char('r') => self
                .selected_session_id()
                .map(|id| vec![Effect::Resume(id)])
                .unwrap_or_default(),
            KeyCode::Char('d') => {
                if let Some(id) = self.selected_session_id() {
                    self.pending_delete = Some(id);
                    self.status = "Delete this session? [y/N]".to_string();
                }
                Vec::new()
            }
            KeyCode::Enter => self
                .selected_session_id()
                .map(|id| vec![Effect::ExecSsh(id)])
                .unwrap_or_default(),
            KeyCode::Esc => {
                self.screen = Screen::Search;
                Vec::new()
            }
            KeyCode::Char('q') => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    fn selected_session_id(&self) -> Option<String> {
        self.sessions
            .get(self.sessions_selected)
            .map(|s| s.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use wargame_core::domain::{ChallengeSummary, KeepPolicy, Resources, SessionState};

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn app() -> App {
        App::new(
            "macos-minimal-arm64".to_string(),
            NetworkMode::Isolated,
            vec![
                "macos-minimal-arm64".to_string(),
                "macos-re-arm64".to_string(),
            ],
        )
    }

    fn summary(id: &str, title: &str) -> ChallengeSummary {
        ChallengeSummary {
            r#ref: ChallengeRef {
                provider: "dreamhack".to_string(),
                id: id.to_string(),
            },
            slug: title.to_string(),
            title: title.to_string(),
            categories: vec!["pwnable".to_string()],
            difficulty: Some("Beginner".to_string()),
        }
    }

    fn challenge(id: &str, title: &str) -> Challenge {
        Challenge {
            summary: summary(id, title),
            description: "a description".to_string(),
            tags: vec![],
            assets: vec![],
            remote_endpoints: vec![],
            source_url: "https://example.invalid".to_string(),
        }
    }

    fn sample_session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            display_name: "out_of_bound".to_string(),
            challenge: summary("1", "out_of_bound"),
            challenge_snapshot: challenge("1", "out_of_bound"),
            vm_provider: "tart".to_string(),
            profile: "macos-re-arm64".to_string(),
            guest_os: wargame_core::domain::GuestOs::Macos,
            guest_arch: wargame_core::domain::GuestArch::Arm64,
            network: NetworkMode::Isolated,
            resources: Resources {
                cpu: 2,
                memory_mib: 4096,
                disk_gib: 20,
            },
            created_at: time::OffsetDateTime::now_utc(),
            state: SessionState::Ready,
            vm_ref: None,
            assets: vec![],
            keep: KeepPolicy::Discard,
            last_error: None,
            runner_pid: None,
            guest_ip: None,
        }
    }

    #[test]
    fn typing_then_enter_emits_search() {
        let mut a = app();
        assert!(a.update(key(KeyCode::Char('o'))).is_empty());
        assert!(a.update(key(KeyCode::Char('b'))).is_empty());
        assert_eq!(a.search.input, "ob");

        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Search(q) => assert_eq!(q.text, "ob"),
            other => panic!("expected Search, got {other:?}"),
        }
        assert!(!a.search.editing, "Enter should hand focus to the list");
    }

    #[test]
    fn search_results_fill_list_and_enter_opens() {
        let mut a = app();
        a.update(key(KeyCode::Enter)); // leaves editing mode with an empty query
        a.update(Msg::SearchResults(SearchPage {
            items: vec![summary("11", "out_of_bound")],
            next_page: None,
            total: 1,
        }));
        assert_eq!(a.search.results.len(), 1);

        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Open(r) => assert_eq!(r.id, "11"),
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn n_toggles_nat_and_warns_on_problem_screen() {
        let mut a = app();
        a.update(Msg::Challenge(challenge("11", "out_of_bound")));
        assert_eq!(a.screen, Screen::Problem);

        a.update(key(KeyCode::Char('n')));
        assert_eq!(a.problem.as_ref().unwrap().network, NetworkMode::Nat);
        assert!(a.status.contains("NAT"), "status was: {}", a.status);

        a.update(key(KeyCode::Char('n')));
        assert_eq!(a.problem.as_ref().unwrap().network, NetworkMode::Isolated);
    }

    #[test]
    fn l_emits_launch_with_selected_profile_and_network() {
        let mut a = app();
        a.update(Msg::Challenge(challenge("11", "out_of_bound")));
        a.update(key(KeyCode::Tab)); // -> Network field, still same selected profile
        a.update(key(KeyCode::Char('n'))); // -> Nat

        let effects = a.update(key(KeyCode::Char('l')));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Launch(req) => {
                assert_eq!(req.challenge.id, "11");
                assert_eq!(req.profile, "macos-minimal-arm64");
                assert_eq!(req.network, NetworkMode::Nat);
            }
            other => panic!("expected Launch, got {other:?}"),
        }
        assert_eq!(a.screen, Screen::Launching);
    }

    #[test]
    fn ready_launch_event_sets_launching_screen_with_ready_flag() {
        let mut a = app();
        a.update(Msg::Launch(LaunchEvent::Ready {
            session_id: "dh-out-of-bound-a1b2c3".to_string(),
            ssh_hint: "wargame ssh dh-out-of-bound-a1b2c3".to_string(),
        }));
        assert_eq!(a.screen, Screen::Launching);
        let launch = a.launch.as_ref().unwrap();
        assert!(launch.ready);
        assert_eq!(launch.session_id.as_deref(), Some("dh-out-of-bound-a1b2c3"));
    }

    #[test]
    fn q_emits_quit() {
        let mut a = app();
        a.search.editing = false;
        let effects = a.update(key(KeyCode::Char('q')));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Quit));
    }

    #[test]
    fn q_while_editing_types_instead_of_quitting() {
        let mut a = app();
        assert!(a.search.editing);
        let effects = a.update(key(KeyCode::Char('q')));
        assert!(effects.is_empty());
        assert_eq!(a.search.input, "q");
    }

    #[test]
    fn sessions_screen_delete_requires_confirmation() {
        let mut a = app();
        a.update(Msg::Sessions(vec![sample_session("dh-a-1")]));
        a.screen = Screen::Sessions;

        let effects = a.update(key(KeyCode::Char('d')));
        assert!(effects.is_empty());
        assert!(a.pending_delete.is_some());

        // Anything but y/Y cancels without emitting Delete.
        let effects = a.update(key(KeyCode::Char('n')));
        assert!(effects.is_empty());
        assert!(a.pending_delete.is_none());

        a.update(key(KeyCode::Char('d')));
        let effects = a.update(key(KeyCode::Char('y')));
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], Effect::Delete(id) if id == "dh-a-1"));
    }
}
