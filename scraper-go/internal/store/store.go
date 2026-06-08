// Package store owns the SQLite database (data/ufc.db). It is the sole writer.
//
// Design: a SINGLE writer goroutine owns the *sql.DB. All mutating calls
// (UpsertFighter, SaveEvent) are submitted as closures over a channel and run
// on that one goroutine, so even though many fetch/parse workers run
// concurrently, writes are fully serialized — no SQLITE_BUSY contention between
// writers, and each SaveEvent runs its whole event (event + fights +
// round_stats) in one transaction. Read helpers (ExistingFighterNames,
// ExistingEventTitles) run on the caller's goroutine; SQLite in WAL mode allows
// reads concurrent with the single writer.
package store

import (
	"database/sql"
	"fmt"
	"os"
	"path/filepath"

	_ "modernc.org/sqlite" // pure-Go SQLite driver, registered as "sqlite"

	"ufcscraper/internal/model"
)

// Store wraps the database and the single-writer machinery.
type Store struct {
	db     *sql.DB
	cmds   chan func()
	closed chan struct{}
}

// Open creates the parent directory if missing, opens the DB via the pure-Go
// "sqlite" driver, sets WAL + busy_timeout + foreign_keys pragmas, applies the
// contract DDL, and starts the single writer goroutine. Callers must Close the
// returned Store to stop that goroutine and flush.
func Open(path string) (*Store, error) {
	if dir := filepath.Dir(path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return nil, fmt.Errorf("create db dir %s: %w", dir, err)
		}
	}

	// busy_timeout via DSN so even the initial pragma/DDL statements wait
	// rather than failing fast if the file is briefly locked.
	dsn := path + "?_pragma=busy_timeout(5000)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open sqlite %s: %w", path, err)
	}

	// Single writer: serialize at the application layer, and also pin the
	// pool to one connection so WAL/pragma state is consistent for writes.
	db.SetMaxOpenConns(1)

	for _, pragma := range []string{
		"PRAGMA journal_mode = WAL;",
		"PRAGMA busy_timeout = 5000;",
		"PRAGMA foreign_keys = ON;",
	} {
		if _, err := db.Exec(pragma); err != nil {
			db.Close()
			return nil, fmt.Errorf("apply %q: %w", pragma, err)
		}
	}

	if _, err := db.Exec(schemaDDL); err != nil {
		db.Close()
		return nil, fmt.Errorf("apply schema: %w", err)
	}

	s := &Store{
		db:     db,
		cmds:   make(chan func()),
		closed: make(chan struct{}),
	}
	go s.writerLoop()
	return s, nil
}

// writerLoop is the single goroutine that owns all writes. It runs submitted
// closures one at a time until the cmds channel is closed.
func (s *Store) writerLoop() {
	defer close(s.closed)
	for fn := range s.cmds {
		fn()
	}
}

// submit runs fn on the writer goroutine and blocks until it completes,
// returning fn's error. This is how UpsertFighter / SaveEvent stay synchronous
// for callers while keeping all DB mutation on one goroutine.
func (s *Store) submit(fn func() error) error {
	resultCh := make(chan error, 1)
	s.cmds <- func() { resultCh <- fn() }
	return <-resultCh
}

// Close stops the writer goroutine (waiting for in-flight work to finish) and
// closes the database.
func (s *Store) Close() error {
	close(s.cmds)
	<-s.closed
	return s.db.Close()
}

// ExistingFighterNames returns the set of fighter names already stored, for the
// incremental fighter pass (existing -> update, new -> insert).
func (s *Store) ExistingFighterNames() (map[string]bool, error) {
	return s.querySet("SELECT name FROM fighters")
}

// ExistingEventTitles returns the set of event titles already stored, for the
// newest-first early-stop in the incremental event pass.
func (s *Store) ExistingEventTitles() (map[string]bool, error) {
	return s.querySet("SELECT title FROM events")
}

// querySet runs a single-column query and collects the values into a set.
func (s *Store) querySet(query string) (map[string]bool, error) {
	rows, err := s.db.Query(query)
	if err != nil {
		return nil, fmt.Errorf("query %q: %w", query, err)
	}
	defer rows.Close()

	set := make(map[string]bool)
	for rows.Next() {
		var v string
		if err := rows.Scan(&v); err != nil {
			return nil, fmt.Errorf("scan %q: %w", query, err)
		}
		set[v] = true
	}
	return set, rows.Err()
}

// fighterUpsertSQL upserts a fighter by unique name. On conflict it refreshes
// the record, physical attributes, and career averages — but deliberately does
// NOT touch was_champion / championship_bouts_won, which are accumulated by
// SaveEvent for title bouts and must survive a re-scrape.
const fighterUpsertSQL = `
INSERT INTO fighters (
    name, nickname, nationality, height_in, weight_lbs, reach_in, stance,
    date_of_birth, wins, losses, draws, no_contests,
    slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def, sub_avg
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
ON CONFLICT(name) DO UPDATE SET
    nickname      = excluded.nickname,
    nationality   = excluded.nationality,
    height_in     = excluded.height_in,
    weight_lbs    = excluded.weight_lbs,
    reach_in      = excluded.reach_in,
    stance        = excluded.stance,
    date_of_birth = excluded.date_of_birth,
    wins          = excluded.wins,
    losses        = excluded.losses,
    draws         = excluded.draws,
    no_contests   = excluded.no_contests,
    slpm    = excluded.slpm,
    str_acc = excluded.str_acc,
    sapm    = excluded.sapm,
    str_def = excluded.str_def,
    td_avg  = excluded.td_avg,
    td_acc  = excluded.td_acc,
    td_def  = excluded.td_def,
    sub_avg = excluded.sub_avg
`

// UpsertFighter inserts or updates one fighter by name, on the writer goroutine.
func (s *Store) UpsertFighter(f model.Fighter) error {
	return s.submit(func() error {
		_, err := s.db.Exec(fighterUpsertSQL,
			f.Name, f.Nickname, f.Nationality, f.HeightIn, f.WeightLbs, f.ReachIn, f.Stance,
			f.DOB, f.Wins, f.Losses, f.Draws, f.NoContests,
			f.SLpM, f.StrAcc, f.SApM, f.StrDef, f.TDAvg, f.TDAcc, f.TDDef, f.SubAvg,
		)
		if err != nil {
			return fmt.Errorf("upsert fighter %q: %w", f.Name, err)
		}
		return nil
	})
}

// SaveEvent persists an entire event — the events row, every fights row, and
// every round_stats row — in ONE transaction, on the writer goroutine. It also
// applies the champion-status update: a title bout WITH a winner sets that
// winner's was_champion = 1 and increments championship_bouts_won (only if the
// winner already exists in fighters; per the contract we do not create
// placeholder fighter rows for card-only names).
func (s *Store) SaveEvent(ev model.Event) error {
	return s.submit(func() (err error) {
		tx, err := s.db.Begin()
		if err != nil {
			return fmt.Errorf("begin tx for event %q: %w", ev.Title, err)
		}
		// Roll back on any error; commit only on the happy path.
		defer func() {
			if err != nil {
				_ = tx.Rollback()
			}
		}()

		res, err := tx.Exec(
			`INSERT INTO events (title, date, location) VALUES (?,?,?)`,
			ev.Title, ev.Date, ev.Location,
		)
		if err != nil {
			return fmt.Errorf("insert event %q: %w", ev.Title, err)
		}
		eventID, err := res.LastInsertId()
		if err != nil {
			return fmt.Errorf("event id for %q: %w", ev.Title, err)
		}

		for _, fight := range ev.Fights {
			if err = insertFight(tx, eventID, fight); err != nil {
				return err
			}
		}

		return tx.Commit()
	})
}

// insertFight writes one fight row, its two competitors' round_stats rows, and
// applies the champion-status update for a title bout with a winner.
func insertFight(tx *sql.Tx, eventID int64, fight model.Fight) error {
	res, err := tx.Exec(`
INSERT INTO fights (
    event_id, event_name, date, winner_name, loser_name, weight_class,
    title_bout, method, round_ended, time_ended, referee
) VALUES (?,?,?,?,?,?,?,?,?,?,?)`,
		eventID, fight.EventName, fight.Date, fight.WinnerName, fight.LoserName,
		fight.WeightClass, fight.TitleBout, fight.Method, fight.RoundEnded,
		fight.TimeEnded, fight.Referee,
	)
	if err != nil {
		return fmt.Errorf("insert fight (%s) in event %d: %w", fight.WeightClass, eventID, err)
	}
	fightID, err := res.LastInsertId()
	if err != nil {
		return fmt.Errorf("fight id in event %d: %w", eventID, err)
	}

	for _, rs := range fight.Competitor1.Rounds {
		if err := insertRoundStat(tx, fightID, rs); err != nil {
			return err
		}
	}
	for _, rs := range fight.Competitor2.Rounds {
		if err := insertRoundStat(tx, fightID, rs); err != nil {
			return err
		}
	}

	// Champion status: title bout with a winner -> winner was_champion = 1,
	// championship_bouts_won += 1. UPDATE-only (no insert): if the winner is
	// not in fighters, this affects 0 rows, which is the intended behavior.
	if fight.TitleBout == 1 && fight.WinnerName != "" {
		_, err := tx.Exec(
			`UPDATE fighters
			    SET was_champion = 1,
			        championship_bouts_won = championship_bouts_won + 1
			  WHERE name = ?`,
			fight.WinnerName,
		)
		if err != nil {
			return fmt.Errorf("champion update for %q: %w", fight.WinnerName, err)
		}
	}
	return nil
}

// insertRoundStat writes one round_stats row.
func insertRoundStat(tx *sql.Tx, fightID int64, rs model.RoundStat) error {
	_, err := tx.Exec(`
INSERT INTO round_stats (
    fight_id, fighter_name, result, round_number,
    knockdowns, sub_attempts, reversals, control_time,
    td_landed, td_attempted, td_pct,
    sig_str_landed, sig_str_attempted, sig_str_pct,
    total_str_landed, total_str_attempted, total_str_pct,
    head_landed, head_attempted, head_pct,
    body_landed, body_attempted, body_pct,
    leg_landed, leg_attempted, leg_pct,
    distance_landed, distance_attempted, distance_pct,
    clinch_landed, clinch_attempted, clinch_pct,
    ground_landed, ground_attempted, ground_pct
) VALUES (
    ?,?,?,?,
    ?,?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?,
    ?,?,?
)`,
		fightID, rs.FighterName, rs.Result, rs.RoundNumber,
		rs.Knockdowns, rs.SubAttempts, rs.Reversals, rs.ControlTime,
		rs.TDLanded, rs.TDAttempted, rs.TDPct,
		rs.SigStrLanded, rs.SigStrAttempted, rs.SigStrPct,
		rs.TotalStrLanded, rs.TotalStrAttempted, rs.TotalStrPct,
		rs.HeadLanded, rs.HeadAttempted, rs.HeadPct,
		rs.BodyLanded, rs.BodyAttempted, rs.BodyPct,
		rs.LegLanded, rs.LegAttempted, rs.LegPct,
		rs.DistanceLanded, rs.DistanceAttempted, rs.DistancePct,
		rs.ClinchLanded, rs.ClinchAttempted, rs.ClinchPct,
		rs.GroundLanded, rs.GroundAttempted, rs.GroundPct,
	)
	if err != nil {
		return fmt.Errorf("insert round_stat (%s r%d): %w", rs.FighterName, rs.RoundNumber, err)
	}
	return nil
}
