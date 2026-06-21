//! Application state and the pure update/transition layer.
//!
//! `App` holds all UI state; the render functions in `ui::` read it, and the
//! event loop in `main` feeds input + ticks to it. Logic here is kept pure and
//! testable (no terminal I/O); side-effecting collaborators (DB, sidecar, jobs)
//! are held as owned handles and driven from the update methods.
//!
//! REDESIGN (see the locked spec): navigation is a SCREEN STACK (no hotkey
//! jumps) with menu-driven push and Esc/Backspace pop; long actions (scrape,
//! train) run on BACKGROUND threads streaming into a channel that `on_tick`
//! drains, so the loop never blocks; the home screen plays a one-shot intro and
//! shows a 4-item menu.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::Config;
use crate::db::Db;
use crate::jobs::{self, JobKind, JobStatus, RunningJob};
use crate::models::{
    Division, DbSummary, EligibilityRules, EventRow, FightRow, Fighter, LatestCard, PredictResult,
    RoundStat,
};
use std::collections::HashMap;
use crate::scraper::ScrapeOptions;
use crate::sidecar::Sidecar;
use crate::{fuzzy, stats_text};

// =========================================================================== //
// SCREEN STACK
// =========================================================================== //

/// One screen in the navigation stack. The current screen is the LAST element of
/// `App.nav`; pushing navigates deeper, popping goes back. `Home` is always the
/// bottom of the stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Landing screen: one-shot intro poster + the 4-item top menu.
    Home,
    /// Run the Go scraper (async) with toggleable options.
    Scrape,
    /// Database hub sub-menu: "Browse events" / "Find a fighter".
    Database,
    /// Browse events list (Database -> Browse events).
    Events,
    /// Fights on one event (Events -> select an event).
    EventFights { event_id: i64 },
    /// Live fuzzy fighter search (Database -> Find a fighter).
    FighterSearch,
    /// A single fighter's annotated career profile.
    Fighter { name: String },
    /// Pick two fighters and run a prediction via the sidecar.
    Predict,
    /// Model status / metrics + (async) train / reload actions.
    Model,
}

impl Screen {
    /// Human-readable title for the header.
    pub fn title(&self) -> String {
        match self {
            Screen::Home => "Home".to_string(),
            Screen::Scrape => "Scrape".to_string(),
            Screen::Database => "Database".to_string(),
            Screen::Events => "Events".to_string(),
            Screen::EventFights { .. } => "Fight card".to_string(),
            Screen::FighterSearch => "Find a fighter".to_string(),
            Screen::Fighter { name } => name.clone(),
            Screen::Predict => "Predict".to_string(),
            Screen::Model => "Model".to_string(),
        }
    }

    /// Contextual footer hint string for this screen (without job overlay).
    pub fn footer_hint(&self) -> &'static str {
        match self {
            Screen::Home => "↑↓ move · ⏎ select · q Quit",
            Screen::Database => "↑↓ move · ⏎ select · Esc Back · Home · q Quit",
            Screen::Scrape => {
                "f full · +/- limit · [/] rate · c clear · ⏎/r run · Esc Back · Home · q Quit"
            }
            Screen::Events => "↑↓ move · ⏎ open card · Esc Back · Home · q Quit",
            Screen::EventFights { .. } => "↑↓ move · ⏎ fighter · Esc Back · Home · q Quit",
            Screen::FighterSearch => "type to search · ↑↓ move · ⏎ open · Esc Back · Home · q Quit",
            Screen::Fighter { .. } => "Esc Back · Home · q Quit",
            Screen::Predict => "type · ↑↓ pick · ⏎ choose · ←→ slot · Esc Back · Home · q Quit",
            Screen::Model => "t train · l reload · r refresh · Esc Back · Home · q Quit",
        }
    }
}

// =========================================================================== //
// TOP MENUS (selection indices)
// =========================================================================== //

/// The 4 top-level menu options on Home, in display order.
pub const HOME_MENU: [(&str, &str); 4] = [
    ("Scrape", "refresh data/ufc.db with the Go scraper"),
    ("Database", "browse events & cards, or find a fighter"),
    ("Predict", "pick two fighters, get a win probability"),
    ("Model", "view metrics, train or reload the model"),
];

/// The 2 Database sub-menu paths, in display order.
pub const DATABASE_MENU: [(&str, &str); 2] = [
    ("Browse events", "events -> a card -> a fighter profile"),
    (
        "Find a fighter",
        "fuzzy search by name -> a fighter profile",
    ),
];

// =========================================================================== //
// PER-SCREEN STATE
// =========================================================================== //

/// State for the fuzzy fighter search screen.
#[derive(Debug, Clone, Default)]
pub struct FighterSearchState {
    /// Current search query.
    pub query: String,
    /// Names currently shown (post fuzzy filter), best-match-first.
    pub filtered: Vec<String>,
    /// Index of the highlighted row in `filtered`.
    pub selected: usize,
}

/// Loaded data for the fighter-profile screen (career stats + history).
#[derive(Debug, Clone, Default)]
pub struct FighterProfileState {
    /// The profile currently displayed, if loaded.
    pub profile: Option<Fighter>,
    /// Fight history for the displayed fighter.
    pub fights: Vec<FightRow>,
}

/// Which of the two predict slots currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PredictSlot {
    #[default]
    A,
    B,
}

/// State for the Predict screen.
#[derive(Debug, Clone, Default)]
pub struct PredictState {
    /// Slot currently being edited / picked.
    pub slot: PredictSlot,
    /// Chosen fighter A (None until picked).
    pub name_a: Option<String>,
    /// Chosen fighter B (None until picked).
    pub name_b: Option<String>,
    /// Active fuzzy query for the focused slot.
    pub query: String,
    /// The unfiltered candidate POOL for the focused slot: the full roster when
    /// the OTHER slot is empty, or the OTHER slot's eligible opponents (computed
    /// LOCALLY from the startup-fetched eligibility policy + divisions) when it
    /// holds a pick. The fuzzy query ranks over THIS pool only, so an ineligible
    /// opponent can never be selected.
    pub pool: Vec<String>,
    /// Candidate names for the focused slot (fuzzy-filtered `pool`).
    pub candidates: Vec<String>,
    /// Index of the highlighted candidate.
    pub selected: usize,
    /// Last prediction result, if any.
    pub result: Option<PredictResult>,
    /// Last error message (e.g. model not trained / unknown fighter).
    pub error: Option<String>,
}

/// State for the Events screen (browse path).
#[derive(Debug, Clone, Default)]
pub struct EventsState {
    /// All events, most-recent first.
    pub events: Vec<EventRow>,
    /// Index of the highlighted event.
    pub selected: usize,
}

/// State for the fights-on-one-event screen.
#[derive(Debug, Clone, Default)]
pub struct EventFightsState {
    /// Event whose card is shown (kept for the header).
    pub event: Option<EventRow>,
    /// Fights on the event, card order.
    pub fights: Vec<FightRow>,
    /// Index of the highlighted fight.
    pub selected: usize,
    /// Round stats for the highlighted fight (preview).
    pub rounds: Vec<RoundStat>,
}

/// State for the Scrape screen options (chips that update instantly).
#[derive(Debug, Clone, Default)]
pub struct ScrapeState {
    /// `--full` toggle for the next run.
    pub full: bool,
    /// `--limit` value for the next run (None = no limit).
    pub limit: Option<u32>,
    /// `--rate` value for the next run (None = scraper default).
    pub rate: Option<f64>,
}

/// State for the Model screen.
#[derive(Debug, Clone, Default)]
pub struct ModelState {
    /// Whether the sidecar reports a model loaded.
    pub model_loaded: bool,
    /// Roster size reported by the sidecar.
    pub n_fighters: i64,
    /// Free-form metrics JSON from the model payload.
    pub metrics: Option<serde_json::Value>,
    /// Resolved model path reported by the sidecar.
    pub model_path: Option<String>,
}

/// The eligibility POLICY + per-fighter division metadata, fetched ONCE from the
/// sidecar at startup (and refreshed after a reload). All matchup gating is then
/// applied LOCALLY against this cache — there are NO per-selection round-trips and
/// NO hardcoded policy values in Rust (every threshold/flag comes from `rules`).
#[derive(Debug, Clone, Default)]
pub struct EligibilityState {
    /// The policy shipped by Python (`None` until fetched / when no model).
    pub rules: Option<EligibilityRules>,
    /// `fighter name -> the (gender, ordinal) divisions they have fought in`.
    pub divisions: HashMap<String, Vec<Division>>,
}

impl EligibilityState {
    /// Sorted opponent names for `a` that the policy ALLOWS, excluding `a` itself.
    /// Computed purely from the cached `rules` + `divisions` (mirrors the sidecar's
    /// old `eligible` command, but with zero IPC). When no policy has been fetched
    /// yet, falls back to "every other roster name" so the screen stays usable;
    /// `predict()`'s server-side gate is the final safety net.
    pub fn eligible_opponents(&self, a: &str, roster: &[String]) -> Vec<String> {
        let Some(rules) = self.rules.as_ref() else {
            // No policy yet: don't over-filter — allow all but A.
            return roster.iter().filter(|n| n.as_str() != a).cloned().collect();
        };
        let empty: Vec<Division> = Vec::new();
        let divs_a = self.divisions.get(a).unwrap_or(&empty);
        let mut out: Vec<String> = roster
            .iter()
            .filter(|n| n.as_str() != a)
            .filter(|n| {
                let divs_b = self.divisions.get(n.as_str()).unwrap_or(&empty);
                crate::models::eligible(divs_a, divs_b, rules)
            })
            .cloned()
            .collect();
        out.sort();
        out
    }
}

// =========================================================================== //
// APP
// =========================================================================== //

/// The complete application state.
///
/// Owns the side-effecting collaborators (DB, sidecar) plus the nav stack, menu
/// selections, per-screen state, the running-job slot, and the animation frame
/// counter. `should_quit` is checked by the event loop each tick.
pub struct App {
    /// Resolved configuration.
    pub config: Config,
    /// Read-only DB handle.
    pub db: Db,
    /// Long-lived Python ML sidecar client (model loaded ONCE).
    pub sidecar: Sidecar,
    /// Set to true to exit the event loop.
    pub should_quit: bool,
    /// Transient status line shown at the bottom of the UI.
    pub status_line: Option<String>,

    /// Navigation stack; current screen = last element, always non-empty.
    pub nav: Vec<Screen>,
    /// Legacy tick counter: incremented once per `on_tick`. Retained for any
    /// per-tick housekeeping; the ANIMATION clock is time-based (see
    /// [`App::anim_frame`]) so motion speed is independent of the redraw rate.
    pub frame: usize,
    /// Animation epoch: the instant the app started. The integer animation frame
    /// index is derived from elapsed wall-clock time at [`anim::ANIM_FPS`]
    /// ([`App::anim_frame`]), so the intro + loading choreography play at a
    /// constant ~30 fps no matter how often the screen is redrawn.
    pub anim_start: std::time::Instant,
    /// The in-flight (or just-finished) background job, if any. While `Some` the
    /// active screen renders the loading overlay.
    pub job: Option<RunningJob>,

    /// Selection index on the Home menu (0..HOME_MENU.len()).
    pub home_selected: usize,
    /// Selection index on the Database sub-menu (0..DATABASE_MENU.len()).
    pub database_selected: usize,

    /// Cached DB-wide summary for the header / home poster.
    pub summary: Option<DbSummary>,
    /// Cached newest numbered UFC card for the home poster.
    pub latest_card: Option<LatestCard>,
    /// Cached roster (fighter names) from the sidecar / DB.
    pub roster: Vec<String>,
    /// Eligibility policy + per-fighter divisions, fetched ONCE at startup so the
    /// Predict screen filters opponents LOCALLY (no per-selection round-trips).
    pub eligibility: EligibilityState,

    pub search: FighterSearchState,
    pub fighter: FighterProfileState,
    pub predict: PredictState,
    pub events: EventsState,
    pub event_fights: EventFightsState,
    pub scrape: ScrapeState,
    pub model: ModelState,
}

impl App {
    /// Build the application: prime the DB summary + latest card, model status,
    /// and roster. Never fails over a not-yet-loaded sidecar / untrained model.
    pub fn new(config: Config, db: Db, sidecar: Sidecar) -> Result<App> {
        let mut app = App {
            config,
            db,
            sidecar,
            should_quit: false,
            status_line: None,
            nav: vec![Screen::Home],
            frame: 0,
            anim_start: std::time::Instant::now(),
            job: None,
            home_selected: 0,
            database_selected: 0,
            summary: None,
            latest_card: None,
            roster: Vec::new(),
            eligibility: EligibilityState::default(),
            search: FighterSearchState::default(),
            fighter: FighterProfileState::default(),
            predict: PredictState::default(),
            events: EventsState::default(),
            event_fights: EventFightsState::default(),
            scrape: ScrapeState::default(),
            model: ModelState::default(),
        };

        app.refresh_db_summary();
        app.refresh_model_status();
        Ok(app)
    }

    // ----------------------------------------------------------------------- //
    // Nav-stack primitives (PURE, unit-tested).
    // ----------------------------------------------------------------------- //

    /// The current (top-of-stack) screen.
    pub fn current(&self) -> &Screen {
        // `nav` is never empty (constructed with Home, pop keeps >= 1).
        self.nav.last().expect("nav stack is never empty")
    }

    /// Push `screen`, making it current, then load any data it needs.
    pub fn push(&mut self, screen: Screen) {
        self.nav.push(screen);
        self.on_enter_current();
    }

    /// Pop one level (Esc / Backspace). No-op at the root (Home stays). Returns
    /// true if a pop actually happened.
    pub fn pop(&mut self) -> bool {
        if self.nav.len() > 1 {
            self.nav.pop();
            self.on_enter_current();
            true
        } else {
            false
        }
    }

    /// Clear the stack back to Home (the Home key).
    pub fn go_home(&mut self) {
        self.nav.truncate(1); // keep the root Home
        self.home_selected = 0;
        self.on_enter_current();
    }

    /// Lazily load whatever the now-current screen needs to render.
    fn on_enter_current(&mut self) {
        match self.current().clone() {
            Screen::Database => self.database_selected = 0,
            Screen::Events => self.reload_events(),
            Screen::EventFights { event_id } => self.load_event_fights(event_id),
            Screen::FighterSearch => {
                if self.search.filtered.is_empty() {
                    self.reload_search_list();
                }
            }
            Screen::Fighter { name } => self.load_fighter_profile(&name),
            Screen::Predict => {
                if self.roster.is_empty() {
                    self.refresh_roster();
                }
                if self.eligibility.rules.is_none() {
                    self.refresh_eligibility();
                }
                self.refresh_predict_pool();
                self.recompute_predict_candidates();
            }
            Screen::Model => self.refresh_model_status(),
            _ => {}
        }
    }

    /// Whether the event loop should exit.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    // ----------------------------------------------------------------------- //
    // Sidecar / DB refreshes (side-effecting, tolerant of failure).
    // ----------------------------------------------------------------------- //

    /// Refresh the cached DB summary + latest-card poster headline.
    pub fn refresh_db_summary(&mut self) {
        match self.db.summary() {
            Ok(s) => self.summary = Some(s),
            Err(e) => self.status_line = Some(format!("DB summary failed: {e}")),
        }
        match self.db.latest_numbered_card() {
            Ok(c) => self.latest_card = c,
            Err(e) => self.status_line = Some(format!("Latest card lookup failed: {e}")),
        }
    }

    /// Query sidecar `status`, refresh `ModelState`, and (if loaded) the roster.
    pub fn refresh_model_status(&mut self) {
        match self.sidecar.status() {
            Ok(status) => {
                self.model.model_loaded = status.model_loaded;
                self.model.n_fighters = status.n_fighters;
                self.model.metrics = status.metrics;
                self.model.model_path = Some(status.model_path);
                if status.model_loaded {
                    self.refresh_roster();
                    self.refresh_eligibility();
                } else {
                    self.roster.clear();
                    self.eligibility = EligibilityState::default();
                }
            }
            Err(e) => {
                self.model.model_loaded = false;
                self.status_line = Some(format!("Sidecar status failed: {e}"));
            }
        }
    }

    /// Pull the fighter roster from the sidecar into `self.roster`.
    fn refresh_roster(&mut self) {
        match self.sidecar.roster() {
            Ok(names) => self.roster = names,
            Err(e) => self.status_line = Some(format!("Roster unavailable: {e}")),
        }
    }

    /// Fetch the eligibility POLICY (`rules`) + per-fighter divisions from the
    /// sidecar ONCE (called when a model is loaded). Thereafter the Predict screen
    /// filters opponents LOCALLY against this cache. Tolerant of failure: on error
    /// the cache is cleared so the local filter falls back to "all but the picked
    /// fighter" (predict()'s server-side gate is the final safety net).
    fn refresh_eligibility(&mut self) {
        match self.sidecar.eligibility() {
            Ok(payload) => {
                self.eligibility.rules = Some(payload.rules);
                self.eligibility.divisions = payload.divisions;
            }
            Err(e) => {
                self.eligibility = EligibilityState::default();
                self.status_line = Some(format!("Eligibility policy unavailable: {e}"));
            }
        }
    }

    /// Names to search over: the sidecar roster if present, else every DB name.
    fn fighter_name_pool(&self) -> Vec<String> {
        if !self.roster.is_empty() {
            return self.roster.clone();
        }
        match self.db.load_fighters() {
            Ok(fs) => fs.into_iter().map(|f| f.name).collect(),
            Err(_) => Vec::new(),
        }
    }

    // ======================================================================= //
    // INPUT DISPATCH
    // ======================================================================= //

    /// Handle one key event, mutating state and possibly triggering side
    /// effects. Implements the global nav model then per-screen keys.
    pub fn on_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }

        // While a job is on screen, keys are limited to dismiss / quit / Home so
        // the loading overlay can't be navigated away from mid-stream.
        if self.job.is_some() {
            return self.on_key_job(key);
        }

        // Global bindings shared by every screen.
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return Ok(());
            }
            KeyCode::Home => {
                self.go_home();
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Backspace => {
                // On text-entry screens, Backspace edits the query instead of
                // popping; those screens handle Backspace themselves below.
                let text_entry = matches!(self.current(), Screen::FighterSearch | Screen::Predict);
                if key.code == KeyCode::Backspace && text_entry {
                    // fall through to per-screen handling
                } else {
                    self.pop();
                    return Ok(());
                }
            }
            _ => {}
        }

        match self.current().clone() {
            Screen::Home => self.on_key_home(key),
            Screen::Database => self.on_key_database(key),
            Screen::Scrape => self.on_key_scrape(key),
            Screen::Events => self.on_key_events(key),
            Screen::EventFights { .. } => self.on_key_event_fights(key),
            Screen::FighterSearch => self.on_key_search(key),
            Screen::Fighter { .. } => Ok(()), // Esc/Home only (handled above)
            Screen::Predict => self.on_key_predict(key),
            Screen::Model => self.on_key_model(key),
        }
    }

    /// Keys while a job overlay is visible: dismiss the FINISHED log with
    /// Esc/Enter, quit with q, jump Home with Home. Running jobs ignore input.
    fn on_key_job(&mut self, key: KeyEvent) -> Result<()> {
        let finished = self
            .job
            .as_ref()
            .map(|j| j.status.is_finished())
            .unwrap_or(false);
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Home if finished => {
                self.job = None;
                self.go_home();
            }
            KeyCode::Esc | KeyCode::Enter if finished => {
                self.job = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// Periodic tick: advance the animation clock and drain the job channel,
    /// running post-completion actions. NEVER blocks.
    pub fn on_tick(&mut self) -> Result<()> {
        self.frame = self.frame.wrapping_add(1);

        if let Some(job) = self.job.as_mut()
            && let Some(success) = job.drain()
        {
            let kind = job.kind;
            self.on_job_complete(kind, success);
        }
        Ok(())
    }

    /// Run side effects after a job finishes: scrape -> refresh DB summary +
    /// sidecar reload; train -> sidecar reload. The finished log stays on screen
    /// (the job slot is cleared only when the user dismisses it).
    fn on_job_complete(&mut self, kind: JobKind, success: bool) {
        let verb = kind.label();
        if let Some(job) = self.job.as_mut() {
            if success {
                job.push_log(format!("$ {verb} finished OK"));
            } else {
                job.push_log(format!("$ {verb} exited with errors"));
            }
        }

        match kind {
            JobKind::Scrape => {
                self.refresh_db_summary();
                self.reload_sidecar_into_model();
            }
            JobKind::Train => {
                self.reload_sidecar_into_model();
            }
        }
        self.status_line = Some(format!(
            "{verb} {} — press Esc/Enter to dismiss.",
            if success { "complete" } else { "failed" }
        ));
    }

    /// Ask the sidecar to reload the on-disk model and fold the result into
    /// `ModelState`. Tolerant of failure (logged to the job, if any).
    fn reload_sidecar_into_model(&mut self) {
        match self.sidecar.reload() {
            Ok(rl) => {
                self.model.model_loaded = rl.model_loaded;
                self.model.n_fighters = rl.n_fighters;
                if rl.model_loaded {
                    self.refresh_roster();
                    self.refresh_eligibility();
                }
                self.refresh_model_status();
            }
            Err(e) => {
                if let Some(job) = self.job.as_mut() {
                    job.push_log(format!("$ sidecar reload failed: {e}"));
                }
            }
        }
    }

    // ======================================================================= //
    // HOME
    // ======================================================================= //

    fn on_key_home(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => {
                self.home_selected = self.home_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.home_selected + 1 < HOME_MENU.len() {
                    self.home_selected += 1;
                }
            }
            KeyCode::Enter => {
                let target = match self.home_selected {
                    0 => Screen::Scrape,
                    1 => Screen::Database,
                    2 => Screen::Predict,
                    _ => Screen::Model,
                };
                self.push(target);
            }
            _ => {}
        }
        Ok(())
    }

    // ======================================================================= //
    // DATABASE HUB
    // ======================================================================= //

    fn on_key_database(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => {
                self.database_selected = self.database_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.database_selected + 1 < DATABASE_MENU.len() {
                    self.database_selected += 1;
                }
            }
            KeyCode::Enter => {
                let target = if self.database_selected == 0 {
                    Screen::Events
                } else {
                    Screen::FighterSearch
                };
                self.push(target);
            }
            _ => {}
        }
        Ok(())
    }

    // ======================================================================= //
    // EVENTS  (Database -> Browse events)
    // ======================================================================= //

    fn on_key_events(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.events.select_prev(),
            KeyCode::Down => self.events.select_next(),
            KeyCode::Enter => {
                if let Some(ev) = self.events.selected_event() {
                    let id = ev.event_id;
                    self.push(Screen::EventFights { event_id: id });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn reload_events(&mut self) {
        match self.db.load_events() {
            Ok(evs) => self.events.events = evs,
            Err(e) => {
                self.events.events.clear();
                self.status_line = Some(format!("Events load failed: {e}"));
            }
        }
        self.events.clamp_selection();
    }

    // ======================================================================= //
    // EVENT FIGHTS  (Events -> select an event)
    // ======================================================================= //

    fn on_key_event_fights(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => {
                self.event_fights.select_prev();
                self.load_selected_fight_rounds();
            }
            KeyCode::Down => {
                self.event_fights.select_next();
                self.load_selected_fight_rounds();
            }
            KeyCode::Enter => {
                // Open the highlighted fight's winner as a fighter profile; the
                // screen agent may refine which side to open.
                if let Some(name) = self.event_fights.selected_fighter_name() {
                    let name = name.to_string();
                    self.push(Screen::Fighter { name });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn load_event_fights(&mut self, event_id: i64) {
        self.event_fights.event = self
            .events
            .events
            .iter()
            .find(|e| e.event_id == event_id)
            .cloned();
        match self.db.fights_for_event(event_id) {
            Ok(fs) => self.event_fights.fights = fs,
            Err(e) => {
                self.event_fights.fights.clear();
                self.status_line = Some(format!("Card load failed: {e}"));
            }
        }
        self.event_fights.selected = 0;
        self.event_fights.clamp_selection();
        self.load_selected_fight_rounds();
    }

    fn load_selected_fight_rounds(&mut self) {
        let Some(fr) = self.event_fights.fights.get(self.event_fights.selected) else {
            self.event_fights.rounds.clear();
            return;
        };
        match self.db.rounds_for_fight(fr.fight_id) {
            Ok(rs) => self.event_fights.rounds = rs,
            Err(_) => self.event_fights.rounds.clear(),
        }
    }

    // ======================================================================= //
    // FIGHTER SEARCH  (Database -> Find a fighter)
    // ======================================================================= //

    fn on_key_search(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.search.select_prev(),
            KeyCode::Down => self.search.select_next(),
            KeyCode::Enter => {
                if let Some(name) = self.search.selected_name() {
                    let name = name.to_string();
                    self.push(Screen::Fighter { name });
                }
            }
            KeyCode::Backspace => {
                self.search.query.pop();
                self.reload_search_list();
            }
            KeyCode::Char(c) => {
                self.search.query.push(c);
                self.reload_search_list();
            }
            _ => {}
        }
        Ok(())
    }

    /// Recompute the live fuzzy-filtered name list from the query.
    fn reload_search_list(&mut self) {
        let names = self.fighter_name_pool();
        self.search.filtered = fuzzy::rank(&names, &self.search.query);
        self.search.clamp_selection();
    }

    // ======================================================================= //
    // FIGHTER PROFILE
    // ======================================================================= //

    fn load_fighter_profile(&mut self, name: &str) {
        match self.db.fighter_profile(name) {
            Ok(p) => self.fighter.profile = p,
            Err(e) => {
                self.fighter.profile = None;
                self.status_line = Some(format!("Profile load failed: {e}"));
            }
        }
        match self.db.fights_for_fighter(name) {
            Ok(fs) => self.fighter.fights = fs,
            Err(e) => {
                self.fighter.fights.clear();
                self.status_line = Some(format!("Fight history failed: {e}"));
            }
        }
    }

    // ======================================================================= //
    // PREDICT
    // ======================================================================= //

    fn on_key_predict(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Left | KeyCode::Right => {
                self.predict.toggle_slot();
                // The focused slot changed, so its pool now depends on the OTHER
                // slot's pick — refresh it (sidecar) before re-ranking.
                self.refresh_predict_pool();
                self.recompute_predict_candidates();
            }
            KeyCode::Up => self.predict.select_prev(),
            KeyCode::Down => self.predict.select_next(),
            KeyCode::Enter => {
                if let Some(name) = self.predict.highlighted_candidate() {
                    let name = name.to_string();
                    self.predict.commit_slot(name.clone());
                    // The just-committed pick constrains the OTHER slot. If that
                    // slot holds a pick the new gate no longer allows, clear it so
                    // a stale ineligible pairing can never persist (re-pick case).
                    self.validate_other_slot(&name);
                    self.predict.advance_after_commit();
                    // Recompute the (now focused) slot's eligible pool + ranking.
                    self.refresh_predict_pool();
                    self.recompute_predict_candidates();
                    self.maybe_run_prediction();
                }
            }
            KeyCode::Backspace => {
                self.predict.query.pop();
                self.recompute_predict_candidates();
            }
            KeyCode::Char(c) => {
                self.predict.query.push(c);
                self.recompute_predict_candidates();
            }
            _ => {}
        }
        Ok(())
    }

    /// After committing `just_committed` in the focused slot, check the OTHER
    /// slot: if it holds a pick that the new policy no longer allows, clear it.
    ///
    /// Covers the re-pick case — both slots filled, then the user changes one to
    /// a fighter that makes the existing opponent ineligible. The gate is
    /// symmetric, so the other slot is valid iff it appears in the LOCAL
    /// `eligible_opponents(just_committed)` (computed from the startup-fetched
    /// policy + divisions — no IPC).
    fn validate_other_slot(&mut self, just_committed: &str) {
        let other = match self.predict.slot.other() {
            PredictSlot::A => self.predict.name_a.clone(),
            PredictSlot::B => self.predict.name_b.clone(),
        };
        let Some(other_name) = other else { return };

        let eligible = self
            .eligibility
            .eligible_opponents(just_committed, &self.roster);

        if !eligible.iter().any(|n| n == &other_name) {
            match self.predict.slot.other() {
                PredictSlot::A => self.predict.name_a = None,
                PredictSlot::B => self.predict.name_b = None,
            }
            self.predict.result = None;
            self.predict.error = None;
            self.status_line = Some(format!(
                "{other_name} is no longer an eligible opponent — pick cleared."
            ));
        }
    }

    /// Recompute the FOCUSED slot's candidate pool and clear any now-ineligible
    /// pick in the FOCUSED slot.
    ///
    /// The pool is the OTHER slot's eligible opponents (computed LOCALLY from the
    /// startup-fetched eligibility policy + divisions) when that slot holds a pick,
    /// else the full roster. Because the gate is symmetric, the OTHER slot's
    /// eligible set is exactly the set of fighters allowed in the focused slot. If
    /// the focused slot itself already holds a pick that is no longer in the pool,
    /// that pick is cleared so an ineligible matchup can never persist.
    fn refresh_predict_pool(&mut self) {
        // The name committed in the slot OPPOSITE the one currently focused; it
        // constrains the focused slot's eligible opponents.
        let other = self.predict.other_committed().map(str::to_string);

        let pool = match other {
            Some(name) => self.eligibility.eligible_opponents(&name, &self.roster),
            None => self.roster.clone(),
        };

        let dropped = self.predict.set_pool(pool);
        if let Some(name) = dropped {
            self.status_line = Some(format!(
                "{name} is no longer an eligible opponent — pick cleared."
            ));
        }
    }

    fn recompute_predict_candidates(&mut self) {
        self.predict.candidates = fuzzy::rank(&self.predict.pool, &self.predict.query);
        self.predict.clamp_selection();
    }

    fn maybe_run_prediction(&mut self) {
        let (Some(a), Some(b)) = (self.predict.name_a.clone(), self.predict.name_b.clone()) else {
            return;
        };
        match self.sidecar.predict(&a, &b) {
            Ok(result) => {
                self.predict.result = Some(result);
                self.predict.error = None;
                self.status_line = Some(format!("Predicted {a} vs {b}."));
            }
            Err(e) => {
                self.predict.result = None;
                self.predict.error = Some(e.to_string());
                self.status_line = Some(format!("Prediction failed: {e}"));
            }
        }
    }

    // ======================================================================= //
    // SCRAPE  (async, non-blocking)
    // ======================================================================= //

    fn on_key_scrape(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('f') => self.scrape.full = !self.scrape.full,
            KeyCode::Char('+') | KeyCode::Char('=') => self.scrape.bump_limit(50),
            KeyCode::Char('-') | KeyCode::Char('_') => self.scrape.bump_limit(-50),
            KeyCode::Char('[') => self.scrape.bump_rate(-0.5),
            KeyCode::Char(']') => self.scrape.bump_rate(0.5),
            KeyCode::Char('c') => {
                // Clearing the log only makes sense for a dismissed job; the
                // running-job log lives on the job itself. No-op here otherwise.
            }
            KeyCode::Enter | KeyCode::Char('r') => self.start_scrape(),
            _ => {}
        }
        Ok(())
    }

    /// Launch the scraper as a background job + show the loading overlay.
    pub fn start_scrape(&mut self) {
        if self.job.is_some() {
            return;
        }
        let opts = ScrapeOptions {
            full: self.scrape.full,
            limit: self.scrape.limit,
            rate: self.scrape.rate,
        };
        let mut job = jobs::spawn_scrape(&self.config, &opts);
        job.push_log("$ starting scraper...".to_string());
        self.job = Some(job);
        self.status_line = Some("Scraping...".to_string());
    }

    // ======================================================================= //
    // MODEL  (train async; reload / refresh sync)
    // ======================================================================= //

    fn on_key_model(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('r') => {
                self.refresh_model_status();
                self.status_line = Some("Model status refreshed.".to_string());
            }
            KeyCode::Char('l') => {
                self.reload_sidecar_into_model();
                self.status_line = Some("Sidecar reloaded model from disk.".to_string());
            }
            KeyCode::Char('t') | KeyCode::Enter => self.start_train(),
            _ => {}
        }
        Ok(())
    }

    /// Launch training as a background job + show the loading overlay.
    pub fn start_train(&mut self) {
        if self.job.is_some() {
            return;
        }
        let mut job = jobs::spawn_train(&self.config);
        job.push_log("$ training model...".to_string());
        self.job = Some(job);
        self.status_line = Some("Training model (this can take a while)...".to_string());
    }

    // ======================================================================= //
    // Convenience accessors for renderers.
    // ======================================================================= //

    /// True when a job overlay should be drawn over the current screen.
    pub fn job_active(&self) -> bool {
        self.job.is_some()
    }

    /// The current job status, if any.
    pub fn job_status(&self) -> Option<JobStatus> {
        self.job.as_ref().map(|j| j.status)
    }

    /// The integer ANIMATION frame index derived from elapsed wall-clock time at
    /// [`anim::ANIM_FPS`]. All animation generators (`anim::*`) take this so the
    /// motion plays at a constant speed regardless of how often the loop redraws
    /// (decoupled from the input-poll cadence). PURE w.r.t. the clock.
    pub fn anim_frame(&self) -> usize {
        let ms = self.anim_start.elapsed().as_millis() as u64;
        (ms * crate::anim::ANIM_FPS / 1000) as usize
    }

    /// Whether anything on screen is currently ANIMATING and therefore needs a
    /// fast (time-based) redraw cadence: a background job is in flight OR the
    /// one-shot intro is still playing on Home. When neither is true the screen
    /// is static and the loop can idle on a slow input poll to keep the CPU calm.
    pub fn is_animating(&self) -> bool {
        // A running job animates the spinner/progress/fighters; a FINISHED job's
        // overlay is frozen (see ui::loading), so it does NOT keep us redrawing.
        let job_running = matches!(self.job_status(), Some(JobStatus::Running));
        let intro_playing =
            matches!(self.current(), Screen::Home) && !crate::anim::intro_done(self.anim_frame());
        job_running || intro_playing
    }
}

// =========================================================================== //
// PURE STATE HELPERS (unit-tested below; no I/O).
// =========================================================================== //

impl FighterSearchState {
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    pub fn clamp_selection(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.filtered.get(self.selected).map(String::as_str)
    }
}

impl PredictSlot {
    pub fn other(self) -> PredictSlot {
        match self {
            PredictSlot::A => PredictSlot::B,
            PredictSlot::B => PredictSlot::A,
        }
    }
}

impl PredictState {
    pub fn toggle_slot(&mut self) {
        self.slot = self.slot.other();
        self.query.clear();
        self.selected = 0;
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.candidates.len() {
            self.selected += 1;
        }
    }

    pub fn clamp_selection(&mut self) {
        if self.candidates.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len() - 1;
        }
    }

    pub fn highlighted_candidate(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(String::as_str)
    }

    pub fn commit_slot(&mut self, name: String) {
        match self.slot {
            PredictSlot::A => self.name_a = Some(name),
            PredictSlot::B => self.name_b = Some(name),
        }
        self.query.clear();
        self.selected = 0;
    }

    /// The committed name (if any) in the slot OPPOSITE the focused one. This is
    /// the fighter that constrains the focused slot's eligible pool (the gate is
    /// symmetric, so the OTHER slot's eligible set lists the fighters allowed here).
    pub fn other_committed(&self) -> Option<&str> {
        match self.slot {
            PredictSlot::A => self.name_b.as_deref(),
            PredictSlot::B => self.name_a.as_deref(),
        }
    }

    /// The committed name (if any) in the FOCUSED slot.
    pub fn focused_committed(&self) -> Option<&str> {
        match self.slot {
            PredictSlot::A => self.name_a.as_deref(),
            PredictSlot::B => self.name_b.as_deref(),
        }
    }

    /// Replace the focused slot's candidate `pool`. If the focused slot already
    /// holds a pick that is NOT in the new pool (the OTHER slot's choice made it
    /// ineligible), clear that pick and return the dropped name so the caller can
    /// surface a notice. This is the pure core of the "never let an ineligible
    /// pairing persist" rule.
    pub fn set_pool(&mut self, pool: Vec<String>) -> Option<String> {
        self.pool = pool;

        let stale = match self.focused_committed() {
            Some(name) if !self.pool.iter().any(|n| n == name) => Some(name.to_string()),
            _ => None,
        };
        if stale.is_some() {
            match self.slot {
                PredictSlot::A => self.name_a = None,
                PredictSlot::B => self.name_b = None,
            }
            // The slot reverts to picking, so reset the query/highlight.
            self.query.clear();
            self.selected = 0;
            // A cleared pick also invalidates any stale prediction/error.
            self.result = None;
            self.error = None;
        }
        stale
    }

    pub fn advance_after_commit(&mut self) {
        if self.slot == PredictSlot::A && self.name_b.is_none() {
            self.slot = PredictSlot::B;
            self.query.clear();
            self.selected = 0;
        }
    }

    pub fn both_selected(&self) -> bool {
        self.name_a.is_some() && self.name_b.is_some()
    }
}

impl EventsState {
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.events.len() {
            self.selected += 1;
        }
    }

    pub fn clamp_selection(&mut self) {
        if self.events.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.events.len() {
            self.selected = self.events.len() - 1;
        }
    }

    pub fn selected_event(&self) -> Option<&EventRow> {
        self.events.get(self.selected)
    }
}

impl EventFightsState {
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.fights.len() {
            self.selected += 1;
        }
    }

    pub fn clamp_selection(&mut self) {
        if self.fights.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.fights.len() {
            self.selected = self.fights.len() - 1;
        }
    }

    pub fn selected_fight(&self) -> Option<&FightRow> {
        self.fights.get(self.selected)
    }

    /// The "primary" fighter on the highlighted fight to open as a profile —
    /// the winner when present, else the loser. Screen agents may add an
    /// A/B toggle later; this gives Enter a sensible default.
    pub fn selected_fighter_name(&self) -> Option<&str> {
        let fr = self.selected_fight()?;
        fr.winner_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(fr.loser_name.as_deref().filter(|s| !s.is_empty()))
    }
}

impl ScrapeState {
    /// Adjust the `--limit` value by `delta`; clamps to >= 0 and clears at 0.
    pub fn bump_limit(&mut self, delta: i64) {
        let current = self.limit.unwrap_or(0) as i64;
        let next = (current + delta).max(0);
        self.limit = if next == 0 { None } else { Some(next as u32) };
    }

    /// Adjust the `--rate` value by `delta`; clamps to > 0 and clears at <= 0.
    pub fn bump_rate(&mut self, delta: f64) {
        let next = self.rate.unwrap_or(0.0) + delta;
        self.rate = if next > 0.0 {
            Some((next * 100.0).round() / 100.0)
        } else {
            None
        };
    }
}

/// Format a single stat line "Label: value — explanation" for the profile pane.
/// Pure helper shared by the Fighter renderer; lives here so it stays testable.
pub fn stat_line(label: &str, key: &str, value: Option<f64>) -> String {
    format!(
        "{label}: {} — {}",
        stats_text::describe(key, value),
        stats_text::explain(key)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobMsg, RunningJob};
    use std::sync::mpsc;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ----------------------------------------------------------------------- //
    // NAV STACK
    // ----------------------------------------------------------------------- //

    /// Build the nav-stack ops without a real DB/sidecar by exercising the pure
    /// `Vec<Screen>` helpers directly. (App::push/pop trigger I/O loaders, so the
    /// stack semantics themselves are tested through a bare Vec mirror.)
    #[test]
    fn screen_titles_are_stable() {
        assert_eq!(Screen::Home.title(), "Home");
        assert_eq!(Screen::Database.title(), "Database");
        assert_eq!(Screen::EventFights { event_id: 1 }.title(), "Fight card");
        assert_eq!(
            Screen::Fighter {
                name: "Jon Jones".into()
            }
            .title(),
            "Jon Jones"
        );
    }

    #[test]
    fn nav_stack_push_pop_home_semantics() {
        // Mirror the App nav-stack invariants on a bare Vec.
        let mut nav = vec![Screen::Home];
        nav.push(Screen::Database);
        nav.push(Screen::FighterSearch);
        assert_eq!(nav.last(), Some(&Screen::FighterSearch));

        // pop one level
        nav.pop();
        assert_eq!(nav.last(), Some(&Screen::Database));

        // Home reset: truncate to root
        nav.truncate(1);
        assert_eq!(nav, vec![Screen::Home]);

        // pop at root is a no-op guard (len stays 1)
        if nav.len() > 1 {
            nav.pop();
        }
        assert_eq!(nav, vec![Screen::Home]);
    }

    #[test]
    fn home_menu_has_four_entries_in_order() {
        let labels: Vec<&str> = HOME_MENU.iter().map(|(k, _)| *k).collect();
        assert_eq!(labels, vec!["Scrape", "Database", "Predict", "Model"]);
    }

    #[test]
    fn database_menu_has_two_paths() {
        let labels: Vec<&str> = DATABASE_MENU.iter().map(|(k, _)| *k).collect();
        assert_eq!(labels, vec!["Browse events", "Find a fighter"]);
    }

    // ----------------------------------------------------------------------- //
    // MENU SELECTION BOUNDS
    // ----------------------------------------------------------------------- //

    #[test]
    fn home_menu_selection_clamps() {
        // Simulate Up/Down arithmetic used in on_key_home.
        let mut sel: usize = 0;
        sel = sel.saturating_sub(1); // up at top
        assert_eq!(sel, 0);
        for _ in 0..10 {
            if sel + 1 < HOME_MENU.len() {
                sel += 1;
            }
        }
        assert_eq!(sel, HOME_MENU.len() - 1);
    }

    #[test]
    fn search_selection_bounds() {
        let mut s = FighterSearchState {
            filtered: names(&["a", "b", "c"]),
            ..Default::default()
        };
        s.select_prev();
        assert_eq!(s.selected, 0);
        s.select_next();
        s.select_next();
        s.select_next(); // clamp
        assert_eq!(s.selected, 2);
        assert_eq!(s.selected_name(), Some("c"));
        s.filtered = names(&["a"]);
        s.clamp_selection();
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn events_selection_bounds() {
        let mut e = EventsState {
            events: vec![
                EventRow {
                    event_id: 1,
                    title: "UFC 1".into(),
                    date: None,
                    location: None,
                },
                EventRow {
                    event_id: 2,
                    title: "UFC 2".into(),
                    date: None,
                    location: None,
                },
            ],
            ..EventsState::default()
        };
        e.select_next();
        e.select_next();
        assert_eq!(e.selected, 1);
        assert_eq!(e.selected_event().map(|x| x.event_id), Some(2));
    }

    #[test]
    fn event_fights_default_opens_winner_then_loser() {
        let mut ef = EventFightsState {
            fights: vec![FightRow {
                fight_id: 7,
                event_id: Some(1),
                event_name: None,
                date: None,
                winner_name: Some("Winner".into()),
                loser_name: Some("Loser".into()),
                weight_class: None,
                title_bout: 0,
                method: None,
                round_ended: 1,
                time_ended: 0,
                referee: None,
            }],
            ..Default::default()
        };
        assert_eq!(ef.selected_fighter_name(), Some("Winner"));
        // Empty winner falls back to loser.
        ef.fights[0].winner_name = Some(String::new());
        assert_eq!(ef.selected_fighter_name(), Some("Loser"));
    }

    // ----------------------------------------------------------------------- //
    // PREDICT
    // ----------------------------------------------------------------------- //

    #[test]
    fn predict_toggle_and_commit_flow() {
        let mut p = PredictState::default();
        assert_eq!(p.slot, PredictSlot::A);
        p.candidates = names(&["Jon Jones", "Jan Blachowicz"]);
        p.selected = 1;
        assert_eq!(p.highlighted_candidate(), Some("Jan Blachowicz"));

        p.commit_slot("Jan Blachowicz".to_string());
        assert_eq!(p.name_a.as_deref(), Some("Jan Blachowicz"));
        p.advance_after_commit();
        assert_eq!(p.slot, PredictSlot::B);

        p.candidates = names(&["Jon Jones"]);
        p.selected = 0;
        p.commit_slot("Jon Jones".to_string());
        assert!(p.both_selected());
    }

    #[test]
    fn predict_other_and_focused_committed_track_the_slot() {
        let mut p = PredictState {
            name_a: Some("Alex".into()),
            name_b: Some("Bob".into()),
            ..Default::default()
        };
        // Focused on A: focused = A, other = B.
        p.slot = PredictSlot::A;
        assert_eq!(p.focused_committed(), Some("Alex"));
        assert_eq!(p.other_committed(), Some("Bob"));
        // Focused on B: focused = B, other = A.
        p.slot = PredictSlot::B;
        assert_eq!(p.focused_committed(), Some("Bob"));
        assert_eq!(p.other_committed(), Some("Alex"));
    }

    #[test]
    fn set_pool_keeps_an_eligible_focused_pick() {
        // Focused slot B holds "Bob", which is still in the new (eligible) pool:
        // nothing is dropped.
        let mut p = PredictState {
            slot: PredictSlot::B,
            name_a: Some("Alex".into()),
            name_b: Some("Bob".into()),
            ..Default::default()
        };
        let dropped = p.set_pool(names(&["Bob", "Cara"]));
        assert_eq!(dropped, None);
        assert_eq!(p.name_b.as_deref(), Some("Bob"));
        assert_eq!(p.pool, names(&["Bob", "Cara"]));
    }

    #[test]
    fn set_pool_clears_a_now_ineligible_focused_pick() {
        // Focused slot B holds "Bob", but the new pool (eligible vs the OTHER
        // slot's pick) no longer contains him -> clear B and report the drop.
        let mut p = PredictState {
            slot: PredictSlot::B,
            name_a: Some("Alex".into()),
            name_b: Some("Bob".into()),
            query: "bo".into(),
            selected: 3,
            result: None,
            error: Some("stale".into()),
            ..Default::default()
        };
        let dropped = p.set_pool(names(&["Cara", "Dana"]));
        assert_eq!(dropped.as_deref(), Some("Bob"));
        assert_eq!(p.name_b, None);
        // Slot reverts to picking: query/highlight reset, stale prediction cleared.
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
        assert_eq!(p.error, None);
        // The OTHER slot is untouched.
        assert_eq!(p.name_a.as_deref(), Some("Alex"));
    }

    #[test]
    fn set_pool_leaves_an_empty_focused_slot_alone() {
        // No committed pick in the focused slot -> nothing to drop; pool just set.
        let mut p = PredictState {
            slot: PredictSlot::A,
            name_b: Some("Bob".into()),
            ..Default::default()
        };
        let dropped = p.set_pool(names(&["X", "Y", "Z"]));
        assert_eq!(dropped, None);
        assert_eq!(p.name_a, None);
        assert_eq!(p.pool, names(&["X", "Y", "Z"]));
    }

    // ----------------------------------------------------------------------- //
    // SCRAPE OPTIONS
    // ----------------------------------------------------------------------- //

    #[test]
    fn scrape_limit_and_rate_bumps() {
        let mut s = ScrapeState::default();
        s.bump_limit(50);
        s.bump_limit(50);
        assert_eq!(s.limit, Some(100));
        s.bump_limit(-200);
        assert_eq!(s.limit, None);

        s.bump_rate(1.0);
        s.bump_rate(0.5);
        assert_eq!(s.rate, Some(1.5));
        s.bump_rate(-5.0);
        assert_eq!(s.rate, None);
    }

    // ----------------------------------------------------------------------- //
    // JOB LIFECYCLE (state transitions; no real process)
    // ----------------------------------------------------------------------- //

    #[test]
    fn job_lifecycle_running_to_done() {
        let (tx, rx) = mpsc::channel();
        let mut job = RunningJob::new(JobKind::Scrape, rx);
        assert_eq!(job.status, JobStatus::Running);
        assert!(!job.status.is_finished());

        tx.send(JobMsg::Line("saved event 1/2".into())).unwrap();
        tx.send(JobMsg::Progress(1, 2)).unwrap();
        tx.send(JobMsg::Done(true)).unwrap();

        let completed = job.drain();
        assert_eq!(completed, Some(true));
        assert_eq!(job.status, JobStatus::Done);
        assert!(job.status.is_finished());
        assert_eq!(job.progress, Some((1, 2)));
        assert!(job.log.iter().any(|l| l.contains("saved event 1/2")));
    }

    #[test]
    fn job_lifecycle_failure_marks_failed() {
        let (tx, rx) = mpsc::channel();
        let mut job = RunningJob::new(JobKind::Train, rx);
        tx.send(JobMsg::Done(false)).unwrap();
        assert_eq!(job.drain(), Some(false));
        assert_eq!(job.status, JobStatus::Failed);
    }
}
