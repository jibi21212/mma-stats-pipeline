package parse

import (
	"math"
	"testing"

	"github.com/PuerkitoBio/goquery"
	"strings"
)

// almostEqual reports whether two floats are within a small epsilon, the Go
// analogue of unittest's assertAlmostEqual.
func almostEqual(a, b float64) bool {
	return math.Abs(a-b) < 1e-9
}

// --- ParseOptional (float / int) ---------------------------------------------

func TestParseOptionalFloat(t *testing.T) {
	if got := ParseOptionalFloat("3.5"); !got.Valid || got.Float64 != 3.5 {
		t.Errorf(`ParseOptionalFloat("3.5") = %+v, want valid 3.5`, got)
	}
	if got := ParseOptionalFloat("--"); got.Valid {
		t.Errorf(`ParseOptionalFloat("--") = %+v, want NULL`, got)
	}
	if got := ParseOptionalFloat("---"); got.Valid {
		t.Errorf(`ParseOptionalFloat("---") = %+v, want NULL`, got)
	}
	if got := ParseOptionalFloat("  --  "); got.Valid {
		t.Errorf(`ParseOptionalFloat("  --  ") = %+v, want NULL (whitespace stripped first)`, got)
	}
	if got := ParseOptionalFloat("  7.0  "); !got.Valid || got.Float64 != 7.0 {
		t.Errorf(`ParseOptionalFloat("  7.0  ") = %+v, want valid 7.0`, got)
	}
}

func TestParseOptionalInt(t *testing.T) {
	if got := ParseOptionalInt("155"); !got.Valid || got.Int64 != 155 {
		t.Errorf(`ParseOptionalInt("155") = %+v, want valid 155`, got)
	}
	if got := ParseOptionalInt("--"); got.Valid {
		t.Errorf(`ParseOptionalInt("--") = %+v, want NULL`, got)
	}
}

// --- ParsePercentage ----------------------------------------------------------

func TestParsePercentage(t *testing.T) {
	cases := []struct {
		in   string
		want float64
	}{
		{"56%", 0.56},
		{"5%", 0.05},   // regression: must not be 0.5
		{"100%", 1.0},  // regression: must not be 0.1
		{"0%", 0.0},
		{"  44%  ", 0.44},
	}
	for _, c := range cases {
		got := ParsePercentage(c.in)
		if !got.Valid || !almostEqual(got.Float64, c.want) {
			t.Errorf("ParsePercentage(%q) = %+v, want %v", c.in, got, c.want)
		}
	}
	for _, ph := range []string{"--", "---", ""} {
		if got := ParsePercentage(ph); got.Valid {
			t.Errorf("ParsePercentage(%q) = %+v, want NULL", ph, got)
		}
	}
}

// --- ConvertHeight ------------------------------------------------------------

func TestConvertHeight(t *testing.T) {
	cases := []struct {
		in   string
		want int64
	}{
		{`5'10"`, 70},
		{`6'0"`, 72},
		{`5' 10"`, 70}, // space after feet (ufcstats rendering)
		{`6' 4"`, 76},  // Jon Jones fixture
	}
	for _, c := range cases {
		got := ConvertHeight(c.in)
		if !got.Valid || got.Int64 != c.want {
			t.Errorf("ConvertHeight(%q) = %+v, want %d", c.in, got, c.want)
		}
	}
	for _, bad := range []string{"garbage", ""} {
		if got := ConvertHeight(bad); got.Valid {
			t.Errorf("ConvertHeight(%q) = %+v, want NULL", bad, got)
		}
	}
}

// --- ConvertTimeToSeconds -----------------------------------------------------

func TestConvertTimeToSeconds(t *testing.T) {
	cases := []struct {
		in   string
		want int
	}{
		{"1:30", 90},
		{"0:00", 0},
		{"4:05", 245},
		{"1:24", 84}, // control-time fixture
		{"2:04", 124},
		{"--", 0},
		{"---", 0},
		{"", 0},
	}
	for _, c := range cases {
		if got := ConvertTimeToSeconds(c.in); got != c.want {
			t.Errorf("ConvertTimeToSeconds(%q) = %d, want %d", c.in, got, c.want)
		}
	}
}

// --- ParseRecord --------------------------------------------------------------

func TestParseRecord(t *testing.T) {
	cases := []struct {
		in   string
		want Record
	}{
		{"Record: 29-7-0 (1 NC)", Record{29, 7, 0, 1}},
		{"Record: 10-2-1", Record{10, 2, 1, 0}},
		{"15-3-0", Record{15, 3, 0, 0}}, // prefix absent
		{"Record: N/A", Record{0, 0, 0, 0}},
		{"Record: 27-1-0 (1 NC)", Record{27, 1, 0, 1}}, // Jon Jones fixture
	}
	for _, c := range cases {
		if got := ParseRecord(c.in); got != c.want {
			t.Errorf("ParseRecord(%q) = %+v, want %+v", c.in, got, c.want)
		}
	}
}

// --- ParseStrikeData ----------------------------------------------------------

func TestParseStrikeData(t *testing.T) {
	cases := []struct {
		in   string
		want StrikeData
	}{
		{"50 of 100", StrikeData{50, 100, 0.5}},
		{"0 of 0", StrikeData{0, 0, 0.0}}, // no divide-by-zero
		{"3 of 12", StrikeData{3, 12, 0.25}},
		{"6 of 10", StrikeData{6, 10, 0.6}}, // sig head-strike fixture
		{"---", StrikeData{0, 0, 0.0}},
		{"--", StrikeData{0, 0, 0.0}},
		{"", StrikeData{0, 0, 0.0}},
		{"nonsense", StrikeData{0, 0, 0.0}},
	}
	for _, c := range cases {
		got := ParseStrikeData(c.in)
		if got.Landed != c.want.Landed || got.Attempted != c.want.Attempted || !almostEqual(got.Percentage, c.want.Percentage) {
			t.Errorf("ParseStrikeData(%q) = %+v, want %+v", c.in, got, c.want)
		}
	}
}

// --- ExtractText --------------------------------------------------------------

func TestExtractText(t *testing.T) {
	doc, err := goquery.NewDocumentFromReader(strings.NewReader(
		`<li><i class="title">Height:</i> 5' 10"</li>`,
	))
	if err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	li := doc.Find("li").First()
	if got := ExtractText(li, "Height:"); got != `5' 10"` {
		t.Errorf(`ExtractText(li, "Height:") = %q, want %q`, got, `5' 10"`)
	}

	// A nil/empty selection returns the "--" sentinel.
	empty := doc.Find("does-not-exist")
	if got := ExtractText(empty, "Height:"); got != "--" {
		t.Errorf(`ExtractText(empty, "Height:") = %q, want "--"`, got)
	}
}
