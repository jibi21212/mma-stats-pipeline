package parse

import "testing"

const fighterHTML = `
<html><body>
  <section class="b-statistics__section_details">
    <div class="l-page__container">
      <h2 class="b-content__title">
        <span class="b-content__title-highlight">Jon Jones</span>
        <span class="b-content__title-record">Record: 27-1-0 (1 NC)</span>
      </h2>
      <p class="b-content__Nickname">Bones</p>

      <div class="b-list__info-box b-list__info-box_style_small-width js-guide">
        <ul class="b-list__box-list">
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">Height:</i> 6' 4"</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">Weight:</i> 205 lbs.</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">Reach:</i> 84"</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">STANCE:</i> Orthodox</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">DOB:</i> Jul 19, 1987</li>
        </ul>
      </div>

      <div class="b-list__info-box-left clearfix">
        <ul class="b-list__box-list b-list__box-list_margin-top">
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">SLpM:</i> 4.30</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">Str. Acc.:</i> 57%</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">SApM:</i> 2.22</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">Str. Def:</i> 64%</li>
        </ul>
      </div>

      <div class="b-list__info-box-right clearfix">
        <ul class="b-list__box-list b-list__box-list_margin-top">
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">TD Avg.:</i> 1.90</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">TD Acc.:</i> 45%</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">TD Def.:</i> 95%</li>
          <li class="b-list__box-list-item"><i class="b-list__box-item-title">Sub. Avg.:</i> 0.50</li>
        </ul>
      </div>
    </div>
  </section>
</body></html>`

func TestParseFighterPage_HappyPath(t *testing.T) {
	f, ok := ParseFighterPage(mustDoc(t, fighterHTML))
	if !ok {
		t.Fatal("fighter page should parse, not fall through to false")
	}
	if f.Name != "Jon Jones" {
		t.Errorf("Name = %q, want Jon Jones", f.Name)
	}
	if !f.Nickname.Valid || f.Nickname.String != "Bones" {
		t.Errorf("Nickname = %+v, want Bones", f.Nickname)
	}
	if !f.HeightIn.Valid || f.HeightIn.Int64 != 76 {
		t.Errorf("HeightIn = %+v, want 76", f.HeightIn)
	}
	if !f.WeightLbs.Valid || f.WeightLbs.Int64 != 205 {
		t.Errorf("WeightLbs = %+v, want 205", f.WeightLbs)
	}
	if !f.ReachIn.Valid || f.ReachIn.Int64 != 84 {
		t.Errorf("ReachIn = %+v, want 84", f.ReachIn)
	}
	if !f.Stance.Valid || f.Stance.String != "Orthodox" {
		t.Errorf("Stance = %+v, want Orthodox", f.Stance)
	}
	if !f.DOB.Valid || f.DOB.String != "1987-07-19" {
		t.Errorf("DOB = %+v, want 1987-07-19", f.DOB)
	}
	if f.Nationality != "Unlisted" {
		t.Errorf("Nationality = %q, want Unlisted", f.Nationality)
	}

	if f.Wins != 27 || f.Losses != 1 || f.Draws != 0 || f.NoContests != 1 {
		t.Errorf("record = %d-%d-%d (%d NC), want 27-1-0 (1 NC)",
			f.Wins, f.Losses, f.Draws, f.NoContests)
	}

	checkF := func(name string, got float64, valid bool, want float64) {
		if !valid || !almostEqual(got, want) {
			t.Errorf("%s = %v (valid=%v), want %v", name, got, valid, want)
		}
	}
	checkF("slpm", f.SLpM.Float64, f.SLpM.Valid, 4.30)
	checkF("str_acc", f.StrAcc.Float64, f.StrAcc.Valid, 0.57)
	checkF("sapm", f.SApM.Float64, f.SApM.Valid, 2.22)
	checkF("str_def", f.StrDef.Float64, f.StrDef.Valid, 0.64)
	checkF("td_avg", f.TDAvg.Float64, f.TDAvg.Valid, 1.90)
	checkF("td_acc", f.TDAcc.Float64, f.TDAcc.Valid, 0.45)
	checkF("td_def", f.TDDef.Float64, f.TDDef.Valid, 0.95)
	checkF("sub_avg", f.SubAvg.Float64, f.SubAvg.Valid, 0.50)
}

func TestParseFighterPage_MissingSectionReturnsFalse(t *testing.T) {
	if _, ok := ParseFighterPage(mustDoc(t, `<html><body></body></html>`)); ok {
		t.Error("expected ok=false for a page with no details section")
	}
}

const fighterListingHTML = `
<html><body class="b-page">
  <table class="b-statistics__table">
    <tbody>
      <tr class="b-statistics__table-row"><th>header</th></tr>
      <tr class="b-statistics__table-row">
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/1">Adams</a></td>
      </tr>
      <tr class="b-statistics__table-row">
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/2">Allen</a></td>
      </tr>
    </tbody>
  </table>
</body></html>`

func TestScrapeFighterURLs(t *testing.T) {
	urls := ScrapeFighterURLs(mustDoc(t, fighterListingHTML))
	want := []string{"http://example.com/fighter/1", "http://example.com/fighter/2"}
	if len(urls) != len(want) {
		t.Fatalf("got %d urls %v, want %d", len(urls), urls, len(want))
	}
	for i := range want {
		if urls[i] != want[i] {
			t.Errorf("url[%d] = %q, want %q", i, urls[i], want[i])
		}
	}
}

func TestScrapeFighterURLs_NoTbody(t *testing.T) {
	urls := ScrapeFighterURLs(mustDoc(t, `<html><body class="b-page"></body></html>`))
	if len(urls) != 0 {
		t.Errorf("expected no urls, got %v", urls)
	}
}

// fighterIndexHTML mirrors the real letter-index structure: the first two
// columns are the first and last name (both linking to the same detail page),
// followed by a nickname column. ScrapeFighterIndex must join first + last into
// the fighter name used by the incremental skip/refresh logic.
const fighterIndexHTML = `
<html><body class="b-page">
  <table class="b-statistics__table">
    <tbody>
      <tr class="b-statistics__table-row"><th>First</th><th>Last</th></tr>
      <tr class="b-statistics__table-row">
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/1">Billy</a></td>
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/1">Quarantillo</a></td>
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/1">"Billy Q"</a></td>
      </tr>
      <tr class="b-statistics__table-row">
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/2">Jon</a></td>
        <td class="b-statistics__table-col"><a href="http://example.com/fighter/2">Jones</a></td>
        <td class="b-statistics__table-col"></td>
      </tr>
    </tbody>
  </table>
</body></html>`

func TestScrapeFighterIndex_NameAndURL(t *testing.T) {
	links := ScrapeFighterIndex(mustDoc(t, fighterIndexHTML))
	if len(links) != 2 {
		t.Fatalf("got %d links, want 2: %+v", len(links), links)
	}
	if links[0].URL != "http://example.com/fighter/1" || links[0].Name != "Billy Quarantillo" {
		t.Errorf("link[0] = %+v, want {.../1, Billy Quarantillo}", links[0])
	}
	if links[1].URL != "http://example.com/fighter/2" || links[1].Name != "Jon Jones" {
		t.Errorf("link[1] = %+v, want {.../2, Jon Jones}", links[1])
	}
}
