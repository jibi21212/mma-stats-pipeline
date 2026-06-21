//! Rendering layer. Each screen exposes a `render(frame, area, app)` fn that
//! draws into the given `Rect` reading immutable `App` state. `draw` is the
//! top-level entry the event loop calls each frame: it lays out the header +
//! body + persistent footer + status line and dispatches to the active screen's
//! renderer — OR, when a background job is in flight, overlays the loading
//! animation (fighters + spinner + progress + log) regardless of screen.
//!
//! OWNERSHIP: the Foundation agent owns this dispatcher, the header, the
//! persistent footer, and the loading-overlay routing. The per-screen render
//! bodies (`home`, `database`, `scrape`, `predict`, `model`, and the reused
//! `events` / `fighters`) are owned by the screen agents; the Foundation
//! provides minimal stubs so the crate compiles and launches.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Screen};

pub mod database;
pub mod events;
pub mod fighters;
pub mod home;
pub mod loading;
pub mod model;
pub mod predict;
pub mod scrape;

/// Top-level draw: render shared chrome and dispatch to the active screen (or
/// the loading overlay when a job is running).
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // header | body | footer (controls) | status line
    let [header, body, footer, status] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, app);

    if app.job_active() {
        // A background job is in flight (or its finished log is still up): the
        // loading overlay replaces the body regardless of which screen is under
        // it, so the user always sees the live animation + log.
        loading::render(frame, body, app);
    } else {
        dispatch(frame, body, app);
    }

    render_footer(frame, footer, app);
    render_status(frame, status, app);
}

/// Route the body to the current screen's renderer.
fn dispatch(frame: &mut Frame, body: Rect, app: &App) {
    match app.current() {
        Screen::Home => home::render(frame, body, app),
        Screen::Database => database::render(frame, body, app),
        Screen::Scrape => scrape::render(frame, body, app),
        Screen::Events => events::render(frame, body, app),
        Screen::EventFights { .. } => events::render_card(frame, body, app),
        Screen::FighterSearch => fighters::render_search(frame, body, app),
        Screen::Fighter { .. } => fighters::render_profile(frame, body, app),
        Screen::Predict => predict::render(frame, body, app),
        Screen::Model => model::render(frame, body, app),
    }
}

/// Header: app title + a compact summary of DB counts and model state.
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let model_label = if app.model.model_loaded {
        Span::styled("model: ready", Style::default().fg(Color::Green))
    } else {
        Span::styled("model: not trained", Style::default().fg(Color::Yellow))
    };

    let counts = app
        .summary
        .as_ref()
        .map(|s| {
            format!(
                "fighters {} · events {} · fights {}",
                s.n_fighters, s.n_events, s.n_fights
            )
        })
        .unwrap_or_else(|| "DB summary unavailable".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::styled(
                " mma-tui ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("│ {} │ {counts} │ ", app.current().title())),
            model_label,
            Span::raw(" "),
        ]));

    // The header body is just the title block (a breadcrumb of the nav stack).
    let crumbs: Vec<String> = app.nav.iter().map(|s| s.title()).collect();
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {}", crumbs.join(" › ")),
        Style::default().fg(Color::Gray),
    )))
    .block(block);
    frame.render_widget(p, area);
}

/// Persistent footer: the contextual controls for the current screen (or the
/// job overlay when one is active).
fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hints = if app.job_active() {
        match app.job_status() {
            Some(s) if s.is_finished() => "Esc/⏎ dismiss · Home · q Quit",
            _ => "running… · q Quit",
        }
    } else {
        app.current().footer_hint()
    };
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {hints}"),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(p, area);
}

/// Transient status line at the very bottom.
fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let text = app
        .status_line
        .clone()
        .unwrap_or_else(|| "ready".to_string());
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {text}"),
        Style::default().fg(Color::White).bg(Color::Blue),
    )));
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// Shared rendering helpers used by multiple screens.
// --------------------------------------------------------------------------- //

/// A standard bordered block with a titled header.
pub fn titled_block(title: &str) -> Block<'static> {
    Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

/// A bordered block that highlights when `focused` is true (active pane).
pub fn focusable_block(title: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if focused { Color::Cyan } else { Color::Gray })
                .add_modifier(Modifier::BOLD),
        ))
}
