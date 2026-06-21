//! Shared data model — the SINGLE source of truth for every type that crosses a
//! module boundary or the IPC boundary to the Python sidecar.
//!
//! FROZEN: Phase-2 build agents import these types and MUST NOT change them.
//! - DB row structs mirror `data/ufc.db` columns (see docs/SCHEMA_CONTRACT.md).
//!   Nullable numerics are `Option<T>`; percentages are 0..1 fractions; inches /
//!   lbs / seconds per the contract; dates/DOB are ISO `YYYY-MM-DD` text.
//! - `TaleOfTape` / `PredictResult` mirror Python `predict()` output. Any field
//!   that can be NaN/Inf in Python is `Option<f64>` here (sidecar replaces
//!   NaN/Inf with JSON `null` before sending).
//! - The IPC request/response types match CONTRACT.md verbatim.

use serde::{Deserialize, Serialize};

// =========================================================================== //
// DB ROW MODELS  (mirror data/ufc.db — read-only via rusqlite in src/db.rs)
// =========================================================================== //

/// One row of the `fighters` table (the per-fighter feature matrix).
///
/// `nationality` defaults to the literal `"Unlisted"` in the DB but is modelled
/// as `Option<String>` for safety. `*_acc` / `*_def` and the career-average
/// REAL columns are 0..1 fractions where applicable and NULL when unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fighter {
    pub fighter_id: i64,
    pub name: String,
    pub nickname: Option<String>,
    pub nationality: Option<String>,
    pub height_in: Option<i64>,
    pub weight_lbs: Option<i64>,
    pub reach_in: Option<i64>,
    pub stance: Option<String>,
    /// ISO `YYYY-MM-DD`.
    pub date_of_birth: Option<String>,
    pub wins: i64,
    pub losses: i64,
    pub draws: i64,
    pub no_contests: i64,
    /// 0/1 in the DB.
    pub was_champion: i64,
    pub championship_bouts_won: i64,
    /// Strikes landed per minute.
    pub slpm: Option<f64>,
    /// Striking accuracy (0..1).
    pub str_acc: Option<f64>,
    /// Strikes absorbed per minute.
    pub sapm: Option<f64>,
    /// Striking defense (0..1).
    pub str_def: Option<f64>,
    /// Takedowns avg / 15 min.
    pub td_avg: Option<f64>,
    /// Takedown accuracy (0..1).
    pub td_acc: Option<f64>,
    /// Takedown defense (0..1).
    pub td_def: Option<f64>,
    /// Submission attempts avg / 15 min.
    pub sub_avg: Option<f64>,
}

/// One row of the `events` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub event_id: i64,
    pub title: String,
    /// ISO `YYYY-MM-DD`.
    pub date: Option<String>,
    pub location: Option<String>,
}

/// One row of the `fights` table.
///
/// `winner_name` / `loser_name` may be empty / NULL for a draw or no-contest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FightRow {
    pub fight_id: i64,
    pub event_id: Option<i64>,
    pub event_name: Option<String>,
    /// ISO `YYYY-MM-DD`.
    pub date: Option<String>,
    pub winner_name: Option<String>,
    pub loser_name: Option<String>,
    pub weight_class: Option<String>,
    /// 0/1 in the DB.
    pub title_bout: i64,
    pub method: Option<String>,
    pub round_ended: i64,
    /// Seconds.
    pub time_ended: i64,
    pub referee: Option<String>,
}

/// One row of the `round_stats` table — one WIDE row per (fight × fighter × round).
///
/// Percentage (`*_pct`) columns are 0..1 fractions. Counters default to 0 in the
/// DB (NOT NULL) so they are plain integers here. `control_time` is in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundStat {
    pub round_stat_id: i64,
    pub fight_id: Option<i64>,
    pub fighter_name: Option<String>,
    /// `'w'` / `'l'` / `'d'`.
    pub result: Option<String>,
    pub round_number: Option<i64>,
    pub knockdowns: i64,
    pub sub_attempts: i64,
    pub reversals: i64,
    /// Seconds.
    pub control_time: i64,
    // takedowns
    pub td_landed: i64,
    pub td_attempted: i64,
    pub td_pct: f64,
    // significant strikes
    pub sig_str_landed: i64,
    pub sig_str_attempted: i64,
    pub sig_str_pct: f64,
    // total strikes
    pub total_str_landed: i64,
    pub total_str_attempted: i64,
    pub total_str_pct: f64,
    // by target
    pub head_landed: i64,
    pub head_attempted: i64,
    pub head_pct: f64,
    pub body_landed: i64,
    pub body_attempted: i64,
    pub body_pct: f64,
    pub leg_landed: i64,
    pub leg_attempted: i64,
    pub leg_pct: f64,
    // by position
    pub distance_landed: i64,
    pub distance_attempted: i64,
    pub distance_pct: f64,
    pub clinch_landed: i64,
    pub clinch_attempted: i64,
    pub clinch_pct: f64,
    pub ground_landed: i64,
    pub ground_attempted: i64,
    pub ground_pct: f64,
}

/// The newest numbered UFC card, used as the home-screen fight-poster headline.
///
/// `number` is the parsed card number (e.g. `311` from `"UFC 311"`) when the
/// title is a numbered card, or `None` when this is the fallback newest event of
/// any kind (`"UFC Fight Night"`, etc.). See `db::latest_numbered_card`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatestCard {
    pub event_id: i64,
    pub title: String,
    /// ISO `YYYY-MM-DD`.
    pub date: Option<String>,
    pub location: Option<String>,
    /// Parsed card number (e.g. 311), or `None` for the non-numbered fallback.
    pub number: Option<u32>,
}

/// Lightweight DB-wide counts for the Home / Model screens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DbSummary {
    pub n_fighters: i64,
    pub n_events: i64,
    pub n_fights: i64,
    pub n_round_stats: i64,
    /// Earliest event date seen (ISO `YYYY-MM-DD`), if any.
    pub earliest_event: Option<String>,
    /// Latest event date seen (ISO `YYYY-MM-DD`), if any.
    pub latest_event: Option<String>,
}

// =========================================================================== //
// PREDICTION MODELS  (mirror ml/predict.py predict() output)
// =========================================================================== //

/// Per-fighter "tale of the tape" — mirrors `_tale_of_tape()` in predict.py.
///
/// Every numeric is `Option<f64>` because the Python side can emit NaN (missing
/// reach / height / age / elo); the sidecar converts NaN/Inf to JSON `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaleOfTape {
    pub elo: Option<f64>,
    pub age: Option<f64>,
    /// Formatted record string, e.g. `"23-1"`.
    pub record: Option<String>,
    pub reach_in: Option<f64>,
    pub height_in: Option<f64>,
    pub stance: Option<String>,
    pub recent_winrate: Option<f64>,
    pub form_delta: Option<f64>,
    pub layoff_days: Option<f64>,
    pub divisions: Vec<String>,
}

/// Full result of a `predict` call — mirrors the dict returned by
/// `predict(name_a, name_b)` in predict.py (NaN/Inf already replaced by null).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictResult {
    pub name_a: String,
    pub name_b: String,
    pub allowed: bool,
    /// Refusal / low-confidence explanation, `None` when allowed cleanly.
    pub reason: Option<String>,
    pub prob_a: Option<f64>,
    pub prob_b: Option<f64>,
    #[serde(default)]
    pub low_confidence: bool,
    /// Minimum division distance over the shared gender ladder.
    pub distance: Option<i64>,
    #[serde(default)]
    pub tale_a: Option<TaleOfTape>,
    #[serde(default)]
    pub tale_b: Option<TaleOfTape>,
    /// Name of the best model, e.g. `"logreg"` / `"gboost"`.
    pub model: Option<String>,
    pub test_accuracy: Option<f64>,
}

// =========================================================================== //
// SIDECAR IPC  (newline-delimited JSON over ml/serve.py stdin/stdout)
// =========================================================================== //

/// A request sent Rust -> sidecar. Serializes to one compact JSON object with an
/// integer `id` and a string `cmd` plus command-specific fields (CONTRACT.md).
///
/// On the wire:
/// - `{"id":N,"cmd":"ping"}`
/// - `{"id":N,"cmd":"status"}`
/// - `{"id":N,"cmd":"roster"}`
/// - `{"id":N,"cmd":"eligibility"}`
/// - `{"id":N,"cmd":"predict","a":"<name>","b":"<name>"}`
/// - `{"id":N,"cmd":"reload"}`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidecarRequest {
    pub id: u64,
    #[serde(flatten)]
    pub cmd: SidecarCommand,
}

/// The command discriminant + payload. `cmd` is the serde tag; predict carries
/// `a` / `b`. Lowercase tag values match the IPC contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum SidecarCommand {
    Ping,
    Status,
    Roster,
    /// Fetch the eligibility POLICY (`rules`) + per-fighter division metadata
    /// (`divisions`) ONCE at startup. Thereafter the TUI filters eligible
    /// opponents LOCALLY (no per-selection round-trip).
    Eligibility,
    Predict { a: String, b: String },
    Reload,
}

/// A response received sidecar -> Rust. Always echoes the request `id` and
/// carries `ok`. On success the command-specific payload is flattened in; on
/// failure `error` holds a human-readable message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidecarResponse {
    pub id: u64,
    pub ok: bool,
    /// Present (and meaningful) when `ok == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The successful payload, untyped so callers can deserialize into the right
    /// concrete type per command (see typed accessors below).
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

/// Typed payload for `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusPayload {
    pub model_loaded: bool,
    pub n_fighters: i64,
    /// Free-form metrics dict from the model payload, or null.
    pub metrics: Option<serde_json::Value>,
    pub model_path: String,
}

/// Typed payload for `roster`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RosterPayload {
    pub fighters: Vec<String>,
}

/// Typed payload for `predict`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictPayload {
    pub result: PredictResult,
}

/// Typed payload for `reload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReloadPayload {
    pub model_loaded: bool,
    pub n_fighters: i64,
}

// =========================================================================== //
// ELIGIBILITY  (policy + per-fighter divisions, fetched ONCE at startup)
// =========================================================================== //

/// One division a fighter has fought in: a `(gender, ordinal)` pair on one of the
/// two ordinal ladders. The wire form is a 2-element JSON array `["M", 6]`, so
/// this is a serde TUPLE struct (serializes/deserializes as `["M", 6]`). The
/// ladder NAMES + the ordinal meanings live ONLY in Python — Rust only ever
/// compares the ordinals it was handed.
///
/// `.0` is the gender (`"M"` / `"W"`); `.1` is the division's rank on that
/// gender's ladder. Use [`Division::gender`] / [`Division::ordinal`] for clarity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Division(pub String, pub i32);

impl Division {
    /// The gender tag (`"M"` or `"W"`).
    pub fn gender(&self) -> &str {
        &self.0
    }

    /// The division's ordinal rank on its gender ladder.
    pub fn ordinal(&self) -> i32 {
        self.1
    }
}

/// The eligibility POLICY shipped by the sidecar at startup. Every threshold/flag
/// the local filter applies comes from HERE — there are NO hardcoded policy
/// values anywhere in Rust (the single source of truth is `ml/predict.py`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligibilityRules {
    /// A matchup is allowed iff the MIN division distance over the shared gender
    /// ladder is `<= max_distance`.
    pub max_distance: i32,
    /// Whether a matchup may cross the men's / women's ladders.
    pub allow_cross_gender: bool,
    /// Whether a fighter with NO resolvable division may still be matched.
    pub allow_unknown_division: bool,
}

/// Typed payload for `eligibility`: the policy `rules` plus the per-fighter
/// `divisions` map (`name -> [["M",6], ...]`, empty Vec when none resolvable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EligibilityPayload {
    pub rules: EligibilityRules,
    pub divisions: std::collections::HashMap<String, Vec<Division>>,
}

/// GENERIC eligibility decision — applies the fetched `rules` to two fighters'
/// division sets. Contains NO hardcoded policy values: every threshold/flag is
/// read from `rules`, and the only thing compared is ordinals handed to us by
/// Python. This mirrors `gate_matchup` in `ml/predict.py` exactly.
///
/// - either side has NO divisions -> `rules.allow_unknown_division`
/// - no gender shared by BOTH sides -> `rules.allow_cross_gender`
/// - else true iff MIN `|ord_a - ord_b|` over the shared gender(s)
///   `<= rules.max_distance`
pub fn eligible(divs_a: &[Division], divs_b: &[Division], rules: &EligibilityRules) -> bool {
    if divs_a.is_empty() || divs_b.is_empty() {
        return rules.allow_unknown_division;
    }

    // Genders present on BOTH sides (the shared ladders we can measure across).
    let shared: Vec<&str> = divs_a
        .iter()
        .map(|d| d.gender())
        .filter(|g| divs_b.iter().any(|d| d.gender() == *g))
        .collect();
    if shared.is_empty() {
        return rules.allow_cross_gender;
    }

    // Minimum ordinal distance over the shared gender ladder(s).
    let mut best: Option<i32> = None;
    for g in &shared {
        for a in divs_a.iter().filter(|d| d.gender() == *g) {
            for b in divs_b.iter().filter(|d| d.gender() == *g) {
                let dist = (a.ordinal() - b.ordinal()).abs();
                best = Some(best.map_or(dist, |cur| cur.min(dist)));
            }
        }
    }
    best.map(|d| d <= rules.max_distance).unwrap_or(false)
}
