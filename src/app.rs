//! Application state and input handling.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use ratatui::widgets::ListState;

use crate::{
    input::Input,
    pio::{self, Board, JobMsg, Library, ProjectSpec, StepState, Summary},
};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Wizard,
    Compiledb,
    Running,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Step {
    Project,
    Platform,
    Board,
    Framework,
    Libraries,
    Options,
    Review,
}

impl Step {
    pub const ALL: [Step; 7] = [
        Step::Project,
        Step::Platform,
        Step::Board,
        Step::Framework,
        Step::Libraries,
        Step::Options,
        Step::Review,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Step::Project => "Project",
            Step::Platform => "Platform",
            Step::Board => "Board",
            Step::Framework => "Framework",
            Step::Libraries => "Libraries",
            Step::Options => "Options",
            Step::Review => "Review",
        }
    }

    fn prev(self) -> Option<Step> {
        let i = Self::ALL.iter().position(|s| *s == self)?;
        i.checked_sub(1).map(|i| Self::ALL[i])
    }
}

pub struct PlatformInfo {
    pub name: String,
    pub boards: usize,
    pub installed: bool,
    pub frameworks: Vec<(String, usize)>,
}

pub const HOME_ITEMS: [(&str, &str, &str); 4] = [
    ("✚", "New project", "Scaffold a PlatformIO project — pick a platform, board, framework and libraries."),
    ("⚙", "Generate compile_commands.json", "Build a clangd compilation database for an existing project."),
    ("↻", "Refresh board catalogue", "Re-query `pio boards` and update the local cache."),
    ("⏻", "Quit", "See you next build."),
];

pub const OPTION_ROWS: usize = 6;

pub struct RunState {
    pub title: String,
    pub steps: Vec<(String, StepState)>,
    pub log: Vec<String>,
    rx: Receiver<JobMsg>,
    pub started: Instant,
    pub finished: Option<(Result<Summary, String>, Duration)>,
    pub scroll: usize,
}

type LibResult = (u64, Result<(Vec<Library>, u64), String>);

pub struct App {
    pub screen: Screen,
    pub step: Step,
    pub quit: bool,
    pub exit_note: Option<String>,
    pub started: Instant,
    pub toast: Option<(String, Instant)>,

    pub home: ListState,

    // board catalogue
    pub boards: Arc<Vec<Board>>,
    pub boards_loading: bool,
    pub boards_err: Option<String>,
    boards_rx: Option<Receiver<Result<Vec<Board>>>>,
    pub installed: HashSet<String>,
    pub platforms: Vec<PlatformInfo>,

    // step: project
    pub name: Input,
    pub dir: Input,
    pub proj_focus: usize,

    // step: platform
    pub plat_filter: Input,
    pub plat_view: Vec<Option<usize>>,
    pub plat_state: ListState,
    pub platform: Option<String>,

    // step: board
    pub board_filter: Input,
    pub board_view: Vec<usize>,
    pub board_state: ListState,
    pub board: Option<usize>,

    // step: framework
    pub fw_state: ListState,
    pub framework: Option<String>,

    // step: libraries
    pub lib_query: Input,
    pub lib_results: Vec<Library>,
    pub lib_total: u64,
    pub lib_state: ListState,
    pub lib_sel_state: ListState,
    pub libs: Vec<Library>,
    pub lib_focus_selected: bool,
    pub lib_loading: bool,
    pub lib_err: Option<String>,
    pub lib_platform_only: bool,
    lib_seq: u64,
    lib_due: Option<Instant>,
    lib_for: Option<(String, Option<String>)>,
    lib_tx: Sender<LibResult>,
    lib_rx: Receiver<LibResult>,

    // step: options
    pub monitor_speed: Input,
    pub upload_port: Input,
    pub opt_compiledb: bool,
    pub opt_clangd: bool,
    pub opt_template: bool,
    pub opt_git: bool,
    pub opt_sel: usize,

    // compiledb screen
    pub cdb_dir: Input,
    pub cdb_envs: Option<Vec<String>>,
    pub cdb_state: ListState,
    pub cdb_clangd: bool,

    pub run: Option<RunState>,
}

impl App {
    pub fn new(start_wizard: bool, refresh: bool) -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        let (lib_tx, lib_rx) = mpsc::channel();
        let mut app = Self {
            screen: if start_wizard { Screen::Wizard } else { Screen::Home },
            step: Step::Project,
            quit: false,
            exit_note: None,
            started: Instant::now(),
            toast: None,
            home: ListState::default().with_selected(Some(0)),
            boards: Arc::new(Vec::new()),
            boards_loading: false,
            boards_err: None,
            boards_rx: None,
            installed: pio::installed_platforms(),
            platforms: Vec::new(),
            name: Input::default(),
            dir: Input::new(&pio::contract_tilde(&cwd)),
            proj_focus: 0,
            plat_filter: Input::default(),
            plat_view: Vec::new(),
            plat_state: ListState::default(),
            platform: None,
            board_filter: Input::default(),
            board_view: Vec::new(),
            board_state: ListState::default(),
            board: None,
            fw_state: ListState::default().with_selected(Some(0)),
            framework: None,
            lib_query: Input::default(),
            lib_results: Vec::new(),
            lib_total: 0,
            lib_state: ListState::default(),
            lib_sel_state: ListState::default(),
            libs: Vec::new(),
            lib_focus_selected: false,
            lib_loading: false,
            lib_err: None,
            lib_platform_only: true,
            lib_seq: 0,
            lib_due: None,
            lib_for: None,
            lib_tx,
            lib_rx,
            monitor_speed: Input::new("115200"),
            upload_port: Input::default(),
            opt_compiledb: true,
            opt_clangd: true,
            opt_template: true,
            opt_git: true,
            opt_sel: 0,
            cdb_dir: Input::new(&pio::contract_tilde(&cwd)),
            cdb_envs: None,
            cdb_state: ListState::default().with_selected(Some(0)),
            cdb_clangd: true,
            run: None,
        };
        app.load_boards(refresh);
        app.refresh_cdb_envs();
        app
    }

    pub fn spinner(&self) -> &'static str {
        let i = (self.started.elapsed().as_millis() / 80) as usize;
        crate::theme::SPINNER[i % crate::theme::SPINNER.len()]
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    // ───────────────────────── background work ─────────────────────────

    fn load_boards(&mut self, refresh: bool) {
        let (tx, rx) = mpsc::channel();
        self.boards_rx = Some(rx);
        self.boards_loading = true;
        self.boards_err = None;
        thread::spawn(move || {
            let _ = tx.send(pio::load_boards(refresh));
        });
    }

    pub fn tick(&mut self) {
        if let Some(rx) = &self.boards_rx {
            if let Ok(res) = rx.try_recv() {
                self.boards_rx = None;
                self.boards_loading = false;
                match res {
                    Ok(b) => {
                        let n = b.len();
                        self.boards = Arc::new(b);
                        self.installed = pio::installed_platforms();
                        self.build_platforms();
                        self.refilter_platforms();
                        self.refilter_boards();
                        self.toast(format!("Board catalogue ready — {n} boards"));
                    }
                    Err(e) => self.boards_err = Some(format!("{e:#}")),
                }
            }
        }

        if self.lib_due.is_some_and(|t| t.elapsed() >= SEARCH_DEBOUNCE) {
            self.lib_due = None;
            self.search_libs();
        }
        while let Ok((seq, res)) = self.lib_rx.try_recv() {
            if seq != self.lib_seq {
                continue;
            }
            self.lib_loading = false;
            match res {
                Ok((items, total)) => {
                    self.lib_err = None;
                    self.lib_results = items;
                    self.lib_total = total;
                    self.lib_state.select((!self.lib_results.is_empty()).then_some(0));
                }
                Err(e) => self.lib_err = Some(e),
            }
        }

        if let Some(run) = &mut self.run {
            while let Ok(msg) = run.rx.try_recv() {
                match msg {
                    JobMsg::Steps(s) => run.steps = s.into_iter().map(|n| (n, StepState::Pending)).collect(),
                    JobMsg::Step(i, st) => {
                        if let Some(s) = run.steps.get_mut(i) {
                            s.1 = st;
                        }
                    }
                    JobMsg::Log(l) => {
                        run.log.push(l);
                        if run.scroll > 0 {
                            run.scroll += 1;
                        }
                    }
                    JobMsg::Done(r) => run.finished = Some((r, run.started.elapsed())),
                }
            }
        }

        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(3)) {
            self.toast = None;
        }
    }

    fn search_libs(&mut self) {
        self.lib_seq += 1;
        let seq = self.lib_seq;
        let q = self.lib_query.value();
        let platform = self.lib_platform_only.then(|| self.effective_platform()).flatten();
        self.lib_for = Some((q.clone(), platform.clone()));
        self.lib_loading = true;
        let tx = self.lib_tx.clone();
        thread::spawn(move || {
            let r = pio::search_libraries(&q, platform.as_deref()).map_err(|e| format!("{e:#}"));
            let _ = tx.send((seq, r));
        });
    }

    fn start_job(&mut self, title: &str, spawn: impl FnOnce(Sender<JobMsg>)) {
        let (tx, rx) = mpsc::channel();
        spawn(tx);
        self.run = Some(RunState {
            title: title.into(),
            steps: Vec::new(),
            log: Vec::new(),
            rx,
            started: Instant::now(),
            finished: None,
            scroll: 0,
        });
        self.screen = Screen::Running;
    }

    // ───────────────────────── derived data ─────────────────────────

    fn build_platforms(&mut self) {
        let mut map: HashMap<&str, (usize, HashMap<&str, usize>)> = HashMap::new();
        for b in self.boards.iter() {
            let e = map.entry(&b.platform).or_default();
            e.0 += 1;
            for f in &b.frameworks {
                *e.1.entry(f).or_default() += 1;
            }
        }
        let mut v: Vec<PlatformInfo> = map
            .into_iter()
            .map(|(name, (boards, fws))| {
                let mut frameworks: Vec<(String, usize)> = fws.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
                frameworks.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                PlatformInfo { installed: self.installed.contains(name), name: name.to_owned(), boards, frameworks }
            })
            .collect();
        v.sort_by(|a, b| b.installed.cmp(&a.installed).then_with(|| b.boards.cmp(&a.boards)));
        self.platforms = v;
    }

    fn refilter_platforms(&mut self) {
        let q = self.plat_filter.value();
        let q = q.trim();
        self.plat_view = if q.is_empty() {
            std::iter::once(None).chain((0..self.platforms.len()).map(Some)).collect()
        } else {
            let m = SkimMatcherV2::default();
            let mut scored: Vec<(i64, usize)> = self
                .platforms
                .iter()
                .enumerate()
                .filter_map(|(i, p)| m.fuzzy_match(&p.name, q).map(|s| (s, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().map(|(_, i)| Some(i)).collect()
        };
        let pos = self
            .plat_view
            .iter()
            .position(|p| p.map(|i| &self.platforms[i].name) == self.platform.as_ref())
            .filter(|_| q.is_empty())
            .unwrap_or(0);
        self.plat_state.select((!self.plat_view.is_empty()).then_some(pos));
    }

    fn refilter_boards(&mut self) {
        let q = self.board_filter.value();
        let q = q.trim();
        let m = SkimMatcherV2::default();
        let mut scored: Vec<(i64, usize)> = self
            .boards
            .iter()
            .enumerate()
            .filter(|(_, b)| self.platform.as_ref().is_none_or(|p| &b.platform == p))
            .filter_map(|(i, b)| {
                if q.is_empty() {
                    return Some((0, i));
                }
                let bonus = if b.id.eq_ignore_ascii_case(q) {
                    10_000
                } else if b.id.to_lowercase().starts_with(&q.to_lowercase()) {
                    1_000
                } else {
                    0
                };
                m.fuzzy_match(&b.haystack(), q).map(|s| (s + bonus, i))
            })
            .collect();
        if !q.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0));
        }
        self.board_view = scored.into_iter().map(|(_, i)| i).collect();
        let pos = if q.is_empty() {
            self.board.and_then(|b| self.board_view.iter().position(|&i| i == b)).unwrap_or(0)
        } else {
            0
        };
        self.board_state.select((!self.board_view.is_empty()).then_some(pos));
    }

    pub fn current_board(&self) -> Option<&Board> {
        self.board.and_then(|i| self.boards.get(i))
    }

    pub fn highlighted_board(&self) -> Option<&Board> {
        self.board_state.selected().and_then(|i| self.board_view.get(i)).and_then(|&i| self.boards.get(i))
    }

    pub fn effective_platform(&self) -> Option<String> {
        self.current_board().map(|b| b.platform.clone()).or_else(|| self.platform.clone())
    }

    /// Framework choices for the selected board (`None` = no framework).
    pub fn framework_choices(&self) -> Vec<Option<String>> {
        match self.current_board() {
            Some(b) if !b.frameworks.is_empty() => b.frameworks.iter().cloned().map(Some).collect(),
            _ => vec![None],
        }
    }

    pub fn project_path(&self) -> PathBuf {
        pio::expand_tilde(&self.dir.value()).join(self.name.value().trim())
    }

    pub fn validate_project(&self) -> Result<(), String> {
        let name = self.name.value();
        let name = name.trim();
        if name.is_empty() {
            return Err("Give your project a name".into());
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) || name.starts_with('.') {
            return Err("Use letters, digits, '-', '_' or '.' only".into());
        }
        let parent = pio::expand_tilde(&self.dir.value());
        if !parent.is_dir() {
            return Err(format!("Location {} does not exist", parent.display()));
        }
        let path = parent.join(name);
        if path.exists() {
            let empty = path.read_dir().map(|mut d| d.next().is_none()).unwrap_or(false);
            if !empty {
                return Err("A non-empty directory with that name already exists".into());
            }
        }
        Ok(())
    }

    pub fn spec(&self) -> Option<ProjectSpec> {
        Some(ProjectSpec {
            name: self.name.value().trim().to_owned(),
            parent: pio::expand_tilde(&self.dir.value()),
            board: self.current_board()?.clone(),
            framework: self.framework.clone(),
            libs: self.libs.iter().map(Library::spec).collect(),
            monitor_speed: self.monitor_speed.value(),
            upload_port: self.upload_port.value(),
            compiledb: self.opt_compiledb,
            clangd: self.opt_clangd,
            template: self.opt_template,
            git: self.opt_git,
        })
    }

    fn refresh_cdb_envs(&mut self) {
        self.cdb_envs = pio::parse_envs(&pio::expand_tilde(&self.cdb_dir.value()));
        self.cdb_state.select(Some(0));
    }

    fn reset_wizard(&mut self) {
        self.name.clear();
        self.libs.clear();
        self.lib_sel_state.select(None);
        self.lib_focus_selected = false;
        self.step = Step::Project;
        self.proj_focus = 0;
    }

    // ───────────────────────── input ─────────────────────────

    pub fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.screen {
            Screen::Home => self.key_home(k),
            Screen::Wizard => self.key_wizard(k),
            Screen::Compiledb => self.key_cdb(k),
            Screen::Running => self.key_running(k),
        }
    }

    fn key_home(&mut self, k: KeyEvent) {
        let n = HOME_ITEMS.len();
        let sel = self.home.selected().unwrap_or(0);
        let activate = match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.home.select(Some((sel + n - 1) % n));
                None
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.home.select(Some((sel + 1) % n));
                None
            }
            KeyCode::Enter | KeyCode::Char(' ') => Some(sel),
            KeyCode::Char('n') => Some(0),
            KeyCode::Char('c') => Some(1),
            KeyCode::Char('r') => Some(2),
            KeyCode::Char('q') | KeyCode::Esc => Some(3),
            _ => None,
        };
        match activate {
            Some(0) => {
                self.screen = Screen::Wizard;
                self.step = Step::Project;
            }
            Some(1) => {
                self.refresh_cdb_envs();
                self.screen = Screen::Compiledb;
            }
            Some(2) => {
                if !self.boards_loading {
                    self.load_boards(true);
                }
            }
            Some(3) => self.quit = true,
            _ => {}
        }
    }

    fn go_back(&mut self) {
        match self.step.prev() {
            Some(s) => self.step = s,
            None => self.screen = Screen::Home,
        }
    }

    fn key_wizard(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('n') {
            return self.confirm();
        }
        if ctrl && k.code == KeyCode::Char('b') {
            return self.go_back();
        }
        match self.step {
            Step::Project => match k.code {
                KeyCode::Esc => self.go_back(),
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => self.proj_focus ^= 1,
                KeyCode::Enter => self.confirm(),
                _ => {
                    let input = if self.proj_focus == 0 { &mut self.name } else { &mut self.dir };
                    input.handle(&k);
                }
            },
            Step::Platform => {
                let len = self.plat_view.len();
                match k.code {
                    KeyCode::Esc => self.go_back(),
                    KeyCode::Enter => self.confirm(),
                    _ if nav(&mut self.plat_state, len, &k) => {}
                    _ => {
                        if self.plat_filter.handle(&k) {
                            self.refilter_platforms();
                        }
                    }
                }
            }
            Step::Board => {
                let len = self.board_view.len();
                match k.code {
                    KeyCode::Esc => self.go_back(),
                    KeyCode::Enter => self.confirm(),
                    _ if nav(&mut self.board_state, len, &k) => {}
                    _ => {
                        if self.board_filter.handle(&k) {
                            self.refilter_boards();
                        }
                    }
                }
            }
            Step::Framework => {
                let len = self.framework_choices().len();
                match k.code {
                    KeyCode::Esc => self.go_back(),
                    KeyCode::Enter | KeyCode::Char(' ') => self.confirm(),
                    KeyCode::Char('k') => nav_by(&mut self.fw_state, len, -1),
                    KeyCode::Char('j') => nav_by(&mut self.fw_state, len, 1),
                    _ => {
                        nav(&mut self.fw_state, len, &k);
                    }
                }
            }
            Step::Libraries => self.key_libs(k),
            Step::Options => self.key_options(k),
            Step::Review => match k.code {
                KeyCode::Esc => self.go_back(),
                KeyCode::Enter => self.confirm(),
                _ => {}
            },
        }
    }

    fn key_libs(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Esc {
            if self.lib_focus_selected {
                self.lib_focus_selected = false;
            } else {
                self.go_back();
            }
            return;
        }
        if ctrl && k.code == KeyCode::Char('f') {
            self.lib_platform_only = !self.lib_platform_only;
            self.search_libs();
            return;
        }
        if k.code == KeyCode::Tab || k.code == KeyCode::BackTab {
            self.lib_focus_selected = !self.lib_focus_selected && !self.libs.is_empty();
            if self.lib_focus_selected && self.lib_sel_state.selected().is_none() {
                self.lib_sel_state.select(Some(0));
            }
            return;
        }

        if self.lib_focus_selected {
            let len = self.libs.len();
            match k.code {
                KeyCode::Enter | KeyCode::Delete | KeyCode::Backspace | KeyCode::Char(' ' | 'd' | 'x') => {
                    if let Some(i) = self.lib_sel_state.selected().filter(|&i| i < len) {
                        let lib = self.libs.remove(i);
                        self.toast(format!("Removed {}", lib.key()));
                        if self.libs.is_empty() {
                            self.lib_focus_selected = false;
                            self.lib_sel_state.select(None);
                        } else {
                            self.lib_sel_state.select(Some(i.min(self.libs.len() - 1)));
                        }
                    }
                }
                KeyCode::Char('k') => nav_by(&mut self.lib_sel_state, len, -1),
                KeyCode::Char('j') => nav_by(&mut self.lib_sel_state, len, 1),
                _ => {
                    nav(&mut self.lib_sel_state, len, &k);
                }
            }
            return;
        }

        let len = self.lib_results.len();
        match k.code {
            KeyCode::Enter => {
                if let Some(lib) = self.lib_state.selected().and_then(|i| self.lib_results.get(i)).cloned() {
                    if let Some(pos) = self.libs.iter().position(|l| l.key() == lib.key()) {
                        self.libs.remove(pos);
                        self.toast(format!("Removed {}", lib.key()));
                    } else {
                        self.toast(format!("Added {}", lib.spec()));
                        self.libs.push(lib);
                    }
                }
            }
            _ if nav(&mut self.lib_state, len, &k) => {}
            _ => {
                if self.lib_query.handle(&k) {
                    self.lib_due = Some(Instant::now());
                }
            }
        }
    }

    fn key_options(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.go_back(),
            KeyCode::Enter => self.confirm(),
            KeyCode::Up | KeyCode::BackTab => self.opt_sel = (self.opt_sel + OPTION_ROWS - 1) % OPTION_ROWS,
            KeyCode::Down | KeyCode::Tab => self.opt_sel = (self.opt_sel + 1) % OPTION_ROWS,
            _ => match self.opt_sel {
                0 => {
                    if !matches!(k.code, KeyCode::Char(c) if !c.is_ascii_digit()) {
                        self.monitor_speed.handle(&k);
                    }
                }
                1 => {
                    self.upload_port.handle(&k);
                }
                n if matches!(k.code, KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right) => {
                    let flag = match n {
                        2 => &mut self.opt_compiledb,
                        3 => &mut self.opt_clangd,
                        4 => &mut self.opt_template,
                        _ => &mut self.opt_git,
                    };
                    *flag = !*flag;
                }
                _ => {}
            },
        }
    }

    /// "Continue" for the current wizard step.
    fn confirm(&mut self) {
        match self.step {
            Step::Project => match self.validate_project() {
                Ok(()) => {
                    self.step = Step::Platform;
                    self.refilter_platforms();
                }
                Err(e) => self.toast(e),
            },
            Step::Platform => {
                if self.boards.is_empty() {
                    return self.toast("Board catalogue is still loading…");
                }
                let Some(choice) = self.plat_state.selected().and_then(|i| self.plat_view.get(i)).copied() else {
                    return;
                };
                let new = choice.map(|i| self.platforms[i].name.clone());
                if new != self.platform {
                    self.platform = new;
                    self.board_filter.clear();
                }
                self.refilter_boards();
                self.step = Step::Board;
            }
            Step::Board => {
                let Some(&idx) = self.board_state.selected().and_then(|i| self.board_view.get(i)) else {
                    return;
                };
                self.board = Some(idx);
                let choices = self.framework_choices();
                let pos = choices.iter().position(|c| *c == self.framework).unwrap_or(0);
                self.fw_state.select(Some(pos));
                self.step = Step::Framework;
            }
            Step::Framework => {
                let choices = self.framework_choices();
                self.framework = self.fw_state.selected().and_then(|i| choices.get(i).cloned()).flatten();
                self.step = Step::Libraries;
                let want = (self.lib_query.value(), self.lib_platform_only.then(|| self.effective_platform()).flatten());
                if self.lib_for.as_ref() != Some(&want) {
                    self.search_libs();
                }
            }
            Step::Libraries => self.step = Step::Options,
            Step::Options => self.step = Step::Review,
            Step::Review => {
                if let Err(e) = self.validate_project() {
                    self.step = Step::Project;
                    return self.toast(e);
                }
                if let Some(spec) = self.spec() {
                    self.start_job("Creating project", |tx| pio::spawn_create(spec, tx));
                }
            }
        }
    }

    fn key_cdb(&mut self, k: KeyEvent) {
        let len = self.cdb_envs.as_ref().map_or(0, |e| e.len() + 1);
        match k.code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Tab => self.cdb_clangd = !self.cdb_clangd,
            KeyCode::Enter => {
                let Some(envs) = &self.cdb_envs else {
                    return self.toast("No platformio.ini in that directory");
                };
                let env = self.cdb_state.selected().filter(|&i| i > 0).and_then(|i| envs.get(i - 1).cloned());
                let dir = pio::expand_tilde(&self.cdb_dir.value());
                let dir = dir.canonicalize().unwrap_or(dir);
                let clangd = self.cdb_clangd;
                self.start_job("Compilation database", |tx| pio::spawn_compiledb(dir, env, clangd, tx));
            }
            _ if nav(&mut self.cdb_state, len, &k) => {}
            _ => {
                if self.cdb_dir.handle(&k) {
                    self.refresh_cdb_envs();
                }
            }
        }
    }

    fn key_running(&mut self, k: KeyEvent) {
        let Some(run) = &mut self.run else { return };
        let done = run.finished.is_some();
        let max = run.log.len();
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => run.scroll = (run.scroll + 1).min(max),
            KeyCode::Down | KeyCode::Char('j') => run.scroll = run.scroll.saturating_sub(1),
            KeyCode::PageUp => run.scroll = (run.scroll + 15).min(max),
            KeyCode::PageDown => run.scroll = run.scroll.saturating_sub(15),
            KeyCode::End | KeyCode::Char('G') => run.scroll = 0,
            KeyCode::Home | KeyCode::Char('g') => run.scroll = max,
            KeyCode::Enter | KeyCode::Char('q') if done => {
                if let Some((Ok(s), _)) = &run.finished {
                    self.exit_note = Some(format!("cd {}", shell_path(&s.project)));
                }
                self.quit = true;
            }
            KeyCode::Esc | KeyCode::Char('h') if done => {
                let ok = matches!(run.finished, Some((Ok(_), _)));
                self.run = None;
                self.installed = pio::installed_platforms();
                self.build_platforms();
                self.refilter_platforms();
                if ok {
                    self.reset_wizard();
                    self.screen = Screen::Home;
                } else {
                    self.screen = if self.step == Step::Review { Screen::Wizard } else { Screen::Home };
                }
            }
            _ => {}
        }
    }
}

fn shell_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if s.contains(char::is_whitespace) { format!("\"{s}\"") } else { s }
}

fn nav_by(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let cur = state.selected().unwrap_or(0) as isize;
    state.select(Some((cur + delta).clamp(0, len as isize - 1) as usize));
}

/// Shared list navigation keys. Returns true when the key was consumed.
fn nav(state: &mut ListState, len: usize, k: &KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Up => nav_by(state, len, -1),
        KeyCode::Down => nav_by(state, len, 1),
        KeyCode::Char('p') if ctrl => nav_by(state, len, -1),
        KeyCode::Char('n') if ctrl => nav_by(state, len, 1),
        KeyCode::PageUp => nav_by(state, len, -10),
        KeyCode::PageDown => nav_by(state, len, 10),
        _ => return false,
    }
    true
}
