//! Predict screen renderer (REDESIGN).
//!
//! Reached from the Home menu. Two fighter pickers with live fuzzy autocomplete
//! sit on top; below them a fight-poster style VS banner, the win-probability
//! split + gauge, and a side-by-side tale-of-the-tape with plain-English stat
//! explanations (via `stats_text`).
//!
//! ELIGIBILITY: once a slot is committed, the OTHER slot's candidate POOL is the
//! set of eligible opponents computed LOCALLY from the eligibility POLICY +
//! per-fighter divisions the TUI fetched ONCE at startup (the policy is the single
//! source of truth in `ml/predict.py`; Rust just applies the fetched `rules`), so
//! an ineligible opponent can never be picked. The "matchup not allowed" branch in
//! `render_probability` is therefore only a DEFENSIVE fallback that should not
//! appear in normal use. Low-confidence (allowed) cases are still surfaced inline.
//!
//! Key handling lives in `app::on_key_predict`; this module only renders. The
//! persistent footer (controls) is drawn by `ui::mod`, so we never draw our own.
//! All state is read from `app.predict` (the migrated `PredictState`).

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, List, ListItem, ListState, Paragraph, Wrap};

use super::{focusable_block, titled_block};
use crate::app::{App, PredictSlot};
use crate::models::{PredictResult, TaleOfTape};
use crate::stats_text;

/// Accent for fighter A throughout the screen (corner, gauge, tale).
const ACCENT_A: Color = Color::Green;
/// Accent for fighter B throughout the screen.
const ACCENT_B: Color = Color::Red;

/// Render the Predict screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // class selector (fixed) | pickers (fixed) | VS banner (fixed) | output (rest)
    let [class, pickers, banner, output] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(11),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .areas(area);

    render_class_selector(frame, class, app);
    render_pickers(frame, pickers, app);
    render_banner(frame, banner, app);
    render_output(frame, output, app);
}

// --------------------------------------------------------------------------- //
// WEIGHT-CLASS SELECTOR
// --------------------------------------------------------------------------- //

/// A single-line chip row: "All weight classes" followed by each fetched class
/// name, with the active selection highlighted. The classes come ENTIRELY from
/// the sidecar-fetched `eligibility.weight_classes` (no names hardcoded here);
/// when none were fetched only "All weight classes" shows. ⇥ cycles the chips
/// (see the footer); the fighter pickers below reflect the active filter.
fn render_class_selector(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app.predict.weight_class; // None = "All weight classes"

    // Highlight style for the active chip; dim for the rest.
    let on = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let off = Style::default().fg(Color::Gray);

    let mut spans: Vec<Span> = Vec::new();
    // The "All" chip (no filter).
    spans.push(Span::styled(
        " All weight classes ",
        if selected.is_none() { on } else { off },
    ));
    for (i, wc) in app.eligibility.weight_classes.iter().enumerate() {
        // Member count for the chip, derived from the fetched divisions (no
        // hardcoded membership): how many roster fighters fought in this class.
        let count = app.eligibility.in_class(wc, &app.roster).len();
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {} ({count}) ", wc.name),
            if selected == Some(i) { on } else { off },
        ));
    }

    let title = match app.selected_weight_class() {
        Some(wc) => format!("Weight class — {} (⇥ change)", wc.name),
        None => "Weight class — All (⇥ change)".to_string(),
    };
    let p = Paragraph::new(Line::from(spans))
        .block(titled_block(&title))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// PICKERS
// --------------------------------------------------------------------------- //

fn render_pickers(frame: &mut Frame, area: Rect, app: &App) {
    let [slot_a, slot_b] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    render_slot(frame, slot_a, app, PredictSlot::A);
    render_slot(frame, slot_b, app, PredictSlot::B);
}

fn render_slot(frame: &mut Frame, area: Rect, app: &App, slot: PredictSlot) {
    let focused = app.predict.slot == slot;
    let (chosen, label, accent) = match slot {
        PredictSlot::A => (app.predict.name_a.as_deref(), "Fighter A", ACCENT_A),
        PredictSlot::B => (app.predict.name_b.as_deref(), "Fighter B", ACCENT_B),
    };

    let [header, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

    // Header shows the chosen name, or the live query when this slot is focused.
    let header_line = if let Some(name) = chosen {
        Line::from(vec![
            Span::styled("✓ ", Style::default().fg(accent)),
            Span::styled(
                name.to_string(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
        ])
    } else if focused {
        Line::from(vec![
            Span::raw(app.predict.query.as_str()),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ])
    } else {
        Line::styled("(not selected)", Style::default().fg(Color::DarkGray))
    };
    let head = Paragraph::new(header_line).block(focusable_block(label, focused));
    frame.render_widget(head, header);

    // Candidate list only for the focused slot (autocomplete).
    if focused {
        if app.roster.is_empty() {
            let hint = Paragraph::new(Line::styled(
                "Roster unavailable — train or reload the model first (Model screen).",
                Style::default().fg(Color::Yellow),
            ))
            .block(titled_block("Matches"))
            .wrap(Wrap { trim: true });
            frame.render_widget(hint, body);
            return;
        }
        let items: Vec<ListItem> = app
            .predict
            .candidates
            .iter()
            .take(200)
            .map(|n| ListItem::new(n.as_str()))
            .collect();
        let count = app.predict.candidates.len();
        let list = List::new(items)
            .block(titled_block(&format!("Matches ({count})")))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        let mut state = ListState::default();
        if count > 0 {
            state.select(Some(app.predict.selected.min(count - 1)));
        }
        frame.render_stateful_widget(list, body, &mut state);
    } else {
        let hint = Paragraph::new(Line::styled(
            "Press ←/→ to focus this slot, then type to search.",
            Style::default().fg(Color::DarkGray),
        ))
        .block(titled_block("Matches"))
        .wrap(Wrap { trim: true });
        frame.render_widget(hint, body);
    }
}

// --------------------------------------------------------------------------- //
// VS BANNER (fight-poster framing)
// --------------------------------------------------------------------------- //

/// A compact "A  VS  B" banner that frames the matchup like a fight poster.
fn render_banner(frame: &mut Frame, area: Rect, app: &App) {
    let a = app.predict.name_a.as_deref().unwrap_or("—");
    let b = app.predict.name_b.as_deref().unwrap_or("—");
    let line = Line::from(vec![
        Span::styled(
            a.to_string(),
            Style::default().fg(ACCENT_A).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   VS   ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            b.to_string(),
            Style::default().fg(ACCENT_B).add_modifier(Modifier::BOLD),
        ),
    ])
    .alignment(Alignment::Center);
    let p = Paragraph::new(line).block(titled_block("Tale of the tape"));
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// OUTPUT (error / prompt / result)
// --------------------------------------------------------------------------- //

fn render_output(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(err) = &app.predict.error {
        let p = Paragraph::new(vec![
            Line::styled(
                "Prediction unavailable",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::raw(err.clone()),
        ])
        .block(titled_block("Result"))
        .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    }

    let Some(res) = &app.predict.result else {
        let msg = if app.predict.both_selected() {
            "Both fighters chosen — running prediction..."
        } else {
            "Choose two fighters above to run a prediction. \
             Type to fuzzy-search, ↑↓ to pick, ⏎ to choose, ←→ to switch slots."
        };
        let p = Paragraph::new(msg)
            .block(titled_block("Result"))
            .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    };

    let [prob_area, tale_area] =
        Layout::vertical([Constraint::Length(7), Constraint::Min(0)]).areas(area);
    render_probability(frame, prob_area, res);
    render_tale(frame, tale_area, res);
}

fn render_probability(frame: &mut Frame, area: Rect, res: &PredictResult) {
    let [info_area, gauge_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Length(3)]).areas(area);

    let mut lines: Vec<Line> = Vec::new();

    if !res.allowed {
        lines.push(Line::styled(
            "Matchup not allowed",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(reason) = &res.reason {
            lines.push(Line::raw(reason.clone()));
        }
    } else {
        let pa = res.prob_a.unwrap_or(f64::NAN);
        let pb = res.prob_b.unwrap_or(f64::NAN);
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", res.name_a), Style::default().fg(ACCENT_A)),
            Span::styled(pct(pa), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("    "),
            Span::styled(format!("{}: ", res.name_b), Style::default().fg(ACCENT_B)),
            Span::styled(pct(pb), Style::default().add_modifier(Modifier::BOLD)),
        ]));
        if res.low_confidence {
            lines.push(Line::styled(
                "⚠ Low confidence prediction — treat this as a rough lean, not a strong pick.",
                Style::default().fg(Color::Yellow),
            ));
        }
        if let Some(reason) = &res.reason {
            lines.push(Line::styled(
                reason.clone(),
                Style::default().fg(Color::Gray),
            ));
        }
        let mut meta: Vec<Span> = Vec::new();
        if let Some(model) = &res.model {
            meta.push(Span::styled(
                format!("model: {model}  "),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if let Some(acc) = res.test_accuracy {
            meta.push(Span::styled(
                format!("test acc: {:.1}%  ", acc * 100.0),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if let Some(d) = res.distance {
            meta.push(Span::styled(
                format!("division distance: {d}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !meta.is_empty() {
            lines.push(Line::from(meta));
        }
    }

    let info = Paragraph::new(lines)
        .block(titled_block("Win probability"))
        .wrap(Wrap { trim: true });
    frame.render_widget(info, info_area);

    // Gauge for P(A wins) when allowed and finite.
    if res.allowed {
        let ratio = res
            .prob_a
            .filter(|p| p.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(ACCENT_A).bg(ACCENT_B))
            .ratio(ratio)
            .label(format!(
                "{}  {} | {}  {}",
                res.name_a,
                pct(res.prob_a.unwrap_or(f64::NAN)),
                pct(res.prob_b.unwrap_or(f64::NAN)),
                res.name_b
            ));
        frame.render_widget(gauge, gauge_area);
    }
}

// --------------------------------------------------------------------------- //
// TALE OF THE TAPE
// --------------------------------------------------------------------------- //

fn render_tale(frame: &mut Frame, area: Rect, res: &PredictResult) {
    let [a_area, b_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    render_tale_col(frame, a_area, &res.name_a, res.tale_a.as_ref(), ACCENT_A);
    render_tale_col(frame, b_area, &res.name_b, res.tale_b.as_ref(), ACCENT_B);
}

fn render_tale_col(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    tale: Option<&TaleOfTape>,
    accent: Color,
) {
    let title = name.to_string();
    let mut lines: Vec<Line> = Vec::new();
    match tale {
        None => lines.push(Line::styled(
            "No tale-of-the-tape data.",
            Style::default().fg(Color::DarkGray),
        )),
        Some(t) => {
            lines.push(tape_line(
                "Record",
                t.record.clone().unwrap_or_else(|| "—".into()),
            ));
            lines.push(tape_stat("Elo rating", "elo", t.elo));
            lines.push(tape_stat("Age", "age", t.age));
            lines.push(tape_stat("Reach", "reach_in", t.reach_in));
            lines.push(tape_stat("Height", "height_in", t.height_in));
            lines.push(tape_line(
                "Stance",
                t.stance.clone().unwrap_or_else(|| "—".into()),
            ));
            lines.push(tape_stat(
                "Recent win rate",
                "recent_winrate",
                t.recent_winrate,
            ));
            lines.push(tape_stat("Form (trend)", "form_delta", t.form_delta));
            lines.push(tape_stat("Layoff", "layoff_days", t.layoff_days));
            let divs = if t.divisions.is_empty() {
                "—".to_string()
            } else {
                t.divisions.join(", ")
            };
            lines.push(tape_line("Divisions", divs));
        }
    }
    let block = titled_block(&title).border_style(Style::default().fg(accent));
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

/// A tale line whose value is a numeric stat — uses stats_text for units.
fn tape_stat(label: &str, key: &str, value: Option<f64>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(Color::Gray)),
        Span::styled(
            stats_text::describe(key, value),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ])
}

/// A tale line whose value is already a string.
fn tape_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(Color::Gray)),
        Span::styled(value, Style::default().add_modifier(Modifier::BOLD)),
    ])
}

/// Format a probability (0..1) as a percent, or "—" when missing/non-finite.
fn pct(p: f64) -> String {
    if p.is_finite() {
        format!("{:.0}%", p * 100.0)
    } else {
        "—".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_formats_finite_and_missing() {
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(0.5), "50%");
        assert_eq!(pct(1.0), "100%");
        // rounds to whole percent
        assert_eq!(pct(0.476), "48%");
        // non-finite / missing render as an em dash, never "NaN%"
        assert_eq!(pct(f64::NAN), "—");
        assert_eq!(pct(f64::INFINITY), "—");
    }
}
