//! Read-only access to `data/ufc.db` via rusqlite.
//!
//! The Go scraper is the SOLE WRITER; this module opens the DB read-only and
//! never mutates it. All query fns return the frozen `models` row structs.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};

use crate::models::{DbSummary, EventRow, FightRow, Fighter, LatestCard, RoundStat};

/// A read-only handle to the SQLite database.
pub struct Db {
    /// The underlying rusqlite connection (opened read-only).
    pub conn: Connection,
}

// --------------------------------------------------------------------------- //
// Column lists (kept next to the row mappers so SELECT order can never drift).
// --------------------------------------------------------------------------- //

const FIGHTER_COLS: &str = "fighter_id, name, nickname, nationality, height_in, \
    weight_lbs, reach_in, stance, date_of_birth, wins, losses, draws, no_contests, \
    was_champion, championship_bouts_won, slpm, str_acc, sapm, str_def, td_avg, \
    td_acc, td_def, sub_avg";

const EVENT_COLS: &str = "event_id, title, date, location";

const FIGHT_COLS: &str = "fight_id, event_id, event_name, date, winner_name, \
    loser_name, weight_class, title_bout, method, round_ended, time_ended, referee";

const ROUND_COLS: &str = "round_stat_id, fight_id, fighter_name, result, round_number, \
    knockdowns, sub_attempts, reversals, control_time, \
    td_landed, td_attempted, td_pct, \
    sig_str_landed, sig_str_attempted, sig_str_pct, \
    total_str_landed, total_str_attempted, total_str_pct, \
    head_landed, head_attempted, head_pct, \
    body_landed, body_attempted, body_pct, \
    leg_landed, leg_attempted, leg_pct, \
    distance_landed, distance_attempted, distance_pct, \
    clinch_landed, clinch_attempted, clinch_pct, \
    ground_landed, ground_attempted, ground_pct";

// --------------------------------------------------------------------------- //
// Row mappers — column index order MUST match the *_COLS constants above.
// Nullable DB columns map to Option<T>; rusqlite yields None on SQL NULL.
// --------------------------------------------------------------------------- //

fn map_fighter(row: &Row) -> rusqlite::Result<Fighter> {
    Ok(Fighter {
        fighter_id: row.get(0)?,
        name: row.get(1)?,
        nickname: row.get(2)?,
        nationality: row.get(3)?,
        height_in: row.get(4)?,
        weight_lbs: row.get(5)?,
        reach_in: row.get(6)?,
        stance: row.get(7)?,
        date_of_birth: row.get(8)?,
        wins: row.get(9)?,
        losses: row.get(10)?,
        draws: row.get(11)?,
        no_contests: row.get(12)?,
        was_champion: row.get(13)?,
        championship_bouts_won: row.get(14)?,
        slpm: row.get(15)?,
        str_acc: row.get(16)?,
        sapm: row.get(17)?,
        str_def: row.get(18)?,
        td_avg: row.get(19)?,
        td_acc: row.get(20)?,
        td_def: row.get(21)?,
        sub_avg: row.get(22)?,
    })
}

fn map_event(row: &Row) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        event_id: row.get(0)?,
        title: row.get(1)?,
        date: row.get(2)?,
        location: row.get(3)?,
    })
}

fn map_fight(row: &Row) -> rusqlite::Result<FightRow> {
    Ok(FightRow {
        fight_id: row.get(0)?,
        event_id: row.get(1)?,
        event_name: row.get(2)?,
        date: row.get(3)?,
        winner_name: row.get(4)?,
        loser_name: row.get(5)?,
        weight_class: row.get(6)?,
        title_bout: row.get(7)?,
        method: row.get(8)?,
        round_ended: row.get(9)?,
        time_ended: row.get(10)?,
        referee: row.get(11)?,
    })
}

fn map_round(row: &Row) -> rusqlite::Result<RoundStat> {
    Ok(RoundStat {
        round_stat_id: row.get(0)?,
        fight_id: row.get(1)?,
        fighter_name: row.get(2)?,
        result: row.get(3)?,
        round_number: row.get(4)?,
        knockdowns: row.get(5)?,
        sub_attempts: row.get(6)?,
        reversals: row.get(7)?,
        control_time: row.get(8)?,
        td_landed: row.get(9)?,
        td_attempted: row.get(10)?,
        td_pct: row.get(11)?,
        sig_str_landed: row.get(12)?,
        sig_str_attempted: row.get(13)?,
        sig_str_pct: row.get(14)?,
        total_str_landed: row.get(15)?,
        total_str_attempted: row.get(16)?,
        total_str_pct: row.get(17)?,
        head_landed: row.get(18)?,
        head_attempted: row.get(19)?,
        head_pct: row.get(20)?,
        body_landed: row.get(21)?,
        body_attempted: row.get(22)?,
        body_pct: row.get(23)?,
        leg_landed: row.get(24)?,
        leg_attempted: row.get(25)?,
        leg_pct: row.get(26)?,
        distance_landed: row.get(27)?,
        distance_attempted: row.get(28)?,
        distance_pct: row.get(29)?,
        clinch_landed: row.get(30)?,
        clinch_attempted: row.get(31)?,
        clinch_pct: row.get(32)?,
        ground_landed: row.get(33)?,
        ground_attempted: row.get(34)?,
        ground_pct: row.get(35)?,
    })
}

impl Db {
    /// Open `path` read-only (`SQLITE_OPEN_READ_ONLY`). Fails if the file is
    /// missing or not a valid SQLite database.
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("opening DB read-only: {}", path.display()))?;
        Ok(Db { conn })
    }

    /// All fighters, ordered by name.
    pub fn load_fighters(&self) -> Result<Vec<Fighter>> {
        let sql = format!("SELECT {FIGHTER_COLS} FROM fighters ORDER BY name COLLATE NOCASE");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_fighter)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Fighters whose name matches `query` (case-insensitive substring /
    /// SQL-side filter). Empty `query` returns all fighters. Ordered by name.
    ///
    /// Part of the public DB API and covered by `tests/db_tests.rs`. The
    /// fighters screen currently narrows the in-memory roster via `fuzzy`, so
    /// this SQL-side variant is not yet wired into a UI path.
    #[allow(dead_code)]
    pub fn search_fighters(&self, query: &str) -> Result<Vec<Fighter>> {
        if query.trim().is_empty() {
            return self.load_fighters();
        }
        // Escape LIKE wildcards in the user query so they match literally.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let sql = format!(
            "SELECT {FIGHTER_COLS} FROM fighters \
             WHERE name LIKE ?1 ESCAPE '\\' COLLATE NOCASE \
             ORDER BY name COLLATE NOCASE"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([pattern], map_fighter)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Full fighter row by exact name, or `None` if not found.
    pub fn fighter_profile(&self, name: &str) -> Result<Option<Fighter>> {
        let sql = format!("SELECT {FIGHTER_COLS} FROM fighters WHERE name = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let out = stmt.query_row([name], map_fighter).optional()?;
        Ok(out)
    }

    /// All events, most-recent first by date.
    pub fn load_events(&self) -> Result<Vec<EventRow>> {
        // NULL dates sort last; tie-break by event_id desc for stable ordering.
        let sql = format!(
            "SELECT {EVENT_COLS} FROM events \
             ORDER BY date IS NULL, date DESC, event_id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_event)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Every fight in which `name` appears as winner or loser, most-recent first.
    pub fn fights_for_fighter(&self, name: &str) -> Result<Vec<FightRow>> {
        let sql = format!(
            "SELECT {FIGHT_COLS} FROM fights \
             WHERE winner_name = ?1 OR loser_name = ?1 \
             ORDER BY date IS NULL, date DESC, fight_id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([name], map_fight)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// All fights on a given event, ordered as stored (card order).
    pub fn fights_for_event(&self, event_id: i64) -> Result<Vec<FightRow>> {
        let sql = format!("SELECT {FIGHT_COLS} FROM fights WHERE event_id = ?1 ORDER BY fight_id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([event_id], map_fight)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// All `round_stats` rows for one fight, ordered by fighter then round.
    pub fn rounds_for_fight(&self, fight_id: i64) -> Result<Vec<RoundStat>> {
        let sql = format!(
            "SELECT {ROUND_COLS} FROM round_stats \
             WHERE fight_id = ?1 \
             ORDER BY fighter_name COLLATE NOCASE, round_number, round_stat_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([fight_id], map_round)?;
        let out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// The newest NUMBERED UFC card (title matching `UFC <number>`), used as the
    /// headline for the home-screen fight poster.
    ///
    /// "Numbered" means a title like `"UFC 311"` (GLOB `'UFC [0-9]*'`) — it
    /// EXCLUDES `"UFC Fight Night"`, `"UFC on ESPN"`, etc. The newest such event
    /// (by date, then event_id) is returned with its card number parsed out of
    /// the title. If NO numbered card exists, falls back to the newest event of
    /// any kind (with `number == None`). Returns `None` only when there are no
    /// events at all.
    pub fn latest_numbered_card(&self) -> Result<Option<LatestCard>> {
        // Prefer the newest title that GLOB-matches a numbered card.
        let numbered: Option<EventRow> = {
            let sql = format!(
                "SELECT {EVENT_COLS} FROM events \
                 WHERE title GLOB 'UFC [0-9]*' \
                 ORDER BY date IS NULL, date DESC, event_id DESC LIMIT 1"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_row([], map_event).optional()?
        };

        let row = match numbered {
            Some(row) => row,
            None => {
                // Fallback: newest event of ANY kind.
                let sql = format!(
                    "SELECT {EVENT_COLS} FROM events \
                     ORDER BY date IS NULL, date DESC, event_id DESC LIMIT 1"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                match stmt.query_row([], map_event).optional()? {
                    Some(row) => row,
                    None => return Ok(None),
                }
            }
        };

        let number = parse_card_number(&row.title);
        Ok(Some(LatestCard {
            event_id: row.event_id,
            title: row.title,
            date: row.date,
            location: row.location,
            number,
        }))
    }

    /// DB-wide counts and date span for Home / Model screens.
    pub fn summary(&self) -> Result<DbSummary> {
        let n_fighters: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM fighters", [], |r| r.get(0))?;
        let n_events: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        let n_fights: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM fights", [], |r| r.get(0))?;
        let n_round_stats: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM round_stats", [], |r| r.get(0))?;

        // MIN/MAX ignore NULLs; the whole expression is NULL only when no
        // non-null dates exist, which rusqlite maps to None.
        let earliest_event: Option<String> = self.conn.query_row(
            "SELECT MIN(date) FROM events WHERE date IS NOT NULL AND date <> ''",
            [],
            |r| r.get(0),
        )?;
        let latest_event: Option<String> = self.conn.query_row(
            "SELECT MAX(date) FROM events WHERE date IS NOT NULL AND date <> ''",
            [],
            |r| r.get(0),
        )?;

        Ok(DbSummary {
            n_fighters,
            n_events,
            n_fights,
            n_round_stats,
            earliest_event,
            latest_event,
        })
    }
}

/// Parse the card number out of a numbered UFC title like `"UFC 311"` ->
/// `Some(311)`. Returns `None` for non-numbered titles (`"UFC Fight Night"`,
/// `"UFC on ESPN 42"` etc.) — only a title whose first whitespace-separated
/// token after `"UFC"` is purely digits counts. Pure helper, unit-tested below.
pub fn parse_card_number(title: &str) -> Option<u32> {
    let rest = title.strip_prefix("UFC ")?;
    let token = rest.split_whitespace().next()?;
    token.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_card_number_extracts_numbered_cards() {
        assert_eq!(parse_card_number("UFC 311"), Some(311));
        assert_eq!(parse_card_number("UFC 1"), Some(1));
        assert_eq!(parse_card_number("UFC 300"), Some(300));
    }

    #[test]
    fn parse_card_number_rejects_non_numbered() {
        assert_eq!(parse_card_number("UFC Fight Night"), None);
        assert_eq!(parse_card_number("UFC on ESPN 42"), None);
        assert_eq!(parse_card_number("UFC Fight Night: Smith vs Jones"), None);
        assert_eq!(parse_card_number("PFL 5"), None);
        assert_eq!(parse_card_number("UFC"), None);
    }
}
