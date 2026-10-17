//! Garnish spider overlay: spokes, outline polygons, short axis labels.
//!
//! DIY ratatui Canvas + Braille. No numbers on the plot.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::canvas::{Canvas, Line, Points};

use super::titled_block;
use crate::app::App;
use crate::radar::{AXIS_COUNT, AXIS_LABELS};

const ACCENT_A: Color = Color::Green;
const ACCENT_B: Color = Color::Red;
const ACCENT_ONE: Color = Color::Cyan;
const GRID: Color = Color::DarkGray;
const TYPICAL: Color = Color::Gray;

const BOUNDS: f64 = 1.55;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(overlay) = app.radar.as_ref() else {
        return;
    };

    let popup = centered_rect(58, 82, area);
    frame.render_widget(Clear, popup);

    let series = overlay.series.clone();
    let n = series.len();
    let canvas = Canvas::default()
        .block(titled_block(&overlay.title))
        .marker(Marker::Braille)
        .x_bounds([-BOUNDS, BOUNDS])
        .y_bounds([-BOUNDS, BOUNDS])
        .paint(move |ctx| {
            for i in 0..AXIS_COUNT {
                let (x, y) = vertex(i, 1.0);
                ctx.draw(&Line::new(0.0, 0.0, x, y, GRID));
            }
            draw_polygon(ctx, &[1.0; AXIS_COUNT], GRID);
            draw_polygon(ctx, &[0.5; AXIS_COUNT], TYPICAL);

            for (i, s) in series.iter().enumerate() {
                ctx.layer();
                let color = series_color(i, n);
                draw_polygon(ctx, &s.values, color);
                let pts: Vec<(f64, f64)> = (0..AXIS_COUNT)
                    .map(|ax| vertex(ax, s.values[ax].clamp(0.0, 1.0)))
                    .collect();
                ctx.draw(&Points::new(&pts, color));
            }

            for (i, label) in AXIS_LABELS.iter().enumerate() {
                let (x, y) = label_pos(i, label);
                ctx.print(x, y, Span::styled(*label, Style::default().fg(Color::Gray)));
            }
        });
    frame.render_widget(canvas, popup);
}

fn series_color(index: usize, n: usize) -> Color {
    match (index, n) {
        (0, 2) => ACCENT_A,
        (1, 2) => ACCENT_B,
        _ => ACCENT_ONE,
    }
}

fn vertex(index: usize, radius: f64) -> (f64, f64) {
    let theta =
        std::f64::consts::FRAC_PI_2 - (index as f64) * std::f64::consts::TAU / AXIS_COUNT as f64;
    (radius * theta.cos(), radius * theta.sin())
}

fn draw_polygon(
    ctx: &mut ratatui::widgets::canvas::Context<'_>,
    values: &[f64; AXIS_COUNT],
    color: Color,
) {
    for i in 0..AXIS_COUNT {
        let (x1, y1) = vertex(i, values[i].clamp(0.0, 1.0));
        let j = (i + 1) % AXIS_COUNT;
        let (x2, y2) = vertex(j, values[j].clamp(0.0, 1.0));
        ctx.draw(&Line::new(x1, y1, x2, y2, color));
    }
}

fn label_pos(index: usize, label: &str) -> (f64, f64) {
    let (x, y) = vertex(index, 1.24);
    let shift = label.len() as f64 * 0.09;
    let lx = if x < -0.2 {
        x - shift
    } else if x > 0.2 {
        x + 0.05
    } else {
        x - shift * 0.45
    };
    let ly = if y > 0.7 {
        y + 0.06
    } else if y < -0.7 {
        y - 0.08
    } else {
        y
    };
    (lx, ly)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [v] = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .areas(area);
    let [h] = Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .areas(v);
    h
}
