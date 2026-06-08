# ufcscraper (`scraper-go/`)

A fast, concurrent Go scraper for [ufcstats.com](http://www.ufcstats.com). It is a 1:1
behavioral port of the original Python/Django scraper (now archived outside the project),
rewritten for throughput. It scrapes fighters, events, fights, and per-round statistics and
writes them to a local SQLite database (`data/ufc.db`).

## Pipeline role

This is the **write** half of a two-part pipeline: the Go scraper is the **sole writer** of
`data/ufc.db`, which the Python ML component (`ml/`) then reads **read-only** for archetype
clustering and analysis. The DB schema and value conventions are pinned by
[`docs/SCHEMA_CONTRACT.md`](../docs/SCHEMA_CONTRACT.md).

## Why it's fast

The old Python scraper slept 1 second after **every** page fetch (fully serial). This version
replaces that with a token-bucket rate limiter that caps the *aggregate* request rate
(`--rate`) while a worker pool (`--concurrency`) keeps many requests in flight at once, so
throughput approaches `--rate` req/s instead of one page per second. A single writer goroutine
owns the database (WAL mode), so parse parallelism never causes write contention; each event
(event + its fights + all round stats) is committed in one transaction.

## Anti-bot challenge handling

ufcstats.com now gates every page behind a lightweight JavaScript "proof of work" interstitial
("Checking your browser…"): the page ships a `nonce` and a difficulty, and a browser must
brute-force an `n` such that `sha256(nonce + ":" + n)` begins with N hex zeros, POST `{nonce, n}`
to `/__c` to obtain a clearance cookie, then reload. **A plain HTTP client — including the original
Python `requests` scraper — only ever receives the ~3 KB stub**, which is why the old scraper
silently stopped returning data.

`internal/fetch` performs that same computation transparently (it runs the site's *own published*
algorithm; no authentication or paywall is involved): on detecting the interstitial it solves the
proof of work, POSTs to `/__c`, stores the clearance cookie in a shared cookie jar, and retries the
request. A mutex serializes solving so a burst of concurrent workers only solves it once; the cookie
is then reused for every subsequent request. The hermetic test `internal/fetch/fetch_test.go`
exercises the whole solve flow against a local server — reproduce any future challenge change there.
If the site alters the challenge (difficulty, endpoint, or scheme), update `isChallenge` /
`solveChallenge` in `internal/fetch/fetch.go`.

## Politeness & throttling

The site enforces a server-side rate limit and returns `429 Too Many Requests` to aggressive
bursts. To stay under it the limiter uses a **burst of 1** (requests are spaced smoothly at `--rate`
rather than fired in a clump), and transient `429`/`503` responses are **retried with exponential
backoff** (honoring a numeric `Retry-After`). If you see sustained 429s on a large run, lower
`--rate` (e.g. `--rate 5`) — the data still lands, just more slowly.

## Prerequisites

- **Go 1.26+**. That is the only requirement.
- **No GCC / no CGO.** SQLite is the pure-Go `modernc.org/sqlite` driver. A
  `CGO_ENABLED=0` build is verified to work.

Dependencies (`goquery`, `golang.org/x/time`, `modernc.org/sqlite`) are pinned in `go.mod`
and fetched automatically by the Go toolchain.

## Build

Run from the `scraper-go/` directory:

```sh
go build ./...                      # compile everything
go test ./...                       # run the unit tests (offline, in-code HTML fixtures)
go build -o ufcscraper .            # produce a standalone binary
```

## Run

```sh
go run .                            # default full pipeline: fighters then events
go run . --help                     # list all flags and defaults
./ufcscraper --limit 5             # if you built the binary
```

By default the pipeline runs three stages, **incrementally — it only scrapes what is new or
changed**, so after the first run you never need a full rescrape:

1. **Fighters.** All 26 letter index pages are read (cheap); fighters already in the DB are
   **skipped**, so only *new* fighters are fetched and inserted.
2. **Events.** The completed-events listing is walked **newest-first** and the scrape **stops at
   the first event already stored** — so only events newer than your latest are fetched, with all
   their fights and round-by-round stats.
3. **Refresh.** The fighters who competed in those new events have changed records / career
   averages, so *just those* fighters are **re-fetched and updated** (their names are mapped back
   to detail-page URLs via the index from stage 1). Disable with `--no-refresh`.

Use `--full` to ignore every skip set and re-fetch *everything*. The first ever run is effectively
a full scrape because the DB starts empty.

The scraper is **fail-soft**: a bad page is logged and skipped, never fatal. `Ctrl-C` /
`SIGTERM` cancels cleanly, flushing the writer and closing the DB.

### Flags

| Flag | Type | Default | Description |
|---|---|---|---|
| `--letter` | string | `""` (all 26, `a`–`z`) | Scrape only this single letter's fighters. |
| `--full` | bool | `false` | Full re-scrape: ignore all incremental skip sets and re-fetch everything. |
| `--limit` | int | `0` (unlimited) | Max number of events to save. |
| `--fighters-only` | bool | `false` | Run only the fighters stage (no events, no refresh). |
| `--events-only` | bool | `false` | Run only the events stage (no fighters, no refresh). |
| `--no-refresh` | bool | `false` | Skip stage 3 (don't re-fetch fighters who appear in newly scraped events). |
| `--concurrency` | int | `16` | Number of concurrent fetch/parse workers. |
| `--rate` | float | `10` | Aggregate request rate limit (requests/second). |
| `--db` | string | `../data/ufc.db` | Path to the SQLite database file (parent dir auto-created). |

### Example invocations

```sh
# Full pipeline at default rate, writing to ../data/ufc.db
go run .

# Quick smoke test: just the 5 most recent events
go run . --limit 5

# Refresh only fighters whose names start with "j"
go run . --fighters-only --letter j

# Re-scrape only events (skip fighters), forcing a full pass
go run . --events-only --full

# Crank throughput: 32 workers at 20 req/s, custom DB path
go run . --concurrency 32 --rate 20 --db ./ufc.db
```

## What it produces

A single SQLite file (default `../data/ufc.db`) with the schema from
[`docs/SCHEMA_CONTRACT.md`](../docs/SCHEMA_CONTRACT.md), applied idempotently on open. Four
tables are written:

| Table | One row per | Notes |
|---|---|---|
| `fighters` | fighter | Upserted by unique `name`. Bio + record + eight career averages (`slpm`…`sub_avg`) inlined. A re-scrape never clobbers `was_champion` / `championship_bouts_won`. |
| `events` | event | Unique by `title`; has `date` and `location`. |
| `fights` | bout | Denormalized `event_name` / `date` plus `winner_name` / `loser_name`, `weight_class`, `title_bout`, `method`, round/time ended, referee. |
| `round_stats` | fight × fighter × round | Wide row of strike/takedown/control stats, plus `result` (`w`/`l`/`d`). |

### Value conventions (preserved from the original scraper)

- Percentages stored as **0..1 fractions** (`57%` → `0.57`); strike/takedown pct = `landed / attempted`.
- Height/reach in **inches**, weight in **lbs**, control/finish times in **seconds**.
- Dates and DOB as ISO text `YYYY-MM-DD`.
- `title_bout` is `0`/`1`; missing strike triples are `0/0/0.0`.
- Nullable numerics (`height_in`, `reach_in`, `weight_lbs`, `slpm`…`sub_avg`) are stored as
  real `NULL` when the source shows a `--`/`---` placeholder — distinct from `0`.

## Project layout

```
scraper-go/
├── main.go                 # CLI flags + two-stage orchestration
├── internal/
│   ├── model/              # shared structs (Fighter, Event, Fight, RoundStat)
│   ├── parse/              # goquery page parsers (1:1 port of parsers.py) + tests
│   ├── fetch/              # rate-limited HTTP client -> goquery.Document
│   └── store/              # SQLite schema + single-writer persistence + tests
```
