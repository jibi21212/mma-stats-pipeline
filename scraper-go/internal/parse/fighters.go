package parse

import (
	"context"
	"database/sql"
	"strings"
	"time"

	"github.com/PuerkitoBio/goquery"

	"ufcscraper/internal/model"
)

// nullStr is a small constructor for a valid sql.NullString.
func nullStr(s string) sql.NullString {
	return sql.NullString{String: s, Valid: true}
}

// Fetcher is the minimal fetch surface the page parsers need. fetch.Client
// satisfies it; tests can pass a stub. Kept here (the first parser file) and
// reused by the event/fight parsers.
type Fetcher interface {
	GetDoc(ctx context.Context, url string) (*goquery.Document, error)
}

// labelMapFromList walks every li.b-list__box-list-item under sel, reads each
// item's inner <i> text as the label, and maps label -> the li's label-stripped
// text (i.e. ExtractText on the whole li). This deliberately avoids the
// cascadia :has / :-soup-contains pseudo-classes the Python used: we iterate and
// match labels in Go instead. The label keys are the exact strings from the
// schema contract ("Height:", "SLpM:", ...).
func labelMapFromList(sel *goquery.Selection) map[string]string {
	out := make(map[string]string)
	sel.Find("li.b-list__box-list-item").Each(func(_ int, li *goquery.Selection) {
		label := strings.TrimSpace(li.Find("i").First().Text())
		if label == "" {
			return
		}
		// Mirror extract_text: strip the label from the li's full text.
		value := strings.TrimSpace(strings.ReplaceAll(strings.TrimSpace(li.Text()), label, ""))
		out[label] = value
	})
	return out
}

// labelValue returns the value for a label, or the "--" sentinel when the label
// was not present — matching extract_text's behavior when its selector found
// nothing (it returned "--").
func labelValue(m map[string]string, label string) string {
	if v, ok := m[label]; ok {
		return v
	}
	return "--"
}

// ParseFighterPage ports parse_fighter_page. It reads the fighter detail
// document and returns a fully-populated model.Fighter plus ok=false when the
// page cannot be parsed (the orchestrator logs and skips). Parsing never panics:
// missing nodes degrade to placeholders / NULL.
func ParseFighterPage(doc *goquery.Document) (model.Fighter, bool) {
	var f model.Fighter

	section := doc.Find("section.b-statistics__section_details").First()
	if section.Length() == 0 {
		return f, false
	}
	div := section.Find("div.l-page__container").First()
	if div.Length() == 0 {
		return f, false
	}

	// Name and record live in the h2.b-content__title.
	h2 := div.Find("h2.b-content__title").First()
	name := strings.TrimSpace(h2.Find("span.b-content__title-highlight").First().Text())
	if name == "" {
		return f, false
	}
	f.Name = name

	recordText := strings.TrimSpace(h2.Find("span.b-content__title-record").First().Text())
	rec := ParseRecord(recordText)
	f.Wins, f.Losses, f.Draws, f.NoContests = rec.Wins, rec.Losses, rec.Draws, rec.NoContests

	// Nickname: div.p.text.strip() or None.
	nickname := strings.TrimSpace(div.Find("p").First().Text())
	if nickname != "" {
		f.Nickname = nullStr(nickname)
	}

	f.Nationality = "Unlisted"

	// Collect every labelled list-item across the detail container into one
	// label -> value map. The labels are globally unique on the page, so a
	// single sweep covers the basic, striking, and takedown info boxes.
	labels := labelMapFromList(div)

	// Basic physical stats. extract_text already stripped the label; the extra
	// .replace('"','') / .replace(' lbs.','') from Python is applied here too.
	heightStr := strings.ReplaceAll(labelValue(labels, "Height:"), `"`, "")
	weightStr := strings.ReplaceAll(labelValue(labels, "Weight:"), " lbs.", "")
	reachStr := strings.ReplaceAll(labelValue(labels, "Reach:"), `"`, "")
	stance := labelValue(labels, "STANCE:")
	dobStr := labelValue(labels, "DOB:")

	f.HeightIn = parseOptionalHeight(heightStr)
	f.WeightLbs = ParseOptionalInt(weightStr)
	f.ReachIn = ParseOptionalInt(strings.ReplaceAll(reachStr, `"`, ""))
	if stance != "--" {
		f.Stance = nullStr(stance)
	}
	f.DOB = parseDOB(dobStr)

	// Career striking stats.
	f.SLpM = ParseOptionalFloat(labelValue(labels, "SLpM:"))
	f.StrAcc = ParsePercentage(labelValue(labels, "Str. Acc.:"))
	f.SApM = ParseOptionalFloat(labelValue(labels, "SApM:"))
	f.StrDef = ParsePercentage(labelValue(labels, "Str. Def:"))

	// Career takedown stats.
	f.TDAvg = ParseOptionalFloat(labelValue(labels, "TD Avg.:"))
	f.TDAcc = ParsePercentage(labelValue(labels, "TD Acc.:"))
	f.TDDef = ParsePercentage(labelValue(labels, "TD Def.:"))
	f.SubAvg = ParseOptionalFloat(labelValue(labels, "Sub. Avg.:"))

	return f, true
}

// FighterLink is one entry from a letter-index listing: the detail-page URL and
// the fighter's name (first + last name columns joined). The incremental pass
// uses Name to skip fighters already stored and to refresh those who appear in
// newly-scraped events.
type FighterLink struct {
	URL  string
	Name string
}

// ScrapeFighterIndex extracts (URL, Name) for every fighter row in a letter-
// index listing, in document order. The first two columns are the first and
// last name (both cells link to the same detail page), so the name is their
// join and the URL comes from the first link. Header rows (no <td><a>) are
// skipped naturally.
func ScrapeFighterIndex(doc *goquery.Document) []FighterLink {
	var links []FighterLink
	tbody := doc.Find("body.b-page tbody").First()
	if tbody.Length() == 0 {
		return links
	}
	tbody.Find("tr.b-statistics__table-row").Each(func(_ int, tr *goquery.Selection) {
		tds := tr.Find("td")
		if tds.Length() == 0 {
			return
		}
		href, ok := tds.First().Find("a").First().Attr("href")
		if !ok || href == "" {
			return // header rows carry no link
		}
		first := strings.TrimSpace(tds.Eq(0).Find("a").First().Text())
		last := ""
		if tds.Length() > 1 {
			last = strings.TrimSpace(tds.Eq(1).Find("a").First().Text())
		}
		links = append(links, FighterLink{URL: href, Name: strings.TrimSpace(first + " " + last)})
	})
	return links
}

// ScrapeFighterURLs ports scrape_fighter_urls: just the detail hrefs, in
// document order (a thin projection of ScrapeFighterIndex).
func ScrapeFighterURLs(doc *goquery.Document) []string {
	links := ScrapeFighterIndex(doc)
	urls := make([]string, 0, len(links))
	for _, l := range links {
		urls = append(urls, l.URL)
	}
	return urls
}

// parseOptionalHeight applies parse_optional(height_str, convert_height): the
// placeholder check happens first, then convert_height. Returns NULL for
// placeholders or unparseable heights.
func parseOptionalHeight(value string) sql.NullInt64 {
	value = strings.TrimSpace(value)
	if value == "--" || value == "---" {
		return sql.NullInt64{}
	}
	return ConvertHeight(value)
}

// parseDOB converts a "%b %d, %Y" date (e.g. "Jul 19, 1987") to ISO
// "YYYY-MM-DD". Placeholders and unparseable dates yield NULL, matching the
// Python try/except that left dob = None on failure.
func parseDOB(dobStr string) sql.NullString {
	dobStr = strings.TrimSpace(dobStr)
	if dobStr == "" || dobStr == "--" {
		return sql.NullString{}
	}
	t, err := time.Parse("Jan 2, 2006", dobStr)
	if err != nil {
		return sql.NullString{}
	}
	return nullStr(t.Format("2006-01-02"))
}
