//! Runtime configuration: resolves repo paths, the Python interpreter, the
//! sidecar script, and how to launch the Go scraper.
//!
//! `load()` walks up from the current directory to find the repo root (the
//! ancestor that contains `data/ufc.db`) and derives every other path from it.
//!
//! ENV OVERRIDES (all optional; current behavior is the default when unset).
//! These exist so the TUI can be driven against lightweight stubs for hermetic,
//! offline E2E testing without touching the real DB / model / network:
//! - `MMA_DB`: if set, use it VERBATIM as `db_path` and as the resolved
//!   `repo_root` parent context (skips the walk-up search for `data/ufc.db`).
//! - `MMA_SIDECAR`: if set, the sidecar is launched as this SINGLE executable
//!   run with NO `serve.py` argument (instead of `python ml/serve.py`). It must
//!   speak the same newline-delimited JSON IPC protocol.
//! - `MMA_SCRAPER`: if set, the scraper is this SINGLE executable; the usual
//!   flag args (`--full` / `--limit` / `--rate`) are still appended (a stub may
//!   ignore them).
//! - `MMA_PYTHON`: still honored — the Python interpreter for the default
//!   sidecar / training when `MMA_SIDECAR` is unset.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// How the Go scraper should be invoked.
///
/// Prefer a prebuilt binary if one exists; otherwise fall back to running
/// `go run .` inside `scraper-go/`. When `MMA_SCRAPER` is set it resolves to a
/// [`ScraperLaunch::Binary`] pointing at that executable (flags are still
/// appended; see `src/scraper.rs`).
#[derive(Debug, Clone, PartialEq)]
pub enum ScraperLaunch {
    /// A prebuilt scraper binary to execute directly.
    Binary(PathBuf),
    /// Run `go run .` with this directory as the working dir.
    GoRun { dir: PathBuf },
}

/// How the Python ML sidecar should be launched.
///
/// Default is `python <ml/serve.py>` (the `Script` variant). When `MMA_SIDECAR`
/// is set it resolves to [`SidecarLaunch::Executable`]: a single program run
/// with NO `serve.py` argument, used to inject a hermetic stub that speaks the
/// same newline-delimited JSON IPC protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum SidecarLaunch {
    /// Default: run `python <script>` (the `python` / `script` pair below).
    Script {
        /// Python interpreter (`$MMA_PYTHON`, else venv python, else `python3`).
        python: PathBuf,
        /// The `ml/serve.py` script path.
        script: PathBuf,
    },
    /// Override (`MMA_SIDECAR`): run this single executable, no script arg.
    Executable(PathBuf),
}

/// Fully-resolved configuration for one TUI session.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Repo root (the ancestor directory containing `data/ufc.db`).
    pub repo_root: PathBuf,
    /// Read-only SQLite DB. Default `<repo>/data/ufc.db`; `$MMA_DB` verbatim if set.
    pub db_path: PathBuf,
    /// Python interpreter: `$MMA_PYTHON`, else the repo `.venv` python, else `python3`.
    pub python: PathBuf,
    /// Long-lived sidecar script: `<repo>/ml/serve.py`.
    pub sidecar_script: PathBuf,
    /// How to launch the long-lived IPC sidecar. Default `python <serve.py>`;
    /// `$MMA_SIDECAR` (a single executable, no script arg) if set.
    pub sidecar: SidecarLaunch,
    /// Working directory for the ML sidecar / training: `<repo>/ml`.
    pub ml_dir: PathBuf,
    /// Working directory for the scraper: `<repo>/scraper-go`.
    pub scraper_dir: PathBuf,
    /// How to launch the scraper (`$MMA_SCRAPER` binary, a prebuilt binary, or `go run .`).
    pub scraper: ScraperLaunch,
}

/// Snapshot of the environment variables that influence configuration.
///
/// Captured once (via [`EnvVars::from_process`]) and threaded through the PURE
/// resolver so tests can inject values WITHOUT mutating the process-global
/// environment. `None` means the variable is unset (or set to an empty string,
/// which we treat as unset to mirror the existing `MMA_PYTHON` handling).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnvVars {
    /// `$MMA_DB` — verbatim DB path override.
    pub mma_db: Option<OsString>,
    /// `$MMA_PYTHON` — Python interpreter override.
    pub mma_python: Option<OsString>,
    /// `$MMA_SIDECAR` — single-executable sidecar override (no `serve.py` arg).
    pub mma_sidecar: Option<OsString>,
    /// `$MMA_SCRAPER` — single-executable scraper override.
    pub mma_scraper: Option<OsString>,
}

impl EnvVars {
    /// Read the four override variables from the real process environment,
    /// treating empty strings as unset.
    pub fn from_process() -> EnvVars {
        EnvVars {
            mma_db: non_empty_var("MMA_DB"),
            mma_python: non_empty_var("MMA_PYTHON"),
            mma_sidecar: non_empty_var("MMA_SIDECAR"),
            mma_scraper: non_empty_var("MMA_SCRAPER"),
        }
    }
}

/// Read `name` from the environment, returning `None` if unset or empty.
fn non_empty_var(name: &str) -> Option<OsString> {
    match env::var_os(name) {
        Some(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

impl Config {
    /// Resolve all paths by locating the repo root, the Python interpreter, the
    /// sidecar launch method, and the scraper launch method.
    ///
    /// Resolution rules (env overrides take precedence; see module docs):
    /// - repo root: walk up from cwd until an ancestor contains `data/ufc.db`.
    ///   When `MMA_DB` is set, the walk-up is SKIPPED — the repo root is derived
    ///   from cwd (best-effort) and `db_path` is the `MMA_DB` value verbatim.
    /// - python: env `MMA_PYTHON` if set, else `<repo>/.venv/bin/python` if it
    ///   exists, else `python3`.
    /// - sidecar: `MMA_SIDECAR` (single executable) if set, else `python <ml/serve.py>`.
    /// - scraper: `MMA_SCRAPER` (single executable) if set, else
    ///   `<repo>/scraper-go/<binary>` if present, else `GoRun { dir }`.
    pub fn load() -> Result<Config> {
        let env_vars = EnvVars::from_process();
        let cwd = env::current_dir().context("could not determine current directory")?;

        // With MMA_DB set, the DB no longer needs to live at <repo>/data/ufc.db,
        // so the walk-up search must not be a hard requirement. Fall back to cwd
        // as the repo root when the search fails.
        let repo_root = match find_repo_root(&cwd) {
            Some(root) => root,
            None if env_vars.mma_db.is_some() => cwd.clone(),
            None => {
                return Err(anyhow::anyhow!(
                    "could not locate repo root (no ancestor of {} contains data/ufc.db)",
                    cwd.display()
                ));
            }
        };
        Ok(Self::resolve(repo_root, &env_vars))
    }

    /// Build a `Config` from an already-resolved repo root, reading any env
    /// overrides from the real process environment. Split out for testability;
    /// prefer [`Config::resolve`] in tests to inject env without mutating it.
    #[allow(dead_code)] // public convenience kept for callers/tests; `load` uses `resolve`.
    pub fn from_repo_root(repo_root: PathBuf) -> Config {
        Config::resolve(repo_root, &EnvVars::from_process())
    }

    /// PURE resolver: build a `Config` from a repo root plus an explicit
    /// [`EnvVars`] snapshot, applying every documented override rule. Mutates no
    /// global state, so tests can assert each override deterministically.
    pub fn resolve(repo_root: PathBuf, env: &EnvVars) -> Config {
        let ml_dir = repo_root.join("ml");
        let sidecar_script = ml_dir.join("serve.py");
        let scraper_dir = repo_root.join("scraper-go");

        let db_path = match &env.mma_db {
            Some(db) => PathBuf::from(db),
            None => repo_root.join("data").join("ufc.db"),
        };
        let python = resolve_python(&repo_root, env.mma_python.as_deref());
        let sidecar = resolve_sidecar(&python, &sidecar_script, env.mma_sidecar.as_deref());
        let scraper = resolve_scraper(&scraper_dir, env.mma_scraper.as_deref());

        Config {
            repo_root,
            db_path,
            python,
            sidecar_script,
            sidecar,
            ml_dir,
            scraper_dir,
            scraper,
        }
    }
}

/// Walk up from `start` looking for an ancestor that contains `data/ufc.db`.
fn find_repo_root(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if dir.join("data").join("ufc.db").is_file() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Resolve the Python interpreter per the documented precedence.
///
/// `mma_python` is the injected `$MMA_PYTHON` value (already normalised so an
/// empty string is `None`). When unset, prefer the repo `.venv` python, else
/// fall back to `python3` on PATH.
fn resolve_python(repo_root: &Path, mma_python: Option<&std::ffi::OsStr>) -> PathBuf {
    if let Some(p) = mma_python {
        return PathBuf::from(p);
    }
    let venv_python = repo_root.join(".venv").join("bin").join("python");
    if venv_python.is_file() {
        return venv_python;
    }
    PathBuf::from("python3")
}

/// Resolve how to launch the IPC sidecar.
///
/// When `$MMA_SIDECAR` is set, the sidecar is that SINGLE executable run with no
/// `serve.py` argument ([`SidecarLaunch::Executable`]). Otherwise the default
/// `python <ml/serve.py>` pair ([`SidecarLaunch::Script`]).
fn resolve_sidecar(
    python: &Path,
    sidecar_script: &Path,
    mma_sidecar: Option<&std::ffi::OsStr>,
) -> SidecarLaunch {
    if let Some(exe) = mma_sidecar {
        return SidecarLaunch::Executable(PathBuf::from(exe));
    }
    SidecarLaunch::Script {
        python: python.to_path_buf(),
        script: sidecar_script.to_path_buf(),
    }
}

/// Resolve how to launch the scraper.
///
/// When `$MMA_SCRAPER` is set, that single executable is used verbatim
/// ([`ScraperLaunch::Binary`]); flag args are still appended by `src/scraper.rs`.
/// Otherwise prefer a prebuilt binary in `scraper-go/`, else `go run .`.
fn resolve_scraper(scraper_dir: &Path, mma_scraper: Option<&std::ffi::OsStr>) -> ScraperLaunch {
    if let Some(exe) = mma_scraper {
        return ScraperLaunch::Binary(PathBuf::from(exe));
    }
    // Common output names for the Go scraper binary.
    for name in ["scraper-go", "scraper", "scraper-go.bin"] {
        let candidate = scraper_dir.join(name);
        if is_executable_file(&candidate) {
            return ScraperLaunch::Binary(candidate);
        }
    }
    ScraperLaunch::GoRun {
        dir: scraper_dir.to_path_buf(),
    }
}

/// True if `path` is a regular file (and, on Unix, has an execute bit set).
fn is_executable_file(path: &Path) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// An `EnvVars` with every override unset (the default-behavior baseline).
    fn no_env() -> EnvVars {
        EnvVars::default()
    }

    #[test]
    fn resolve_derives_paths_with_no_env_overrides() {
        let root = PathBuf::from("/tmp/repo");
        let cfg = Config::resolve(root.clone(), &no_env());
        assert_eq!(cfg.db_path, root.join("data/ufc.db"));
        assert_eq!(cfg.sidecar_script, root.join("ml/serve.py"));
        assert_eq!(cfg.ml_dir, root.join("ml"));
        assert_eq!(cfg.scraper_dir, root.join("scraper-go"));
    }

    #[test]
    fn default_sidecar_is_python_plus_serve_script() {
        let root = PathBuf::from("/tmp/repo");
        let cfg = Config::resolve(root.clone(), &no_env());
        match cfg.sidecar {
            SidecarLaunch::Script { python, script } => {
                // No MMA_PYTHON and no venv at this fake root -> python3.
                assert_eq!(python, PathBuf::from("python3"));
                assert_eq!(script, root.join("ml/serve.py"));
            }
            other => panic!("expected default Script sidecar, got {other:?}"),
        }
    }

    #[test]
    fn default_scraper_falls_back_to_go_run_when_no_binary() {
        // A directory that does not contain a built scraper binary, no override.
        let dir = PathBuf::from("/tmp/definitely-not-a-scraper-dir-xyz");
        match resolve_scraper(&dir, None) {
            ScraperLaunch::GoRun { dir: d } => assert_eq!(d, dir),
            other => panic!("expected GoRun fallback, got {other:?}"),
        }
    }

    #[test]
    fn find_repo_root_returns_none_for_filesystem_root() {
        // Walking up from `/` will not find data/ufc.db on the test machine.
        assert!(find_repo_root(Path::new("/")).is_none());
    }

    // ----------------------------------------------------------------------- //
    // ENV OVERRIDE TESTS — all use the PURE resolver with injected EnvVars, so
    // they never touch the process-global environment and need no serialization.
    // ----------------------------------------------------------------------- //

    #[test]
    fn mma_db_overrides_db_path_verbatim() {
        let root = PathBuf::from("/tmp/repo");
        let env = EnvVars {
            mma_db: Some(OsString::from("/custom/place/test.db")),
            ..Default::default()
        };
        let cfg = Config::resolve(root, &env);
        assert_eq!(cfg.db_path, PathBuf::from("/custom/place/test.db"));
    }

    #[test]
    fn mma_db_unset_uses_default_db_path() {
        let root = PathBuf::from("/tmp/repo");
        let cfg = Config::resolve(root.clone(), &no_env());
        assert_eq!(cfg.db_path, root.join("data/ufc.db"));
    }

    #[test]
    fn mma_sidecar_overrides_to_single_executable() {
        let root = PathBuf::from("/tmp/repo");
        let env = EnvVars {
            mma_sidecar: Some(OsString::from("/fixtures/stub_sidecar.py")),
            ..Default::default()
        };
        let cfg = Config::resolve(root, &env);
        match cfg.sidecar {
            SidecarLaunch::Executable(p) => {
                assert_eq!(p, PathBuf::from("/fixtures/stub_sidecar.py"));
            }
            other => panic!("expected Executable sidecar override, got {other:?}"),
        }
    }

    #[test]
    fn mma_sidecar_unset_keeps_default_script_sidecar() {
        let root = PathBuf::from("/tmp/repo");
        let cfg = Config::resolve(root.clone(), &no_env());
        assert!(matches!(cfg.sidecar, SidecarLaunch::Script { .. }));
    }

    #[test]
    fn mma_scraper_overrides_to_binary() {
        let root = PathBuf::from("/tmp/repo");
        let env = EnvVars {
            mma_scraper: Some(OsString::from("/fixtures/stub_scraper.sh")),
            ..Default::default()
        };
        let cfg = Config::resolve(root, &env);
        match cfg.scraper {
            ScraperLaunch::Binary(p) => {
                assert_eq!(p, PathBuf::from("/fixtures/stub_scraper.sh"));
            }
            other => panic!("expected Binary scraper override, got {other:?}"),
        }
    }

    #[test]
    fn mma_scraper_unset_uses_default_resolution() {
        let dir = PathBuf::from("/tmp/definitely-not-a-scraper-dir-xyz");
        assert!(matches!(
            resolve_scraper(&dir, None),
            ScraperLaunch::GoRun { .. }
        ));
    }

    #[test]
    fn mma_python_overrides_interpreter_and_flows_into_sidecar() {
        let root = PathBuf::from("/tmp/repo");
        let env = EnvVars {
            mma_python: Some(OsString::from("/opt/py/bin/python")),
            ..Default::default()
        };
        let cfg = Config::resolve(root.clone(), &env);
        assert_eq!(cfg.python, PathBuf::from("/opt/py/bin/python"));
        match cfg.sidecar {
            SidecarLaunch::Script { python, .. } => {
                assert_eq!(python, PathBuf::from("/opt/py/bin/python"));
            }
            other => panic!("expected Script sidecar carrying MMA_PYTHON, got {other:?}"),
        }
    }

    #[test]
    fn mma_python_falls_back_to_python3_when_unset_and_no_venv() {
        let root = PathBuf::from("/tmp/repo-without-venv");
        assert_eq!(resolve_python(&root, None), PathBuf::from("python3"));
    }

    #[test]
    fn resolve_python_prefers_injected_value() {
        let root = PathBuf::from("/tmp/repo");
        let injected = OsString::from("/usr/local/bin/python3.12");
        assert_eq!(
            resolve_python(&root, Some(OsStr::new(&injected))),
            PathBuf::from("/usr/local/bin/python3.12")
        );
    }

    #[test]
    fn all_overrides_together_are_independent() {
        let root = PathBuf::from("/tmp/repo");
        let env = EnvVars {
            mma_db: Some(OsString::from("/d/test.db")),
            mma_python: Some(OsString::from("/p/python")),
            mma_sidecar: Some(OsString::from("/s/stub_sidecar.py")),
            mma_scraper: Some(OsString::from("/c/stub_scraper.sh")),
        };
        let cfg = Config::resolve(root, &env);
        assert_eq!(cfg.db_path, PathBuf::from("/d/test.db"));
        assert_eq!(cfg.python, PathBuf::from("/p/python"));
        // MMA_SIDECAR wins over MMA_PYTHON for the IPC sidecar launch.
        assert!(
            matches!(cfg.sidecar, SidecarLaunch::Executable(ref p) if p == Path::new("/s/stub_sidecar.py"))
        );
        assert!(
            matches!(cfg.scraper, ScraperLaunch::Binary(ref p) if p == Path::new("/c/stub_scraper.sh"))
        );
    }
}
