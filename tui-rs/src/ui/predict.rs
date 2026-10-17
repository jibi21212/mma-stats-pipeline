//! Predict screen renderer (REDESIGN).
//!
//! Reached from the Home menu. Two fighter pickers with live fuzzy autocomplete
//! sit on top; below them a fight-poster style VS banner, the win-probability
//! split + gauge, an "If X wins" method card for EACH fighter, and a side-by-side
//! tale-of-the-tape with plain-English stat explanations (via `stats_text`).
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
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use super::{focusable_block, titled_block};
use crate::app::{App, PredictSlot, PredictView, UpcomingFightPrediction};
use crate::models::{MethodProbs, MethodSplit, PredictResult, TaleOfTape};
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

    // The snapshot picker is a modal overlay drawn last, over everything.
    if app.predict.picker_open {
        render_snapshot_picker(frame, area, app);
    }
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
    if app.predict.view == PredictView::UpcomingCard {
        render_upcoming_card(frame, area, app);
        return;
    }

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

    let info = info_lines(res);
    if !res.allowed {
        // Defensive refusal: just the explanation, then the tale.
        let [info_area, tale_area] = Layout::vertical([
            Constraint::Length(info.len() as u16 + 2),
            Constraint::Min(0),
        ])
        .areas(area);
        render_info(frame, info_area, info);
        render_tale(frame, tale_area, res);
        return;
    }

    let method_rows = match &res.method_probs {
        Some(mp) if mp.a_given_win.is_some() && mp.b_given_win.is_some() => WIN_CARD_ROWS,
        Some(_) => LEGACY_METHOD_ROWS,
        None => 0,
    };
    let rows = result_rows(area.height, info.len() as u16, method_rows);
    let [info_area, gauge_area, method_area, tale_area] = Layout::vertical([
        Constraint::Length(rows.info),
        Constraint::Length(rows.gauge),
        Constraint::Length(rows.method),
        Constraint::Min(0),
    ])
    .areas(area);
    let info_hidden = rows.info == 0;
    if !info_hidden {
        render_info(frame, info_area, info);
    }
    render_gauge(frame, gauge_area, res, info_hidden && res.low_confidence);
    render_method(frame, method_area, res);
    render_tale(frame, tale_area, res);
}

/// Rows of the 3-line-per-fighter "If X wins" cards (border + 3 methods + best path).
const WIN_CARD_ROWS: u16 = 6;
/// Rows of the legacy joint-only method block (border + 2 fighters + most likely).
const LEGACY_METHOD_ROWS: u16 = 5;

/// How many terminal rows each result block gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResultRows {
    info: u16,
    gauge: u16,
    method: u16,
}

/// Split `height` rows between the result blocks, most important first so a
/// short terminal still shows who wins and how: the method cards (which carry
/// each fighter's win % in their titles), then a 1-row gauge, then the "Win
/// probability" box (`info_lines` + borders, all or nothing), then a taller
/// 3-row gauge. The tale of the tape takes whatever is left.
fn result_rows(height: u16, info_lines: u16, method_rows: u16) -> ResultRows {
    let mut rem = height;
    let method = method_rows.min(rem);
    rem -= method;
    let mut gauge = rem.min(1);
    rem -= gauge;
    let info_need = info_lines + 2;
    let info = if rem >= info_need { info_need } else { 0 };
    rem -= info;
    if gauge == 1 && rem >= 2 {
        gauge = 3;
    }
    ResultRows {
        info,
        gauge,
        method,
    }
}

/// The "Win probability" box content: snapshot labels, the win split (or the
/// refusal), the low-confidence warning, and the model metadata.
fn info_lines(res: &PredictResult) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    // Which snapshot each side is using (only when not the plain "current" view).
    let la = res.snap_a_label.as_deref().unwrap_or("current");
    let lb = res.snap_b_label.as_deref().unwrap_or("current");
    if la != "current" || lb != "current" {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: {la}", res.name_a),
                Style::default().fg(ACCENT_A),
            ),
            Span::styled("   ", Style::default()),
            Span::styled(
                format!("{}: {lb}", res.name_b),
                Style::default().fg(ACCENT_B),
            ),
        ]));
    }

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
        return lines;
    }

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
    lines
}

fn render_info(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let info = Paragraph::new(lines)
        .block(titled_block("Win probability"))
        .wrap(Wrap { trim: true });
    frame.render_widget(info, area);
}

/// Gauge for P(A wins). `warn` appends a low-confidence marker to the label for
/// when the "Win probability" box (which normally carries the warning) is hidden.
fn render_gauge(frame: &mut Frame, area: Rect, res: &PredictResult, warn: bool) {
    let ratio = res
        .prob_a
        .filter(|p| p.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let mut label = format!(
        "{}  {} | {}  {}",
        res.name_a,
        pct(res.prob_a.unwrap_or(f64::NAN)),
        pct(res.prob_b.unwrap_or(f64::NAN)),
        res.name_b
    );
    if warn {
        label.push_str("  ⚠ low confidence");
    }
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(ACCENT_A).bg(ACCENT_B))
        .ratio(ratio)
        .label(label);
    frame.render_widget(gauge, area);
}

// --------------------------------------------------------------------------- //
// UPCOMING CARD
// --------------------------------------------------------------------------- //

fn render_upcoming_card(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(err) = &app.predict.card_error {
        let p = Paragraph::new(vec![
            Line::styled(
                "Upcoming card unavailable",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::raw(err.clone()),
        ])
        .block(titled_block("Upcoming card predictions"))
        .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    }

    let Some(event) = &app.predict.card_event else {
        let p = Paragraph::new("Press ^U to predict the next scraped upcoming card.")
            .block(titled_block("Upcoming card predictions"))
            .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    };

    let rows = &app.predict.card_predictions;
    let predicted = rows
        .iter()
        .filter(|row| row.result.as_ref().map(|r| r.allowed).unwrap_or(false))
        .count();
    let date = event.date.as_deref().unwrap_or("date unknown");
    let location = event.location.as_deref().unwrap_or("location unknown");

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                event.title.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {date}  {location}"),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::styled(
            format!("Predicted {predicted}/{} fights", rows.len()),
            Style::default().fg(Color::DarkGray),
        ),
        Line::raw(""),
    ];

    for (idx, row) in rows.iter().enumerate() {
        lines.push(card_prediction_line(idx + 1, row));
    }

    let p = Paragraph::new(lines)
        .block(titled_block("Upcoming card predictions"))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn card_prediction_line(idx: usize, row: &UpcomingFightPrediction) -> Line<'static> {
    let a = row.fighter_a.as_deref().unwrap_or("Unknown");
    let b = row.fighter_b.as_deref().unwrap_or("Unknown");

    if let Some(res) = &row.result {
        if res.allowed {
            return Line::from(vec![
                Span::styled(format!("{idx:>2}. "), Style::default().fg(Color::DarkGray)),
                Span::styled(a.to_string(), Style::default().fg(ACCENT_A)),
                Span::raw(format!(" {}  |  ", pct(res.prob_a.unwrap_or(f64::NAN)))),
                Span::styled(b.to_string(), Style::default().fg(ACCENT_B)),
                Span::raw(format!(" {}", pct(res.prob_b.unwrap_or(f64::NAN)))),
            ]);
        }

        let reason = res.reason.as_deref().unwrap_or("not allowed by model gate");
        return Line::from(vec![
            Span::styled(format!("{idx:>2}. "), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{a} vs {b}"), Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("  not predicted: {reason}"),
                Style::default().fg(Color::Gray),
            ),
        ]);
    }

    let label = match (&row.fighter_a, &row.fighter_b) {
        (Some(a), Some(b)) => format!("{a} vs {b}"),
        _ => row
            .fight
            .weight_class
            .clone()
            .unwrap_or_else(|| "fight row".to_string()),
    };
    let err = row.error.as_deref().unwrap_or("prediction failed");
    Line::from(vec![
        Span::styled(format!("{idx:>2}. "), Style::default().fg(Color::DarkGray)),
        Span::styled(label, Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("  skipped: {err}"),
            Style::default().fg(Color::Gray),
        ),
    ])
}

// --------------------------------------------------------------------------- //
// METHOD OF VICTORY
// --------------------------------------------------------------------------- //

/// How each fighter wins: one "If X wins" card per fighter, side by side, so the
/// underdog gets the same treatment as the favourite. A sidecar that predates the
/// per-fighter conditional split falls back to the joint-only lines.
fn render_method(frame: &mut Frame, area: Rect, res: &PredictResult) {
    let Some(mp) = &res.method_probs else { return };
    match (&mp.a_given_win, &mp.b_given_win) {
        (Some(ga), Some(gb)) => {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(area);
            render_win_card(frame, left, &res.name_a, res.prob_a, ga, &mp.a, ACCENT_A);
            render_win_card(frame, right, &res.name_b, res.prob_b, gb, &mp.b, ACCENT_B);
        }
        _ => render_method_joint(frame, area, res, mp),
    }
}

/// The method labels in display order, paired with their `MethodProbs` field.
fn method_rows(mp: &MethodProbs) -> [(&'static str, Option<f64>); 3] {
    [
        ("decision", mp.decision),
        ("KO/TKO", mp.ko_tko),
        ("submission", mp.submission),
    ]
}

/// Index of the largest finite value, i.e. a fighter's most likely way to win.
fn best_index(values: &[Option<f64>]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.filter(|p| p.is_finite()).map(|p| (i, p)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

/// A horizontal share bar `width` cells wide, filled in proportion to `share`.
fn share_bar(share: Option<f64>, width: usize) -> (String, String) {
    let ratio = share
        .filter(|p| p.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    ("█".repeat(filled), "░".repeat(width - filled))
}

/// One fighter's "If X wins · 62%" card: how they win as a share of THEIR wins
/// (the three rows sum to 100%), their most likely path highlighted, and that
/// path's joint probability across every outcome of the fight.
fn render_win_card(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    prob_win: Option<f64>,
    given: &MethodProbs,
    joint: &MethodProbs,
    accent: Color,
) {
    let rows = method_rows(given);
    let best = best_index(&rows.map(|(_, p)| p));
    // "submission" (10) + gap + bar + gap + "100%" (4).
    let bar_w = (area.width.saturating_sub(2) as usize).saturating_sub(16);

    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, (label, share))| {
            let label_style = if Some(i) == best {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let (filled, empty) = share_bar(*share, bar_w);
            let mut spans = vec![Span::styled(format!("{label:<10} "), label_style)];
            if bar_w > 0 {
                spans.push(Span::styled(filled, Style::default().fg(accent)));
                spans.push(Span::styled(empty, Style::default().fg(Color::DarkGray)));
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("{:>4}", pct(share.unwrap_or(f64::NAN))),
                label_style,
            ));
            Line::from(spans)
        })
        .collect();

    if let Some(i) = best {
        let (label, _) = rows[i];
        let overall = method_rows(joint)[i].1.unwrap_or(f64::NAN);
        lines.push(Line::from(vec![
            Span::styled("Best path: ", Style::default().fg(Color::Gray)),
            Span::styled(
                label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} overall", pct(overall)),
                Style::default().fg(Color::Gray),
            ),
        ]));
    }

    let title = format!("If {name} wins · {}", pct(prob_win.unwrap_or(f64::NAN)));
    let card =
        Paragraph::new(lines).block(titled_block(&title).border_style(Style::default().fg(accent)));
    frame.render_widget(card, area);
}

/// Legacy view for a sidecar without the per-fighter split: each fighter's JOINT
/// method probabilities (all six sum to 100%) and the single most likely outcome.
fn render_method_joint(frame: &mut Frame, area: Rect, res: &PredictResult, mp: &MethodSplit) {
    let mut lines = vec![
        method_line(&res.name_a, &mp.a, ACCENT_A),
        method_line(&res.name_b, &mp.b, ACCENT_B),
    ];
    // The most-likely single outcome across all six.
    let outcomes = [
        (mp.a.decision, res.name_a.as_str(), "decision"),
        (mp.a.ko_tko, res.name_a.as_str(), "KO/TKO"),
        (mp.a.submission, res.name_a.as_str(), "submission"),
        (mp.b.decision, res.name_b.as_str(), "decision"),
        (mp.b.ko_tko, res.name_b.as_str(), "KO/TKO"),
        (mp.b.submission, res.name_b.as_str(), "submission"),
    ];
    let best = outcomes
        .iter()
        .filter_map(|(p, n, m)| p.map(|p| (p, *n, *m)))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if let Some((p, n, m)) = best {
        lines.push(Line::from(vec![
            Span::styled("Most likely: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{n} by {m} ({})", pct(p)),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    let para = Paragraph::new(lines)
        .block(titled_block("How it ends (all outcomes sum to 100%)"))
        .wrap(Wrap { trim: true });
    frame.render_widget(para, area);
}

/// One fighter's method line, e.g. "Name  decision 30% · KO/TKO 25% · submission 3%".
fn method_line(name: &str, mp: &MethodProbs, accent: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{name}  "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "decision {} · KO/TKO {} · submission {}",
            pct(mp.decision.unwrap_or(f64::NAN)),
            pct(mp.ko_tko.unwrap_or(f64::NAN)),
            pct(mp.submission.unwrap_or(f64::NAN)),
        )),
    ])
}

// --------------------------------------------------------------------------- //
// SNAPSHOT PICKER (modal overlay — drill into a fighter's career)
// --------------------------------------------------------------------------- //

/// Modal overlay listing a fighter's selectable snapshots: Current, Prime, then
/// each past fight. Row 0/1 are the anchors; rows 2.. mirror `picker_snaps`.
fn render_snapshot_picker(frame: &mut Frame, area: Rect, app: &App) {
    let p = &app.predict;
    let (slot_name, accent) = match p.picker_slot {
        PredictSlot::A => (p.name_a.as_deref().unwrap_or("Fighter A"), ACCENT_A),
        PredictSlot::B => (p.name_b.as_deref().unwrap_or("Fighter B"), ACCENT_B),
    };
    let popup = centered_rect(72, 80, area);

    let mut items: Vec<ListItem> = Vec::with_capacity(p.picker_snaps.len() + 2);
    items.push(ListItem::new("Current  (latest version)"));
    items.push(ListItem::new("Prime  (best self)"));
    for s in &p.picker_snaps {
        let star = if s.is_prime { "  ★prime" } else { "" };
        let elo = s
            .elo
            .map(|e| format!("{e:.0}"))
            .unwrap_or_else(|| "—".into());
        items.push(ListItem::new(format!(
            "{}  {} vs {:<22} {:>7}  Elo {elo}  streak {}{star}",
            s.date, s.result, s.opponent, s.record, s.win_streak
        )));
    }

    let title =
        format!("Snapshot — {slot_name}   (↑↓ pick · ←→ switch fighter · ⏎ use · esc/^S close)");
    let list = List::new(items)
        .block(titled_block(&title).border_style(Style::default().fg(accent)))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(p.picker_selected));

    frame.render_widget(Clear, popup); // clear whatever is underneath
    frame.render_stateful_widget(list, popup, &mut state);
}

/// A centered sub-rectangle `percent_x` × `percent_y` of `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [v] = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .areas(area);
    let [h] = Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .areas(v);
    h
}

// --------------------------------------------------------------------------- //
// TALE OF THE TAPE
// --------------------------------------------------------------------------- //

fn render_tale(frame: &mut Frame, area: Rect, res: &PredictResult) {
    // Below borders + one row there is nothing to read, only a dangling border.
    if area.height < 3 {
        return;
    }
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

    #[test]
    fn result_rows_has_room_for_everything_when_tall() {
        // 23 rows (a 160x45 terminal): info (2 lines + borders), a 3-row gauge,
        // the 6-row method cards; the tale keeps the remaining 10.
        assert_eq!(
            result_rows(23, 2, WIN_CARD_ROWS),
            ResultRows {
                info: 4,
                gauge: 3,
                method: 6
            }
        );
    }

    #[test]
    fn result_rows_keeps_method_cards_and_gauge_when_short() {
        // 8 rows (an 80x30 terminal): the cards and a 1-row gauge win; the info
        // box is dropped whole rather than clipped to an empty border.
        assert_eq!(
            result_rows(8, 2, WIN_CARD_ROWS),
            ResultRows {
                info: 0,
                gauge: 1,
                method: 6
            }
        );
        // One more row is still not enough for the info box; it upgrades nothing.
        assert_eq!(result_rows(9, 2, WIN_CARD_ROWS).info, 0);
        // Enough for the info box -> it comes back before the taller gauge.
        assert_eq!(
            result_rows(11, 2, WIN_CARD_ROWS),
            ResultRows {
                info: 4,
                gauge: 1,
                method: 6
            }
        );
    }

    #[test]
    fn result_rows_never_exceeds_the_area() {
        for h in 0..30 {
            let r = result_rows(h, 3, WIN_CARD_ROWS);
            assert!(r.info + r.gauge + r.method <= h, "{h}: {r:?}");
        }
        // No method block -> nothing reserved for it.
        assert_eq!(result_rows(20, 2, 0).method, 0);
    }

    #[test]
    fn best_index_picks_the_largest_finite_share() {
        assert_eq!(best_index(&[Some(0.2), Some(0.5), Some(0.3)]), Some(1));
        assert_eq!(best_index(&[None, Some(f64::NAN), Some(0.1)]), Some(2));
        assert_eq!(best_index(&[None, None, None]), None);
    }

    #[test]
    fn share_bar_fills_in_proportion() {
        assert_eq!(share_bar(Some(0.5), 10), ("█".repeat(5), "░".repeat(5)));
        assert_eq!(share_bar(Some(1.0), 4), ("█".repeat(4), String::new()));
        assert_eq!(share_bar(None, 3), (String::new(), "░".repeat(3)));
        assert_eq!(share_bar(Some(f64::NAN), 0), (String::new(), String::new()));
    }
}
