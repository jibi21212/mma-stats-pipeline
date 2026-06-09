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
    X, y, dates, snaps, divs, static = P.build_training_frame(fighters, fights)

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
    X, y, dates, snaps, divs, static = P.build_training_frame(fighters, fights)

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
    X, y, dates, snaps, divs, static = P.build_training_frame(fighters, fights)
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
            cur.execute("INSERT INTO fights (fight_id,event_id,event_name,date,winner_name,"
                        "loser_name,weight_class,title_bout,method,round_ended,time_ended) "
                        "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                        (fid, 1, "E", f"{year}-06-15", winner, loser,
                         "Lightweight Bout", 0, "KO/TKO", 1, 60))
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
                "static", "metrics"):
        assert key in payload
    # Metrics carry both models + baselines.
    m = payload["metrics"]
    assert "models" in m and "baselines" in m
    assert "logreg" in m["models"] and "hgb" in m["models"]
