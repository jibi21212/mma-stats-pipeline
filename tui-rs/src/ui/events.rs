//! Events screens: `render` = the events list (Database -> Browse events);
//! `render_card` = the fights on one selected event (Events -> a card).
//!
//! Browse path: events list (newest first) -> selected event's fights ->
//! selecting a fighter in a fight opens that fighter's profile. The list bodies
//! are kept thin; the load-bearing string formatting lives in pure helpers
//! (`event_summary`, `fight_summary`, `round_summary`) that are unit-tested.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::titled_block;
use crate::app::App;
use crate::db::parse_card_number;
use crate::models::{EventRow, FightRow, RoundStat};

/// Render the events LIST into `area` (Database -> Browse events).
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let events = &app.events.events;
    let count = events.len();

    let items: Vec<ListItem> = if events.is_empty() {
        vec![ListItem::new(Line::styled(
            "No events in the database. Run a Scrape first.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        events
            .iter()
            .map(|e| ListItem::new(event_line(e)))
            .collect()
    };

    let list = List::new(items)
        .block(titled_block(&format!("Events ({count})")))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    if count > 0 {
        state.select(Some(app.events.selected.min(count - 1)));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// One styled row for an event: date · title (numbered cards highlighted) · loc.
fn event_line(e: &EventRow) -> Line<'static> {
    let date = e.date.as_deref().unwrap_or("????-??-??").to_string();
    // Numbered cards (UFC 311, etc.) get a brighter title; named cards are plain.
    let title_style = if parse_card_number(&e.title).is_some() {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let mut spans = vec![
        Span::styled(format!("{date}  "), Style::default().fg(Color::DarkGray)),
        Span::styled(e.title.clone(), title_style),
    ];
    if let Some(loc) = e.location.as_deref().filter(|s| !s.is_empty()) {
        spans.push(Span::styled(
            format!("  · {loc}"),
            Style::default().fg(Color::Gray),
        ));
    }
    Line::from(spans)
}

/// Render the FIGHT CARD for one event into `area` (Events -> a card).
pub fn render_card(frame: &mut Frame, area: Rect, app: &App) {
    let [card_area, rounds_area] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);

    let title = app
        .event_fights
        .event
        .as_ref()
        .map(event_card_title)
        .unwrap_or_else(|| "Fight card".to_string());

    let fights = &app.event_fights.fights;
    let items: Vec<ListItem> = if fights.is_empty() {
        vec![ListItem::new(Line::styled(
            "No fights recorded for this event.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        fights
            .iter()
            .map(|fr| ListItem::new(fight_line(fr)))
            .collect()
    };
    let list = List::new(items)
        .block(titled_block(&title))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    if !fights.is_empty() {
        state.select(Some(app.event_fights.selected.min(fights.len() - 1)));
    }
    frame.render_stateful_widget(list, card_area, &mut state);

    render_rounds(frame, rounds_area, &app.event_fights.rounds);
}

/// Header for the fight-card pane: "UFC 311 — Location".
fn event_card_title(e: &EventRow) -> String {
    match e.location.as_deref().filter(|s| !s.is_empty()) {
        Some(loc) => format!("{} — {loc}", e.title),
        None => e.title.clone(),
    }
}

/// One styled row for a fight on the card. The plain text it carries is produced
/// by `fight_summary` (tested); the styling splits winner/loser for colour.
fn fight_line(fr: &FightRow) -> Line<'static> {
    let winner = fr.winner_name.clone().filter(|s| !s.is_empty());
    let loser = fr.loser_name.clone().filter(|s| !s.is_empty());
    let title_mark = if fr.title_bout != 0 { " 🏆" } else { "" };
    let detail = fight_detail(fr);

    match (winner, loser) {
        (Some(w), Some(l)) => Line::from(vec![
            Span::styled(format!("{w} "), Style::default().fg(Color::Green)),
            Span::styled("def. ", Style::default().fg(Color::DarkGray)),
            Span::styled(l, Style::default().fg(Color::Red)),
            Span::styled(
                format!("{title_mark}  {detail}"),
                Style::default().fg(Color::Gray),
            ),
        ]),
        // Draw / no-contest: no decisive winner, show both fighters neutrally.
        (w, l) => {
            let a = w.or(l).unwrap_or_else(|| "?".to_string());
            Line::from(vec![
                Span::styled(a, Style::default().fg(Color::White)),
                Span::styled(
                    format!("{title_mark}  {detail}"),
                    Style::default().fg(Color::Gray),
                ),
            ])
        }
    }
}

/// The trailing detail of a fight line: method, finish round/time, weight class.
fn fight_detail(fr: &FightRow) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = fr.method.as_deref().filter(|s| !s.is_empty()) {
        parts.push(m.to_string());
    }
    if fr.round_ended > 0 {
        parts.push(format!("R{} {}", fr.round_ended, mmss(fr.time_ended)));
    }
    if let Some(w) = fr.weight_class.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("[{w}]"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("· {}", parts.join(" · "))
    }
}

fn render_rounds(frame: &mut Frame, area: Rect, rounds: &[RoundStat]) {
    let mut lines: Vec<Line> = Vec::new();
    if rounds.is_empty() {
        lines.push(Line::styled(
            "Per-round stats for the highlighted bout appear here.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for r in rounds.iter().take(20) {
            lines.push(Line::raw(round_summary(r)));
        }
    }
    let p = Paragraph::new(lines)
        .block(titled_block("Round-by-round"))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// PURE HELPERS (string formatting; unit-tested below — no terminal I/O).
// --------------------------------------------------------------------------- //

/// Plain-text one-liner summarising one fighter's work in one round.
pub fn round_summary(r: &RoundStat) -> String {
    let name = r.fighter_name.clone().unwrap_or_else(|| "?".into());
    let rn = r.round_number.unwrap_or(0);
    format!(
        "{name}  R{rn}  sig {}/{}  td {}/{}  kd {}  ctrl {}",
        r.sig_str_landed,
        r.sig_str_attempted,
        r.td_landed,
        r.td_attempted,
        r.knockdowns,
        mmss(r.control_time),
    )
}

/// Format seconds as `m:ss` (e.g. `135 -> "2:15"`). Negatives clamp to 0:00.
fn mmss(total_seconds: i64) -> String {
    let secs = total_seconds.max(0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;

    /// Flatten a styled `Line` to its plain text for content assertions.
    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn event(title: &str, date: Option<&str>, loc: Option<&str>) -> EventRow {
        EventRow {
            event_id: 1,
            title: title.to_string(),
            date: date.map(str::to_string),
            location: loc.map(str::to_string),
        }
    }

    fn fight() -> FightRow {
        FightRow {
            fight_id: 1,
            event_id: Some(1),
            event_name: None,
            date: Some("2025-01-18".into()),
            winner_name: Some("Islam Makhachev".into()),
            loser_name: Some("Renato Moicano".into()),
            weight_class: Some("Lightweight".into()),
            title_bout: 1,
            method: Some("Submission".into()),
            round_ended: 1,
            time_ended: 222,
            referee: None,
        }
    }

    #[test]
    fn mmss_formats() {
        assert_eq!(mmss(0), "0:00");
        assert_eq!(mmss(9), "0:09");
        assert_eq!(mmss(222), "3:42");
        assert_eq!(mmss(-5), "0:00");
    }

    #[test]
    fn event_line_includes_date_title_and_location() {
        let e = event("UFC 311", Some("2025-01-18"), Some("Inglewood, USA"));
        let s = text(&event_line(&e));
        assert!(s.contains("2025-01-18"));
        assert!(s.contains("UFC 311"));
        assert!(s.contains("· Inglewood, USA"));
    }

    #[test]
    fn event_line_handles_missing_date_and_location() {
        let s = text(&event_line(&event("UFC 1", None, None)));
        assert!(s.contains("????-??-??"));
        assert!(s.contains("UFC 1"));
        assert!(!s.contains("·"));
        // Empty-string location is treated as missing.
        let s2 = text(&event_line(&event("UFC 1", Some("1993-11-12"), Some(""))));
        assert!(
            !s2.contains("·"),
            "empty location should not add a separator: {s2}"
        );
    }

    #[test]
    fn fight_line_full_decisive_bout() {
        let s = text(&fight_line(&fight()));
        assert!(s.contains("Islam Makhachev"));
        assert!(s.contains("def."));
        assert!(s.contains("Renato Moicano"));
        assert!(s.contains("Submission"));
        assert!(s.contains("R1 3:42"));
        assert!(s.contains("[Lightweight]"));
    }

    #[test]
    fn fight_line_draw_has_no_def() {
        let mut fr = fight();
        fr.winner_name = Some(String::new());
        fr.loser_name = None;
        fr.title_bout = 0;
        let s = text(&fight_line(&fr));
        assert!(!s.contains("def."), "draw/NC should have no 'def.': {s}");
    }

    #[test]
    fn fight_detail_skips_empty_parts() {
        let fr = FightRow {
            method: None,
            round_ended: 0,
            time_ended: 0,
            weight_class: None,
            ..fight()
        };
        assert_eq!(fight_detail(&fr), "");
    }

    #[test]
    fn round_summary_lists_key_counters() {
        let r = RoundStat {
            round_stat_id: 1,
            fight_id: Some(1),
            fighter_name: Some("Islam Makhachev".into()),
            result: Some("w".into()),
            round_number: Some(1),
            knockdowns: 1,
            sub_attempts: 2,
            reversals: 0,
            control_time: 184,
            td_landed: 1,
            td_attempted: 2,
            td_pct: 0.5,
            sig_str_landed: 12,
            sig_str_attempted: 20,
            sig_str_pct: 0.6,
            total_str_landed: 30,
            total_str_attempted: 40,
            total_str_pct: 0.75,
            head_landed: 6,
            head_attempted: 12,
            head_pct: 0.5,
            body_landed: 3,
            body_attempted: 4,
            body_pct: 0.75,
            leg_landed: 3,
            leg_attempted: 4,
            leg_pct: 0.75,
            distance_landed: 5,
            distance_attempted: 10,
            distance_pct: 0.5,
            clinch_landed: 2,
            clinch_attempted: 3,
            clinch_pct: 0.66,
            ground_landed: 5,
            ground_attempted: 7,
            ground_pct: 0.71,
        };
        let s = round_summary(&r);
        assert!(s.contains("Islam Makhachev  R1"));
        assert!(s.contains("sig 12/20"));
        assert!(s.contains("td 1/2"));
        assert!(s.contains("kd 1"));
        assert!(s.contains("ctrl 3:04"));
    }
}
