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
//!
//! There is NO fighter animation: the loading overlay's only motion is the
//! braille spinner, so these tests cover the spinner + the one-shot MMA intro.

#[path = "../src/anim.rs"]
mod anim_impl;

use anim_impl::{ANIM_FPS, INTRO_TICKS, SPINNER_FRAMES, intro_done, mma_intro, spinner_frame};

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

#[test]
fn spinner_advances_within_an_e2e_capture_window() {
    // The e2e no-freeze test captures the running overlay twice ~300ms apart (= ~9
    // frames at 30 fps) and asserts the SPINNER glyph CHANGED. With a 10-glyph
    // cycle, any offset of 1..=9 frames lands on a DIFFERENT glyph (the cycle only
    // returns to the same glyph after a full 10 frames). ~9 frames sits safely
    // inside that window. Prove it for every phase: from every starting frame, the
    // glyph 7..=9 frames later differs.
    for f in 0..SPINNER_FRAMES.len() {
        for delta in 7..=9usize {
            assert_ne!(
                spinner_frame(f),
                spinner_frame(f + delta),
                "spinner at frame {f} should differ from frame {} (~300ms later)",
                f + delta
            );
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

// --- intro_done ------------------------------------------------------------

#[test]
fn intro_done_is_a_clean_threshold() {
    assert!(!intro_done(0));
    assert!(!intro_done(INTRO_TICKS - 1));
    assert!(intro_done(INTRO_TICKS));
    assert!(intro_done(usize::MAX));
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
