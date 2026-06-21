#!/usr/bin/env bash
# =============================================================================
# tui_smoke.sh — black-box SMOKE test for the MMA TUI (mma-tui) — REDESIGN.
#
# WHAT THIS IS
#   A language-agnostic, end-to-end "drive it in a subshell and assert" harness.
#   The TUI is PURELY INTERACTIVE (no CLI args), so we drive it through a real
#   pseudo-terminal using tmux: `send-keys` to feed input, `capture-pane` to read
#   the rendered screen, then `grep` to assert on what the user would see.
#
# This drives the REDESIGNED UX (see the locked spec), NOT the old one:
#   * HOME plays a one-shot "MMA" block-letter intro framed as a fight poster
#     headlining the latest numbered UFC card, then a vertical 4-item MENU:
#     Scrape / Database / Predict / Model.
#   * NAVIGATION is a SCREEN STACK with NO hotkey jumps: on a menu, Up/Down move
#     the selection and Enter pushes the chosen screen; Esc / Backspace pop one
#     level; the Home key clears back to the home menu; q quits. A persistent
#     FOOTER shows the contextual controls.
#   * DATABASE is a hub with two paths: "Browse events" and "Find a fighter".
#   * Scrape runs ASYNC on a background thread and streams into a loading overlay
#     (ASCII fighters + a braille spinner + a progress bar + the live log) while
#     the event loop keeps ticking — it never freezes.
#
# HOW IT STAYS HERMETIC / OFFLINE
#   * A tiny fixture SQLite DB is built in a tempdir via the sqlite3 CLI (the 4
#     tables from docs/SCHEMA_CONTRACT.md, seeded with known fighters incl.
#     "Israel Adesanya", "Robert Whittaker", "Alex Pereira" and one NUMBERED card
#     "UFC 281"), exported as MMA_DB.
#   * The Python ML sidecar is replaced by the deterministic stub at
#     tui-rs/tests/fixtures/stub_sidecar.py (MMA_SIDECAR) — no sklearn / model /
#     network. Roster is exactly: Alex Pereira, Israel Adesanya, Robert Whittaker.
#   * The Go scraper is replaced by tui-rs/tests/fixtures/stub_scraper.sh
#     (MMA_SCRAPER) — prints fixed progress lines (with a small sleep between
#     each so streaming is observable) and exits 0.
#   If the sqlite3 CLI is unavailable, it falls back to the real data/ufc.db
#   (read-only) so the harness still runs.
#
# USAGE
#   scripts/tui_smoke.sh            # build + run the smoke test
#   On success: prints "SMOKE OK" and exits 0.
#   On failure: prints "FAIL: ..." plus the captured screen, exits non-zero.
#
# REQUIREMENTS
#   tmux, /opt/homebrew/bin/cargo, and (preferably) the sqlite3 CLI on PATH.
#
# STEPS / ASSERTIONS
#   (1) Intro + Home menu  -> assert the "mma-tui" brand, the poster ("UFC 281",
#                             "MAIN CARD"), and the 4 menu options; assert the
#                             highlight "▶ " moves Scrape -> Database with Down.
#   (2) Database paths     -> Down/Enter into Database; "Browse events" opens the
#                             card (assert "UFC 281"); Esc back; "Find a fighter"
#                             narrows on "adesa" to "Israel Adesanya" and the
#                             profile shows a layman stat phrase.
#   (3) Predict full run   -> Home, Down x2, Enter into Predict; commit Alex
#                             Pereira (Enter) then Israel Adesanya (Down, Enter);
#                             assert the OUTPUT: win % (62%/38%), the Win
#                             probability pane, and a tale row with record "24-3".
#   (4) Scrape no-freeze   -> Home, Enter into Scrape; assert "Full: OFF"; press
#                             f -> "Full: ON"; Enter to run; assert the running
#                             overlay streams "saved event 1/3" AND the animation
#                             frame CHANGES between two captures (loop not frozen)
#                             AND a completion line "Scraping finished OK".
#   (5) Quit               -> Home, q, assert the tmux session is gone.
# =============================================================================

set -euo pipefail

# --------------------------------------------------------------------------- #
# Constants / paths.
# --------------------------------------------------------------------------- #
REPO="/Users/jibi/Documents/Personal_Projects/mma-stats-pipeline"
TUI_DIR="$REPO/tui-rs"
CARGO="/opt/homebrew/bin/cargo"
BIN="$TUI_DIR/target/debug/mma-tui"
FIX="$TUI_DIR/tests/fixtures"
STUB_SIDECAR="$FIX/stub_sidecar.py"
STUB_SCRAPER="$FIX/stub_scraper.sh"

SES="mma_tui_smoke_$$"          # unique tmux session name (PID-tagged)
TMPDIR_SMOKE=""                 # filled in below; cleaned by trap
DB_PATH=""                      # fixture DB path; cleaned by trap

# --------------------------------------------------------------------------- #
# Cleanup: always kill the tmux session and remove the tempdir.
# --------------------------------------------------------------------------- #
cleanup() {
    tmux kill-session -t "$SES" 2>/dev/null || true
    [ -n "${TMPDIR_SMOKE:-}" ] && rm -rf "$TMPDIR_SMOKE" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# --------------------------------------------------------------------------- #
# Helpers.
# --------------------------------------------------------------------------- #

# send KEYS... : forward key(s) to the TUI, then let the event loop redraw.
send() {
    tmux send-keys -t "$SES" "$@"
    sleep 0.4
}

# screen : dump the current pane as plain text (what the user sees).
screen() {
    tmux capture-pane -pt "$SES"
}

# fail MSG : print the message + the captured screen, then exit non-zero.
# (cleanup runs via the EXIT trap.)
fail() {
    echo "FAIL: $1"
    echo "----- captured screen -----"
    screen 2>/dev/null || echo "(no pane — session already gone)"
    echo "---------------------------"
    exit 1
}

# assert_contains TEXT : current screen must contain TEXT (fixed-string).
assert_contains() {
    screen | grep -qF "$1" || fail "missing '$1'"
}

# assert_not_contains TEXT : current screen must NOT contain TEXT (fixed-string).
assert_not_contains() {
    if screen | grep -qF "$1"; then
        fail "unexpectedly present: '$1'"
    fi
}

# overlay_fighters : dump just the loading overlay's two-fighter animation body
# (the rows of the "Scraping" panel, before the Status/Progress/Output panels),
# with box-drawing frame chars stripped, so two captures differ ONLY when the
# pose actually changes. Used to prove the event loop keeps ticking (no freeze).
overlay_fighters() {
    screen | awk '
        /Scraping/ { grab=1; next }
        grab && (/Status/ || /Progress/ || /Output/) { exit }
        grab {
            gsub(/[│┌┐└┘─]/, "")
            gsub(/^[ \t]+|[ \t]+$/, "")
            if (length($0) > 0) print
        }
    '
}

# wait_for TEXT : POLL the parsed screen for TEXT, up to ~6s, before asserting.
# Lets the first frame render and absorbs IPC / redraw latency.
wait_for() {
    local needle="$1"
    local i
    for i in $(seq 1 30); do
        if screen | grep -qF "$needle"; then
            return 0
        fi
        sleep 0.2
    done
    fail "timed out waiting for '$needle'"
}

# --------------------------------------------------------------------------- #
# 0. Preconditions.
# --------------------------------------------------------------------------- #
command -v tmux >/dev/null 2>&1 || { echo "FAIL: tmux not found on PATH"; exit 1; }
[ -x "$CARGO" ] || { echo "FAIL: cargo not found at $CARGO"; exit 1; }
[ -f "$STUB_SIDECAR" ] || { echo "FAIL: stub sidecar missing: $STUB_SIDECAR"; exit 1; }
[ -x "$STUB_SCRAPER" ] || { echo "FAIL: stub scraper missing/not executable: $STUB_SCRAPER"; exit 1; }

# --------------------------------------------------------------------------- #
# 1. Build the binary.
# --------------------------------------------------------------------------- #
echo "[smoke] building mma-tui ..."
( cd "$TUI_DIR" && "$CARGO" build ) || { echo "FAIL: cargo build failed"; exit 1; }
[ -x "$BIN" ] || { echo "FAIL: built binary not found at $BIN"; exit 1; }

# --------------------------------------------------------------------------- #
# 2. Build the hermetic fixture DB (4 tables per SCHEMA_CONTRACT, seeded).
#    Fall back to the real read-only DB if the sqlite3 CLI is unavailable.
# --------------------------------------------------------------------------- #
TMPDIR_SMOKE="$(mktemp -d "${TMPDIR:-/tmp}/mma_tui_smoke.XXXXXX")"

if command -v sqlite3 >/dev/null 2>&1; then
    DB_PATH="$TMPDIR_SMOKE/fixture.db"
    echo "[smoke] seeding fixture DB at $DB_PATH ..."
    sqlite3 "$DB_PATH" <<'SQL'
PRAGMA foreign_keys = ON;

CREATE TABLE fighters (
    fighter_id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name                    TEXT NOT NULL UNIQUE,
    nickname                TEXT,
    nationality             TEXT DEFAULT 'Unlisted',
    height_in               INTEGER,
    weight_lbs              INTEGER,
    reach_in                INTEGER,
    stance                  TEXT,
    date_of_birth           TEXT,
    wins                    INTEGER NOT NULL DEFAULT 0,
    losses                  INTEGER NOT NULL DEFAULT 0,
    draws                   INTEGER NOT NULL DEFAULT 0,
    no_contests             INTEGER NOT NULL DEFAULT 0,
    was_champion            INTEGER NOT NULL DEFAULT 0,
    championship_bouts_won  INTEGER NOT NULL DEFAULT 0,
    slpm                    REAL,
    str_acc                 REAL,
    sapm                    REAL,
    str_def                 REAL,
    td_avg                  REAL,
    td_acc                  REAL,
    td_def                  REAL,
    sub_avg                 REAL
);

CREATE TABLE events (
    event_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT NOT NULL UNIQUE,
    date       TEXT,
    location   TEXT
);

CREATE TABLE fights (
    fight_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id      INTEGER REFERENCES events(event_id) ON DELETE CASCADE,
    event_name    TEXT,
    date          TEXT,
    winner_name   TEXT,
    loser_name    TEXT,
    weight_class  TEXT,
    title_bout    INTEGER NOT NULL DEFAULT 0,
    method        TEXT,
    round_ended   INTEGER NOT NULL DEFAULT 0,
    time_ended    INTEGER NOT NULL DEFAULT 0,
    referee       TEXT
);

CREATE TABLE round_stats (
    round_stat_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    fight_id        INTEGER REFERENCES fights(fight_id) ON DELETE CASCADE,
    fighter_name    TEXT,
    result          TEXT,
    round_number    INTEGER,
    knockdowns      INTEGER NOT NULL DEFAULT 0,
    sub_attempts    INTEGER NOT NULL DEFAULT 0,
    reversals       INTEGER NOT NULL DEFAULT 0,
    control_time    INTEGER NOT NULL DEFAULT 0,
    td_landed       INTEGER NOT NULL DEFAULT 0,
    td_attempted    INTEGER NOT NULL DEFAULT 0,
    td_pct          REAL    NOT NULL DEFAULT 0.0,
    sig_str_landed      INTEGER NOT NULL DEFAULT 0,
    sig_str_attempted   INTEGER NOT NULL DEFAULT 0,
    sig_str_pct         REAL    NOT NULL DEFAULT 0.0,
    total_str_landed    INTEGER NOT NULL DEFAULT 0,
    total_str_attempted INTEGER NOT NULL DEFAULT 0,
    total_str_pct       REAL    NOT NULL DEFAULT 0.0,
    head_landed     INTEGER NOT NULL DEFAULT 0, head_attempted     INTEGER NOT NULL DEFAULT 0, head_pct     REAL NOT NULL DEFAULT 0.0,
    body_landed     INTEGER NOT NULL DEFAULT 0, body_attempted     INTEGER NOT NULL DEFAULT 0, body_pct     REAL NOT NULL DEFAULT 0.0,
    leg_landed      INTEGER NOT NULL DEFAULT 0, leg_attempted      INTEGER NOT NULL DEFAULT 0, leg_pct      REAL NOT NULL DEFAULT 0.0,
    distance_landed INTEGER NOT NULL DEFAULT 0, distance_attempted INTEGER NOT NULL DEFAULT 0, distance_pct REAL NOT NULL DEFAULT 0.0,
    clinch_landed   INTEGER NOT NULL DEFAULT 0, clinch_attempted   INTEGER NOT NULL DEFAULT 0, clinch_pct   REAL NOT NULL DEFAULT 0.0,
    ground_landed   INTEGER NOT NULL DEFAULT 0, ground_attempted   INTEGER NOT NULL DEFAULT 0, ground_pct   REAL NOT NULL DEFAULT 0.0
);

-- Known fighters (match the stub sidecar roster so both screens agree). Adesanya
-- gets full career stats so his profile renders real numbers + layman phrases
-- (the str_acc explanation "...more precise punching and kicking" is asserted).
INSERT INTO fighters (name, nickname, nationality, height_in, weight_lbs, reach_in, stance, wins, losses, was_champion, championship_bouts_won, slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def, sub_avg)
VALUES
    ('Israel Adesanya', 'The Last Stylebender', 'Nigeria', 76, 185, 80, 'Switch',     24, 3, 1, 5, 3.93, 0.49, 3.06, 0.61, 0.07, 0.20, 0.71, 0.1),
    ('Robert Whittaker', 'The Reaper',          'Australia', 72, 185, 73, 'Orthodox', 25, 7, 1, 2, 4.46, 0.39, 3.30, 0.57, 0.55, 0.36, 0.69, 0.2),
    ('Alex Pereira',    'Poatan',               'Brazil',  76, 205, 79, 'Orthodox',   12, 2, 1, 5, 5.10, 0.59, 3.00, 0.50, 0.10, 0.40, 0.60, 0.1);

-- A NUMBERED card (so the home poster headlines "UFC 281") + fight + rounds so
-- the Browse-events path and the per-round panes are non-empty.
INSERT INTO events (title, date, location)
VALUES ('UFC 281', '2022-11-12', 'New York, USA');

INSERT INTO fights (event_id, event_name, date, winner_name, loser_name, weight_class, method, round_ended, time_ended)
VALUES (1, 'UFC 281', '2022-11-12', 'Alex Pereira', 'Israel Adesanya', 'Middleweight', 'TKO', 5, 124);

INSERT INTO round_stats (fight_id, fighter_name, result, round_number)
VALUES (1, 'Alex Pereira', 'w', 5),
       (1, 'Israel Adesanya', 'l', 5);
SQL
    [ -f "$DB_PATH" ] || { echo "FAIL: could not create fixture DB"; exit 1; }
else
    echo "[smoke] sqlite3 CLI not found — falling back to real data/ufc.db (read-only)."
    DB_PATH="$REPO/data/ufc.db"
    [ -f "$DB_PATH" ] || { echo "FAIL: real DB not found at $DB_PATH"; exit 1; }
fi

# --------------------------------------------------------------------------- #
# 3. Launch the TUI in a DETACHED tmux session, fixed 140x45 window.
#    Env overrides make it fully hermetic. We launch the prebuilt binary via a
#    login shell so the user profile (PATH etc.) is available.
# --------------------------------------------------------------------------- #
echo "[smoke] launching TUI in detached tmux session '$SES' ..."

# Make sure any stale session with this name is gone first.
tmux kill-session -t "$SES" 2>/dev/null || true

TERM=xterm-256color \
MMA_DB="$DB_PATH" \
MMA_SIDECAR="$STUB_SIDECAR" \
MMA_SCRAPER="$STUB_SCRAPER" \
tmux new-session -d -s "$SES" -x 140 -y 45 \
    "TERM=xterm-256color MMA_DB='$DB_PATH' MMA_SIDECAR='$STUB_SIDECAR' MMA_SCRAPER='$STUB_SCRAPER' '$BIN'"

# Give the alt-screen event loop a moment to draw the first frame.
sleep 1.0

# --------------------------------------------------------------------------- #
# STEP 1: Intro + Home menu render; the selection highlight moves with Down.
# --------------------------------------------------------------------------- #
echo "[smoke] step 1: intro + home menu render"
wait_for "mma-tui"
assert_contains "mma-tui"
# The fight poster headlines the latest numbered card + the 4-option menu.
wait_for "MAIN CARD"
assert_contains "MAIN CARD"
wait_for "UFC 281"
assert_contains "UFC 281"
for opt in Scrape Database Predict Model; do
    assert_contains "$opt"
done
# The home footer is unique to the root menu (no Esc Back).
assert_contains "⏎ select · q Quit"
# Highlight starts on Scrape, then moves to Database with Down.
wait_for "▶ Scrape"
assert_contains "▶ Scrape"
send Down
wait_for "▶ Database"
assert_contains "▶ Database"
assert_not_contains "▶ Scrape"
# Back up to Scrape so we leave the menu where we found it.
send Up
wait_for "▶ Scrape"

# --------------------------------------------------------------------------- #
# STEP 2: Database hub — BOTH paths.
#
#   2a. Browse events: Down/Enter into Database, Enter on "Browse events", Enter
#       to open the seeded card, assert the fight row renders.
#   2b. Find a fighter: Esc back to the hub, Down/Enter into "Find a fighter",
#       type "adesa" to narrow to Adesanya, Enter to open his profile, assert a
#       layman stat phrase.
# --------------------------------------------------------------------------- #
echo "[smoke] step 2a: database -> browse events"
send Down            # select Database
send Enter           # push Database hub
wait_for "Choose a path"
assert_contains "Browse events"
assert_contains "Find a fighter"
send Enter           # "Browse events" is the first option (already selected)
wait_for "Home › Database › Events"
assert_contains "UFC 281"
send Enter           # open the highlighted event's card
wait_for "Home › Database › Events › Fight card"
wait_for "Alex Pereira def. Israel Adesanya"
assert_contains "Alex Pereira def. Israel Adesanya"

echo "[smoke] step 2b: database -> find a fighter -> profile"
send Escape          # card -> Events
wait_for "Home › Database › Events"
send Escape          # Events -> Database hub
wait_for "Choose a path"
send Down            # select "Find a fighter"
send Enter           # push the fuzzy search
wait_for "Search"
# Type the query a character at a time (mirrors live narrowing).
send "a"; send "d"; send "e"; send "s"; send "a"
wait_for "Fighters (1)"
assert_contains "Israel Adesanya"
send Enter           # open the profile
wait_for "Career stats"
assert_contains "Striking accuracy"
# Layman phrase straight from stats_text::explain(str_acc).
assert_contains "more precise punching"
assert_contains "Record:"

# --------------------------------------------------------------------------- #
# STEP 3: Predict — Home, then drive a FULL prediction and assert the OUTPUT.
#
# We don't just check the header renders; we run the exact commit flow and
# assert the prediction OUTPUT (a win %, the Win-probability pane, and a
# tale-of-the-tape row with a stub value) so this step would FAIL if the predict
# feature regressed — not merely if the screen opened.
#
# Flow (matches app.rs::on_key_predict, same as the Rust e2e test) — this is also
# the LOCAL-FILTER PROOF: at startup the TUI fetched the eligibility POLICY +
# per-fighter divisions ONCE (stub: max_distance 1; Adesanya/Whittaker M#6,
# Pereira M#8). All slot filtering thereafter is LOCAL — no per-selection IPC.
#   * Home, Down x2 selects Predict; Enter pushes it (focus slot A).
#   * Candidates are the stub roster sorted: ["Alex Pereira", "Israel Adesanya",
#     "Robert Whittaker"], selection 0.
#   * Down once -> index 1 = "Israel Adesanya"; Enter commits A and auto-advances
#     to slot B.
#   * Slot B's pool is now the LOCAL eligible set for Adesanya = ["Robert
#     Whittaker"] — "Alex Pereira" (M#8, distance 2 > max_distance 1) is FILTERED
#     OUT, proving the filter is REAL (not "all-but-A"). The single candidate is at
#     index 0, so Enter commits B = "Robert Whittaker".
#   * With both slots committed the stub prediction fires: prob_a 0.62 /
#     prob_b 0.38 plus a full tale-of-the-tape (tale_a record "24-3"). Because the
#     OTHER slot was pre-filtered LOCALLY, the completed screen carries NONE of the
#     refusal vocabulary (no "refused"/"ineligible"/"not allowed"/"cross-").
# --------------------------------------------------------------------------- #
echo "[smoke] step 3: predict runs and renders a probability + tale"
tmux send-keys -t "$SES" Home    # clear the stack back to the home menu
sleep 0.4
wait_for "⏎ select · q Quit"
send Down            # Scrape -> Database
send Down            # Database -> Predict
send Enter           # push Predict
wait_for "Fighter A"
assert_contains "Fighter A"
assert_contains "Fighter B"
# Candidate list is live (sidecar roster).
wait_for "Alex Pereira"

# Commit slot A = Israel Adesanya (index 1); the committed header shows a check
# mark. Down once then Enter so A is an eligible-having fighter.
send Down
send Enter
wait_for "✓ Israel Adesanya"

# Slot B's pool is now the LOCAL eligible set for Adesanya = ["Robert Whittaker"]
# — "Alex Pereira" (M#8) is dropped (distance 2 > max_distance 1), proving the
# OTHER slot is genuinely filtered by the policy, not "all-but-A". The single
# candidate is at index 0, so Enter commits B = Robert Whittaker.
send Enter
wait_for "✓ Robert Whittaker"

# Prediction fires automatically — assert the rendered OUTPUT, not just the form.
wait_for "62%"
assert_contains "62%"
assert_contains "38%"
wait_for "Win probability"
assert_contains "Win probability"
wait_for "Tale of the tape"
assert_contains "Tale of the tape"
assert_contains "Elo rating"
# Deterministic stub tale value: tale_a record "24-3".
assert_contains "24-3"
# LOCAL-FILTER PROOF (the negative): no refusal vocabulary may appear — an
# ineligible matchup can never reach the predict call, so the defensive "Matchup
# not allowed" branch (and any cross-gender / not-allowed reason) never renders.
assert_not_contains "refused"
assert_not_contains "ineligible"
assert_not_contains "not allowed"
assert_not_contains "cross-"

# --------------------------------------------------------------------------- #
# STEP 4: Scrape — chips + NO-FREEZE streaming + completion.
#
# Home, Enter into Scrape; assert the "Full: OFF" chip and that f flips it ON;
# Enter runs the (stub) scraper. WHILE it runs we prove the loop is NOT frozen:
# a mid-run progress line streams in AND the two-fighter animation frame CHANGES
# between two captures. Finally the app's own completion marker appears.
# --------------------------------------------------------------------------- #
echo "[smoke] step 4: scrape streams without freezing"
tmux send-keys -t "$SES" Home
sleep 0.4
wait_for "⏎ select · q Quit"
send Enter           # Scrape is the first option (already highlighted)
wait_for "Scraper options"
assert_contains "Full: OFF"
send "f"
wait_for "Full: ON"
assert_contains "Full: ON"
send "f"             # flip back OFF (cheap incremental run)
wait_for "Full: OFF"
send Enter           # RUN the async job + loading overlay

# Running state: the overlay footer shows the running hint, and an early progress
# line streams in BEFORE completion (proof of progressive, non-blocking output).
wait_for "running"
assert_contains "running"
wait_for "saved event 1/3"
assert_contains "saved event 1/3"

# NO-FREEZE PROOF: the fighters animation advances between two captures (~0.4s
# apart). The event loop must keep ticking while the background job streams.
FRAME_A="$(overlay_fighters)"
sleep 0.4
FRAME_B="$(overlay_fighters)"
[ -n "$FRAME_A" ] || fail "loading overlay animation panel was empty while running"
if [ "$FRAME_A" = "$FRAME_B" ]; then
    fail "fighters animation did not advance between captures (event loop frozen?)"
fi

# Completion: the app appends "$ Scraping finished OK" after the job exits OK.
wait_for "Scraping finished OK"
assert_contains "Scraping finished OK"
# The finished overlay is dismissable; Enter returns to the Scrape options.
send Enter
wait_for "Scraper options"

# --------------------------------------------------------------------------- #
# STEP 5: Quit — Home, then q; the tmux session/pane must disappear.
# --------------------------------------------------------------------------- #
echo "[smoke] step 5: quit ends the session"
tmux send-keys -t "$SES" Home
sleep 0.4
wait_for "⏎ select · q Quit"
tmux send-keys -t "$SES" "q"

# Poll for the session to vanish (the binary exits, tmux reaps the pane).
gone=0
for _ in $(seq 1 30); do
    if ! tmux has-session -t "$SES" 2>/dev/null; then
        gone=1
        break
    fi
    sleep 0.2
done
[ "$gone" -eq 1 ] || fail "TUI did not quit (tmux session '$SES' still alive)"

echo "SMOKE OK"
exit 0
