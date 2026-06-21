# mma-tui

A terminal UI (TUI) for the local UFC stats pipeline. It is the single thing you
launch: it reads `data/ufc.db` read-only, talks to a long-lived Python ML sidecar
for predictions, and can run the Go scraper on demand.

## Run

```sh
cd /Users/jibi/Documents/Personal_Projects/mma-stats-pipeline/tui-rs
cargo run
```

(The first build compiles `ratatui` and a bundled SQLite, so it is slow; later
builds are fast.)

## What it does

- **Reads** `data/ufc.db` read-only via `rusqlite` — never writes it. The Go
  scraper in `scraper-go/` is the sole writer.
- **Spawns** the Python sidecar `ml/serve.py` (loads the model once) and asks it
  for the roster, model status, and fight predictions over JSON lines. All ML
  math lives in Python; the TUI never reimplements it.
- **Runs** the Go scraper on demand and streams its progress into a log pane.

## Configuration

Resolved automatically at startup (see `src/config.rs`):

- **Repo root** — the nearest ancestor directory containing `data/ufc.db`.
- **Python** — `$MMA_PYTHON` if set, else `<repo>/.venv/bin/python` if it exists,
  else `python3`.
- **Sidecar** — `<repo>/ml/serve.py`.
- **Scraper** — a prebuilt binary in `scraper-go/` if present, else `go run .`
  there.

If no model has been trained yet, the sidecar stays up and the TUI offers to
train one (training writes `ml/models/predictor.joblib`).

## Layout

| Path                | Role                                                        |
|---------------------|-------------------------------------------------------------|
| `src/models.rs`     | Frozen shared types (DB rows, predict result, IPC types).   |
| `src/config.rs`     | Path/interpreter resolution.                                |
| `src/db.rs`         | Read-only SQLite queries → `models` structs.                |
| `src/sidecar.rs`    | Client for the Python ML sidecar (JSON lines).              |
| `src/scraper.rs`    | Spawns + streams the Go scraper.                            |
| `src/stats_text.rs` | Plain-English stat explanations (the "layman layer").       |
| `src/fuzzy.rs`      | Fuzzy fighter-name narrowing.                               |
| `src/app.rs`        | App state + screens + update/transition logic.              |
| `src/ui/`           | Per-screen renderers + top-level `draw`.                    |
| `src/main.rs`       | Terminal setup + event loop.                                |

See [`CONTRACT.md`](./CONTRACT.md) for the frozen IPC protocol and Rust
signatures the build agents implement against.
