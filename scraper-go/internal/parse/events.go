package parse

import (
	"strings"
	"time"

	"github.com/PuerkitoBio/goquery"

	"ufcscraper/internal/model"
)

// EventPage is the parsed result of an event detail page: the event metadata
// plus the list of its fight detail URLs (to be fetched separately).
type EventPage struct {
	Title    string
	Date     string // ISO YYYY-MM-DD
	Location string
	FightURLs []string
}

// ParseEventPage ports parse_event_page. Returns ok=false when the page lacks
// the expected structure or the date cannot be parsed (the Python version threw
// and the except returned None). The date is converted from "%B %d, %Y"
// ("March 04, 2023") to ISO "2023-03-04".
func ParseEventPage(doc *goquery.Document) (EventPage, bool) {
	var ev EventPage

	section := doc.Find("section.b-statistics__section_details").First()
	if section.Length() == 0 {
		return ev, false
	}

	title := strings.TrimSpace(
		section.Find("h2.b-content__title span.b-content__title-highlight").First().Text(),
	)
	if title == "" {
		return ev, false
	}
	ev.Title = title

	// Date and location from the large-width info box, via label lookup
	// (no :has / :-soup-contains).
	infoBox := section.Find("div.b-list__info-box_style_large-width").First()
	labels := labelMapFromList(infoBox)
	dateStr := labelValue(labels, "Date:")
	ev.Location = labelValue(labels, "Location:")

	// datetime.strptime(date_str, '%B %d, %Y') — failure aborts the parse,
	// matching the Python exception path.
	t, err := time.Parse("January 2, 2006", strings.TrimSpace(dateStr))
	if err != nil {
		return ev, false
	}
	ev.Date = t.Format("2006-01-02")

	// Collect fight URLs in document order from the event-details table rows.
	tbody := section.Find("tbody").First()
	if tbody.Length() > 0 {
		tbody.Find("tr.js-fight-details-click").Each(func(_ int, tr *goquery.Selection) {
			td := tr.Find("td").First()
			if td.Length() == 0 {
				return
			}
			a := td.Find("a").First()
			if href, ok := a.Attr("href"); ok && href != "" {
				ev.FightURLs = append(ev.FightURLs, href)
			}
		})
	}

	return ev, true
}

// ScrapeEventURLs ports scrape_event_urls: extract event detail hrefs from the
// completed-events listing, preserving the site's newest-first order. Header
// rows (no <a>) are skipped.
func ScrapeEventURLs(doc *goquery.Document) []string {
	var urls []string
	tbody := doc.Find("div.b-statistics__inner table.b-statistics__table-events tbody").First()
	if tbody.Length() == 0 {
		return urls
	}
	tbody.Find("tr.b-statistics__table-row").Each(func(_ int, tr *goquery.Selection) {
		a := tr.Find("a").First()
		if href, ok := a.Attr("href"); ok && href != "" {
			urls = append(urls, href)
		}
	})
	return urls
}

// ToEvent converts a parsed EventPage plus its assembled fights into a
// model.Event ready for persistence.
func ToEvent(ep EventPage, fights []model.Fight) model.Event {
	return model.Event{
		Title:    ep.Title,
		Date:     ep.Date,
		Location: ep.Location,
		Fights:   fights,
	}
}
