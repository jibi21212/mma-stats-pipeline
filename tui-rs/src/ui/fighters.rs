//! Fighter screens: `render_search` = the live fuzzy search (Database -> Find a
//! fighter); `render_profile` = one fighter's annotated career stats + history.
//!
//! Find path: a live fuzzy search input over the fighter roster (`app.search`,
//! narrowed in app.rs via `fuzzy`) -> select -> profile. The profile shows the
//! career stats WITH layman explanations via `app::stat_line` (the `stats_text`
//! layer), the record, physical tale-of-the-tape, and the fight history marked
//! from the profiled fighter's own perspective (W / L).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::{focusable_block, titled_block};
use crate::app::{App, stat_line};
use crate::models::{FightRow, Fighter};
use crate::stats_text;

/// Render the live fuzzy fighter SEARCH into `area`.
pub fn render_search(frame: &mut Frame, area: Rect, app: &App) {
    let [search_area, list_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

    let q = &app.search.query;
    let prompt: Line = if q.is_empty() {
        Line::from(vec![
            Span::styled("▏", Style::default().fg(Color::Cyan)),
            Span::styled(
                " start typing a fighter's name…",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::raw(q.as_str()),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ])
    };
    let search = Paragraph::new(prompt).block(titled_block("Search"));
    frame.render_widget(search, search_area);

    let count = app.search.filtered.len();
    let items: Vec<ListItem> = if count == 0 {
        vec![ListItem::new(Line::styled(
            "No fighters match — try a different spelling.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.search
            .filtered
            .iter()
            .map(|n| ListItem::new(Span::raw(n.clone())))
            .collect()
    };
    let list = List::new(items)
        .block(titled_block(&format!("Fighters ({count})")))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    if count > 0 {
        state.select(Some(app.search.selected.min(count - 1)));
    }
    frame.render_stateful_widget(list, list_area, &mut state);
}

/// Render a single fighter's annotated PROFILE into `area`.
pub fn render_profile(frame: &mut Frame, area: Rect, app: &App) {
    let Some(f) = &app.fighter.profile else {
        let p = Paragraph::new("Fighter profile unavailable.")
            .block(focusable_block("Profile", false))
            .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    };

    let [head_area, stats_area, fights_area] = Layout::vertical([
        Constraint::Length(8),
        Constraint::Min(8),
        Constraint::Length(10),
    ])
    .areas(area);

    render_header(frame, head_area, f);
    render_stats(frame, stats_area, f);
    render_fights(frame, fights_area, f, &app.fighter.fights);
}

fn render_header(frame: &mut Frame, area: Rect, f: &Fighter) {
    let mut lines: Vec<Line> = Vec::new();

    // Name (+ nickname) with a champion belt marker when applicable.
    let mut name_spans = vec![Span::styled(
        f.name.clone(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(nick) = &f.nickname
        && !nick.trim().is_empty()
    {
        name_spans.push(Span::styled(
            format!("  \"{}\"", nick.trim()),
            Style::default().fg(Color::Gray),
        ));
    }
    if f.was_champion != 0 {
        name_spans.push(Span::styled(
            "  🏆 champion",
            Style::default().fg(Color::Yellow),
        ));
    }
    lines.push(Line::from(name_spans));

    // Record — load-bearing for tests (must start with "Record:").
    lines.push(Line::from(vec![
        Span::styled(
            record_line(f),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}", finish_summary(f)),
            Style::default().fg(Color::Gray),
        ),
    ]));

    lines.push(kv("Nationality", f.nationality.as_deref().unwrap_or("—")));
    lines.push(Line::from(vec![
        Span::styled("Stance       ", Style::default().fg(Color::Gray)),
        Span::raw(f.stance.as_deref().unwrap_or("—").to_string()),
        Span::styled("   Weight ", Style::default().fg(Color::Gray)),
        Span::raw(opt_lbs(f.weight_lbs)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Height ", Style::default().fg(Color::Gray)),
        Span::raw(opt_in("height_in", f.height_in)),
        Span::styled("   Reach ", Style::default().fg(Color::Gray)),
        Span::raw(opt_in("reach_in", f.reach_in)),
    ]));

    let p = Paragraph::new(lines).block(focusable_block("Profile", true));
    frame.render_widget(p, area);
}

fn render_stats(frame: &mut Frame, area: Rect, f: &Fighter) {
    let lines: Vec<Line> = vec![
        Line::raw(stat_line("Strikes landed/min", "slpm", f.slpm)),
        Line::raw(stat_line("Striking accuracy", "str_acc", f.str_acc)),
        Line::raw(stat_line("Strikes absorbed/min", "sapm", f.sapm)),
        Line::raw(stat_line("Striking defense", "str_def", f.str_def)),
        Line::raw(stat_line("Takedowns/15min", "td_avg", f.td_avg)),
        Line::raw(stat_line("Takedown accuracy", "td_acc", f.td_acc)),
        Line::raw(stat_line("Takedown defense", "td_def", f.td_def)),
        Line::raw(stat_line("Submission attempts/15min", "sub_avg", f.sub_avg)),
    ];
    let p = Paragraph::new(lines)
        .block(titled_block("Career stats (explained)"))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn render_fights(frame: &mut Frame, area: Rect, f: &Fighter, fights: &[FightRow]) {
    let mut lines: Vec<Line> = Vec::new();
    if fights.is_empty() {
        lines.push(Line::styled(
            "No fight history recorded.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for fr in fights.iter().take(40) {
            lines.push(history_line(&f.name, fr));
        }
    }
    let p = Paragraph::new(lines).block(titled_block("Fight history"));
    frame.render_widget(p, area);
}

// --------------------------------------------------------------------------- //
// PURE HELPERS (string formatting; unit-tested below — no terminal I/O).
// --------------------------------------------------------------------------- //

/// Outcome of a fight from one fighter's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Win,
    Loss,
    Other,
}

/// Decide whether `name` won, lost, or neither (draw / NC / not listed) `fr`.
fn outcome_for(name: &str, fr: &FightRow) -> Outcome {
    let won = fr.winner_name.as_deref().is_some_and(|w| w == name);
    let lost = fr.loser_name.as_deref().is_some_and(|l| l == name);
    match (won, lost) {
        (true, _) => Outcome::Win,
        (false, true) => Outcome::Loss,
        _ => Outcome::Other,
    }
}

/// The opponent's name from `name`'s perspective in `fr`, if any.
fn opponent_of(name: &str, fr: &FightRow) -> Option<String> {
    let w = fr.winner_name.as_deref().filter(|s| !s.is_empty());
    let l = fr.loser_name.as_deref().filter(|s| !s.is_empty());
    match (w, l) {
        (Some(w), Some(l)) if w == name => Some(l.to_string()),
        (Some(w), Some(l)) if l == name => Some(w.to_string()),
        // name not clearly on either side: show whichever opponent we can.
        (Some(w), _) if w != name => Some(w.to_string()),
        (_, Some(l)) if l != name => Some(l.to_string()),
        _ => None,
    }
}

/// "Record: W-L-D" (+ NC when present). Must start with "Record:" (test-locked).
pub fn record_line(f: &Fighter) -> String {
    let mut s = format!("Record: {}-{}-{}", f.wins, f.losses, f.draws);
    if f.no_contests > 0 {
        s.push_str(&format!(" ({} NC)", f.no_contests));
    }
    s
}

/// Short finish-record blurb derived from the title/win counters we have.
fn finish_summary(f: &Fighter) -> String {
    let total = f.wins + f.losses + f.draws + f.no_contests;
    if total <= 0 {
        return "no recorded bouts".to_string();
    }
    if f.championship_bouts_won > 0 {
        format!(
            "{} title win{}",
            f.championship_bouts_won,
            if f.championship_bouts_won == 1 {
                ""
            } else {
                "s"
            }
        )
    } else {
        format!("{total} pro bouts on record")
    }
}

/// A single history row from the profiled fighter's perspective:
/// "W/L  date  vs Opponent  · method".
pub fn history_line(self_name: &str, fr: &FightRow) -> Line<'static> {
    let date = fr.date.as_deref().unwrap_or("????-??-??").to_string();
    let method = fr
        .method
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|m| format!("  · {m}"))
        .unwrap_or_default();
    let opp = opponent_of(self_name, fr).unwrap_or_else(|| "?".to_string());

    let (tag, tag_style) = match outcome_for(self_name, fr) {
        Outcome::Win => ("W", Style::default().fg(Color::Green)),
        Outcome::Loss => ("L", Style::default().fg(Color::Red)),
        Outcome::Other => ("·", Style::default().fg(Color::DarkGray)),
    };

    Line::from(vec![
        Span::styled(format!("{tag} "), tag_style.add_modifier(Modifier::BOLD)),
        Span::styled(format!("{date}  "), Style::default().fg(Color::DarkGray)),
        Span::styled("vs ", Style::default().fg(Color::DarkGray)),
        Span::raw(opp),
        Span::styled(method, Style::default().fg(Color::Gray)),
    ])
}

fn kv(key: &str, val: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<13}"), Style::default().fg(Color::Gray)),
        Span::raw(val.to_string()),
    ])
}

fn opt_in(key: &str, v: Option<i64>) -> String {
    match v {
        Some(n) => stats_text::describe(key, Some(n as f64)),
        None => "—".to_string(),
    }
}

fn opt_lbs(v: Option<i64>) -> String {
    match v {
        Some(n) => format!("{n} lbs"),
        None => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fighter() -> Fighter {
        Fighter {
            fighter_id: 1,
            name: "Israel Adesanya".into(),
            nickname: Some("The Last Stylebender".into()),
            nationality: Some("Nigeria".into()),
            height_in: Some(76),
            weight_lbs: Some(185),
            reach_in: Some(80),
            stance: Some("Switch".into()),
            date_of_birth: Some("1989-07-22".into()),
            wins: 24,
            losses: 4,
            draws: 0,
            no_contests: 1,
            was_champion: 1,
            championship_bouts_won: 5,
            slpm: Some(4.0),
            str_acc: Some(0.49),
            sapm: Some(3.0),
            str_def: Some(0.6),
            td_avg: Some(0.1),
            td_acc: Some(0.4),
            td_def: Some(0.7),
            sub_avg: Some(0.1),
        }
    }

    fn fight(winner: Option<&str>, loser: Option<&str>) -> FightRow {
        FightRow {
            fight_id: 1,
            event_id: Some(1),
            event_name: None,
            date: Some("2023-04-08".into()),
            winner_name: winner.map(str::to_string),
            loser_name: loser.map(str::to_string),
            weight_class: Some("Middleweight".into()),
            title_bout: 1,
            method: Some("KO/TKO".into()),
            round_ended: 2,
            time_ended: 271,
            referee: None,
        }
    }

    #[test]
    fn record_line_starts_with_record_and_includes_nc() {
        let f = fighter();
        let s = record_line(&f);
        assert!(s.starts_with("Record:"), "got: {s}");
        assert!(s.contains("24-4-0"));
        assert!(s.contains("(1 NC)"));
    }

    #[test]
    fn record_line_omits_nc_when_zero() {
        let mut f = fighter();
        f.no_contests = 0;
        assert_eq!(record_line(&f), "Record: 24-4-0");
    }

    #[test]
    fn outcome_is_perspective_relative() {
        let me = "Israel Adesanya";
        assert_eq!(
            outcome_for(me, &fight(Some(me), Some("Alex Pereira"))),
            Outcome::Win
        );
        assert_eq!(
            outcome_for(me, &fight(Some("Alex Pereira"), Some(me))),
            Outcome::Loss
        );
        assert_eq!(outcome_for(me, &fight(None, None)), Outcome::Other);
    }

    #[test]
    fn opponent_is_the_other_corner() {
        let me = "Israel Adesanya";
        assert_eq!(
            opponent_of(me, &fight(Some(me), Some("Alex Pereira"))),
            Some("Alex Pereira".to_string())
        );
        assert_eq!(
            opponent_of(me, &fight(Some("Alex Pereira"), Some(me))),
            Some("Alex Pereira".to_string())
        );
    }

    #[test]
    fn history_line_marks_win_and_names_opponent() {
        let me = "Israel Adesanya";
        let line = history_line(me, &fight(Some(me), Some("Alex Pereira")));
        let text: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.starts_with("W "), "win should be tagged W: {text}");
        assert!(text.contains("vs Alex Pereira"));
        assert!(text.contains("KO/TKO"));
    }

    #[test]
    fn history_line_marks_loss() {
        let me = "Israel Adesanya";
        let line = history_line(me, &fight(Some("Alex Pereira"), Some(me)));
        let text: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.starts_with("L "), "loss should be tagged L: {text}");
        assert!(text.contains("vs Alex Pereira"));
    }

    #[test]
    fn finish_summary_prefers_title_wins() {
        assert!(finish_summary(&fighter()).contains("5 title wins"));
        let mut f = fighter();
        f.championship_bouts_won = 0;
        assert!(finish_summary(&f).contains("pro bouts"));
        f.wins = 0;
        f.losses = 0;
        f.draws = 0;
        f.no_contests = 0;
        assert_eq!(finish_summary(&f), "no recorded bouts");
    }

    #[test]
    fn opt_lbs_renders_or_dashes() {
        assert_eq!(opt_lbs(Some(185)), "185 lbs");
        assert_eq!(opt_lbs(None), "—");
    }
}
