# Hermetic E2E fixtures for `mma-tui`

These stubs let the `mma-tui` binary be driven through a pseudo-terminal
OFFLINE and DETERMINISTICALLY — no real model, no sklearn, no network, no DB
writes. They are wired in entirely through env-var overrides resolved in
`src/config.rs`; the product code paths are unchanged, the stubs just replace
the heavy external processes.

## Files

| File              | Replaces             | Pointed at by  |
| ----------------- | -------------------- | -------------- |
| `stub_sidecar.py` | `ml/serve.py`        | `MMA_SIDECAR`  |
| `stub_scraper.sh` | the Go scraper       | `MMA_SCRAPER`  |

Both are committed executable (`chmod +x`). Use ABSOLUTE paths in the env vars.

## Env overrides (all optional; unset == current/real behavior)

| Var          | Effect                                                                                         |
| ------------ | --------------------------------------------------------------------------------------------- |
| `MMA_DB`     | Use this path VERBATIM as the read-only SQLite DB (skips the walk-up search for `data/ufc.db`).|
| `MMA_SIDECAR`| Launch this SINGLE executable as the IPC sidecar — **no `serve.py` arg** (instead of `python ml/serve.py`). Must speak the JSON-lines protocol. |
| `MMA_SCRAPER`| Launch this SINGLE executable as the scraper. The usual flags (`--full` / `--limit N` / `--rate R`) are still appended; the stub ignores them. |
| `MMA_PYTHON` | Still honored: the Python interpreter for the default sidecar / training when `MMA_SIDECAR` is unset. |

## Pointing the TUI at the stubs

```sh
FIX="$(git rev-parse --show-toplevel)/tui-rs/tests/fixtures"
export TERM=xterm-256color
export MMA_SIDECAR="$FIX/stub_sidecar.py"
export MMA_SCRAPER="$FIX/stub_scraper.sh"
# MMA_DB optional: a tiny seeded sqlite file works; the real DB is fine too.
# export MMA_DB="$FIX/mini.db"

cargo run --manifest-path "$(git rev-parse --show-toplevel)/tui-rs/Cargo.toml"
```

Drive it through a PTY (45 rows x 140 cols), send key encodings, then POLL the
parsed screen for expected text (~5s timeout) before asserting; allow the first
frame to render. Key encodings: Enter=`\r`, Esc=`\x1b`, Up=`\x1b[A`,
Down=`\x1b[B`, Left=`\x1b[D`, Right=`\x1b[C`, Tab=`\t`, Ctrl-C=`\x03`.

## Canned data (assert against these)

### `stub_sidecar.py`

Roster (sorted, 3 fighters):

```
["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]
```

- `ping`   -> `{"ok": true}`
- `status` -> `model_loaded: true`, `n_fighters: 3`,
  `metrics: {"test_accuracy": 0.6}`, `model_path: "<stub>/predictor.joblib"`
- `roster` -> the 3 names above
- `reload` -> `model_loaded: true`, `n_fighters: 3`
- `predict` (any two of the roster) -> deterministic ALLOWED result:
  - `prob_a: 0.62`, `prob_b: 0.38`
  - `allowed: true`, `reason: null`, `low_confidence: false`, `distance: 0`
  - `model: "stub"`, `test_accuracy: 0.6`
  - full `tale_a` / `tale_b` (every key: `elo, age, record, reach_in,
    height_in, stance, recent_winrate, form_delta, layoff_days, divisions`)
  - `tale_a`: elo 1650, age 34, record "24-3", reach_in 80, height_in 76,
    stance Orthodox, recent_winrate 0.8, form_delta 0.2, layoff_days 180,
    divisions ["Middleweight", "Light Heavyweight"]
  - `tale_b`: elo 1580, age 33, record "25-7", reach_in 73.5, height_in 72,
    stance Orthodox, recent_winrate 0.6, form_delta -0.1, layoff_days 300,
    divisions ["Middleweight"]

The stub never crashes on bad input (returns `{"ok": false, "error": ...}`),
flushes after every line, and has no heavy imports so it starts instantly.

### `stub_scraper.sh`

Prints exactly these 5 lines to stdout, then exits 0 (ignores all args):

```
scanning events...
saved event 1/3
saved event 2/3
saved event 3/3
done
```
