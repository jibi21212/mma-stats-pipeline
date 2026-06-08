package parse

import (
	"strings"
	"testing"

	"github.com/PuerkitoBio/goquery"
)

func mustDoc(t *testing.T, html string) *goquery.Document {
	t.Helper()
	doc, err := goquery.NewDocumentFromReader(strings.NewReader(html))
	if err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	return doc
}

// --- parseTableRows -----------------------------------------------------------

const tableRowsHTML = `
<table><tbody>
  <tr>
    <td><p>Fighter One</p><p>Fighter Two</p></td>
    <td><p>1</p><p>0</p></td>
  </tr>
  <tr>
    <td><p>Fighter One</p><p>Fighter Two</p></td>
    <td><p>2</p><p>3</p></td>
  </tr>
</tbody></table>`

func TestParseTableRows_SplitsPerRoundAndFighter(t *testing.T) {
	rows := mustDoc(t, tableRowsHTML).Find("tbody").Find("tr")
	statMap := map[int]string{0: "name", 1: "kd"}
	f1, f2 := parseTableRows(rows, statMap)

	if len(f1) != 2 || len(f2) != 2 {
		t.Fatalf("expected 2 rounds each, got f1=%d f2=%d", len(f1), len(f2))
	}
	if f1[1]["name"] != "Fighter One" || f1[1]["kd"] != "1" {
		t.Errorf("f1[1] = %v, want {name:Fighter One, kd:1}", f1[1])
	}
	if f2[1]["name"] != "Fighter Two" || f2[1]["kd"] != "0" {
		t.Errorf("f2[1] = %v, want {name:Fighter Two, kd:0}", f2[1])
	}
	if f1[2]["kd"] != "2" || f2[2]["kd"] != "3" {
		t.Errorf("round 2 kd: f1=%q f2=%q, want 2 and 3", f1[2]["kd"], f2[2]["kd"])
	}
}

func TestParseTableRows_UnmappedColumnsIgnored(t *testing.T) {
	rows := mustDoc(t, tableRowsHTML).Find("tbody").Find("tr")
	f1, _ := parseTableRows(rows, map[int]string{0: "name"})
	if f1[1]["name"] != "Fighter One" {
		t.Errorf("f1[1][name] = %q, want Fighter One", f1[1]["name"])
	}
	if _, ok := f1[1]["kd"]; ok {
		t.Errorf("column 1 should have been ignored, got kd=%q", f1[1]["kd"])
	}
}

func TestParseTableRows_SinglePCellSkipped(t *testing.T) {
	rows := mustDoc(t, `<table><tbody><tr><td><p>Solo</p></td></tr></tbody></table>`).
		Find("tbody").Find("tr")
	f1, f2 := parseTableRows(rows, map[int]string{0: "name"})
	if len(f1[1]) != 0 || len(f2[1]) != 0 {
		t.Errorf("single <p> cell should contribute nothing, got f1=%v f2=%v", f1[1], f2[1])
	}
}

// --- buildRoundStat -----------------------------------------------------------

func TestBuildRoundStat_FullRound(t *testing.T) {
	totals := map[string]string{
		"name": "Fighter One", "kd": "1",
		"sig_str": "30 of 50", "total_str": "40 of 70",
		"td": "2 of 4", "sub_att": "1", "rev": "0", "ctrl": "1:30",
	}
	sig := map[string]string{
		"head": "20 of 35", "body": "5 of 8", "leg": "5 of 7",
		"distance": "25 of 40", "clinch": "3 of 5", "ground": "2 of 5",
	}
	rs := buildRoundStat(totals, sig, 1, "Fighter One", "w")

	if rs.RoundNumber != 1 || rs.FighterName != "Fighter One" {
		t.Errorf("round/name = %d/%q, want 1/Fighter One", rs.RoundNumber, rs.FighterName)
	}
	if rs.Knockdowns != 1 || rs.SubAttempts != 1 || rs.Reversals != 0 {
		t.Errorf("kd/sub/rev = %d/%d/%d, want 1/1/0", rs.Knockdowns, rs.SubAttempts, rs.Reversals)
	}
	if rs.ControlTime != 90 {
		t.Errorf("ControlTime = %d, want 90", rs.ControlTime)
	}
	if rs.TDLanded != 2 || rs.TDAttempted != 4 || !almostEqual(rs.TDPct, 0.5) {
		t.Errorf("takedowns = %d/%d/%v, want 2/4/0.5", rs.TDLanded, rs.TDAttempted, rs.TDPct)
	}
	if rs.SigStrLanded != 30 || rs.SigStrAttempted != 50 || !almostEqual(rs.SigStrPct, 0.6) {
		t.Errorf("sig = %d/%d/%v, want 30/50/0.6", rs.SigStrLanded, rs.SigStrAttempted, rs.SigStrPct)
	}
	if rs.TotalStrLanded != 40 || !almostEqual(rs.TotalStrPct, 40.0/70.0) {
		t.Errorf("total = %d/%v, want 40/%v", rs.TotalStrLanded, rs.TotalStrPct, 40.0/70.0)
	}
	if rs.HeadLanded != 20 || rs.HeadAttempted != 35 || !almostEqual(rs.HeadPct, 20.0/35.0) {
		t.Errorf("head = %d/%d/%v", rs.HeadLanded, rs.HeadAttempted, rs.HeadPct)
	}
	if rs.BodyLanded != 5 || rs.LegLanded != 5 || rs.DistanceLanded != 25 || rs.ClinchLanded != 3 || rs.GroundLanded != 2 {
		t.Errorf("breakdown landed mismatch: body=%d leg=%d dist=%d clinch=%d ground=%d",
			rs.BodyLanded, rs.LegLanded, rs.DistanceLanded, rs.ClinchLanded, rs.GroundLanded)
	}
}

func TestBuildRoundStat_PlaceholdersBecomeZero(t *testing.T) {
	rs := buildRoundStat(map[string]string{}, map[string]string{}, 2, "Nobody", "l")
	if rs.RoundNumber != 2 || rs.FighterName != "Nobody" {
		t.Errorf("round/name = %d/%q, want 2/Nobody", rs.RoundNumber, rs.FighterName)
	}
	if rs.Knockdowns != 0 || rs.SubAttempts != 0 || rs.Reversals != 0 || rs.ControlTime != 0 {
		t.Errorf("scalars should be zero, got kd=%d sub=%d rev=%d ctrl=%d",
			rs.Knockdowns, rs.SubAttempts, rs.Reversals, rs.ControlTime)
	}
	if rs.TDAttempted != 0 || rs.SigStrAttempted != 0 || rs.HeadAttempted != 0 || rs.GroundAttempted != 0 {
		t.Errorf("all attempted counts should be zero")
	}
	if rs.TDPct != 0.0 || rs.SigStrPct != 0.0 {
		t.Errorf("all pct should be 0.0")
	}
}

func TestBuildRoundStat_SigMergesOverTotals(t *testing.T) {
	totals := map[string]string{"sig_str": "10 of 20", "kd": "0"}
	sig := map[string]string{"sig_str": "11 of 22", "head": "5 of 9"}
	rs := buildRoundStat(totals, sig, 1, "X", "d")
	// sig_str from the sig dict overwrites the totals value.
	if rs.SigStrLanded != 11 || rs.SigStrAttempted != 22 || !almostEqual(rs.SigStrPct, 0.5) {
		t.Errorf("sig = %d/%d/%v, want 11/22/0.5", rs.SigStrLanded, rs.SigStrAttempted, rs.SigStrPct)
	}
}

// --- ParseFightPage -----------------------------------------------------------

const fightHTML = `
<html><body>
  <section class="b-statistics__section_details">
    <h2 class="b-content__title">UFC 285</h2>
    <div class="b-fight-details">

      <div class="b-fight-details__persons clearfix">
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status b-fight-details__person-status_style_green">W</i>
          <h3 class="b-fight-details__person-name"><a href="#">Jon Jones</a></h3>
        </div>
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status b-fight-details__person-status_style_gray">L</i>
          <h3 class="b-fight-details__person-name"><a href="#">Ciryl Gane</a></h3>
        </div>
      </div>

      <div class="b-fight-details__fight">
        <i class="b-fight-details__fight-title">UFC Heavyweight Title Bout</i>
      </div>

      <div class="b-fight-details__content">
        <p class="b-fight-details__text">
          <i class="b-fight-details__text-item_first">Method: Submission</i>
          <i class="b-fight-details__text-item">Round: 1</i>
          <i class="b-fight-details__text-item">Time: 2:04</i>
          <i class="b-fight-details__text-item">Time format: 5 Rnd (5-5-5-5-5)</i>
          <i class="b-fight-details__text-item">Referee: Marc Goddard</i>
        </p>
      </div>

      <table class="b-fight-details__table js-fight-table">
        <tbody>
          <tr>
            <td><p>Jon Jones</p><p>Ciryl Gane</p></td>
            <td><p>1</p><p>0</p></td>
            <td><p>10 of 20</p><p>5 of 15</p></td>
            <td><p>50%</p><p>33%</p></td>
            <td><p>15 of 25</p><p>7 of 18</p></td>
            <td><p>1 of 2</p><p>0 of 0</p></td>
            <td><p>50%</p><p>0%</p></td>
            <td><p>1</p><p>0</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>1:24</p><p>0:00</p></td>
          </tr>
        </tbody>
      </table>

      <section class="b-fight-details__section js-fight-section">s0</section>
      <section class="b-fight-details__section js-fight-section">s1</section>
      <section class="b-fight-details__section js-fight-section">s2</section>
      <section class="b-fight-details__section js-fight-section">s3</section>
      <section class="b-fight-details__section js-fight-section">
        <table class="b-fight-details__table js-fight-table">
          <tbody>
            <tr>
              <td><p>Jon Jones</p><p>Ciryl Gane</p></td>
              <td><p>10 of 20</p><p>5 of 15</p></td>
              <td><p>50%</p><p>33%</p></td>
              <td><p>6 of 10</p><p>2 of 8</p></td>
              <td><p>2 of 5</p><p>2 of 4</p></td>
              <td><p>2 of 5</p><p>1 of 3</p></td>
              <td><p>5 of 12</p><p>3 of 10</p></td>
              <td><p>2 of 3</p><p>1 of 2</p></td>
              <td><p>3 of 5</p><p>1 of 3</p></td>
            </tr>
          </tbody>
        </table>
      </section>

    </div>
  </section>
</body></html>`

func TestParseFightPage_HappyPath(t *testing.T) {
	fight, ok := ParseFightPage(mustDoc(t, fightHTML), "2023-03-04")
	if !ok {
		t.Fatal("fight page should parse, not fall through to false")
	}
	if fight.EventName != "UFC 285" {
		t.Errorf("EventName = %q, want UFC 285", fight.EventName)
	}
	if fight.Date != "2023-03-04" {
		t.Errorf("Date = %q, want 2023-03-04", fight.Date)
	}
	if fight.WinnerName != "Jon Jones" || fight.LoserName != "Ciryl Gane" {
		t.Errorf("winner/loser = %q/%q, want Jon Jones/Ciryl Gane", fight.WinnerName, fight.LoserName)
	}
	if fight.WeightClass != "UFC Heavyweight Title Bout" {
		t.Errorf("WeightClass = %q", fight.WeightClass)
	}
	if fight.TitleBout != 1 {
		t.Errorf("TitleBout = %d, want 1", fight.TitleBout)
	}
	if fight.Method != "Submission" {
		t.Errorf("Method = %q, want Submission", fight.Method)
	}
	if fight.RoundEnded != 1 {
		t.Errorf("RoundEnded = %d, want 1", fight.RoundEnded)
	}
	if fight.TimeEnded != 124 {
		t.Errorf("TimeEnded = %d, want 124", fight.TimeEnded)
	}
	if fight.Referee != "Marc Goddard" {
		t.Errorf("Referee = %q, want Marc Goddard", fight.Referee)
	}
	if fight.Competitor1.FighterName != "Jon Jones" || fight.Competitor2.FighterName != "Ciryl Gane" {
		t.Errorf("competitors = %q/%q", fight.Competitor1.FighterName, fight.Competitor2.FighterName)
	}

	// One round each.
	if len(fight.Competitor1.Rounds) != 1 || len(fight.Competitor2.Rounds) != 1 {
		t.Fatalf("expected 1 round each, got c1=%d c2=%d",
			len(fight.Competitor1.Rounds), len(fight.Competitor2.Rounds))
	}
	r1 := fight.Competitor1.Rounds[0]
	if r1.RoundNumber != 1 || r1.FighterName != "Jon Jones" {
		t.Errorf("c1 round = %d/%q", r1.RoundNumber, r1.FighterName)
	}
	if r1.Result != "w" {
		t.Errorf("c1 result = %q, want w", r1.Result)
	}
	if r1.Knockdowns != 1 || r1.SubAttempts != 1 {
		t.Errorf("c1 kd/sub = %d/%d, want 1/1", r1.Knockdowns, r1.SubAttempts)
	}
	if r1.ControlTime != 84 {
		t.Errorf("c1 ControlTime = %d, want 84", r1.ControlTime)
	}
	if r1.TDLanded != 1 || r1.TDAttempted != 2 || !almostEqual(r1.TDPct, 0.5) {
		t.Errorf("c1 td = %d/%d/%v, want 1/2/0.5", r1.TDLanded, r1.TDAttempted, r1.TDPct)
	}
	if r1.SigStrLanded != 10 || r1.SigStrAttempted != 20 || !almostEqual(r1.SigStrPct, 0.5) {
		t.Errorf("c1 sig = %d/%d/%v, want 10/20/0.5", r1.SigStrLanded, r1.SigStrAttempted, r1.SigStrPct)
	}
	// Head strikes come from the dedicated sig-strike section (index 4).
	if r1.HeadLanded != 6 || r1.HeadAttempted != 10 || !almostEqual(r1.HeadPct, 0.6) {
		t.Errorf("c1 head = %d/%d/%v, want 6/10/0.6", r1.HeadLanded, r1.HeadAttempted, r1.HeadPct)
	}

	r2 := fight.Competitor2.Rounds[0]
	if r2.FighterName != "Ciryl Gane" || r2.Result != "l" {
		t.Errorf("c2 name/result = %q/%q, want Ciryl Gane/l", r2.FighterName, r2.Result)
	}
	if r2.Knockdowns != 0 {
		t.Errorf("c2 kd = %d, want 0", r2.Knockdowns)
	}
	if r2.TDLanded != 0 || r2.TDAttempted != 0 || r2.TDPct != 0.0 {
		t.Errorf("c2 td = %d/%d/%v, want 0/0/0.0", r2.TDLanded, r2.TDAttempted, r2.TDPct)
	}
}

func TestParseFightPage_NonTitleBout(t *testing.T) {
	html := strings.Replace(fightHTML, "UFC Heavyweight Title Bout", "Lightweight Bout", 1)
	fight, ok := ParseFightPage(mustDoc(t, html), "2023-01-01")
	if !ok {
		t.Fatal("expected parse to succeed")
	}
	if fight.TitleBout != 0 {
		t.Errorf("TitleBout = %d, want 0", fight.TitleBout)
	}
	if fight.WeightClass != "Lightweight Bout" {
		t.Errorf("WeightClass = %q, want Lightweight Bout", fight.WeightClass)
	}
}

// drawFightHTML is the fight fixture with both status markers showing "D"
// (draw / no-contest): neither corner is detected as W or L, so the parser must
// fall back to reading both names in document order and assign both 'd'.
const drawFightHTML = `
<html><body>
  <section class="b-statistics__section_details">
    <h2 class="b-content__title">UFC 285</h2>
    <div class="b-fight-details">
      <div class="b-fight-details__persons clearfix">
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status b-fight-details__person-status_style_gray">D</i>
          <h3 class="b-fight-details__person-name"><a href="#">Jon Jones</a></h3>
        </div>
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status b-fight-details__person-status_style_gray">D</i>
          <h3 class="b-fight-details__person-name"><a href="#">Ciryl Gane</a></h3>
        </div>
      </div>
      <div class="b-fight-details__fight">
        <i class="b-fight-details__fight-title">Lightweight Bout</i>
      </div>
      <div class="b-fight-details__content">
        <p class="b-fight-details__text">
          <i class="b-fight-details__text-item_first">Method: Decision - Draw</i>
          <i class="b-fight-details__text-item">Round: 3</i>
          <i class="b-fight-details__text-item">Time: 5:00</i>
          <i class="b-fight-details__text-item">Time format: 3 Rnd (5-5-5)</i>
          <i class="b-fight-details__text-item">Referee: Herb Dean</i>
        </p>
      </div>
      <table class="b-fight-details__table js-fight-table">
        <tbody>
          <tr>
            <td><p>Jon Jones</p><p>Ciryl Gane</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>5 of 10</p><p>5 of 10</p></td>
            <td><p>50%</p><p>50%</p></td>
            <td><p>5 of 10</p><p>5 of 10</p></td>
            <td><p>0 of 0</p><p>0 of 0</p></td>
            <td><p>0%</p><p>0%</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>0:00</p><p>0:00</p></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</body></html>`

func TestParseFightPage_DrawFallback(t *testing.T) {
	// Faithful 1:1 port of parse_fight_page + save_event: when neither corner is
	// flagged W/L but BOTH names are present, the fallback fills
	// winner = names[0], loser = names[1]. save_event then assigns results from
	// that filled winner_name -> w for corner 1, l for corner 2. The 'd'/'d'
	// path only fires when winner_name stays empty (see the no-names test below).
	fight, ok := ParseFightPage(mustDoc(t, drawFightHTML), "2023-03-04")
	if !ok {
		t.Fatal("expected parse to succeed")
	}
	if fight.WinnerName != "Jon Jones" || fight.LoserName != "Ciryl Gane" {
		t.Errorf("fallback names = %q/%q, want Jon Jones/Ciryl Gane (document order)",
			fight.WinnerName, fight.LoserName)
	}
	if fight.Competitor1.Rounds[0].Result != "w" || fight.Competitor2.Rounds[0].Result != "l" {
		t.Errorf("results = %q/%q, want w/l (fallback fills winner from names[0])",
			fight.Competitor1.Rounds[0].Result, fight.Competitor2.Rounds[0].Result)
	}
}

// noNamesFightHTML has status markers of "D" and NO <a> inside the person-name
// headers, so neither a W/L corner nor the name fallback can fill a winner.
// winner_name stays empty -> both corners get result 'd' (the genuine draw/NC
// path).
const noNamesFightHTML = `
<html><body>
  <section class="b-statistics__section_details">
    <h2 class="b-content__title">UFC 285</h2>
    <div class="b-fight-details">
      <div class="b-fight-details__persons clearfix">
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status b-fight-details__person-status_style_gray">D</i>
          <h3 class="b-fight-details__person-name"></h3>
        </div>
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status b-fight-details__person-status_style_gray">D</i>
          <h3 class="b-fight-details__person-name"></h3>
        </div>
      </div>
      <div class="b-fight-details__fight">
        <i class="b-fight-details__fight-title">Lightweight Bout</i>
      </div>
      <div class="b-fight-details__content">
        <p class="b-fight-details__text">
          <i class="b-fight-details__text-item_first">Method: Decision - Draw</i>
          <i class="b-fight-details__text-item">Round: 3</i>
          <i class="b-fight-details__text-item">Time: 5:00</i>
          <i class="b-fight-details__text-item">Time format: 3 Rnd (5-5-5)</i>
          <i class="b-fight-details__text-item">Referee: Herb Dean</i>
        </p>
      </div>
      <table class="b-fight-details__table js-fight-table">
        <tbody>
          <tr>
            <td><p></p><p></p></td>
            <td><p>0</p><p>0</p></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</body></html>`

func TestParseFightPage_NoWinnerYieldsDraw(t *testing.T) {
	fight, ok := ParseFightPage(mustDoc(t, noNamesFightHTML), "2023-03-04")
	if !ok {
		t.Fatal("expected parse to succeed")
	}
	if fight.WinnerName != "" || fight.LoserName != "" {
		t.Errorf("winner/loser = %q/%q, want empty (no names to fall back on)",
			fight.WinnerName, fight.LoserName)
	}
	if fight.Competitor1.Rounds[0].Result != "d" || fight.Competitor2.Rounds[0].Result != "d" {
		t.Errorf("results = %q/%q, want d/d (no winner)",
			fight.Competitor1.Rounds[0].Result, fight.Competitor2.Rounds[0].Result)
	}
}

// multiTbodyFightHTML reproduces the LIVE ufcstats.com structure where each
// round's row is wrapped in its OWN <tbody>, preceded by an empty leading
// <tbody> — for both the totals table and the significant-strikes section. The
// earlier parser used table.Find("tbody").First().Find("tr"), which grabbed the
// empty leading <tbody> and produced 0 rounds (so 0 round_stats were written
// despite fights being saved). This fixture guards the fix that collects rows
// across ALL tbodies.
const multiTbodyFightHTML = `
<html><body>
  <section class="b-statistics__section_details">
    <h2 class="b-content__title">UFC 300</h2>
    <div class="b-fight-details">
      <div class="b-fight-details__persons clearfix">
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status">W</i>
          <h3 class="b-fight-details__person-name"><a href="#">Alpha</a></h3>
        </div>
        <div class="b-fight-details__person">
          <i class="b-fight-details__person-status">L</i>
          <h3 class="b-fight-details__person-name"><a href="#">Beta</a></h3>
        </div>
      </div>
      <div class="b-fight-details__fight">
        <i class="b-fight-details__fight-title">Welterweight Bout</i>
      </div>
      <div class="b-fight-details__content">
        <p class="b-fight-details__text">
          <i class="b-fight-details__text-item_first">Method: Decision - Unanimous</i>
          <i class="b-fight-details__text-item">Round: 2</i>
          <i class="b-fight-details__text-item">Time: 5:00</i>
          <i class="b-fight-details__text-item">Time format: 3 Rnd (5-5-5)</i>
          <i class="b-fight-details__text-item">Referee: Herb Dean</i>
        </p>
      </div>
      <table class="b-fight-details__table js-fight-table">
        <tbody></tbody>
        <tbody>
          <tr>
            <td><p>Alpha</p><p>Beta</p></td>
            <td><p>1</p><p>0</p></td>
            <td><p>10 of 20</p><p>4 of 16</p></td>
            <td><p>50%</p><p>25%</p></td>
            <td><p>12 of 24</p><p>6 of 20</p></td>
            <td><p>1 of 2</p><p>0 of 1</p></td>
            <td><p>50%</p><p>0%</p></td>
            <td><p>0</p><p>1</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>1:00</p><p>0:30</p></td>
          </tr>
        </tbody>
        <tbody>
          <tr>
            <td><p>Alpha</p><p>Beta</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>20 of 30</p><p>8 of 18</p></td>
            <td><p>66%</p><p>44%</p></td>
            <td><p>22 of 34</p><p>10 of 22</p></td>
            <td><p>2 of 3</p><p>0 of 0</p></td>
            <td><p>66%</p><p>0%</p></td>
            <td><p>1</p><p>0</p></td>
            <td><p>0</p><p>0</p></td>
            <td><p>2:00</p><p>0:10</p></td>
          </tr>
        </tbody>
      </table>
      <section class="b-fight-details__section js-fight-section">s0</section>
      <section class="b-fight-details__section js-fight-section">s1</section>
      <section class="b-fight-details__section js-fight-section">s2</section>
      <section class="b-fight-details__section js-fight-section">s3</section>
      <section class="b-fight-details__section js-fight-section">
        <table class="b-fight-details__table js-fight-table">
          <tbody></tbody>
          <tbody>
            <tr>
              <td><p>Alpha</p><p>Beta</p></td>
              <td><p>10 of 20</p><p>4 of 16</p></td>
              <td><p>50%</p><p>25%</p></td>
              <td><p>6 of 12</p><p>2 of 8</p></td>
              <td><p>2 of 4</p><p>1 of 4</p></td>
              <td><p>2 of 4</p><p>1 of 4</p></td>
              <td><p>5 of 10</p><p>2 of 9</p></td>
              <td><p>3 of 6</p><p>1 of 4</p></td>
              <td><p>2 of 4</p><p>1 of 3</p></td>
            </tr>
          </tbody>
          <tbody>
            <tr>
              <td><p>Alpha</p><p>Beta</p></td>
              <td><p>20 of 30</p><p>8 of 18</p></td>
              <td><p>66%</p><p>44%</p></td>
              <td><p>12 of 18</p><p>4 of 9</p></td>
              <td><p>4 of 7</p><p>2 of 5</p></td>
              <td><p>4 of 5</p><p>2 of 4</p></td>
              <td><p>10 of 16</p><p>4 of 11</p></td>
              <td><p>6 of 9</p><p>2 of 5</p></td>
              <td><p>4 of 5</p><p>2 of 2</p></td>
            </tr>
          </tbody>
        </table>
      </section>
    </div>
  </section>
</body></html>`

func TestParseFightPage_MultiTbodyPerRound(t *testing.T) {
	fight, ok := ParseFightPage(mustDoc(t, multiTbodyFightHTML), "2024-04-13")
	if !ok {
		t.Fatal("expected parse to succeed")
	}
	// Two rounds, collected across the per-round <tbody> elements (the empty
	// leading <tbody> must be skipped, not chosen by .First()).
	if len(fight.Competitor1.Rounds) != 2 || len(fight.Competitor2.Rounds) != 2 {
		t.Fatalf("expected 2 rounds each, got c1=%d c2=%d",
			len(fight.Competitor1.Rounds), len(fight.Competitor2.Rounds))
	}
	r1 := fight.Competitor1.Rounds[0]
	r2 := fight.Competitor1.Rounds[1]
	if r1.RoundNumber != 1 || r2.RoundNumber != 2 {
		t.Errorf("round numbers = %d,%d, want 1,2", r1.RoundNumber, r2.RoundNumber)
	}
	if r1.SigStrLanded != 10 || r1.SigStrAttempted != 20 {
		t.Errorf("r1 sig = %d/%d, want 10/20", r1.SigStrLanded, r1.SigStrAttempted)
	}
	if r2.SigStrLanded != 20 || r2.SigStrAttempted != 30 {
		t.Errorf("r2 sig = %d/%d, want 20/30", r2.SigStrLanded, r2.SigStrAttempted)
	}
	if r1.Knockdowns != 1 || r2.Knockdowns != 0 {
		t.Errorf("kd r1/r2 = %d/%d, want 1/0", r1.Knockdowns, r2.Knockdowns)
	}
	if r1.ControlTime != 60 || r2.ControlTime != 120 {
		t.Errorf("ctrl r1/r2 = %d/%d, want 60/120", r1.ControlTime, r2.ControlTime)
	}
	// Head strikes come from the per-round sig section (also multi-<tbody>).
	if r1.HeadLanded != 6 || r2.HeadLanded != 12 {
		t.Errorf("head r1/r2 landed = %d/%d, want 6/12", r1.HeadLanded, r2.HeadLanded)
	}
	// Competitor 2, round 2 sanity (loser corner, second <p> in each cell).
	c2r2 := fight.Competitor2.Rounds[1]
	if c2r2.SigStrLanded != 8 || c2r2.Result != "l" {
		t.Errorf("c2 r2 sig/result = %d/%q, want 8/l", c2r2.SigStrLanded, c2r2.Result)
	}
}
