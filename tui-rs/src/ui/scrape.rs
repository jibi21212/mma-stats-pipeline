//! Scrape screen body: the run options shown as CLEAR chips that update the
//! instant a key is pressed — "Full: ON" / "Full: OFF", "Limit: N", "Rate: R/s".
//! Each chip lights up when its option is active so the next run's command is
//! readable at a glance, and a short legend underneath spells out the keys.
//!
//! This screen renders ONLY the option chips + how-to-run guidance. While a
//! scrape JOB is in flight (`App.job` is `Some`), `ui::mod::draw` swaps the whole
//! body for the loading overlay (`ui::loading`) — fighters animation, braille
//! spinner, elapsed time, progress bar, and the live process log — so the live
//! run UI is handled there, not here. The toggle state lives on `App.scrape`
//! (Foundation); key handling (f / +/- / [ ] / c / Enter / r) lives in
//! `app::on_key_scrape`. This file stays thin and pure.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::titled_block;
use crate::app::{App, ScrapeState};

/// Render the Scrape screen into `area`.
///
/// Layout (top to bottom): the chip row, a blank spacer, a per-option legend,
/// and a "press Enter/r to run" call-to-action. Everything reads `app.scrape`.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let lines = body_lines(&app.scrape);
    let p = Paragraph::new(lines)
        .block(titled_block("Scraper options"))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

/// Build the full set of styled lines for the screen body from the option state.
fn body_lines(s: &ScrapeState) -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        chip_row(s),
        Line::raw(""),
        section("Adjust the next run"),
        legend(
            "f",
            &format!("toggle full re-scrape (now {})", on_off(s.full)),
        ),
        legend(
            "+ / -",
            &format!("raise / lower limit by 50 (now {})", limit_text(s.limit)),
        ),
        legend(
            "[ / ]",
            &format!("slow / speed up rate by 0.5 (now {})", rate_text(s.rate)),
        ),
        legend("c", "clear the log from the last finished run"),
        Line::raw(""),
        section("Run"),
        run_call_to_action(),
        Line::raw(""),
        explainer(s),
    ]
}

/// The single row of three option chips, separated by spacing.
fn chip_row(s: &ScrapeState) -> Line<'static> {
    Line::from(vec![
        chip(&full_chip_text(s.full), s.full),
        Span::raw("   "),
        chip(&limit_chip_text(s.limit), s.limit.is_some()),
        Span::raw("   "),
        chip(&rate_chip_text(s.rate), s.rate.is_some()),
    ])
}

/// A single highlighted/dimmed chip span.
fn chip(text: &str, active: bool) -> Span<'static> {
    let style = if active {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Gray)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::DIM)
    };
    Span::styled(format!(" {text} "), style)
}

/// A bold cyan section heading.
fn section(title: &str) -> Line<'static> {
    Line::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

/// A "key — description" legend row, with the key highlighted.
fn legend(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<7}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
    ])
}

/// The prominent run prompt.
fn run_call_to_action() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "  Enter / r ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "start scraping — fighters spar in the overlay while it runs",
            Style::default().fg(Color::Gray),
        ),
    ])
}

/// A one-line plain-English summary of exactly what the next run will do.
fn explainer(s: &ScrapeState) -> Line<'static> {
    Line::styled(
        format!("Next run: {}", run_summary(s)),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )
}

// --------------------------------------------------------------------------- //
// PURE FORMATTING HELPERS (unit-tested below). No `Frame`, no `Style` — just
// the strings, so the ON/OFF / Limit / Rate wording stays verifiable.
// --------------------------------------------------------------------------- //

/// "ON" / "OFF" for a boolean flag.
fn on_off(active: bool) -> &'static str {
    if active { "ON" } else { "OFF" }
}

/// Chip text for the full-rescrape toggle: "Full: ON" / "Full: OFF".
fn full_chip_text(full: bool) -> String {
    format!("Full: {}", on_off(full))
}

/// Display text for the limit value ("250" or "none").
fn limit_text(limit: Option<u32>) -> String {
    limit
        .map(|l| l.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// Chip text for the limit option: "Limit: N" / "Limit: none".
fn limit_chip_text(limit: Option<u32>) -> String {
    format!("Limit: {}", limit_text(limit))
}

/// Display text for the rate value ("2.5/s" or "default").
fn rate_text(rate: Option<f64>) -> String {
    rate.map(|r| format!("{r}/s"))
        .unwrap_or_else(|| "default".to_string())
}

/// Chip text for the rate option: "Rate: R/s" / "Rate: default".
fn rate_chip_text(rate: Option<f64>) -> String {
    format!("Rate: {}", rate_text(rate))
}

/// A plain-English summary of the next run's effective behaviour.
fn run_summary(s: &ScrapeState) -> String {
    let scope = if s.full {
        "full re-scrape of every event".to_string()
    } else {
        "incremental scrape (new/changed events only)".to_string()
    };
    let limit = match s.limit {
        Some(n) => format!(", up to {n} events"),
        None => String::new(),
    };
    let rate = match s.rate {
        Some(r) => format!(", at {r} req/s"),
        None => ", at the scraper's default rate".to_string(),
    };
    format!("{scope}{limit}{rate}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_chip_reads_on_or_off() {
        assert_eq!(full_chip_text(true), "Full: ON");
        assert_eq!(full_chip_text(false), "Full: OFF");
    }

    #[test]
    fn limit_chip_shows_value_or_none() {
        assert_eq!(limit_chip_text(Some(250)), "Limit: 250");
        assert_eq!(limit_chip_text(None), "Limit: none");
    }

    #[test]
    fn rate_chip_shows_per_second_or_default() {
        assert_eq!(rate_chip_text(Some(2.5)), "Rate: 2.5/s");
        assert_eq!(rate_chip_text(None), "Rate: default");
    }

    #[test]
    fn on_off_maps_bool() {
        assert_eq!(on_off(true), "ON");
        assert_eq!(on_off(false), "OFF");
    }

    #[test]
    fn run_summary_describes_full_vs_incremental() {
        let full = ScrapeState {
            full: true,
            limit: Some(10),
            rate: Some(3.0),
        };
        let s = run_summary(&full);
        assert!(s.contains("full re-scrape"));
        assert!(s.contains("up to 10 events"));
        assert!(s.contains("3 req/s"));

        let incr = ScrapeState::default();
        let s = run_summary(&incr);
        assert!(s.contains("incremental"));
        assert!(s.contains("default rate"));
        // No limit clause when limit is None.
        assert!(!s.contains("up to"));
    }

    #[test]
    fn body_lines_render_without_panicking() {
        // Smoke: both an all-defaults and an all-set state produce a non-empty
        // body. (Pure check; no Frame needed.)
        assert!(!body_lines(&ScrapeState::default()).is_empty());
        let set = ScrapeState {
            full: true,
            limit: Some(100),
            rate: Some(1.5),
        };
        assert!(!body_lines(&set).is_empty());
    }
}
