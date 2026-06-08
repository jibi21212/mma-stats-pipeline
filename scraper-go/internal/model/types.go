// Package model holds the data structures the scraper parses and persists.
//
// These mirror the SQLite schema in docs/SCHEMA_CONTRACT.md exactly. NULLABLE
// numeric columns use sql.Null* so a genuine NULL (missing data, e.g. a "--"
// placeholder) is preserved distinctly from a real 0. The columns that the
// contract declares NOT NULL DEFAULT 0 / 0.0 (every round_stats stat triple,
// the win/loss counters, etc.) are plain int / float64.
package model

import "database/sql"

// Fighter is one row of the `fighters` table: the feature matrix for archetype
// clustering. The eight career-average fields are inlined (the old Django
// CareerStats has been merged in here).
type Fighter struct {
	Name        string         // NOT NULL UNIQUE
	Nickname    sql.NullString // nullable TEXT
	Nationality string         // defaults to "Unlisted"

	// Physical attributes — nullable numerics (placeholders => NULL).
	HeightIn  sql.NullInt64  // height_in (inches)
	WeightLbs sql.NullInt64  // weight_lbs (lbs)
	ReachIn   sql.NullInt64  // reach_in (inches)
	Stance    sql.NullString // nullable TEXT
	DOB       sql.NullString // date_of_birth, ISO YYYY-MM-DD

	// Record — NOT NULL DEFAULT 0 in the contract.
	Wins       int
	Losses     int
	Draws      int
	NoContests int

	// Champion status — set when persisting a title bout with a winner.
	WasChampion          int // 0/1
	ChampionshipBoutsWon int

	// Career averages. *_acc / *_def are stored as 0..1 fractions. All eight
	// are nullable so a missing stat stays NULL rather than collapsing to 0.
	SLpM   sql.NullFloat64 // strikes landed per minute
	StrAcc sql.NullFloat64 // striking accuracy (0..1)
	SApM   sql.NullFloat64 // strikes absorbed per minute
	StrDef sql.NullFloat64 // striking defense (0..1)
	TDAvg  sql.NullFloat64 // takedowns avg / 15 min
	TDAcc  sql.NullFloat64 // takedown accuracy (0..1)
	TDDef  sql.NullFloat64 // takedown defense (0..1)
	SubAvg sql.NullFloat64 // submission attempts avg / 15 min
}

// Event is one row of the `events` table.
type Event struct {
	Title    string // NOT NULL UNIQUE
	Date     string // ISO YYYY-MM-DD
	Location string

	// Fights belonging to this event. Persisted with the event in one
	// transaction by store.SaveEvent.
	Fights []Fight
}

// Fight is one row of the `fights` table, carrying its two competitors and
// each competitor's per-round stats so the whole bout can be written together.
type Fight struct {
	EventName   string
	Date        string // ISO YYYY-MM-DD
	WinnerName  string // may be "" for a draw / no-contest
	LoserName   string
	WeightClass string
	TitleBout   int // 0/1
	Method      string
	RoundEnded  int
	TimeEnded   int // seconds
	Referee     string

	// Competitor1 == winner corner, Competitor2 == loser corner (matches the
	// fighter_1 / fighter_2 convention in the Python parser). Each carries its
	// own per-round RoundStat rows.
	Competitor1 Competitor
	Competitor2 Competitor
}

// Competitor groups one fighter's name with their per-round stat rows for a
// single fight.
type Competitor struct {
	FighterName string
	Rounds      []RoundStat
}

// RoundStat is one WIDE row of the `round_stats` table: one row per
// (fight × fighter × round). Every field maps 1:1 onto a column. All stat
// triples are NOT NULL DEFAULT 0 / 0.0 per the contract, so they are plain
// numeric types (a missing strike triple is 0/0/0.0, never NULL).
type RoundStat struct {
	FighterName string
	Result      string // 'w' / 'l' / 'd'
	RoundNumber int

	Knockdowns  int
	SubAttempts int
	Reversals   int
	ControlTime int // seconds

	// Takedowns (the old RoundStats.strike_stats FK).
	TDLanded    int
	TDAttempted int
	TDPct       float64

	// Significant strikes.
	SigStrLanded    int
	SigStrAttempted int
	SigStrPct       float64

	// Total strikes.
	TotalStrLanded    int
	TotalStrAttempted int
	TotalStrPct       float64

	// By target.
	HeadLanded    int
	HeadAttempted int
	HeadPct       float64
	BodyLanded    int
	BodyAttempted int
	BodyPct       float64
	LegLanded     int
	LegAttempted  int
	LegPct        float64

	// By position.
	DistanceLanded    int
	DistanceAttempted int
	DistancePct       float64
	ClinchLanded      int
	ClinchAttempted   int
	ClinchPct         float64
	GroundLanded      int
	GroundAttempted   int
	GroundPct         float64
}
