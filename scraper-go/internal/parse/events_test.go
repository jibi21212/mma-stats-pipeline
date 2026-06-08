package parse

import "testing"

const eventHTML = `
<html><body>
  <section class="b-statistics__section_details">
    <h2 class="b-content__title">
      <span class="b-content__title-highlight">UFC 285: Jones vs Gane</span>
    </h2>

    <div class="b-list__info-box b-list__info-box_style_large-width">
      <ul class="b-list__box-list">
        <li class="b-list__box-list-item"><i class="b-list__box-item-title">Date:</i> March 04, 2023</li>
        <li class="b-list__box-list-item"><i class="b-list__box-item-title">Location:</i> Las Vegas, Nevada, USA</li>
      </ul>
    </div>

    <table class="b-fight-details__table b-fight-details__table_style_margin-top b-fight-details__table_type_event-details js-fight-table">
      <tbody>
        <tr class="b-fight-details__table-row b-fight-details__table-row__hover js-fight-details-click"
            data-link="http://example.com/fight/1">
          <td class="b-fight-details__table-col"><a href="http://example.com/fight/1">view</a></td>
        </tr>
        <tr class="b-fight-details__table-row b-fight-details__table-row__hover js-fight-details-click"
            data-link="http://example.com/fight/2">
          <td class="b-fight-details__table-col"><a href="http://example.com/fight/2">view</a></td>
        </tr>
      </tbody>
    </table>
  </section>
</body></html>`

func TestParseEventPage_HappyPath(t *testing.T) {
	ev, ok := ParseEventPage(mustDoc(t, eventHTML))
	if !ok {
		t.Fatal("event page should parse, not fall through to false")
	}
	if ev.Title != "UFC 285: Jones vs Gane" {
		t.Errorf("Title = %q", ev.Title)
	}
	if ev.Date != "2023-03-04" {
		t.Errorf("Date = %q, want 2023-03-04 (March 04, 2023 -> ISO)", ev.Date)
	}
	if ev.Location != "Las Vegas, Nevada, USA" {
		t.Errorf("Location = %q", ev.Location)
	}
	want := []string{"http://example.com/fight/1", "http://example.com/fight/2"}
	if len(ev.FightURLs) != len(want) {
		t.Fatalf("got %d fight urls %v, want %d", len(ev.FightURLs), ev.FightURLs, len(want))
	}
	for i := range want {
		if ev.FightURLs[i] != want[i] {
			t.Errorf("fight url[%d] = %q, want %q", i, ev.FightURLs[i], want[i])
		}
	}
}

func TestParseEventPage_MissingSectionReturnsFalse(t *testing.T) {
	if _, ok := ParseEventPage(mustDoc(t, `<html><body></body></html>`)); ok {
		t.Error("expected ok=false for a page with no details section")
	}
}

const eventListingHTML = `
<html><body>
  <div class="b-statistics__inner">
    <table class="b-statistics__table-events">
      <tbody>
        <tr class="b-statistics__table-row"><th>header</th></tr>
        <tr class="b-statistics__table-row">
          <td><a href="http://example.com/event/1">UFC 285</a></td>
        </tr>
        <tr class="b-statistics__table-row">
          <td><a href="http://example.com/event/2">UFC 284</a></td>
        </tr>
      </tbody>
    </table>
  </div>
</body></html>`

func TestScrapeEventURLs(t *testing.T) {
	urls := ScrapeEventURLs(mustDoc(t, eventListingHTML))
	want := []string{"http://example.com/event/1", "http://example.com/event/2"}
	if len(urls) != len(want) {
		t.Fatalf("got %d urls %v, want %d", len(urls), urls, len(want))
	}
	for i := range want {
		if urls[i] != want[i] {
			t.Errorf("url[%d] = %q, want %q (newest-first preserved)", i, urls[i], want[i])
		}
	}
}
