//! Offline, deterministic integration tests for `db.rs`.
//!
//! Each test builds a TEMP SQLite file on disk, creates the four real tables
//! (matching docs/SCHEMA_CONTRACT.md), inserts a handful of rows including some
//! NULLs, then opens it READ-ONLY through `Db::open` and asserts the typed query
//! results — covering ordering, NULL handling, search, and not-found cases.
//!
//! The crate is a pure binary (no `[lib]` target and `main.rs` declares its
//! modules privately), so an external test cannot `use mma_tui::db`. We instead
//! `include!` the module source directly into this test crate via `#[path]`,
//! which compiles and tests the exact same code. `src/db.rs` does
//! `use crate::models::...`, so we also pull in `src/models.rs` as the test
//! crate's `models` module so that path resolves identically.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "../src/models.rs"]
mod models;

#[path = "../src/db.rs"]
mod db_impl;

use db_impl::Db;
use rusqlite::Connection;

/// RAII temp-file guard: holds a unique on-disk path and deletes it (plus any
/// WAL/SHM siblings) on drop so tests leave nothing behind.
struct TempDb {
    path: PathBuf,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempDb {
    /// Create a uniquely-named temp DB path, build the schema, and seed rows.
    fn new() -> TempDb {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mma_tui_db_test_{}_{}.sqlite",
            std::process::id(),
            n
        ));
        // Start clean in case a previous crashed run left the file behind.
        let _ = std::fs::remove_file(&path);

        {
            let conn = Connection::open(&path).expect("create temp db");
            create_schema(&conn);
            seed(&conn);
        } // writer connection dropped here; file is closed before read-only open.

        TempDb { path }
    }

    fn open(&self) -> Db {
        Db::open(&self.path).expect("open read-only")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(self.path.with_extension("sqlite-shm"));
    }
}

/// Create the 4 tables exactly as the schema contract defines them.
fn create_schema(conn: &Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE fighters (
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

        CREATE TABLE events (
            event_id   INTEGER PRIMARY KEY AUTOINCREMENT,
            title      TEXT NOT NULL UNIQUE,
            date       TEXT,
            location   TEXT
        );

        CREATE TABLE fights (
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

        CREATE TABLE round_stats (
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
        "#,
    )
    .expect("create schema");
}

/// Seed deterministic rows. Fully-specified rows mix with rows that leave
/// nullable columns NULL so the Option<T> mapping is exercised both ways.
fn seed(conn: &Connection) {
    // Fighter 1: every column populated.
    conn.execute(
        "INSERT INTO fighters (
            fighter_id, name, nickname, nationality, height_in, weight_lbs, reach_in,
            stance, date_of_birth, wins, losses, draws, no_contests, was_champion,
            championship_bouts_won, slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def, sub_avg
         ) VALUES (
            1, 'Jon Jones', 'Bones', 'United States', 76, 205, 84,
            'Orthodox', '1987-07-19', 27, 1, 0, 1, 1,
            8, 4.29, 0.57, 2.22, 0.64, 1.92, 0.46, 0.95, 0.5
         )",
        [],
    )
    .unwrap();

    // Fighter 2: all nullable numeric/text columns left NULL; NOT-NULL counters
    // omitted so they take their DEFAULT 0.
    conn.execute(
        "INSERT INTO fighters (fighter_id, name) VALUES (2, 'Anderson Silva')",
        [],
    )
    .unwrap();

    // Fighter 3: distinct name for case-insensitive search checks.
    conn.execute(
        "INSERT INTO fighters (fighter_id, name, stance) VALUES (3, 'Jan Blachowicz', 'Southpaw')",
        [],
    )
    .unwrap();

    // Events: one with a date, one earlier, one with NULL date.
    conn.execute(
        "INSERT INTO events (event_id, title, date, location)
         VALUES (1, 'UFC 285', '2023-03-04', 'Las Vegas, Nevada, USA')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (event_id, title, date, location)
         VALUES (2, 'UFC 200', '2016-07-09', 'Las Vegas, Nevada, USA')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (event_id, title, date, location)
         VALUES (3, 'UFC TBD', NULL, NULL)",
        [],
    )
    .unwrap();

    // Fights: fully specified + one with NULL nullable fields (draw-ish).
    conn.execute(
        "INSERT INTO fights (
            fight_id, event_id, event_name, date, winner_name, loser_name,
            weight_class, title_bout, method, round_ended, time_ended, referee
         ) VALUES (
            1, 1, 'UFC 285', '2023-03-04', 'Jon Jones', 'Ciryl Gane',
            'Heavyweight', 1, 'Submission', 1, 124, 'Marc Goddard'
         )",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fights (
            fight_id, event_id, event_name, date, winner_name, loser_name,
            weight_class, title_bout, method, round_ended, time_ended, referee
         ) VALUES (
            2, 2, 'UFC 200', '2016-07-09', 'Jon Jones', 'Anderson Silva',
            'Light Heavyweight', 0, 'Decision', 3, 300, NULL
         )",
        [],
    )
    .unwrap();
    // Fight 3: nullable cols NULL, NOT-NULL ints take defaults.
    conn.execute(
        "INSERT INTO fights (fight_id, event_id, event_name) VALUES (3, 1, 'UFC 285')",
        [],
    )
    .unwrap();

    // Round stats for fight 1: two fighters, fighter 'Jon Jones' across 2 rounds,
    // 'Ciryl Gane' for 1 round. Inserted out of order to verify ORDER BY.
    conn.execute(
        "INSERT INTO round_stats (
            round_stat_id, fight_id, fighter_name, result, round_number,
            knockdowns, sub_attempts, reversals, control_time,
            td_landed, td_attempted, td_pct,
            sig_str_landed, sig_str_attempted, sig_str_pct
         ) VALUES (
            10, 1, 'Jon Jones', 'w', 2, 0, 1, 0, 90, 2, 3, 0.6667, 14, 20, 0.7
         )",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO round_stats (
            round_stat_id, fight_id, fighter_name, result, round_number,
            knockdowns, sub_attempts, control_time,
            sig_str_landed, sig_str_attempted, sig_str_pct
         ) VALUES (
            11, 1, 'Jon Jones', 'w', 1, 0, 0, 30, 9, 12, 0.75
         )",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO round_stats (
            round_stat_id, fight_id, fighter_name, result, round_number,
            sig_str_landed, sig_str_attempted, sig_str_pct
         ) VALUES (
            12, 1, 'Ciryl Gane', 'l', 1, 6, 18, 0.3333
         )",
        [],
    )
    .unwrap();
    // Round stat with NULL fighter_name/result/round_number to test Option mapping.
    conn.execute(
        "INSERT INTO round_stats (round_stat_id, fight_id) VALUES (13, 2)",
        [],
    )
    .unwrap();
}

#[test]
fn open_is_read_only() {
    let tmp = TempDb::new();
    let db = tmp.open();
    // Any write must be rejected on a read-only connection.
    let err = db
        .conn
        .execute("INSERT INTO fighters (name) VALUES ('Should Fail')", []);
    assert!(err.is_err(), "writes must fail on a read-only connection");
}

#[test]
fn load_fighters_orders_by_name_and_maps_nulls() {
    let tmp = TempDb::new();
    let db = tmp.open();
    let fighters = db.load_fighters().unwrap();
    assert_eq!(fighters.len(), 3);

    // Ordered by name COLLATE NOCASE: Anderson, Jan, Jon.
    let names: Vec<&str> = fighters.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Anderson Silva", "Jan Blachowicz", "Jon Jones"]);

    // Fully-populated row maps every field.
    let jones = fighters.iter().find(|f| f.name == "Jon Jones").unwrap();
    assert_eq!(jones.fighter_id, 1);
    assert_eq!(jones.nickname.as_deref(), Some("Bones"));
    assert_eq!(jones.nationality.as_deref(), Some("United States"));
    assert_eq!(jones.height_in, Some(76));
    assert_eq!(jones.weight_lbs, Some(205));
    assert_eq!(jones.reach_in, Some(84));
    assert_eq!(jones.stance.as_deref(), Some("Orthodox"));
    assert_eq!(jones.date_of_birth.as_deref(), Some("1987-07-19"));
    assert_eq!(jones.wins, 27);
    assert_eq!(jones.losses, 1);
    assert_eq!(jones.was_champion, 1);
    assert_eq!(jones.championship_bouts_won, 8);
    assert_eq!(jones.str_acc, Some(0.57));
    assert_eq!(jones.sub_avg, Some(0.5));

    // Sparse row: nullable columns NULL, NOT-NULL counters default to 0.
    let silva = fighters
        .iter()
        .find(|f| f.name == "Anderson Silva")
        .unwrap();
    assert_eq!(silva.fighter_id, 2);
    assert_eq!(silva.nickname, None);
    // nationality has a DEFAULT 'Unlisted' so it is non-null even when omitted.
    assert_eq!(silva.nationality.as_deref(), Some("Unlisted"));
    assert_eq!(silva.height_in, None);
    assert_eq!(silva.weight_lbs, None);
    assert_eq!(silva.reach_in, None);
    assert_eq!(silva.stance, None);
    assert_eq!(silva.date_of_birth, None);
    assert_eq!(silva.wins, 0);
    assert_eq!(silva.losses, 0);
    assert_eq!(silva.draws, 0);
    assert_eq!(silva.no_contests, 0);
    assert_eq!(silva.was_champion, 0);
    assert_eq!(silva.championship_bouts_won, 0);
    assert_eq!(silva.slpm, None);
    assert_eq!(silva.str_acc, None);
    assert_eq!(silva.td_def, None);
    assert_eq!(silva.sub_avg, None);
}

#[test]
fn search_fighters_empty_returns_all() {
    let tmp = TempDb::new();
    let db = tmp.open();
    let all = db.search_fighters("").unwrap();
    assert_eq!(all.len(), 3);
    // Whitespace-only is also treated as empty.
    let all_ws = db.search_fighters("   ").unwrap();
    assert_eq!(all_ws.len(), 3);
}

#[test]
fn search_fighters_case_insensitive_substring() {
    let tmp = TempDb::new();
    let db = tmp.open();

    // "jo" matches "Jon Jones" only (case-insensitive).
    let jo = db.search_fighters("jo").unwrap();
    assert_eq!(jo.len(), 1);
    assert_eq!(jo[0].name, "Jon Jones");

    // "ja" matches "Jan Blachowicz" only.
    let ja = db.search_fighters("JA").unwrap();
    assert_eq!(ja.len(), 1);
    assert_eq!(ja[0].name, "Jan Blachowicz");

    // "silva" substring, case-insensitive.
    let silva = db.search_fighters("SiLvA").unwrap();
    assert_eq!(silva.len(), 1);
    assert_eq!(silva[0].name, "Anderson Silva");

    // No match.
    let none = db.search_fighters("zzzznomatch").unwrap();
    assert!(none.is_empty());
}

#[test]
fn search_fighters_treats_wildcards_literally() {
    let tmp = TempDb::new();
    let db = tmp.open();
    // A bare LIKE pattern char must NOT act as a wildcard; no name contains '%'.
    let pct = db.search_fighters("%").unwrap();
    assert!(
        pct.is_empty(),
        "'%' must be matched literally, not as a wildcard"
    );
    let underscore = db.search_fighters("_").unwrap();
    assert!(underscore.is_empty(), "'_' must be matched literally");
}

#[test]
fn fighter_profile_found_and_not_found() {
    let tmp = TempDb::new();
    let db = tmp.open();

    let found = db.fighter_profile("Jon Jones").unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().fighter_id, 1);

    // Exact match only: a substring must NOT resolve.
    let partial = db.fighter_profile("Jon").unwrap();
    assert!(partial.is_none());

    let missing = db.fighter_profile("Nobody Here").unwrap();
    assert!(missing.is_none());
}

#[test]
fn load_events_most_recent_first_nulls_last() {
    let tmp = TempDb::new();
    let db = tmp.open();
    let events = db.load_events().unwrap();
    assert_eq!(events.len(), 3);

    // Dated events newest-first, then the NULL-dated event last.
    assert_eq!(events[0].title, "UFC 285");
    assert_eq!(events[0].date.as_deref(), Some("2023-03-04"));
    assert_eq!(
        events[0].location.as_deref(),
        Some("Las Vegas, Nevada, USA")
    );
    assert_eq!(events[1].title, "UFC 200");
    assert_eq!(events[1].date.as_deref(), Some("2016-07-09"));
    assert_eq!(events[2].title, "UFC TBD");
    assert_eq!(events[2].date, None);
    assert_eq!(events[2].location, None);
}

#[test]
fn fights_for_fighter_matches_winner_or_loser() {
    let tmp = TempDb::new();
    let db = tmp.open();

    // Jon Jones is in fights 1 (winner) and 2 (winner); newest-first.
    let jones = db.fights_for_fighter("Jon Jones").unwrap();
    assert_eq!(jones.len(), 2);
    assert_eq!(jones[0].fight_id, 1);
    assert_eq!(jones[0].date.as_deref(), Some("2023-03-04"));
    assert_eq!(jones[1].fight_id, 2);

    // Anderson Silva appears only as a loser in fight 2.
    let silva = db.fights_for_fighter("Anderson Silva").unwrap();
    assert_eq!(silva.len(), 1);
    assert_eq!(silva[0].fight_id, 2);
    assert_eq!(silva[0].loser_name.as_deref(), Some("Anderson Silva"));
    // Fight 2 has a NULL referee — verify Option mapping.
    assert_eq!(silva[0].referee, None);

    let none = db.fights_for_fighter("Nobody").unwrap();
    assert!(none.is_empty());
}

#[test]
fn fights_for_event_card_order_and_field_mapping() {
    let tmp = TempDb::new();
    let db = tmp.open();

    // Event 1 has fights 1 and 3, ordered by fight_id (card order).
    let card = db.fights_for_event(1).unwrap();
    assert_eq!(card.len(), 2);
    assert_eq!(card[0].fight_id, 1);
    assert_eq!(card[1].fight_id, 3);

    // Fully-specified fight maps all fields incl. integers & seconds.
    let f1 = &card[0];
    assert_eq!(f1.event_id, Some(1));
    assert_eq!(f1.event_name.as_deref(), Some("UFC 285"));
    assert_eq!(f1.winner_name.as_deref(), Some("Jon Jones"));
    assert_eq!(f1.loser_name.as_deref(), Some("Ciryl Gane"));
    assert_eq!(f1.weight_class.as_deref(), Some("Heavyweight"));
    assert_eq!(f1.title_bout, 1);
    assert_eq!(f1.method.as_deref(), Some("Submission"));
    assert_eq!(f1.round_ended, 1);
    assert_eq!(f1.time_ended, 124);
    assert_eq!(f1.referee.as_deref(), Some("Marc Goddard"));

    // Sparse fight: nullable cols NULL, NOT-NULL ints default to 0.
    let f3 = &card[1];
    assert_eq!(f3.date, None);
    assert_eq!(f3.winner_name, None);
    assert_eq!(f3.loser_name, None);
    assert_eq!(f3.weight_class, None);
    assert_eq!(f3.method, None);
    assert_eq!(f3.referee, None);
    assert_eq!(f3.title_bout, 0);
    assert_eq!(f3.round_ended, 0);
    assert_eq!(f3.time_ended, 0);

    // Event 2 has only fight 2.
    let card2 = db.fights_for_event(2).unwrap();
    assert_eq!(card2.len(), 1);
    assert_eq!(card2[0].fight_id, 2);

    // No such event.
    let empty = db.fights_for_event(999).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn rounds_for_fight_ordered_and_typed() {
    let tmp = TempDb::new();
    let db = tmp.open();

    // Fight 1: ordered by fighter_name then round_number.
    // Ciryl Gane (1 round) sorts before Jon Jones; Jones rounds 1 then 2.
    let rounds = db.rounds_for_fight(1).unwrap();
    assert_eq!(rounds.len(), 3);

    assert_eq!(rounds[0].fighter_name.as_deref(), Some("Ciryl Gane"));
    assert_eq!(rounds[0].round_number, Some(1));
    assert_eq!(rounds[0].result.as_deref(), Some("l"));

    assert_eq!(rounds[1].fighter_name.as_deref(), Some("Jon Jones"));
    assert_eq!(rounds[1].round_number, Some(1));
    assert_eq!(rounds[2].fighter_name.as_deref(), Some("Jon Jones"));
    assert_eq!(rounds[2].round_number, Some(2));

    // Typed field mapping: round 2 row (round_stat_id 10).
    let r2 = rounds.iter().find(|r| r.round_stat_id == 10).unwrap();
    assert_eq!(r2.result.as_deref(), Some("w"));
    assert_eq!(r2.sub_attempts, 1);
    assert_eq!(r2.control_time, 90);
    assert_eq!(r2.td_landed, 2);
    assert_eq!(r2.td_attempted, 3);
    assert!((r2.td_pct - 0.6667).abs() < 1e-9);
    assert_eq!(r2.sig_str_landed, 14);
    assert_eq!(r2.sig_str_attempted, 20);
    assert!((r2.sig_str_pct - 0.7).abs() < 1e-9);
    // Counters not set in INSERT keep their DEFAULT 0 (NOT NULL).
    assert_eq!(r2.knockdowns, 0);
    assert_eq!(r2.reversals, 0);
    assert_eq!(r2.head_landed, 0);
    assert_eq!(r2.ground_pct, 0.0);

    // Fight 2: single row with NULL nullable columns.
    let rounds2 = db.rounds_for_fight(2).unwrap();
    assert_eq!(rounds2.len(), 1);
    assert_eq!(rounds2[0].round_stat_id, 13);
    assert_eq!(rounds2[0].fight_id, Some(2));
    assert_eq!(rounds2[0].fighter_name, None);
    assert_eq!(rounds2[0].result, None);
    assert_eq!(rounds2[0].round_number, None);
    // NOT-NULL counters default to 0.
    assert_eq!(rounds2[0].knockdowns, 0);
    assert_eq!(rounds2[0].sig_str_landed, 0);

    // No rounds for a fight without stats.
    let empty = db.rounds_for_fight(3).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn summary_counts_and_date_span() {
    let tmp = TempDb::new();
    let db = tmp.open();
    let s = db.summary().unwrap();
    assert_eq!(s.n_fighters, 3);
    assert_eq!(s.n_events, 3);
    assert_eq!(s.n_fights, 3);
    assert_eq!(s.n_round_stats, 4);
    // MIN/MAX ignore the NULL-dated event.
    assert_eq!(s.earliest_event.as_deref(), Some("2016-07-09"));
    assert_eq!(s.latest_event.as_deref(), Some("2023-03-04"));
}
