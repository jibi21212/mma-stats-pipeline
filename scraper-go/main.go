// Command ufcscraper is a fast, concurrent replacement for the original
// Python/Django ufcstats.com scraper. It writes a local SQLite DB (data/ufc.db)
// that the Python ML component reads.
//
// Speed model: the old scraper slept 1s after every single page fetch (serial).
// Here a token-bucket rate limiter caps the *aggregate* request rate (--rate
// req/s) while a worker pool (--concurrency goroutines) issues many requests in
// flight at once. A single writer goroutine owns the DB, so parsing parallelism
// never causes write contention. Net effect: throughput approaches --rate
// instead of being pinned to one-request-per-second.
//
// Incremental by default ("only scrape what we need"):
//   - Fighters: the letter index gives (url, name); fighters already in the DB
//     are SKIPPED, so only NEW fighters are fetched.
//   - Events: the completed-events listing is newest-first, so the scrape stops
//     at the first already-stored event.
//   - Refresh: after new events are saved, the fighters who fought in them have
//     changed records/career averages, so just those are re-fetched.
//
// Use --full to ignore all skip sets and re-fetch everything.
package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"ufcscraper/internal/fetch"
	"ufcscraper/internal/model"
	"ufcscraper/internal/parse"
	"ufcscraper/internal/store"
	"ufcscraper/internal/telemetry"

	"github.com/PuerkitoBio/goquery"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
)

const (
	fightersBaseURL = "http://www.ufcstats.com/statistics/fighters"
	eventsURL       = "http://www.ufcstats.com/statistics/events/completed?page=all"
)

// config holds the parsed CLI flags.
type config struct {
	letter       string
	full         bool
	limit        int
	fightersOnly bool
	eventsOnly   bool
	noRefresh    bool
	concurrency  int
	rate         float64
	dbPath       string
}

func main() {
	cfg := parseFlags()

	// Cancel cleanly on Ctrl-C / SIGTERM so an interrupted run still flushes
	// the writer and closes the DB.
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if err := run(ctx, cfg); err != nil {
		log.Fatalf("fatal: %v", err)
	}
}

func parseFlags() config {
	var cfg config
	flag.StringVar(&cfg.letter, "letter", "", "scrape only this single letter's fighters (default: all 26)")
	flag.BoolVar(&cfg.full, "full", false, "full re-scrape: ignore incremental skip sets and re-fetch everything")
	flag.IntVar(&cfg.limit, "limit", 0, "max events to save (0 = no limit)")
	flag.BoolVar(&cfg.fightersOnly, "fighters-only", false, "run only the fighters stage")
	flag.BoolVar(&cfg.eventsOnly, "events-only", false, "run only the events stage")
	flag.BoolVar(&cfg.noRefresh, "no-refresh", false, "skip re-fetching fighters who appear in newly scraped events")
	flag.IntVar(&cfg.concurrency, "concurrency", 16, "number of concurrent fetch/parse workers")
	flag.Float64Var(&cfg.rate, "rate", 10, "aggregate request rate limit (requests/second)")
	flag.StringVar(&cfg.dbPath, "db", "../data/ufc.db", "path to the SQLite database file")
	flag.Parse()
	return cfg
}

func run(ctx context.Context, cfg config) error {
	shutdown, err := telemetry.Setup(ctx)
	if err != nil {
		return fmt.Errorf("telemetry setup: %w", err)
	}
	if shutdown != nil {
		defer func() {
			flushCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			defer cancel()
			if err := shutdown(flushCtx); err != nil {
				log.Printf("telemetry: shutdown: %v", err)
			}
		}()
	}

	start := time.Now()

	ctx, runSpan := telemetry.Start(ctx, telemetry.SpanRun,
		attribute.Bool("scraper.full", cfg.full),
		attribute.Int("scraper.concurrency", cfg.concurrency),
		attribute.Float64("scraper.rate", cfg.rate),
		attribute.String("scraper.db", cfg.dbPath),
	)
	defer runSpan.End()

	db, err := store.Open(cfg.dbPath)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	defer db.Close()

	client := fetch.New(cfg.rate)

	mode := "incremental"
	if cfg.full {
		mode = "full"
	}
	log.Printf("ufcscraper starting: db=%s concurrency=%d rate=%.1f/s mode=%s",
		cfg.dbPath, cfg.concurrency, cfg.rate, mode)

	var fStats stageStats
	var eStats stageStats

	// Stage 1 — fighters. Returns the index name->url map (used by the refresh)
	// and the set of names fetched fresh this run (so the refresh doesn't redo
	// them).
	var nameToURL map[string]string
	var fetchedNew map[string]bool
	if !cfg.eventsOnly {
		nameToURL, fetchedNew, err = runFighters(ctx, cfg, client, db, &fStats)
		if err != nil {
			return fmt.Errorf("fighters stage: %w", err)
		}
	}

	// Stage 2 — events. Returns the set of fighter names that appeared in the
	// newly-saved events.
	var newEventFighters map[string]bool
	if !cfg.fightersOnly {
		newEventFighters, err = runEvents(ctx, cfg, client, db, &eStats)
		if err != nil {
			return fmt.Errorf("events stage: %w", err)
		}
	}

	// Stage 3 — refresh fighters who fought in the new events (their records and
	// career averages changed). Only when both stages ran (we need the index
	// map) and not a --full pass (which already re-fetched everyone) and not
	// disabled via --no-refresh.
	if !cfg.eventsOnly && !cfg.fightersOnly && !cfg.full && !cfg.noRefresh && len(newEventFighters) > 0 {
		if err = refreshFighters(ctx, cfg, client, db, nameToURL, newEventFighters, fetchedNew, &fStats); err != nil {
			return fmt.Errorf("refresh stage: %w", err)
		}
	}

	elapsed := time.Since(start).Round(time.Millisecond)
	log.Printf("SUMMARY fighters[new=%d refreshed=%d skipped=%d failed=%d] events[new=%d skipped=%d failed=%d] elapsed=%s",
		fStats.created.Load(), fStats.updated.Load(), fStats.skipped.Load(), fStats.failed.Load(),
		eStats.created.Load(), eStats.skipped.Load(), eStats.failed.Load(),
		elapsed,
	)
	return nil
}

// stageStats holds atomic counters shared across workers in a stage.
type stageStats struct {
	created atomic.Int64 // new fighters / new events
	updated atomic.Int64 // fighters refreshed (or updated under --full)
	skipped atomic.Int64 // fighters skipped as already-stored / event early-stop
	failed  atomic.Int64
}

// letters returns the 26 lowercase letters, or just the single configured
// letter when --letter is set.
func letters(cfg config) []string {
	if cfg.letter != "" {
		return []string{cfg.letter}
	}
	out := make([]string, 0, 26)
	for c := 'a'; c <= 'z'; c++ {
		out = append(out, string(c))
	}
	return out
}

// runFighters is Stage 1. It fetches every letter index (cheap: 26 pages),
// builds a name->url map, then fetches detail pages. Incremental by default:
// fighters whose name is already in the DB are SKIPPED, so only NEW fighters are
// fetched. --full re-fetches everyone (existing -> updated). Returns the
// name->url map and the set of names fetched fresh this run.
func runFighters(ctx context.Context, cfg config, client *fetch.Client, db *store.Store, stats *stageStats) (map[string]string, map[string]bool, error) {
	ctx, stageSpan := telemetry.Start(ctx, telemetry.SpanFighters)
	defer stageSpan.End()

	existing := map[string]bool{}
	if !cfg.full {
		var err error
		existing, err = db.ExistingFighterNames()
		if err != nil {
			return nil, nil, err
		}
	}

	// Phase A: gather (url, name) for every fighter from the letter index pages.
	var links []parse.FighterLink
	nameToURL := make(map[string]string)
	for _, ch := range letters(cfg) {
		if ctx.Err() != nil {
			return nameToURL, nil, ctx.Err()
		}
		indexURL := fmt.Sprintf("%s?char=%s&page=all", fightersBaseURL, ch)
		doc, err := client.GetDoc(ctx, indexURL)
		if err != nil {
			log.Printf("fighters: letter %q index fetch failed: %v", ch, err)
			continue
		}
		_, tspan := telemetry.StartTransform(ctx, "fighter_index",
			attribute.String("index.letter", ch),
		)
		found := parse.ScrapeFighterIndex(doc)
		tspan.End()
		log.Printf("fighters: letter %q -> %d fighters", ch, len(found))
		for _, l := range found {
			nameToURL[l.Name] = l.URL
		}
		links = append(links, found...)
	}

	// Decide which to fetch: incremental skips fighters already in the DB.
	var toFetch []parse.FighterLink
	for _, l := range links {
		if cfg.full || !existing[l.Name] {
			toFetch = append(toFetch, l)
		} else {
			stats.skipped.Add(1)
		}
	}
	log.Printf("fighters: %d to fetch, %d skipped as already-stored", len(toFetch), len(links)-len(toFetch))

	fetchedNew := make(map[string]bool)
	var mu sync.Mutex

	// Phase B: worker pool over the chosen links. Each worker inherits the
	// fighters-stage context so extract/transform/load spans nest under one trace.
	linkCh := make(chan parse.FighterLink)
	var wg sync.WaitGroup
	for i := 0; i < cfg.concurrency; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for l := range linkCh {
				scrapeOneFighter(ctx, client, db, l, existing, stats, fetchedNew, &mu)
			}
		}()
	}

	for _, l := range toFetch {
		if ctx.Err() != nil {
			break
		}
		linkCh <- l
	}
	close(linkCh)
	wg.Wait()

	log.Printf("fighters done: new=%d updated=%d skipped=%d failed=%d",
		stats.created.Load(), stats.updated.Load(), stats.skipped.Load(), stats.failed.Load())
	return nameToURL, fetchedNew, ctx.Err()
}

// eventWork is the tri-state result of scraping one event from the listing.
type eventWork int

const (
	eventSaved eventWork = iota
	eventFailed
	eventEarlyStop
)

// scrapeOneFighter is one fighter unit of work: extract (GetDoc), transform
// (ParseFighterPage), load (UpsertFighter). The passed ctx is the fighters
// (or refresh) stage span; a child scraper.fighter span parents the three
// ETL stages so they nest under one trace across the worker-pool goroutine.
func scrapeOneFighter(ctx context.Context, client *fetch.Client, db *store.Store, l parse.FighterLink, existing map[string]bool, stats *stageStats, fetchedNew map[string]bool, mu *sync.Mutex) {
	ctx, span := telemetry.Start(ctx, telemetry.SpanFighter,
		attribute.String("fighter.name", l.Name),
		attribute.String("url", l.URL),
	)
	defer span.End()

	doc, err := client.GetDoc(ctx, l.URL)
	if err != nil {
		log.Printf("fighter fetch failed %s: %v", l.URL, err)
		stats.failed.Add(1)
		telemetry.RecordError(span, err)
		return
	}
	f, ok := parseFighterTraced(ctx, doc)
	if !ok {
		log.Printf("fighter parse failed %s", l.URL)
		stats.failed.Add(1)
		span.SetStatus(codes.Error, "parse failed")
		return
	}
	if err := db.UpsertFighterCtx(ctx, f); err != nil {
		log.Printf("fighter upsert failed %s: %v", f.Name, err)
		stats.failed.Add(1)
		return
	}
	if existing[f.Name] {
		stats.updated.Add(1)
	} else {
		stats.created.Add(1)
		if fetchedNew != nil && mu != nil {
			mu.Lock()
			fetchedNew[f.Name] = true
			mu.Unlock()
		}
	}
}

func parseFighterTraced(ctx context.Context, doc *goquery.Document) (model.Fighter, bool) {
	_, span := telemetry.StartTransform(ctx, "fighter")
	defer span.End()
	return parse.ParseFighterPage(doc)
}

func parseFightTraced(ctx context.Context, doc *goquery.Document, eventDate string) (model.Fight, bool) {
	_, span := telemetry.StartTransform(ctx, "fight")
	defer span.End()
	return parse.ParseFightPage(doc, eventDate)
}

func parseEventTraced(ctx context.Context, doc *goquery.Document) (parse.EventPage, bool) {
	_, span := telemetry.StartTransform(ctx, "event")
	defer span.End()
	return parse.ParseEventPage(doc)
}

// runEvents is Stage 2: fetch the completed-events listing (newest-first), then
// for each event fetch the event page and ALL its fight pages concurrently,
// assemble, and SaveEvent in one transaction. Newest-first early-stop: stop at
// the first already-stored title (unless --full). Honors --limit (max events
// saved). Returns the set of fighter names that appeared in the saved events
// (for the incremental refresh).
func runEvents(ctx context.Context, cfg config, client *fetch.Client, db *store.Store, stats *stageStats) (map[string]bool, error) {
	ctx, stageSpan := telemetry.Start(ctx, telemetry.SpanEvents)
	defer stageSpan.End()

	newEventFighters := make(map[string]bool)

	existing := map[string]bool{}
	if !cfg.full {
		var err error
		existing, err = db.ExistingEventTitles()
		if err != nil {
			return nil, err
		}
	}

	listDoc, err := client.GetDoc(ctx, eventsURL)
	if err != nil {
		return nil, fmt.Errorf("events listing fetch: %w", err)
	}
	var eventURLs []string
	{
		_, tspan := telemetry.StartTransform(ctx, "event_index")
		eventURLs = parse.ScrapeEventURLs(listDoc)
		tspan.End()
	}
	log.Printf("events: %d events in listing (newest first)", len(eventURLs))

	saved := 0
	for _, eventURL := range eventURLs {
		if ctx.Err() != nil {
			break
		}
		if cfg.limit > 0 && saved >= cfg.limit {
			log.Printf("events: hit --limit=%d, stopping", cfg.limit)
			break
		}

		switch scrapeOneEvent(ctx, cfg, client, db, eventURL, existing, stats, newEventFighters, saved) {
		case eventSaved:
			saved++
		case eventEarlyStop:
			log.Printf("events done: new=%d skipped=%d failed=%d",
				stats.created.Load(), stats.skipped.Load(), stats.failed.Load())
			return newEventFighters, ctx.Err()
		}
	}

	log.Printf("events done: new=%d skipped=%d failed=%d",
		stats.created.Load(), stats.skipped.Load(), stats.failed.Load())
	return newEventFighters, ctx.Err()
}

func scrapeOneEvent(ctx context.Context, cfg config, client *fetch.Client, db *store.Store, eventURL string, existing map[string]bool, stats *stageStats, newEventFighters map[string]bool, savedSoFar int) eventWork {
	ctx, span := telemetry.Start(ctx, telemetry.SpanEvent,
		attribute.String("url", eventURL),
	)
	defer span.End()

	doc, err := client.GetDoc(ctx, eventURL)
	if err != nil {
		log.Printf("event fetch failed %s: %v", eventURL, err)
		stats.failed.Add(1)
		telemetry.RecordError(span, err)
		return eventFailed
	}
	ep, ok := parseEventTraced(ctx, doc)
	if !ok {
		log.Printf("event parse failed %s", eventURL)
		stats.failed.Add(1)
		span.SetStatus(codes.Error, "parse failed")
		return eventFailed
	}
	span.SetAttributes(attribute.String("event.title", ep.Title))

	// Newest-first early stop: the first already-stored title means every
	// older event is already in the DB.
	if !cfg.full && existing[ep.Title] {
		log.Printf("events: reached already-stored %q, stopping (incremental)", ep.Title)
		stats.skipped.Add(1)
		return eventEarlyStop
	}

	fights := fetchFightsConcurrently(ctx, cfg, client, ep)
	if len(fights) == 0 {
		log.Printf("events: no fights parsed for %q, skipping", ep.Title)
		stats.failed.Add(1)
		span.SetStatus(codes.Error, "no fights")
		return eventFailed
	}

	ev := parse.ToEvent(ep, fights)
	if err := db.SaveEventCtx(ctx, ev); err != nil {
		log.Printf("event save failed %q: %v", ep.Title, err)
		stats.failed.Add(1)
		return eventFailed
	}
	stats.created.Add(1)
	for _, f := range fights {
		if f.Competitor1.FighterName != "" {
			newEventFighters[f.Competitor1.FighterName] = true
		}
		if f.Competitor2.FighterName != "" {
			newEventFighters[f.Competitor2.FighterName] = true
		}
	}
	log.Printf("events: saved %q (%d fights) [%d total]", ep.Title, len(fights), savedSoFar+1)
	return eventSaved
}

// refreshFighters re-fetches the detail page of every fighter who appeared in a
// newly-saved event and was not already fetched fresh this run — their record
// and career averages changed because they just fought. Names are mapped back to
// detail URLs via the index map built in Stage 1; names not present in the index
// (rare card-only spellings) are skipped. Refreshes count as fighters[refreshed]
// in the summary.
func refreshFighters(ctx context.Context, cfg config, client *fetch.Client, db *store.Store, nameToURL map[string]string, newEventFighters, fetchedNew map[string]bool, stats *stageStats) error {
	ctx, stageSpan := telemetry.Start(ctx, telemetry.SpanRefresh)
	defer stageSpan.End()

	var targets []string
	for name := range newEventFighters {
		if fetchedNew[name] {
			continue // already scraped fresh as a new fighter this run
		}
		if _, ok := nameToURL[name]; !ok {
			continue // not in the index -> cannot map name to a detail URL
		}
		targets = append(targets, name)
	}
	if len(targets) == 0 {
		return ctx.Err()
	}
	log.Printf("refresh: %d fighters from new events to re-fetch", len(targets))

	nameCh := make(chan string)
	var wg sync.WaitGroup
	for i := 0; i < cfg.concurrency; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for name := range nameCh {
				l := parse.FighterLink{Name: name, URL: nameToURL[name]}
				// existing=true so the helper counts these as refreshed/updated.
				scrapeOneFighter(ctx, client, db, l, map[string]bool{name: true}, stats, nil, nil)
			}
		}()
	}
	for _, name := range targets {
		if ctx.Err() != nil {
			break
		}
		nameCh <- name
	}
	close(nameCh)
	wg.Wait()

	log.Printf("refresh done: %d fighters refreshed", stats.updated.Load())
	return ctx.Err()
}

// fetchFightsConcurrently fetches and parses all of an event's fight pages in
// parallel (bounded by --concurrency), returning the successfully parsed fights.
// Failed individual fights are logged and skipped (fail-soft).
//
// The event span in ctx is the parent; each fight goroutine starts a
// scraper.fight child so extract/transform nest under the same trace.
func fetchFightsConcurrently(ctx context.Context, cfg config, client *fetch.Client, ep parse.EventPage) []model.Fight {
	results := make([]model.Fight, len(ep.FightURLs))
	ok := make([]bool, len(ep.FightURLs))

	sem := make(chan struct{}, cfg.concurrency)
	var wg sync.WaitGroup

	for i, fightURL := range ep.FightURLs {
		if ctx.Err() != nil {
			break
		}
		wg.Add(1)
		sem <- struct{}{}
		go func(ctx context.Context, i int, url string) {
			defer wg.Done()
			defer func() { <-sem }()

			ctx, span := telemetry.Start(ctx, telemetry.SpanFight,
				attribute.String("url", url),
			)
			defer span.End()

			doc, err := client.GetDoc(ctx, url)
			if err != nil {
				log.Printf("fight fetch failed %s: %v", url, err)
				telemetry.RecordError(span, err)
				return
			}
			fight, parsed := parseFightTraced(ctx, doc, ep.Date)
			if !parsed {
				log.Printf("fight parse failed %s", url)
				span.SetStatus(codes.Error, "parse failed")
				return
			}
			results[i] = fight
			ok[i] = true
		}(ctx, i, fightURL)
	}
	wg.Wait()

	// Preserve document order; drop the ones that failed.
	out := make([]model.Fight, 0, len(results))
	for i := range results {
		if ok[i] {
			out = append(out, results[i])
		}
	}
	return out
}
