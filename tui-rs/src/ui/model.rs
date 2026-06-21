//! Model screen: the sidecar's model-load state, the headline metrics, the full
//! per-model hold-out table, and the train / reload / refresh actions.
//!
//! Training output streams in the loading overlay (`ui::loading`), NOT here —
//! while a train job is in flight, `ui::mod::draw` routes the body to the overlay
//! (fighters + spinner + progress + live log) and this renderer is not called.
//! On completion `app::on_job_complete` reloads the sidecar so the new metrics
//! show up the moment the user dismisses the finished log and lands back here.
//!
//! Reads only `app.model` (`ModelState`, populated by `App::refresh_model_status`
//! from `sidecar.status()`). Key handling (t/Enter train, l reload, r refresh)
//! lives in `app::on_key_model`; this file is render-only.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Wrap};

use super::titled_block;
use crate::app::App;

/// Render the Model screen into `area`: a left status/actions column and a right
/// metrics column (headline numbers + the per-model hold-out table).
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let [status_area, metrics_area] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);
    render_status(frame, status_area, app);
    render_metrics(frame, metrics_area, app);
}

// --------------------------------------------------------------------------- //
// LEFT: status + actions
// --------------------------------------------------------------------------- //

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let m = &app.model;
    let mut lines: Vec<Line> = Vec::new();

    // Big, unmissable load-state banner.
    if m.model_loaded {
        lines.push(Line::from(Span::styled(
            "● MODEL READY",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "○ NO MODEL — train one",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::raw(""));

    lines.push(kv("Roster size", &format!("{} fighters", m.n_fighters)));

    // Headline metric pulled straight from the metrics blob, when present.
    if let Some(acc) = metric_f64(m.metrics.as_ref(), "test_accuracy") {
        lines.push(kv("Test accuracy", &fmt_pct(acc)));
    }
    if let Some(best) = metric_str(m.metrics.as_ref(), "best_model") {
        lines.push(kv("Best model", &best));
    }

    if let Some(path) = &m.model_path {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "Model file",
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(Span::styled(
            path.clone(),
            Style::default().fg(Color::White),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Actions",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(action("t / ⏎", "train the model (streams in the overlay)"));
    lines.push(action("l", "reload the on-disk model into the sidecar"));
    lines.push(action("r", "refresh status from the sidecar"));

    let p = Paragraph::new(lines)
        .block(titled_block("Model status"))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// RIGHT: metrics (headline summary + per-model table)
// --------------------------------------------------------------------------- //

fn render_metrics(frame: &mut Frame, area: Rect, app: &App) {
    let Some(metrics) = app.model.metrics.as_ref() else {
        let p = Paragraph::new(vec![
            Line::from(Span::styled(
                "No metrics yet.",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "Press t (or Enter) to train the model. Metrics appear here once",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "training finishes and the sidecar reloads.",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(titled_block("Metrics"))
        .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    };

    // If the trainer emitted a per-model table, give it its own panel; otherwise
    // the whole area is the flat summary.
    if has_model_table(metrics) {
        let [summary_area, table_area] =
            Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(area);
        render_summary(frame, summary_area, metrics);
        render_model_table(frame, table_area, metrics);
    } else {
        render_summary(frame, area, metrics);
    }
}

/// The flat, scalar part of the metrics blob: every top-level key whose value is
/// not itself an object (those — `models` / `baselines` — get the table panel).
fn render_summary(frame: &mut Frame, area: Rect, metrics: &serde_json::Value) {
    let lines: Vec<Line> = summary_lines(metrics);
    let p = Paragraph::new(lines)
        .block(titled_block("Metrics"))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

/// Build the scalar summary lines (pure, unit-tested).
fn summary_lines(metrics: &serde_json::Value) -> Vec<Line<'static>> {
    let serde_json::Value::Object(map) = metrics else {
        // Non-object metrics: show the raw rendering.
        return vec![Line::raw(metrics.to_string())];
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    for (key, value) in map {
        // Nested objects are rendered by the table panel, not here.
        if value.is_object() {
            continue;
        }
        out.push(metric_kv(key, value));
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled(
            "(no scalar metrics)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    out
}

/// Render the `models` map (per-model accuracy / log-loss / brier) as a table,
/// starring the `best_model`, with the baselines appended below.
fn render_model_table(frame: &mut Frame, area: Rect, metrics: &serde_json::Value) {
    let rows = model_rows(metrics);
    if rows.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "(no per-model breakdown)",
            Style::default().fg(Color::DarkGray),
        )))
        .block(titled_block("Per-model hold-out"));
        frame.render_widget(p, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("model"),
        Cell::from("accuracy"),
        Cell::from("log-loss"),
        Cell::from("brier"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let table_rows: Vec<Row> = rows
        .into_iter()
        .map(|r| {
            let style = if r.is_best {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if r.is_baseline {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(r.name),
                Cell::from(r.accuracy),
                Cell::from(r.log_loss),
                Cell::from(r.brier),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Percentage(40),
        Constraint::Percentage(20),
        Constraint::Percentage(20),
        Constraint::Percentage(20),
    ];
    let table = Table::new(table_rows, widths)
        .header(header)
        .block(titled_block("Per-model hold-out (★ = best)"));
    frame.render_widget(table, area);
}

// --------------------------------------------------------------------------- //
// PURE helpers (formatting + JSON extraction; unit-tested below).
// --------------------------------------------------------------------------- //

/// One assembled row of the per-model table.
#[derive(Debug, Clone, PartialEq)]
struct ModelRow {
    name: String,
    accuracy: String,
    log_loss: String,
    brier: String,
    is_best: bool,
    is_baseline: bool,
}

/// Whether the metrics blob carries a per-model breakdown worth tabulating.
fn has_model_table(metrics: &serde_json::Value) -> bool {
    metrics
        .get("models")
        .and_then(|v| v.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false)
        || metrics
            .get("baselines")
            .and_then(|v| v.as_object())
            .map(|m| !m.is_empty())
            .unwrap_or(false)
}

/// Flatten `models` (model -> {accuracy, log_loss, brier}) and `baselines`
/// (name -> accuracy) into ordered rows; the `best_model` row is flagged.
fn model_rows(metrics: &serde_json::Value) -> Vec<ModelRow> {
    let mut rows: Vec<ModelRow> = Vec::new();
    let best = metric_str(Some(metrics), "best_model");

    if let Some(models) = metrics.get("models").and_then(|v| v.as_object()) {
        for (name, m) in models {
            let acc = m.get("accuracy").and_then(serde_json::Value::as_f64);
            let ll = m.get("log_loss").and_then(serde_json::Value::as_f64);
            let br = m.get("brier").and_then(serde_json::Value::as_f64);
            let is_best = best.as_deref() == Some(name.as_str());
            let display = if is_best {
                format!("{name} ★")
            } else {
                name.clone()
            };
            rows.push(ModelRow {
                name: display,
                accuracy: opt_num(acc, 4),
                log_loss: opt_num(ll, 4),
                brier: opt_num(br, 4),
                is_best,
                is_baseline: false,
            });
        }
    }

    if let Some(baselines) = metrics.get("baselines").and_then(|v| v.as_object()) {
        for (name, v) in baselines {
            let acc = v.as_f64();
            rows.push(ModelRow {
                name: format!("baseline: {name}"),
                accuracy: opt_num(acc, 4),
                log_loss: "—".to_string(),
                brier: "—".to_string(),
                is_best: false,
                is_baseline: true,
            });
        }
    }

    rows
}

/// Render one scalar metric "key: value" line with a friendly label and a
/// value formatted for its kind (accuracy as a percent, ints as ints).
fn metric_kv(key: &str, value: &serde_json::Value) -> Line<'static> {
    let label = friendly_label(key);
    let rendered = format_metric_value(key, value);
    Line::from(vec![
        Span::styled(format!("{label:<22}"), Style::default().fg(Color::Gray)),
        Span::styled(rendered, Style::default().add_modifier(Modifier::BOLD)),
    ])
}

/// Map a raw metrics key to a human label; unknown keys pass through unchanged.
fn friendly_label(key: &str) -> String {
    match key {
        "test_accuracy" => "Test accuracy".to_string(),
        "test_log_loss" => "Test log-loss".to_string(),
        "test_brier" => "Test brier".to_string(),
        "best_model" => "Best model".to_string(),
        "n_train" => "Train rows".to_string(),
        "n_test" => "Test rows".to_string(),
        "test_cutoff" => "Test cutoff".to_string(),
        other => other.to_string(),
    }
}

/// Format a scalar metric value, special-casing the percent-style accuracy keys.
fn format_metric_value(key: &str, value: &serde_json::Value) -> String {
    let is_pct = matches!(key, "test_accuracy");
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if is_pct {
                    fmt_pct(f)
                } else if n.is_i64() || n.is_u64() {
                    n.to_string()
                } else {
                    fmt_num(f, 4)
                }
            } else {
                n.to_string()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "—".to_string(),
        other => other.to_string(),
    }
}

/// Extract a top-level f64 metric (handles ints) from the blob.
fn metric_f64(metrics: Option<&serde_json::Value>, key: &str) -> Option<f64> {
    metrics?.get(key).and_then(serde_json::Value::as_f64)
}

/// Extract a top-level string metric from the blob.
fn metric_str(metrics: Option<&serde_json::Value>, key: &str) -> Option<String> {
    metrics?
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Format a 0..1 fraction as a percentage, e.g. `0.624 -> "62.4%"`.
fn fmt_pct(v: f64) -> String {
    if v.is_finite() {
        format!("{:.1}%", v * 100.0)
    } else {
        "—".to_string()
    }
}

/// Format a float to `places` decimals, or `—` when non-finite.
fn fmt_num(v: f64, places: usize) -> String {
    if v.is_finite() {
        format!("{v:.places$}")
    } else {
        "—".to_string()
    }
}

/// Format an optional float, or `—` when absent/non-finite.
fn opt_num(v: Option<f64>, places: usize) -> String {
    match v {
        Some(f) => fmt_num(f, places),
        None => "—".to_string(),
    }
}

fn kv(key: &str, val: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<14}"), Style::default().fg(Color::Gray)),
        Span::styled(
            val.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ])
}

fn action(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<8}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fmt_pct_and_num() {
        assert_eq!(fmt_pct(0.624), "62.4%");
        assert_eq!(fmt_pct(1.0), "100.0%");
        assert_eq!(fmt_pct(f64::NAN), "—");
        assert_eq!(fmt_num(0.51234, 4), "0.5123");
        assert_eq!(fmt_num(f64::INFINITY, 2), "—");
        assert_eq!(opt_num(None, 4), "—");
        assert_eq!(opt_num(Some(0.5), 2), "0.50");
    }

    #[test]
    fn friendly_labels_known_and_passthrough() {
        assert_eq!(friendly_label("test_accuracy"), "Test accuracy");
        assert_eq!(friendly_label("best_model"), "Best model");
        assert_eq!(friendly_label("something_else"), "something_else");
    }

    #[test]
    fn format_value_specialcases_accuracy_and_ints() {
        assert_eq!(format_metric_value("test_accuracy", &json!(0.6)), "60.0%");
        // Non-accuracy float keeps 4 decimals.
        assert_eq!(
            format_metric_value("test_log_loss", &json!(0.6543)),
            "0.6543"
        );
        // Integers render as integers (no decimals).
        assert_eq!(format_metric_value("n_train", &json!(1200)), "1200");
        // Strings pass through; null -> dash.
        assert_eq!(
            format_metric_value("best_model", &json!("gboost")),
            "gboost"
        );
        assert_eq!(
            format_metric_value("whatever", &serde_json::Value::Null),
            "—"
        );
    }

    #[test]
    fn metric_extractors_pull_scalars() {
        let m = json!({"test_accuracy": 0.62, "best_model": "logreg", "n_train": 900});
        assert_eq!(metric_f64(Some(&m), "test_accuracy"), Some(0.62));
        assert_eq!(metric_f64(Some(&m), "n_train"), Some(900.0));
        assert_eq!(metric_f64(Some(&m), "missing"), None);
        assert_eq!(
            metric_str(Some(&m), "best_model").as_deref(),
            Some("logreg")
        );
        assert_eq!(metric_str(None, "best_model"), None);
    }

    #[test]
    fn summary_skips_nested_objects() {
        let m = json!({
            "test_accuracy": 0.62,
            "best_model": "gboost",
            "models": {"gboost": {"accuracy": 0.62}},
            "baselines": {"base_rate": 0.5}
        });
        let lines = summary_lines(&m);
        // 2 scalar keys -> 2 lines; nested objects excluded.
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn summary_handles_only_nested() {
        let m = json!({"models": {"x": {"accuracy": 0.5}}});
        let lines = summary_lines(&m);
        assert_eq!(lines.len(), 1); // the "(no scalar metrics)" placeholder
    }

    #[test]
    fn summary_handles_non_object() {
        let lines = summary_lines(&json!("oops"));
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn has_model_table_detects_models_or_baselines() {
        assert!(has_model_table(&json!({"models": {"a": {}}})));
        assert!(has_model_table(&json!({"baselines": {"b": 0.5}})));
        assert!(!has_model_table(&json!({"models": {}})));
        assert!(!has_model_table(&json!({"test_accuracy": 0.6})));
    }

    #[test]
    fn model_rows_flag_best_and_baselines() {
        let m = json!({
            "best_model": "gboost",
            "models": {
                "gboost": {"accuracy": 0.63, "log_loss": 0.64, "brier": 0.23},
                "logreg": {"accuracy": 0.60, "log_loss": 0.66, "brier": 0.24}
            },
            "baselines": {"higher_elo": 0.58}
        });
        let rows = model_rows(&m);
        assert_eq!(rows.len(), 3);

        let best = rows.iter().find(|r| r.is_best).expect("a best row");
        assert!(best.name.starts_with("gboost"));
        assert!(best.name.contains('★'));
        assert_eq!(best.accuracy, "0.6300");
        assert_eq!(best.log_loss, "0.6400");

        let base = rows.iter().find(|r| r.is_baseline).expect("a baseline row");
        assert_eq!(base.name, "baseline: higher_elo");
        assert_eq!(base.accuracy, "0.5800");
        assert_eq!(base.log_loss, "—"); // baselines have no log-loss/brier
    }

    #[test]
    fn model_rows_empty_without_breakdown() {
        assert!(model_rows(&json!({"test_accuracy": 0.6})).is_empty());
    }
}
