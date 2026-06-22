//! mma-tui — terminal UI for the local UFC stats pipeline.
//!
//! Responsibilities (see CONTRACT.md):
//! 1. Read `data/ufc.db` read-only via rusqlite (`db`).
//! 2. Spawn and talk to the long-lived Python ML sidecar `ml/serve.py` over
//!    newline-delimited JSON (`sidecar`).
//! 3. Spawn the Go scraper on demand and stream its output (`scraper`).
//!
//! ALL prediction logic lives in Python; Rust never reimplements ML math.
//!
//! This file owns terminal setup/teardown and the event loop. Per-screen logic
//! lives in `app` (state) and `ui` (rendering).

mod anim;
mod app;
mod config;
mod db;
mod fuzzy;
mod jobs;
mod models;
mod scraper;
mod sidecar;
mod stats_text;
mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};

use crate::app::App;
use crate::config::Config;
use crate::db::Db;
use crate::sidecar::Sidecar;

/// Fast poll/redraw cadence used WHILE SOMETHING IS ANIMATING (a job is running
/// or the intro is playing). ~33ms bounds the loop near the anim's ~30 fps so the
/// time-based animation clock is sampled smoothly; ratatui's cell-diff keeps the
/// redraw flicker-free (no full clear). Slightly below 1000/ANIM_FPS so we never
/// systematically miss a frame.
const TICK_ANIMATING: Duration = Duration::from_millis(28);

/// Slow poll/redraw cadence used WHEN THE SCREEN IS STATIC. A long input timeout
/// keeps the CPU idle (no busy redraw loop) while still draining the job channel
/// and staying responsive to keystrokes (a key short-circuits the wait).
const TICK_IDLE: Duration = Duration::from_millis(120);

fn main() -> Result<()> {
    // Resolve config, open the DB read-only, and start the Python sidecar
    // BEFORE entering raw mode so any startup error prints cleanly.
    let config = Config::load()?;
    let db = Db::open(&config.db_path)?;
    let sidecar = Sidecar::start(&config)?;

    let mut app = App::new(config, db, sidecar)?;

    // Enter the alternate screen + raw mode. `init` installs a panic hook that
    // restores the terminal, so a `todo!()` panic won't wreck the user's shell.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);

    // Always restore the terminal, then surface any loop error.
    ratatui::restore();
    result
}

/// The main event loop: draw, then handle input AND tick until quit.
///
/// SMOOTHNESS: animation is decoupled from the input poll. The animation frame
/// index is derived from elapsed wall-clock time ([`App::anim_frame`]), so motion
/// speed is constant regardless of redraw rate. The poll TIMEOUT is adaptive:
///   * while something is animating (job running or the intro is playing) we poll
///     on the fast [`TICK_ANIMATING`] cadence and redraw every loop, giving a
///     smooth ~30 fps. ratatui's per-cell diff makes each redraw flicker-free.
///   * when the screen is static we poll on the slow [`TICK_IDLE`] cadence so the
///     CPU idles instead of busy-redrawing a still frame.
/// In both cases a keypress short-circuits the wait, and we always `on_tick`
/// (drain the job channel) so streaming + completion are handled promptly.
fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit() {
        terminal.draw(|frame| ui::draw(frame, app))?;

        let timeout = if app.is_animating() {
            TICK_ANIMATING
        } else {
            TICK_IDLE
        };

        if event::poll(timeout)? {
            // Resize / mouse / paste are redrawn on the next loop iteration.
            if let Event::Key(key) = event::read()? {
                app.on_key(key)?;
            }
        }
        // Always advance the legacy tick counter + drain the job channel.
        app.on_tick()?;
    }
    Ok(())
}
