//! OpenTelemetry env injected into TUI child processes (scraper + ML sidecar).
//!
//! The Go scraper and Python sidecar keep telemetry **opt-in** (`MMA_OTEL`,
//! `MMA_OTEL_FILE`, or `OTEL_EXPORTER_OTLP_ENDPOINT`). A normal `./mma` /
//! `make` scrape therefore emitted nothing: `Command` inherits the parent
//! environment and the TUI never set those vars. This module enables them on
//! spawn so `make telemetry-up && make`, then scrape inside the TUI, is enough.
//!
//! OTLP is only added when the collector answers on `:4318` (or the user already
//! exported `OTEL_EXPORTER_OTLP_ENDPOINT`). Metrics are never enabled — Jaeger
//! 404s `/v1/metrics`.

use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Service name the scraper child reports in Jaeger.
pub const SERVICE_SCRAPER: &str = "mma-scraper";
/// Service name the ML sidecar child reports in Jaeger.
pub const SERVICE_SIDECAR: &str = "mma-sidecar";

/// Default local Jaeger OTLP HTTP endpoint.
pub const DEFAULT_OTLP_ENDPOINT: &str = "http://127.0.0.1:4318";

const DEFAULT_OTLP_HOST: &str = "127.0.0.1";
const DEFAULT_OTLP_PORT: u16 = 4318;
const SCRAPER_FILE_REL: &str = "telemetry/runs/tui-scraper.jsonl";
const SIDECAR_FILE_REL: &str = "telemetry/runs/tui-ml.jsonl";
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

const STATUS_JAEGER: &str = "traces: Jaeger :16686 + file";
const STATUS_FILE_ONLY: &str = "traces: file only (Jaeger not reachable on :4318)";

/// Snapshot of process env that influences child telemetry. Injected in tests
/// so spawn-env assertions never touch the real environment or Docker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessOtelEnv {
    pub mma_otel: Option<String>,
    pub mma_otel_file: Option<String>,
    pub otel_exporter_otlp_endpoint: Option<String>,
    pub otel_sdk_disabled: Option<String>,
}

impl ProcessOtelEnv {
    /// Read the relevant variables from the real process environment.
    pub fn from_process() -> ProcessOtelEnv {
        ProcessOtelEnv {
            mma_otel: non_empty_var("MMA_OTEL"),
            mma_otel_file: non_empty_var("MMA_OTEL_FILE"),
            otel_exporter_otlp_endpoint: non_empty_var("OTEL_EXPORTER_OTLP_ENDPOINT"),
            otel_sdk_disabled: non_empty_var("OTEL_SDK_DISABLED"),
        }
    }
}

fn non_empty_var(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Env + UI status for one child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOtel {
    /// Explicit env vars to set on the child. Empty when the user opted out.
    pub vars: Vec<(String, String)>,
    /// One-line scrape-UI / job-log status. Empty when opted out.
    pub status_line: String,
    /// JSONL dump path (if any), so the TUI can create the parent directory.
    pub file_path: Option<PathBuf>,
}

impl ChildOtel {
    /// Scraper child env: probe local Jaeger, honor user-set OTLP / opt-out.
    pub fn for_scraper(repo_root: &Path) -> ChildOtel {
        ChildOtel::scraper_with(
            repo_root,
            &ProcessOtelEnv::from_process(),
            probe_default_otlp(),
        )
    }

    /// Sidecar child env: same policy, `mma-sidecar` + `tui-ml.jsonl`.
    pub fn for_sidecar(repo_root: &Path) -> ChildOtel {
        ChildOtel::sidecar_with(
            repo_root,
            &ProcessOtelEnv::from_process(),
            probe_default_otlp(),
        )
    }

    /// Pure scraper plan (no process env, no network). Used by tests.
    pub fn scraper_with(repo_root: &Path, env: &ProcessOtelEnv, otlp_reachable: bool) -> ChildOtel {
        plan(
            SERVICE_SCRAPER,
            repo_root.join(SCRAPER_FILE_REL),
            env,
            otlp_reachable,
        )
    }

    /// Pure sidecar plan (no process env, no network). Used by tests.
    pub fn sidecar_with(repo_root: &Path, env: &ProcessOtelEnv, otlp_reachable: bool) -> ChildOtel {
        plan(
            SERVICE_SIDECAR,
            repo_root.join(SIDECAR_FILE_REL),
            env,
            otlp_reachable,
        )
    }

    /// Set the planned vars on `command`. Does not clear inherited env.
    pub fn apply(&self, command: &mut Command) {
        for (k, v) in &self.vars {
            command.env(k, v);
        }
    }

    /// Create `{repo}/telemetry/runs/` (or the parent of a user-set file).
    pub fn ensure_file_dir(&self) {
        if let Some(path) = &self.file_path
            && let Some(dir) = path.parent()
        {
            let _ = std::fs::create_dir_all(dir);
        }
    }
}

/// Build the child env from an explicit snapshot + OTLP reachability.
///
/// Opt-out (`MMA_OTEL=0`/`false` or `OTEL_SDK_DISABLED=true`) yields an empty
/// plan so the child inherits the parent's disable flags unchanged.
pub fn plan(
    service_name: &str,
    default_file: PathBuf,
    env: &ProcessOtelEnv,
    otlp_reachable: bool,
) -> ChildOtel {
    if is_disabled(env) {
        return ChildOtel {
            vars: Vec::new(),
            status_line: String::new(),
            file_path: None,
        };
    }

    let file = env
        .mma_otel_file
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_file);

    let mut vars = vec![
        ("OTEL_SERVICE_NAME".to_string(), service_name.to_string()),
        ("MMA_OTEL".to_string(), "1".to_string()),
        (
            "MMA_OTEL_FILE".to_string(),
            file.to_string_lossy().into_owned(),
        ),
    ];

    let otlp = if let Some(ep) = env
        .otel_exporter_otlp_endpoint
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        Some(ep.to_string())
    } else if otlp_reachable {
        Some(DEFAULT_OTLP_ENDPOINT.to_string())
    } else {
        None
    };

    let status_line = if otlp.is_some() {
        STATUS_JAEGER.to_string()
    } else {
        STATUS_FILE_ONLY.to_string()
    };

    if let Some(ep) = otlp {
        vars.push(("OTEL_EXPORTER_OTLP_ENDPOINT".to_string(), ep));
    }

    ChildOtel {
        vars,
        status_line,
        file_path: Some(file),
    }
}

fn is_disabled(env: &ProcessOtelEnv) -> bool {
    matches!(
        env.otel_sdk_disabled.as_deref(),
        Some(v) if v.eq_ignore_ascii_case("true")
    ) || matches!(
        env.mma_otel.as_deref(),
        Some(v) if v == "0" || v.eq_ignore_ascii_case("false")
    )
}

/// True when something accepts TCP on the default local OTLP HTTP port.
pub fn probe_default_otlp() -> bool {
    collector_reachable(DEFAULT_OTLP_HOST, DEFAULT_OTLP_PORT)
}

fn collector_reachable(host: &str, port: u16) -> bool {
    let ip: IpAddr = host.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), PROBE_TIMEOUT).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;

    fn vars_map(otel: &ChildOtel) -> HashMap<String, String> {
        otel.vars.iter().cloned().collect()
    }

    fn default_env() -> ProcessOtelEnv {
        ProcessOtelEnv::default()
    }

    #[test]
    fn scraper_file_only_when_collector_down() {
        let root = PathBuf::from("/tmp/repo");
        let otel = ChildOtel::scraper_with(&root, &default_env(), false);
        let vars = vars_map(&otel);
        assert_eq!(
            vars.get("OTEL_SERVICE_NAME").map(String::as_str),
            Some(SERVICE_SCRAPER)
        );
        assert_eq!(vars.get("MMA_OTEL").map(String::as_str), Some("1"));
        assert_eq!(
            vars.get("MMA_OTEL_FILE").map(String::as_str),
            Some("/tmp/repo/telemetry/runs/tui-scraper.jsonl")
        );
        assert!(!vars.contains_key("OTEL_EXPORTER_OTLP_ENDPOINT"));
        assert!(!vars.contains_key("MMA_OTEL_METRICS"));
        assert_eq!(otel.status_line, STATUS_FILE_ONLY);
    }

    #[test]
    fn scraper_sets_otlp_when_collector_up() {
        let root = PathBuf::from("/tmp/repo");
        let otel = ChildOtel::scraper_with(&root, &default_env(), true);
        let vars = vars_map(&otel);
        assert_eq!(
            vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").map(String::as_str),
            Some(DEFAULT_OTLP_ENDPOINT)
        );
        assert!(!vars.contains_key("MMA_OTEL_METRICS"));
        assert_eq!(otel.status_line, STATUS_JAEGER);
    }

    #[test]
    fn honors_user_otlp_endpoint_even_if_probe_fails() {
        let env = ProcessOtelEnv {
            otel_exporter_otlp_endpoint: Some("http://collector.example:4318".into()),
            ..Default::default()
        };
        let otel = ChildOtel::scraper_with(&PathBuf::from("/tmp/repo"), &env, false);
        let vars = vars_map(&otel);
        assert_eq!(
            vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").map(String::as_str),
            Some("http://collector.example:4318")
        );
        assert_eq!(otel.status_line, STATUS_JAEGER);
    }

    #[test]
    fn honors_user_mma_otel_file() {
        let env = ProcessOtelEnv {
            mma_otel_file: Some("/custom/spans.jsonl".into()),
            ..Default::default()
        };
        let otel = ChildOtel::scraper_with(&PathBuf::from("/tmp/repo"), &env, false);
        let vars = vars_map(&otel);
        assert_eq!(
            vars.get("MMA_OTEL_FILE").map(String::as_str),
            Some("/custom/spans.jsonl")
        );
        assert_eq!(
            otel.file_path.as_deref(),
            Some(Path::new("/custom/spans.jsonl"))
        );
    }

    #[test]
    fn mma_otel_zero_opts_out() {
        let env = ProcessOtelEnv {
            mma_otel: Some("0".into()),
            otel_exporter_otlp_endpoint: Some(DEFAULT_OTLP_ENDPOINT.into()),
            ..Default::default()
        };
        let otel = ChildOtel::scraper_with(&PathBuf::from("/tmp/repo"), &env, true);
        assert!(otel.vars.is_empty());
        assert!(otel.status_line.is_empty());
        assert!(otel.file_path.is_none());
    }

    #[test]
    fn otel_sdk_disabled_opts_out() {
        let env = ProcessOtelEnv {
            otel_sdk_disabled: Some("true".into()),
            mma_otel: Some("1".into()),
            ..Default::default()
        };
        let otel = ChildOtel::scraper_with(&PathBuf::from("/tmp/repo"), &env, true);
        assert!(otel.vars.is_empty());
        assert!(otel.status_line.is_empty());
    }

    #[test]
    fn sidecar_plan_uses_ml_service_and_file() {
        let otel = ChildOtel::sidecar_with(&PathBuf::from("/tmp/repo"), &default_env(), true);
        let vars = vars_map(&otel);
        assert_eq!(
            vars.get("OTEL_SERVICE_NAME").map(String::as_str),
            Some(SERVICE_SIDECAR)
        );
        assert_eq!(
            vars.get("MMA_OTEL_FILE").map(String::as_str),
            Some("/tmp/repo/telemetry/runs/tui-ml.jsonl")
        );
        assert_eq!(
            vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").map(String::as_str),
            Some(DEFAULT_OTLP_ENDPOINT)
        );
        assert!(!vars.contains_key("MMA_OTEL_METRICS"));
    }

    #[test]
    fn apply_sets_command_env_and_skips_metrics() {
        let otel = ChildOtel::scraper_with(&PathBuf::from("/tmp/repo"), &default_env(), true);
        let mut cmd = Command::new("true");
        otel.apply(&mut cmd);
        let envs: HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            envs.get("OTEL_SERVICE_NAME").map(String::as_str),
            Some("mma-scraper")
        );
        assert_eq!(envs.get("MMA_OTEL").map(String::as_str), Some("1"));
        assert_eq!(
            envs.get("OTEL_EXPORTER_OTLP_ENDPOINT").map(String::as_str),
            Some(DEFAULT_OTLP_ENDPOINT)
        );
        assert!(!envs.contains_key("MMA_OTEL_METRICS"));
    }

    #[test]
    fn for_scraper_and_sidecar_construct_without_panic() {
        // Covers the process-env + probe wrappers (no Docker required).
        let _ = ChildOtel::for_scraper(Path::new("/tmp/repo"));
        let _ = ChildOtel::for_sidecar(Path::new("/tmp/repo"));
    }

    #[test]
    fn collector_probe_true_on_open_port_false_on_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");
        assert!(
            collector_reachable("127.0.0.1", addr.port()),
            "open port should count as reachable"
        );
        drop(listener);
        assert!(
            !collector_reachable("127.0.0.1", addr.port()),
            "closed port should not count as reachable"
        );
    }
}
