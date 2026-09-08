//! The TUI's Elm-style state machine (DESIGN.md §7; PLAN.md Task 8): every
//! key press and every async result flows through one [`App::update`],
//! which returns the [`Effect`]s the runtime loop (`tui::run`) should carry
//! out. The reducer itself never touches a terminal, a socket, or
//! [`AppContext`] -- that is what makes it unit-testable without a pty.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use wargame_core::app::{LaunchEvent, LaunchRequest};
use wargame_core::domain::{
    Challenge, ChallengeRef, KeepPolicy, NetworkMode, Session, SessionState,
};
use wargame_core::ports::{SearchPage, SearchQuery};

use super::launch::LaunchState;
use super::problem::{Field, ProblemState, ProfileChoice};
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
    /// `(id, keep)`: flip a session's keep policy.
    Keep(String, bool),
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
    available_profiles: Vec<ProfileChoice>,
}

impl App {
    pub fn new(
        default_profile: String,
        default_network: NetworkMode,
        available_profiles: Vec<ProfileChoice>,
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
    /// whenever `status` itself is empty (MVP.md §6's mockup footer). Only
    /// keys that would actually do something right now are listed.
    pub fn help_text(&self) -> &'static str {
        match self.screen {
            Screen::Search => {
                if self.search.editing {
                    "Enter: search   Esc: stop editing   Ctrl+C: quit"
                } else if self.search.results.is_empty() {
                    "/: edit query   s: sessions   q: quit"
                } else {
                    "↑/↓: select   Enter: open   [/]: page   /: edit query   s: sessions   q: quit"
                }
            }
            Screen::Problem => {
                "Enter/l: launch   ↑/↓: profile   n: toggle network   Esc: back   q: quit"
            }
            Screen::Launching => {
                let launch = self.launch.as_ref();
                if launch.is_some_and(|l| l.ready) {
                    "s: ssh   g: gui   b: sessions   Esc: search   q: quit"
                } else if launch.is_some_and(|l| l.failed) {
                    "b: sessions (inspect/delete)   Esc: search   q: quit"
                } else {
                    "launching…   b: sessions   q: quit"
                }
            }
            Screen::Sessions => {
                if self.sessions.is_empty() {
                    "Esc: search   R: refresh   q: quit"
                } else {
                    "Enter: ssh   g: gui   x: stop   r: resume   k: keep   d: delete   R: refresh   Esc: search   q: quit"
                }
            }
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::SearchResults(page) => {
                self.search.results = page.items;
                self.search.selected = 0;
                self.search.next_page = page.next_page;
                self.search.total = page.total;
                self.search.searched = true;
                self.status = if page.total == 0 {
                    "No challenges matched; press / to change the query.".to_string()
                } else {
                    format!("{} result(s), page {}", page.total, self.search.page)
                };
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
                if self.status.ends_with('…') {
                    // A pending "Stopping…"/"Resuming…" note is resolved by
                    // the refreshed list.
                    self.status.clear();
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
        // Ctrl+C quits from anywhere, including while typing a query (raw
        // mode swallows the terminal's own SIGINT).
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return vec![Effect::Quit];
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
                    self.search.page = 1;
                    self.search_page(1)
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
                self.status = "Loading sessions…".to_string();
                vec![Effect::LoadSessions]
            }
            KeyCode::Char(']') | KeyCode::Right | KeyCode::PageDown => {
                match self.search.next_page {
                    Some(next) => self.search_page(next),
                    None => Vec::new(),
                }
            }
            KeyCode::Char('[') | KeyCode::Left | KeyCode::PageUp => {
                if self.search.page > 1 {
                    let prev = self.search.page - 1;
                    self.search_page(prev)
                } else {
                    Vec::new()
                }
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
            KeyCode::Enter => match self.search.results.get(self.search.selected) {
                Some(item) => {
                    self.status = format!("Opening {}…", item.title);
                    vec![Effect::Open(item.r#ref.clone())]
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Issues the current query for `page`, noting it in the status line
    /// so the user sees something happened before the results arrive.
    fn search_page(&mut self, page: u32) -> Vec<Effect> {
        self.search.page = page;
        self.status = format!("Searching \"{}\"…", self.search.input);
        vec![Effect::Search(SearchQuery {
            text: self.search.input.clone(),
            category: None,
            page,
            page_size: 20,
        })]
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
                if problem.field == Field::Profile && !problem.profiles.is_empty() =>
            {
                let len = problem.profiles.len();
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
            KeyCode::Char('l') | KeyCode::Enter => {
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
            KeyCode::Char('b') => {
                // Always available: a failed launch leaves a session to
                // inspect or delete, and a slow one can be watched from
                // the list.
                self.screen = Screen::Sessions;
                self.status = "Loading sessions…".to_string();
                vec![Effect::LoadSessions]
            }
            KeyCode::Esc => {
                self.screen = Screen::Search;
                self.status.clear();
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
            KeyCode::Char('x') => match self.selected_session() {
                Some(s) if s.state.is_up() => {
                    let id = s.id.clone();
                    self.status = format!("Stopping {id}…");
                    vec![Effect::Stop(id)]
                }
                Some(s) => {
                    self.status = format!("{} is {}; nothing to stop.", s.id, s.state);
                    Vec::new()
                }
                None => Vec::new(),
            },
            KeyCode::Char('r') => match self.selected_session() {
                Some(s) if s.state == SessionState::Stopped => {
                    let id = s.id.clone();
                    self.status = format!("Resuming {id}… (booting the guest)");
                    vec![Effect::Resume(id)]
                }
                Some(s) if s.state.is_up() => {
                    self.status = format!("{} is already up; Enter opens a shell.", s.id);
                    Vec::new()
                }
                Some(s) => {
                    self.status = format!("{} is {}; only stopped sessions resume.", s.id, s.state);
                    Vec::new()
                }
                None => Vec::new(),
            },
            KeyCode::Char('k') => match self.selected_session() {
                Some(s) => {
                    let keep = s.keep != KeepPolicy::Keep;
                    vec![Effect::Keep(s.id.clone(), keep)]
                }
                None => Vec::new(),
            },
            KeyCode::Char('g') => match self.selected_session() {
                Some(s) if s.state.is_up() || s.state == SessionState::Stopped => {
                    let id = s.id.clone();
                    self.status = format!("Opening a window for {id}… (closing it stops the VM)");
                    vec![Effect::OpenGui(id)]
                }
                Some(s) => {
                    self.status = format!("{} is {}; no VM to show.", s.id, s.state);
                    Vec::new()
                }
                None => Vec::new(),
            },
            KeyCode::Char('R') => {
                self.status = "Refreshing…".to_string();
                vec![Effect::LoadSessions]
            }
            KeyCode::Char('d') => {
                if let Some(s) = self.selected_session() {
                    let id = s.id.clone();
                    let kept = if s.keep == KeepPolicy::Keep {
                        " (it is marked keep)"
                    } else {
                        ""
                    };
                    self.status = format!("Delete {id}{kept}? [y/N]");
                    self.pending_delete = Some(id);
                }
                Vec::new()
            }
            KeyCode::Enter => match self.selected_session() {
                Some(s) if s.state.is_up() => vec![Effect::ExecSsh(s.id.clone())],
                Some(s) if s.state == SessionState::Stopped => {
                    self.status = format!("{} is stopped; press r to resume it first.", s.id);
                    Vec::new()
                }
                Some(s) => {
                    self.status = format!("{} is {}; nothing to connect to.", s.id, s.state);
                    Vec::new()
                }
                None => Vec::new(),
            },
            KeyCode::Esc => {
                self.screen = Screen::Search;
                self.status.clear();
                Vec::new()
            }
            KeyCode::Char('q') => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.sessions_selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wargame_core::domain::{ChallengeSummary, ExecutionMode, GuestConnection, Resources};

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn choice(name: &str, guest: &str, backend: &str, image: &str) -> ProfileChoice {
        ProfileChoice {
            name: name.to_string(),
            guest: guest.to_string(),
            backend: backend.to_string(),
            image: image.to_string(),
        }
    }

    fn app() -> App {
        App::new(
            "macos-minimal-arm64".to_string(),
            NetworkMode::Isolated,
            vec![
                choice(
                    "macos-minimal-arm64",
                    "macos/arm64",
                    "tart/virtualize",
                    "ready",
                ),
                choice("macos-re-arm64", "macos/arm64", "tart/virtualize", "ready"),
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
            guest_distribution: None,
            vm_mode: ExecutionMode::Virtualize,
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
            connection: GuestConnection::default(),
            workspace_root: "/Users/admin/wargame".to_string(),
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
    fn ctrl_c_quits_even_while_editing() {
        let mut a = app();
        assert!(a.search.editing);
        let effects = a.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(effects.as_slice(), [Effect::Quit]));
    }

    #[test]
    fn bracket_keys_page_through_results() {
        let mut a = app();
        a.update(key(KeyCode::Enter)); // search page 1
        assert!(a.status.starts_with("Searching"));
        a.update(Msg::SearchResults(SearchPage {
            items: vec![summary("11", "out_of_bound")],
            next_page: Some(2),
            total: 40,
        }));
        assert_eq!(a.status, "40 result(s), page 1");

        let effects = a.update(key(KeyCode::Char(']')));
        match effects.as_slice() {
            [Effect::Search(q)] => assert_eq!(q.page, 2),
            other => panic!("expected a page-2 search, got {other:?}"),
        }
        a.update(Msg::SearchResults(SearchPage {
            items: vec![summary("12", "second")],
            next_page: None,
            total: 40,
        }));
        // No next page: `]` is a no-op; `[` goes back.
        assert!(a.update(key(KeyCode::Char(']'))).is_empty());
        match a.update(key(KeyCode::Char('['))).as_slice() {
            [Effect::Search(q)] => assert_eq!(q.page, 1),
            other => panic!("expected a page-1 search, got {other:?}"),
        }
    }

    #[test]
    fn enter_launches_from_the_problem_screen() {
        let mut a = app();
        a.update(Msg::Challenge(challenge("11", "out_of_bound")));
        let effects = a.update(key(KeyCode::Enter));
        assert!(matches!(effects.as_slice(), [Effect::Launch(_)]));
        assert_eq!(a.screen, Screen::Launching);
    }

    #[test]
    fn launching_screen_offers_sessions_after_a_failure() {
        let mut a = app();
        a.update(Msg::Launch(LaunchEvent::Failed {
            session_id: Some("dh-x-1".to_string()),
            error: "boom".to_string(),
            hint: None,
        }));
        assert!(a.help_text().contains("b: sessions"));
        assert!(!a.help_text().contains("ssh"));
        let effects = a.update(key(KeyCode::Char('b')));
        assert!(matches!(effects.as_slice(), [Effect::LoadSessions]));
        assert_eq!(a.screen, Screen::Sessions);
    }

    #[test]
    fn sessions_screen_keys_respect_state() {
        let mut a = app();
        let mut stopped = sample_session("dh-stopped");
        stopped.state = SessionState::Stopped;
        a.update(Msg::Sessions(vec![sample_session("dh-ready"), stopped]));
        a.screen = Screen::Sessions;

        // Ready: Enter -> ssh, x -> stop, r -> explains.
        assert!(
            matches!(a.update(key(KeyCode::Enter)).as_slice(), [Effect::ExecSsh(id)] if id == "dh-ready")
        );
        assert!(
            matches!(a.update(key(KeyCode::Char('x'))).as_slice(), [Effect::Stop(id)] if id == "dh-ready")
        );
        assert!(a.update(key(KeyCode::Char('r'))).is_empty());
        assert!(a.status.contains("already up"));

        // Stopped: Enter explains, r -> resume, x -> explains.
        a.update(key(KeyCode::Down));
        assert!(a.update(key(KeyCode::Enter)).is_empty());
        assert!(a.status.contains("press r"));
        assert!(
            matches!(a.update(key(KeyCode::Char('r'))).as_slice(), [Effect::Resume(id)] if id == "dh-stopped")
        );
        assert!(a.update(key(KeyCode::Char('x'))).is_empty());

        // k toggles keep; R refreshes.
        assert!(
            matches!(a.update(key(KeyCode::Char('k'))).as_slice(), [Effect::Keep(id, true)] if id == "dh-stopped")
        );
        assert!(matches!(
            a.update(key(KeyCode::Char('R'))).as_slice(),
            [Effect::LoadSessions]
        ));
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
