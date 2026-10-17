# mma-tui — Contract (FROZEN)

This document freezes (a) the IPC protocol with the Python ML sidecar and (b) the
public Rust types and function signatures the Phase-2 build agents implement
against. **Phase-2 agents import these and MUST NOT change them.** If a signature
must change, change it HERE first, then in code.

The TUI is the only thing the user launches. It:
1. reads `data/ufc.db` **read-only** via rusqlite (`src/db.rs`),
2. spawns a **long-lived** Python sidecar (`ml/serve.py`) that loads the model once
   and answers JSON-line requests on stdin/stdout (`src/sidecar.rs`),
3. spawns the Go scraper on demand and streams its output (`src/scraper.rs`).

**ALL prediction logic stays in Python; Rust NEVER reimplements ML math.**

---

## 1. IPC protocol (Rust ⇄ `ml/serve.py`)

Newline-delimited JSON ("JSON lines"): **one compact JSON object per line, no
embedded newlines.** Requests go to the sidecar's **stdin**, responses come back on
**stdout**. **stderr is for logs ONLY.** The sidecar **must flush stdout after every
response** and **must not crash on bad input** (it returns an error object instead).

### Requests (Rust → sidecar)
Every request has an integer `id` and a string `cmd`:

```
{"id":N,"cmd":"ping"}
{"id":N,"cmd":"status"}
{"id":N,"cmd":"roster"}
{"id":N,"cmd":"predict","a":"<name>","b":"<name>","snap_a":<sel>,"snap_b":<sel>}
{"id":N,"cmd":"timeline","name":"<name>"}
{"id":N,"cmd":"reload"}
```

A snapshot `<sel>` (both `snap_a`/`snap_b` are **optional**) picks which career
snapshot of a fighter to use: omitted/`null`/`"current"` (default = latest
version), `"prime"`, `{"fight_id":<int>}`, or `{"date":"YYYY-MM-DD"}`. Omitting
them yields the **byte-identical** old predict request.

### Responses (sidecar → Rust)
Echo the same `id`:

```
ok:    {"id":N,"ok":true, ...payload}
error: {"id":N,"ok":false,"error":"<message>"}
```

### Payloads (on `ok:true`)
| cmd       | payload fields                                                                   |
|-----------|---------------------------------------------------------------------------------|
| `ping`    | *(none)* — just `{"ok":true}`                                                    |
| `status`  | `model_loaded:bool`, `n_fighters:int`, `metrics:{...}\|null`, `model_path:str`   |
| `roster`  | `fighters:[str,...]`  (→ `ok:false "model not trained"` if no model)             |
| `predict` | `result:{...}` — the `predict()` dict with **NaN/Inf replaced by null**          |
| `timeline`| `name:str`, `snapshots:[{...}, ...]` — a fighter's selectable career snapshots   |
| `reload`  | `model_loaded:bool`, `n_fighters:int`                                            |

`predict.result` mirrors `ml/predict.py::predict()`:
`name_a, name_b, allowed, reason, prob_a, prob_b, low_confidence, distance,
tale_a, tale_b, model, test_accuracy`, plus `snap_a_label`/`snap_b_label` (which
snapshot each side used, e.g. `"current"` / `"prime (2021-03-06)"`) and
`method_probs` — a coherent **joint** win-method distribution
`{"a":{decision,ko_tko,submission},"b":{...},"a_given_win":{...},"b_given_win":{...},"kappa":f}`
(or `null` on a legacy payload), where the six `a`/`b` values sum to 1 and
`a_given_win`/`b_given_win` are each fighter's conditional split
`P(method | that fighter wins)` (each sums to 1; absent on an older sidecar). Each `tale_*` has:
`elo, age, record (str "W-L"), reach_in, height_in, stance, recent_winrate,
form_delta, layoff_days, divisions (list)`.

`timeline.snapshots` items (oldest first, the computed prime flagged):
`index, fight_id, date, opponent, result ("w"/"l"), weight_class, record (str
"W-L"), elo, win_streak, is_prime`.

**JSON `null` rule:** any Python `NaN`/`Inf` (missing reach/height/age/elo) MUST be
serialized as JSON `null` so the Rust `Option<f64>` fields deserialize cleanly.

### Lifecycle
On startup the sidecar **tries** to load the model. If the model is absent it
**stays up** and answers `ping`/`status`, but `predict`/`roster` return
`ok:false` with a clear **"model not trained"** error so the TUI can offer to train.

---

## 2. Frozen Rust types (`src/models.rs`)

These cross module/IPC boundaries and are serde-derived where noted.

### DB row models (mirror `data/ufc.db`; nullable numerics = `Option<T>`)
- `struct Fighter` — `fighter_id, name, nickname, nationality, height_in,
  weight_lbs, reach_in, stance, date_of_birth, wins, losses, draws, no_contests,
  was_champion, championship_bouts_won, slpm, str_acc, sapm, str_def, td_avg,
  td_acc, td_def, sub_avg`.
- `struct EventRow` — `event_id, title, date, location`.
- `struct FightRow` — `fight_id, event_id, event_name, date, winner_name,
  loser_name, weight_class, title_bout, method, round_ended, time_ended, referee`.
- `struct RoundStat` — full wide row: scalars (`knockdowns, sub_attempts,
  reversals, control_time`), `td_*`, `sig_str_*`, `total_str_*`, and the
  target/position breakdowns (`head/body/leg/distance/clinch/ground_{landed,
  attempted,pct}`). `*_pct` are 0..1 fractions.
- `struct DbSummary` — `n_fighters, n_events, n_fights, n_round_stats,
  earliest_event, latest_event`.

Conventions (per `docs/SCHEMA_CONTRACT.md`): percentages are **0..1 fractions**;
height/reach in **inches**; weight in **lbs**; control/finish times in **seconds**;
dates/DOB ISO `YYYY-MM-DD` text; missing numerics are NULL → `Option::None`.

### Prediction models (mirror `predict()`; NaN/Inf already → null)
- `struct TaleOfTape` — `elo, age, record, reach_in, height_in, stance,
  recent_winrate, form_delta, layoff_days: Option<f64>` (and `record: Option<String>`,
  `stance: Option<String>`), `divisions: Vec<String>`.
- `struct PredictResult` — `name_a, name_b: String; allowed: bool;
  reason: Option<String>; prob_a, prob_b: Option<f64>; low_confidence: bool;
  distance: Option<i64>; tale_a, tale_b: Option<TaleOfTape>;
  model: Option<String>; test_accuracy: Option<f64>`, plus (all `#[serde(default)]`
  so an older sidecar still deserializes) `snap_a_label, snap_b_label: Option<String>`
  and `method_probs: Option<MethodSplit>`.
- `struct MethodProbs` — `decision, ko_tko, submission: Option<f64>`.
- `struct MethodSplit` — `a: MethodProbs, b: MethodProbs, kappa: Option<f64>` (the
  six `a`/`b` values are a coherent joint distribution summing to 1), plus
  `#[serde(default)] a_given_win, b_given_win: Option<MethodProbs>` (each fighter's
  conditional "if they win" split, summing to 1; `None` from an older sidecar).
- `enum SnapSelector` (`#[serde(untagged)]`) — `Anchor(String)` (`"current"`/
  `"latest"`/`"prime"`), `Fight { fight_id: i64 }`, or `Date { date: String }`.
- `struct SnapshotMeta` — `index: i64, fight_id: Option<i64>, date: String,
  opponent: String, result: String, weight_class: Option<String>, record: String,
  elo: Option<f64>, win_streak: i64, is_prime: bool`.
- `struct TimelinePayload` — `name: String, snapshots: Vec<SnapshotMeta>`.

### IPC types
- `struct SidecarRequest { id: u64, #[serde(flatten)] cmd: SidecarCommand }`.
- `enum SidecarCommand` — `#[serde(tag = "cmd", rename_all = "lowercase")]`:
  `Ping, Status, Roster, Eligibility,
  Predict { a, b: String, snap_a, snap_b: Option<SnapSelector> (both
  #[serde(default, skip_serializing_if = "Option::is_none")]) },
  Timeline { name: String }, Reload`.
- `struct SidecarResponse { id: u64, ok: bool, error: Option<String>,
  #[serde(flatten)] payload: serde_json::Value }`.
- Typed payloads (deserialize `payload` into these per command):
  `StatusPayload { model_loaded, n_fighters, metrics: Option<Value>, model_path }`,
  `RosterPayload { fighters: Vec<String> }`,
  `PredictPayload { result: PredictResult }`,
  `ReloadPayload { model_loaded, n_fighters }`.

---

## 3. Frozen Rust signatures (stub bodies = `todo!()`)

### `src/config.rs`
```rust
pub enum ScraperLaunch { Binary(PathBuf), GoRun { dir: PathBuf } }
pub struct Config {
    pub repo_root: PathBuf, pub db_path: PathBuf, pub python: PathBuf,
    pub sidecar_script: PathBuf, pub ml_dir: PathBuf,
    pub scraper_dir: PathBuf, pub scraper: ScraperLaunch,
}
impl Config { pub fn load() -> anyhow::Result<Config>; }
```
Resolution: repo root = nearest ancestor containing `data/ufc.db`; python = env
`MMA_PYTHON`, else `<repo>/.venv/bin/python` if present, else `python3`; sidecar =
`<repo>/ml/serve.py`; scraper = built binary in `scraper-go/` if present, else
`GoRun { dir: <repo>/scraper-go }`.

### `src/db.rs`
```rust
pub struct Db { pub conn: rusqlite::Connection }
impl Db {
    pub fn open(path: &Path) -> Result<Db>;                       // READ-ONLY
    pub fn load_fighters(&self) -> Result<Vec<Fighter>>;
    pub fn search_fighters(&self, query: &str) -> Result<Vec<Fighter>>;
    pub fn fighter_profile(&self, name: &str) -> Result<Option<Fighter>>;
    pub fn load_events(&self) -> Result<Vec<EventRow>>;
    pub fn fights_for_fighter(&self, name: &str) -> Result<Vec<FightRow>>;
    pub fn fights_for_event(&self, event_id: i64) -> Result<Vec<FightRow>>;
    pub fn rounds_for_fight(&self, fight_id: i64) -> Result<Vec<RoundStat>>;
    pub fn summary(&self) -> Result<DbSummary>;
}
```

### `src/sidecar.rs`
```rust
pub struct Sidecar { /* owns child + stdin/stdout + id counter */ }
impl Sidecar {
    pub fn start(cfg: &Config) -> Result<Sidecar>;          // spawn ml/serve.py
    pub fn ping(&mut self) -> Result<()>;
    pub fn status(&mut self) -> Result<StatusPayload>;
    pub fn roster(&mut self) -> Result<Vec<String>>;
    pub fn predict(&mut self, a: &str, b: &str,
                   snap_a: Option<SnapSelector>, snap_b: Option<SnapSelector>)
                   -> Result<PredictResult>;
    pub fn timeline(&mut self, name: &str) -> Result<Vec<SnapshotMeta>>;
    pub fn reload(&mut self) -> Result<ReloadPayload>;
}
impl Drop for Sidecar { /* kills the child */ }
```
`ok:false` responses map to `Err(...)`. Requests get monotonically increasing ids;
read response lines until the matching `id` is seen. Spawn may inject OpenTelemetry
env (`OTEL_SERVICE_NAME=mma-sidecar`, `MMA_OTEL`, `MMA_OTEL_FILE`, and OTLP when
`:4318` is up). **JSON lines are unchanged.**

### `src/scraper.rs`
```rust
pub struct ScrapeOptions { pub full: bool, pub limit: Option<u32>, pub rate: Option<f64> }
pub fn run<F>(cfg: &Config, opts: &ScrapeOptions, on_line: F)
    -> Result<std::process::ExitStatus>
where F: FnMut(&str);
```
Maps `full→--full`, `limit→--limit N`, `rate→--rate R`. `on_line` is called per
streamed output line (stdout+stderr merged, newline-stripped). Blocks to exit.
The TUI enables OpenTelemetry on the scraper child (`OTEL_SERVICE_NAME=mma-scraper`,
`MMA_OTEL=1`, `MMA_OTEL_FILE=<repo>/telemetry/runs/tui-scraper.jsonl`, and
`OTEL_EXPORTER_OTLP_ENDPOINT` when `:4318` is reachable). Never sets
`MMA_OTEL_METRICS`. This is spawn env, not IPC.

### `src/stats_text.rs`
```rust
pub fn explain(stat_key: &str) -> &'static str;
pub fn describe(stat_key: &str, value: Option<f64>) -> String;
```

### `src/fuzzy.rs`
```rust
pub fn rank(names: &[String], query: &str) -> Vec<String>;          // empty query → all
pub fn rank_scored(names: &[String], query: &str) -> Vec<(String, i64)>;
```

### `src/app.rs`
```rust
pub enum Screen { Home, Fighters, Predict, Events, Scrape, Model }
pub struct FightersState { query, filtered, selected, profile, fights }
pub enum PredictSlot { A, B }
pub struct PredictState { slot, name_a, name_b, query, candidates, selected, result, error }
pub struct EventsState { events, selected, fights, rounds }
pub struct ScrapeState { running, log, full, limit, rate }
pub struct ModelState { model_loaded, n_fighters, metrics, model_path }
pub struct App {
    pub config: Config, pub db: Db, pub sidecar: Sidecar,
    pub screen: Screen, pub should_quit: bool, pub status_line: Option<String>,
    pub summary: Option<DbSummary>, pub roster: Vec<String>,
    pub fighters: FightersState, pub predict: PredictState,
    pub events: EventsState, pub scrape: ScrapeState, pub model: ModelState,
}
impl App {
    pub fn new(config: Config, db: Db, sidecar: Sidecar) -> Result<App>;
    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Result<()>;
    pub fn on_tick(&mut self) -> Result<()>;
    pub fn goto(&mut self, screen: Screen) -> Result<()>;
    pub fn should_quit(&self) -> bool;
}
```

### `src/ui/`
```rust
// src/ui/mod.rs
pub fn draw(frame: &mut ratatui::Frame, app: &App);
// src/ui/{home,fighters,predict,events,scrape,model}.rs
pub fn render(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App);
```

### `src/main.rs`
Owns terminal setup (`ratatui::init` / `ratatui::restore`) and the event loop
(`event::poll(TICK)` → `app.on_key` / `app.on_tick`, `terminal.draw(|f| ui::draw(f, app))`).
