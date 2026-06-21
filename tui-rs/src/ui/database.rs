//! Database hub sub-menu: two paths — "Browse events" and "Find a fighter".
//!
//! Both paths are real (see app.rs nav wiring):
//!   - "Browse events" -> events list -> a card -> a fighter profile;
//!   - "Find a fighter" -> live fuzzy search -> a fighter profile.
//!
//! This screen is the hub itself: a short intro of what's inside the DB plus the
//! two-item menu with the current selection. It reads `app.database_selected`
//! and the cached `app.summary` for the at-a-glance counts.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::titled_block;
use crate::app::{App, DATABASE_MENU};

/// Render the Database hub into `area`.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let [intro_area, menu_area] =
        Layout::vertical([Constraint::Length(6), Constraint::Min(0)]).areas(area);

    render_intro(frame, intro_area, app);
    render_menu(frame, menu_area, app);
}

/// A small "what's in the database" blurb with live counts.
fn render_intro(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    match &app.summary {
        Some(s) => {
            lines.push(Line::from(vec![
                Span::styled("In the database: ", Style::default().fg(Color::Gray)),
                count_span(s.n_fighters, "fighter"),
                Span::styled(" · ", Style::default().fg(Color::DarkGray)),
                count_span(s.n_events, "event"),
                Span::styled(" · ", Style::default().fg(Color::DarkGray)),
                count_span(s.n_fights, "fight"),
            ]));
            let span = match (&s.earliest_event, &s.latest_event) {
                (Some(a), Some(b)) if a != b => format!("Coverage: {a} → {b}"),
                (Some(a), Some(_)) => format!("Coverage: {a}"),
                _ => "Coverage: (no dated events)".to_string(),
            };
            lines.push(Line::styled(span, Style::default().fg(Color::DarkGray)));
        }
        None => lines.push(Line::styled(
            "DB summary unavailable.",
            Style::default().fg(Color::DarkGray),
        )),
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Pick how you want to explore:",
        Style::default().fg(Color::Gray),
    ));

    let p = Paragraph::new(lines)
        .block(titled_block("Database"))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

/// The two-path menu (Browse events / Find a fighter).
fn render_menu(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = DATABASE_MENU
        .iter()
        .map(|(label, desc)| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    label.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("   {desc}"),
                    Style::default().fg(Color::Gray),
                )),
            ])
        })
        .collect();

    let list = List::new(items)
        .block(titled_block("Choose a path"))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.database_selected.min(DATABASE_MENU.len() - 1)));
    frame.render_stateful_widget(list, area, &mut state);
}

/// "N thing(s)" with the number emphasised.
fn count_span(n: i64, noun: &str) -> Span<'static> {
    let s = if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    };
    Span::styled(
        s,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
}
