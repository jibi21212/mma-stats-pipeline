//! PURE animation frame generators (no terminal I/O) — frame counter in, lines
//! out — so every animation is unit-testable without a `Frame` or a terminal.
//!
//! All functions are PURE: the same `frame` (and `width`) always yields the same
//! output, no globals.
//!
//! Animation timing: `frame` is a TIME-DERIVED animation clock advanced by the
//! event loop toward [`ANIM_FPS`] (see `App::anim_frame` / `App::on_tick`), so
//! animation speed is constant regardless of the redraw rate. Every generator
//! stays a pure `frame -> output` function.
//!
//! Contract (return types are FROZEN; the bodies fill in the art):
//! - [`spinner_frame`]: one braille spinner glyph from `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`. This is the
//!   ONLY motion the loading overlay shows (there is no fighter animation).
//! - [`mma_intro`]: a ONE-SHOT left-to-right reveal of chunky block-letter
//!   "MMA", framed as a fight poster, fitting within `width` columns. (Kept as
//!   plain `Vec<String>`; the home screen colors it — do NOT regress it.)
//! - [`intro_done`]: true once the one-shot intro has fully revealed.

/// The braille spinner cycle, exposed so callers/tests can reason about length.
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Target animation frame rate. The event loop derives the integer animation
/// `frame` index from wall-clock elapsed time at this rate (see
/// `App::anim_frame`), so the spinner/intro play at a constant speed no matter
/// how often the screen is actually redrawn. ~30 fps reads as smooth motion
/// while keeping the redraw cost modest.
pub const ANIM_FPS: u64 = 30;

/// Number of intro animation FRAMES (at [`ANIM_FPS`]) the one-shot intro runs
/// before [`intro_done`] returns true. Shared with `App`/`ui::home`/tests so
/// there is one source of truth.
///
/// Sized so the intro lasts ~1.2s at 30 fps (the reveal "looks great" at this
/// pace and the e2e intro assertions only require that blocks reveal then
/// hold, which this preserves).
pub const INTRO_TICKS: usize = 36;

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

/// One braille spinner glyph for `frame` (cycles through [`SPINNER_FRAMES`]).
/// PURE.
pub fn spinner_frame(frame: usize) -> char {
    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
}

// ---------------------------------------------------------------------------
// MMA intro poster
// ---------------------------------------------------------------------------

/// Chunky block-letter rows for "MMA". `#` marks a filled cell; spaces are
/// gaps. Five rows tall; rendered with `█` for the fill. Every row is the same
/// length so column-by-column reveal math is exact.
const MMA_ART: [&str; 5] = [
    "#   # #   #  ### ",
    "## ## ## ## #   #",
    "# # # # # # #####",
    "#   # #   # #   #",
    "#   # #   # #   #",
];

/// Total columns in the block-letter art (all [`MMA_ART`] rows share this).
const ART_COLS: usize = 17;

/// Reveal the whole logo a couple ticks before [`INTRO_TICKS`] so the finished
/// poster lingers briefly before the menu replaces it.
const REVEAL_TICKS: usize = INTRO_TICKS - 4;

/// One-shot block-letter "MMA" intro revealed left-to-right, sized to `width`.
///
/// At `frame == 0` nothing is revealed; each successive frame slams in more
/// columns from the left. Once `frame >= REVEAL_TICKS` the full logo is shown
/// (so [`intro_done`] and the static-logo path in `ui::home` both display the
/// complete poster). The art is wrapped in a fight-poster frame and clamped to
/// `width` columns. PURE.
pub fn mma_intro(frame: usize, width: usize) -> Vec<String> {
    // How many of the ART_COLS columns are revealed so far (0..=ART_COLS).
    let revealed = if frame >= REVEAL_TICKS {
        ART_COLS
    } else {
        // Linear slam-in across REVEAL_TICKS ticks.
        ((frame * ART_COLS) / REVEAL_TICKS.max(1)).min(ART_COLS)
    };

    // Build each art row, replacing not-yet-revealed columns with spaces and
    // filled cells with the block glyph.
    let art_rows: Vec<String> = MMA_ART
        .iter()
        .map(|row| {
            // Pad/clamp each source row to exactly ART_COLS so the poster frame
            // is always a perfect rectangle, even if the art is later edited.
            (0..ART_COLS)
                .map(|col| {
                    let ch = row.chars().nth(col).unwrap_or(' ');
                    if col >= revealed {
                        ' '
                    } else if ch == '#' {
                        '█'
                    } else {
                        ' '
                    }
                })
                .collect::<String>()
        })
        .collect();

    // Frame the art as a fight poster. The inner content is ART_COLS wide; add
    // a 1-space inner margin on each side, then a box border.
    let inner_w = ART_COLS + 2; // one space padding each side
    let top = format!("╔{}╗", "═".repeat(inner_w));
    let bottom = format!("╚{}╝", "═".repeat(inner_w));
    let blank_inner = format!("║ {} ║", " ".repeat(ART_COLS));

    let mut lines: Vec<String> = Vec::with_capacity(MMA_ART.len() + 4);
    lines.push(top);
    lines.push(blank_inner.clone());
    for row in &art_rows {
        lines.push(format!("║ {row} ║"));
    }
    lines.push(blank_inner);
    lines.push(bottom);

    // Clamp every line to `width` columns (by character count, since `█`/box
    // glyphs are multi-byte but single-column).
    if width > 0 {
        for line in &mut lines {
            if line.chars().count() > width {
                *line = line.chars().take(width).collect();
            }
        }
    }
    lines
}

/// True once the one-shot intro has fully played (>= [`INTRO_TICKS`] frames).
/// PURE.
pub fn intro_done(frame: usize) -> bool {
    frame >= INTRO_TICKS
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- spinner -----------------------------------------------------------

    #[test]
    fn spinner_cycles_through_all_frames() {
        for (i, &expected) in SPINNER_FRAMES.iter().enumerate() {
            assert_eq!(spinner_frame(i), expected);
        }
        // Wraps around.
        assert_eq!(spinner_frame(SPINNER_FRAMES.len()), SPINNER_FRAMES[0]);
        assert_eq!(spinner_frame(SPINNER_FRAMES.len() + 3), SPINNER_FRAMES[3]);
    }

    #[test]
    fn spinner_uses_only_braille_glyphs() {
        for f in 0..100 {
            assert!(SPINNER_FRAMES.contains(&spinner_frame(f)));
        }
    }

    // --- intro_done --------------------------------------------------------

    #[test]
    fn intro_done_flips_after_intro_ticks() {
        assert!(!intro_done(0));
        assert!(!intro_done(INTRO_TICKS - 1));
        assert!(intro_done(INTRO_TICKS));
        assert!(intro_done(INTRO_TICKS + 1000));
    }

    // --- mma_intro ---------------------------------------------------------

    #[test]
    fn mma_intro_is_pure() {
        assert_eq!(mma_intro(7, 40), mma_intro(7, 40));
    }

    #[test]
    fn mma_intro_frame_zero_reveals_no_blocks() {
        let lines = mma_intro(0, 80);
        let joined = lines.join("\n");
        assert!(!lines.is_empty());
        assert!(
            !joined.contains('█'),
            "frame 0 should not have revealed any block glyphs yet: {joined}"
        );
    }

    #[test]
    fn mma_intro_reveals_progressively_left_to_right() {
        // The count of revealed block glyphs is monotonically non-decreasing as
        // the frame advances, and strictly increases at least once.
        let blocks = |f: usize| {
            mma_intro(f, 80)
                .join("")
                .chars()
                .filter(|&c| c == '█')
                .count()
        };
        let mut prev = blocks(0);
        let mut grew = false;
        for f in 1..=REVEAL_TICKS {
            let cur = blocks(f);
            assert!(cur >= prev, "reveal went backwards at frame {f}");
            if cur > prev {
                grew = true;
            }
            prev = cur;
        }
        assert!(grew, "reveal never added any blocks");
    }

    #[test]
    fn mma_intro_completes_and_stays_complete() {
        let full = mma_intro(REVEAL_TICKS, 80);
        // Once revealed, later frames render the same finished poster.
        assert_eq!(mma_intro(INTRO_TICKS, 80), full);
        assert_eq!(mma_intro(INTRO_TICKS * 4, 80), full);
        // The finished logo contains all the filled cells of the art.
        let want = MMA_ART
            .iter()
            .map(|r| r.matches('#').count())
            .sum::<usize>();
        let got = full.join("").chars().filter(|&c| c == '█').count();
        assert_eq!(got, want, "completed poster should reveal every block");
    }

    #[test]
    fn mma_intro_lines_have_uniform_width_per_frame() {
        // Within a single frame every line should be the same display width
        // (the poster frame is a rectangle).
        for f in [0, 1, 5, REVEAL_TICKS / 2, REVEAL_TICKS, INTRO_TICKS] {
            let lines = mma_intro(f, 200);
            let w = lines[0].chars().count();
            for line in &lines {
                assert_eq!(
                    line.chars().count(),
                    w,
                    "non-uniform line width at frame {f}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn mma_intro_respects_width_clamp() {
        let narrow = mma_intro(INTRO_TICKS, 6);
        for line in &narrow {
            assert!(line.chars().count() <= 6, "line exceeded width: {line:?}");
        }
        // width 0 must not panic and must still return lines.
        let zero = mma_intro(INTRO_TICKS, 0);
        assert!(!zero.is_empty());
    }

    #[test]
    fn mma_intro_no_panic_at_extremes() {
        let _ = mma_intro(0, 0);
        let _ = mma_intro(usize::MAX, 1);
        let _ = mma_intro(usize::MAX, usize::MAX);
    }
}
