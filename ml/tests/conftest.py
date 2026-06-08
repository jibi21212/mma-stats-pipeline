"""Pytest fixtures for the ML test suite.

Builds a small, deterministic synthetic ``ufc.db`` (matching the schema in
``docs/SCHEMA_CONTRACT.md``) in a temp dir, so the tests exercise the real
db/archetypes/relationships code paths without needing a scraped database.
"""
import os
import sqlite3
import sys

import pytest

# Make the ml/ modules (db, archetypes, relationships) importable from ml/tests/.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# Minimal but faithful subset of the contract schema (NOT NULL DEFAULTs let us
# insert only the columns each test cares about).
SCHEMA = """
CREATE TABLE fighters (
    fighter_id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE, nickname TEXT, nationality TEXT DEFAULT 'Unlisted',
    height_in INTEGER, weight_lbs INTEGER, reach_in INTEGER, stance TEXT, date_of_birth TEXT,
    wins INTEGER NOT NULL DEFAULT 0, losses INTEGER NOT NULL DEFAULT 0,
    draws INTEGER NOT NULL DEFAULT 0, no_contests INTEGER NOT NULL DEFAULT 0,
    was_champion INTEGER NOT NULL DEFAULT 0, championship_bouts_won INTEGER NOT NULL DEFAULT 0,
    slpm REAL, str_acc REAL, sapm REAL, str_def REAL,
    td_avg REAL, td_acc REAL, td_def REAL, sub_avg REAL
);
CREATE TABLE events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL UNIQUE, date TEXT, location TEXT
);
CREATE TABLE fights (
    fight_id INTEGER PRIMARY KEY AUTOINCREMENT, event_id INTEGER, event_name TEXT, date TEXT,
    winner_name TEXT, loser_name TEXT, weight_class TEXT, title_bout INTEGER NOT NULL DEFAULT 0,
    method TEXT, round_ended INTEGER NOT NULL DEFAULT 0, time_ended INTEGER NOT NULL DEFAULT 0, referee TEXT
);
CREATE TABLE round_stats (
    round_stat_id INTEGER PRIMARY KEY AUTOINCREMENT, fight_id INTEGER, fighter_name TEXT, result TEXT,
    round_number INTEGER, knockdowns INTEGER NOT NULL DEFAULT 0, sub_attempts INTEGER NOT NULL DEFAULT 0,
    reversals INTEGER NOT NULL DEFAULT 0, control_time INTEGER NOT NULL DEFAULT 0,
    td_landed INTEGER NOT NULL DEFAULT 0, td_attempted INTEGER NOT NULL DEFAULT 0, td_pct REAL NOT NULL DEFAULT 0.0,
    sig_str_landed INTEGER NOT NULL DEFAULT 0, sig_str_attempted INTEGER NOT NULL DEFAULT 0, sig_str_pct REAL NOT NULL DEFAULT 0.0,
    total_str_landed INTEGER NOT NULL DEFAULT 0, total_str_attempted INTEGER NOT NULL DEFAULT 0, total_str_pct REAL NOT NULL DEFAULT 0.0,
    head_landed INTEGER NOT NULL DEFAULT 0, head_attempted INTEGER NOT NULL DEFAULT 0, head_pct REAL NOT NULL DEFAULT 0.0,
    body_landed INTEGER NOT NULL DEFAULT 0, body_attempted INTEGER NOT NULL DEFAULT 0, body_pct REAL NOT NULL DEFAULT 0.0,
    leg_landed INTEGER NOT NULL DEFAULT 0, leg_attempted INTEGER NOT NULL DEFAULT 0, leg_pct REAL NOT NULL DEFAULT 0.0,
    distance_landed INTEGER NOT NULL DEFAULT 0, distance_attempted INTEGER NOT NULL DEFAULT 0, distance_pct REAL NOT NULL DEFAULT 0.0,
    clinch_landed INTEGER NOT NULL DEFAULT 0, clinch_attempted INTEGER NOT NULL DEFAULT 0, clinch_pct REAL NOT NULL DEFAULT 0.0,
    ground_landed INTEGER NOT NULL DEFAULT 0, ground_attempted INTEGER NOT NULL DEFAULT 0, ground_pct REAL NOT NULL DEFAULT 0.0
);
"""

_RS_SQL = (
    "INSERT INTO round_stats (fight_id, fighter_name, result, round_number, knockdowns, "
    "sub_attempts, control_time, sig_str_landed, head_landed, body_landed, leg_landed) "
    "VALUES (?,?,?,?,?,?,?,?,?,?,?)"
)


def _populate(conn: sqlite3.Connection) -> None:
    cur = conn.cursor()
    cur.executescript(SCHEMA)
    cur.execute("INSERT INTO events (title,date,location) VALUES (?,?,?)",
                ("Synth Event 1", "2024-01-01", "Testville"))
    event_id = cur.lastrowid

    # 30 "main" fighters: varied career stats + round data. reach tracks height.
    for i in range(30):
        height = 64 + (i % 12)
        cur.execute(
            "INSERT INTO fighters (name,nationality,height_in,weight_lbs,reach_in,stance,"
            "wins,losses,draws,no_contests,slpm,str_acc,sapm,str_def,td_avg,td_acc,td_def,sub_avg) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (f"Fighter{i:02d}", "Unlisted", height, 125 + (i % 10) * 15, height + 1 + (i % 3), "Orthodox",
             5 + (i % 8), 1 + (i % 4), 0, 0,
             2.0 + (i % 7) * 0.6, 0.35 + (i % 5) * 0.05, 1.5 + (i % 4) * 0.5, 0.40 + (i % 6) * 0.04,
             (i % 5) * 0.8, 0.20 + (i % 5) * 0.07, 0.50 + (i % 5) * 0.08, (i % 3) * 0.4),
        )

    # 15 fights: Fighter(2k) beats Fighter(2k+1); even k -> KO (finish), odd -> decision.
    for k in range(15):
        a, b = f"Fighter{2 * k:02d}", f"Fighter{2 * k + 1:02d}"
        method = "KO/TKO" if k % 2 == 0 else "Decision - Unanimous"
        cur.execute(
            "INSERT INTO fights (event_id,event_name,date,winner_name,loser_name,weight_class,"
            "title_bout,method,round_ended,time_ended,referee) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (event_id, "Synth Event 1", "2024-01-01", a, b, "Lightweight Bout", 0, method, 3, 300, "Ref"),
        )
        fight_id = cur.lastrowid
        for rnd in range(1, 4):
            cur.execute(_RS_SQL, (fight_id, a, "w", rnd, 1 if rnd == 1 else 0, 0, 60, 20 + rnd, 12 + rnd, 4, 4))
            cur.execute(_RS_SQL, (fight_id, b, "l", rnd, 0, 1, 20, 10, 5, 3, 2))

    # 2 low-fight fighters (total_fights=2): dropped by min_fights>=3. They DO have round data.
    for nm in ("LowA", "LowB"):
        cur.execute(
            "INSERT INTO fighters (name,nationality,wins,losses,draws,no_contests,height_in,reach_in,"
            "weight_lbs,slpm,str_acc,sapm,str_def,td_avg,td_acc,td_def,sub_avg) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (nm, "Unlisted", 1, 1, 0, 0, 70, 72, 155, 3.0, 0.5, 2.0, 0.5, 1.0, 0.4, 0.6, 0.3),
        )
        cur.execute(
            "INSERT INTO fights (event_id,event_name,date,winner_name,loser_name,weight_class,"
            "title_bout,method,round_ended,time_ended,referee) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (event_id, "Synth Event 1", "2024-01-01", nm, "Fighter00", "LW", 0, "KO/TKO", 1, 60, "Ref"),
        )
        cur.execute(_RS_SQL, (cur.lastrowid, nm, "w", 1, 0, 0, 30, 15, 8, 4, 3))

    # 3 fighters with NO round data (total_fights=8): dropped by require_round_data.
    for nm in ("NoRoundA", "NoRoundB", "NoRoundC"):
        cur.execute(
            "INSERT INTO fighters (name,nationality,wins,losses,draws,no_contests,height_in,reach_in,"
            "weight_lbs,slpm,str_acc,sapm,str_def,td_avg,td_acc,td_def,sub_avg) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (nm, "Unlisted", 6, 2, 0, 0, 71, 73, 170, 4.0, 0.5, 2.5, 0.55, 1.5, 0.45, 0.7, 0.4),
        )
    conn.commit()


@pytest.fixture(scope="session")
def synthetic_db(tmp_path_factory) -> str:
    """Path to a populated synthetic SQLite DB (session-scoped)."""
    path = str(tmp_path_factory.mktemp("data") / "synthetic_ufc.db")
    conn = sqlite3.connect(path)
    try:
        _populate(conn)
    finally:
        conn.close()
    return path
