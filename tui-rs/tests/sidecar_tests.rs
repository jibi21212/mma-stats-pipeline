//! Offline integration tests for the Python-sidecar client (`src/sidecar.rs`).
//!
//! These DO NOT depend on the real model, `ml/predict.py`, or `ml/serve.py`.
//! Instead each test injects a STUB process (a small inline Python script) that
//! speaks the same newline-delimited JSON protocol and emits canned responses.
//! The stub is wired in via `Sidecar::from_command`, so the transport logic is
//! exercised in full isolation.
//!
//! The crate is a pure binary (no `[lib]` target; `main.rs` declares its modules
//! privately), so an external test cannot `use mma_tui::sidecar`. We instead
//! `include!` the module sources directly into this test crate. `sidecar.rs`
//! references `crate::config` and `crate::models`, so we mirror that exact module
//! tree here (the test crate root becomes the `crate` root for these includes),
//! which compiles and tests the very same code.
//!
//! Coverage:
//! - requests reach the stub as well-formed one-line JSON (the stub `json.loads`
//!   each line; malformed framing would make it raise and answer nothing),
//! - every command's typed payload parses (`ping`/`status`/`roster`/`predict`/`reload`),
//! - request/response `id` correlation works even when the stub interleaves a
//!   decoy line bearing a different id,
//! - an `ok:false` response is surfaced to the caller as an `Err`,
//! - a sidecar that exits without answering is handled gracefully (an `Err`, no
//!   panic / hang).

// Mirror the crate's module tree so `crate::config` / `crate::models` resolve.
#[path = "../src/config.rs"]
mod config;
#[path = "../src/models.rs"]
mod models;
#[path = "../src/sidecar.rs"]
mod sidecar;

use std::process::Command;

use config::{Config, ScraperLaunch, SidecarLaunch};
use models::TaleOfTape;
use sidecar::Sidecar;

/// Locate a usable Python interpreter for the stub process.
///
/// Prefers `$MMA_PYTHON`, then the repo `.venv` python (present for this repo),
/// then falls back to `python3` on PATH.
fn python() -> String {
    if let Ok(p) = std::env::var("MMA_PYTHON")
        && !p.is_empty()
    {
        return p;
    }
    // <repo>/.venv/bin/python3 relative to this crate (tui-rs/).
    let manifest = env!("CARGO_MANIFEST_DIR");
    if let Some(root) = std::path::Path::new(manifest).parent() {
        let p = root.join(".venv/bin/python3");
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    "python3".to_string()
}

/// Build a `Command` that runs `python -c <script>` as the sidecar stub.
fn stub_command(script: &str) -> Command {
    let mut cmd = Command::new(python());
    cmd.arg("-c").arg(script);
    cmd
}

/// A throwaway `Config` pointing every path at the stub interpreter, used to
/// prove `Sidecar::start` builds a working `python <script>` invocation through
/// the same plumbing as `from_command`. The "sidecar script" is just the inline
/// stub source written to a temp file.
fn stub_config_running(script_path: &std::path::Path) -> Config {
    let dir = script_path.parent().unwrap().to_path_buf();
    let python: std::path::PathBuf = python().into();
    Config {
        repo_root: dir.clone(),
        db_path: dir.join("ufc.db"),
        python: python.clone(),
        sidecar_script: script_path.to_path_buf(),
        sidecar: SidecarLaunch::Script {
            python,
            script: script_path.to_path_buf(),
        },
        ml_dir: dir.clone(),
        scraper_dir: dir.clone(),
        scraper: ScraperLaunch::GoRun { dir },
    }
}

/// A full-protocol stub: reads JSON-line requests on stdin and answers per `cmd`,
/// always echoing the request `id`. It mirrors `ml/serve.py` closely enough to
/// drive every client method.
///
/// Special inputs to exercise error paths:
/// - `predict` with `a == "__nomodel__"` -> `{"ok":false,"error":"model not trained"}`
/// - any command when env `STUB_NO_MODEL=1` and cmd==`roster` -> `ok:false`
///
/// For `status` the stub first emits a DECOY line with a different id to prove
/// the client correlates by id rather than blindly taking the first line.
const STUB: &str = r#"
import sys, json, os
no_model = os.environ.get('STUB_NO_MODEL') == '1'
for raw in sys.stdin:
    line = raw.rstrip('\n')
    if not line.strip():
        continue
    try:
        req = json.loads(line)
    except Exception:
        # Never crash on bad input; emit a generic error object.
        sys.stdout.write(json.dumps({"id": 0, "ok": False, "error": "bad json"}) + "\n")
        sys.stdout.flush()
        continue
    rid = req.get("id")
    cmd = req.get("cmd")
    if cmd == "ping":
        resp = {"id": rid, "ok": True}
    elif cmd == "status":
        # Emit a decoy line with a wrong id first to test id correlation.
        sys.stdout.write(json.dumps({"id": rid + 100000, "ok": True, "stray": True}) + "\n")
        sys.stdout.flush()
        resp = {"id": rid, "ok": True, "model_loaded": True, "n_fighters": 3,
                "metrics": {"test_accuracy": 0.71}, "model_path": "/tmp/predictor.joblib"}
    elif cmd == "roster":
        if no_model:
            resp = {"id": rid, "ok": False, "error": "model not trained"}
        else:
            resp = {"id": rid, "ok": True, "fighters": ["Jon Jones", "Stipe Miocic"]}
    elif cmd == "eligibility":
        if no_model:
            resp = {"id": rid, "ok": False, "error": "model not trained"}
        else:
            resp = {"id": rid, "ok": True,
                    "rules": {"max_distance": 1, "allow_cross_gender": False,
                              "allow_unknown_division": True},
                    "divisions": {"Jon Jones": [["M", 7]], "Stipe Miocic": [["M", 8]]}}
    elif cmd == "predict":
        if req.get("a") == "__nomodel__":
            resp = {"id": rid, "ok": False, "error": "model not trained"}
        else:
            resp = {"id": rid, "ok": True, "result": {
                "name_a": req.get("a"), "name_b": req.get("b"),
                "allowed": True, "reason": None,
                "prob_a": 0.62, "prob_b": 0.38,
                "low_confidence": False, "distance": 0,
                "tale_a": {"elo": 1600.0, "age": None, "record": "27-1",
                           "reach_in": 84.5, "height_in": None, "stance": "Orthodox",
                           "recent_winrate": 1.0, "form_delta": 0.2,
                           "layoff_days": 365.0, "divisions": ["Light Heavyweight", "Heavyweight"]},
                "tale_b": {"elo": 1500.0, "age": 41.0, "record": "20-4",
                           "reach_in": None, "height_in": 76.0, "stance": "Orthodox",
                           "recent_winrate": 0.6, "form_delta": -0.1,
                           "layoff_days": None, "divisions": ["Heavyweight"]},
                "model": "logreg", "test_accuracy": 0.71}}
    elif cmd == "reload":
        resp = {"id": rid, "ok": True, "model_loaded": True, "n_fighters": 3}
    else:
        resp = {"id": rid, "ok": False, "error": "unknown command"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

/// A stub that reads one line and then exits immediately WITHOUT answering, to
/// exercise the "process died mid-request" path (EOF on stdout).
const STUB_DIES: &str = r#"
import sys
sys.stdin.readline()
sys.exit(0)
"#;

/// A stub that exits before reading anything at all (broken pipe on write / EOF).
const STUB_EXITS_NOW: &str = r#"
import sys
sys.exit(0)
"#;

#[test]
fn ping_succeeds_over_stub() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    sc.ping().expect("ping should succeed");
    sc.ping().expect("second ping should succeed");
}

#[test]
fn well_formed_request_line_is_json_loadable_by_stub() {
    // This stub `json.loads` every incoming line and only answers if parsing
    // succeeds. A clean Ok therefore proves the client wrote a single, valid,
    // newline-terminated compact JSON object (no embedded newlines).
    const STRICT_STUB: &str = r#"
import sys, json
for raw in sys.stdin:
    line = raw.rstrip('\n')
    if not line.strip():
        continue
    req = json.loads(line)          # raises (=> no reply) if framing is wrong
    assert "\n" not in line          # single line on the wire
    assert isinstance(req["id"], int)
    assert isinstance(req["cmd"], str)
    sys.stdout.write(json.dumps({"id": req["id"], "ok": True}) + "\n")
    sys.stdout.flush()
"#;
    let mut sc = Sidecar::from_command(stub_command(STRICT_STUB)).expect("spawn strict stub");
    sc.ping().expect("ping proves well-formed request line");
    let _ = sc.predict("a", "b"); // predict carries extra fields; framing still valid
}

#[test]
fn status_parses_and_correlates_id_past_a_decoy_line() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let status = sc
        .status()
        .expect("status should succeed despite decoy line");
    assert!(status.model_loaded);
    assert_eq!(status.n_fighters, 3);
    assert_eq!(status.model_path, "/tmp/predictor.joblib");
    let metrics = status.metrics.expect("metrics present");
    assert_eq!(metrics["test_accuracy"], 0.71);
}

#[test]
fn roster_returns_fighter_names() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let names = sc.roster().expect("roster should succeed");
    assert_eq!(
        names,
        vec!["Jon Jones".to_string(), "Stipe Miocic".to_string()]
    );
}

#[test]
fn roster_surfaces_ok_false_as_err() {
    let mut cmd = stub_command(STUB);
    cmd.env("STUB_NO_MODEL", "1");
    let mut sc = Sidecar::from_command(cmd).expect("spawn stub");
    let err = sc
        .roster()
        .expect_err("roster must Err when model not trained");
    assert!(
        err.to_string().contains("model not trained"),
        "error should carry the sidecar message, got: {err}",
    );
}

#[test]
fn eligibility_returns_rules_and_divisions() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let payload = sc.eligibility().expect("eligibility should succeed");
    // RULES: the policy the TUI applies locally (no hardcoded values in Rust).
    assert_eq!(payload.rules.max_distance, 1);
    assert!(!payload.rules.allow_cross_gender);
    assert!(payload.rules.allow_unknown_division);
    // DIVISIONS: per-fighter [gender, ordinal] pairs deserialize into Division.
    let jj = payload.divisions.get("Jon Jones").expect("Jon Jones present");
    assert_eq!(jj.len(), 1);
    assert_eq!(jj[0].gender(), "M");
    assert_eq!(jj[0].ordinal(), 7);
    let stipe = payload.divisions.get("Stipe Miocic").expect("Stipe present");
    assert_eq!(stipe[0].ordinal(), 8);
}

#[test]
fn eligibility_rules_drive_local_filter() {
    // The GENERIC local filter (models::eligible) uses ONLY the fetched rules:
    // Jon Jones (M#7) vs Stipe (M#8) is distance 1 -> allowed at max_distance 1.
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let payload = sc.eligibility().expect("eligibility ok");
    let jj = payload.divisions["Jon Jones"].clone();
    let stipe = payload.divisions["Stipe Miocic"].clone();
    assert!(
        models::eligible(&jj, &stipe, &payload.rules),
        "M#7 vs M#8 (distance 1) must be eligible at max_distance 1",
    );
    // Tightening max_distance to 0 (rules-driven) must now REFUSE the same pair.
    let strict = models::EligibilityRules {
        max_distance: 0,
        ..payload.rules.clone()
    };
    assert!(
        !models::eligible(&jj, &stipe, &strict),
        "distance 1 must be refused once max_distance drops to 0",
    );
}

#[test]
fn eligibility_surfaces_model_not_trained_as_err() {
    let mut cmd = stub_command(STUB);
    cmd.env("STUB_NO_MODEL", "1");
    let mut sc = Sidecar::from_command(cmd).expect("spawn stub");
    let err = sc
        .eligibility()
        .expect_err("eligibility must Err when model not trained");
    assert!(
        err.to_string().contains("model not trained"),
        "error should carry the sidecar message, got: {err}",
    );
}

#[test]
fn predict_parses_full_result_with_nulls() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let res = sc
        .predict("Jon Jones", "Stipe Miocic")
        .expect("predict should succeed");

    assert_eq!(res.name_a, "Jon Jones");
    assert_eq!(res.name_b, "Stipe Miocic");
    assert!(res.allowed);
    assert_eq!(res.reason, None);
    assert_eq!(res.prob_a, Some(0.62));
    assert_eq!(res.prob_b, Some(0.38));
    assert!(!res.low_confidence);
    assert_eq!(res.distance, Some(0));
    assert_eq!(res.model.as_deref(), Some("logreg"));
    assert_eq!(res.test_accuracy, Some(0.71));

    let a: TaleOfTape = res.tale_a.expect("tale_a present");
    assert_eq!(a.elo, Some(1600.0));
    assert_eq!(a.age, None); // JSON null -> None
    assert_eq!(a.record.as_deref(), Some("27-1"));
    assert_eq!(a.reach_in, Some(84.5));
    assert_eq!(a.height_in, None); // JSON null -> None
    assert_eq!(a.stance.as_deref(), Some("Orthodox"));
    assert_eq!(
        a.divisions,
        vec!["Light Heavyweight".to_string(), "Heavyweight".to_string()],
    );

    let b: TaleOfTape = res.tale_b.expect("tale_b present");
    assert_eq!(b.reach_in, None); // JSON null -> None
    assert_eq!(b.layoff_days, None); // JSON null -> None
    assert_eq!(b.age, Some(41.0));
}

#[test]
fn predict_surfaces_ok_false_as_err() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let err = sc
        .predict("__nomodel__", "anyone")
        .expect_err("predict must Err when model not trained");
    assert!(
        err.to_string().contains("model not trained"),
        "error should carry the sidecar message, got: {err}",
    );
}

#[test]
fn reload_returns_load_state() {
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    let payload = sc.reload().expect("reload should succeed");
    assert!(payload.model_loaded);
    assert_eq!(payload.n_fighters, 3);
}

#[test]
fn many_sequential_calls_all_correlate() {
    // Each typed call requires its own id to be echoed back; a stale/mismatched
    // id would hang or Err. A clean run of mixed commands confirms ids advance
    // monotonically and correlate per request.
    let mut sc = Sidecar::from_command(stub_command(STUB)).expect("spawn stub");
    sc.ping().expect("call 1");
    let _ = sc.status().expect("call 2");
    let _ = sc.roster().expect("call 3");
    let _ = sc.predict("a", "b").expect("call 4");
    let _ = sc.reload().expect("call 5");
    sc.ping().expect("call 6");
}

#[test]
fn start_spawns_via_config_and_talks_to_stub() {
    // Write the stub to a temp file and point Config::sidecar_script at it, so
    // `Sidecar::start` builds `python <script>` and the same transport works.
    let dir = std::env::temp_dir().join(format!("mma_sidecar_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    let script = dir.join("stub_serve.py");
    std::fs::write(&script, STUB).expect("write stub script");

    let cfg = stub_config_running(&script);
    let mut sc = Sidecar::start(&cfg).expect("start should spawn the stub sidecar");
    sc.ping().expect("ping via started sidecar");
    let status = sc.status().expect("status via started sidecar");
    assert!(status.model_loaded);

    drop(sc);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn process_exit_after_request_is_handled_gracefully() {
    let mut sc = Sidecar::from_command(stub_command(STUB_DIES)).expect("spawn dying stub");
    // The stub reads our line then exits without answering -> EOF on stdout.
    let err = sc
        .ping()
        .expect_err("ping must Err when sidecar exits without answering");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("exit") || msg.contains("closed") || msg.contains("stream"),
        "error should explain the process died, got: {err}",
    );
}

#[test]
fn process_exit_before_reading_is_handled_gracefully() {
    let mut sc =
        Sidecar::from_command(stub_command(STUB_EXITS_NOW)).expect("spawn instantly-exiting stub");
    // The child may exit before/while we write; either the write fails (broken
    // pipe) or the read hits EOF. Both must surface as an Err, never panic/hang.
    let result = sc.ping();
    assert!(
        result.is_err(),
        "ping must Err when sidecar is already gone"
    );
}

// =========================================================================== //
// HERMETIC FIXTURE WIRING — exercises the REAL committed E2E fixtures
// (tests/fixtures/stub_sidecar.py) through the EXACT path the harness uses:
//   MMA_SIDECAR override -> Config::resolve -> SidecarLaunch::Executable ->
//   Sidecar::start. This guarantees the fixture stays in lock-step with the
//   Rust IPC model (PredictResult / TaleOfTape) so the harness assertions hold.
// =========================================================================== //

/// Absolute path to the committed stub sidecar fixture.
fn stub_sidecar_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub_sidecar.py")
}

/// Build a Config whose IPC sidecar is the real fixture, via the MMA_SIDECAR
/// override and the pure resolver (no global env mutation).
fn fixture_sidecar_config() -> Config {
    let fixture = stub_sidecar_fixture();
    let env = config::EnvVars {
        mma_sidecar: Some(fixture.clone().into_os_string()),
        ..Default::default()
    };
    // ml_dir (cwd for the child) just needs to exist; use the fixtures dir.
    let mut cfg = Config::resolve(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf(),
        &env,
    );
    cfg.ml_dir = fixture.parent().unwrap().to_path_buf();
    cfg
}

#[test]
fn mma_sidecar_override_resolves_to_executable_launch() {
    let cfg = fixture_sidecar_config();
    match &cfg.sidecar {
        SidecarLaunch::Executable(p) => assert_eq!(p, &stub_sidecar_fixture()),
        other => panic!("MMA_SIDECAR must resolve to Executable, got {other:?}"),
    }
}

#[test]
fn fixture_stub_sidecar_speaks_protocol_via_start() {
    // Drive the committed fixture exactly as main() / the harness would.
    let cfg = fixture_sidecar_config();
    let mut sc = Sidecar::start(&cfg).expect("start fixture stub sidecar via MMA_SIDECAR override");

    sc.ping().expect("ping ok");

    let status = sc.status().expect("status ok");
    assert!(status.model_loaded);
    assert_eq!(status.n_fighters, 3);

    let roster = sc.roster().expect("roster ok");
    assert_eq!(
        roster,
        vec![
            "Alex Pereira".to_string(),
            "Israel Adesanya".to_string(),
            "Robert Whittaker".to_string(),
        ]
    );

    let reload = sc.reload().expect("reload ok");
    assert!(reload.model_loaded);
    assert_eq!(reload.n_fighters, 3);
}

#[test]
fn fixture_stub_predict_matches_rust_model_exactly() {
    // The canned predict result must deserialize cleanly into PredictResult /
    // TaleOfTape with the documented deterministic values.
    let cfg = fixture_sidecar_config();
    let mut sc = Sidecar::start(&cfg).expect("start fixture stub sidecar");

    let res = sc
        .predict("Israel Adesanya", "Robert Whittaker")
        .expect("predict ok");

    assert_eq!(res.name_a, "Israel Adesanya");
    assert_eq!(res.name_b, "Robert Whittaker");
    assert!(res.allowed);
    assert_eq!(res.reason, None);
    assert_eq!(res.prob_a, Some(0.62));
    assert_eq!(res.prob_b, Some(0.38));
    assert!(!res.low_confidence);
    assert_eq!(res.distance, Some(0));
    assert_eq!(res.model.as_deref(), Some("stub"));
    assert_eq!(res.test_accuracy, Some(0.6));

    let a: TaleOfTape = res.tale_a.expect("tale_a present");
    assert_eq!(a.elo, Some(1650.0));
    assert_eq!(a.age, Some(34.0));
    assert_eq!(a.record.as_deref(), Some("24-3"));
    assert_eq!(a.reach_in, Some(80.0));
    assert_eq!(a.height_in, Some(76.0));
    assert_eq!(a.stance.as_deref(), Some("Orthodox"));
    assert_eq!(a.recent_winrate, Some(0.8));
    assert_eq!(a.form_delta, Some(0.2));
    assert_eq!(a.layoff_days, Some(180.0));
    assert_eq!(
        a.divisions,
        vec!["Middleweight".to_string(), "Light Heavyweight".to_string()]
    );

    let b: TaleOfTape = res.tale_b.expect("tale_b present");
    assert_eq!(b.elo, Some(1580.0));
    assert_eq!(b.reach_in, Some(73.5));
    assert_eq!(b.divisions, vec!["Middleweight".to_string()]);
}

/// Local eligibility filter over a roster: every name `b != a` whose matchup with
/// `a` the fetched `rules` allow, sorted. Mirrors `app::EligibilityState::
/// eligible_opponents` so this test exercises the SAME local-filter logic the TUI
/// uses (zero per-selection IPC).
fn local_eligible(
    a: &str,
    roster: &[&str],
    payload: &models::EligibilityPayload,
) -> Vec<String> {
    let empty: Vec<models::Division> = Vec::new();
    let divs_a = payload.divisions.get(a).unwrap_or(&empty);
    let mut out: Vec<String> = roster
        .iter()
        .filter(|n| **n != a)
        .filter(|n| {
            let divs_b = payload.divisions.get(**n).unwrap_or(&empty);
            models::eligible(divs_a, divs_b, &payload.rules)
        })
        .map(|n| n.to_string())
        .collect();
    out.sort();
    out
}

#[test]
fn fixture_stub_eligibility_filters_the_other_slot_locally() {
    // The load-bearing case: with the ONE startup-fetched eligibility payload, the
    // LOCAL filter for "Israel Adesanya" DROPS "Alex Pereira" (the canned M#8
    // ineligible, distance 2 > max_distance 1) and keeps only "Robert Whittaker".
    // This proves the OTHER predict slot is genuinely filtered by the policy, not
    // just "all-but-A".
    let cfg = fixture_sidecar_config();
    let mut sc = Sidecar::start(&cfg).expect("start fixture stub sidecar");

    let payload = sc.eligibility().expect("eligibility ok");
    // The policy ships from Python (no hardcoded values in Rust).
    assert_eq!(payload.rules.max_distance, 1);

    let roster = ["Alex Pereira", "Israel Adesanya", "Robert Whittaker"];

    let elig = local_eligible("Israel Adesanya", &roster, &payload);
    assert_eq!(elig, vec!["Robert Whittaker".to_string()]);
    assert!(
        !elig.contains(&"Alex Pereira".to_string()),
        "the ineligible opponent must be filtered out of the other slot"
    );
    assert!(
        !elig.contains(&"Israel Adesanya".to_string()),
        "A must be excluded from its own eligible list"
    );

    // The other two fighters exclude only A (no canned ineligibility).
    let pereira = local_eligible("Alex Pereira", &roster, &payload);
    // Pereira (M#8) is distance 2 from both Middleweights -> neither is eligible.
    assert!(
        pereira.is_empty(),
        "Pereira (M#8) is 2 divisions from both M#6 fighters -> no eligible foes"
    );

    // Whittaker (M#6) keeps only Adesanya (M#6, distance 0); Pereira is dropped.
    let whittaker = local_eligible("Robert Whittaker", &roster, &payload);
    assert_eq!(whittaker, vec!["Israel Adesanya".to_string()]);
}

// =========================================================================== //
// GENERIC LOCAL FILTER (models::eligible) — pure rules-driven logic, no IPC.
// Proves Rust holds NO hardcoded policy: every branch is driven by the rules arg.
// =========================================================================== //

fn div(g: &str, o: i32) -> models::Division {
    models::Division(g.to_string(), o)
}

fn rules(max: i32, cross: bool, unknown: bool) -> models::EligibilityRules {
    models::EligibilityRules {
        max_distance: max,
        allow_cross_gender: cross,
        allow_unknown_division: unknown,
    }
}

#[test]
fn division_round_trips_as_json_array() {
    // The wire form is a 2-element array; serde must (de)serialize Division as such.
    let d = div("M", 6);
    let text = serde_json::to_string(&d).unwrap();
    assert_eq!(text, r#"["M",6]"#);
    let back: models::Division = serde_json::from_str(r#"["W",3]"#).unwrap();
    assert_eq!(back.gender(), "W");
    assert_eq!(back.ordinal(), 3);
}

#[test]
fn eligible_distance_respects_max_distance_rule() {
    let a = [div("M", 6)];
    let b = [div("M", 7)]; // distance 1
    assert!(models::eligible(&a, &b, &rules(1, false, true)));
    assert!(!models::eligible(&a, &b, &rules(0, false, true)));
    let far = [div("M", 8)]; // distance 2
    assert!(!models::eligible(&a, &far, &rules(1, false, true)));
    assert!(models::eligible(&a, &far, &rules(2, false, true)));
}

#[test]
fn eligible_cross_gender_follows_the_rule() {
    let m = [div("M", 2)];
    let w = [div("W", 3)]; // different ladders -> no shared gender
    assert!(!models::eligible(&m, &w, &rules(1, false, true)));
    assert!(models::eligible(&m, &w, &rules(1, true, true)));
}

#[test]
fn eligible_unknown_division_follows_the_rule() {
    let known = [div("M", 4)];
    let none: [models::Division; 0] = [];
    assert!(models::eligible(&none, &known, &rules(1, false, true)));
    assert!(!models::eligible(&none, &known, &rules(1, false, false)));
    // Both unknown is also governed by the unknown-division rule.
    assert!(models::eligible(&none, &none, &rules(1, false, true)));
}
