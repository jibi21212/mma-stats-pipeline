//! Client for the long-lived Python ML sidecar (`ml/serve.py`).
//!
//! The sidecar loads the model ONCE and answers newline-delimited JSON requests
//! over stdin, writing one JSON response line per request to stdout (stderr is
//! logs only). This client owns the child process, assigns monotonically
//! increasing request ids, writes a request line, and blocks reading response
//! lines until it sees the matching `id`. `Drop` kills the child.
//!
//! All prediction logic stays in Python — this is a thin transport.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::config::{Config, SidecarLaunch};
use crate::models::{
    EligibilityPayload, PredictPayload, PredictResult, ReloadPayload, RosterPayload, SidecarCommand,
    SidecarRequest, SidecarResponse, StatusPayload,
};

/// A running sidecar process plus the plumbing to talk to it.
///
/// Construction starts the child via [`Sidecar::start`]; the struct holds the
/// child handle, its stdin writer, a buffered stdout reader, and the next id
/// counter. It is NOT `Sync`; drive it from a single owner (the app loop).
pub struct Sidecar {
    inner: SidecarInner,
}

/// Implementation detail kept private so Phase-2 can choose the exact plumbing
/// (blocking line read vs. reader thread + channel) without changing the public
/// API. Left opaque on purpose.
struct SidecarInner {
    /// The child process handle (killed on `Drop`).
    child: Child,
    /// Writer half: we push one compact JSON line per request here.
    stdin: ChildStdin,
    /// Buffered reader over the child's stdout; one JSON line per response.
    stdout: BufReader<ChildStdout>,
    /// Monotonic request id counter (starts at 1).
    next_id: u64,
}

impl Sidecar {
    /// Spawn the IPC sidecar with `cfg.ml_dir` as the working directory, piping
    /// stdin/stdout (and inheriting stderr for logs). Returns once the child is
    /// spawned; the model may still be loading.
    ///
    /// Per `cfg.sidecar` ([`SidecarLaunch`]):
    /// - [`SidecarLaunch::Script`]: run `python <ml/serve.py>` (default).
    /// - [`SidecarLaunch::Executable`]: run a single executable with NO script
    ///   arg (the `$MMA_SIDECAR` override — e.g. a hermetic test stub). It must
    ///   speak the same newline-delimited JSON protocol.
    pub fn start(cfg: &Config) -> Result<Sidecar> {
        let mut command = match &cfg.sidecar {
            SidecarLaunch::Script { python, script } => {
                let mut c = Command::new(python);
                c.arg(script);
                c
            }
            SidecarLaunch::Executable(exe) => Command::new(exe),
        };
        command.current_dir(&cfg.ml_dir);
        Sidecar::from_command(command).with_context(|| {
            format!(
                "failed to start ML sidecar: {:?} (cwd {})",
                cfg.sidecar,
                cfg.ml_dir.display()
            )
        })
    }

    /// Spawn an arbitrary command as the sidecar, wiring stdin (piped), stdout
    /// (piped) and stderr (inherited for logs).
    ///
    /// `start` builds the real `python ml/serve.py` invocation and delegates
    /// here. Tests inject a stub process that speaks the same JSON-line protocol,
    /// so the spawn command is fully decoupled from the transport logic.
    pub fn from_command(mut command: Command) -> Result<Sidecar> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().context("failed to spawn sidecar process")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("sidecar child has no stdin handle"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("sidecar child has no stdout handle"))?;
        Ok(Sidecar {
            inner: SidecarInner {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 1,
            },
        })
    }

    /// Send `{"cmd":"ping"}`; returns `Ok(())` on `ok:true`.
    ///
    /// Part of the IPC protocol and covered by `tests/sidecar_tests.rs`. The
    /// app uses `status` for liveness checks, so `ping` is not yet on a UI path.
    #[allow(dead_code)]
    pub fn ping(&mut self) -> Result<()> {
        self.request(SidecarCommand::Ping)?;
        Ok(())
    }

    /// Send `{"cmd":"status"}`; returns the parsed status payload.
    pub fn status(&mut self) -> Result<StatusPayload> {
        let resp = self.request(SidecarCommand::Status)?;
        let payload: StatusPayload = serde_json::from_value(resp.payload)
            .context("malformed status payload from sidecar")?;
        Ok(payload)
    }

    /// Send `{"cmd":"roster"}`; returns the fighter-name list. Errors (mapped
    /// from `ok:false`) when the model is not trained.
    pub fn roster(&mut self) -> Result<Vec<String>> {
        let resp = self.request(SidecarCommand::Roster)?;
        let payload: RosterPayload = serde_json::from_value(resp.payload)
            .context("malformed roster payload from sidecar")?;
        Ok(payload.fighters)
    }

    /// Send `{"cmd":"eligibility"}`; returns the eligibility POLICY (`rules`) plus
    /// the per-fighter `divisions` map. The Predict screen fetches this ONCE at
    /// startup and then filters eligible opponents LOCALLY (applying `rules` to the
    /// `divisions` ordinals), so an ineligible opponent can never be picked WITHOUT
    /// a per-selection round-trip.
    ///
    /// Errors (mapped from `ok:false`) when the model is not trained.
    pub fn eligibility(&mut self) -> Result<EligibilityPayload> {
        let resp = self.request(SidecarCommand::Eligibility)?;
        let payload: EligibilityPayload = serde_json::from_value(resp.payload)
            .context("malformed eligibility payload from sidecar")?;
        Ok(payload)
    }

    /// Send `{"cmd":"predict","a":a,"b":b}`; returns the parsed prediction.
    /// Errors (from `ok:false`) when the model is missing or a name is unknown.
    pub fn predict(&mut self, a: &str, b: &str) -> Result<PredictResult> {
        let resp = self.request(SidecarCommand::Predict {
            a: a.to_string(),
            b: b.to_string(),
        })?;
        let payload: PredictPayload = serde_json::from_value(resp.payload)
            .context("malformed predict payload from sidecar")?;
        Ok(payload.result)
    }

    /// Send `{"cmd":"reload"}`; asks the sidecar to re-read the model from disk
    /// (used after training). Returns the post-reload load state.
    pub fn reload(&mut self) -> Result<ReloadPayload> {
        let resp = self.request(SidecarCommand::Reload)?;
        let payload: ReloadPayload = serde_json::from_value(resp.payload)
            .context("malformed reload payload from sidecar")?;
        Ok(payload)
    }

    /// Send one command and block until the matching response arrives.
    ///
    /// Assigns the next monotonic id, writes a single compact JSON line to the
    /// child's stdin, flushes, then reads stdout lines until one echoes our id.
    /// A response with `ok:false` is surfaced as an `Err` carrying its `error`
    /// message. A closed/broken pipe (child died) is also an `Err`.
    fn request(&mut self, cmd: SidecarCommand) -> Result<SidecarResponse> {
        let id = self.inner.next_id;
        self.inner.next_id += 1;

        let req = SidecarRequest { id, cmd };
        let line = serde_json::to_string(&req).context("failed to serialize sidecar request")?;
        debug_assert!(
            !line.contains('\n'),
            "request line must not contain embedded newlines",
        );

        // Write the request line, then flush so the sidecar sees it promptly.
        self.inner
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.inner.stdin.write_all(b"\n"))
            .and_then(|()| self.inner.stdin.flush())
            .context("failed to write request to sidecar (process may have exited)")?;

        // Read response lines until we find the one echoing our id. Lines with a
        // different id (e.g. stray/out-of-order output) are skipped defensively.
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self
                .inner
                .stdout
                .read_line(&mut buf)
                .context("failed to read response from sidecar")?;
            if n == 0 {
                // EOF: the child closed stdout / exited without answering.
                bail!("sidecar closed its output stream (process exited unexpectedly)");
            }

            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            let resp: SidecarResponse = serde_json::from_str(trimmed)
                .with_context(|| format!("malformed JSON response from sidecar: {trimmed}"))?;

            if resp.id != id {
                // Not ours — keep reading.
                continue;
            }

            if !resp.ok {
                let msg = resp
                    .error
                    .unwrap_or_else(|| "sidecar returned ok:false with no error message".into());
                return Err(anyhow!(msg));
            }

            return Ok(resp);
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Best-effort teardown: kill the child and reap it so we don't leave a
        // zombie. Ignore errors — the process may already be gone.
        let _ = self.inner.child.kill();
        let _ = self.inner.child.wait();
    }
}
