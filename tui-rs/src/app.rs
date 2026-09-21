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
    DbSummary, Division, EligibilityRules, EventRow, FightRow, Fighter, LatestCard, PredictResult,
    RoundStat, SnapSelector, SnapshotMeta, WeightClass,
};
use crate::radar::RadarOverlay;
use crate::scraper::ScrapeOptions;
use crate::sidecar::Sidecar;
use crate::{fuzzy, stats_text};
use std::collections::HashMap;

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
            Screen::Home => "↑↓ move · ⏎ select · ^Q q",
            Screen::Database => "↑↓ move · ⏎ select · Esc Back · Home · ^Q q",
            Screen::Scrape => {
                "^Q f full · +/- limit · [/] rate · ^Q c clear · ⏎/^Q r run · Esc Back · Home · ^Q q"
            }
            Screen::Events => "↑↓ move · ⏎ open card · Esc Back · Home · ^Q q",
            Screen::EventFights { .. } => "↑↓ move · ⏎ fighter · Esc Back · Home · ^Q q",
            Screen::FighterSearch => {
                "type to search · ↑↓ move · ⏎ open · ^Q r radar · Esc Back · Home · ^Q q"
            }
            Screen::Fighter { .. } => "^Q r radar · Esc Back · Home · ^Q q",
            Screen::Predict => {
                "type · ↑↓ pick · ⏎ choose · ←→ slot · ⇥ class · ^Q p/s/u · ^Q r radar · ^⌫ clear · Esc Back · ^Q q"
            }
            Screen::Model => "⏎/^Q t train · ^Q l reload · ^Q r refresh · Esc Back · Home · ^Q q",
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

/// Which result surface the Predict screen is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PredictView {
    #[default]
    Manual,
    UpcomingCard,
}

/// One row in a whole-card prediction run.
#[derive(Debug, Clone)]
pub struct UpcomingFightPrediction {
    pub fight: FightRow,
    pub fighter_a: Option<String>,
    pub fighter_b: Option<String>,
    pub result: Option<PredictResult>,
    pub error: Option<String>,
}

/// State for the Predict screen.
#[derive(Debug, Clone, Default)]
pub struct PredictState {
    /// Which output surface is active.
    pub view: PredictView,
    /// Slot currently being edited / picked.
    pub slot: PredictSlot,
    /// Selected weight-class filter, as an index into the fetched
    /// `eligibility.weight_classes`. `None` = "All weight classes" (no filter, the
    /// original behaviour). When `Some(i)`, the candidate POOL for BOTH slots is
    /// restricted to fighters who fought in `weight_classes[i]` (composing with the
    /// eligibility rules on slot B). The index — never a class name — is stored, so
    /// nothing about the ladder is hardcoded here.
    pub weight_class: Option<usize>,
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
    /// Snapshot selector for fighter A (`None` = current / latest version).
    pub snap_a: Option<SnapSelector>,
    /// Snapshot selector for fighter B (`None` = current / latest version).
    pub snap_b: Option<SnapSelector>,
    /// Whether the snapshot picker overlay is open (drill into a career timeline).
    pub picker_open: bool,
    /// Which slot the picker is choosing a snapshot for.
    pub picker_slot: PredictSlot,
    /// The picker's fighter timeline (the two anchors Current/Prime are prepended
    /// by the renderer; these are the per-fight rows).
    pub picker_snaps: Vec<SnapshotMeta>,
    /// Highlighted picker row: 0 = Current, 1 = Prime, 2.. = `picker_snaps[i-2]`.
    pub picker_selected: usize,
    /// Event selected by the whole-card predictor.
    pub card_event: Option<EventRow>,
    /// Per-fight prediction rows for `card_event`.
    pub card_predictions: Vec<UpcomingFightPrediction>,
    /// Whole-card loading/query error, such as no upcoming event in the DB.
    pub card_error: Option<String>,
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
    /// The full weight-class ladder shipped by Python (men ascending then women
    /// ascending). The Predict screen's class picker is built ENTIRELY from this —
    /// no class names/ordinals are hardcoded in Rust. Empty until fetched / when no
    /// model is loaded (the picker then offers only "All weight classes").
    pub weight_classes: Vec<WeightClass>,
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

    /// Whether fighter `name` "is in" weight class `wc` — i.e. the divisions they
    /// have fought in contain the class's `(gender, ordinal)` identity (the
    /// MEMBERSHIP CONTRACT, applied via [`crate::models::fighter_in_class`]). A
    /// fighter with no cached divisions is in no class.
    pub fn fighter_in_class(&self, name: &str, wc: &WeightClass) -> bool {
        let empty: Vec<Division> = Vec::new();
        let divs = self.divisions.get(name).unwrap_or(&empty);
        crate::models::fighter_in_class(divs, wc)
    }

    /// The subset of `roster` whose fighters fought in weight class `wc`, in the
    /// roster's given order. PURE; membership derives entirely from the fetched
    /// `divisions` + the class identity (no hardcoded names/ordinals).
    pub fn in_class(&self, wc: &WeightClass, roster: &[String]) -> Vec<String> {
        roster
            .iter()
            .filter(|n| self.fighter_in_class(n, wc))
            .cloned()
            .collect()
    }

    /// COMPOSE the eligibility gate with the weight-class filter into one candidate
    /// pool. PURE core of the Predict screen's pool computation (the `App` wrapper
    /// just resolves `other` from the focused slot and the class from the selected
    /// index):
    ///   * base pool = `eligible_opponents(other)` when `other` is `Some` (the
    ///     symmetric gate, excludes `other`), else the full `roster`;
    ///   * then narrowed to `class` (fighters in that class) when `Some`, or left
    ///     as-is for "All weight classes" (`None`).
    ///
    /// So the result is `in-class AND eligible(other, b, rules)`.
    pub fn compose_pool(
        &self,
        other: Option<&str>,
        class: Option<&WeightClass>,
        roster: &[String],
    ) -> Vec<String> {
        let base = match other {
            Some(name) => self.eligible_opponents(name, roster),
            None => roster.to_vec(),
        };
        match class {
            Some(wc) => base
                .into_iter()
                .filter(|n| self.fighter_in_class(n, wc))
                .collect(),
            None => base,
        }
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
    /// Ctrl+Q prefix is armed: the next letter is a command, not search text.
    /// Footer shows `^Q-` while this is true. A second Ctrl+Q or Esc cancels.
    pub prefix_armed: bool,
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
    /// Live scrape-telemetry status (`traces: Jaeger :16686 + file` or file-only).
    /// Refreshed when entering the Scrape screen and again at scrape spawn.
    pub scrape_traces_status: String,
    /// Garnish radar overlay (Predict / Find-a-fighter / Fighter profile).
    pub radar: Option<RadarOverlay>,
}

/// Which screen a key is being interpreted against (the prefix command map is
/// per-screen; Other covers Home / Database / Events / profiles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyScreen {
    Predict,
    Scrape,
    Model,
    FighterSearch,
    Fighter,
    Other,
}

impl KeyScreen {
    fn of(screen: &Screen) -> Self {
        match screen {
            Screen::Predict => Self::Predict,
            Screen::Scrape => Self::Scrape,
            Screen::Model => Self::Model,
            Screen::FighterSearch => Self::FighterSearch,
            Screen::Fighter { .. } => Self::Fighter,
            _ => Self::Other,
        }
    }
}

/// Snapshot of the bits `interpret_key` needs. Pure so prefix behaviour can be
/// unit-tested without constructing a full `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyCtx {
    prefix_armed: bool,
    picker_open: bool,
    radar_open: bool,
    /// `None` = no job overlay; `Some(false)` = running; `Some(true)` = finished.
    job_finished: Option<bool>,
    screen: KeyScreen,
}

/// Outcome of one key after prefix / global binding rules, before per-screen
/// typing and arrows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyIntent {
    Noop,
    Quit,
    ArmPrefix,
    DisarmPrefix,
    GoHome,
    Pop,
    DismissJob,
    CloseSnapshotPicker,
    CloseRadar,
    OpenRadar,
    ClearMatchup,
    PredictPrime,
    PredictToggleSnapshot,
    PredictUpcomingCard,
    ScrapeToggleFull,
    ScrapeClearLog,
    ScrapeRun,
    ModelRefresh,
    ModelTrain,
    ModelReload,
    Forward,
}

fn is_ctrl_backspace(key: &KeyEvent) -> bool {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    matches!(key.code, KeyCode::Backspace | KeyCode::Char('h' | 'H'))
}

fn typing_char(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            Some(c)
        }
        _ => None,
    }
}

/// Pure key interpretation: Ctrl+C quits, Ctrl+Q is a prefix, letter commands
/// fire only after that prefix, and printable letters otherwise Forward into
/// the focused search field.
fn interpret_key(key: &KeyEvent, ctx: &KeyCtx) -> KeyIntent {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
    {
        return KeyIntent::Quit;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q' | 'Q'))
    {
        return if ctx.prefix_armed {
            KeyIntent::DisarmPrefix
        } else {
            KeyIntent::ArmPrefix
        };
    }

    if ctx.prefix_armed {
        return interpret_prefixed(key, ctx);
    }

    if ctx.job_finished.is_none() && ctx.screen == KeyScreen::Predict && is_ctrl_backspace(key) {
        return KeyIntent::ClearMatchup;
    }

    if ctx.job_finished.is_some() {
        let finished = ctx.job_finished == Some(true);
        return match key.code {
            KeyCode::Home if finished => KeyIntent::GoHome,
            KeyCode::Esc | KeyCode::Enter if finished => KeyIntent::DismissJob,
            _ => KeyIntent::Noop,
        };
    }

    if ctx.radar_open {
        return match key.code {
            KeyCode::Home => KeyIntent::GoHome,
            KeyCode::Esc => KeyIntent::CloseRadar,
            _ => KeyIntent::Noop,
        };
    }

    match key.code {
        KeyCode::Home => KeyIntent::GoHome,
        KeyCode::Esc => {
            if ctx.picker_open {
                KeyIntent::CloseSnapshotPicker
            } else {
                KeyIntent::Pop
            }
        }
        KeyCode::Backspace
            if !matches!(ctx.screen, KeyScreen::Predict | KeyScreen::FighterSearch) =>
        {
            KeyIntent::Pop
        }
        _ => KeyIntent::Forward,
    }
}

fn interpret_prefixed(key: &KeyEvent, ctx: &KeyCtx) -> KeyIntent {
    match key.code {
        KeyCode::Esc => KeyIntent::DisarmPrefix,
        KeyCode::Char(c) => {
            let c = c.to_ascii_lowercase();
            if c == 'q' {
                return KeyIntent::Quit;
            }
            if ctx.job_finished.is_some() {
                return KeyIntent::Noop;
            }
            match (c, ctx.screen) {
                ('p', KeyScreen::Predict) => KeyIntent::PredictPrime,
                ('s', KeyScreen::Predict) => KeyIntent::PredictToggleSnapshot,
                ('u', KeyScreen::Predict) => KeyIntent::PredictUpcomingCard,
                ('r', KeyScreen::Predict | KeyScreen::FighterSearch | KeyScreen::Fighter) => {
                    KeyIntent::OpenRadar
                }
                ('f', KeyScreen::Scrape) => KeyIntent::ScrapeToggleFull,
                ('c', KeyScreen::Scrape) => KeyIntent::ScrapeClearLog,
                ('r', KeyScreen::Scrape) => KeyIntent::ScrapeRun,
                ('r', KeyScreen::Model) => KeyIntent::ModelRefresh,
                ('t', KeyScreen::Model) => KeyIntent::ModelTrain,
                ('l', KeyScreen::Model) => KeyIntent::ModelReload,
                _ => KeyIntent::Noop,
            }
        }
        _ => KeyIntent::Noop,
    }
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
            prefix_armed: false,
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
            scrape_traces_status: String::new(),
            radar: None,
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
        self.radar = None;
        self.nav.push(screen);
        self.on_enter_current();
    }

    /// Pop one level (Esc / Backspace). No-op at the root (Home stays). Returns
    /// true if a pop actually happened.
    pub fn pop(&mut self) -> bool {
        self.radar = None;
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
        self.radar = None;
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
            Screen::Scrape => self.refresh_scrape_traces_status(),
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
                self.eligibility.weight_classes = payload.weight_classes;
                // A re-fetch can shrink/reorder the ladder; drop a now-invalid index.
                if let Some(i) = self.predict.weight_class
                    && i >= self.eligibility.weight_classes.len()
                {
                    self.predict.weight_class = None;
                }
            }
            Err(e) => {
                self.eligibility = EligibilityState::default();
                self.predict.weight_class = None;
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

    fn key_ctx(&self) -> KeyCtx {
        KeyCtx {
            prefix_armed: self.prefix_armed,
            picker_open: self.predict.picker_open,
            radar_open: self.radar.is_some(),
            job_finished: self.job.as_ref().map(|j| j.status.is_finished()),
            screen: KeyScreen::of(self.current()),
        }
    }

    /// Handle one key event, mutating state and possibly triggering side
    /// effects. Letter commands require a Ctrl+Q prefix so search fields can
    /// accept `q` / `s` / `p` / etc. as typed text.
    pub fn on_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        let intent = interpret_key(&key, &self.key_ctx());
        match intent {
            KeyIntent::ArmPrefix => {
                self.prefix_armed = true;
                Ok(())
            }
            other => {
                self.prefix_armed = false;
                self.apply_key_intent(key, other)
            }
        }
    }

    fn apply_key_intent(&mut self, key: KeyEvent, intent: KeyIntent) -> Result<()> {
        match intent {
            KeyIntent::ArmPrefix => self.prefix_armed = true,
            KeyIntent::DisarmPrefix | KeyIntent::Noop => {}
            KeyIntent::Quit => self.should_quit = true,
            KeyIntent::GoHome => {
                if self.job.as_ref().is_some_and(|j| j.status.is_finished()) {
                    self.job = None;
                }
                self.go_home();
            }
            KeyIntent::Pop => {
                self.pop();
            }
            KeyIntent::DismissJob => self.job = None,
            KeyIntent::CloseSnapshotPicker => self.predict.picker_open = false,
            KeyIntent::CloseRadar => self.radar = None,
            KeyIntent::OpenRadar => {
                if self.radar.is_some() {
                    self.radar = None;
                } else {
                    self.open_radar();
                }
            }
            KeyIntent::ClearMatchup => self.clear_matchup(),
            KeyIntent::PredictPrime => {
                if self.predict.both_selected() {
                    self.toggle_prime_vs_prime();
                }
            }
            KeyIntent::PredictToggleSnapshot => {
                if self.predict.picker_open {
                    self.predict.picker_open = false;
                } else if self.predict.both_selected() {
                    self.open_snapshot_picker(PredictSlot::A);
                }
            }
            KeyIntent::PredictUpcomingCard => self.predict_latest_upcoming_card(),
            KeyIntent::ScrapeToggleFull => self.scrape.full = !self.scrape.full,
            KeyIntent::ScrapeClearLog => {}
            KeyIntent::ScrapeRun => self.start_scrape(),
            KeyIntent::ModelRefresh => {
                self.refresh_model_status();
                self.status_line = Some("Model status refreshed.".to_string());
            }
            KeyIntent::ModelTrain => self.start_train(),
            KeyIntent::ModelReload => {
                self.reload_sidecar_into_model();
                self.status_line = Some("Sidecar reloaded model from disk.".to_string());
            }
            KeyIntent::Forward => match self.current().clone() {
                Screen::Home => return self.on_key_home(key),
                Screen::Database => return self.on_key_database(key),
                Screen::Scrape => return self.on_key_scrape(key),
                Screen::Events => return self.on_key_events(key),
                Screen::EventFights { .. } => return self.on_key_event_fights(key),
                Screen::FighterSearch => return self.on_key_search(key),
                Screen::Fighter { .. } => {}
                Screen::Predict => return self.on_key_predict(key),
                Screen::Model => return self.on_key_model(key),
            },
        }
        Ok(())
    }

    /// Reset both Predict slots and return to an empty manual search.
    fn clear_matchup(&mut self) {
        self.radar = None;
        self.predict.clear_matchup();
        self.refresh_predict_pool();
        self.recompute_predict_candidates();
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
            _ => {
                if let Some(c) = typing_char(key) {
                    self.search.query.push(c);
                    self.reload_search_list();
                }
            }
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

    fn radar_subject_names(&self) -> Vec<String> {
        match self.current() {
            Screen::Predict => [self.predict.name_a.clone(), self.predict.name_b.clone()]
                .into_iter()
                .flatten()
                .collect(),
            Screen::Fighter { name } => vec![name.clone()],
            Screen::FighterSearch => self
                .search
                .selected_name()
                .map(|n| vec![n.to_string()])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn open_radar(&mut self) {
        let names = self.radar_subject_names();
        if names.is_empty() {
            self.status_line = Some("Pick a fighter first.".to_string());
            return;
        }
        match self.db.load_radar_stats() {
            Ok(roster) => match crate::radar::build_overlay_from_stats(&names, &roster) {
                Some(o) => self.radar = Some(o),
                None => {
                    self.status_line = Some("No radar stats for that fighter.".to_string());
                }
            },
            Err(e) => self.status_line = Some(format!("Radar load failed: {e}")),
        }
    }

    // ======================================================================= //
    // PREDICT
    // ======================================================================= //

    fn on_key_predict(&mut self, key: KeyEvent) -> Result<()> {
        if self.radar.is_some() {
            return Ok(());
        }
        // The snapshot picker overlay grabs all keys while open.
        if self.predict.picker_open {
            return self.on_key_snapshot_picker(key);
        }
        match key.code {
            KeyCode::Tab => {
                self.predict.view = PredictView::Manual;
                self.cycle_weight_class(1);
            }
            KeyCode::BackTab => {
                self.predict.view = PredictView::Manual;
                self.cycle_weight_class(-1);
            }
            KeyCode::Left | KeyCode::Right => {
                self.predict.view = PredictView::Manual;
                self.predict.toggle_slot();
                // The focused slot changed, so its pool now depends on the OTHER
                // slot's pick — refresh it (sidecar) before re-ranking.
                self.refresh_predict_pool();
                self.recompute_predict_candidates();
            }
            KeyCode::Up => self.predict.select_prev(),
            KeyCode::Down => self.predict.select_next(),
            KeyCode::Enter => {
                self.predict.view = PredictView::Manual;
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
                self.predict.view = PredictView::Manual;
                self.predict.query.pop();
                self.recompute_predict_candidates();
            }
            _ => {
                if let Some(c) = typing_char(key) {
                    self.predict.view = PredictView::Manual;
                    self.predict.query.push(c);
                    self.recompute_predict_candidates();
                }
            }
        }
        Ok(())
    }

    /// Keys while the snapshot-picker overlay is open. Rows are
    /// `[Current, Prime, <fight 0>, <fight 1>, ...]`; Up/Down navigate, Left/Right
    /// switch which fighter is being picked, Enter commits the snapshot + re-runs.
    /// Esc and `^Q s` close the picker (handled in `interpret_key`).
    fn on_key_snapshot_picker(&mut self, key: KeyEvent) -> Result<()> {
        let total = 2 + self.predict.picker_snaps.len(); // Current + Prime + fights
        match key.code {
            KeyCode::Up => {
                self.predict.picker_selected = self.predict.picker_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.predict.picker_selected + 1 < total {
                    self.predict.picker_selected += 1;
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let other = self.predict.picker_slot.other();
                let name = match other {
                    PredictSlot::A => self.predict.name_a.clone(),
                    PredictSlot::B => self.predict.name_b.clone(),
                };
                if let Some(name) = name {
                    self.open_snapshot_picker_for(other, &name);
                }
            }
            KeyCode::Enter => {
                let sel = self.snapshot_selector_for_index(self.predict.picker_selected);
                match self.predict.picker_slot {
                    PredictSlot::A => self.predict.snap_a = sel,
                    PredictSlot::B => self.predict.snap_b = sel,
                }
                self.predict.picker_open = false;
                self.maybe_run_prediction();
            }
            _ => {}
        }
        Ok(())
    }

    /// Toggle prime-vs-prime: if A is already on its prime, reset BOTH sides to
    /// current; otherwise set BOTH to prime. Re-runs the prediction.
    fn toggle_prime_vs_prime(&mut self) {
        let already_prime =
            matches!(&self.predict.snap_a, Some(SnapSelector::Anchor(s)) if s == "prime");
        if already_prime {
            self.predict.snap_a = None;
            self.predict.snap_b = None;
            self.status_line = Some("Snapshots reset to current.".to_string());
        } else {
            self.predict.snap_a = Some(SnapSelector::Anchor("prime".to_string()));
            self.predict.snap_b = Some(SnapSelector::Anchor("prime".to_string()));
            self.status_line = Some("Prime vs prime.".to_string());
        }
        self.maybe_run_prediction();
    }

    /// Open the snapshot picker for `slot`, fetching that fighter's timeline.
    fn open_snapshot_picker(&mut self, slot: PredictSlot) {
        let name = match slot {
            PredictSlot::A => self.predict.name_a.clone(),
            PredictSlot::B => self.predict.name_b.clone(),
        };
        if let Some(name) = name {
            self.open_snapshot_picker_for(slot, &name);
        }
    }

    fn open_snapshot_picker_for(&mut self, slot: PredictSlot, name: &str) {
        match self.sidecar.timeline(name) {
            Ok(snaps) => {
                self.predict.picker_open = true;
                self.predict.picker_slot = slot;
                self.predict.picker_snaps = snaps;
                self.predict.picker_selected = 0;
            }
            Err(e) => {
                self.predict.picker_open = false;
                self.status_line = Some(format!("Timeline failed: {e}"));
            }
        }
    }

    /// Map a picker row index to a snapshot selector: 0 = Current (`None`),
    /// 1 = Prime, 2.. = the corresponding fight (`{fight_id}`).
    fn snapshot_selector_for_index(&self, idx: usize) -> Option<SnapSelector> {
        match idx {
            0 => None,
            1 => Some(SnapSelector::Anchor("prime".to_string())),
            i => self
                .predict
                .picker_snaps
                .get(i - 2)
                .and_then(|s| s.fight_id)
                .map(|fight_id| SnapSelector::Fight { fight_id }),
        }
    }

    /// After committing `just_committed` in the focused slot, check the OTHER
    /// slot: if it holds a pick that is no longer in the composed candidate pool,
    /// clear it.
    ///
    /// Covers the re-pick case — both slots filled, then the user changes one to a
    /// fighter that makes the existing opponent ineligible. The composed pool is
    /// symmetric (the gate is symmetric and the class filter is the same for both
    /// slots), so the other slot is valid iff it appears in
    /// `predict_pool_for(just_committed)` (eligible vs `just_committed` AND in the
    /// selected class — computed locally, no IPC).
    fn validate_other_slot(&mut self, just_committed: &str) {
        let other = match self.predict.slot.other() {
            PredictSlot::A => self.predict.name_a.clone(),
            PredictSlot::B => self.predict.name_b.clone(),
        };
        let Some(other_name) = other else { return };

        let pool = self.predict_pool_for(Some(just_committed));

        if !pool.iter().any(|n| n == &other_name) {
            match self.predict.slot.other() {
                PredictSlot::A => {
                    self.predict.name_a = None;
                    self.predict.snap_a = None;
                }
                PredictSlot::B => {
                    self.predict.name_b = None;
                    self.predict.snap_b = None;
                }
            }
            self.predict.result = None;
            self.predict.error = None;
            self.status_line = Some(format!(
                "{other_name} is no longer in the candidate pool — pick cleared."
            ));
        }
    }

    /// Recompute the FOCUSED slot's candidate pool and clear any now-ineligible
    /// pick in the FOCUSED slot.
    ///
    /// The pool COMPOSES two filters:
    ///   1. the OTHER slot's eligible opponents (computed LOCALLY from the
    ///      startup-fetched eligibility policy + divisions) when that slot holds a
    ///      pick, else the full roster — the symmetric eligibility gate, and
    ///   2. the selected WEIGHT-CLASS filter (fighters who fought in the chosen
    ///      class), or no class filter when "All weight classes" is selected.
    ///
    /// So `pool = in-class AND eligible(other, b, rules)`. If the focused slot
    /// itself already holds a pick that is no longer in the pool (the other slot's
    /// choice OR a new class selection dropped it), that pick is cleared so an
    /// ineligible / out-of-class matchup can never persist.
    fn refresh_predict_pool(&mut self) {
        // The name committed in the slot OPPOSITE the one currently focused; it
        // constrains the focused slot's eligible opponents.
        let other = self.predict.other_committed().map(str::to_string);
        let pool = self.predict_pool_for(other.as_deref());

        let dropped = self.predict.set_pool(pool);
        if let Some(name) = dropped {
            self.status_line = Some(format!(
                "{name} is no longer in the candidate pool — pick cleared."
            ));
        }
    }

    /// PURE: the candidate pool for a slot whose OPPOSITE slot holds `other` (or
    /// `None` when empty), composing the eligibility gate with the selected
    /// weight-class filter. Used by [`Self::refresh_predict_pool`] and unit-tested
    /// directly. The base pool is `eligible_opponents(other)` when `other` is set
    /// (symmetric gate), else the full roster; it is THEN narrowed to the selected
    /// class (no-op for "All"). NOTHING about the ladder is hardcoded — the class
    /// is looked up by index in the fetched `weight_classes`.
    pub fn predict_pool_for(&self, other: Option<&str>) -> Vec<String> {
        self.eligibility
            .compose_pool(other, self.selected_weight_class(), &self.roster)
    }

    /// The currently-selected [`WeightClass`], or `None` for "All weight classes"
    /// (or when the cached index no longer maps to a class). Resolved by index into
    /// the fetched ladder, so the selection survives only as long as that ladder.
    pub fn selected_weight_class(&self) -> Option<&WeightClass> {
        self.predict
            .weight_class
            .and_then(|i| self.eligibility.weight_classes.get(i))
    }

    /// Change the weight-class filter by `dir` (+1 forward / -1 back) over
    /// `[All, ...fetched classes]`, then re-apply the filter: drop any committed
    /// pick (in EITHER slot) that is no longer in the newly-selected class, and
    /// recompute the focused slot's pool + ranked candidates. The class filter is
    /// the SAME for both slots, so a class change can invalidate either pick.
    fn cycle_weight_class(&mut self, dir: i32) {
        let n = self.eligibility.weight_classes.len();
        self.predict.cycle_weight_class(dir, n);

        // Drop any committed pick that falls out of the newly-selected class. The
        // class filter applies to BOTH slots independently of the eligibility gate.
        if let Some(wc) = self.selected_weight_class().cloned() {
            if let Some(a) = self.predict.name_a.clone()
                && !self.eligibility.fighter_in_class(&a, &wc)
            {
                self.predict.name_a = None;
                self.predict.result = None;
                self.predict.error = None;
            }
            if let Some(b) = self.predict.name_b.clone()
                && !self.eligibility.fighter_in_class(&b, &wc)
            {
                self.predict.name_b = None;
                self.predict.result = None;
                self.predict.error = None;
            }
        }

        let label = match self.selected_weight_class() {
            Some(wc) => wc.name.clone(),
            None => "All weight classes".to_string(),
        };
        self.status_line = Some(format!("Weight class: {label}."));

        self.refresh_predict_pool();
        self.recompute_predict_candidates();
    }

    fn recompute_predict_candidates(&mut self) {
        self.predict.candidates = fuzzy::rank(&self.predict.pool, &self.predict.query);
        self.predict.clamp_selection();
    }

    fn maybe_run_prediction(&mut self) {
        self.predict.view = PredictView::Manual;
        let (Some(a), Some(b)) = (self.predict.name_a.clone(), self.predict.name_b.clone()) else {
            return;
        };
        let (snap_a, snap_b) = (self.predict.snap_a.clone(), self.predict.snap_b.clone());
        match self.sidecar.predict(&a, &b, snap_a, snap_b) {
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

    /// Predict every fight on the next future-dated event in the scraped DB.
    /// Uses the sidecar's single-fight predictor for each row so the Python model
    /// remains the only source of prediction behavior.
    fn predict_latest_upcoming_card(&mut self) {
        self.predict.view = PredictView::UpcomingCard;
        self.predict.card_event = None;
        self.predict.card_predictions.clear();
        self.predict.card_error = None;

        let event = match self.db.next_upcoming_event() {
            Ok(Some(event)) => event,
            Ok(None) => {
                self.predict.card_error =
                    Some("No upcoming event found in the scraped database.".to_string());
                self.status_line = Some("No upcoming event found.".to_string());
                return;
            }
            Err(e) => {
                self.predict.card_error = Some(format!("Upcoming card lookup failed: {e}"));
                self.status_line = Some(format!("Upcoming card lookup failed: {e}"));
                return;
            }
        };

        let fights = match self.db.fights_for_event(event.event_id) {
            Ok(fights) => fights,
            Err(e) => {
                self.predict.card_event = Some(event);
                self.predict.card_error = Some(format!("Card load failed: {e}"));
                self.status_line = Some(format!("Card load failed: {e}"));
                return;
            }
        };

        if fights.is_empty() {
            self.predict.card_event = Some(event);
            self.predict.card_error = Some("Upcoming card has no fights recorded.".to_string());
            self.status_line = Some("Upcoming card has no fights recorded.".to_string());
            return;
        }

        let title = event.title.clone();
        self.predict.card_event = Some(event);

        let mut rows = Vec::with_capacity(fights.len());
        for fight in fights {
            let Some((a, b)) = fight_prediction_names(&fight) else {
                rows.push(UpcomingFightPrediction {
                    fight,
                    fighter_a: None,
                    fighter_b: None,
                    result: None,
                    error: Some("missing fighter names".to_string()),
                });
                continue;
            };

            match self.sidecar.predict(&a, &b, None, None) {
                Ok(result) => rows.push(UpcomingFightPrediction {
                    fight,
                    fighter_a: Some(a),
                    fighter_b: Some(b),
                    result: Some(result),
                    error: None,
                }),
                Err(e) => rows.push(UpcomingFightPrediction {
                    fight,
                    fighter_a: Some(a),
                    fighter_b: Some(b),
                    result: None,
                    error: Some(e.to_string()),
                }),
            }
        }

        let predicted = rows
            .iter()
            .filter(|row| row.result.as_ref().map(|r| r.allowed).unwrap_or(false))
            .count();
        let total = rows.len();
        self.predict.card_predictions = rows;
        self.status_line = Some(format!("Predicted {predicted}/{total} fights for {title}."));
    }

    // ======================================================================= //
    // SCRAPE  (async, non-blocking)
    // ======================================================================= //

    fn on_key_scrape(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('+') | KeyCode::Char('=') => self.scrape.bump_limit(50),
            KeyCode::Char('-') | KeyCode::Char('_') => self.scrape.bump_limit(-50),
            KeyCode::Char('[') => self.scrape.bump_rate(-0.5),
            KeyCode::Char(']') => self.scrape.bump_rate(0.5),
            KeyCode::Enter => self.start_scrape(),
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
        self.refresh_scrape_traces_status();
        let mut job = jobs::spawn_scrape(&self.config, &opts);
        job.push_log("$ starting scraper...".to_string());
        self.job = Some(job);
        self.status_line = Some("Scraping...".to_string());
    }

    /// Probe local Jaeger and cache the one-line traces status for the Scrape UI.
    fn refresh_scrape_traces_status(&mut self) {
        self.scrape_traces_status =
            crate::otel::ChildOtel::for_scraper(&self.config.repo_root).status_line;
    }

    // ======================================================================= //
    // MODEL  (train async; reload / refresh sync)
    // ======================================================================= //

    fn on_key_model(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Enter => self.start_train(),
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
        // A running job animates the spinner/progress strip; a FINISHED job's
        // overlay is frozen (see ui::loading), so it does NOT keep us redrawing.
        let job_running = matches!(self.job_status(), Some(JobStatus::Running));
        let intro_playing =
            matches!(self.current(), Screen::Home) && !crate::anim::intro_done(self.anim_frame());
        job_running || intro_playing
    }
}

/// Extract the two fighter slots from a stored fight row for prediction. Completed
/// fights use winner/loser names; future cards may use the same columns simply as
/// the two scheduled competitors. Empty names are treated as missing.
fn fight_prediction_names(fight: &FightRow) -> Option<(String, String)> {
    let a = fight.winner_name.as_deref()?.trim();
    let b = fight.loser_name.as_deref()?.trim();
    if a.is_empty() || b.is_empty() {
        return None;
    }
    Some((a.to_string(), b.to_string()))
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
        // A new fighter resets that slot's snapshot to current (a prime/fight_id
        // from a previous fighter is meaningless for this one).
        match self.slot {
            PredictSlot::A => {
                self.name_a = Some(name);
                self.snap_a = None;
            }
            PredictSlot::B => {
                self.name_b = Some(name);
                self.snap_b = None;
            }
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
                PredictSlot::A => {
                    self.name_a = None;
                    self.snap_a = None;
                }
                PredictSlot::B => {
                    self.name_b = None;
                    self.snap_b = None;
                }
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

    /// Cycle the weight-class selection by `dir` (+1 forward / -1 back) over the
    /// sequence `[All, class_0, class_1, ..., class_{n-1}]` (so `n + 1` options),
    /// where `n` is the number of fetched classes. `None` is "All weight classes".
    /// PURE: mutates only the index and resets the live query/highlight (the focused
    /// slot reverts to fresh picking under the new filter); the caller re-filters the
    /// pools + clears any out-of-class picks. With no classes fetched this is a no-op
    /// (only "All" exists).
    pub fn cycle_weight_class(&mut self, dir: i32, n_classes: usize) {
        if n_classes == 0 {
            self.weight_class = None;
            return;
        }
        let total = n_classes as i64 + 1; // +1 for the "All" sentinel
        // Map current selection to a position: 0 = All, i+1 = class i.
        let cur = match self.weight_class {
            None => 0i64,
            Some(i) => i as i64 + 1,
        };
        let next = (cur + dir as i64).rem_euclid(total);
        self.weight_class = if next == 0 {
            None
        } else {
            Some((next - 1) as usize)
        };
        self.query.clear();
        self.selected = 0;
    }

    pub fn both_selected(&self) -> bool {
        self.name_a.is_some() && self.name_b.is_some()
    }

    /// Wipe both fighter slots, snapshots, the last result, and the upcoming-card
    /// view, then return focus to slot A with an empty query.
    pub fn clear_matchup(&mut self) {
        self.name_a = None;
        self.name_b = None;
        self.snap_a = None;
        self.snap_b = None;
        self.result = None;
        self.error = None;
        self.query.clear();
        self.selected = 0;
        self.slot = PredictSlot::A;
        self.view = PredictView::Manual;
        self.card_event = None;
        self.card_predictions.clear();
        self.card_error = None;
        self.picker_open = false;
        self.picker_snaps.clear();
        self.picker_selected = 0;
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
    fn fight_prediction_names_requires_two_non_empty_names() {
        let mut fight = FightRow {
            fight_id: 1,
            event_id: Some(1),
            event_name: None,
            date: None,
            winner_name: Some("  Israel Adesanya  ".into()),
            loser_name: Some("Robert Whittaker".into()),
            weight_class: Some("Middleweight".into()),
            title_bout: 0,
            method: None,
            round_ended: 0,
            time_ended: 0,
            referee: None,
        };
        assert_eq!(
            fight_prediction_names(&fight),
            Some(("Israel Adesanya".into(), "Robert Whittaker".into()))
        );

        fight.loser_name = Some("   ".into());
        assert_eq!(fight_prediction_names(&fight), None);
        fight.winner_name = None;
        fight.loser_name = Some("Robert Whittaker".into());
        assert_eq!(fight_prediction_names(&fight), None);
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
    // WEIGHT-CLASS FILTER (membership, compose with eligibility, cycling)
    // ----------------------------------------------------------------------- //

    use crate::models::{Gender, WeightClass};

    fn div(g: &str, o: i32) -> Division {
        Division(g.to_string(), o)
    }

    fn wc(name: &str, g: Gender, o: i32) -> WeightClass {
        WeightClass {
            name: name.to_string(),
            gender: g,
            ordinal: o,
        }
    }

    /// The canned stub-shaped eligibility cache: Pereira at Heavyweight (M#8),
    /// Adesanya + Whittaker at Middleweight (M#6), max_distance 1, two classes.
    fn stub_eligibility() -> EligibilityState {
        let mut divisions = HashMap::new();
        divisions.insert("Alex Pereira".to_string(), vec![div("M", 8)]);
        divisions.insert("Israel Adesanya".to_string(), vec![div("M", 6)]);
        divisions.insert("Robert Whittaker".to_string(), vec![div("M", 6)]);
        EligibilityState {
            rules: Some(EligibilityRules {
                max_distance: 1,
                allow_cross_gender: false,
                allow_unknown_division: true,
            }),
            divisions,
            weight_classes: vec![
                wc("Middleweight", Gender::Men, 6),
                wc("Heavyweight", Gender::Men, 8),
            ],
        }
    }

    #[test]
    fn in_class_filters_roster_by_membership() {
        let elig = stub_eligibility();
        let roster = names(&["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]);

        // Middleweight (M#6) -> Adesanya + Whittaker (NOT Pereira).
        let mw = wc("Middleweight", Gender::Men, 6);
        assert_eq!(
            elig.in_class(&mw, &roster),
            names(&["Israel Adesanya", "Robert Whittaker"])
        );
        // Heavyweight (M#8) -> only Pereira.
        let hw = wc("Heavyweight", Gender::Men, 8);
        assert_eq!(elig.in_class(&hw, &roster), names(&["Alex Pereira"]));
    }

    #[test]
    fn compose_pool_all_classes_is_pure_eligibility() {
        let elig = stub_eligibility();
        let roster = names(&["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]);

        // No class filter, no other slot -> the whole roster (current behaviour).
        assert_eq!(elig.compose_pool(None, None, &roster), roster);

        // No class filter, slot A = Adesanya -> eligible opponents only:
        // Whittaker (M#6, dist 0) stays; Pereira (M#8, dist 2 > 1) is gated out.
        assert_eq!(
            elig.compose_pool(Some("Israel Adesanya"), None, &roster),
            names(&["Robert Whittaker"])
        );
    }

    #[test]
    fn compose_pool_narrows_slot_a_by_selected_class() {
        let elig = stub_eligibility();
        let roster = names(&["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]);

        // Slot A pool (no other slot) restricted to Middleweight: Pereira dropped.
        let mw = wc("Middleweight", Gender::Men, 6);
        assert_eq!(
            elig.compose_pool(None, Some(&mw), &roster),
            names(&["Israel Adesanya", "Robert Whittaker"])
        );
        // Restricted to Heavyweight: only Pereira.
        let hw = wc("Heavyweight", Gender::Men, 8);
        assert_eq!(
            elig.compose_pool(None, Some(&hw), &roster),
            names(&["Alex Pereira"])
        );
    }

    #[test]
    fn compose_pool_composes_class_filter_with_eligibility_on_slot_b() {
        let elig = stub_eligibility();
        let roster = names(&["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]);
        let mw = wc("Middleweight", Gender::Men, 6);
        let hw = wc("Heavyweight", Gender::Men, 8);

        // Slot A = Adesanya, class = Middleweight: pool is eligible-vs-Adesanya
        // (Whittaker) AND in Middleweight (Whittaker) -> [Whittaker].
        assert_eq!(
            elig.compose_pool(Some("Israel Adesanya"), Some(&mw), &roster),
            names(&["Robert Whittaker"])
        );

        // Slot A = Adesanya, class = Heavyweight: the only Heavyweight is Pereira,
        // but Pereira is INELIGIBLE vs Adesanya -> pool is EMPTY (compose, not OR).
        assert_eq!(
            elig.compose_pool(Some("Israel Adesanya"), Some(&hw), &roster),
            Vec::<String>::new()
        );
    }

    #[test]
    fn cycle_weight_class_walks_all_then_each_class_and_wraps() {
        let mut p = PredictState::default();
        let n = 2; // [All, class0, class1]

        // Start at All (None).
        assert_eq!(p.weight_class, None);
        // Forward: None -> class0 -> class1 -> None ...
        p.cycle_weight_class(1, n);
        assert_eq!(p.weight_class, Some(0));
        p.cycle_weight_class(1, n);
        assert_eq!(p.weight_class, Some(1));
        p.cycle_weight_class(1, n);
        assert_eq!(p.weight_class, None);

        // Backward from All wraps to the LAST class.
        p.cycle_weight_class(-1, n);
        assert_eq!(p.weight_class, Some(1));
        p.cycle_weight_class(-1, n);
        assert_eq!(p.weight_class, Some(0));
        p.cycle_weight_class(-1, n);
        assert_eq!(p.weight_class, None);
    }

    #[test]
    fn cycle_weight_class_is_noop_without_fetched_classes() {
        let mut p = PredictState {
            weight_class: None,
            query: "search".into(),
            selected: 2,
            ..Default::default()
        };
        p.cycle_weight_class(1, 0);
        assert_eq!(p.weight_class, None);
    }

    #[test]
    fn cycle_weight_class_resets_query_and_highlight() {
        let mut p = PredictState {
            query: "ades".into(),
            selected: 3,
            ..Default::default()
        };
        p.cycle_weight_class(1, 2);
        assert_eq!(p.weight_class, Some(0));
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
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

    // ----------------------------------------------------------------------- //
    // PREFIX KEYS + CLEAR MATCHUP
    // ----------------------------------------------------------------------- //

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn ctx_predict() -> KeyCtx {
        KeyCtx {
            prefix_armed: false,
            picker_open: false,
            radar_open: false,
            job_finished: None,
            screen: KeyScreen::Predict,
        }
    }

    fn ctx_home() -> KeyCtx {
        KeyCtx {
            prefix_armed: false,
            picker_open: false,
            radar_open: false,
            job_finished: None,
            screen: KeyScreen::Other,
        }
    }

    fn ctx_armed(screen: KeyScreen) -> KeyCtx {
        KeyCtx {
            prefix_armed: true,
            picker_open: false,
            radar_open: false,
            job_finished: None,
            screen,
        }
    }

    #[test]
    fn clear_matchup_wipes_picks_snaps_result_and_card() {
        let mut p = PredictState {
            view: PredictView::UpcomingCard,
            slot: PredictSlot::B,
            name_a: Some("Alex".into()),
            name_b: Some("Bob".into()),
            snap_a: Some(SnapSelector::Anchor("prime".into())),
            snap_b: Some(SnapSelector::Fight { fight_id: 9 }),
            query: "bo".into(),
            selected: 2,
            result: Some(PredictResult {
                name_a: "Alex".into(),
                name_b: "Bob".into(),
                allowed: true,
                reason: None,
                prob_a: Some(0.62),
                prob_b: Some(0.38),
                low_confidence: false,
                distance: None,
                tale_a: None,
                tale_b: None,
                model: None,
                test_accuracy: None,
                snap_a_label: None,
                snap_b_label: None,
                method_probs: None,
            }),
            error: Some("stale".into()),
            picker_open: true,
            card_event: Some(EventRow {
                event_id: 1,
                title: "UFC 1".into(),
                date: None,
                location: None,
            }),
            card_error: Some("nope".into()),
            ..Default::default()
        };
        p.clear_matchup();
        assert_eq!(p.name_a, None);
        assert_eq!(p.name_b, None);
        assert_eq!(p.snap_a, None);
        assert_eq!(p.snap_b, None);
        assert_eq!(p.result, None);
        assert_eq!(p.error, None);
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
        assert_eq!(p.slot, PredictSlot::A);
        assert_eq!(p.view, PredictView::Manual);
        assert!(p.card_event.is_none());
        assert!(p.card_predictions.is_empty());
        assert_eq!(p.card_error, None);
        assert!(!p.picker_open);
    }

    #[test]
    fn interpret_key_typing_q_s_p_on_predict_forwards() {
        let ctx = ctx_predict();
        for c in ['q', 's', 'p', 'r'] {
            assert_eq!(
                interpret_key(&press(KeyCode::Char(c)), &ctx),
                KeyIntent::Forward,
                "bare {c} on Predict must type, not fire a command"
            );
        }
        assert_eq!(
            interpret_key(&press(KeyCode::Char('q')), &ctx_home()),
            KeyIntent::Forward,
            "bare q on Home must not quit"
        );
    }

    #[test]
    fn interpret_key_ctrl_q_then_q_quits() {
        assert_eq!(
            interpret_key(&ctrl(KeyCode::Char('q')), &ctx_home()),
            KeyIntent::ArmPrefix
        );
        let mut armed = ctx_home();
        armed.prefix_armed = true;
        assert_eq!(
            interpret_key(&press(KeyCode::Char('q')), &armed),
            KeyIntent::Quit
        );
        assert_eq!(
            interpret_key(&press(KeyCode::Char('Q')), &armed),
            KeyIntent::Quit
        );
    }

    #[test]
    fn interpret_key_ctrl_q_then_esc_disarms_without_pop() {
        let mut armed = ctx_home();
        armed.prefix_armed = true;
        assert_eq!(
            interpret_key(&press(KeyCode::Esc), &armed),
            KeyIntent::DisarmPrefix
        );
        assert_ne!(interpret_key(&press(KeyCode::Esc), &armed), KeyIntent::Pop);
        // A second Ctrl+Q also disarms.
        assert_eq!(
            interpret_key(&ctrl(KeyCode::Char('q')), &armed),
            KeyIntent::DisarmPrefix
        );
    }

    #[test]
    fn interpret_key_esc_closes_snapshot_picker_not_pop() {
        let mut ctx = ctx_predict();
        ctx.picker_open = true;
        assert_eq!(
            interpret_key(&press(KeyCode::Esc), &ctx),
            KeyIntent::CloseSnapshotPicker
        );
    }

    #[test]
    fn interpret_key_esc_closes_radar_not_pop() {
        let mut ctx = ctx_predict();
        ctx.radar_open = true;
        ctx.picker_open = true;
        assert_eq!(
            interpret_key(&press(KeyCode::Esc), &ctx),
            KeyIntent::CloseRadar
        );
        let mut armed = ctx;
        armed.prefix_armed = true;
        assert_eq!(
            interpret_key(&press(KeyCode::Esc), &armed),
            KeyIntent::DisarmPrefix
        );
    }

    #[test]
    fn interpret_key_ctrl_q_r_opens_radar_on_fighter_screens() {
        for screen in [
            KeyScreen::Predict,
            KeyScreen::FighterSearch,
            KeyScreen::Fighter,
        ] {
            assert_eq!(
                interpret_key(&press(KeyCode::Char('r')), &ctx_armed(screen)),
                KeyIntent::OpenRadar,
                "^Q r on {screen:?} must open radar"
            );
        }
        assert_eq!(
            interpret_key(&press(KeyCode::Char('r')), &ctx_predict()),
            KeyIntent::Forward,
            "bare r on Predict must type, not open radar"
        );
    }

    #[test]
    fn interpret_key_ctrl_q_r_stays_run_on_scrape_and_model() {
        assert_eq!(
            interpret_key(&press(KeyCode::Char('r')), &ctx_armed(KeyScreen::Scrape)),
            KeyIntent::ScrapeRun
        );
        assert_eq!(
            interpret_key(&press(KeyCode::Char('r')), &ctx_armed(KeyScreen::Model)),
            KeyIntent::ModelRefresh
        );
    }

    #[test]
    fn interpret_key_ctrl_backspace_and_ctrl_h_clear_matchup() {
        let ctx = ctx_predict();
        assert_eq!(
            interpret_key(&ctrl(KeyCode::Backspace), &ctx),
            KeyIntent::ClearMatchup
        );
        assert_eq!(
            interpret_key(&ctrl(KeyCode::Char('h')), &ctx),
            KeyIntent::ClearMatchup
        );
        assert_eq!(
            interpret_key(&press(KeyCode::Backspace), &ctx),
            KeyIntent::Forward,
            "plain Backspace still edits the query"
        );
    }

    #[test]
    fn interpret_key_unknown_prefixed_letter_is_noop() {
        let mut armed = ctx_predict();
        armed.prefix_armed = true;
        assert_eq!(
            interpret_key(&press(KeyCode::Char('x')), &armed),
            KeyIntent::Noop
        );
        assert_eq!(
            interpret_key(&press(KeyCode::Char('s')), &armed),
            KeyIntent::PredictToggleSnapshot
        );
        let mut scrape = armed;
        scrape.screen = KeyScreen::Scrape;
        assert_eq!(
            interpret_key(&press(KeyCode::Char('f')), &scrape),
            KeyIntent::ScrapeToggleFull
        );
        assert_eq!(
            interpret_key(&press(KeyCode::Char('s')), &scrape),
            KeyIntent::Noop
        );
    }

    #[test]
    fn footer_hints_use_ctrl_q_prefix() {
        assert!(Screen::Home.footer_hint().contains("^Q q"));
        assert!(!Screen::Home.footer_hint().contains("q Quit"));
        let predict = Screen::Predict.footer_hint();
        assert!(predict.contains("^Q p/s/u"));
        assert!(predict.contains("^Q r radar"));
        assert!(predict.contains("^⌫ clear"));
        assert!(predict.contains("^Q q"));
        assert!(Screen::FighterSearch.footer_hint().contains("^Q r radar"));
        assert!(
            Screen::Fighter { name: "x".into() }
                .footer_hint()
                .contains("^Q r radar")
        );
        assert!(Screen::Scrape.footer_hint().contains("^Q f full"));
        assert!(Screen::Scrape.footer_hint().contains("⏎/^Q r run"));
        assert!(!Screen::Scrape.footer_hint().contains("radar"));
        assert!(Screen::Model.footer_hint().contains("^Q t train"));
        assert!(Screen::Model.footer_hint().contains("^Q r refresh"));
    }
}
