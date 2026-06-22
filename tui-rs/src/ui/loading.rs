//! The loading overlay shown whenever a background job is in flight (or its
//! finished log is still on screen). TOP = a braille spinner + the job's status
//! label + elapsed time, then a progress bar, then the live tail of the job log.
//!
//! There is NO fighter/stick-man art — the overlay is deliberately minimal: a
//! spinner while running (frozen to a static result glyph once finished), a
//! determinate-or-pulsing progress bar (frozen at 100%/✓ on completion), and the
//! streaming log.
//!
//! Owned by the Foundation agent (it is the job-overlay routing the spec asks
//! for). It calls into the PURE `anim` frame generators, so the anim agent can
//! swap in the spinner without touching this file.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph};

use super::titled_block;
use crate::anim;
use crate::app::App;
use crate::jobs::JobStatus;

/// Render the loading overlay for the active job into `area`.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(job) = app.job.as_ref() else {
        return;
    };

    // A compact status strip (spinner + label + elapsed, alongside the progress
    // bar) on top, then the live log fills the rest.
    let [status_area, log_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

    render_status(frame, status_area, app);
    render_log(frame, log_area, job.log.as_slice());
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let Some(job) = app.job.as_ref() else {
        return;
    };

    let [info_area, bar_area] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);

    // Spinner + status label + elapsed. The spinner only spins while RUNNING; once
    // the job finishes it locks to a static result glyph (freeze-on-finish), so the
    // strip goes still alongside the progress bar.
    let elapsed = job.elapsed_secs();
    let label = job.kind.label();
    let (status_text, status_color) = match job.status {
        JobStatus::Running => ("running", Color::Yellow),
        JobStatus::Done => ("✓ done", Color::Green),
        JobStatus::Failed => ("✗ failed", Color::Red),
    };
    let head = if job.status == JobStatus::Running {
        // Animated braille spinner driven by the still-ticking event loop.
        let spinner = anim::spinner_frame(app.anim_frame());
        format!("{spinner} {label}…  {status_text}  {elapsed}s")
    } else {
        // FINISHED: a static result glyph, no spinner (motion stops with the work).
        format!("✓ {label}  {status_text}  {elapsed}s")
    };
    let info = Paragraph::new(Line::from(Span::styled(
        head,
        Style::default()
            .fg(status_color)
            .add_modifier(Modifier::BOLD),
    )))
    .block(titled_block("Status"));
    frame.render_widget(info, info_area);

    // Progress bar.
    //   * FINISHED  -> a STILL bar: 100% green "✓ done" (or red "✗ failed" at
    //                  the last-known fill). It must NOT keep animating 0->100.
    //   * RUNNING + parsed "N/M" progress -> a determinate bar at done/total.
    //   * RUNNING + no progress yet        -> a tasteful indeterminate pulse.
    let (ratio, bar_label, gauge_fg) = match job.status {
        JobStatus::Done => (1.0_f64, "✓ done · 100%".to_string(), Color::Green),
        JobStatus::Failed => {
            // Freeze at whatever fraction was reached; never re-sweep.
            let r = match job.progress {
                Some((done, total)) if total > 0 => done as f64 / total as f64,
                _ => 1.0,
            };
            (r, "✗ failed".to_string(), Color::Red)
        }
        JobStatus::Running => match job.progress {
            Some((done, total)) if total > 0 => (
                done as f64 / total as f64,
                format!("{done}/{total}"),
                Color::Cyan,
            ),
            _ => {
                // Indeterminate: a slow sweep driven by the animation clock.
                let pulse = (app.anim_frame() % 30) as f64 / 30.0;
                (pulse, "working…".to_string(), Color::Cyan)
            }
        },
    };
    let gauge = Gauge::default()
        .block(titled_block("Progress"))
        .gauge_style(Style::default().fg(gauge_fg).bg(Color::Black))
        .ratio(ratio.clamp(0.0, 1.0))
        .label(bar_label);
    frame.render_widget(gauge, bar_area);
}

fn render_log(frame: &mut Frame, area: Rect, log: &[String]) {
    // Auto-scroll: show the tail that fits in the visible area.
    let inner_height = area.height.saturating_sub(2) as usize;
    let total = log.len();
    let start = total.saturating_sub(inner_height);
    let visible = &log[start..];

    let lines: Vec<Line> = if visible.is_empty() {
        vec![Line::styled(
            "Output will stream here…",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        visible.iter().map(|l| Line::raw(l.clone())).collect()
    };
    let p = Paragraph::new(lines).block(titled_block(&format!("Output ({total} lines)")));
    frame.render_widget(p, area);
}
