//! Background job model: the async, NON-BLOCKING side of long-running actions
//! (scrape, train, model-load) plus the per-job state the event loop ticks.
//!
//! The redesign fixes a real freeze bug: long actions used to run synchronously
//! on the event-loop thread, blocking input and the animation. Now every long
//! action spawns a worker thread that streams [`JobMsg`] into an `mpsc` channel
//! and returns immediately; `App::on_tick` drains the channel each tick, so the
//! loop keeps drawing the loading animation and live log without ever blocking.
//!
//! The scrape runner lives in `scraper::run_async` (it reuses the existing
//! streaming core); the train runner lives here ([`spawn_train`]). Both speak the
//! same [`JobMsg`] protocol so the UI overlay is identical for either job.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Instant;

use crate::config::Config;
use crate::scraper;

/// Which kind of background job is running. Drives post-completion side effects
/// (`App` reloads the DB summary + sidecar after a scrape; reloads the sidecar
/// after a train) and the loading-overlay title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// `scraper::run_async` — the Go scraper refreshing `data/ufc.db`.
    Scrape,
    /// `jobs::spawn_train` — `python predict.py --train` retraining the model.
    Train,
}

impl JobKind {
    /// Human-readable label for the loading overlay header.
    pub fn label(self) -> &'static str {
        match self {
            JobKind::Scrape => "Scraping",
            JobKind::Train => "Training model",
        }
    }
}

/// One message streamed from a worker thread to the event loop over the job
/// channel. The worker sends zero+ `Line`/`Progress` then exactly one `Done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobMsg {
    /// One newline-stripped output line (merged stdout+stderr).
    Line(String),
    /// Parsed `(done, total)` progress, e.g. from `"saved event 3/12"`.
    Progress(usize, usize),
    /// Terminal message: the job finished; `true` == success (exit status 0).
    Done(bool),
}

/// Lifecycle status of the running (or just-finished) job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// The worker thread is still streaming; overlay + spinner are live.
    Running,
    /// The job exited 0. The completed log stays visible until Esc/Enter.
    Done,
    /// The job exited non-zero / failed to spawn. Log stays visible.
    Failed,
}

impl JobStatus {
    /// True once the job has stopped (success or failure).
    pub fn is_finished(self) -> bool {
        !matches!(self, JobStatus::Running)
    }
}

/// Maximum job-log lines retained in memory (matches the old scrape cap).
const MAX_LOG_LINES: usize = 5000;

/// State for one in-flight (or just-completed) background job.
///
/// `App` holds `Option<RunningJob>`: `Some` while a job runs OR while its
/// finished log is still on screen; `None` once the user dismisses it. The event
/// loop drains `rx` every tick via [`RunningJob::drain`].
pub struct RunningJob {
    /// Which job this is (drives post-completion actions + overlay title).
    pub kind: JobKind,
    /// Channel receiver fed by the worker thread; drained on every tick.
    pub rx: Receiver<JobMsg>,
    /// Rolling buffer of streamed output lines (tail shown in the overlay).
    pub log: Vec<String>,
    /// When the job started, for the elapsed-time readout.
    pub started: Instant,
    /// Current lifecycle status.
    pub status: JobStatus,
    /// Latest `(done, total)` progress, if the job reports any.
    pub progress: Option<(usize, usize)>,
    /// Wall-clock seconds elapsed AT COMPLETION, frozen the moment the job
    /// finishes. `None` while running. Freezing it keeps the finished overlay
    /// (and the progress strip) STILL — the elapsed readout no longer ticks
    /// after the work is done.
    pub finished_secs: Option<u64>,
}

impl RunningJob {
    /// Create a fresh running job around a worker channel.
    pub fn new(kind: JobKind, rx: Receiver<JobMsg>) -> RunningJob {
        RunningJob {
            kind,
            rx,
            log: Vec::new(),
            started: Instant::now(),
            status: JobStatus::Running,
            progress: None,
            finished_secs: None,
        }
    }

    /// Append a log line, trimming the buffer to [`MAX_LOG_LINES`].
    pub fn push_log(&mut self, line: String) {
        self.log.push(line);
        let len = self.log.len();
        if len > MAX_LOG_LINES {
            self.log.drain(0..len - MAX_LOG_LINES);
        }
    }

    /// Drain all currently-available messages WITHOUT blocking. Updates the log,
    /// progress, and status. Returns `Some(success)` exactly once — on the tick
    /// the `Done` message is observed — so the caller can run post-actions; `None`
    /// otherwise (still running, or already finished on a prior tick).
    pub fn drain(&mut self) -> Option<bool> {
        let mut completed: Option<bool> = None;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                JobMsg::Line(line) => self.push_log(line),
                JobMsg::Progress(done, total) => self.progress = Some((done, total)),
                JobMsg::Done(success) => {
                    self.status = if success {
                        JobStatus::Done
                    } else {
                        JobStatus::Failed
                    };
                    // Freeze the elapsed readout so the finished overlay is still.
                    if self.finished_secs.is_none() {
                        self.finished_secs = Some(self.started.elapsed().as_secs());
                    }
                    completed = Some(success);
                }
            }
        }
        completed
    }

    /// Elapsed wall-clock seconds for the readout. While running this is the live
    /// elapsed time; once the job finishes it returns the FROZEN duration captured
    /// at completion, so the finished overlay stops ticking.
    pub fn elapsed_secs(&self) -> u64 {
        self.finished_secs
            .unwrap_or_else(|| self.started.elapsed().as_secs())
    }

    /// True while the worker thread is still streaming. (Convenience accessor
    /// for screen renderers; not used by the Foundation overlay directly.)
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.status == JobStatus::Running
    }
}

/// Spawn the Go scraper as a background job. Thin wrapper over
/// [`scraper::run_async`] returning a ready-to-store [`RunningJob`].
pub fn spawn_scrape(cfg: &Config, opts: &scraper::ScrapeOptions) -> RunningJob {
    let rx = scraper::run_async(cfg, opts);
    RunningJob::new(JobKind::Scrape, rx)
}

/// Spawn `python predict.py --train` on a BACKGROUND thread, streaming its
/// merged stdout+stderr into a [`JobMsg`] channel and returning IMMEDIATELY.
///
/// Mirrors the scraper async runner so the UI overlay is identical. The model
/// still loads ONCE in the long-lived sidecar — this only retrains the on-disk
/// model; `App` asks the sidecar to `reload()` on completion.
pub fn spawn_train(cfg: &Config) -> RunningJob {
    let (tx, rx) = std::sync::mpsc::channel::<JobMsg>();
    let python = cfg.python.clone();
    let ml_dir = cfg.ml_dir.clone();
    let predict_py = ml_dir.join("predict.py");

    thread::spawn(move || {
        let spawned = Command::new(&python)
            .arg(&predict_py)
            .arg("--train")
            .current_dir(&ml_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(JobMsg::Line(format!("training spawn failed: {e}")));
                let _ = tx.send(JobMsg::Done(false));
                return;
            }
        };

        // Reader threads keep stdout/stderr from dead-locking on full pipes.
        let mut handles = Vec::new();
        if let Some(out) = child.stdout.take() {
            let tx = tx.clone();
            handles.push(thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if let Some((d, t)) = scraper::parse_progress(&line) {
                        let _ = tx.send(JobMsg::Progress(d, t));
                    }
                    let _ = tx.send(JobMsg::Line(line));
                }
            }));
        }
        if let Some(err) = child.stderr.take() {
            let tx = tx.clone();
            handles.push(thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let _ = tx.send(JobMsg::Line(line));
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }

        let success = matches!(child.wait(), Ok(st) if st.success());
        let _ = tx.send(JobMsg::Done(success));
    });

    RunningJob::new(JobKind::Train, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn drain_collects_lines_progress_and_done_once() {
        let (tx, rx) = mpsc::channel();
        tx.send(JobMsg::Line("a".into())).unwrap();
        tx.send(JobMsg::Progress(1, 3)).unwrap();
        tx.send(JobMsg::Line("b".into())).unwrap();
        tx.send(JobMsg::Done(true)).unwrap();
        let mut job = RunningJob::new(JobKind::Scrape, rx);

        let done = job.drain();
        assert_eq!(done, Some(true));
        assert_eq!(job.log, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(job.progress, Some((1, 3)));
        assert_eq!(job.status, JobStatus::Done);
        assert!(job.status.is_finished());

        // A second drain on a closed channel reports no fresh completion.
        assert_eq!(job.drain(), None);
    }

    #[test]
    fn drain_marks_failed_on_nonzero() {
        let (tx, rx) = mpsc::channel();
        tx.send(JobMsg::Done(false)).unwrap();
        let mut job = RunningJob::new(JobKind::Train, rx);
        assert_eq!(job.drain(), Some(false));
        assert_eq!(job.status, JobStatus::Failed);
    }

    #[test]
    fn log_trims_to_cap() {
        let (_tx, rx) = mpsc::channel();
        let mut job = RunningJob::new(JobKind::Scrape, rx);
        for i in 0..(MAX_LOG_LINES + 50) {
            job.push_log(format!("line {i}"));
        }
        assert_eq!(job.log.len(), MAX_LOG_LINES);
        assert_eq!(
            job.log.last().unwrap(),
            &format!("line {}", MAX_LOG_LINES + 49)
        );
    }
}
