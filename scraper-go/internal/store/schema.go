package store

// schemaDDL is the authoritative schema from docs/SCHEMA_CONTRACT.md, applied
// verbatim on Open. It is idempotent (CREATE TABLE IF NOT EXISTS / CREATE INDEX
// IF NOT EXISTS), so re-opening an existing DB is a no-op. The WAL and
// foreign_keys pragmas in the contract are set separately in Open as connection
// pragmas rather than embedded here.
const schemaDDL = `
CREATE TABLE IF NOT EXISTS fighters (
    fighter_id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name                    TEXT NOT NULL UNIQUE,
    nickname                TEXT,
    nationality             TEXT DEFAULT 'Unlisted',
    height_in               INTEGER,
    weight_lbs              INTEGER,
    reach_in                INTEGER,
    stance                  TEXT,
    date_of_birth           TEXT,
    wins                    INTEGER NOT NULL DEFAULT 0,
    losses                  INTEGER NOT NULL DEFAULT 0,
    draws                   INTEGER NOT NULL DEFAULT 0,
    no_contests             INTEGER NOT NULL DEFAULT 0,
    was_champion            INTEGER NOT NULL DEFAULT 0,
    championship_bouts_won  INTEGER NOT NULL DEFAULT 0,
    slpm                    REAL,
    str_acc                 REAL,
    sapm                    REAL,
    str_def                 REAL,
    td_avg                  REAL,
    td_acc                  REAL,
    td_def                  REAL,
    sub_avg                 REAL
);

CREATE TABLE IF NOT EXISTS events (
    event_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT NOT NULL UNIQUE,
    date       TEXT,
    location   TEXT
);

CREATE TABLE IF NOT EXISTS fights (
    fight_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id      INTEGER REFERENCES events(event_id) ON DELETE CASCADE,
    event_name    TEXT,
    date          TEXT,
    winner_name   TEXT,
    loser_name    TEXT,
    weight_class  TEXT,
    title_bout    INTEGER NOT NULL DEFAULT 0,
    method        TEXT,
    round_ended   INTEGER NOT NULL DEFAULT 0,
    time_ended    INTEGER NOT NULL DEFAULT 0,
    referee       TEXT
);

CREATE TABLE IF NOT EXISTS round_stats (
    round_stat_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    fight_id        INTEGER REFERENCES fights(fight_id) ON DELETE CASCADE,
    fighter_name    TEXT,
    result          TEXT,
    round_number    INTEGER,
    knockdowns      INTEGER NOT NULL DEFAULT 0,
    sub_attempts    INTEGER NOT NULL DEFAULT 0,
    reversals       INTEGER NOT NULL DEFAULT 0,
    control_time    INTEGER NOT NULL DEFAULT 0,
    td_landed       INTEGER NOT NULL DEFAULT 0,
    td_attempted    INTEGER NOT NULL DEFAULT 0,
    td_pct          REAL    NOT NULL DEFAULT 0.0,
    sig_str_landed      INTEGER NOT NULL DEFAULT 0,
    sig_str_attempted   INTEGER NOT NULL DEFAULT 0,
    sig_str_pct         REAL    NOT NULL DEFAULT 0.0,
    total_str_landed    INTEGER NOT NULL DEFAULT 0,
    total_str_attempted INTEGER NOT NULL DEFAULT 0,
    total_str_pct       REAL    NOT NULL DEFAULT 0.0,
    head_landed     INTEGER NOT NULL DEFAULT 0, head_attempted     INTEGER NOT NULL DEFAULT 0, head_pct     REAL NOT NULL DEFAULT 0.0,
    body_landed     INTEGER NOT NULL DEFAULT 0, body_attempted     INTEGER NOT NULL DEFAULT 0, body_pct     REAL NOT NULL DEFAULT 0.0,
    leg_landed      INTEGER NOT NULL DEFAULT 0, leg_attempted      INTEGER NOT NULL DEFAULT 0, leg_pct      REAL NOT NULL DEFAULT 0.0,
    distance_landed INTEGER NOT NULL DEFAULT 0, distance_attempted INTEGER NOT NULL DEFAULT 0, distance_pct REAL NOT NULL DEFAULT 0.0,
    clinch_landed   INTEGER NOT NULL DEFAULT 0, clinch_attempted   INTEGER NOT NULL DEFAULT 0, clinch_pct   REAL NOT NULL DEFAULT 0.0,
    ground_landed   INTEGER NOT NULL DEFAULT 0, ground_attempted   INTEGER NOT NULL DEFAULT 0, ground_pct   REAL NOT NULL DEFAULT 0.0
);

CREATE INDEX IF NOT EXISTS idx_fights_event       ON fights(event_id);
CREATE INDEX IF NOT EXISTS idx_round_stats_fight  ON round_stats(fight_id);
CREATE INDEX IF NOT EXISTS idx_round_stats_name   ON round_stats(fighter_name);
`
