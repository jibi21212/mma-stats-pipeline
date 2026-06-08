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
	start := time.Now()

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
		found := parse.ScrapeFighterIndex(doc)
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

	// Phase B: worker pool over the chosen links.
	linkCh := make(chan parse.FighterLink)
	var wg sync.WaitGroup
	for i := 0; i < cfg.concurrency; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for l := range linkCh {
				doc, err := client.GetDoc(ctx, l.URL)
				if err != nil {
					log.Printf("fighter fetch failed %s: %v", l.URL, err)
					stats.failed.Add(1)
					continue
				}
				f, ok := parse.ParseFighterPage(doc)
				if !ok {
					log.Printf("fighter parse failed %s", l.URL)
					stats.failed.Add(1)
					continue
				}
				if err := db.UpsertFighter(f); err != nil {
					log.Printf("fighter upsert failed %s: %v", f.Name, err)
					stats.failed.Add(1)
					continue
				}
				if existing[f.Name] {
					stats.updated.Add(1)
				} else {
					stats.created.Add(1)
					mu.Lock()
					fetchedNew[f.Name] = true
					mu.Unlock()
				}
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

// runEvents is Stage 2: fetch the completed-events listing (newest-first), then
// for each event fetch the event page and ALL its fight pages concurrently,
// assemble, and SaveEvent in one transaction. Newest-first early-stop: stop at
// the first already-stored title (unless --full). Honors --limit (max events
// saved). Returns the set of fighter names that appeared in the saved events
// (for the incremental refresh).
func runEvents(ctx context.Context, cfg config, client *fetch.Client, db *store.Store, stats *stageStats) (map[string]bool, error) {
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
	eventURLs := parse.ScrapeEventURLs(listDoc)
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

		doc, err := client.GetDoc(ctx, eventURL)
		if err != nil {
			log.Printf("event fetch failed %s: %v", eventURL, err)
			stats.failed.Add(1)
			continue
		}
		ep, ok := parse.ParseEventPage(doc)
		if !ok {
			log.Printf("event parse failed %s", eventURL)
			stats.failed.Add(1)
			continue
		}

		// Newest-first early stop: the first already-stored title means every
		// older event is already in the DB.
		if !cfg.full && existing[ep.Title] {
			log.Printf("events: reached already-stored %q, stopping (incremental)", ep.Title)
			stats.skipped.Add(1)
			break
		}

		fights := fetchFightsConcurrently(ctx, cfg, client, ep)
		if len(fights) == 0 {
			log.Printf("events: no fights parsed for %q, skipping", ep.Title)
			stats.failed.Add(1)
			continue
		}

		ev := parse.ToEvent(ep, fights)
		if err := db.SaveEvent(ev); err != nil {
			log.Printf("event save failed %q: %v", ep.Title, err)
			stats.failed.Add(1)
			continue
		}
		saved++
		stats.created.Add(1)
		for _, f := range fights {
			if f.Competitor1.FighterName != "" {
				newEventFighters[f.Competitor1.FighterName] = true
			}
			if f.Competitor2.FighterName != "" {
				newEventFighters[f.Competitor2.FighterName] = true
			}
		}
		log.Printf("events: saved %q (%d fights) [%d total]", ep.Title, len(fights), saved)
	}

	log.Printf("events done: new=%d skipped=%d failed=%d",
		stats.created.Load(), stats.skipped.Load(), stats.failed.Load())
	return newEventFighters, ctx.Err()
}

// refreshFighters re-fetches the detail page of every fighter who appeared in a
// newly-saved event and was not already fetched fresh this run — their record
// and career averages changed because they just fought. Names are mapped back to
// detail URLs via the index map built in Stage 1; names not present in the index
// (rare card-only spellings) are skipped. Refreshes count as fighters[refreshed]
// in the summary.
func refreshFighters(ctx context.Context, cfg config, client *fetch.Client, db *store.Store, nameToURL map[string]string, newEventFighters, fetchedNew map[string]bool, stats *stageStats) error {
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
				doc, err := client.GetDoc(ctx, nameToURL[name])
				if err != nil {
					log.Printf("refresh fetch failed %q: %v", name, err)
					stats.failed.Add(1)
					continue
				}
				f, ok := parse.ParseFighterPage(doc)
				if !ok {
					log.Printf("refresh parse failed %q", name)
					stats.failed.Add(1)
					continue
				}
				if err := db.UpsertFighter(f); err != nil {
					log.Printf("refresh upsert failed %q: %v", f.Name, err)
					stats.failed.Add(1)
					continue
				}
				stats.updated.Add(1)
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
		go func(i int, url string) {
			defer wg.Done()
			defer func() { <-sem }()

			doc, err := client.GetDoc(ctx, url)
			if err != nil {
				log.Printf("fight fetch failed %s: %v", url, err)
				return
			}
			fight, parsed := parse.ParseFightPage(doc, ep.Date)
			if !parsed {
				log.Printf("fight parse failed %s", url)
				return
			}
			results[i] = fight
			ok[i] = true
		}(i, fightURL)
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
