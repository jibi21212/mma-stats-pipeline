package parse

import (
	"strconv"
	"strings"

	"github.com/PuerkitoBio/goquery"

	"ufcscraper/internal/model"
)

// totalsStatMap ports TOTALS_STAT_MAP: column index -> stat key in the main
// per-round "Totals" table.
var totalsStatMap = map[int]string{
	0: "name", 1: "kd", 2: "sig_str", 3: "sig_str_pct",
	4: "total_str", 5: "td", 6: "td_pct", 7: "sub_att",
	8: "rev", 9: "ctrl",
}

// sigStrikeMap ports SIG_STRIKE_MAP: column index -> stat key in the
// significant-strikes breakdown table (the 5th js-fight-section).
var sigStrikeMap = map[int]string{
	0: "name", 1: "sig_str", 2: "sig_str_pct",
	3: "head", 4: "body", 5: "leg",
	6: "distance", 7: "clinch", 8: "ground",
}

// parseTableRows ports _parse_table_rows. Each <tr> is one round; within a row,
// each mapped <td> holds two <p> tags (the first = fighter1/winner corner, the
// second = fighter2/loser corner). Returns per-round maps keyed by round number
// (1-based), each value a stat-key -> raw-string map. A <td> with fewer than 2
// <p> contributes nothing for that stat (matching the len(ps) >= 2 guard).
func parseTableRows(rows *goquery.Selection, statMap map[int]string) (map[int]map[string]string, map[int]map[string]string) {
	f1 := make(map[int]map[string]string)
	f2 := make(map[int]map[string]string)

	rows.Each(func(j int, tr *goquery.Selection) {
		roundNum := j + 1
		f1Round := make(map[string]string)
		f2Round := make(map[string]string)

		tr.Find("td").Each(func(i int, td *goquery.Selection) {
			statName, ok := statMap[i]
			if !ok {
				return
			}
			ps := td.Find("p")
			if ps.Length() >= 2 {
				f1Round[statName] = strings.TrimSpace(ps.Eq(0).Text())
				f2Round[statName] = strings.TrimSpace(ps.Eq(1).Text())
			}
		})

		f1[roundNum] = f1Round
		f2[roundNum] = f2Round
	})

	return f1, f2
}

// getOr returns m[key] or the fallback when the key is absent — the analogue of
// Python's dict.get(key, default).
func getOr(m map[string]string, key, fallback string) string {
	if v, ok := m[key]; ok {
		return v
	}
	return fallback
}

// intStatOrZero ports the `int(combined.get(k, 0)) if combined.get(k,'---') != '---' else 0`
// idiom: an absent key or the "---" placeholder yields 0, otherwise the parsed
// int (0 on any parse error, matching int() raising being avoided by upstream
// data shape).
func intStatOrZero(m map[string]string, key string) int {
	v := getOr(m, key, "---")
	if v == "---" {
		return 0
	}
	n, err := strconv.Atoi(strings.TrimSpace(v))
	if err != nil {
		return 0
	}
	return n
}

// buildRoundStat ports _build_round_stats: merge the sig-strike row over the
// totals row (shared keys overwritten by sig), then map every value through the
// strike/time/int helpers into a flat model.RoundStat.
func buildRoundStat(totals, sig map[string]string, roundNum int, fighterName, result string) model.RoundStat {
	// combined = {**totals}; if sig: combined.update(sig)
	combined := make(map[string]string, len(totals)+len(sig))
	for k, v := range totals {
		combined[k] = v
	}
	for k, v := range sig {
		combined[k] = v
	}

	td := ParseStrikeData(getOr(combined, "td", "---"))
	sigStr := ParseStrikeData(getOr(combined, "sig_str", "---"))
	totalStr := ParseStrikeData(getOr(combined, "total_str", "---"))
	head := ParseStrikeData(getOr(combined, "head", "---"))
	body := ParseStrikeData(getOr(combined, "body", "---"))
	leg := ParseStrikeData(getOr(combined, "leg", "---"))
	distance := ParseStrikeData(getOr(combined, "distance", "---"))
	clinch := ParseStrikeData(getOr(combined, "clinch", "---"))
	ground := ParseStrikeData(getOr(combined, "ground", "---"))

	return model.RoundStat{
		FighterName: fighterName,
		Result:      result,
		RoundNumber: roundNum,

		Knockdowns:  intStatOrZero(combined, "kd"),
		SubAttempts: intStatOrZero(combined, "sub_att"),
		Reversals:   intStatOrZero(combined, "rev"),
		ControlTime: ConvertTimeToSeconds(getOr(combined, "ctrl", "0:00")),

		TDLanded:    td.Landed,
		TDAttempted: td.Attempted,
		TDPct:       td.Percentage,

		SigStrLanded:    sigStr.Landed,
		SigStrAttempted: sigStr.Attempted,
		SigStrPct:       sigStr.Percentage,

		TotalStrLanded:    totalStr.Landed,
		TotalStrAttempted: totalStr.Attempted,
		TotalStrPct:       totalStr.Percentage,

		HeadLanded:    head.Landed,
		HeadAttempted: head.Attempted,
		HeadPct:       head.Percentage,
		BodyLanded:    body.Landed,
		BodyAttempted: body.Attempted,
		BodyPct:       body.Percentage,
		LegLanded:     leg.Landed,
		LegAttempted:  leg.Attempted,
		LegPct:        leg.Percentage,

		DistanceLanded:    distance.Landed,
		DistanceAttempted: distance.Attempted,
		DistancePct:       distance.Percentage,
		ClinchLanded:      clinch.Landed,
		ClinchAttempted:   clinch.Attempted,
		ClinchPct:         clinch.Percentage,
		GroundLanded:      ground.Landed,
		GroundAttempted:   ground.Attempted,
		GroundPct:         ground.Percentage,
	}
}

// ParseFightPage ports parse_fight_page. eventDate (already ISO) is threaded
// onto the fight. Returns ok=false on any structural problem (the Python
// except-path returned None). The result ('w'/'l'/'d') is assigned here so the
// flat round_stats rows carry it, matching save_event's result logic.
func ParseFightPage(doc *goquery.Document, eventDate string) (model.Fight, bool) {
	var fight model.Fight

	section := doc.Find("section.b-statistics__section_details").First()
	if section.Length() == 0 {
		return fight, false
	}
	div := section.Find("div.b-fight-details").First()
	if div.Length() == 0 {
		return fight, false
	}

	// Winner / loser via the person status <i> text (no :has/:-soup-contains):
	// iterate the person divs, read each status i, match "W" / "L".
	personsDiv := div.Find("div.b-fight-details__persons").First()
	var winner, loser string
	var orderedNames []string
	personsDiv.Find("div.b-fight-details__person").Each(func(_ int, person *goquery.Selection) {
		name := strings.TrimSpace(person.Find("h3.b-fight-details__person-name a").First().Text())
		if name != "" {
			orderedNames = append(orderedNames, name)
		}
		status := strings.TrimSpace(person.Find("i.b-fight-details__person-status").First().Text())
		switch status {
		case "W":
			if winner == "" {
				winner = name
			}
		case "L":
			if loser == "" {
				loser = name
			}
		}
	})

	// Draw / NC fallback: if either corner is missing, fill from the names in
	// document order (winner or names[0]; loser or names[1]).
	if winner == "" || loser == "" {
		if len(orderedNames) >= 2 {
			if winner == "" {
				winner = orderedNames[0]
			}
			if loser == "" {
				loser = orderedNames[1]
			}
		}
	}

	// Weight class and title detection (the word "Title" in the title text).
	weightClass := strings.TrimSpace(
		div.Find("div.b-fight-details__fight i.b-fight-details__fight-title").First().Text(),
	)
	titleBout := 0
	if strings.Contains(weightClass, "Title") {
		titleBout = 1
	}

	// Fight metadata. The "_first" item carries the method; the remaining
	// text-items (round, time, time-format, referee) are positional. The class
	// selector ".b-fight-details__text-item" deliberately excludes the
	// "_first" item (a distinct class token), reproducing the Python find_all.
	contentDiv := div.Find("div.b-fight-details__content").First()
	firstItem := contentDiv.Find("i.b-fight-details__text-item_first").First()
	method := ""
	if firstItem.Length() > 0 {
		method = strings.TrimSpace(strings.ReplaceAll(strings.TrimSpace(firstItem.Text()), "Method:", ""))
	}

	textItems := contentDiv.Find("i.b-fight-details__text-item")
	rnd := "0"
	timeStr := "0:00"
	referee := ""
	if textItems.Length() > 0 {
		rnd = strings.TrimSpace(strings.ReplaceAll(strings.TrimSpace(textItems.Eq(0).Text()), "Round:", ""))
	}
	if textItems.Length() > 1 {
		timeStr = strings.TrimSpace(strings.ReplaceAll(strings.TrimSpace(textItems.Eq(1).Text()), "Time:", ""))
	}
	if textItems.Length() > 3 {
		referee = strings.TrimSpace(strings.ReplaceAll(strings.TrimSpace(textItems.Eq(3).Text()), "Referee:", ""))
	}

	eventName := strings.TrimSpace(section.Find("h2.b-content__title").First().Text())

	// Round-by-round totals table. NOTE: the live site wraps EACH round's row in
	// its own <tbody> (preceded by an empty leading <tbody>), so the per-round
	// rows must be collected across ALL tbodies — not just the first, which is
	// empty. (Using .First() here is the bug that produced 0 round_stats rows.)
	table := div.Find("table.b-fight-details__table.js-fight-table").First()
	rows := table.Find("tbody").Find("tr")
	f1Totals, f2Totals := parseTableRows(rows, totalsStatMap)

	// Significant-strike breakdown: the 5th js-fight-section (index 4), guarded
	// by len(sections) >= 5. Same multi-<tbody> structure as the totals table.
	sections := div.Find("section.b-fight-details__section.js-fight-section")
	f1Sig := map[int]map[string]string{}
	f2Sig := map[int]map[string]string{}
	if sections.Length() >= 5 {
		sigRows := sections.Eq(4).Find("tbody").Find("tr")
		if sigRows.Length() > 0 {
			f1Sig, f2Sig = parseTableRows(sigRows, sigStrikeMap)
		}
	}

	numRounds := rows.Length()
	f1Name := winner
	f2Name := loser

	// Result assignment mirrors save_event: 'w' for the winner-named corner,
	// 'l' for the other; both 'd' when there is no winner (draw / NC).
	f1Result := "l"
	f2Result := "l"
	if winner != "" && f1Name == winner {
		f1Result = "w"
	}
	if winner != "" && f2Name == winner {
		f2Result = "w"
	}
	if winner == "" {
		f1Result = "d"
		f2Result = "d"
	}

	f1Rounds := make([]model.RoundStat, 0, numRounds)
	f2Rounds := make([]model.RoundStat, 0, numRounds)
	for r := 1; r <= numRounds; r++ {
		f1Rounds = append(f1Rounds, buildRoundStat(f1Totals[r], f1Sig[r], r, f1Name, f1Result))
		f2Rounds = append(f2Rounds, buildRoundStat(f2Totals[r], f2Sig[r], r, f2Name, f2Result))
	}

	roundEnded := 0
	if rnd != "" && rnd != "---" {
		if n, err := strconv.Atoi(rnd); err == nil {
			roundEnded = n
		}
	}

	fight = model.Fight{
		EventName:   eventName,
		Date:        eventDate,
		WinnerName:  winner,
		LoserName:   loser,
		WeightClass: weightClass,
		TitleBout:   titleBout,
		Method:      method,
		RoundEnded:  roundEnded,
		TimeEnded:   ConvertTimeToSeconds(timeStr),
		Referee:     referee,
		Competitor1: model.Competitor{FighterName: f1Name, Rounds: f1Rounds},
		Competitor2: model.Competitor{FighterName: f2Name, Rounds: f2Rounds},
	}
	return fight, true
}
