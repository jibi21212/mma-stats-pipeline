// Package parse ports the Python scraper's parsing logic (parsers.py) 1:1.
//
// The helpers here reproduce parse_record, parse_percentage, parse_strike_data,
// convert_height, convert_time_to_seconds, parse_optional and extract_text with
// identical placeholder handling ("--", "---", "") so the Go output matches the
// numbers verified in the original test_parsers.py fixtures.
package parse

import (
	"database/sql"
	"regexp"
	"strconv"
	"strings"

	"github.com/PuerkitoBio/goquery"
)

// recordRe matches "29-7-0" with an optional " (1 NC)" suffix, mirroring the
// regex in parse_record.
var recordRe = regexp.MustCompile(`(\d+)-(\d+)-(\d+)(?: \((\d+) NC\))?`)

// Record holds a parsed fighter win/loss/draw/no-contest line.
type Record struct {
	Wins       int
	Losses     int
	Draws      int
	NoContests int
}

// StrikeData is the {landed, attempted, percentage} triple produced by
// parse_strike_data.
type StrikeData struct {
	Landed     int
	Attempted  int
	Percentage float64
}

// isPlaceholder reports whether a (trimmed) value is one of the "missing"
// markers the original parser treats as absent: "--", "---" or empty string.
func isPlaceholder(v string) bool {
	return v == "--" || v == "---" || v == ""
}

// ParseOptionalFloat ports parse_optional with the default float converter:
// returns (NULL) for the "--"/"---" placeholders, otherwise the parsed float.
// The value is stripped before the placeholder check, matching Python's
// value.strip().
func ParseOptionalFloat(value string) sql.NullFloat64 {
	value = strings.TrimSpace(value)
	if value == "--" || value == "---" {
		return sql.NullFloat64{}
	}
	f, err := strconv.ParseFloat(value, 64)
	if err != nil {
		return sql.NullFloat64{}
	}
	return sql.NullFloat64{Float64: f, Valid: true}
}

// ParseOptionalInt ports parse_optional with the int converter.
func ParseOptionalInt(value string) sql.NullInt64 {
	value = strings.TrimSpace(value)
	if value == "--" || value == "---" {
		return sql.NullInt64{}
	}
	n, err := strconv.Atoi(value)
	if err != nil {
		return sql.NullInt64{}
	}
	return sql.NullInt64{Int64: int64(n), Valid: true}
}

// ParsePercentage ports parse_percentage: "56%" -> 0.56, placeholders -> NULL.
// Division by 100 keeps single-digit and 100% correct (5% -> 0.05, 100% -> 1.0).
func ParsePercentage(value string) sql.NullFloat64 {
	value = strings.TrimSpace(value)
	if isPlaceholder(value) {
		return sql.NullFloat64{}
	}
	f, err := strconv.ParseFloat(strings.ReplaceAll(value, "%", ""), 64)
	if err != nil {
		return sql.NullFloat64{}
	}
	return sql.NullFloat64{Float64: f / 100, Valid: true}
}

// ConvertHeight ports convert_height: 5'10" -> 70 inches; returns NULL on
// anything it cannot parse. ufcstats renders heights like `5' 10"` with a space,
// so each part is trimmed before conversion (Python's int() tolerated the space;
// Go's strconv.Atoi does not, hence the explicit TrimSpace).
func ConvertHeight(heightStr string) sql.NullInt64 {
	cleaned := strings.ReplaceAll(heightStr, `"`, "")
	parts := strings.Split(cleaned, "'")
	if len(parts) != 2 {
		return sql.NullInt64{}
	}
	feet, err := strconv.Atoi(strings.TrimSpace(parts[0]))
	if err != nil {
		return sql.NullInt64{}
	}
	inches, err := strconv.Atoi(strings.TrimSpace(parts[1]))
	if err != nil {
		return sql.NullInt64{}
	}
	return sql.NullInt64{Int64: int64(feet*12 + inches), Valid: true}
}

// ConvertTimeToSeconds ports convert_time_to_seconds: "MM:SS" -> total seconds.
// Empty / placeholder input and any malformed value yield 0 (the contract
// stores control_time / time_ended as NOT NULL DEFAULT 0).
func ConvertTimeToSeconds(timeStr string) int {
	if timeStr == "" || timeStr == "--" || timeStr == "---" {
		return 0
	}
	parts := strings.Split(timeStr, ":")
	if len(parts) != 2 {
		return 0
	}
	mins, err := strconv.Atoi(strings.TrimSpace(parts[0]))
	if err != nil {
		return 0
	}
	secs, err := strconv.Atoi(strings.TrimSpace(parts[1]))
	if err != nil {
		return 0
	}
	return mins*60 + secs
}

// ParseRecord ports parse_record: "Record: 29-7-0 (1 NC)" -> components.
// Unparseable input returns all-zeros, matching the Python fallback.
func ParseRecord(recordStr string) Record {
	values := strings.ReplaceAll(recordStr, "Record: ", "")
	m := recordRe.FindStringSubmatch(values)
	if m == nil {
		return Record{}
	}
	rec := Record{
		Wins:   atoiOrZero(m[1]),
		Losses: atoiOrZero(m[2]),
		Draws:  atoiOrZero(m[3]),
	}
	if m[4] != "" {
		rec.NoContests = atoiOrZero(m[4])
	}
	return rec
}

// ParseStrikeData ports parse_strike_data: "50 of 100" -> {50, 100, 0.5}.
// Placeholders and any string without " of " return the zero triple
// {0, 0, 0.0} (a missing strike triple is 0/0/0.0, never NULL). Division guards
// attempted == 0 to avoid a divide-by-zero (-> 0.0).
func ParseStrikeData(strikeStr string) StrikeData {
	strikeStr = strings.TrimSpace(strikeStr)
	if isPlaceholder(strikeStr) {
		return StrikeData{}
	}
	if strings.Contains(strikeStr, " of ") {
		parts := strings.SplitN(strikeStr, " of ", 2)
		landed, err1 := strconv.Atoi(strings.TrimSpace(parts[0]))
		attempted, err2 := strconv.Atoi(strings.TrimSpace(parts[1]))
		if err1 != nil || err2 != nil {
			return StrikeData{}
		}
		pct := 0.0
		if attempted > 0 {
			pct = float64(landed) / float64(attempted)
		}
		return StrikeData{Landed: landed, Attempted: attempted, Percentage: pct}
	}
	return StrikeData{}
}

// ExtractText ports extract_text: read a selection's stripped text, drop the
// label prefix, and re-trim. A nil/empty selection yields the "--" sentinel,
// exactly like the Python `return ... if tag else '--'`.
func ExtractText(sel *goquery.Selection, label string) string {
	if sel == nil || sel.Length() == 0 {
		return "--"
	}
	text := strings.TrimSpace(sel.Text())
	return strings.TrimSpace(strings.ReplaceAll(text, label, ""))
}

// atoiOrZero parses an int, returning 0 on failure. Only used after the regex
// has already guaranteed digit-only groups, so failure is not expected.
func atoiOrZero(s string) int {
	n, err := strconv.Atoi(s)
	if err != nil {
		return 0
	}
	return n
}
