//! Home screen: the one-shot "MMA" intro poster (headlining the latest numbered
//! UFC card) followed by the 4-item top menu.
//!
//! The poster frames `anim::mma_intro` — a chunky block-letter "MMA" revealed
//! left-to-right while `App.frame` advances during the intro, then held as the
//! static logo once `anim::intro_done`. Under the logo it headlines the latest
//! numbered UFC card from the DB (`app.latest_card`) with a fight-poster vibe
//! (TONIGHT / MAIN CARD), then a graceful fallback when the DB is empty. Below
//! the poster sits the vertical 4-item menu driven by `app.home_selected`, and a
//! tasteful one-line DB summary strip when counts are available.
//!
//! Render logic is intentionally thin: selection + intro state live on `App`
//! (owned by Foundation); this file only turns that state into widgets.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::titled_block;
use crate::anim;
use crate::app::{App, HOME_MENU};
use crate::models::{DbSummary, LatestCard};

/// Render the Home screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // Poster (logo + headline) on top, then the menu, then a thin DB-summary
    // strip. The summary strip collapses to nothing when no counts are known.
    let summary_h: u16 = if app.summary.is_some() { 1 } else { 0 };
    let [poster_area, menu_area, summary_area] = Layout::vertical([
        Constraint::Min(7),
        Constraint::Length(HOME_MENU.len() as u16 + 2),
        Constraint::Length(summary_h),
    ])
    .areas(area);

    render_poster(frame, poster_area, app);
    render_menu(frame, menu_area, app);
    if summary_h > 0 {
        render_summary_strip(frame, summary_area, app.summary.as_ref());
    }
}

/// The fight-poster: the block-letter "MMA" logo (revealed during the intro,
/// held afterwards) above the headline for the latest numbered UFC card.
fn render_poster(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();

    // The one-shot intro reveals "MMA" left-to-right; once done we hold the
    // final frame as the static logo. Driven by the TIME-BASED animation clock
    // so the reveal plays smoothly at ~30 fps.
    let af = app.anim_frame();
    let intro_frame = if anim::intro_done(af) {
        anim::INTRO_TICKS
    } else {
        af
    };
    for l in anim::mma_intro(intro_frame, width) {
        lines.push(Line::from(Span::styled(
            l,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }

    lines.push(Line::raw(""));
    lines.extend(headline_lines(app.latest_card.as_ref()));

    let p = Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .block(titled_block("Tonight's main card"));
    frame.render_widget(p, area);
}

/// The vertical 4-option menu with the current selection highlighted.
fn render_menu(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = HOME_MENU
        .iter()
        .map(|(label, desc)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{label:<10}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(*desc, Style::default().fg(Color::Gray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(titled_block("Menu"))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.home_selected.min(HOME_MENU.len() - 1)));
    frame.render_stateful_widget(list, area, &mut state);
}

/// A single thin strip reusing the DB-wide counts in a tasteful way.
fn render_summary_strip(frame: &mut Frame, area: Rect, summary: Option<&DbSummary>) {
    let Some(s) = summary else { return };
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {}", summary_text(s)),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// PURE helpers (turn state into text; unit-tested below).
// --------------------------------------------------------------------------- //

/// The display labels for the home menu, in order — handy for tests + callers
/// that want the option names without the per-row descriptions.
#[allow(dead_code)] // public convenience kept for callers/tests; render reads HOME_MENU directly.
pub fn menu_labels() -> Vec<&'static str> {
    HOME_MENU.iter().map(|(label, _)| *label).collect()
}

/// The fight-poster headline for `card`, as styled lines.
///
/// When a numbered card is present we shout the number ("UFC 311") in poster
/// style with a "TONIGHT · MAIN CARD" banner; the full title, date and location
/// follow. A non-numbered fallback event drops the big number but keeps the
/// banner. With no events at all we show a graceful placeholder.
fn headline_lines(card: Option<&LatestCard>) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match card {
        Some(card) => {
            lines.push(Line::from(Span::styled(
                "★ TONIGHT · MAIN CARD ★",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            // The big, prominent headline: the card number when numbered, else
            // the event title itself.
            let headline = poster_headline(card);
            lines.push(Line::from(Span::styled(
                headline,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
            // The full event title (shown even when the number headlines above,
            // since a numbered title can carry extra fight billing).
            lines.push(Line::from(Span::styled(
                card.title.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            if let Some(sub) = subtitle(card) {
                lines.push(Line::from(Span::styled(
                    sub,
                    Style::default().fg(Color::Gray),
                )));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                "No events in the database yet.",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                "Run Scrape to populate data/ufc.db.",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines
}

/// The big poster headline text: the card number ("UFC 311") when the latest
/// event is a numbered card, otherwise the event title verbatim.
fn poster_headline(card: &LatestCard) -> String {
    match card.number {
        Some(n) => format!("UFC {n}"),
        None => card.title.clone(),
    }
}

/// The poster subtitle: date and location joined when present, e.g.
/// `"2025-01-18 · Inglewood, USA"`. `None` when neither is known.
fn subtitle(card: &LatestCard) -> Option<String> {
    match (card.date.as_deref(), card.location.as_deref()) {
        (Some(d), Some(l)) if !d.is_empty() && !l.is_empty() => Some(format!("{d} · {l}")),
        (Some(d), _) if !d.is_empty() => Some(d.to_string()),
        (_, Some(l)) if !l.is_empty() => Some(l.to_string()),
        _ => None,
    }
}

/// One-line DB summary reused from the cached counts.
fn summary_text(s: &DbSummary) -> String {
    format!(
        "Database: {} fighters · {} events · {} fights",
        s.n_fighters, s.n_events, s.n_fights
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_labels_are_the_four_options_in_order() {
        assert_eq!(
            menu_labels(),
            vec!["Scrape", "Database", "Predict", "Model"]
        );
    }

    #[test]
    fn poster_headline_shouts_card_number_when_numbered() {
        let card = LatestCard {
            event_id: 1,
            title: "UFC 311: Makhachev vs Moicano".into(),
            date: Some("2025-01-18".into()),
            location: Some("Inglewood, USA".into()),
            number: Some(311),
        };
        assert_eq!(poster_headline(&card), "UFC 311");
        assert_eq!(
            subtitle(&card),
            Some("2025-01-18 · Inglewood, USA".to_string())
        );
    }

    #[test]
    fn poster_headline_falls_back_to_title_when_unnumbered() {
        let card = LatestCard {
            event_id: 2,
            title: "UFC Fight Night: Smith vs Jones".into(),
            date: None,
            location: Some("Las Vegas, USA".into()),
            number: None,
        };
        assert_eq!(poster_headline(&card), "UFC Fight Night: Smith vs Jones");
        // Only location known -> subtitle is just the location.
        assert_eq!(subtitle(&card), Some("Las Vegas, USA".to_string()));
    }

    #[test]
    fn subtitle_is_none_when_nothing_known() {
        let card = LatestCard {
            event_id: 3,
            title: "UFC 1".into(),
            date: None,
            location: None,
            number: Some(1),
        };
        assert_eq!(subtitle(&card), None);
        // Date present but empty string is treated as unknown.
        let card2 = LatestCard {
            date: Some(String::new()),
            location: Some(String::new()),
            ..card
        };
        assert_eq!(subtitle(&card2), None);
    }

    #[test]
    fn headline_lines_render_a_fallback_when_no_card() {
        let lines = headline_lines(None);
        assert!(!lines.is_empty());
    }

    #[test]
    fn summary_text_reuses_counts() {
        let s = DbSummary {
            n_fighters: 4,
            n_events: 2,
            n_fights: 9,
            ..Default::default()
        };
        assert_eq!(
            summary_text(&s),
            "Database: 4 fighters · 2 events · 9 fights"
        );
    }
}
