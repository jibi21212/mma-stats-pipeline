# Shared Schema Contract — `data/ufc.db`

This is the **single source of truth** for the SQLite database produced by the Go scraper
(`scraper-go/`) and consumed by the Python ML component (`ml/`). Both sides must agree on this exact
schema, column names, and value conventions. If a column changes, it changes **here first**, then in
both components.

The Go scraper is the **sole writer**; the Python ML is **read-only**.

---

## Value conventions (carried over verbatim from the original Python scraper)

These are verified against `mma_stats_tracker_website/scraper/tests/test_parsers.py` — preserve them
exactly so the Go port produces identical numbers:

| Concept | Convention | Example |
|---|---|---|
| Percentages | stored as **0..1 fractions**, never 0–100 | `'57%'` → `0.57` |
| Strike/takedown pct | `landed / attempted` (0.0 if attempted == 0) | `30 of 50` → `0.6` |
| Height | total **inches** | `6' 4"` → `76` |
| Reach | **inches** (strip `"`) | `84"` → `84` |
| Weight | **lbs** (strip ` lbs.`) | `205 lbs.` → `205` |
| Times (control, finish) | total **seconds** | `1:30` → `90` |
| Dates (event/fight) | ISO text `YYYY-MM-DD` | `March 04, 2023` → `2023-03-04` |
| DOB | ISO text `YYYY-MM-DD` (parsed from `%b %d, %Y`) | `Jul 19, 1987` → `1987-07-19` |
| `title_bout` | integer `0` / `1` (1 when weight-class text contains `"Title"`) | |
| Missing numeric | `NULL` (placeholders `--` / `---` → `NULL`) | |
| Missing strike triple | `0 / 0 / 0.0` (NOT null) — matches `parse_strike_data` | |
| Result | `'w'` / `'l'` / `'d'` (`'d'` for both sides when no winner = draw/NC) | |
| `nationality` | defaults to literal `'Unlisted'` | |

---

## DDL (authoritative)

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- One row per fighter == the feature matrix for archetype clustering.
-- (Old CareerStats is merged in here as the slpm..sub_avg columns.)
CREATE TABLE IF NOT EXISTS fighters (
    fighter_id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name                    TEXT NOT NULL UNIQUE,
    nickname                TEXT,
    nationality             TEXT DEFAULT 'Unlisted',
    height_in               INTEGER,
    weight_lbs              INTEGER,
    reach_in                INTEGER,
    stance                  TEXT,
    date_of_birth           TEXT,            -- ISO YYYY-MM-DD
    wins                    INTEGER NOT NULL DEFAULT 0,
    losses                  INTEGER NOT NULL DEFAULT 0,
    draws                   INTEGER NOT NULL DEFAULT 0,
    no_contests             INTEGER NOT NULL DEFAULT 0,
    was_champion            INTEGER NOT NULL DEFAULT 0,   -- 0/1
    championship_bouts_won  INTEGER NOT NULL DEFAULT 0,
    -- career averages (0..1 fractions for the *_acc / *_def fields)
    slpm                    REAL,   -- strikes landed per minute
    str_acc                 REAL,   -- striking accuracy        (0..1)
    sapm                    REAL,   -- strikes absorbed per minute
    str_def                 REAL,   -- striking defense          (0..1)
    td_avg                  REAL,   -- takedowns avg / 15 min
    td_acc                  REAL,   -- takedown accuracy         (0..1)
    td_def                  REAL,   -- takedown defense          (0..1)
    sub_avg                 REAL    -- submission attempts avg / 15 min
);

CREATE TABLE IF NOT EXISTS events (
    event_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT NOT NULL UNIQUE,
    date       TEXT,        -- ISO YYYY-MM-DD
    location   TEXT
);

CREATE TABLE IF NOT EXISTS fights (
    fight_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id      INTEGER REFERENCES events(event_id) ON DELETE CASCADE,
    event_name    TEXT,
    date          TEXT,        -- ISO YYYY-MM-DD
    winner_name   TEXT,        -- may be '' / NULL for draw/NC
    loser_name    TEXT,
    weight_class  TEXT,
    title_bout    INTEGER NOT NULL DEFAULT 0,  -- 0/1
    method        TEXT,
    round_ended   INTEGER NOT NULL DEFAULT 0,
    time_ended    INTEGER NOT NULL DEFAULT 0,  -- seconds
    referee       TEXT
);

-- One WIDE row per (fight × fighter × round).
-- Replaces the old RoundStats + StrikeBreakdown + 9×StrikeStats normalization.
CREATE TABLE IF NOT EXISTS round_stats (
    round_stat_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    fight_id        INTEGER REFERENCES fights(fight_id) ON DELETE CASCADE,
    fighter_name    TEXT,
    result          TEXT,        -- 'w' / 'l' / 'd'
    round_number    INTEGER,
    knockdowns      INTEGER NOT NULL DEFAULT 0,
    sub_attempts    INTEGER NOT NULL DEFAULT 0,
    reversals       INTEGER NOT NULL DEFAULT 0,
    control_time    INTEGER NOT NULL DEFAULT 0,   -- seconds
    -- takedowns (the old RoundStats.strike_stats FK)
    td_landed       INTEGER NOT NULL DEFAULT 0,
    td_attempted    INTEGER NOT NULL DEFAULT 0,
    td_pct          REAL    NOT NULL DEFAULT 0.0,
    -- significant strikes
    sig_str_landed      INTEGER NOT NULL DEFAULT 0,
    sig_str_attempted   INTEGER NOT NULL DEFAULT 0,
    sig_str_pct         REAL    NOT NULL DEFAULT 0.0,
    -- total strikes
    total_str_landed    INTEGER NOT NULL DEFAULT 0,
    total_str_attempted INTEGER NOT NULL DEFAULT 0,
    total_str_pct       REAL    NOT NULL DEFAULT 0.0,
    -- by target
    head_landed     INTEGER NOT NULL DEFAULT 0, head_attempted     INTEGER NOT NULL DEFAULT 0, head_pct     REAL NOT NULL DEFAULT 0.0,
    body_landed     INTEGER NOT NULL DEFAULT 0, body_attempted     INTEGER NOT NULL DEFAULT 0, body_pct     REAL NOT NULL DEFAULT 0.0,
    leg_landed      INTEGER NOT NULL DEFAULT 0, leg_attempted      INTEGER NOT NULL DEFAULT 0, leg_pct      REAL NOT NULL DEFAULT 0.0,
    -- by position
    distance_landed INTEGER NOT NULL DEFAULT 0, distance_attempted INTEGER NOT NULL DEFAULT 0, distance_pct REAL NOT NULL DEFAULT 0.0,
    clinch_landed   INTEGER NOT NULL DEFAULT 0, clinch_attempted   INTEGER NOT NULL DEFAULT 0, clinch_pct   REAL NOT NULL DEFAULT 0.0,
    ground_landed   INTEGER NOT NULL DEFAULT 0, ground_attempted   INTEGER NOT NULL DEFAULT 0, ground_pct   REAL NOT NULL DEFAULT 0.0
);

CREATE INDEX IF NOT EXISTS idx_fights_event       ON fights(event_id);
CREATE INDEX IF NOT EXISTS idx_round_stats_fight  ON round_stats(fight_id);
CREATE INDEX IF NOT EXISTS idx_round_stats_name   ON round_stats(fighter_name);
```

---

## Mapping: original Python parser dicts → these columns

The Go scraper must reproduce the exact parse logic in
`mma_stats_tracker_website/scraper/parsers.py`. The output dicts map to columns as follows.

**`parse_fighter_page` dict → `fighters` row**

| Python dict key | Column |
|---|---|
| `name` | `name` |
| `nickname` | `nickname` |
| `nationality` (always `'Unlisted'`) | `nationality` |
| `height` (inches) | `height_in` |
| `weight` (lbs) | `weight_lbs` |
| `reach` (inches) | `reach_in` |
| `stance` | `stance` |
| `date_of_birth` | `date_of_birth` |
| `wins/losses/draws/no_contests` | same |
| `career_stats.strikes_per_minute` | `slpm` |
| `career_stats.strike_accuracy` | `str_acc` |
| `career_stats.strikes_absorbed_per_minute` | `sapm` |
| `career_stats.strike_defense` | `str_def` |
| `career_stats.takedown_average` | `td_avg` |
| `career_stats.takedown_accuracy` | `td_acc` |
| `career_stats.takedown_defense` | `td_def` |
| `career_stats.submission_average` | `sub_avg` |

`was_champion`/`championship_bouts_won` start at `0`; set them when persisting a **title bout with a
winner** (winner fighter's `was_champion=1`, `championship_bouts_won += 1`) — same rule as the old
`save_event`. On re-scrape, **upsert by `name`**: update record + career averages (and physical attrs)
for an existing fighter; insert otherwise.

**`scrape_event_with_fights` dict → `events` + `fights` + `round_stats`**

- event: `title`→`title`, `date`→`date` (ISO), `location`→`location`.
- per fight: `event_name`, `date` (ISO), `winner`→`winner_name`, `loser`→`loser_name`,
  `weight_class`, `title_bout` (bool→0/1), `method`, `round_ended`, `time_ended` (sec), `referee`.
- per fight, **two** competitors (`fighter_1` = winner corner, `fighter_2` = loser corner); for each,
  one `round_stats` row **per round** with `fighter_name`, `result` (`'w'`/`'l'`/`'d'`),
  `round_number`, and the flattened stats. The old nested round dict maps as:
  - scalars: `knockdowns`, `submission_attempts`→`sub_attempts`, `reversals`, `control_time`.
  - `takedowns.{landed,attempted,percentage}` → `td_landed/td_attempted/td_pct`.
  - `strike_breakdown.significant_strikes.*` → `sig_str_*`; `.total_strikes.*` → `total_str_*`;
    `.head_strikes.*`→`head_*`; `.body_strikes.*`→`body_*`; `.leg_strikes.*`→`leg_*`;
    `.distance_strikes.*`→`distance_*`; `.clinch_strikes.*`→`clinch_*`; `.ground_strikes.*`→`ground_*`.

**Result assignment** (same as old `save_event`): `f1_result = 'w' if fighter_1.name == winner else 'l'`;
likewise f2; if `winner_name` is empty (draw/NC) → both `'d'`.

**Difference from old behavior (intentional):** do **not** create empty placeholder `fighters` rows for
names that only appear on a fight card. Fighter name is stored denormalized on `fights`/`round_stats`,
so nothing is lost; the `fighters` table stays a clean feature matrix.

---

## Incremental scraping (same semantics as the old commands)

- **Fighters:** read the letter index for `(url, name)` pairs; **skip** names already in
  `SELECT name FROM fighters` (only *new* fighters are fetched + inserted). `--full` ignores the set
  and re-fetches every fighter (upsert by name).
- **Events:** load `SELECT title FROM events` into a set; iterate the events listing **newest-first**
  and **stop at the first already-stored title** (unless `--full`). Honor `--limit` (max events saved).
- **Refresh:** after new events are saved, the fighters who fought in them are re-fetched + upserted
  (their record / career averages changed). Skipped under `--full` (already refetched) or `--no-refresh`.
