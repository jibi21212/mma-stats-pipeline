//! Spawns the Go scraper on demand and streams its human-readable stdout/stderr
//! line-by-line to a caller-supplied callback, returning the final exit status.
//!
//! The scraper is launched per `Config::scraper` (a prebuilt binary or
//! `go run .` in `scraper-go/`). It is the SOLE WRITER of `data/ufc.db`; this
//! module only invokes it and relays progress — it never touches the DB.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};

use crate::config::{Config, ScraperLaunch};
use crate::jobs::JobMsg;

/// Options controlling one scraper run, mapped to the Go CLI flags.
///
/// - `full` -> `--full` (ignore incremental skip sets, re-fetch everything)
/// - `limit` -> `--limit N` (max events to save; `None` = no limit)
/// - `rate` -> `--rate R` (aggregate requests/second; `None` = scraper default)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrapeOptions {
    /// Full re-scrape (`--full`).
    pub full: bool,
    /// Max events to save (`--limit`); `None` means no limit flag is passed.
    pub limit: Option<u32>,
    /// Aggregate request rate (`--rate`); `None` means no rate flag is passed.
    pub rate: Option<f64>,
}

/// Run the scraper to completion, invoking `on_line` once per output line
/// (merged stdout+stderr, newline-stripped) as it streams in. Blocks until the
/// child exits and returns its [`ExitStatus`].
///
/// `on_line` is `FnMut` so the caller can forward lines into a channel / TUI
/// log buffer.
///
/// The app now uses [`run_async`] (non-blocking); this synchronous variant is
/// retained for the module's own streaming tests and as a reusable building
/// block, so it is allowed to be otherwise unused.
#[allow(dead_code)]
pub fn run<F>(cfg: &Config, opts: &ScrapeOptions, on_line: F) -> Result<ExitStatus>
where
    F: FnMut(&str),
{
    let mut command = build_command(&cfg.scraper, opts, db_flag(cfg).as_deref());
    run_command(&mut command, on_line)
}

/// Compute the `--db <path>` value to pass to the scraper, or `None` to preserve
/// the scraper's own relative default.
///
/// The Go scraper is the SOLE DB WRITER. When the active `db_path` matches the
/// repo default (`<repo>/data/ufc.db`), we pass NOTHING so the scraper keeps its
/// built-in relative default (`../data/ufc.db`, resolved against its `scraper-go`
/// cwd) — identical to prior behavior. When `db_path` was overridden (an
/// installed/writable copy or `$MMA_DB`), we pass that ABSOLUTE path via `--db`
/// so the writer targets the location the rest of the app reads.
fn db_flag(cfg: &Config) -> Option<std::path::PathBuf> {
    if cfg.db_path == cfg.default_db_path() {
        None
    } else {
        Some(cfg.db_path.clone())
    }
}

/// Spawn the scraper on a BACKGROUND thread and return IMMEDIATELY with a
/// channel receiver of [`JobMsg`]. This is the non-blocking entry the event loop
/// uses: the caller stores the `Receiver` in the running-job slot and drains it
/// on every tick, so the UI keeps animating/redrawing while the scraper runs.
///
/// The spawned worker thread builds the command, streams merged stdout+stderr
/// line-by-line as [`JobMsg::Line`] (parsing `"saved event N/M"`-style progress
/// into [`JobMsg::Progress`]), then emits a single [`JobMsg::Done(success)`] when
/// the child exits (or fails to spawn). The thread owns its own reader threads.
pub fn run_async(cfg: &Config, opts: &ScrapeOptions) -> std::sync::mpsc::Receiver<JobMsg> {
    let (tx, rx) = mpsc::channel::<JobMsg>();
    let launch = cfg.scraper.clone();
    let db = db_flag(cfg);
    let opts = opts.clone();
    thread::spawn(move || {
        let mut command = build_command(&launch, &opts, db.as_deref());
        let success = match run_command(&mut command, |line| {
            // Forward progress events first when the line encodes them.
            if let Some((done, total)) = parse_progress(line) {
                let _ = tx.send(JobMsg::Progress(done, total));
            }
            let _ = tx.send(JobMsg::Line(line.to_string()));
        }) {
            Ok(status) => status.success(),
            Err(e) => {
                let _ = tx.send(JobMsg::Line(format!("scrape failed to spawn: {e}")));
                false
            }
        };
        let _ = tx.send(JobMsg::Done(success));
    });
    rx
}

/// Parse a `"saved event N/M"`-style progress fragment out of a streamed line,
/// returning `(done, total)`. Tolerant of surrounding text and varied phrasing
/// ("saved event 3/12", "event 3 of 12", "3/12 events"). Returns `None` when no
/// `N/M` pair with `N <= M` and `M > 0` is found. Pure; unit-tested below.
pub fn parse_progress(line: &str) -> Option<(usize, usize)> {
    // Find the first "<digits>/<digits>" pair in the line.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'/' {
                let num = &line[start..i];
                i += 1; // skip '/'
                let den_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i > den_start {
                    let den = &line[den_start..i];
                    if let (Ok(n), Ok(m)) = (num.parse::<usize>(), den.parse::<usize>())
                        && m > 0
                        && n <= m
                    {
                        return Some((n, m));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Build the [`Command`] for one scraper run from the launch method, options,
/// and an optional explicit DB path.
///
/// For [`ScraperLaunch::Binary`] the binary is executed directly with the
/// scraper directory as the working dir. For [`ScraperLaunch::GoRun`] we invoke
/// `go run .` inside the configured directory. Flags are appended identically in
/// both cases.
///
/// `db` is the resolved DB path to write, or `None` to leave the scraper's own
/// relative default (`../data/ufc.db`) in place. Callers pass `Some` only when
/// the configured `db_path` was overridden (see [`db_flag`]), so the default run
/// is byte-for-byte unchanged from before.
fn build_command(launch: &ScraperLaunch, opts: &ScrapeOptions, db: Option<&Path>) -> Command {
    let mut command = match launch {
        ScraperLaunch::Binary(path) => {
            let mut c = Command::new(path);
            // Run the binary with the scraper dir as cwd so its relative
            // default `--db ../data/ufc.db` resolves the same as `go run .`.
            if let Some(dir) = path.parent() {
                c.current_dir(dir);
            }
            c
        }
        ScraperLaunch::GoRun { dir } => {
            let mut c = Command::new("go");
            c.arg("run").arg(".");
            c.current_dir(dir);
            c
        }
    };
    append_flags(&mut command, opts, db);
    command
}

/// Append the option-derived CLI flags to `command` (shared by both launch
/// kinds). The incremental default passes no extra flags.
///
/// When `db` is `Some`, `--db <path>` is appended FIRST so the scraper writes to
/// the configured (overridden) location; when `None`, no `--db` is passed and
/// the scraper keeps its built-in relative default.
fn append_flags(command: &mut Command, opts: &ScrapeOptions, db: Option<&Path>) {
    if let Some(db) = db {
        command.arg("--db").arg(db);
    }
    if opts.full {
        command.arg("--full");
    }
    if let Some(limit) = opts.limit {
        command.arg("--limit").arg(limit.to_string());
    }
    if let Some(rate) = opts.rate {
        command.arg("--rate").arg(rate.to_string());
    }
}

/// Spawn `command`, stream merged stdout+stderr line-by-line into `on_line`, and
/// block until the child exits. Factored out from [`run`] so tests can drive it
/// with a stub command without a real [`Config`].
fn run_command<F>(command: &mut Command, mut on_line: F) -> Result<ExitStatus>
where
    F: FnMut(&str),
{
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn scraper: {command:?}"))?;

    let stdout = child
        .stdout
        .take()
        .context("scraper child stdout pipe missing")?;
    let stderr = child
        .stderr
        .take()
        .context("scraper child stderr pipe missing")?;

    // Merge both streams into one channel. Two reader threads keep stdout and
    // stderr from dead-locking on full pipe buffers; lines stay ordered within
    // each stream (the only ordering the scraper relies on) and `on_line` is
    // invoked on the calling thread so `F` need not be `Send`.
    let (tx, rx) = mpsc::channel::<String>();
    let out_handle = spawn_line_reader(stdout, tx.clone());
    let err_handle = spawn_line_reader(stderr, tx);

    for line in rx.iter() {
        on_line(&line);
    }

    // Both readers have closed their senders (channel drained) before we join.
    let _ = out_handle.join();
    let _ = err_handle.join();

    let status = child.wait().context("waiting for scraper to exit")?;
    Ok(status)
}

/// Spawn a thread that reads `reader` line-by-line (newline-stripped) and sends
/// each line over `tx`. Lossy UTF-8 decoding keeps the stream alive even if the
/// scraper emits non-UTF-8 bytes.
fn spawn_line_reader<R>(reader: R, tx: mpsc::Sender<String>) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = BufReader::new(reader);
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            bytes.clear();
            match buf.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    // Strip a trailing "\n" / "\r\n" without allocating twice.
                    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
                        bytes.pop();
                    }
                    let line = String::from_utf8_lossy(&bytes).into_owned();
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a stub command through the streaming core and collect its lines.
    fn collect(mut command: Command) -> (Vec<String>, ExitStatus) {
        let mut lines = Vec::new();
        let status = run_command(&mut command, |l| lines.push(l.to_string()))
            .expect("stub command should run");
        (lines, status)
    }

    #[test]
    fn streams_stdout_lines_in_order() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'line1\\nline2\\nline3\\n'");
        let (lines, status) = collect(cmd);
        assert_eq!(lines, vec!["line1", "line2", "line3"]);
        assert!(status.success());
    }

    #[test]
    fn strips_trailing_newline_and_carriage_return() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'a\\r\\nb\\n'");
        let (lines, _status) = collect(cmd);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn final_line_without_trailing_newline_is_captured() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'only-line'");
        let (lines, status) = collect(cmd);
        assert_eq!(lines, vec!["only-line"]);
        assert!(status.success());
    }

    #[test]
    fn captures_stderr_lines() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'err1\\nerr2\\n' 1>&2");
        let (mut lines, _status) = collect(cmd);
        lines.sort();
        assert_eq!(lines, vec!["err1", "err2"]);
    }

    #[test]
    fn merges_stdout_and_stderr() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'out\\n'; printf 'oops\\n' 1>&2");
        let (mut lines, status) = collect(cmd);
        lines.sort();
        assert_eq!(lines, vec!["oops", "out"]);
        assert!(status.success());
    }

    #[test]
    fn reports_nonzero_exit_status() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'before-fail\\n'; exit 7");
        let (lines, status) = collect(cmd);
        assert_eq!(lines, vec!["before-fail"]);
        assert!(!status.success());
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn spawn_failure_is_an_error() {
        let mut cmd = Command::new("definitely-not-a-real-binary-xyzzy");
        let result = run_command(&mut cmd, |_| {});
        assert!(result.is_err());
    }

    #[test]
    fn parse_progress_extracts_n_over_m() {
        assert_eq!(parse_progress("saved event 3/12"), Some((3, 12)));
        assert_eq!(parse_progress("event 3 of 12 [3/12] done"), Some((3, 12)));
        assert_eq!(parse_progress("0/5 events"), Some((0, 5)));
    }

    #[test]
    fn parse_progress_rejects_non_progress_lines() {
        assert_eq!(parse_progress("starting scraper..."), None);
        assert_eq!(parse_progress("rate 2.5/s"), None); // denominator not digits
        assert_eq!(parse_progress("12/0"), None); // zero total
        assert_eq!(parse_progress("13/12"), None); // n > m
    }

    #[test]
    fn append_flags_incremental_default_is_empty() {
        let opts = ScrapeOptions::default();
        let mut cmd = Command::new("true");
        append_flags(&mut cmd, &opts, None);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.is_empty(), "default run passes no flags, got {args:?}");
    }

    #[test]
    fn append_flags_maps_all_options() {
        let opts = ScrapeOptions {
            full: true,
            limit: Some(25),
            rate: Some(4.5),
        };
        let mut cmd = Command::new("true");
        append_flags(&mut cmd, &opts, None);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--full", "--limit", "25", "--rate", "4.5"]);
    }

    #[test]
    fn append_flags_prepends_db_when_path_supplied() {
        // An overridden DB path is passed first as `--db <path>`, ahead of the
        // option-derived flags.
        let opts = ScrapeOptions {
            full: true,
            limit: Some(25),
            rate: Some(4.5),
        };
        let mut cmd = Command::new("true");
        append_flags(&mut cmd, &opts, Some(Path::new("/writable/ufc.db")));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec!["--db", "/writable/ufc.db", "--full", "--limit", "25", "--rate", "4.5"]
        );
    }

    #[test]
    fn append_flags_db_only_when_no_options() {
        // With default options but an overridden DB, only `--db <path>` is added.
        let opts = ScrapeOptions::default();
        let mut cmd = Command::new("true");
        append_flags(&mut cmd, &opts, Some(Path::new("/writable/ufc.db")));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--db", "/writable/ufc.db"]);
    }

    #[test]
    fn db_flag_is_none_for_default_db_path() {
        // The default repo DB must NOT trigger a `--db` flag (preserves prior
        // behavior: the scraper keeps its own relative default).
        let cfg = Config::resolve(
            std::path::PathBuf::from("/tmp/repo"),
            &crate::config::EnvVars::default(),
        );
        assert_eq!(cfg.db_path, cfg.default_db_path());
        assert_eq!(db_flag(&cfg), None);
    }

    #[test]
    fn db_flag_is_some_for_overridden_db_path() {
        // An installed/writable DB (modeled here via $MMA_DB) flows through as a
        // `--db` value so the scraper writes where the app reads.
        use crate::config::EnvVars;
        let env = EnvVars {
            mma_db: Some(std::ffi::OsString::from("/writable/ufc.db")),
            ..Default::default()
        };
        let cfg = Config::resolve(std::path::PathBuf::from("/tmp/repo"), &env);
        assert_eq!(db_flag(&cfg), Some(std::path::PathBuf::from("/writable/ufc.db")));
    }

    #[test]
    fn build_command_gorun_includes_db_when_overridden() {
        // End-to-end through build_command: an overridden DB is appended to the
        // `go run .` invocation as `--db <path>`.
        let opts = ScrapeOptions {
            full: true,
            ..Default::default()
        };
        let launch = ScraperLaunch::GoRun {
            dir: std::path::PathBuf::from("/tmp/scraper-go"),
        };
        let cmd = build_command(&launch, &opts, Some(Path::new("/writable/ufc.db")));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["run", ".", "--db", "/writable/ufc.db", "--full"]);
    }

    #[test]
    fn build_command_gorun_uses_go_run_dot_in_dir() {
        let opts = ScrapeOptions {
            full: true,
            ..Default::default()
        };
        let launch = ScraperLaunch::GoRun {
            dir: std::path::PathBuf::from("/tmp/scraper-go"),
        };
        let cmd = build_command(&launch, &opts, None);
        assert_eq!(cmd.get_program().to_string_lossy(), "go");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["run", ".", "--full"]);
        assert_eq!(
            cmd.get_current_dir().map(|p| p.to_path_buf()),
            Some(std::path::PathBuf::from("/tmp/scraper-go"))
        );
    }

    #[test]
    fn mma_scraper_override_builds_that_executable_with_flags_appended() {
        // The MMA_SCRAPER env override resolves to ScraperLaunch::Binary; the
        // usual flags must still be appended (a stub may ignore them).
        use crate::config::{Config, EnvVars, ScraperLaunch};

        let env = EnvVars {
            mma_scraper: Some(std::ffi::OsString::from("/fixtures/stub_scraper.sh")),
            ..Default::default()
        };
        let cfg = Config::resolve(std::path::PathBuf::from("/tmp/repo"), &env);
        assert_eq!(
            cfg.scraper,
            ScraperLaunch::Binary(std::path::PathBuf::from("/fixtures/stub_scraper.sh"))
        );

        let opts = ScrapeOptions {
            full: true,
            limit: Some(50),
            rate: Some(2.0),
        };
        let cmd = build_command(&cfg.scraper, &opts, None);
        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "/fixtures/stub_scraper.sh"
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--full", "--limit", "50", "--rate", "2"]);
    }

    #[test]
    fn build_command_binary_runs_path_with_parent_cwd() {
        let opts = ScrapeOptions {
            limit: Some(3),
            ..Default::default()
        };
        let launch = ScraperLaunch::Binary(std::path::PathBuf::from("/tmp/scraper-go/scraper"));
        let cmd = build_command(&launch, &opts, None);
        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "/tmp/scraper-go/scraper"
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--limit", "3"]);
        assert_eq!(
            cmd.get_current_dir().map(|p| p.to_path_buf()),
            Some(std::path::PathBuf::from("/tmp/scraper-go"))
        );
    }
}
