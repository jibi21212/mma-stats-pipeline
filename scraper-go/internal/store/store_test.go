package store

import (
	"database/sql"
	"path/filepath"
	"testing"

	"ufcscraper/internal/model"
)

// openTemp opens a store backed by a temp-file DB (a real file, not :memory:,
// so the single-connection WAL setup is exercised as in production). The store
// is closed automatically when the test ends.
func openTemp(t *testing.T) *Store {
	t.Helper()
	path := filepath.Join(t.TempDir(), "test.db")
	s, err := Open(path)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { s.Close() })
	return s
}

func TestUpsertFighter_InsertAndReadBack(t *testing.T) {
	s := openTemp(t)

	f := model.Fighter{
		Name:        "Jon Jones",
		Nickname:    sql.NullString{String: "Bones", Valid: true},
		Nationality: "Unlisted",
		HeightIn:    sql.NullInt64{Int64: 76, Valid: true},
		WeightLbs:   sql.NullInt64{Int64: 205, Valid: true},
		ReachIn:     sql.NullInt64{Int64: 84, Valid: true},
		Stance:      sql.NullString{String: "Orthodox", Valid: true},
		DOB:         sql.NullString{String: "1987-07-19", Valid: true},
		Wins:        27, Losses: 1, Draws: 0, NoContests: 1,
		SLpM:   sql.NullFloat64{Float64: 4.30, Valid: true},
		StrAcc: sql.NullFloat64{Float64: 0.57, Valid: true},
		// Leave SApM NULL on purpose to verify NULL round-trips distinct from 0.
	}
	if err := s.UpsertFighter(f); err != nil {
		t.Fatalf("UpsertFighter: %v", err)
	}

	names, err := s.ExistingFighterNames()
	if err != nil {
		t.Fatalf("ExistingFighterNames: %v", err)
	}
	if !names["Jon Jones"] {
		t.Fatalf("expected Jon Jones in name set, got %v", names)
	}

	var (
		name      string
		height    sql.NullInt64
		strAcc    sql.NullFloat64
		sapm      sql.NullFloat64
		wins      int
		champ     int
		boutsWon  int
	)
	row := s.db.QueryRow(
		`SELECT name, height_in, str_acc, sapm, wins, was_champion, championship_bouts_won
		   FROM fighters WHERE name = ?`, "Jon Jones")
	if err := row.Scan(&name, &height, &strAcc, &sapm, &wins, &champ, &boutsWon); err != nil {
		t.Fatalf("scan fighter: %v", err)
	}
	if name != "Jon Jones" || !height.Valid || height.Int64 != 76 {
		t.Errorf("name/height = %q/%+v", name, height)
	}
	if !strAcc.Valid || strAcc.Float64 != 0.57 {
		t.Errorf("str_acc = %+v, want 0.57", strAcc)
	}
	if sapm.Valid {
		t.Errorf("sapm = %+v, want NULL (distinct from 0)", sapm)
	}
	if wins != 27 {
		t.Errorf("wins = %d, want 27", wins)
	}
	if champ != 0 || boutsWon != 0 {
		t.Errorf("champ/boutsWon = %d/%d, want 0/0 before any title bout", champ, boutsWon)
	}

	// Upsert again with updated record; verify it updates in place (no dup).
	f.Wins = 28
	if err := s.UpsertFighter(f); err != nil {
		t.Fatalf("second UpsertFighter: %v", err)
	}
	var count, winsAfter int
	if err := s.db.QueryRow(`SELECT COUNT(*), MAX(wins) FROM fighters WHERE name=?`, "Jon Jones").
		Scan(&count, &winsAfter); err != nil {
		t.Fatalf("scan count: %v", err)
	}
	if count != 1 {
		t.Errorf("row count = %d, want 1 (upsert, not insert)", count)
	}
	if winsAfter != 28 {
		t.Errorf("wins after upsert = %d, want 28", winsAfter)
	}
}

func TestSaveEvent_InsertAndReadBack(t *testing.T) {
	s := openTemp(t)

	// Seed the winner so the champion-status update has a row to touch.
	if err := s.UpsertFighter(model.Fighter{Name: "Jon Jones", Nationality: "Unlisted"}); err != nil {
		t.Fatalf("seed fighter: %v", err)
	}

	ev := model.Event{
		Title:    "UFC 285: Jones vs Gane",
		Date:     "2023-03-04",
		Location: "Las Vegas, Nevada, USA",
		Fights: []model.Fight{
			{
				EventName:   "UFC 285",
				Date:        "2023-03-04",
				WinnerName:  "Jon Jones",
				LoserName:   "Ciryl Gane",
				WeightClass: "UFC Heavyweight Title Bout",
				TitleBout:   1,
				Method:      "Submission",
				RoundEnded:  1,
				TimeEnded:   124,
				Referee:     "Marc Goddard",
				Competitor1: model.Competitor{
					FighterName: "Jon Jones",
					Rounds: []model.RoundStat{
						{FighterName: "Jon Jones", Result: "w", RoundNumber: 1,
							Knockdowns: 1, SubAttempts: 1, ControlTime: 84,
							TDLanded: 1, TDAttempted: 2, TDPct: 0.5,
							SigStrLanded: 10, SigStrAttempted: 20, SigStrPct: 0.5,
							HeadLanded: 6, HeadAttempted: 10, HeadPct: 0.6},
					},
				},
				Competitor2: model.Competitor{
					FighterName: "Ciryl Gane",
					Rounds: []model.RoundStat{
						{FighterName: "Ciryl Gane", Result: "l", RoundNumber: 1},
					},
				},
			},
		},
	}
	if err := s.SaveEvent(ev); err != nil {
		t.Fatalf("SaveEvent: %v", err)
	}

	// Event row round-trips.
	titles, err := s.ExistingEventTitles()
	if err != nil {
		t.Fatalf("ExistingEventTitles: %v", err)
	}
	if !titles["UFC 285: Jones vs Gane"] {
		t.Fatalf("expected event title in set, got %v", titles)
	}

	var loc, date string
	if err := s.db.QueryRow(`SELECT date, location FROM events WHERE title=?`,
		"UFC 285: Jones vs Gane").Scan(&date, &loc); err != nil {
		t.Fatalf("scan event: %v", err)
	}
	if date != "2023-03-04" || loc != "Las Vegas, Nevada, USA" {
		t.Errorf("event date/loc = %q/%q", date, loc)
	}

	// One fight, title bout flagged.
	var fightCount, titleBout int
	var winner string
	if err := s.db.QueryRow(
		`SELECT COUNT(*), MAX(title_bout), MAX(winner_name) FROM fights`).
		Scan(&fightCount, &titleBout, &winner); err != nil {
		t.Fatalf("scan fights: %v", err)
	}
	if fightCount != 1 || titleBout != 1 || winner != "Jon Jones" {
		t.Errorf("fights: count=%d title=%d winner=%q", fightCount, titleBout, winner)
	}

	// Two round_stats rows (one per competitor), with the head-strike values.
	var rsCount int
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM round_stats`).Scan(&rsCount); err != nil {
		t.Fatalf("scan round_stats count: %v", err)
	}
	if rsCount != 2 {
		t.Errorf("round_stats count = %d, want 2", rsCount)
	}

	var head, headAtt int
	var headPct float64
	var result string
	if err := s.db.QueryRow(
		`SELECT result, head_landed, head_attempted, head_pct
		   FROM round_stats WHERE fighter_name=?`, "Jon Jones").
		Scan(&result, &head, &headAtt, &headPct); err != nil {
		t.Fatalf("scan round_stat: %v", err)
	}
	if result != "w" || head != 6 || headAtt != 10 || headPct != 0.6 {
		t.Errorf("round_stat = result %q head %d/%d/%v, want w 6/10/0.6", result, head, headAtt, headPct)
	}

	// Champion status: title bout with a winner bumped the winner's fields.
	var champ, boutsWon int
	if err := s.db.QueryRow(
		`SELECT was_champion, championship_bouts_won FROM fighters WHERE name=?`,
		"Jon Jones").Scan(&champ, &boutsWon); err != nil {
		t.Fatalf("scan champion: %v", err)
	}
	if champ != 1 || boutsWon != 1 {
		t.Errorf("champion status = %d/%d, want 1/1", champ, boutsWon)
	}

	// Per the contract, no placeholder fighter row is created for the loser.
	loserNames, _ := s.ExistingFighterNames()
	if loserNames["Ciryl Gane"] {
		t.Errorf("loser should NOT have a fighters row (no placeholder rows)")
	}
}
