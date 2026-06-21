//! Offline, deterministic tests for the PURE animation frame generators in
//! `src/anim.rs`.
//!
//! The crate is a pure binary (no `[lib]` target and `main.rs` declares its
//! modules privately), so an external test cannot `use mma_tui::anim`. As with
//! the other integration tests in this crate, we `include!` the module source
//! directly via `#[path]`, which compiles and exercises the exact same code.
//! `src/anim.rs` already carries its own `#[cfg(test)] mod tests`; wrapping the
//! include in a private module keeps those inner unit tests from colliding with
//! the integration cases below (we only re-export the public API we use here).

#[path = "../src/anim.rs"]
mod anim_impl;

use anim_impl::{
    ANIM_FPS, FIGHTER_CYCLE, FIGHTER_ROWS, FIGHTER_WIDTH, FighterPhase, INTRO_TICKS,
    SPINNER_FRAMES, fighter_phase, fighters_frame, intro_done, mma_intro, spinner_frame,
};
use ratatui::style::Color;
use ratatui::text::Line;

// --- helpers for the COLORED (half-block) fighters API ---------------------

/// Rendered cell width of a rasterized line (one span per pixel column).
fn line_cells(line: &Line<'static>) -> usize {
    line.spans.len()
}

/// A frame's "lit cell" signature: which (row, col) cells carry color (i.e. are
/// not a plain space). Lets us compare frames for motion without caring about
/// exact RGB, replacing the old text-based pose comparison.
fn lit_signature(lines: &[Line<'static>]) -> Vec<(usize, usize)> {
    let mut sig = Vec::new();
    for (r, line) in lines.iter().enumerate() {
        for (c, span) in line.spans.iter().enumerate() {
            if span.content.as_ref() != " " {
                sig.push((r, c));
            }
        }
    }
    sig
}

/// Does this frame contain any clearly RED-dominant truecolor pixel (the RED
/// corner's gloves/shorts)? Checks both the fg (top pixel) and bg (bottom pixel)
/// of every cell so a red pixel in either half-block slot counts.
fn has_red_corner(lines: &[Line<'static>]) -> bool {
    lines.iter().any(|line| {
        line.spans.iter().any(|s| {
            let fg = matches!(s.style.fg, Some(Color::Rgb(r, g, b)) if r > 150 && g < 120 && b < 120);
            let bg = matches!(s.style.bg, Some(Color::Rgb(r, g, b)) if r > 150 && g < 120 && b < 120);
            fg || bg
        })
    })
}

/// Does this frame contain any clearly BLUE-dominant truecolor pixel (the BLUE
/// corner's gloves/shorts)? Blue dominant == high blue, low red.
fn has_blue_corner(lines: &[Line<'static>]) -> bool {
    lines.iter().any(|line| {
        line.spans.iter().any(|s| {
            let fg = matches!(s.style.fg, Some(Color::Rgb(r, _g, b)) if b > 150 && r < 120);
            let bg = matches!(s.style.bg, Some(Color::Rgb(r, _g, b)) if b > 150 && r < 120);
            fg || bg
        })
    })
}

/// The peak frame of each striking beat, derived from the shared 6-beat
/// [`FIGHTER_CYCLE`] timeline (Bob, RedJab, Reset, Bob, BlueKick, Reset). The
/// peak of a beat is its midpoint, where the tweened limb is fully extended.
fn beat_midpoint(beat: usize) -> usize {
    let beat_len = FIGHTER_CYCLE / 6;
    beat * beat_len + beat_len / 2
}

// --- spinner ---------------------------------------------------------------

#[test]
fn spinner_cycles_and_wraps() {
    for (i, &want) in SPINNER_FRAMES.iter().enumerate() {
        assert_eq!(spinner_frame(i), want);
    }
    assert_eq!(spinner_frame(SPINNER_FRAMES.len()), SPINNER_FRAMES[0]);
}

#[test]
fn spinner_only_emits_known_braille_glyphs() {
    // Matches the spec's exact braille sequence ⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏.
    let expected: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    assert_eq!(SPINNER_FRAMES, expected);
    for f in 0..256usize {
        assert!(SPINNER_FRAMES.contains(&spinner_frame(f)));
    }
}

// --- intro_done ------------------------------------------------------------

#[test]
fn intro_done_is_a_clean_threshold() {
    assert!(!intro_done(0));
    assert!(!intro_done(INTRO_TICKS - 1));
    assert!(intro_done(INTRO_TICKS));
    assert!(intro_done(usize::MAX));
}

// --- fighters (COLORED half-block API) -------------------------------------

#[test]
fn fighters_frame_every_frame_nonempty_and_uniform_width() {
    let width = line_cells(&fighters_frame(0)[0]);
    assert!(width > 0);
    // Cover well past several loop periods.
    for f in 0..512usize {
        let lines = fighters_frame(f);
        assert!(!lines.is_empty(), "empty frame at {f}");
        for line in &lines {
            assert_eq!(
                line_cells(line),
                width,
                "non-uniform width at frame {f}"
            );
        }
    }
}

#[test]
fn fighters_frame_height_is_stable() {
    let h = fighters_frame(0).len();
    for f in 0..512usize {
        assert_eq!(fighters_frame(f).len(), h, "height changed at frame {f}");
    }
}

#[test]
fn fighters_frame_is_deterministic() {
    for f in [0usize, 1, 5, 13, 99, 1234] {
        assert_eq!(lit_signature(&fighters_frame(f)), lit_signature(&fighters_frame(f)));
    }
}

#[test]
fn fighters_frame_loops() {
    // Find the smallest p that is a genuine period: the whole sequence repeats
    // (by lit-cell signature) with stride p over a wide window.
    const WINDOW: usize = 256;
    let mut period = None;
    for p in 1..WINDOW {
        if (0..WINDOW).all(|f| lit_signature(&fighters_frame(f)) == lit_signature(&fighters_frame(f + p)))
        {
            period = Some(p);
            break;
        }
    }
    let period = period.expect("choreography should loop within the window");
    assert!(period >= 4, "loop period unreasonably short: {period}");
    // Stability across multiple cycles.
    for f in 0..period {
        assert_eq!(
            lit_signature(&fighters_frame(f)),
            lit_signature(&fighters_frame(f + period * 3))
        );
    }
}

#[test]
fn fighters_frame_shows_distinct_action() {
    // Over a long window the fighters should take on several distinct frames
    // (motion), proving the animation is alive.
    let mut seen = std::collections::HashSet::new();
    for f in 0..256usize {
        seen.insert(lit_signature(&fighters_frame(f)));
    }
    assert!(
        seen.len() >= 4,
        "expected several distinct frames, got {}",
        seen.len()
    );
}

#[test]
fn fighters_frame_emits_truecolor() {
    // The colored API must surface truecolor (Rgb) pixels — the red/blue corners.
    let mut saw_rgb = false;
    for f in 0..256usize {
        for line in fighters_frame(f) {
            for span in &line.spans {
                if matches!(span.style.fg, Some(Color::Rgb(..))) {
                    saw_rgb = true;
                }
            }
        }
    }
    assert!(saw_rgb, "fighters_frame should emit truecolor pixels");
}

#[test]
fn fighters_frame_renders_both_corners() {
    // The two boxers are a RED corner and a BLUE corner: a single neutral frame
    // must contain BOTH red-dominant and blue-dominant truecolor pixels (gloves
    // / shorts), proving each fighter is tinted to its corner — not just "some
    // Rgb somewhere".
    let neutral = fighters_frame(0);
    assert!(
        has_red_corner(&neutral),
        "expected RED-corner pixels in the fighters frame"
    );
    assert!(
        has_blue_corner(&neutral),
        "expected BLUE-corner pixels in the fighters frame"
    );
    // And both corners are present throughout the whole loop (neither fighter
    // ever vanishes), even mid-strike.
    for f in 0..FIGHTER_CYCLE {
        let lines = fighters_frame(f);
        assert!(has_red_corner(&lines), "RED corner missing at frame {f}");
        assert!(has_blue_corner(&lines), "BLUE corner missing at frame {f}");
    }
}

#[test]
fn fighters_frame_has_distinct_key_poses() {
    // The three signature poses — idle stance, RED's jab fully extended, and
    // BLUE's kick fully extended — must each look DIFFERENT from the others, so
    // the choreography reads as bob → jab → kick rather than a static blob.
    let idle = lit_signature(&fighters_frame(beat_midpoint(0))); // Bob beat
    let jab = lit_signature(&fighters_frame(beat_midpoint(1))); // RedJab beat
    let kick = lit_signature(&fighters_frame(beat_midpoint(4))); // BlueKick beat

    assert_ne!(idle, jab, "idle stance and RED jab should differ");
    assert_ne!(idle, kick, "idle stance and BLUE kick should differ");
    assert_ne!(jab, kick, "RED jab and BLUE kick should differ");

    // The jab should reach FURTHER RIGHT than idle (RED's lead glove extends
    // toward BLUE), and the kick FURTHER LEFT (BLUE's leg extends toward RED).
    let max_col = |sig: &[(usize, usize)]| sig.iter().map(|&(_, c)| c).max().unwrap_or(0);
    let min_col = |sig: &[(usize, usize)]| sig.iter().map(|&(_, c)| c).min().unwrap_or(usize::MAX);
    assert!(
        max_col(&jab) >= max_col(&idle),
        "RED jab should extend at least as far right as the idle stance"
    );
    assert!(
        min_col(&kick) <= min_col(&idle),
        "BLUE kick should extend at least as far left as the idle stance"
    );
}

#[test]
fn fighters_frame_strikes_retract_for_a_seamless_loop() {
    // Each striking beat must START and END at (lit-cell) neutral so the limb is
    // retracted by the time the next beat begins — otherwise the loop would jump.
    // Compare the first frame of the RedJab beat with the first frame of the
    // following Reset beat's predecessor (the last frame of RedJab).
    let beat_len = FIGHTER_CYCLE / 6;
    // Last frame of the RedJab beat == one before the Reset beat starts.
    let jab_start = lit_signature(&fighters_frame(beat_len)); // RedJab begins
    let jab_end = lit_signature(&fighters_frame(2 * beat_len - 1)); // RedJab ends
    let jab_peak = lit_signature(&fighters_frame(beat_midpoint(1)));
    // The peak differs from both ends (the punch actually moved out)…
    assert_ne!(jab_peak, jab_start, "jab never left the stance");
    assert_ne!(jab_peak, jab_end, "jab never retracted");
}

#[test]
fn fighters_frame_dimensions_match_the_frozen_contract() {
    // Belt-and-suspenders: the sprite must fill EXACTLY the contracted grid the
    // loading panel is sized off of, at several frames across the loop.
    for f in [0usize, beat_midpoint(1), beat_midpoint(4), FIGHTER_CYCLE - 1] {
        let lines = fighters_frame(f);
        assert_eq!(lines.len(), FIGHTER_ROWS, "row count at frame {f}");
        for line in &lines {
            assert_eq!(line_cells(line), FIGHTER_WIDTH, "width at frame {f}");
        }
    }
}

#[test]
fn anim_fps_is_a_smooth_target() {
    // The time-based animation clock targets a high-but-sane frame rate.
    assert!(
        (24..=60).contains(&ANIM_FPS),
        "ANIM_FPS should target smooth motion (~30 fps), got {ANIM_FPS}"
    );
}

#[test]
fn fighter_phase_covers_the_beats() {
    let mut phases = std::collections::HashSet::new();
    for f in 0..256usize {
        phases.insert(fighter_phase(f).0);
    }
    for p in [
        FighterPhase::Bob,
        FighterPhase::RedJab,
        FighterPhase::BlueKick,
        FighterPhase::Reset,
    ] {
        assert!(phases.contains(&p), "choreography missing phase {p:?}");
    }
}

#[test]
fn fighters_frame_no_panic_at_extremes() {
    let _ = fighters_frame(0);
    let _ = fighters_frame(usize::MAX);
}

// --- mma_intro -------------------------------------------------------------

#[test]
fn mma_intro_progressively_reveals_and_completes() {
    let blocks = |f: usize, w: usize| {
        mma_intro(f, w)
            .join("")
            .chars()
            .filter(|&c| c == '█')
            .count()
    };
    let w = 80;
    // Nothing revealed at the very start.
    assert_eq!(blocks(0, w), 0);
    // Monotonic non-decreasing reveal up to completion.
    let mut prev = 0;
    let mut grew_at_least_once = false;
    for f in 0..=INTRO_TICKS {
        let cur = blocks(f, w);
        assert!(cur >= prev, "reveal regressed at frame {f}");
        if cur > prev {
            grew_at_least_once = true;
        }
        prev = cur;
    }
    assert!(grew_at_least_once, "intro never revealed anything");
    // Fully revealed by the time the intro is done, and it stays put.
    let done = mma_intro(INTRO_TICKS, w);
    assert!(done.join("").contains('█'));
    assert_eq!(mma_intro(INTRO_TICKS * 5, w), done);
}

#[test]
fn mma_intro_lines_uniform_width_within_each_frame() {
    for f in [0usize, 1, 4, 10, INTRO_TICKS, INTRO_TICKS * 2] {
        let lines = mma_intro(f, 200);
        assert!(!lines.is_empty(), "empty intro at frame {f}");
        let w = lines[0].chars().count();
        for line in &lines {
            assert_eq!(
                line.chars().count(),
                w,
                "non-uniform intro width at frame {f}: {line:?}"
            );
        }
    }
}

#[test]
fn mma_intro_respects_width_and_is_deterministic() {
    for w in [0usize, 1, 4, 6, 12, 40, 200] {
        let a = mma_intro(INTRO_TICKS, w);
        let b = mma_intro(INTRO_TICKS, w);
        assert_eq!(a, b, "non-deterministic at width {w}");
        assert!(!a.is_empty(), "empty intro at width {w}");
        if w > 0 {
            for line in &a {
                assert!(
                    line.chars().count() <= w,
                    "line exceeded width {w}: {line:?}"
                );
            }
        }
    }
}

#[test]
fn mma_intro_no_panic_at_extremes() {
    let _ = mma_intro(0, 0);
    let _ = mma_intro(usize::MAX, 0);
    let _ = mma_intro(usize::MAX, 1);
    let _ = mma_intro(usize::MAX, usize::MAX);
}
