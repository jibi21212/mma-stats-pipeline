#!/usr/bin/env bash
# Hermetic STUB for the Go scraper (drop-in for the real scraper binary).
#
# Point the TUI at this via MMA_SCRAPER=<abs path to this file> so the Scrape
# screen runs OFFLINE and DETERMINISTICALLY with no network and no DB writes.
#
# It IGNORES all args (the TUI still appends --full / --limit N / --rate R; a
# stub may ignore them) and prints a few fake progress lines to stdout, then
# exits 0. Each line is flushed immediately (line-buffered stdout) so the TUI's
# line-streaming reader shows progress promptly.
#
# STREAMING IS OBSERVABLE BY DESIGN: a small sleep is inserted between lines so
# the run takes a beat (~1.2s total). This is what lets the e2e / tmux tests
# prove the redesign's NON-BLOCKING event loop — while the scraper is still
# emitting lines the TUI must keep ticking (advancing the fighters animation +
# braille spinner) and surface each line PROGRESSIVELY rather than all at once.
# The delay is short enough to keep the suite fast but long enough that a poller
# on a 100ms tick reliably catches the mid-run state.
#
# Output is what the Rust scraper streamer relays into the Scrape-screen log.

set -euo pipefail

# Per-line delay (seconds). Override with MMA_STUB_SCRAPER_DELAY for faster/slower
# runs; defaults to a value that is comfortably observable on a 100ms UI tick.
DELAY="${MMA_STUB_SCRAPER_DELAY:-0.3}"

echo "scanning events..."
sleep "$DELAY"
echo "saved event 1/3"
sleep "$DELAY"
echo "saved event 2/3"
sleep "$DELAY"
echo "saved event 3/3"
sleep "$DELAY"
echo "done"

exit 0
