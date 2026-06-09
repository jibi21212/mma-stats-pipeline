"""UFC fight-outcome predictor: P(fighter A beats fighter B).

This is the SUPERVISED half of the ML component. It reads the read-only SQLite
database produced by the Go scraper (``data/ufc.db``) via the loaders in
``ml/db.py`` and trains a binary classifier that predicts the probability that
fighter A beats fighter B, modelling AGE, ACTIVITY (layoff / recent frequency),
CAREER TRAJECTORY (Elo momentum / form) and SKILL (Elo), while GATING matchups
by weight class so nonsensical cross-weight fights are refused rather than
predicted.

================================================================================
THE THREE CORRECTNESS INVARIANTS
================================================================================

1) NO TEMPORAL LEAKAGE.
   Every feature for a historical fight is computed from that fighter's PRIOR
   fights only (strictly before the fight's date), by walking all fights in
   chronological order and snapshotting each fighter's running state BEFORE
   applying the fight's result. The post-hoc career aggregates in the
   ``fighters`` table (slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def,
   sub_avg) are NEVER used as features -- they are computed over each fighter's
   whole career (incl. this and future fights) and would leak the answer. Only
   STATIC physical attributes (reach_in, height_in, stance, date_of_birth) are
   read from the fighters table. The train/test split is TEMPORAL (older fights
   train, most-recent fights test), never random.

2) SYMMETRY.
   Features are SIGNED DIFFERENCES (A_value - B_value). Each training fight is
   emitted in BOTH orderings -- (winner=A, loser=B, label=1) and (loser=A,
   winner=B, label=0) -- so the diff vector negates and the label flips; the
   model cannot learn "the fighter listed first wins". At predict time we run
   both orderings and average, guaranteeing P(A beats B) == 1 - P(B beats A).

3) WEIGHT-CLASS GATING.
   Each ``weight_class`` string is normalised to a canonical division on one of
   two SEPARATE ordinal ladders (men / women). A matchup is allowed only if the
   two fighters share a gender ladder AND the minimum division distance over the
   sets of divisions they have actually fought in is <= 1. Otherwise it is
   REFUSED with a reason (e.g. Bantam/Feather vs Heavyweight -> distance >= 5).

CLI
---
    python predict.py --train                 # train + save + print metrics
    python predict.py --a "Name" --b "Name"   # predict one matchup
"""

from __future__ import annotations

import argparse
import os
import sys
from typing import Optional

import numpy as np
import pandas as pd

# Make ``import db`` work whether this is run from ml/ or imported as ml.predict.
_THIS_DIR = os.path.dirname(os.path.abspath(__file__))
if _THIS_DIR not in sys.path:
    sys.path.insert(0, _THIS_DIR)

import db  # noqa: E402  (local module; path set above)

# --------------------------------------------------------------------------- #
# Paths / constants
# --------------------------------------------------------------------------- #

MODELS_DIR = os.path.join(_THIS_DIR, "models")
MODEL_PATH = os.path.join(MODELS_DIR, "predictor.joblib")

#: Hold out fights on/after this date for the temporal test set. The remaining
#: (older) fights train. This is a DATE split, never random.
TEST_CUTOFF = "2023-01-01"

#: Elo parameters.
ELO_START = 1500.0
ELO_K = 32.0

#: Layoff sentinel (days) for a fighter's debut / first recorded fight.
DEBUT_LAYOFF_DAYS = 365.0

#: Methods that mean "no real winner": exclude these fights from the LABEL set.
#: The scraper fills winner_name even for draws/NCs (document-order fallback),
#: so we drop any fight whose method indicates a draw / no-contest / overturn.
_NO_WINNER_TOKENS = ("draw", "nc", "no contest", "overturn")


# --------------------------------------------------------------------------- #
# 1) WEIGHT-CLASS NORMALISER + GATING
# --------------------------------------------------------------------------- #

# Men's ordinal ladder (low -> high).
MEN_LADDER = {
    "Flyweight": 1,
    "Bantamweight": 2,
    "Featherweight": 3,
    "Lightweight": 4,
    "Welterweight": 5,
    "Middleweight": 6,
    "Light Heavyweight": 7,
    "Heavyweight": 8,
}

# Women's ladder is SEPARATE (its own ordinals).
WOMEN_LADDER = {
    "Women's Strawweight": 1,
    "Women's Flyweight": 2,
    "Women's Bantamweight": 3,
    "Women's Featherweight": 4,
}


def normalize_weight_class(weight_class):
    """Normalise a raw ``weight_class`` string to a canonical division.

    Returns a ``(gender, ordinal)`` tuple where ``gender`` is ``"M"`` or
    ``"W"`` and ``ordinal`` is the division's rank on that gender's ladder, or
    ``None`` when the division cannot be resolved (catch/open weight, blanks,
    or unrecognised strings).

    ORDER MATTERS (substring match on a lowercased string):
      * "women" anywhere -> women ladder; then match
        strawweight/flyweight/bantamweight/featherweight.
      * else men ladder: check "light heavyweight" BEFORE "heavyweight"
        (the trap: "Light Heavyweight" contains "heavyweight"); "super
        heavyweight" -> Heavyweight; then flyweight, bantamweight,
        featherweight, lightweight, welterweight, middleweight, heavyweight.
      * "catch weight" / "open weight" and anything unresolved -> None.

    Handles the real strings, e.g. 'Lightweight Bout', 'UFC Welterweight Title
    Bout', "Women's Strawweight Bout", 'Light Heavyweight Bout', 'Catch Weight
    Bout', 'Super Heavyweight Bout', and tournament strings like 'Ultimate
    Fighter 9 Welterweight Tournament Title Bout' (-> Welterweight).
    """
    if weight_class is None:
        return None
    s = str(weight_class).strip().lower()
    if not s:
        return None

    # Catch / open weight are explicitly unresolved.
    if "catch weight" in s or "catchweight" in s or "open weight" in s or "openweight" in s:
        return None

    # --- Women ladder (must be checked first; "women's flyweight" etc.) ---- #
    if "women" in s:
        # Order from most-specific to least to avoid 'flyweight' eating
        # 'strawweight'? They don't overlap, but keep an explicit order.
        if "strawweight" in s:
            return ("W", WOMEN_LADDER["Women's Strawweight"])
        if "flyweight" in s:
            return ("W", WOMEN_LADDER["Women's Flyweight"])
        if "bantamweight" in s:
            return ("W", WOMEN_LADDER["Women's Bantamweight"])
        if "featherweight" in s:
            return ("W", WOMEN_LADDER["Women's Featherweight"])
        return None

    # --- Men ladder ------------------------------------------------------- #
    # CRITICAL trap: "light heavyweight" contains "heavyweight", so it MUST be
    # tested before the bare "heavyweight" check. "super heavyweight" maps to
    # Heavyweight (8).
    if "light heavyweight" in s:
        return ("M", MEN_LADDER["Light Heavyweight"])
    if "super heavyweight" in s:
        return ("M", MEN_LADDER["Heavyweight"])
    if "flyweight" in s:
        return ("M", MEN_LADDER["Flyweight"])
    if "bantamweight" in s:
        return ("M", MEN_LADDER["Bantamweight"])
    if "featherweight" in s:
        return ("M", MEN_LADDER["Featherweight"])
    if "lightweight" in s:
        return ("M", MEN_LADDER["Lightweight"])
    if "welterweight" in s:
        return ("M", MEN_LADDER["Welterweight"])
    if "middleweight" in s:
        return ("M", MEN_LADDER["Middleweight"])
    if "heavyweight" in s:  # bare heavyweight last (after light/super handled)
        return ("M", MEN_LADDER["Heavyweight"])

    return None


def gate_matchup(divs_a, divs_b):
    """Decide whether a matchup between two fighters is allowed.

    Parameters
    ----------
    divs_a, divs_b:
        Iterables of ``(gender, ordinal)`` divisions each fighter has actually
        fought in (the resolvable ones; ``None`` entries are excluded upstream).

    Returns
    -------
    dict with keys:
        ``allowed`` (bool), ``reason`` (str|None), ``distance`` (int|None),
        ``low_confidence`` (bool).

    Rule: allowed iff A and B share a gender ladder AND the MINIMUM
    ``|ord_a - ord_b|`` over (a in A's divs, b in B's divs on the shared ladder)
    is <= 1. Cross-gender -> refused. A fighter with NO resolvable division
    (only catch/open) -> allowed but flagged low confidence.
    """
    set_a = {d for d in divs_a if d is not None}
    set_b = {d for d in divs_b if d is not None}

    # Unknown division for either side: allow but flag low confidence.
    if not set_a or not set_b:
        return {
            "allowed": True,
            "reason": None,
            "distance": None,
            "low_confidence": True,
        }

    genders_a = {g for g, _ in set_a}
    genders_b = {g for g, _ in set_b}
    shared_genders = genders_a & genders_b
    if not shared_genders:
        ga = "/".join(sorted("women" if g == "W" else "men" for g in genders_a))
        gb = "/".join(sorted("women" if g == "W" else "men" for g in genders_b))
        return {
            "allowed": False,
            "reason": f"cross-gender matchup not allowed ({ga} vs {gb})",
            "distance": None,
            "low_confidence": False,
        }

    # Minimum division distance over the shared gender ladder(s).
    best = None
    for g in shared_genders:
        ords_a = [o for gg, o in set_a if gg == g]
        ords_b = [o for gg, o in set_b if gg == g]
        for oa in ords_a:
            for ob in ords_b:
                d = abs(oa - ob)
                if best is None or d < best:
                    best = d

    if best is not None and best <= 1:
        return {"allowed": True, "reason": None, "distance": best, "low_confidence": False}

    # Refused: too far apart. Build a readable division summary.
    return {
        "allowed": False,
        "reason": (
            f"too far apart in weight class: closest divisions are "
            f"{best} steps apart on the ladder (need <= 1)"
        ),
        "distance": best,
        "low_confidence": False,
    }


# --------------------------------------------------------------------------- #
# 2) LEAKAGE-SAFE, AS-OF-DATE FEATURE BUILDER
# --------------------------------------------------------------------------- #

#: Feature column order (signed A-minus-B differences plus symmetric context
#: features). This list is the SINGLE source of truth for column order and is
#: persisted with the model.
DIFF_FEATURES = [
    "elo_pre",
    "elo_momentum",
    "age_years",
    "n_prior_fights",
    "winrate_prior",
    "recent_winrate",
    "form_delta",
    "days_since_last",
    "fights_last_365",
    "reach_in",
    "height_in",
    "southpaw",
    "is_debut",
]
# Symmetric fight-context features (already order-invariant; not diffed).
CONTEXT_FEATURES = ["stance_mismatch", "title_bout"]
FEATURE_COLUMNS = [f"{c}_diff" for c in DIFF_FEATURES] + CONTEXT_FEATURES


def _expected_score(ra: float, rb: float) -> float:
    """Elo expected score of A vs B (the win probability under Elo)."""
    return 1.0 / (1.0 + 10.0 ** ((rb - ra) / 400.0))


def _parse_dob(value):
    """Parse an ISO ``YYYY-MM-DD`` DOB to a pandas Timestamp (NaT if missing)."""
    if value is None:
        return pd.NaT
    return pd.to_datetime(value, errors="coerce")


def _is_southpaw(stance) -> float:
    if stance is None or (isinstance(stance, float) and pd.isna(stance)):
        return 0.0
    return 1.0 if "southpaw" in str(stance).strip().lower() else 0.0


def _clean_label_fights(fights: pd.DataFrame) -> pd.DataFrame:
    """Return label-eligible fights: real winner, parseable date, drop draws/NC.

    Excludes any fight whose ``method`` indicates a draw / no-contest / overturn
    (no real winner), and any fight missing a winner, loser, or date.
    """
    df = fights.copy()
    df["date"] = pd.to_datetime(df["date"], errors="coerce")
    df = df[df["date"].notna()]

    for col in ("winner_name", "loser_name"):
        df = df[df[col].notna()]
        df = df[df[col].astype(str).str.strip() != ""]

    method = df.get("method", pd.Series("", index=df.index)).fillna("").astype(str).str.lower()
    drop = pd.Series(False, index=df.index)
    for tok in _NO_WINNER_TOKENS:
        drop = drop | method.str.contains(tok, regex=False)
    # "Other" is the scraper's catch-all for fights with no clear decisive result;
    # in this data they are time-limit draws (the 1995 Shamrock superfights) that
    # carry a fabricated document-order winner_name, so exclude them from labels too.
    drop = drop | (method.str.strip() == "other")
    df = df[~drop]

    # A fighter cannot meaningfully fight themselves.
    df = df[df["winner_name"] != df["loser_name"]]

    # Stable chronological order; tie-break by fight_id so the walk is
    # deterministic for fights on the same date.
    sort_cols = ["date"] + (["fight_id"] if "fight_id" in df.columns else [])
    df = df.sort_values(sort_cols, kind="mergesort").reset_index(drop=True)
    return df


def build_training_frame(
    fighters: pd.DataFrame,
    fights: pd.DataFrame,
):
    """Walk fights chronologically and emit a leakage-safe, symmetric dataset.

    Returns
    -------
    (X, y, dates, snapshots, divisions, static)
        * ``X``         : DataFrame of FEATURE_COLUMNS (2 rows per fight).
        * ``y``         : Series of 0/1 labels aligned to X.
        * ``dates``     : Series of fight dates aligned to X (for temporal split).
        * ``snapshots`` : dict name -> dict of that fighter's CURRENT (latest)
                          per-fighter feature snapshot (computed as-of their last
                          recorded fight), for fast prediction.
        * ``divisions`` : dict name -> set of (gender, ordinal) divisions fought.
        * ``static``    : dict name -> dict of static attrs (reach/height/
                          southpaw/age-now/record) for the tale of the tape.

    LEAKAGE GUARANTEE: for each fight we read each fighter's running state
    (Elo, counts, recent results, last-fight date, Elo history) as it stood
    BEFORE this fight, build the feature row, THEN update the state with this
    fight's outcome. So feature row N only ever sees fights strictly before N.
    """
    # --- static attributes from the fighters table (time-invariant) -------- #
    fmap = {}
    for _, r in fighters.iterrows():
        nm = r["name"]
        fmap[nm] = {
            "reach_in": pd.to_numeric(r.get("reach_in"), errors="coerce"),
            "height_in": pd.to_numeric(r.get("height_in"), errors="coerce"),
            "southpaw": _is_southpaw(r.get("stance")),
            "stance": r.get("stance"),
            "dob": _parse_dob(r.get("date_of_birth")),
            "wins": pd.to_numeric(r.get("wins"), errors="coerce"),
            "losses": pd.to_numeric(r.get("losses"), errors="coerce"),
            "draws": pd.to_numeric(r.get("draws"), errors="coerce"),
        }

    def static_of(name):
        return fmap.get(
            name,
            {"reach_in": np.nan, "height_in": np.nan, "southpaw": 0.0,
             "stance": None, "dob": pd.NaT, "wins": np.nan, "losses": np.nan,
             "draws": np.nan},
        )

    flabel = _clean_label_fights(fights)

    # Per-fighter running state (mutated in chronological order).
    state = {}  # name -> dict

    def get_state(name):
        st = state.get(name)
        if st is None:
            st = {
                "elo": ELO_START,
                "elo_history": [],   # elo_pre recorded BEFORE each prior fight
                "n": 0,              # prior fight count
                "wins": 0,
                "results": [],       # 1/0 per prior fight (most recent last)
                "last_date": None,   # date of previous fight
                "dates": [],         # all prior fight dates (chronological)
                "divisions": set(),  # (gender, ordinal) fought in
            }
            state[name] = st
        return st

    def snapshot(name, fight_date):
        """Build the per-fighter feature snapshot as of (before) ``fight_date``.

        Uses ONLY the fighter's running state, which contains prior fights only.
        """
        st = get_state(name)
        stat = static_of(name)

        elo_pre = st["elo"]
        # Elo ~5 fights earlier: history holds the elo_pre recorded before each
        # prior fight. elo_pre now is "current"; index -5 of history (or the
        # earliest if fewer) gives the skill level ~5 fights ago.
        hist = st["elo_history"]
        if len(hist) >= 2:
            ref = hist[-5] if len(hist) >= 5 else hist[0]
            elo_momentum = elo_pre - ref
        else:
            elo_momentum = 0.0

        # Age as of the fight date.
        dob = stat["dob"]
        if pd.notna(dob) and pd.notna(fight_date):
            age_years = (fight_date - dob).days / 365.25
        else:
            age_years = np.nan

        n_prior = st["n"]
        is_debut = 1.0 if n_prior == 0 else 0.0

        winrate_prior = (st["wins"] / n_prior) if n_prior > 0 else 0.5
        last5 = st["results"][-5:]
        recent_winrate = (sum(last5) / len(last5)) if last5 else 0.5
        form_delta = recent_winrate - winrate_prior

        if st["last_date"] is not None and pd.notna(fight_date):
            days_since_last = float((fight_date - st["last_date"]).days)
        else:
            days_since_last = DEBUT_LAYOFF_DAYS

        if pd.notna(fight_date):
            window_start = fight_date - pd.Timedelta(days=365)
            fights_last_365 = sum(1 for d in st["dates"] if d is not None and d >= window_start and d < fight_date)
        else:
            fights_last_365 = 0

        return {
            "elo_pre": float(elo_pre),
            "elo_momentum": float(elo_momentum),
            "age_years": float(age_years) if pd.notna(age_years) else np.nan,
            "n_prior_fights": float(n_prior),
            "winrate_prior": float(winrate_prior),
            "recent_winrate": float(recent_winrate),
            "form_delta": float(form_delta),
            "days_since_last": float(days_since_last),
            "fights_last_365": float(fights_last_365),
            "reach_in": float(stat["reach_in"]) if pd.notna(stat["reach_in"]) else np.nan,
            "height_in": float(stat["height_in"]) if pd.notna(stat["height_in"]) else np.nan,
            "southpaw": float(stat["southpaw"]),
            "is_debut": float(is_debut),
            "stance": stat["stance"],
        }

    rows = []
    labels = []
    dates = []

    for _, fight in flabel.iterrows():
        w = fight["winner_name"]
        l = fight["loser_name"]
        fight_date = fight["date"]
        div = normalize_weight_class(fight.get("weight_class"))
        title_bout = 1.0 if pd.to_numeric(fight.get("title_bout"), errors="coerce") == 1 else 0.0

        snap_w = snapshot(w, fight_date)
        snap_l = snapshot(l, fight_date)

        # stance mismatch: orthodox-vs-southpaw indicator (symmetric).
        stance_mismatch = 1.0 if snap_w["southpaw"] != snap_l["southpaw"] else 0.0

        # --- emit BOTH orderings for symmetry ----------------------------- #
        # Ordering 1: winner as A (label 1).
        rows.append(_diff_row(snap_w, snap_l, stance_mismatch, title_bout))
        labels.append(1)
        dates.append(fight_date)
        # Ordering 2: loser as A (label 0) -- diff negates, label flips.
        rows.append(_diff_row(snap_l, snap_w, stance_mismatch, title_bout))
        labels.append(0)
        dates.append(fight_date)

        # --- NOW update running state with this fight's outcome ----------- #
        e_w = _expected_score(get_state(w)["elo"], get_state(l)["elo"])
        e_l = 1.0 - e_w
        sw, sl = get_state(w), get_state(l)
        # record elo_pre into history BEFORE updating
        sw["elo_history"].append(sw["elo"])
        sl["elo_history"].append(sl["elo"])
        sw["elo"] = sw["elo"] + ELO_K * (1.0 - e_w)
        sl["elo"] = sl["elo"] + ELO_K * (0.0 - e_l)
        sw["n"] += 1
        sl["n"] += 1
        sw["wins"] += 1
        sw["results"].append(1)
        sl["results"].append(0)
        sw["last_date"] = fight_date
        sl["last_date"] = fight_date
        sw["dates"].append(fight_date)
        sl["dates"].append(fight_date)
        if div is not None:
            sw["divisions"].add(div)
            sl["divisions"].add(div)

    X = pd.DataFrame(rows, columns=FEATURE_COLUMNS)
    y = pd.Series(labels, name="label")
    dates = pd.Series(dates, name="date")

    # --- CURRENT snapshots (as-of the latest fight date in the DB) --------- #
    # Use max(fights.date) as "now" (NOT today's clock) for reproducibility.
    now = flabel["date"].max() if not flabel.empty else pd.Timestamp.today()
    snapshots = {}
    divisions = {}
    static = {}
    for name in state.keys():
        snap = snapshot(name, now)  # current state, evaluated at "now"
        snapshots[name] = snap
        divisions[name] = set(state[name]["divisions"])
        stat = static_of(name)
        # current record from running state (leakage-safe count of recorded W/L)
        st = state[name]
        static[name] = {
            "elo": float(st["elo"]),
            "age_years": snap["age_years"],
            "wins": int(st["wins"]),
            "n_fights": int(st["n"]),
            "losses": int(st["n"] - st["wins"]),
            "reach_in": snap["reach_in"],
            "height_in": snap["height_in"],
            "stance": stat["stance"],
            "southpaw": snap["southpaw"],
            "recent_winrate": snap["recent_winrate"],
            "form_delta": snap["form_delta"],
            "days_since_last": snap["days_since_last"],
            "winrate_prior": snap["winrate_prior"],
        }

    return X, y, dates, snapshots, divisions, static


def _diff_row(snap_a, snap_b, stance_mismatch, title_bout):
    """Build one feature row as signed A-minus-B differences + context."""
    row = []
    for c in DIFF_FEATURES:
        va = snap_a[c]
        vb = snap_b[c]
        if va is None or vb is None or (isinstance(va, float) and pd.isna(va)) or (isinstance(vb, float) and pd.isna(vb)):
            row.append(np.nan)
        else:
            row.append(va - vb)
    row.append(stance_mismatch)
    row.append(title_bout)
    return row


# --------------------------------------------------------------------------- #
# 3) TRAIN / EVALUATE
# --------------------------------------------------------------------------- #

def _temporal_split(X, y, dates, cutoff=TEST_CUTOFF):
    """Split into (train, test) by date: test = fights on/after ``cutoff``."""
    cutoff_ts = pd.Timestamp(cutoff)
    is_test = dates >= cutoff_ts
    return (
        X[~is_test].reset_index(drop=True),
        y[~is_test].reset_index(drop=True),
        X[is_test].reset_index(drop=True),
        y[is_test].reset_index(drop=True),
        dates[~is_test].reset_index(drop=True),
        dates[is_test].reset_index(drop=True),
    )


def _baselines(X_test, y_test):
    """Compute simple non-ML baselines on the test set.

    * 'higher Elo'        : pick the fighter with higher pre-fight Elo (diff>0).
    * 'more experienced'  : pick the fighter with more prior fights.
    * 'base rate'         : always predict label=1 (A wins) -> mean(y).
    """
    out = {}
    # higher pre-fight Elo
    pred_elo = (X_test["elo_pre_diff"] > 0).astype(int)
    # ties (diff==0) -> count as 0.5 accuracy contribution
    tie = (X_test["elo_pre_diff"] == 0)
    acc_elo = ((pred_elo == y_test) & ~tie).sum() + 0.5 * tie.sum()
    out["higher_elo"] = float(acc_elo / len(y_test)) if len(y_test) else float("nan")

    pred_exp = (X_test["n_prior_fights_diff"] > 0).astype(int)
    tie_e = (X_test["n_prior_fights_diff"] == 0)
    acc_exp = ((pred_exp == y_test) & ~tie_e).sum() + 0.5 * tie_e.sum()
    out["more_experienced"] = float(acc_exp / len(y_test)) if len(y_test) else float("nan")

    out["base_rate"] = float(y_test.mean()) if len(y_test) else float("nan")
    return out


def train(db_path: Optional[str] = None, verbose: bool = True) -> dict:
    """Train both models on leakage-safe symmetric features and persist the best.

    Returns a metrics dict and writes ``models/predictor.joblib``.
    """
    from sklearn.pipeline import Pipeline
    from sklearn.preprocessing import StandardScaler
    from sklearn.impute import SimpleImputer
    from sklearn.linear_model import LogisticRegression
    from sklearn.ensemble import HistGradientBoostingClassifier
    from sklearn.metrics import accuracy_score, log_loss, brier_score_loss
    import joblib

    conn = db.get_connection(db_path)
    try:
        fighters = db.load_fighters(conn=conn)
        fights = db.load_fights(conn=conn)
    finally:
        conn.close()

    X, y, dates, snapshots, divisions, static = build_training_frame(fighters, fights)

    if X.empty:
        raise ValueError("No label-eligible fights found; cannot train.")

    X_train, y_train, X_test, y_test, _, _ = _temporal_split(X, y, dates)
    if len(X_test) == 0:
        raise ValueError(
            f"Temporal test set is empty (no fights on/after {TEST_CUTOFF}). "
            f"Check the database date range."
        )

    # --- model A: LogisticRegression (impute -> scale -> logreg) ----------- #
    logreg = Pipeline(steps=[
        ("impute", SimpleImputer(strategy="median")),
        ("scale", StandardScaler()),
        ("clf", LogisticRegression(max_iter=2000, C=1.0)),
    ])
    # --- model B: HistGradientBoosting (handles NaN natively; impute anyway
    #     for a consistent pipeline + identical predict-time path) ---------- #
    hgb = Pipeline(steps=[
        ("impute", SimpleImputer(strategy="median")),
        ("clf", HistGradientBoostingClassifier(
            max_iter=300, learning_rate=0.05, max_depth=3,
            l2_regularization=1.0, random_state=0)),
    ])

    results = {}
    fitted = {}
    for name, model in (("logreg", logreg), ("hgb", hgb)):
        model.fit(X_train, y_train)
        p = model.predict_proba(X_test)[:, 1]
        pred = (p >= 0.5).astype(int)
        results[name] = {
            "accuracy": float(accuracy_score(y_test, pred)),
            "log_loss": float(log_loss(y_test, p, labels=[0, 1])),
            "brier": float(brier_score_loss(y_test, p)),
        }
        fitted[name] = model

    baselines = _baselines(X_test, y_test)

    # Pick the better model by test log-loss (lower is better).
    best_name = min(results, key=lambda k: results[k]["log_loss"])
    best_model = fitted[best_name]

    metrics = {
        "models": results,
        "baselines": baselines,
        "best_model": best_name,
        "n_train": int(len(X_train)),
        "n_test": int(len(X_test)),
        "test_cutoff": TEST_CUTOFF,
        "test_accuracy": results[best_name]["accuracy"],
        "test_log_loss": results[best_name]["log_loss"],
        "test_brier": results[best_name]["brier"],
    }

    # --- persist everything predict() needs -------------------------------- #
    os.makedirs(MODELS_DIR, exist_ok=True)
    payload = {
        "pipeline": best_model,
        "feature_columns": FEATURE_COLUMNS,
        "diff_features": DIFF_FEATURES,
        "context_features": CONTEXT_FEATURES,
        "snapshots": snapshots,
        "divisions": divisions,
        "static": static,
        "metrics": metrics,
        "now": str((pd.to_datetime(fights["date"], errors="coerce")).max()),
    }
    joblib.dump(payload, MODEL_PATH)

    if verbose:
        _print_metrics(metrics)
        print(f"\nSaved model -> {MODEL_PATH}")
        print(f"Fighters in snapshot: {len(snapshots)}")

    return metrics


def _print_metrics(metrics: dict) -> None:
    print("\n=== Fight-outcome predictor: temporal hold-out metrics ===")
    print(f"Temporal split: train < {metrics['test_cutoff']} <= test   "
          f"(train={metrics['n_train']} rows, test={metrics['n_test']} rows; "
          f"2 rows per fight)")
    print(f"\n{'model':<26}{'accuracy':>10}{'log_loss':>11}{'brier':>9}")
    print("-" * 56)
    for name, m in metrics["models"].items():
        star = " *" if name == metrics["best_model"] else "  "
        print(f"{name + star:<26}{m['accuracy']:>10.4f}{m['log_loss']:>11.4f}{m['brier']:>9.4f}")
    print("-" * 56)
    b = metrics["baselines"]
    print(f"{'baseline: higher Elo':<26}{b['higher_elo']:>10.4f}")
    print(f"{'baseline: more experienced':<26}{b['more_experienced']:>10.4f}")
    print(f"{'baseline: base rate (A wins)':<26}{b['base_rate']:>10.4f}")
    print(f"\nBest model (by test log-loss): {metrics['best_model']}")
    if metrics["test_accuracy"] > 0.70:
        print("WARNING: test accuracy > 70% -- RED FLAG for temporal leakage; "
              "honest MMA models land ~60-65%. Investigate the feature builder.")


# --------------------------------------------------------------------------- #
# 4) PREDICT
# --------------------------------------------------------------------------- #

_LOADED = None  # lazy in-process cache of the persisted payload


def load_model(path: str = MODEL_PATH, force: bool = False) -> dict:
    """Load (and cache) the persisted model payload from ``models/``."""
    global _LOADED
    if _LOADED is not None and not force:
        return _LOADED
    if not os.path.exists(path):
        raise FileNotFoundError(
            f"No saved model at '{path}'. Train it first: "
            f"python predict.py --train"
        )
    import joblib
    _LOADED = joblib.load(path)
    return _LOADED


def available_fighters(path: str = MODEL_PATH) -> list:
    """Sorted list of fighter names present in the saved snapshot."""
    payload = load_model(path)
    return sorted(payload["snapshots"].keys())


def _tale_of_tape(static_row, divisions):
    """Human-readable per-fighter summary for the response."""
    age = static_row.get("age_years")
    return {
        "elo": round(static_row.get("elo", float("nan")), 1),
        "age": round(age, 1) if age is not None and not (isinstance(age, float) and pd.isna(age)) else None,
        "record": f"{static_row.get('wins', 0)}-{static_row.get('losses', 0)}",
        "reach_in": None if pd.isna(static_row.get("reach_in", np.nan)) else static_row.get("reach_in"),
        "height_in": None if pd.isna(static_row.get("height_in", np.nan)) else static_row.get("height_in"),
        "stance": static_row.get("stance"),
        "recent_winrate": round(static_row.get("recent_winrate", float("nan")), 3),
        "form_delta": round(static_row.get("form_delta", float("nan")), 3),
        "layoff_days": round(static_row.get("days_since_last", float("nan")), 0),
        "divisions": sorted(divisions),
    }


def predict(name_a: str, name_b: str, path: str = MODEL_PATH) -> dict:
    """Predict P(A beats B). Gating first; symmetric averaging of both orderings.

    Returns a dict with at least: ``allowed`` (bool), ``reason`` (if refused),
    ``prob_a``, ``prob_b``, and a per-fighter 'tale of the tape'.
    """
    payload = load_model(path)
    snaps = payload["snapshots"]
    divs = payload["divisions"]
    static = payload["static"]
    pipeline = payload["pipeline"]
    diff_features = payload["diff_features"]
    feature_columns = payload["feature_columns"]

    result = {"name_a": name_a, "name_b": name_b}

    if name_a not in snaps:
        return {**result, "allowed": False, "reason": f"unknown fighter: {name_a}",
                "prob_a": None, "prob_b": None}
    if name_b not in snaps:
        return {**result, "allowed": False, "reason": f"unknown fighter: {name_b}",
                "prob_a": None, "prob_b": None}
    if name_a == name_b:
        return {**result, "allowed": False, "reason": "a fighter cannot fight themselves",
                "prob_a": None, "prob_b": None}

    divs_a = divs.get(name_a, set())
    divs_b = divs.get(name_b, set())
    gate = gate_matchup(divs_a, divs_b)

    tale_a = _tale_of_tape(static.get(name_a, {}), divs_a)
    tale_b = _tale_of_tape(static.get(name_b, {}), divs_b)

    if not gate["allowed"]:
        return {
            **result,
            "allowed": False,
            "reason": gate["reason"],
            "prob_a": None,
            "prob_b": None,
            "distance": gate["distance"],
            "tale_a": tale_a,
            "tale_b": tale_b,
        }

    snap_a = snaps[name_a]
    snap_b = snaps[name_b]
    stance_mismatch = 1.0 if snap_a.get("southpaw") != snap_b.get("southpaw") else 0.0
    # Title-bout context is a property of a hypothetical bout; default 0.
    title_bout = 0.0

    row_ab = _diff_row(snap_a, snap_b, stance_mismatch, title_bout)
    row_ba = _diff_row(snap_b, snap_a, stance_mismatch, title_bout)
    X = pd.DataFrame([row_ab, row_ba], columns=feature_columns)

    proba = pipeline.predict_proba(X)[:, 1]
    p_ab = float(proba[0])           # P(A beats B) from ordering (A,B)
    p_ba_as_a = float(proba[1])      # P(B beats A) from ordering (B,A)
    # Symmetric average: P(A) = mean of [p_ab, 1 - p_ba_as_a].
    prob_a = 0.5 * (p_ab + (1.0 - p_ba_as_a))
    prob_b = 1.0 - prob_a

    metrics = payload.get("metrics", {})
    return {
        **result,
        "allowed": True,
        "reason": None,
        "prob_a": prob_a,
        "prob_b": prob_b,
        "low_confidence": gate.get("low_confidence", False),
        "distance": gate.get("distance"),
        "tale_a": tale_a,
        "tale_b": tale_b,
        "model": metrics.get("best_model"),
        "test_accuracy": metrics.get("test_accuracy"),
    }


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #

def _print_prediction(res: dict) -> None:
    a, b = res["name_a"], res["name_b"]
    print(f"\n{a}  vs  {b}")
    print("-" * (len(a) + len(b) + 8))
    if not res["allowed"]:
        print(f"REFUSED: {res['reason']}")
        return
    pa, pb = res["prob_a"], res["prob_b"]
    print(f"P({a} wins) = {pa:.1%}")
    print(f"P({b} wins) = {pb:.1%}")
    if res.get("low_confidence"):
        print("(weight class unknown for one fighter -> low confidence)")
    ta, tb = res["tale_a"], res["tale_b"]
    print("\nTale of the tape:")
    keys = ["elo", "age", "record", "reach_in", "height_in", "stance",
            "recent_winrate", "form_delta", "layoff_days"]
    print(f"{'stat':<16}{a[:18]:>20}{b[:18]:>20}")
    for k in keys:
        print(f"{k:<16}{str(ta.get(k)):>20}{str(tb.get(k)):>20}")
    if res.get("test_accuracy") is not None:
        print(f"\n(model: {res.get('model')}; held-out test accuracy "
              f"{res['test_accuracy']:.1%})")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="UFC fight-outcome predictor: P(A beats B).")
    parser.add_argument("--train", action="store_true",
                        help="Train both models, evaluate on the temporal "
                             "hold-out, save the best, and print metrics.")
    parser.add_argument("--db", default=None,
                        help="Path to data/ufc.db (default: ml/../data/ufc.db).")
    parser.add_argument("--a", default=None, help="Fighter A name.")
    parser.add_argument("--b", default=None, help="Fighter B name.")
    args = parser.parse_args(argv)

    if args.train:
        try:
            train(db_path=args.db)
        except FileNotFoundError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 2
        return 0

    if args.a and args.b:
        try:
            res = predict(args.a, args.b)
        except FileNotFoundError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 2
        _print_prediction(res)
        return 0

    parser.print_help()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
