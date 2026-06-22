//! "Playwright for the TUI" — true END-TO-END tests for `mma-tui` (REDESIGN).
//!
//! Each test spawns the REAL compiled `mma-tui` binary inside a pseudo-terminal
//! (`portable-pty`), feeds raw keystrokes to the PTY master, and parses the
//! emitted ANSI byte stream into a screen grid (`vt100`) that we scrape for
//! visible text. There is no access to internal app state — we only assert on
//! what a human would actually see rendered, then drive the quit binding and
//! confirm the process exits cleanly.
//!
//! These tests target the REDESIGNED UX (see the locked spec), NOT the old one:
//!   * HOME plays a one-shot "MMA" block-letter intro framed as a fight poster
//!     that headlines the latest numbered UFC card, then shows a vertical 4-item
//!     MENU: Scrape / Database / Predict / Model.
//!   * NAVIGATION is a SCREEN STACK with NO hotkey jumps. On a menu, ↑/↓ move the
//!     selection and ⏎ pushes the chosen screen; Esc AND Backspace pop one level;
//!     the Home key clears back to the home menu; q quits. A persistent FOOTER
//!     shows the contextual controls (e.g. "↑↓ move · ⏎ select · …").
//!   * DATABASE is a hub with two working paths: "Browse events" (events → a card
//!     → a fighter) and "Find a fighter" (live fuzzy search → a fighter profile).
//!   * Long actions (scrape) run on a BACKGROUND thread and stream into a loading
//!     overlay (a braille spinner + a progress bar + the live log) while the event
//!     loop KEEPS TICKING (never freezes).
//!
//! HERMETIC: every (non-ignored) test points the binary at lightweight stubs via
//! env overrides resolved in `src/config.rs`, so there is NO network, NO real
//! model and NO real DB writes:
//!   * `MMA_DB`      -> a tiny temp SQLite DB seeded here (4 contract tables +
//!                      the same known fighters as the stub roster).
//!   * `MMA_SIDECAR` -> `tests/fixtures/stub_sidecar.py` (canned IPC responses).
//!   * `MMA_SCRAPER` -> `tests/fixtures/stub_scraper.sh` (canned progress lines,
//!                      with a small sleep between each so streaming + the
//!                      non-blocking loop are OBSERVABLE mid-run).
//!
//! Timing: the TUI redraws on a 100ms tick and external processes (sidecar,
//! scraper) run asynchronously, so EVERY assertion is preceded by a poll that
//! waits (up to a few seconds) for the expected text to appear. Nothing here
//! sleeps-then-asserts blindly (the ONE deliberate exception is the no-freeze
//! test, which compares two timed captures to prove the animation is advancing).
//!
//! Run just these: `cargo test --test e2e`
//! The single ignored real-stack smoke test: `cargo test --test e2e -- --ignored`

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rusqlite::Connection;

// =========================================================================== //
// KEY ENCODINGS (xterm) — sent as raw bytes to the PTY master.
// =========================================================================== //

const ENTER: &str = "\r";
const ESC: &str = "\x1b";
const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
#[allow(dead_code)]
const LEFT: &str = "\x1b[D";
const RIGHT: &str = "\x1b[C";
/// xterm "Home" key. crossterm decodes both `\x1b[H` and `\x1b[1~` as
/// `KeyCode::Home`; we use the CSI-H form that the binary's nav stack treats as
/// "clear back to the home menu".
const HOME: &str = "\x1b[H";
#[allow(dead_code)]
const TAB: &str = "\t";
const CTRL_C: &str = "\x03";
const BACKSPACE: &str = "\x7f";

/// Fixed PTY geometry for every session (rows x cols).
const ROWS: u16 = 45;
const COLS: u16 = 140;

/// How long to wait for expected text to render before giving up.
const POLL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the child to exit after a quit key.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

// =========================================================================== //
// BUILD: compile the binary once, before any test runs it.
// =========================================================================== //

static BUILD_ONCE: Once = Once::new();

/// Path to the freshly-built `mma-tui` binary, building it once if needed.
///
/// Tests run the COMPILED binary in a PTY (not via `cargo run`, whose own
/// progress output would pollute the terminal). `cargo test` builds the harness
/// but not necessarily the bin target, so we build it explicitly the first time.
fn mma_tui_bin() -> PathBuf {
    BUILD_ONCE.call_once(|| {
        let status = StdCommand::new(cargo_bin())
            .args(["build", "--bin", "mma-tui"])
            .current_dir(crate_dir())
            .status()
            .expect("failed to spawn cargo build");
        assert!(status.success(), "cargo build --bin mma-tui failed");
    });
    let bin = crate_dir().join("target").join("debug").join("mma-tui");
    assert!(bin.is_file(), "mma-tui binary missing at {}", bin.display());
    bin
}

/// The cargo executable (honors `$CARGO`, else the known Homebrew path, else PATH).
fn cargo_bin() -> PathBuf {
    if let Some(c) = std::env::var_os("CARGO") {
        return PathBuf::from(c);
    }
    let brew = PathBuf::from("/opt/homebrew/bin/cargo");
    if brew.is_file() {
        return brew;
    }
    PathBuf::from("cargo")
}

/// Absolute path to this crate's root (`tui-rs/`).
fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Absolute path to the committed E2E fixtures directory.
fn fixtures_dir() -> PathBuf {
    crate_dir().join("tests").join("fixtures")
}

/// Absolute path to the repo root (parent of this crate).
fn repo_root() -> PathBuf {
    crate_dir()
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

// =========================================================================== //
// TEMP DB: a tiny, hermetic SQLite fixture matching the schema contract.
// =========================================================================== //

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// RAII guard owning a uniquely-named temp SQLite file; deletes it (and any
/// WAL/SHM siblings) on drop so tests leave nothing behind.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    /// Create a fresh temp DB, build the 4 contract tables, and seed the known
    /// roster (the same 3 names the stub sidecar serves, plus a couple of others
    /// so we can prove the fighter search NARROWS rather than always showing
    /// everything).
    fn new() -> TempDb {
        let n = DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("mma_tui_e2e_{}_{}.sqlite", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).expect("create temp db");
            create_schema(&conn);
            seed(&conn);
        } // close the writer before the TUI opens it read-only.
        TempDb { path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(self.path.with_extension("sqlite-shm"));
    }
}

/// Create the 4 tables exactly as docs/SCHEMA_CONTRACT.md defines them. Mirrors
/// `tests/db_tests.rs::create_schema` so the read-only query layer behaves
/// identically against this fixture.
fn create_schema(conn: &Connection) {
    conn.execute_batch(
        r#"
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
        "#,
    )
    .expect("create schema");
}

/// Seed a deterministic mini-roster. The stub-roster three (Israel Adesanya,
/// Robert Whittaker, Alex Pereira) get fully-populated career stats so the
/// fighter profile pane renders real numbers + explanations; two extra names
/// (Jon Jones, Conor McGregor) make narrowing observable. One NUMBERED event
/// (UFC 287) + one fight so the home poster, the Browse-events path and the
/// fight-history pane are all non-empty.
fn seed(conn: &Connection) {
    // Israel Adesanya — fully populated (we open his profile in the search test).
    conn.execute(
        "INSERT INTO fighters (
            fighter_id, name, nickname, nationality, height_in, weight_lbs, reach_in,
            stance, date_of_birth, wins, losses, draws, no_contests, was_champion,
            championship_bouts_won, slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def, sub_avg
         ) VALUES (
            1, 'Israel Adesanya', 'The Last Stylebender', 'Nigeria', 76, 185, 80,
            'Switch', '1989-07-22', 24, 3, 0, 0, 1,
            5, 3.93, 0.49, 3.06, 0.61, 0.07, 0.20, 0.71, 0.1
         )",
        [],
    )
    .unwrap();

    // Robert Whittaker — also fully populated.
    conn.execute(
        "INSERT INTO fighters (
            fighter_id, name, nickname, nationality, height_in, weight_lbs, reach_in,
            stance, date_of_birth, wins, losses, draws, no_contests, was_champion,
            championship_bouts_won, slpm, str_acc, sapm, str_def, td_avg, td_acc, td_def, sub_avg
         ) VALUES (
            2, 'Robert Whittaker', 'The Reaper', 'Australia', 72, 185, 73,
            'Orthodox', '1990-12-20', 25, 7, 0, 0, 1,
            2, 4.46, 0.39, 3.30, 0.57, 0.55, 0.36, 0.69, 0.2
         )",
        [],
    )
    .unwrap();

    // Alex Pereira.
    conn.execute(
        "INSERT INTO fighters (
            fighter_id, name, nickname, nationality, height_in, weight_lbs, reach_in,
            stance, wins, losses, was_champion, slpm, str_acc
         ) VALUES (
            3, 'Alex Pereira', 'Poatan', 'Brazil', 76, 205, 79,
            'Orthodox', 9, 2, 1, 5.10, 0.59
         )",
        [],
    )
    .unwrap();

    // Two clearly-distinct names so a search query can NARROW the list.
    conn.execute(
        "INSERT INTO fighters (fighter_id, name, stance) VALUES (4, 'Jon Jones', 'Orthodox')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fighters (fighter_id, name, stance) VALUES (5, 'Conor McGregor', 'Southpaw')",
        [],
    )
    .unwrap();

    // One NUMBERED event (UFC 287) so the home poster headlines "UFC 287" and the
    // Browse-events path has a real card to open. The single fight lets the fight
    // card + fight-history panes render real rows.
    conn.execute(
        "INSERT INTO events (event_id, title, date, location)
         VALUES (1, 'UFC 287', '2023-04-08', 'Miami, Florida, USA')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO fights (
            fight_id, event_id, event_name, date, winner_name, loser_name,
            weight_class, title_bout, method, round_ended, time_ended, referee
         ) VALUES (
            1, 1, 'UFC 287', '2023-04-08', 'Israel Adesanya', 'Alex Pereira',
            'Middleweight', 1, 'KO/TKO', 2, 264, 'Marc Goddard'
         )",
        [],
    )
    .unwrap();
}

// =========================================================================== //
// PTY SESSION: spawn the binary in a pty, stream output into a vt100 parser.
// =========================================================================== //

/// A live `mma-tui` session driven through a pseudo-terminal.
///
/// Owns the PTY master, the child handle, and a background reader thread that
/// continuously feeds the child's output bytes into a `vt100::Parser` behind a
/// mutex. Keystrokes are written to the master; the rendered screen is scraped
/// from the parser. Dropping the session tears the child down.
struct Session {
    /// Writer half of the PTY master (keystrokes go here).
    writer: Box<dyn Write + Send>,
    /// The child process handle (so we can wait / kill it).
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Shared terminal-emulator state fed by the reader thread.
    parser: Arc<Mutex<vt100::Parser>>,
    /// Set once the reader thread observes EOF (child closed the PTY).
    eof: Arc<Mutex<bool>>,
    /// Keep the master pair alive for the session's lifetime.
    _master: Box<dyn portable_pty::MasterPty + Send>,
    /// Temp DB whose lifetime must outlast the running child.
    _db: TempDb,
}

impl Session {
    /// Spawn the hermetic stack: the temp DB + both stub processes, wired in via
    /// the env overrides. Pass extra env pairs for the (single) real-stack test.
    fn spawn_stub() -> Session {
        let db = TempDb::new();
        let env = vec![
            ("MMA_DB".to_string(), db.path.display().to_string()),
            (
                "MMA_SIDECAR".to_string(),
                fixtures_dir().join("stub_sidecar.py").display().to_string(),
            ),
            (
                "MMA_SCRAPER".to_string(),
                fixtures_dir().join("stub_scraper.sh").display().to_string(),
            ),
        ];
        Session::spawn_with(db, env)
    }

    /// Spawn the binary in a PTY with `env` applied on top of a clean base.
    ///
    /// `TERM=xterm-256color` and a fixed 45x140 size are always set so rendering
    /// is deterministic regardless of the (TTY-less) test runner environment.
    fn spawn_with(db: TempDb, env: Vec<(String, String)>) -> Session {
        let bin = mma_tui_bin();

        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(bin);
        cmd.cwd(repo_root());
        cmd.env("TERM", "xterm-256color");
        // Stable terminal behavior; avoid any locale-dependent rendering.
        cmd.env("LC_ALL", "C.UTF-8");
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let child = pty.slave.spawn_command(cmd).expect("spawn mma-tui in pty");
        // Drop the slave so that when the child exits, the master read sees EOF.
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().expect("clone pty reader");
        let writer = pty.master.take_writer().expect("take pty writer");

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let eof = Arc::new(Mutex::new(false));

        // Reader thread: pump child output into the vt100 parser until EOF.
        {
            let parser = Arc::clone(&parser);
            let eof = Arc::clone(&eof);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: child closed the PTY.
                        Ok(n) => {
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                        }
                        Err(_) => break,
                    }
                }
                if let Ok(mut e) = eof.lock() {
                    *e = true;
                }
            });
        }

        Session {
            writer,
            child,
            parser,
            eof,
            _master: pty.master,
            _db: db,
        }
    }

    /// Current rendered screen as plain text (rows joined by '\n', trailing
    /// blank cells trimmed). This is what a human sees right now.
    fn screen(&self) -> String {
        let p = self.parser.lock().expect("parser lock");
        p.screen().contents()
    }

    /// Send raw bytes (keystrokes / escape sequences) to the PTY master.
    fn send(&mut self, s: &str) {
        self.writer
            .write_all(s.as_bytes())
            .expect("write to pty master");
        self.writer.flush().expect("flush pty master");
    }

    /// Block until the rendered screen CONTAINS `needle`, or panic on timeout.
    ///
    /// Returns the matching screen snapshot so callers can make further
    /// assertions on the same frame without re-reading.
    fn wait_for(&self, needle: &str) -> String {
        self.wait_for_with_timeout(needle, POLL_TIMEOUT)
    }

    fn wait_for_with_timeout(&self, needle: &str, timeout: Duration) -> String {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if screen.contains(needle) {
                return screen;
            }
            if start.elapsed() >= timeout {
                panic!(
                    "timed out after {:?} waiting for {needle:?}.\n--- last screen ---\n{screen}\n--- end ---",
                    timeout
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// True once the reader thread has observed the child closing the PTY.
    fn saw_eof(&self) -> bool {
        *self.eof.lock().expect("eof lock")
    }

    /// Wait for the child to exit and return whether it exited successfully,
    /// panicking on timeout. Also waits for the PTY to reach EOF.
    fn wait_clean_exit(&mut self) -> bool {
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    // Drain the rest of the output stream too.
                    while !self.saw_eof() && start.elapsed() < EXIT_TIMEOUT {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    return status.success();
                }
                Ok(None) => {
                    if start.elapsed() >= EXIT_TIMEOUT {
                        panic!("child did not exit within {EXIT_TIMEOUT:?}");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("error waiting for child: {e}"),
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best-effort teardown so a failed assertion never leaves a stray child.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// =========================================================================== //
// SHARED NAV HELPERS (the redesign's screen-stack flows).
// =========================================================================== //

/// Wait for the home menu to be ready: the brand, the poster banner and all four
/// top-level menu options. Returns the matching screen. Used at the start of
/// every test to absorb the one-shot intro + sidecar-load latency before driving
/// keys, and as the canonical "we're back home" check after Esc/Home.
fn wait_home(s: &Session) -> String {
    s.wait_for("mma-tui");
    s.wait_for("Scrape");
    s.wait_for("Predict");
    // The home footer is unique to the root menu (no "Esc Back" — you can't go
    // back from home), so it doubles as a "current screen == Home" assertion.
    s.wait_for("↑↓ move · ⏎ select · q Quit")
}

/// The nav breadcrumb (the header's body line, e.g. "Home › Database › Find a
/// fighter") with the box-drawing frame chars stripped. Lets navigation tests
/// assert on the ACTUAL current stack rather than guessing from body text.
///
/// The breadcrumb row is the FIRST line containing the "›" separator; the bare
/// root ("Home" with no children) has no separator, so we fall back to the first
/// framed line whose stripped content is exactly "Home".
fn breadcrumb(screen: &str) -> String {
    // Strip the vertical box border + surrounding whitespace from a framed row.
    let strip = |l: &str| -> String {
        l.chars()
            .filter(|c| !matches!(c, '│' | '┌' | '┐' | '└' | '┘' | '─'))
            .collect::<String>()
            .trim()
            .to_string()
    };
    // Deepened stacks: the breadcrumb is the line carrying the "›" separator.
    if let Some(line) = screen.lines().find(|l| l.contains('›')) {
        return strip(line);
    }
    // Root: find the framed line that strips to exactly "Home".
    screen
        .lines()
        .map(|l| strip(l))
        .find(|s| s == "Home")
        .unwrap_or_default()
}

/// Move the home-menu selection DOWN `n` times (each keypress + a redraw beat),
/// then ⏎ to push the chosen screen.
fn home_select_and_enter(s: &mut Session, down: usize) {
    for _ in 0..down {
        s.send(DOWN);
        std::thread::sleep(Duration::from_millis(120));
    }
    s.send(ENTER);
}

/// Extract only the left-hand column of a rendered screen — the fighter search
/// box + results list live there (left ~38-50% of the width), while the profile
/// and fight-history panes are on the right. Lets list-membership assertions
/// ignore names that legitimately appear in the right-hand panes.
fn results_list_region(screen: &str) -> String {
    const SPLIT: usize = 52;
    screen
        .lines()
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            let end = chars.len().min(SPLIT);
            chars[..end].iter().collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// =========================================================================== //
// TESTS
// =========================================================================== //

/// 1. INTRO + HOME: launch shows the block-letter "MMA" banner inside a fight
///    poster that headlines the latest NUMBERED UFC card from the seeded DB
///    ("UFC 287"), and the vertical 4-option menu (Scrape / Database / Predict /
///    Model) is visible. The selection HIGHLIGHT moves with ↑/↓: the highlight
///    glyph "▶ " starts on Scrape and lands on Database after one Down. Then q
///    quits cleanly.
#[test]
fn intro_and_home_menu() {
    let mut s = Session::spawn_stub();

    // Brand + all four menu options + the home-only footer.
    let screen = wait_home(&s);
    for opt in ["Scrape", "Database", "Predict", "Model"] {
        assert!(
            screen.contains(opt),
            "home menu should list the {opt} option:\n{screen}"
        );
    }

    // The fight-poster: the one-shot "MMA" block letters reveal into a poster
    // card, and the poster headlines the latest numbered card. The headline +
    // menu are drawn every frame, so they appear regardless of intro progress.
    let poster = s.wait_for("TONIGHT");
    assert!(
        poster.contains("MAIN CARD"),
        "the poster should carry the fight-card banner:\n{poster}"
    );
    let card = s.wait_for("UFC 287");
    assert!(
        card.contains("UFC 287"),
        "the poster should headline the latest numbered card (UFC 287):\n{card}"
    );
    // The chunky block-letter logo uses the full-block glyph; once the intro has
    // revealed at least one column it must be on screen.
    let logo = s.wait_for("█");
    assert!(
        logo.contains('█'),
        "the block-letter MMA logo should render:\n{logo}"
    );

    // Selection highlight starts on the first option (Scrape).
    let home = s.wait_for("▶ Scrape");
    assert!(
        home.contains("▶ Scrape"),
        "the initial highlight should be on Scrape:\n{home}"
    );

    // ↓ moves the highlight to Database (and OFF Scrape).
    s.send(DOWN);
    let moved = s.wait_for("▶ Database");
    assert!(
        !moved.contains("▶ Scrape"),
        "the highlight must move OFF Scrape after Down:\n{moved}"
    );

    // ↑ moves it back up to Scrape.
    s.send(UP);
    s.wait_for("▶ Scrape");

    // Quit on Home with 'q' -> clean exit.
    s.send("q");
    assert!(s.wait_clean_exit(), "process should exit successfully on q");
}

/// 2. NAVIGATION (screen stack, no hotkey jumps): ⏎ on a menu PUSHES into a
///    screen; Esc AND Backspace each POP one level; the Home key clears the whole
///    stack back to the home menu. Each transition is asserted via the rendered
///    breadcrumb so we prove the screen ACTUALLY changes. Finally q quits.
#[test]
fn navigation_push_pop_home() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // PUSH: Down to Database, Enter -> Database hub. Breadcrumb deepens.
    home_select_and_enter(&mut s, 1);
    let db = s.wait_for("Choose a path");
    assert_eq!(
        breadcrumb(&db),
        "Home › Database",
        "Enter should push the Database screen:\n{db}"
    );
    // The Database screen exposes the contextual footer WITH a Back hint (proving
    // we're no longer on the un-poppable Home).
    assert!(
        db.contains("Esc Back"),
        "non-home screens must show an Esc Back hint:\n{db}"
    );

    // PUSH deeper: Down to "Find a fighter", Enter -> FighterSearch.
    s.send(DOWN);
    std::thread::sleep(Duration::from_millis(120));
    s.send(ENTER);
    let search = s.wait_for("Home › Database › Find a fighter");
    assert!(
        search.contains("Search"),
        "should have pushed the fighter-search screen:\n{search}"
    );

    // POP with Esc: back up one level to the Database hub.
    s.send(ESC);
    let back1 = s.wait_for("Choose a path");
    assert_eq!(
        breadcrumb(&back1),
        "Home › Database",
        "Esc should pop one level back to Database:\n{back1}"
    );

    // PUSH again, then POP with BACKSPACE (the other back binding): from the
    // Browse-events list back to the Database hub.
    s.send(UP); // select "Browse events"
    std::thread::sleep(Duration::from_millis(120));
    s.send(ENTER);
    let events = s.wait_for("Home › Database › Events");
    assert!(
        events.contains("Events ("),
        "Enter should push the Events list:\n{events}"
    );
    s.send(BACKSPACE);
    let back2 = s.wait_for("Choose a path");
    assert_eq!(
        breadcrumb(&back2),
        "Home › Database",
        "Backspace should pop one level back to Database:\n{back2}"
    );

    // HOME key: from anywhere, clear the whole stack back to the home menu.
    // Go one level deeper first so the Home key has more than one level to clear.
    s.send(DOWN);
    std::thread::sleep(Duration::from_millis(120));
    s.send(ENTER); // into Find a fighter (deep)
    s.wait_for("Home › Database › Find a fighter");
    s.send(HOME);
    let home = s.wait_for("↑↓ move · ⏎ select · q Quit");
    assert_eq!(
        breadcrumb(&home),
        "Home",
        "the Home key should clear the stack back to the root menu:\n{home}"
    );

    s.send("q");
    assert!(s.wait_clean_exit(), "process should exit cleanly on q");
}

/// 3a. DATABASE — "Find a fighter" path: Home -> Database -> Find a fighter ->
///     type "adesa" to NARROW the live fuzzy list to "Israel Adesanya" -> Enter
///     to open his profile, which shows a LAYMAN stat phrase (from stats_text).
///
///     Name-pool note (verified against app.rs::fighter_name_pool): the search
///     ranks over the SIDECAR roster when a model is loaded. The stub reports a
///     loaded model, so the live list is the stub's three names — ["Alex Pereira",
///     "Israel Adesanya", "Robert Whittaker"] — and "adesa" narrows to just
///     Adesanya.
#[test]
fn database_find_a_fighter_path() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Home -> Database (Down once, Enter).
    home_select_and_enter(&mut s, 1);
    s.wait_for("Choose a path");
    // Database -> "Find a fighter" (Down once, Enter).
    s.send(DOWN);
    std::thread::sleep(Duration::from_millis(120));
    s.send(ENTER);
    s.wait_for("Search");

    // With an empty query the full (sidecar) roster of 3 is listed.
    let full = s.wait_for("Robert Whittaker");
    assert!(
        full.contains("Israel Adesanya") && full.contains("Alex Pereira"),
        "the unfiltered list should hold all 3 roster names:\n{full}"
    );
    assert!(
        full.contains("Fighters (3)"),
        "the unfiltered count should be 3:\n{full}"
    );

    // Type a query that only matches Adesanya, then wait for the unambiguous
    // count==1 marker before asserting so we never read a stale pre-narrow frame.
    s.send("adesa");
    let narrowed = s.wait_for("Fighters (1)");
    assert!(
        narrowed.contains("Israel Adesanya"),
        "the single remaining match should be Adesanya:\n{narrowed}"
    );
    // The OTHER two roster names must be gone from the RESULTS LIST (scoped to the
    // left pane; they can legitimately appear in the right-hand profile later).
    let list = results_list_region(&narrowed);
    assert!(
        !list.contains("Robert Whittaker") && !list.contains("Alex Pereira"),
        "'adesa' should filter the other roster names OUT of the list:\n{list}"
    );

    // Enter opens Adesanya's profile.
    s.send(ENTER);
    s.wait_for("Career stats");
    let screen = s.wait_for("Striking accuracy");
    // A plain-English explanation phrase taken verbatim from stats_text::explain
    // (str_acc -> "...cleaner, more precise punching and kicking.").
    assert!(
        screen.contains("more precise punching"),
        "expected the layman explanation for striking accuracy:\n{screen}"
    );
    // The profile header rendered for the chosen fighter.
    assert!(
        screen.contains("Record:"),
        "profile header should show a Record line:\n{screen}"
    );

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 3b. DATABASE — "Browse events" path: Home -> Database -> Browse events ->
///     open the seeded event (UFC 287) -> see its FIGHTS. Proves the second DB
///     path end-to-end (events list -> a card -> the fight row).
#[test]
fn database_browse_events_path() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Home -> Database (Down once, Enter).
    home_select_and_enter(&mut s, 1);
    s.wait_for("Choose a path");
    // Database -> "Browse events" is the first sub-menu option (already selected).
    s.send(ENTER);

    // Events list shows the seeded numbered card.
    let events = s.wait_for("Home › Database › Events");
    assert!(
        events.contains("UFC 287"),
        "the events list should contain the seeded UFC 287:\n{events}"
    );
    assert!(
        events.contains("Events (1)"),
        "the events list should report exactly 1 seeded event:\n{events}"
    );

    // Open the highlighted event -> its fight card.
    s.send(ENTER);
    let card = s.wait_for("Home › Database › Events › Fight card");
    // The seeded fight renders as "<winner> def. <loser>" with its method/round.
    let fights = s.wait_for("Israel Adesanya def. Alex Pereira");
    assert!(
        fights.contains("Israel Adesanya def. Alex Pereira"),
        "the card should list the seeded fight:\n{fights}"
    );
    assert!(
        fights.contains("KO/TKO"),
        "the fight row should carry its finish method:\n{fights}"
    );
    // The card header names the event we opened.
    assert!(
        card.contains("UFC 287"),
        "the fight-card header should name the opened event:\n{card}"
    );

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 4. PREDICT: reach Predict from the menu, pick two roster fighters, and assert
///    a win % + a tale-of-the-tape render.
///
///    Commit flow (verified against app.rs::on_key_predict + ui/predict.rs):
///      - At startup the TUI fetched the eligibility POLICY + per-fighter
///        divisions ONCE (stub_sidecar.py: max_distance 1; Adesanya/Whittaker M#6,
///        Pereira M#8). All slot filtering thereafter is LOCAL — no per-selection
///        IPC.
///      - Entering Predict focuses slot A; candidates are the full stub roster
///        sorted: ["Alex Pereira", "Israel Adesanya", "Robert Whittaker"], sel 0.
///      - Down once -> index 1; Enter commits A = "Israel Adesanya" and
///        auto-advances focus to slot B.
///      - Slot B's pool is now the LOCAL eligible set for Adesanya =
///        ["Robert Whittaker"] — "Alex Pereira" (M#8, distance 2 > max_distance 1)
///        is FILTERED OUT, proving the filter is real (not "all-but-A"). So the
///        single candidate at index 0 is "Robert Whittaker"; Enter commits B.
///      - With both slots committed, the sidecar prediction fires automatically.
///    The stub returns prob_a 0.62 / prob_b 0.38 and a full tale-of-the-tape.
#[test]
fn predict_shows_probability_and_tale() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Home -> Predict (Down twice -> index 2, Enter).
    home_select_and_enter(&mut s, 2);
    s.wait_for("Fighter A");
    s.wait_for("Alex Pereira"); // candidate list is live (sidecar roster)

    // Commit slot A = Israel Adesanya (index 1 of the sorted roster); the committed
    // header renders a "✓ " mark before the name, so wait for it to confirm the
    // commit before driving B.
    s.send(DOWN);
    s.send(ENTER);
    s.wait_for("✓ Israel Adesanya");

    // Slot B's pool is now the LOCAL eligible set for Adesanya = ["Robert
    // Whittaker"] — "Alex Pereira" (M#8) is dropped (distance 2 > max_distance 1),
    // proving the OTHER slot is genuinely filtered by the policy, not "all-but-A".
    // The single candidate is at index 0, so Enter commits B = Robert Whittaker.
    s.send(ENTER);
    // Confirm the OTHER slot genuinely picked from the FILTERED pool.
    s.wait_for("✓ Robert Whittaker");

    // Prediction fires automatically. Assert the win percentages render.
    let screen = s.wait_for("62%");
    assert!(
        screen.contains("38%"),
        "underdog probability 38% should render:\n{screen}"
    );
    assert!(
        screen.contains("Win probability"),
        "the Win probability pane should render:\n{screen}"
    );

    // Tale-of-the-tape rows for both fighters (labels + a deterministic value).
    let tale = s.wait_for("Tale of the tape");
    assert!(
        tale.contains("Record"),
        "tale should include a Record row:\n{tale}"
    );
    assert!(
        tale.contains("Elo rating"),
        "tale should include an Elo rating row:\n{tale}"
    );
    assert!(
        tale.contains("Reach"),
        "tale should include a Reach row:\n{tale}"
    );
    // A deterministic stub tale value: tale_a record "24-3".
    assert!(
        tale.contains("24-3"),
        "tale-of-the-tape should show Fighter A's stub record 24-3:\n{tale}"
    );

    // LOCAL-FILTER PROOF (the negative): because the OTHER slot was pre-filtered
    // LOCALLY to the eligible set (no per-selection IPC, no server "refusal"), the
    // completed prediction screen must carry NONE of the refusal vocabulary. An
    // ineligible matchup can never reach the predict call, so the defensive
    // "Matchup not allowed" branch (and any refusal reason) never renders.
    let lower = tale.to_lowercase();
    for forbidden in ["refused", "ineligible", "not allowed", "cross-"] {
        assert!(
            !lower.contains(forbidden),
            "completed prediction must not show any refusal message ({forbidden:?}):\n{tale}"
        );
    }

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 4b. PREDICT — WEIGHT-CLASS filter: the new ⇥ (Tab) control cycles a weight-class
///     selector whose chips come ENTIRELY from the sidecar-fetched weight_classes
///     ([Middleweight M#6, Heavyweight M#8] in the stub), and selecting a class
///     filters BOTH slots' candidate pools to fighters who fought in it — composing
///     with the eligibility rules on the other slot.
///
///     Stub membership (DIVISIONS): Pereira M#8 (Heavyweight); Adesanya + Whittaker
///     M#6 (Middleweight). So:
///       * default "All weight classes" -> slot A lists all 3 (Matches (3)).
///       * ⇥ once -> "Middleweight" -> slot A lists Adesanya + Whittaker only
///         (Matches (2)); Pereira filtered OUT.
///       * ⇥ again -> "Heavyweight" -> slot A lists only Pereira (Matches (1));
///         the Middleweights filtered OUT.
///     The chips also render their member counts derived from membership:
///     "Middleweight (2)" and "Heavyweight (1)".
#[test]
fn predict_weight_class_filters_candidate_pool() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Home -> Predict (Down twice -> index 2, Enter).
    home_select_and_enter(&mut s, 2);
    s.wait_for("Fighter A");

    // The weight-class selector renders with the All chip + both fetched classes
    // (names + membership counts come from the sidecar, nothing hardcoded in Rust).
    let sel = s.wait_for("All weight classes");
    assert!(
        sel.contains("Middleweight (2)") && sel.contains("Heavyweight (1)"),
        "the class selector should list the fetched classes with member counts:\n{sel}"
    );

    // Default "All weight classes": slot A's candidate list holds all 3 roster names.
    let all = s.wait_for("Matches (3)");
    let all_list = results_list_region(&all);
    assert!(
        all_list.contains("Alex Pereira")
            && all_list.contains("Israel Adesanya")
            && all_list.contains("Robert Whittaker"),
        "with All selected, slot A should list all 3 fighters:\n{all_list}"
    );

    // ⇥ once -> Middleweight. Slot A narrows to the two Middleweights; Pereira is
    // filtered OUT (membership, not eligibility — there is no other slot yet).
    s.send(TAB);
    let mw = s.wait_for("Weight class — Middleweight");
    let mw_list = results_list_region(&s.wait_for("Matches (2)"));
    assert!(
        mw_list.contains("Israel Adesanya") && mw_list.contains("Robert Whittaker"),
        "Middleweight should surface Adesanya + Whittaker:\n{mw_list}"
    );
    assert!(
        !mw_list.contains("Alex Pereira"),
        "Middleweight must filter Pereira (M#8) OUT of slot A:\n{mw_list}"
    );
    assert!(
        mw.contains("Weight class — Middleweight"),
        "the selector title should reflect the active class:\n{mw}"
    );

    // ⇥ again -> Heavyweight. Slot A narrows to ONLY Pereira.
    s.send(TAB);
    s.wait_for("Weight class — Heavyweight");
    let hw_list = results_list_region(&s.wait_for("Matches (1)"));
    assert!(
        hw_list.contains("Alex Pereira"),
        "Heavyweight should surface Pereira:\n{hw_list}"
    );
    assert!(
        !hw_list.contains("Israel Adesanya") && !hw_list.contains("Robert Whittaker"),
        "Heavyweight must filter the Middleweights OUT of slot A:\n{hw_list}"
    );

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 4c. PREDICT — WEIGHT-CLASS composes with ELIGIBILITY on slot B: with
///     "Middleweight" selected, commit slot A = Adesanya; slot B's pool is then
///     in-class (Middleweight) AND eligible vs Adesanya. Whittaker (M#6, distance 0)
///     qualifies and Pereira (M#8) is excluded BOTH by class membership and by the
///     max_distance-1 gate. So the single slot-B candidate is Whittaker, and the
///     prediction fires with the stub's 62%/38% split — proving the class filter and
///     the eligibility rules compose (pool = in-class AND eligible).
#[test]
fn predict_weight_class_composes_with_eligibility() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    home_select_and_enter(&mut s, 2);
    s.wait_for("Fighter A");
    s.wait_for("All weight classes");

    // ⇥ once -> Middleweight (the class with Adesanya + Whittaker).
    s.send(TAB);
    s.wait_for("Weight class — Middleweight");
    // Slot A in-class pool = [Adesanya, Whittaker]; sorted index 0 = Adesanya.
    s.wait_for("Matches (2)");

    // Commit slot A = Israel Adesanya (index 0 of the in-class, sorted pool).
    s.send(ENTER);
    s.wait_for("✓ Israel Adesanya");

    // Slot B's pool = in-class (Middleweight) AND eligible vs Adesanya = [Whittaker].
    // Pereira is excluded by BOTH the class filter and the distance gate, so the
    // single candidate at index 0 is Whittaker; Enter commits B.
    s.send(ENTER);
    s.wait_for("✓ Robert Whittaker");

    // The composed selection produced a valid matchup -> prediction fires.
    let screen = s.wait_for("62%");
    assert!(
        screen.contains("38%") && screen.contains("Win probability"),
        "the composed matchup should produce the stub prediction:\n{screen}"
    );
    // No refusal vocabulary: the pool was pre-filtered locally, never refused.
    let lower = screen.to_lowercase();
    for forbidden in ["refused", "ineligible", "not allowed"] {
        assert!(
            !lower.contains(forbidden),
            "a composed in-class eligible matchup must not show a refusal ({forbidden:?}):\n{screen}"
        );
    }

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 5. SCRAPE — chips + NO-FREEZE + animation + streaming + completion.
///
///    Home -> Scrape; assert the "Full: OFF" chip, press f and assert it flips to
///    "Full: ON" INSTANTLY; press Enter to run the (stub) scraper. WHILE it runs
///    we prove the UI is NOT frozen: the loading overlay's braille SPINNER glyph
///    advances between two timed captures AND the streamed log/progress arrives
///    PROGRESSIVELY (we catch a mid-run "saved event 1/3" before the later
///    "saved event 3/3"). Finally the app's own completion marker appears.
///
///    The stub scraper sleeps briefly between lines (see stub_scraper.sh) so this
///    mid-run state is reliably observable on the 100ms UI tick.
#[test]
fn scrape_chips_no_freeze_and_streams() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Home -> Scrape (first option, already highlighted; just Enter).
    s.send(ENTER);
    let opts = s.wait_for("Scraper options");
    // The full-rescrape chip starts OFF.
    assert!(
        opts.contains("Full: OFF"),
        "the Full chip should start OFF:\n{opts}"
    );

    // 'f' flips the chip to ON instantly (chip is re-rendered the next frame).
    s.send("f");
    let on = s.wait_for("Full: ON");
    assert!(
        on.contains("Full: ON"),
        "pressing f should flip the chip to Full: ON:\n{on}"
    );
    // Flip it back OFF so the next run is the cheap incremental scrape.
    s.send("f");
    s.wait_for("Full: OFF");

    // RUN: Enter launches the async job + the loading overlay.
    s.send(ENTER);

    // The overlay is up: the spinner panel reports the job is "running" and the
    // overlay footer switches to the running-job hint (proof the job started and
    // the body was replaced by the loading animation).
    let running = s.wait_for("running");
    assert!(
        running.contains("running… · q Quit"),
        "the footer should show the running-job hint while the scrape runs:\n{running}"
    );

    // NO-FREEZE PROOF #1 — progressive streaming: an early progress line is
    // visible BEFORE the run finishes. (If the loop were blocked, all lines would
    // appear at once only after the process exited.)
    let early = s.wait_for("saved event 1/3");
    assert!(
        early.contains("scanning events"),
        "the scraper banner line should stream in early:\n{early}"
    );

    // NO-FREEZE PROOF #2 — the spinner is advancing. The overlay's only motion is
    // the braille spinner in the Status panel, which the still-ticking event loop
    // re-renders every frame (driven by wall-clock time). We SAMPLE the live spinner
    // glyph repeatedly across a window that spans many animation frames and assert it
    // takes on MORE THAN ONE distinct value. A frozen event loop can only ever render
    // a single glyph, so >1 distinct glyph is exactly the liveness signal — and
    // sampling many times (rather than diffing two fixed captures) makes this immune
    // to braille-cycle aliasing AND scheduling jitter. We re-confirm the job is still
    // RUNNING so we never sample a finished (frozen "✓") overlay.
    s.wait_for("running");
    let mut spinner_glyphs = std::collections::HashSet::new();
    let spin_start = Instant::now();
    while spin_start.elapsed() < Duration::from_millis(700) {
        let g = overlay_spinner(&s.screen());
        if !g.is_empty() {
            spinner_glyphs.insert(g);
        }
        if spinner_glyphs.len() >= 2 {
            break; // already proved it advanced; no need to keep sampling.
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(
        !spinner_glyphs.is_empty(),
        "the running overlay should show a braille spinner glyph:\n{}",
        s.screen()
    );
    assert!(
        spinner_glyphs.len() >= 2,
        "the braille spinner must ADVANCE while the job runs (event loop not frozen); \
         saw only {spinner_glyphs:?} across the sampling window"
    );

    // Streaming continues to the final progress line.
    s.wait_for("saved event 3/3");

    // Completion: the app appends its own finish marker after the job exits OK.
    // (kind.label() == "Scraping", so the marker is "$ Scraping finished OK".)
    let done = s.wait_for("Scraping finished OK");
    assert!(
        done.contains("Scraping finished OK"),
        "the app should report the scrape finished OK:\n{done}"
    );
    // The finished overlay stays up until dismissed; the footer offers dismissal.
    let dismissable = s.wait_for("Esc/⏎ dismiss");
    assert!(
        dismissable.contains("Esc/⏎ dismiss"),
        "the finished log should be dismissable via Esc/Enter:\n{dismissable}"
    );

    // Dismiss the finished log -> back on the Scrape options screen.
    s.send(ENTER);
    s.wait_for("Scraper options");

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 5b. PROGRESS BUG (Goal A): once the stub job COMPLETES, the progress bar must
///     STOP and show a finished/100% state — it must NOT keep cycling 0->100.
///
///     We run the scrape to completion (waiting for the app's own
///     "Scraping finished OK" marker), then capture the Status+Progress strip
///     TWICE ~350ms apart. With the bug, the indeterminate sweep would keep
///     advancing and the two captures would differ; with the fix the strip is
///     STABLE (identical) and shows the done/100% markers. The completed log
///     stays up until dismissed (Esc/Enter).
#[test]
fn progress_bar_stops_and_shows_done_on_completion() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Home -> Scrape (first option) -> run.
    s.send(ENTER);
    s.wait_for("Scraper options");
    s.send(ENTER);

    // Let it run to completion: the app appends its finish marker after exit OK.
    s.wait_for("Scraping finished OK");
    // The finished overlay exposes the dismiss hint (proof the job is DONE, not
    // still running).
    let done = s.wait_for("Esc/⏎ dismiss");
    assert!(
        done.contains("Esc/⏎ dismiss"),
        "the finished overlay should be dismissable:\n{done}"
    );

    // The progress/status strip must show the FINISHED state, not "running".
    let region1 = overlay_progress(&s.screen());
    assert!(
        region1.contains("done"),
        "the finished progress strip should show a done state:\n{region1}"
    );
    assert!(
        region1.contains("100%"),
        "the finished progress bar should read 100%:\n{region1}"
    );
    assert!(
        !region1.contains("working"),
        "the bar must not still show the indeterminate 'working…' state:\n{region1}"
    );

    // STABILITY: capture again after a delay spanning many animation frames. With
    // the old bug the indeterminate sweep would keep moving; with the fix the
    // strip is frozen, so the two captures are IDENTICAL.
    std::thread::sleep(Duration::from_millis(350));
    let region2 = overlay_progress(&s.screen());
    assert_eq!(
        region1, region2,
        "the progress strip must be STABLE (stopped) after completion — it kept \
         animating.\n--- capture 1 ---\n{region1}\n--- capture 2 ---\n{region2}"
    );

    // The completed log is still visible until dismissed.
    s.wait_for("saved event 3/3");

    // Dismiss -> back on the Scrape options screen.
    s.send(ENTER);
    s.wait_for("Scraper options");

    s.send("q");
    assert!(s.wait_clean_exit());
}

/// 6. QUIT restores the terminal: Ctrl-C (global quit) yields a successful exit
///    and PTY EOF. The binary installs a panic/exit hook that restores the
///    alt-screen, so a clean exit is the observable signal that teardown ran.
#[test]
fn quit_restores_terminal() {
    let mut s = Session::spawn_stub();
    wait_home(&s);

    // Ctrl-C is the always-on global quit; verify a clean, successful exit.
    s.send(CTRL_C);
    assert!(
        s.wait_clean_exit(),
        "process must exit with success status after Ctrl-C on Home"
    );
    assert!(s.saw_eof(), "PTY should reach EOF after the child exits");
}

/// Extract the loading overlay's STATUS + PROGRESS strip — every rendered line
/// from the "Status" panel through the "Progress" gauge, up to (but excluding)
/// the "Output" log panel. Box-drawing frame chars are stripped so two captures
/// differ ONLY when the actual status/progress CONTENT changes. Used by the
/// progress-bug test to prove the bar STOPS (is stable) once the job is done.
fn overlay_progress(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(start) = lines.iter().position(|l| l.contains("Status")) else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    for l in lines.iter().skip(start) {
        if l.contains("Output (") {
            break;
        }
        let inner: String = l
            .chars()
            .filter(|c| !matches!(c, '│' | '┌' | '┐' | '└' | '┘' | '─'))
            .collect();
        let inner = inner.trim();
        if !inner.is_empty() {
            out.push(inner.to_string());
        }
    }
    out.join("\n")
}

/// The set of braille spinner glyphs the loading overlay cycles through (mirrors
/// `anim::SPINNER_FRAMES`). Used to scrape the live spinner glyph off the rendered
/// Status panel so the no-freeze test can prove it ADVANCES.
const SPINNER_GLYPHS: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Extract the loading overlay's live braille SPINNER glyph from the rendered
/// "Status" panel (the head reads e.g. "⠋ Scraping…  running  3s" while running).
/// Returns the spinner char as a one-char String, or empty if no spinner glyph is
/// on screen (e.g. the overlay isn't up, or the job already finished and the
/// spinner froze to a static "✓"). Scanning for the known braille glyphs makes the
/// scrape robust to surrounding text/layout.
fn overlay_spinner(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    // The Status + Progress panels share the same rows (a horizontal split), so the
    // border row carrying " Status " also carries " Progress ". Start scanning from
    // that row and stop at the "Output" log panel below; the spinner glyph sits on
    // the content line just under the border, on the left (Status) half.
    let Some(start) = lines.iter().position(|l| l.contains("Status")) else {
        return String::new();
    };
    for l in lines.iter().skip(start) {
        if l.contains("Output (") {
            break;
        }
        if let Some(g) = l.chars().find(|c| SPINNER_GLYPHS.contains(c)) {
            return g.to_string();
        }
    }
    String::new()
}

// =========================================================================== //
// REAL-STACK SMOKE TEST (ignored by default).
//
// Drives the ACTUAL python venv + ml/serve.py + real model + real data/ufc.db —
// NO stubs. Asserts that a genuine prediction for two real fighters renders. Run
// with: `cargo test --test e2e -- --ignored`.
// =========================================================================== //

/// End-to-end against the real ML sidecar + model + DB. Ignored by default
/// because it depends on the trained model and the venv being present; it is the
/// canary that the stub-shaped assertions still match the real wiring.
///
/// REGRESSION GUARD (2026-06): this test originally CAUGHT a real contract bug —
/// the Python `ml/predict.py::_tale_of_tape` emitted `divisions` as a list of
/// `(gender, ordinal)` tuples (JSON `[["M",6],["M",7]]`), but the FROZEN Rust
/// contract `models.rs::TaleOfTape` declares `divisions: Vec<String>`, so serde
/// could not deserialize the real predict payload and the TUI showed
/// "Prediction failed: malformed predict payload from sidecar" for EVERY real
/// matchup. The fix lives on the Python side (`predict.py::division_names` maps
/// each `(gender, ordinal)` to a human-readable division name string, matching
/// the stub + SCHEMA contract) — the frozen Rust model was left untouched. This
/// test now PASSES end-to-end and guards against that regression returning.
#[test]
#[ignore = "real stack: needs .venv + ml/serve.py + trained model + data/ufc.db (run with `cargo test --test e2e -- --ignored`)"]
fn real_stack_smoke() {
    let venv_python = repo_root().join(".venv").join("bin").join("python");
    let real_db = repo_root().join("data").join("ufc.db");
    let real_model = repo_root().join("ml").join("models").join("predictor.joblib");

    // Skip gracefully if the real stack is not provisioned on this machine.
    if !venv_python.is_file() || !real_db.is_file() || !real_model.is_file() {
        eprintln!(
            "real_stack_smoke: skipping — missing one of {:?} / {:?} / {:?}",
            venv_python, real_db, real_model
        );
        return;
    }

    // No stub env: MMA_PYTHON + MMA_DB only, so config resolves the real
    // python ml/serve.py sidecar and the real read-only DB. A throwaway TempDb
    // is created but immediately overridden via MMA_DB to the real DB; the
    // unused fixture file is harmless and cleaned up on drop.
    let db = TempDb::new();
    let env = vec![
        ("MMA_DB".to_string(), real_db.display().to_string()),
        ("MMA_PYTHON".to_string(), venv_python.display().to_string()),
    ];
    let mut s = Session::spawn_with(db, env);

    // Home should render with the REAL roster + poster (the real DB has thousands
    // of fighters and many numbered cards; we just assert the home menu is up).
    wait_home(&s);

    // Go to Predict (Down twice from Scrape -> Predict, Enter) and pick two real,
    // well-known fighters: Israel Adesanya vs Robert Whittaker. The real model
    // takes a moment to load, so allow a generous timeout for roster + prediction.
    home_select_and_enter(&mut s, 2);
    s.wait_for_with_timeout("Fighter A", Duration::from_secs(30));

    // Type to narrow the autocomplete to Adesanya, then commit slot A.
    s.send("adesanya");
    s.wait_for_with_timeout("Adesanya", Duration::from_secs(30));
    s.send(ENTER); // commit A, auto-advance to B

    // Focus is already on B; type to narrow to Whittaker and commit.
    s.wait_for("Fighter B");
    s.send("whittaker");
    s.wait_for_with_timeout("Whittaker", Duration::from_secs(30));
    s.send(ENTER); // commit B -> prediction fires

    // A real prediction renders: the Win probability pane plus a percentage and
    // a tale-of-the-tape. We don't assert exact numbers (the model decides), only
    // that the prediction surface is populated.
    s.wait_for_with_timeout("Win probability", Duration::from_secs(30));
    let screen = s.wait_for_with_timeout("Tale of the tape", Duration::from_secs(30));
    assert!(
        screen.contains('%'),
        "a real win-probability percentage should render:\n{screen}"
    );
    assert!(
        screen.contains("Elo rating"),
        "the real tale-of-the-tape should include an Elo rating row:\n{screen}"
    );
    // The tale must include a Divisions row labelled with a human-readable
    // division NAME string. Both Adesanya and Whittaker have fought at
    // Middleweight, so that name must render. This is the regression guard for
    // the divisions-shape contract bug (see the doc comment above).
    assert!(
        screen.contains("Divisions"),
        "the real tale-of-the-tape should include a Divisions row:\n{screen}"
    );
    assert!(
        screen.contains("Middleweight"),
        "Divisions should render a human-readable division NAME (Middleweight), \
         not raw (gender, ordinal) tuples:\n{screen}"
    );

    // Esc pops back to the Predict screen, Home clears to the root, then Ctrl-C
    // quits — exercising the screen-stack nav against the real stack too.
    s.send(ESC);
    s.send(HOME);
    s.wait_for("↑↓ move · ⏎ select · q Quit");
    s.send(CTRL_C);
    assert!(s.wait_clean_exit(), "real-stack session should exit cleanly");
}

// Reference the rarely-used key constants so `-D unused` stays quiet even if a
// future edit removes their only use; keeps the encoding table self-documenting.
#[allow(dead_code)]
fn _key_table_is_referenced() {
    let _ = (LEFT, RIGHT, TAB, UP, ESC, ENTER, CTRL_C, BACKSPACE, HOME);
}
