# MMA Stats Pipeline

A standalone two-component data pipeline for UFC fight statistics. A fast,
concurrent **Go scraper** pulls fighter, event, and fight data from
[ufcstats.com](http://ufcstats.com) into a local SQLite database, and a
**Python unsupervised-ML** component reads that database to discover fighter
**archetypes** (clustering) and **stat relationships** (correlations +
association rules).

There is no *hosted* web application. The pipeline is these two components plus a
shared SQLite database on disk; an optional **local** Streamlit viewer (`viewer/`)
lets you browse the records on demand at `localhost` (nothing is deployed).

There is also a **terminal UI** (`tui-rs/`) that ties the whole pipeline together:
launch it and, from one screen, scrape, train/load the fight-outcome model, search
fighters, and run predictions — all driven by a long-lived Python sidecar so the
ML model loads once.

## Quick start

From the repo root:

```sh
./mma          # launch the TUI (one-time optimized build on first run)
# or, equivalently:
make run       # same thing
make help      # list every root task
```

Other root tasks (all run from here, no `cd` needed):

```sh
make build     # optimized TUI binary + Go scraper binary (TUI auto-detects it)
make test      # run ALL suites: Rust (cargo) + Python (pytest) + Go (go test)
make e2e       # end-to-end tests: PTY suite + tmux smoke (hermetic, offline)
make train     # train / retrain the fight-outcome predictor
```

Prerequisites: Rust (`cargo`), Go, and the Python `.venv` (the TUI uses `.venv/bin/python`
for its ML sidecar). The TUI itself is the control center — you should not need to run the
Go scraper or Python ML by hand.

## Architecture

```
                                  data/ufc.db
  ┌──────────────┐   scrape    ┌──────────────┐   read-only   ┌──────────────┐
  │ ufcstats.com │ ──────────▶ │  Go scraper  │ ────────────▶ │  Python ML   │
  │  (HTML pages)│             │ (scraper-go/)│   SQLite DB   │    (ml/)     │
  └──────────────┘             └──────┬───────┘               └──────┬───────┘
                                      │ writes                       │ produces
                                      ▼                              ▼
                               ┌──────────────┐              ┌───────────────────┐
                               │  data/ufc.db │              │ CSV + PNG outputs │
                               │   (SQLite)   │              │   + Jupyter nb    │
                               └──────────────┘              └───────────────────┘
```

- The Go scraper is the **sole writer** of `data/ufc.db`.
- The Python ML component opens the database **read-only** and never mutates it.
- The database schema is the contract between the two halves — see
  [docs/SCHEMA_CONTRACT.md](docs/SCHEMA_CONTRACT.md).

## Quickstart

### 1. Build and run the Go scraper to populate `data/ufc.db`

Requires Go 1.26+ (pure-Go SQLite — no gcc/CGO needed).

```sh
cd scraper-go
go build ./...
go run . --db ../data/ufc.db
```

This fetches the fighter index and completed-events listing, then writes
`fighters`, `events`, `fights`, and `round_stats` into `data/ufc.db` (the parent
directory is created automatically). Useful flags include `--letter a` (scope to
one starting letter), `--full` (ignore incremental skip sets), `--limit N` (cap
events saved), `--concurrency N`, and `--rate N` (aggregate requests/sec). See
[scraper-go/README.md](scraper-go/README.md) for the full flag list.

### 2. Install the ML dependencies and run the analysis

Requires Python 3 (pandas, numpy, scikit-learn, matplotlib, etc.).

```sh
cd ml
pip install -r requirements.txt
python run_all.py --db ../data/ufc.db --outdir ./outputs
```

This builds the per-fighter feature matrix, runs both analyses, and writes the
artifacts into `ml/outputs/` (`fighter_clusters.csv`, `cluster_profiles.csv`,
`correlations.csv`, `association_rules.csv`, plus PNG charts). To explore the
same flow interactively, run `jupyter notebook notebook.ipynb` from the `ml/`
directory. See [ml/README.md](ml/README.md) for details.

### 3. (Optional) Browse the records in a local GUI

```sh
pip install -r viewer/requirements.txt
streamlit run viewer/app.py
```

Opens a **local** Streamlit app at `http://localhost:8501` (not hosted) to browse
fighters (with fight history + round-by-round stats), events, fights, and the ML
archetypes/charts. See [viewer/README.md](viewer/README.md).

## Project layout

```
mma/
├── scraper-go/                 Go scraper (writes data/ufc.db)
│   ├── main.go                 CLI entry point + concurrent orchestration
│   ├── internal/               fetch, parse, model, and store packages
│   └── README.md               build, run, and flag reference
├── ml/                         Python unsupervised-ML component (read-only)
│   ├── db.py                   loaders + feature engineering
│   ├── archetypes.py           clustering (PCA, KMeans, hierarchical)
│   ├── relationships.py        correlations + association-rule mining
│   ├── run_all.py              CLI that runs both analyses
│   ├── notebook.ipynb          interactive walkthrough
│   ├── outputs/                generated CSV + PNG artifacts
│   ├── requirements.txt        Python dependencies
│   └── README.md               install and run reference
├── viewer/                     Local Streamlit GUI to browse records (read-only)
│   ├── app.py                  the viewer app (streamlit run viewer/app.py)
│   └── README.md               install and run reference
├── data/                       SQLite database (ufc.db) created at runtime
└── docs/
    └── SCHEMA_CONTRACT.md       authoritative DB schema + value conventions
```

## Documentation

- [scraper-go/README.md](scraper-go/README.md) — Go scraper: build, run, flags.
- [ml/README.md](ml/README.md) — Python ML: install, run, outputs.
- [viewer/README.md](viewer/README.md) — local Streamlit record viewer.
- [docs/SCHEMA_CONTRACT.md](docs/SCHEMA_CONTRACT.md) — the shared SQLite schema
  and value conventions both components rely on.

## Legacy

The old Django web application has been **moved out of this project** to the sibling
folder `../mma_legacy_django` (and is also on GitHub). It is fully superseded by this
pipeline — nothing here depends on it.
