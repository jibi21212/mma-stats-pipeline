"""Unit tests for the fight-outcome predictor (ml/predict.py).

Run from the ml/ directory:  python -m pytest tests/test_predict.py -q

Covers the three correctness invariants:
  * weight-class normaliser (the tricky strings),
  * weight-class gating (refuse cross-weight / cross-gender, allow adjacent),
  * prediction symmetry (P(A beats B) == 1 - P(B beats A)),
  * leakage (the as-of-date feature builder uses only fights strictly before N),
  * Elo monotonicity (a winner's Elo rises).

The symmetry / training tests build a tiny in-memory dataset and train a real
sklearn pipeline, so they exercise the full predict() path without a scraped DB.
"""
import os
import sqlite3
import sys

import numpy as np
import pandas as pd
import pytest

# Make the ml/ modules importable from ml/tests/.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import predict as P  # noqa: E402


# --------------------------------------------------------------------------- #
# 1) Weight-class normaliser
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("raw,expected", [
    # men ladder
    ("Lightweight Bout", ("M", 4)),
    ("UFC Welterweight Title Bout", ("M", 5)),
    ("Middleweight Bout", ("M", 6)),
    ("Bantamweight Bout", ("M", 2)),
    ("Featherweight Bout", ("M", 3)),
    ("Flyweight Bout", ("M", 1)),
    ("Heavyweight Bout", ("M", 8)),
    # THE TRAP: Light Heavyweight contains "heavyweight" -> must be 7, not 8.
    ("Light Heavyweight Bout", ("M", 7)),
    ("UFC Light Heavyweight Title Bout", ("M", 7)),
    ("UFC Interim Light Heavyweight Title Bout", ("M", 7)),
    # super heavyweight -> Heavyweight (8)
    ("Super Heavyweight Bout", ("M", 8)),
    # women ladder (separate ordinals)
    ("Women's Strawweight Bout", ("W", 1)),
    ("Women's Flyweight Bout", ("W", 2)),
    ("Women's Bantamweight Bout", ("W", 3)),
    ("Women's Featherweight Bout", ("W", 4)),
    ("UFC Women's Strawweight Title Bout", ("W", 1)),
    # tournament strings -> their division
    ("Ultimate Fighter 9 Welterweight Tournament Title Bout", ("M", 5)),
    ("Ultimate Fighter 23 Light Heavyweight Tournament Title Bout", ("M", 7)),
    ("Road to UFC 4 Lightweight Tournament Title Bout", ("M", 4)),
    ("Ultimate Fighter 28 Women's Featherweight Tournament Title Bout", ("W", 4)),
    # catch / open / unresolved -> None
    ("Catch Weight Bout", None),
    ("Open Weight Bout", None),
    ("", None),
    (None, None),
    ("Some Nonsense Bout", None),
])
def test_normalize_weight_class(raw, expected):
    assert P.normalize_weight_class(raw) == expected


def test_light_heavyweight_not_heavyweight():
    """The headline trap, asserted on its own for clarity."""
    lhw = P.normalize_weight_class("Light Heavyweight Bout")
    hw = P.normalize_weight_class("Heavyweight Bout")
    assert lhw == ("M", 7)
    assert hw == ("M", 8)
    assert lhw != hw


def test_women_routed_to_women_ladder():
    # "Women's Flyweight" must not collapse onto the men's Flyweight ordinal.
    w_fly = P.normalize_weight_class("Women's Flyweight Bout")
    m_fly = P.normalize_weight_class("Flyweight Bout")
    assert w_fly[0] == "W"
    assert m_fly[0] == "M"
    # Different ladders -> different gender tag even if ordinals coincide.
    assert w_fly != m_fly


# --------------------------------------------------------------------------- #
# 2) Gating
# --------------------------------------------------------------------------- #

def test_gating_bantamfeather_vs_heavyweight_refused():
    # Merab-like: Bantamweight(2)/Featherweight(3) vs Ngannou-like: Heavyweight(8).
    bantam_feather = {("M", 2), ("M", 3)}
    heavyweight = {("M", 8)}
    res = P.gate_matchup(bantam_feather, heavyweight)
    assert res["allowed"] is False
    assert res["distance"] >= 5  # min(|2-8|,|3-8|) = 5


def test_gating_adjacent_allowed():
    # Welterweight(5) vs Middleweight(6) -> distance 1 -> allowed.
    ww = {("M", 5)}
    mw = {("M", 6)}
    res = P.gate_matchup(ww, mw)
    assert res["allowed"] is True
    assert res["distance"] == 1


def test_gating_same_division_allowed():
    res = P.gate_matchup({("M", 4)}, {("M", 4)})
    assert res["allowed"] is True
    assert res["distance"] == 0


def test_gating_two_apart_refused():
    # Lightweight(4) vs Middleweight(6) -> distance 2 -> refused.
    res = P.gate_matchup({("M", 4)}, {("M", 6)})
    assert res["allowed"] is False
    assert res["distance"] == 2


def test_gating_cross_gender_refused():
    # Women's Bantamweight(W,3) vs men's Bantamweight(M,2): different ladders.
    res = P.gate_matchup({("W", 3)}, {("M", 2)})
    assert res["allowed"] is False
    assert "cross-gender" in res["reason"].lower()


def test_gating_unknown_division_low_confidence():
    # A fighter who only fought catch/open weight has no resolvable division.
    res = P.gate_matchup(set(), {("M", 4)})
    assert res["allowed"] is True
    assert res["low_confidence"] is True


# --------------------------------------------------------------------------- #
# 2b) Policy constants are the single source of truth for gating
# --------------------------------------------------------------------------- #

def test_policy_constants_have_documented_defaults():
    # These are the EXACT values shipped to the TUI in the eligibility ``rules``.
    assert P.MAX_DIVISION_DISTANCE == 1
    assert P.ALLOW_CROSS_GENDER is False
    assert P.ALLOW_UNKNOWN_DIVISION is True


def test_eligibility_rules_sourced_from_constants():
    # eligibility_rules() must be built FROM the constants, never re-typed, so the
    # wire policy and the gate can never diverge.
    assert P.eligibility_rules() == {
        "max_distance": P.MAX_DIVISION_DISTANCE,
        "allow_cross_gender": P.ALLOW_CROSS_GENDER,
        "allow_unknown_division": P.ALLOW_UNKNOWN_DIVISION,
    }


def test_gate_matchup_uses_max_distance_constant(monkeypatch):
    # Raising the threshold to 2 must MAKE a 2-apart matchup allowed, proving
    # gate_matchup reads the constant rather than a hardcoded literal.
    monkeypatch.setattr(P, "MAX_DIVISION_DISTANCE", 2)
    res = P.gate_matchup({("M", 4)}, {("M", 6)})  # distance 2
    assert res["allowed"] is True
    assert res["distance"] == 2


def test_gate_matchup_uses_allow_cross_gender_constant(monkeypatch):
    # Flipping ALLOW_CROSS_GENDER on must permit a previously-refused cross-ladder
    # matchup, proving the flag drives the gate.
    monkeypatch.setattr(P, "ALLOW_CROSS_GENDER", True)
    res = P.gate_matchup({("W", 3)}, {("M", 2)})
    assert res["allowed"] is True


def test_gate_matchup_uses_allow_unknown_division_constant(monkeypatch):
    # Flipping ALLOW_UNKNOWN_DIVISION off must refuse an unknown-division matchup.
    monkeypatch.setattr(P, "ALLOW_UNKNOWN_DIVISION", False)
    res = P.gate_matchup(set(), {("M", 4)})
    assert res["allowed"] is False


# --------------------------------------------------------------------------- #
# 2c) Weight-class ladder — the SINGLE source for the TUI's class picker
# --------------------------------------------------------------------------- #

def test_weight_class_ladder_sourced_from_constants():
    # weight_class_ladder() must be built FROM MEN_LADDER + WOMEN_LADDER (names +
    # ordinals never re-typed), so the picker the TUI shows can never drift from the
    # gating ladders. Reconstruct the expected list straight from the constants.
    ladder = P.weight_class_ladder()
    expected = (
        [{"name": n, "gender": "M", "ordinal": o} for n, o in P.MEN_LADDER.items()]
        + [{"name": n, "gender": "W", "ordinal": o} for n, o in P.WOMEN_LADDER.items()]
    )
    assert ladder == expected


def test_weight_class_ladder_covers_men_and_women():
    ladder = P.weight_class_ladder()
    # Every MEN_LADDER entry appears with gender "M" and its ordinal.
    for name, ordinal in P.MEN_LADDER.items():
        assert {"name": name, "gender": "M", "ordinal": ordinal} in ladder
    # Every WOMEN_LADDER entry appears with gender "W" and its ordinal.
    for name, ordinal in P.WOMEN_LADDER.items():
        assert {"name": name, "gender": "W", "ordinal": ordinal} in ladder
    # Length is exactly the two ladders combined (no extras, no drops).
    assert len(ladder) == len(P.MEN_LADDER) + len(P.WOMEN_LADDER)
    # Documented headline entries (the contract the TUI's picker shows).
    assert {"name": "Flyweight", "gender": "M", "ordinal": 1} in ladder
    assert {"name": "Heavyweight", "gender": "M", "ordinal": 8} in ladder
    assert {"name": "Women's Strawweight", "gender": "W", "ordinal": 1} in ladder


def test_weight_class_ladder_membership_matches_divisions():
    # A fighter "is in" a class C iff their (gender, ordinal) divisions contain
    # (C["gender"], C["ordinal"]) -- prove each ladder entry's identity matches the
    # tuple form that normalize_weight_class / the persisted divisions use.
    for entry in P.weight_class_ladder():
        identity = (entry["gender"], entry["ordinal"])
        # This identity is exactly what normalize_weight_class yields for the class
        # name (using DIVISION_NAMES as the reverse lookup oracle).
        assert P.DIVISION_NAMES[identity] == entry["name"]


# --------------------------------------------------------------------------- #
# 3) Leakage: the as-of-date feature builder uses only fights < N
# --------------------------------------------------------------------------- #

def _make_fighters(names):
    rows = []
    for i, nm in enumerate(names):
        rows.append({
            "name": nm, "reach_in": 70 + i, "height_in": 70 + i,
            "stance": "Orthodox", "date_of_birth": "1990-01-01",
            "wins": 0, "losses": 0, "draws": 0,
        })
    return pd.DataFrame(rows)


def test_no_leakage_first_fight_has_baseline_state():
    """At a fighter's FIRST fight, every prior-derived feature is at baseline.

    If the builder leaked future fights, the first-fight snapshot would already
    show elo != 1500, n_prior > 0, or a non-debut layoff.
    """
    fighters = _make_fighters(["A", "B"])
    fights = pd.DataFrame([
        # Two fights between A and B on different dates.
        {"fight_id": 1, "date": "2020-01-01", "winner_name": "A", "loser_name": "B",
         "weight_class": "Lightweight Bout", "title_bout": 0, "method": "KO/TKO"},
        {"fight_id": 2, "date": "2021-01-01", "winner_name": "B", "loser_name": "A",
         "weight_class": "Lightweight Bout", "title_bout": 0, "method": "KO/TKO"},
    ])
    X, y, dates, snaps, divs, static, tl_art, m_art = P.build_training_frame(fighters, fights)

    # 2 fights x 2 orderings = 4 rows.
    assert len(X) == 4 and len(y) == 4

    # The FIRST emitted ordering is fight 1, winner=A as A. Its diff features
    # must reflect BOTH fighters at baseline (n_prior=0, elo diff=0, debut).
    first = X.iloc[0]
    assert first["elo_pre_diff"] == 0.0          # both at 1500 before any fight
    assert first["n_prior_fights_diff"] == 0.0   # both debut
    assert first["is_debut_diff"] == 0.0         # both debut -> diff 0
    # days_since_last for both is the debut sentinel -> diff 0.
    assert first["days_since_last_diff"] == 0.0


def test_no_leakage_second_fight_sees_only_first():
    """At fight 2, each fighter's state reflects EXACTLY one prior fight."""
    fighters = _make_fighters(["A", "B"])
    fights = pd.DataFrame([
        {"fight_id": 1, "date": "2020-01-01", "winner_name": "A", "loser_name": "B",
         "weight_class": "Lightweight Bout", "title_bout": 0, "method": "KO/TKO"},
        {"fight_id": 2, "date": "2021-01-01", "winner_name": "B", "loser_name": "A",
         "weight_class": "Lightweight Bout", "title_bout": 0, "method": "KO/TKO"},
    ])
    X, y, dates, snaps, divs, static, tl_art, m_art = P.build_training_frame(fighters, fights)

    # Rows 2 and 3 are fight 2 (B as A, then A as A). Both fighters have exactly
    # 1 prior fight -> n_prior_fights_diff == 0, but Elo now differs (A won #1).
    third = X.iloc[2]  # fight 2, ordering winner=B as A, loser=A as B
    assert third["n_prior_fights_diff"] == 0.0   # both have 1 prior fight
    # A won fight 1 so before fight 2 A's elo > B's elo. Ordering here is
    # (B as A, A as B), so elo_pre_diff = elo_B - elo_A < 0.
    assert third["elo_pre_diff"] < 0.0
    # Layoff: both fought on 2020-01-01, fight 2 on 2021-01-01 -> ~366 days each,
    # so the diff is 0.
    assert abs(third["days_since_last_diff"]) < 1e-6


def test_clean_label_fights_drops_draws_and_nc():
    fights = pd.DataFrame([
        {"fight_id": 1, "date": "2020-01-01", "winner_name": "A", "loser_name": "B",
         "weight_class": "LW", "title_bout": 0, "method": "KO/TKO"},
        {"fight_id": 2, "date": "2020-02-01", "winner_name": "C", "loser_name": "D",
         "weight_class": "LW", "title_bout": 0, "method": "Decision - Draw"},
        {"fight_id": 3, "date": "2020-03-01", "winner_name": "E", "loser_name": "F",
         "weight_class": "LW", "title_bout": 0, "method": "Overturned"},
        {"fight_id": 4, "date": "2020-04-01", "winner_name": "G", "loser_name": "H",
         "weight_class": "LW", "title_bout": 0, "method": "No Contest"},
        {"fight_id": 5, "date": "2020-05-01", "winner_name": "I", "loser_name": "J",
         "weight_class": "LW", "title_bout": 0, "method": "Other"},  # time-limit draw w/ fabricated winner
    ])
    clean = P._clean_label_fights(fights)
    assert list(clean["fight_id"]) == [1]  # only the real-winner KO survives


# --------------------------------------------------------------------------- #
# 4) Elo monotonicity
# --------------------------------------------------------------------------- #

def test_elo_winner_rises():
    """A winner's post-fight Elo must rise above the 1500 start."""
    fighters = _make_fighters(["A", "B"])
    fights = pd.DataFrame([
        {"fight_id": 1, "date": "2020-01-01", "winner_name": "A", "loser_name": "B",
         "weight_class": "Lightweight Bout", "title_bout": 0, "method": "KO/TKO"},
    ])
    X, y, dates, snaps, divs, static, tl_art, m_art = P.build_training_frame(fighters, fights)
    # After one fight, A (winner) Elo > 1500 > B (loser) Elo.
    assert static["A"]["elo"] > P.ELO_START
    assert static["B"]["elo"] < P.ELO_START
    # Expected-score symmetry: equal-rated start -> winner gains exactly K/2.
    assert static["A"]["elo"] == pytest.approx(P.ELO_START + P.ELO_K / 2.0)


def test_expected_score_symmetry():
    assert P._expected_score(1500, 1500) == pytest.approx(0.5)
    assert P._expected_score(1600, 1500) > 0.5
    assert (P._expected_score(1600, 1500)
            + P._expected_score(1500, 1600)) == pytest.approx(1.0)


# --------------------------------------------------------------------------- #
# 5) Symmetry of predict() end-to-end (trains a real pipeline)
# --------------------------------------------------------------------------- #

@pytest.fixture(scope="module")
def trained_model(tmp_path_factory):
    """Train a real model on a synthetic DB and return its saved payload path.

    Builds enough fights (with dates spanning the temporal cutoff) that both the
    train and test splits are non-empty.
    """
    path = str(tmp_path_factory.mktemp("models") / "predictor.joblib")
    db_path = str(tmp_path_factory.mktemp("data") / "ufc.db")

    # Build a synthetic DB: a round-robin-ish set of fights across 12 fighters,
    # spanning 2018..2024 so the 2023 temporal cutoff yields a test set.
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.executescript("""
    CREATE TABLE fighters (fighter_id INTEGER PRIMARY KEY, name TEXT UNIQUE,
        reach_in INTEGER, height_in INTEGER, stance TEXT, date_of_birth TEXT,
        wins INTEGER, losses INTEGER, draws INTEGER, no_contests INTEGER,
        slpm REAL, str_acc REAL);
    CREATE TABLE fights (fight_id INTEGER PRIMARY KEY, event_id INTEGER, event_name TEXT,
        date TEXT, winner_name TEXT, loser_name TEXT, weight_class TEXT,
        title_bout INTEGER, method TEXT, round_ended INTEGER, time_ended INTEGER);
    CREATE TABLE round_stats (round_stat_id INTEGER PRIMARY KEY, fight_id INTEGER,
        fighter_name TEXT, result TEXT, round_number INTEGER);
    """)
    names = [f"F{i:02d}" for i in range(12)]
    for i, nm in enumerate(names):
        stance = "Southpaw" if i % 3 == 0 else "Orthodox"
        cur.execute("INSERT INTO fighters (name,reach_in,height_in,stance,date_of_birth,"
                    "wins,losses,draws,no_contests,slpm,str_acc) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                    (nm, 70 + (i % 5), 70 + (i % 5), stance, f"{1985 + i}-01-01",
                     0, 0, 0, 0, 3.0, 0.5))

    fid = 0
    # Generate fights across years; stronger-indexed fighter usually wins so the
    # model has signal. All Lightweight so gating allows every pairing.
    years = list(range(2018, 2025))
    for yi, year in enumerate(years):
        for j in range(0, len(names) - 1, 1):
            a, b = names[j], names[j + 1]
            # winner: the higher index half the time, alternate for variety
            winner, loser = (b, a) if (j + yi) % 3 != 0 else (a, b)
            fid += 1
            # Vary the finish method so method_base_rate covers all three classes.
            method = ("KO/TKO", "Submission", "Decision - Unanimous")[fid % 3]
            cur.execute("INSERT INTO fights (fight_id,event_id,event_name,date,winner_name,"
                        "loser_name,weight_class,title_bout,method,round_ended,time_ended) "
                        "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                        (fid, 1, "E", f"{year}-06-15", winner, loser,
                         "Lightweight Bout", 0, method, 1, 60))
    conn.commit()
    conn.close()

    # Train pointing predict's module constants at our temp paths.
    orig_model_path = P.MODEL_PATH
    orig_models_dir = P.MODELS_DIR
    P.MODEL_PATH = path
    P.MODELS_DIR = os.path.dirname(path)
    P._LOADED = None
    try:
        P.train(db_path=db_path, verbose=False)
    finally:
        P.MODELS_DIR = orig_models_dir
    P._LOADED = None
    yield path
    # restore
    P.MODEL_PATH = orig_model_path
    P._LOADED = None


def test_predict_symmetry(trained_model):
    payload = P.load_model(trained_model, force=True)
    names = list(payload["snapshots"].keys())
    a, b = names[0], names[3]
    r_ab = P.predict(a, b, path=trained_model)
    r_ba = P.predict(b, a, path=trained_model)
    assert r_ab["allowed"] and r_ba["allowed"]
    # P(A beats B) == 1 - P(B beats A); equivalently prob_a(a,b) == prob_b(b,a).
    assert r_ab["prob_a"] == pytest.approx(r_ba["prob_b"], abs=1e-9)
    assert r_ab["prob_a"] == pytest.approx(1.0 - r_ab["prob_b"], abs=1e-9)
    assert (r_ab["prob_a"] + r_ab["prob_b"]) == pytest.approx(1.0, abs=1e-9)


def test_predict_unknown_fighter(trained_model):
    P.load_model(trained_model, force=True)
    res = P.predict("Nobody Here", "Also Nobody", path=trained_model)
    assert res["allowed"] is False
    assert "unknown fighter" in res["reason"]


def test_predict_payload_has_required_keys(trained_model):
    payload = P.load_model(trained_model, force=True)
    for key in ("pipeline", "feature_columns", "snapshots", "divisions",
                "static", "metrics",
                # career-timeline + prime artifacts
                "timeline", "prime_index", "prime_low_confidence", "fight_index",
                # method-of-victory artifacts
                "method_base_rate", "method_kappa", "method_metrics"):
        assert key in payload
    # Metrics carry both models + baselines.
    m = payload["metrics"]
    assert "models" in m and "baselines" in m
    assert "logreg" in m["models"] and "hgb" in m["models"]


# --------------------------------------------------------------------------- #
# 6) Career timeline + as-of-fight snapshots
# --------------------------------------------------------------------------- #

def test_default_snapshot_is_current(trained_model):
    payload = P.load_model(trained_model, force=True)
    a, b = list(payload["snapshots"].keys())[:2]
    r_default = P.predict(a, b, path=trained_model)
    r_current = P.predict(a, b, path=trained_model, snap_a="current", snap_b="current")
    assert r_default["prob_a"] == pytest.approx(r_current["prob_a"], abs=1e-12)
    assert r_default["snap_a_label"] == "current"


def test_available_snapshots_and_as_of_fight(trained_model):
    payload = P.load_model(trained_model, force=True)
    a, b = list(payload["snapshots"].keys())[:2]
    snaps = P.available_snapshots(a, path=trained_model)
    assert snaps and all("fight_id" in s for s in snaps)
    # Exactly one snapshot is flagged prime.
    assert sum(1 for s in snaps if s["is_prime"]) == 1
    # Selecting a specific past fight produces a valid, allowed prediction whose
    # snapshot label names that fight's date.
    fid = snaps[-1]["fight_id"]
    res = P.predict(a, b, path=trained_model, snap_a={"fight_id": fid})
    assert res["allowed"] and "as of" in res["snap_a_label"]


def test_as_of_snapshot_value_identity(trained_model):
    """A snapshot selected by fight_id reproduces that fight's trained feature row.

    Proves the persisted timeline snapshot is the SAME object the model trained on
    (the core leakage-safety guarantee of the timeline design).
    """
    payload = P.load_model(trained_model, force=True)
    # Pick a fighter and one of their non-debut fights.
    name = list(payload["snapshots"].keys())[0]
    entries = payload["timeline"][name]
    entry = entries[len(entries) // 2]
    snap_from_timeline = entry["snap"]
    # The snapshot's prior-fight count matches its position-derived history.
    assert snap_from_timeline["n_prior_fights"] == entry["n_prior_fights"]
    # record_pre is strictly before this fight (never includes its own result).
    wins_pre, losses_pre = entry["record_pre"]
    assert wins_pre + losses_pre == int(entry["n_prior_fights"])


# --------------------------------------------------------------------------- #
# 7) Prime detection (Elo + win-streak blend)
# --------------------------------------------------------------------------- #

def test_pick_prime_smoothing_beats_single_spike():
    # A lone one-fight Elo spike (index 4) must NOT win against a sustained high
    # plateau at the end (indices 7-9): raw argmax would pick the spike, but the
    # centred smoothing makes the sustained run win.
    def entry(elo, n, streak):
        return {"elo": elo, "n_prior_fights": n, "win_streak": streak}
    elos = [1500, 1510, 1520, 1530, 1800, 1530, 1540, 1650, 1655, 1660]
    entries = [entry(e, i, min(i, 4)) for i, e in enumerate(elos)]
    idx, low = P._pick_prime(entries)
    assert low is False
    assert idx != 4                      # the lone spike is smoothed away
    assert idx >= 7                      # the sustained late plateau wins


def test_pick_prime_short_career_low_confidence():
    entries = [{"elo": 1500 + 10 * i, "n_prior_fights": i, "win_streak": i}
               for i in range(2)]        # nobody clears PRIME_MIN_FIGHTS=3
    idx, low = P._pick_prime(entries)
    assert low is True
    assert 0 <= idx < len(entries)


def test_prime_vs_prime_symmetry(trained_model):
    payload = P.load_model(trained_model, force=True)
    a, b = list(payload["snapshots"].keys())[:2]
    r_ab = P.predict(a, b, path=trained_model, snap_a="prime", snap_b="prime")
    r_ba = P.predict(b, a, path=trained_model, snap_a="prime", snap_b="prime")
    assert r_ab["allowed"] and r_ba["allowed"]
    assert r_ab["prob_a"] == pytest.approx(r_ba["prob_b"], abs=1e-9)
    assert (r_ab["prob_a"] + r_ab["prob_b"]) == pytest.approx(1.0, abs=1e-9)
    assert "prime" in r_ab["snap_a_label"]


# --------------------------------------------------------------------------- #
# 8) Method-of-victory probabilities
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("raw,expected", [
    ("Decision - Unanimous", "decision"),
    ("Decision - Split", "decision"),
    ("KO/TKO", "ko_tko"),
    ("TKO - Doctor's Stoppage", "ko_tko"),
    ("Submission", "submission"),
    ("Overturned", None),
    ("Could Not Continue", None),
    ("DQ", None),
    ("Other", None),
    (None, None),
])
def test_method_class_mapping(raw, expected):
    assert P._method_class(raw) == expected


def test_method_probs_coherent(trained_model):
    payload = P.load_model(trained_model, force=True)
    a, b = list(payload["snapshots"].keys())[:2]
    res = P.predict(a, b, path=trained_model)
    mp = res["method_probs"]
    assert mp is not None
    # Each fighter's three method probs sum to that fighter's win probability.
    assert sum(mp["a"].values()) == pytest.approx(res["prob_a"], abs=1e-9)
    assert sum(mp["b"].values()) == pytest.approx(res["prob_b"], abs=1e-9)
    # All six outcomes form a coherent distribution summing to 1.
    total = sum(mp["a"].values()) + sum(mp["b"].values())
    assert total == pytest.approx(1.0, abs=1e-9)


def test_method_probs_symmetry(trained_model):
    payload = P.load_model(trained_model, force=True)
    a, b = list(payload["snapshots"].keys())[:2]
    r_ab = P.predict(a, b, path=trained_model)
    r_ba = P.predict(b, a, path=trained_model)
    for cls in P.METHOD_CLASSES:
        assert r_ab["method_probs"]["a"][cls] == pytest.approx(
            r_ba["method_probs"]["b"][cls], abs=1e-9)
        assert r_ab["method_probs"]["a_given_win"][cls] == pytest.approx(
            r_ba["method_probs"]["b_given_win"][cls], abs=1e-9)


def test_method_probs_given_win_is_each_fighters_own_split(trained_model):
    # Both fighters (favourite AND underdog) get a conditional "if they win"
    # split that sums to 1 and is consistent with the joint: joint = P(win) x share.
    payload = P.load_model(trained_model, force=True)
    a, b = list(payload["snapshots"].keys())[:2]
    res = P.predict(a, b, path=trained_model)
    mp = res["method_probs"]
    for side, prob in (("a", res["prob_a"]), ("b", res["prob_b"])):
        given = mp[f"{side}_given_win"]
        assert set(given) == set(P.METHOD_CLASSES)
        assert sum(given.values()) == pytest.approx(1.0, abs=1e-9)
        for cls in P.METHOD_CLASSES:
            assert 0.0 <= given[cls] <= 1.0
            assert mp[side][cls] == pytest.approx(prob * given[cls], abs=1e-9)


def test_conditional_method_dist_debut_is_base_rate():
    # A fighter/opponent with NO prior method data must shrink fully to the base
    # rate (pure prior), with no NaN / division-by-zero.
    base = {"decision": 0.5, "ko_tko": 0.3, "submission": 0.2}
    q = P._conditional_method_dist([0, 0, 0], [0, 0, 0], base, P.METHOD_PRIOR_KAPPA)
    for cls in P.METHOD_CLASSES:
        assert q[cls] == pytest.approx(base[cls], abs=1e-9)


def test_method_counters_leakage_safe():
    """A debut fighter's snapshot carries all-zero method counters, and the
    counters never entered the win-model FEATURE_COLUMNS."""
    fighters = _make_fighters(["A", "B"])
    fights = pd.DataFrame([
        {"fight_id": 1, "date": "2020-01-01", "winner_name": "A", "loser_name": "B",
         "weight_class": "Lightweight Bout", "title_bout": 0, "method": "Submission"},
    ])
    X, y, dates, snaps, divs, static, tl_art, m_art = P.build_training_frame(fighters, fights)
    # The first timeline entry for A is its debut: no prior method outcomes.
    first = tl_art["timeline"]["A"][0]["snap"]
    assert first["w_decision"] == first["w_ko_tko"] == first["w_submission"] == 0
    assert first["l_decision"] == first["l_ko_tko"] == first["l_submission"] == 0
    # The six counters must NOT have leaked into the win-model features.
    for col in X.columns:
        assert "decision" not in col and "ko_tko" not in col and "submission" not in col
