"""SQLite data-access + feature engineering for the UFC ML component.

This module READS the SQLite database produced by the Go scraper
(``data/ufc.db``) and never writes to it. It is the single source of
the per-fighter NUMERIC feature matrix consumed by ``archetypes.py`` and
``relationships.py``.

Value conventions (see ``docs/SCHEMA_CONTRACT.md``):
  * percentages are already stored as 0..1 fractions (NOT 0-100);
  * heights / reach are in inches, weight in lbs;
  * times (control_time, time_ended) are in seconds.

Import safety
-------------
Importing this module must succeed even when ``data/ufc.db`` does not yet
exist (the scraper creates it at runtime). All database access is guarded
*inside* functions; nothing touches the filesystem at import time. A clear
``FileNotFoundError`` is raised only when a loader is actually CALLED
against a missing database.
"""

from __future__ import annotations

import os
import sqlite3
from typing import Optional

import numpy as np
import pandas as pd

# --------------------------------------------------------------------------- #
# Configuration
# --------------------------------------------------------------------------- #

#: Default DB path: ``data/ufc.db`` resolved relative to THIS file (``ml/``),
#: i.e. ``ml/../data/ufc.db``. Kept as a module constant so callers (and the
#: notebook) can reference / override it.
_THIS_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DB_PATH = os.path.normpath(os.path.join(_THIS_DIR, "..", "data", "ufc.db"))

# --- Quality thresholds for build_fighter_features() ----------------------- #
# Documented and overridable via function kwargs.
#
#   MIN_FIGHTS              : a fighter must have at least this many recorded
#                            bouts (wins + losses + draws + no_contests). This
#                            removes one-off / debut fighters whose career
#                            averages are noise and whose win_rate is unstable.
#   MAX_MISSING_CAREER_FRAC: drop a fighter if more than this fraction of the
#                            8 career-average stats (slpm..sub_avg) are NULL.
#                            Such rows are mostly imputed and pollute clusters.
MIN_FIGHTS = 3
MAX_MISSING_CAREER_FRAC = 0.5

# The 8 career-average columns carried verbatim from the schema.
CAREER_STAT_COLS = [
    "slpm",
    "str_acc",
    "sapm",
    "str_def",
    "td_avg",
    "td_acc",
    "td_def",
    "sub_avg",
]

# Physical attribute columns.
PHYSICAL_COLS = ["height_in", "reach_in", "weight_lbs"]


# --------------------------------------------------------------------------- #
# Connection helpers
# --------------------------------------------------------------------------- #

def get_connection(db_path: Optional[str] = None) -> sqlite3.Connection:
    """Open a **read-only** connection to the UFC SQLite database.

    Parameters
    ----------
    db_path:
        Path to the SQLite file. Defaults to :data:`DEFAULT_DB_PATH`
        (``ml/../data/ufc.db``).

    Returns
    -------
    sqlite3.Connection

    Raises
    ------
    FileNotFoundError
        If the database file does not exist. This is raised only when the
        function is actually called, so importing the module is always safe
        even before the scraper has produced the database.
    """
    path = db_path or DEFAULT_DB_PATH
    if not os.path.exists(path):
        raise FileNotFoundError(
            f"UFC database not found at '{path}'. It is produced by the Go "
            f"scraper (scraper-go/) at runtime. Run the scraper first, or pass "
            f"an explicit --db / db_path to a valid data/ufc.db."
        )
    # Open read-only via URI so we can never accidentally mutate the scraper's DB.
    uri = f"file:{os.path.abspath(path)}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True)
    except sqlite3.OperationalError:
        # Some platforms/old SQLite builds dislike the URI form; fall back to a
        # plain connection (we still only ever issue SELECTs).
        conn = sqlite3.connect(path)
    return conn


def _read_table(table: str, db_path: Optional[str] = None,
                conn: Optional[sqlite3.Connection] = None) -> pd.DataFrame:
    """Read a whole table into a DataFrame, managing the connection lifecycle."""
    own_conn = conn is None
    if own_conn:
        conn = get_connection(db_path)
    try:
        return pd.read_sql_query(f"SELECT * FROM {table}", conn)
    finally:
        if own_conn:
            conn.close()


# --------------------------------------------------------------------------- #
# Raw table loaders
# --------------------------------------------------------------------------- #

def load_fighters(db_path: Optional[str] = None,
                  conn: Optional[sqlite3.Connection] = None) -> pd.DataFrame:
    """Load the ``fighters`` table as a DataFrame (one row per fighter)."""
    return _read_table("fighters", db_path, conn)


def load_fights(db_path: Optional[str] = None,
                conn: Optional[sqlite3.Connection] = None) -> pd.DataFrame:
    """Load the ``fights`` table as a DataFrame (one row per bout)."""
    return _read_table("fights", db_path, conn)


def load_round_stats(db_path: Optional[str] = None,
                     conn: Optional[sqlite3.Connection] = None) -> pd.DataFrame:
    """Load the ``round_stats`` table (one wide row per fight x fighter x round)."""
    return _read_table("round_stats", db_path, conn)


def load_events(db_path: Optional[str] = None,
                conn: Optional[sqlite3.Connection] = None) -> pd.DataFrame:
    """Load the ``events`` table as a DataFrame (one row per event)."""
    return _read_table("events", db_path, conn)


# --------------------------------------------------------------------------- #
# Feature engineering
# --------------------------------------------------------------------------- #

def _compute_record_features(fighters: pd.DataFrame) -> pd.DataFrame:
    """Derive ``total_fights`` and ``win_rate`` from the fighters W/L/D/NC.

    ``win_rate = wins / (wins + losses + draws + no_contests)``. Fighters with
    zero recorded bouts get NaN win_rate (they are dropped downstream by the
    MIN_FIGHTS threshold).
    """
    df = fighters.copy()
    for col in ["wins", "losses", "draws", "no_contests"]:
        if col not in df.columns:
            df[col] = 0
        df[col] = pd.to_numeric(df[col], errors="coerce").fillna(0)

    df["total_fights"] = df["wins"] + df["losses"] + df["draws"] + df["no_contests"]
    df["win_rate"] = np.where(
        df["total_fights"] > 0,
        df["wins"] / df["total_fights"].replace(0, np.nan),
        np.nan,
    )
    return df


def _compute_finish_rate(fights: pd.DataFrame) -> pd.Series:
    """Per-fighter finish rate: fraction of a fighter's WINS that were finishes.

    A "finish" is any win whose ``method`` is not a decision (i.e. KO/TKO or
    submission). Computed from the ``fights`` table using ``winner_name`` and
    ``method``. Returns a Series indexed by fighter name. Fighters with no wins
    are absent (NaN after the join, handled by imputation).
    """
    if fights.empty or "winner_name" not in fights.columns:
        return pd.Series(dtype=float, name="finish_rate")

    wins = fights[fights["winner_name"].notna()].copy()
    wins = wins[wins["winner_name"].astype(str).str.strip() != ""]
    if wins.empty:
        return pd.Series(dtype=float, name="finish_rate")

    method = wins.get("method", pd.Series([""] * len(wins), index=wins.index))
    method = method.fillna("").astype(str).str.lower()
    # Decisions contain "decision" (e.g. "Decision - Unanimous"); everything
    # else among recorded wins (KO/TKO, Submission, etc.) counts as a finish.
    is_finish = ~method.str.contains("decision")

    grp = pd.DataFrame({"winner_name": wins["winner_name"], "is_finish": is_finish})
    finish_rate = grp.groupby("winner_name")["is_finish"].mean()
    finish_rate.name = "finish_rate"
    finish_rate.index.name = "name"
    return finish_rate


def _compute_round_aggregates(round_stats: pd.DataFrame) -> pd.DataFrame:
    """Per-fighter aggregates from ``round_stats``, joined later by fighter name.

    Returns a DataFrame indexed by fighter name with:
      * ``avg_sig_str_landed``   : mean significant strikes landed per round;
      * ``head_share`` / ``body_share`` / ``leg_share`` : share of significant
        strikes by target (each in 0..1, summing to ~1 when any landed);
      * ``avg_control_time``     : mean control_time (seconds) per round;
      * ``knockdown_rate``       : mean knockdowns per round;
      * ``sub_attempt_rate``     : mean submission attempts per round.
    """
    empty_cols = [
        "avg_sig_str_landed",
        "head_share",
        "body_share",
        "leg_share",
        "avg_control_time",
        "knockdown_rate",
        "sub_attempt_rate",
    ]
    if round_stats.empty or "fighter_name" not in round_stats.columns:
        out = pd.DataFrame(columns=empty_cols)
        out.index.name = "name"
        return out

    rs = round_stats.copy()
    # Coerce the numeric columns we touch (defensive: scraper writes ints, but
    # be robust to stray strings / NULLs).
    numeric_src = [
        "sig_str_landed",
        "head_landed",
        "body_landed",
        "leg_landed",
        "control_time",
        "knockdowns",
        "sub_attempts",
    ]
    for col in numeric_src:
        if col not in rs.columns:
            rs[col] = 0
        rs[col] = pd.to_numeric(rs[col], errors="coerce").fillna(0)

    grouped = rs.groupby("fighter_name")
    agg = pd.DataFrame(index=grouped.size().index)
    agg.index.name = "name"

    agg["avg_sig_str_landed"] = grouped["sig_str_landed"].mean()
    agg["avg_control_time"] = grouped["control_time"].mean()
    agg["knockdown_rate"] = grouped["knockdowns"].mean()
    agg["sub_attempt_rate"] = grouped["sub_attempts"].mean()

    # Target shares: sum landed-by-target / total of the three targets.
    head_sum = grouped["head_landed"].sum()
    body_sum = grouped["body_landed"].sum()
    leg_sum = grouped["leg_landed"].sum()
    target_total = (head_sum + body_sum + leg_sum).replace(0, np.nan)
    agg["head_share"] = (head_sum / target_total).fillna(0.0)
    agg["body_share"] = (body_sum / target_total).fillna(0.0)
    agg["leg_share"] = (leg_sum / target_total).fillna(0.0)

    return agg[
        [
            "avg_sig_str_landed",
            "head_share",
            "body_share",
            "leg_share",
            "avg_control_time",
            "knockdown_rate",
            "sub_attempt_rate",
        ]
    ]


def build_fighter_features(
    db_path: Optional[str] = None,
    conn: Optional[sqlite3.Connection] = None,
    min_fights: int = MIN_FIGHTS,
    max_missing_career_frac: float = MAX_MISSING_CAREER_FRAC,
    require_round_data: bool = True,
) -> pd.DataFrame:
    """Build the per-fighter NUMERIC feature matrix indexed by fighter name.

    Columns (all numeric):

    Career averages (verbatim from ``fighters``):
        ``slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def, sub_avg``
    Physical attributes:
        ``height_in, reach_in, weight_lbs``
    Derived:
        ``win_rate``     = wins / total bouts,
        ``finish_rate``  = fraction of wins that ended by KO/TKO or submission.
    Per-round aggregates (joined from ``round_stats`` by ``fighter_name``):
        ``avg_sig_str_landed`` (avg significant strikes landed per round),
        ``head_share`` / ``body_share`` / ``leg_share`` (target distribution),
        ``avg_control_time`` (avg control seconds per round),
        ``knockdown_rate`` (avg knockdowns per round),
        ``sub_attempt_rate`` (avg submission attempts per round).

    NULL handling
    -------------
    Remaining NULLs (after row filtering) are imputed with each column's
    **median**. Per-round aggregates for fighters absent from ``round_stats``
    are filled with 0 before median imputation of the rest.

    Row filtering (documented thresholds)
    -------------------------------------
    A fighter is DROPPED when either:
      * ``total_fights < min_fights`` (default 3) — too few bouts for stable
        career stats / win_rate; or
      * more than ``max_missing_career_frac`` (default 0.5) of the 8 career
        stats are NULL — mostly-imputed rows pollute the clustering.

    Returns
    -------
    pandas.DataFrame
        Numeric-only feature matrix, index = fighter ``name``. Imports never
        require the DB; this function raises ``FileNotFoundError`` if called
        without one.
    """
    own_conn = conn is None
    if own_conn:
        conn = get_connection(db_path)
    try:
        fighters = load_fighters(conn=conn)
        fights = load_fights(conn=conn)
        round_stats = load_round_stats(conn=conn)
    finally:
        if own_conn:
            conn.close()

    if fighters.empty:
        raise ValueError(
            "The 'fighters' table is empty — nothing to build features from. "
            "Run the Go scraper to populate data/ufc.db."
        )

    # 1) Record-derived features (total_fights, win_rate).
    fighters = _compute_record_features(fighters)
    fighters = fighters.set_index("name")
    fighters.index.name = "name"

    # 2) finish_rate from fights, joined by name.
    finish_rate = _compute_finish_rate(fights)
    fighters = fighters.join(finish_rate, how="left")

    # 3) Per-round aggregates from round_stats, joined by name.
    round_agg = _compute_round_aggregates(round_stats)
    fighters = fighters.join(round_agg, how="left")

    # --- Row filtering (BEFORE imputation so thresholds see real NULLs) ----- #
    present_career = [c for c in CAREER_STAT_COLS if c in fighters.columns]
    if present_career:
        missing_frac = fighters[present_career].isna().mean(axis=1)
    else:
        missing_frac = pd.Series(0.0, index=fighters.index)

    enough_fights = fighters["total_fights"] >= min_fights
    not_too_missing = missing_frac <= max_missing_career_frac
    # For archetype clustering we want fighters with REAL per-round style data;
    # otherwise their round features are median-imputed and collapse into one
    # dense blob that forces a trivial 2-way split. require_round_data keeps only
    # fighters who appear in round_stats (pass False to include everyone).
    has_round = pd.Series(True, index=fighters.index)
    if require_round_data and not round_agg.empty:
        has_round = pd.Series(fighters.index.isin(round_agg.index), index=fighters.index)
    keep = enough_fights & not_too_missing & has_round
    fighters = fighters[keep]

    if fighters.empty:
        raise ValueError(
            f"No fighters passed the quality thresholds "
            f"(min_fights={min_fights}, max_missing_career_frac="
            f"{max_missing_career_frac}). Check the database contents."
        )

    # --- Assemble the numeric feature matrix ------------------------------- #
    round_cols = [
        "avg_sig_str_landed",
        "head_share",
        "body_share",
        "leg_share",
        "avg_control_time",
        "knockdown_rate",
        "sub_attempt_rate",
    ]
    derived_cols = ["win_rate", "finish_rate"]
    feature_cols = (
        present_career
        + [c for c in PHYSICAL_COLS if c in fighters.columns]
        + derived_cols
        + round_cols
    )
    features = fighters[feature_cols].copy()

    # Coerce everything to numeric (defensive).
    for col in features.columns:
        features[col] = pd.to_numeric(features[col], errors="coerce")

    # finish_rate missing means the fighter has zero wins -> genuinely 0.0.
    if "finish_rate" in features.columns:
        features["finish_rate"] = features["finish_rate"].fillna(0.0)

    # Per-round aggregates are NaN for fighters with no round_stats rows (e.g.
    # they did not appear in the scraped events). Do NOT fill these with 0:
    # that makes "has round data or not" the dominant axis and collapses the
    # clustering onto a data-coverage split instead of fighting style. Leave them
    # NaN here so they are MEDIAN-imputed below ("unknown -> typical fighter").

    # Median imputation for anything still NULL (career stats, physical attrs,
    # win_rate edge cases).
    medians = features.median(numeric_only=True)
    features = features.fillna(medians)
    # If an entire column was NULL its median is NaN; fall back to 0.0.
    features = features.fillna(0.0)

    # Return numeric columns only (drop any that somehow stayed non-numeric).
    features = features.select_dtypes(include=[np.number])
    return features


# --------------------------------------------------------------------------- #
# Convenience
# --------------------------------------------------------------------------- #

def feature_columns() -> list:
    """Return the canonical engineered feature-column names (for docs/tests)."""
    return (
        list(CAREER_STAT_COLS)
        + list(PHYSICAL_COLS)
        + ["win_rate", "finish_rate"]
        + [
            "avg_sig_str_landed",
            "head_share",
            "body_share",
            "leg_share",
            "avg_control_time",
            "knockdown_rate",
            "sub_attempt_rate",
        ]
    )


if __name__ == "__main__":  # pragma: no cover - manual smoke check
    feats = build_fighter_features()
    print(f"Built feature matrix: {feats.shape[0]} fighters x "
          f"{feats.shape[1]} features")
    print(feats.head())
