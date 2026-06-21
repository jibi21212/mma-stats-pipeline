//! PURE animation frame generators (no terminal I/O) — frame counter in, lines
//! out — so every animation is unit-testable without a `Frame`.
//!
//! OWNERSHIP: the ANIM/ART agent owns the BODIES of these functions. The CORE
//! agent owns the colored API CONTRACT below (return types + the half-block
//! pixel-buffer helper the Art phase paints sprites into) so the screen agents
//! (and the loading overlay in `ui::mod`) can call them. All functions are
//! PURE: the same `frame` (and `width`) always yields the same output, no
//! globals.
//!
//! Animation timing: `frame` is now a TIME-DERIVED animation clock advanced by
//! the event loop toward [`ANIM_FPS`] (see `App::anim_frame` /
//! `App::on_tick`), so animation speed is constant regardless of the redraw
//! rate. Every generator stays a pure `frame -> output` function.
//!
//! Contract (return types are FROZEN for the Core phase; Art fills the bodies):
//! - [`fighters_frame`]: a smooth LOOPING two-fighter striking choreography
//!   rendered as COLORED half-block (`▀`) pixel art — RED corner vs BLUE
//!   corner. Returns `Vec<Line<'static>>` so each cell can carry a truecolor
//!   fg (top pixel) + bg (bottom pixel). Every frame is the same width/height
//!   so rendering never jumps. (Body is a minimal colored placeholder until the
//!   Art phase paints the real sprites — see the doc on `fighters_frame`.)
//! - [`spinner_frame`]: one braille spinner glyph from `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`.
//! - [`mma_intro`]: a ONE-SHOT left-to-right reveal of chunky block-letter
//!   "MMA", framed as a fight poster, fitting within `width` columns. (Kept as
//!   plain `Vec<String>`; the home screen colors it — do NOT regress it.)
//! - [`intro_done`]: true once the one-shot intro has fully revealed.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// The braille spinner cycle, exposed so callers/tests can reason about length.
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Target animation frame rate. The event loop derives the integer animation
/// `frame` index from wall-clock elapsed time at this rate (see
/// `App::anim_frame`), so the choreography plays at a constant speed no matter
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

// ===========================================================================
// Fighters choreography — COLORED HALF-BLOCK PIXEL ART
// ===========================================================================
//
// The fighters are drawn as a PIXEL BUFFER and rasterized to half-block (`▀`)
// cells: each character cell stacks TWO pixels — the glyph's FOREGROUND color is
// the TOP pixel and its BACKGROUND color is the BOTTOM pixel. So a [`PixelBuf`]
// that is [`SPRITE_W`] x [`SPRITE_H`] pixels renders to `SPRITE_W` columns x
// `SPRITE_H / 2` rows of `▀`. This is the canvas the ART phase paints the real
// boxers into; the rasterizer + frame contract below are owned by Core and are
// FROZEN.

/// Pixel-buffer width in PIXELS == rendered cell columns. Even, fixed so the
/// panel never jumps between frames.
pub const SPRITE_W: usize = 40;

/// Pixel-buffer height in PIXELS. MUST be even (two pixels per `▀` cell).
/// Renders to `SPRITE_H / 2` rows.
pub const SPRITE_H: usize = 24;

/// Rendered row count (`SPRITE_H / 2`). Each row is a line of `▀` cells.
pub const FIGHTER_ROWS: usize = SPRITE_H / 2;

/// Rendered column count (== [`SPRITE_W`]). Each frame is exactly this wide.
/// Part of the public anim contract (used by the Art phase + tests); the binary
/// reads the rasterized lines directly, so it can be unused there.
#[allow(dead_code)]
pub const FIGHTER_WIDTH: usize = SPRITE_W;

/// One full choreography loop, in animation FRAMES (at [`ANIM_FPS`]).
///
/// Choreography (SIMPLE, with tween frames for smooth motion): both fighters
/// bob on their feet → RED jabs (lead arm out) while BLUE leans/steps back →
/// reset → BLUE throws a kick (leg out) while RED steps back → reset → loop.
/// The Art phase keys its sprite poses off [`fighter_phase`] so it never has to
/// re-derive the timing.
pub const FIGHTER_CYCLE: usize = 96;

/// The transparent "no pixel here" sentinel. Pixels left as `None` render as the
/// cell's background (the panel backdrop), so sprites can have empty space.
pub type Pixel = Option<Color>;

/// A simple RGB-or-transparent pixel canvas the choreography paints into, then
/// rasterizes to half-block (`▀`) [`Line`]s via [`PixelBuf::rasterize`].
///
/// Indexing is `(x, y)` with `y` growing DOWN. Painting out of bounds is a
/// silent no-op so sprite code can be sloppy at the edges.
pub struct PixelBuf {
    w: usize,
    h: usize,
    px: Vec<Pixel>,
}

impl PixelBuf {
    /// A fully-transparent canvas of the fixed sprite size.
    pub fn new() -> PixelBuf {
        PixelBuf {
            w: SPRITE_W,
            h: SPRITE_H,
            px: vec![None; SPRITE_W * SPRITE_H],
        }
    }

    /// Set the pixel at `(x, y)` (no-op if out of bounds). `None` clears it.
    pub fn set(&mut self, x: usize, y: usize, color: Pixel) {
        if x < self.w && y < self.h {
            self.px[y * self.w + x] = color;
        }
    }

    /// Fill an axis-aligned rectangle (clamped to the canvas) with `color`.
    /// Convenience for blocky sprite limbs/torsos; `None` erases.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Pixel) {
        for yy in y..y.saturating_add(h) {
            for xx in x..x.saturating_add(w) {
                self.set(xx, yy, color);
            }
        }
    }

    /// Read the pixel at `(x, y)` (transparent if out of bounds).
    fn get(&self, x: usize, y: usize) -> Pixel {
        if x < self.w && y < self.h {
            self.px[y * self.w + x]
        } else {
            None
        }
    }

    /// Rasterize the buffer to half-block (`▀`) lines: row `r` pairs pixel row
    /// `2r` (glyph FG / top) over pixel row `2r+1` (glyph BG / bottom). A cell
    /// with both pixels transparent renders as a plain space (panel backdrop).
    ///
    /// Returns exactly [`FIGHTER_ROWS`] lines, each [`FIGHTER_WIDTH`] cells.
    pub fn rasterize(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.h / 2);
        for r in 0..(self.h / 2) {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(self.w);
            for x in 0..self.w {
                let top = self.get(x, 2 * r);
                let bottom = self.get(x, 2 * r + 1);
                spans.push(half_block_cell(top, bottom));
            }
            lines.push(Line::from(spans));
        }
        lines
    }
}

impl Default for PixelBuf {
    fn default() -> Self {
        PixelBuf::new()
    }
}

/// Build ONE half-block cell from a (top, bottom) pixel pair.
///
/// - both transparent  -> a plain space.
/// - only top set       -> `▀` with fg = top (bottom shows backdrop).
/// - only bottom set    -> `▄` with fg = bottom (top shows backdrop).
/// - both set           -> `▀` with fg = top, bg = bottom (two stacked pixels).
fn half_block_cell(top: Pixel, bottom: Pixel) -> Span<'static> {
    match (top, bottom) {
        (None, None) => Span::raw(" "),
        (Some(t), None) => Span::styled("▀", Style::default().fg(t)),
        (None, Some(b)) => Span::styled("▄", Style::default().fg(b)),
        (Some(t), Some(b)) => Span::styled("▀", Style::default().fg(t).bg(b)),
    }
}

/// The choreography "phase" for an animation `frame`, so the Art phase can key
/// its sprite poses + tweens off ONE shared timeline. PURE.
///
/// Returns `(phase, t)` where `phase` is the current beat and `t` is a `0.0..1.0`
/// progress WITHIN that beat (for tweening limbs smoothly).
pub fn fighter_phase(frame: usize) -> (FighterPhase, f32) {
    let f = frame % FIGHTER_CYCLE;
    // Six equal beats across the loop: bob, RED jab, reset, bob, BLUE kick, reset.
    let beat_len = FIGHTER_CYCLE / 6;
    let beat = (f / beat_len).min(5);
    let within = (f % beat_len) as f32 / beat_len.max(1) as f32;
    let phase = match beat {
        0 => FighterPhase::Bob,
        1 => FighterPhase::RedJab,
        2 => FighterPhase::Reset,
        3 => FighterPhase::Bob,
        4 => FighterPhase::BlueKick,
        _ => FighterPhase::Reset,
    };
    (phase, within)
}

/// The beats of the simplified two-fighter choreography. The Art phase renders a
/// sprite pose per phase (tweened by the `t` from [`fighter_phase`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FighterPhase {
    /// Both fighters lightly bobbing on their feet (idle).
    Bob,
    /// RED corner throws a JAB (lead arm extends); BLUE leans/steps back.
    RedJab,
    /// BLUE corner throws a KICK (leg out); RED steps back.
    BlueKick,
    /// Both reset to neutral stance between strikes.
    Reset,
}

// --- Sprite palette ---------------------------------------------------------
//
// Truecolor (`Color::Rgb`) so the RED corner and BLUE corner read clearly even
// against a dark panel. Skin/hair/outline are shared; only the gloves + shorts
// (+ a trunk waistband) are tinted per corner.

/// Warm skin tone shared by both boxers (head + forearms + legs).
const SKIN: Color = Color::Rgb(232, 180, 140);
/// A slightly darker skin shade for shading the underside of the head/limbs so
/// the half-block stacking reads as rounded rather than flat.
const SKIN_DK: Color = Color::Rgb(198, 146, 108);
/// Dark hair / outline accent that frames the head against the backdrop.
const HAIR: Color = Color::Rgb(40, 32, 30);
/// Neutral tank-top torso (a light heather) so the colored shorts/gloves pop.
const TORSO: Color = Color::Rgb(225, 228, 235);

/// RED corner colors: gloves, shorts, waistband highlight.
const RED_GLOVE: Color = Color::Rgb(225, 55, 55);
const RED_GLOVE_DK: Color = Color::Rgb(170, 32, 32);
const RED_SHORTS: Color = Color::Rgb(208, 48, 48);
const RED_TRIM: Color = Color::Rgb(255, 120, 120);

/// BLUE corner colors: gloves, shorts, waistband highlight.
const BLUE_GLOVE: Color = Color::Rgb(60, 120, 235);
const BLUE_GLOVE_DK: Color = Color::Rgb(36, 80, 180);
const BLUE_SHORTS: Color = Color::Rgb(52, 110, 220);
const BLUE_TRIM: Color = Color::Rgb(130, 175, 255);

/// Which corner a boxer belongs to — picks its glove/shorts palette and which
/// way it faces (RED faces right toward BLUE; BLUE faces left toward RED).
#[derive(Clone, Copy)]
enum Corner {
    Red,
    Blue,
}

/// Smoothstep easing (`3t² - 2t³`) for limb tweens, so jabs/kicks accelerate out
/// and settle in rather than moving linearly. `t` is clamped to `0.0..=1.0`.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Per-boxer pose parameters derived from the shared choreography. All offsets
/// are in PIXELS (canvas units). `lead_ext` extends the lead glove (jab),
/// `kick_ext` extends the lead leg (kick), `lean` slides the whole body away
/// from the opponent (reacting to a strike), `bob` is the idle bounce.
struct Pose {
    bob: i32,
    lean: i32,
    lead_ext: i32,
    kick_ext: i32,
}

/// Idle "bob" amount (0..=1 px) for a boxer, offset in phase so the two fighters
/// don't bounce in lockstep. Light — one half-block of bounce reads as alive
/// without looking jittery at 30 fps.
fn bob_of(frame: usize, offset: usize) -> i32 {
    // A 16-frame bounce: down for half, up for half. One pixel of travel.
    // `wrapping_add` keeps `fighters_frame(usize::MAX)` panic-free; the bounce
    // is periodic so a wrap at the extreme is harmless.
    match (frame.wrapping_add(offset) / 8) % 2 {
        0 => 0,
        _ => 1,
    }
}

/// Resolve both boxers' poses for an animation `frame` from the shared
/// [`fighter_phase`] timeline. RED is the left boxer, BLUE the right.
fn poses(frame: usize) -> (Pose, Pose) {
    let (phase, raw_t) = fighter_phase(frame);
    // A there-and-back envelope (0→1→0) so a strike extends then RETRACTS within
    // its beat, leaving the loop seamless (each beat starts/ends at neutral).
    let pulse = ease(1.0 - (raw_t * 2.0 - 1.0).abs());

    let red_bob = bob_of(frame, 0);
    let blue_bob = bob_of(frame, 4);

    let mut red = Pose {
        bob: red_bob,
        lean: 0,
        lead_ext: 0,
        kick_ext: 0,
    };
    let mut blue = Pose {
        bob: blue_bob,
        lean: 0,
        lead_ext: 0,
        kick_ext: 0,
    };

    match phase {
        FighterPhase::Bob | FighterPhase::Reset => {
            // Just the idle bounce; both stay planted.
        }
        FighterPhase::RedJab => {
            // RED's lead glove shoots out toward BLUE; BLUE leans/steps back.
            red.lead_ext = (pulse * 8.0).round() as i32;
            blue.lean = (pulse * 4.0).round() as i32; // BLUE slides right (away)
            red.bob = 0; // planted while striking so the punch reads cleanly
        }
        FighterPhase::BlueKick => {
            // BLUE's lead leg kicks out toward RED; RED steps back.
            blue.kick_ext = (pulse * 9.0).round() as i32;
            red.lean = -(pulse * 4.0).round() as i32; // RED slides left (away)
            blue.bob = 0; // planted while kicking
        }
    }

    (red, blue)
}

/// Paint one boxer into `buf`. `base_x` is the left edge of the boxer's bounding
/// box (8px wide core) and `base_y` its top; `corner` selects palette + facing;
/// `pose` supplies the tweened limb offsets. Drawn as stacked colored rects so
/// the half-block rasterizer renders a chunky little fighter (head, gloves,
/// tank top, shorts, legs).
fn draw_boxer(buf: &mut PixelBuf, base_x: i32, base_y: i32, corner: Corner, pose: &Pose) {
    let (glove, glove_dk, shorts, trim, faces_right) = match corner {
        Corner::Red => (RED_GLOVE, RED_GLOVE_DK, RED_SHORTS, RED_TRIM, true),
        Corner::Blue => (BLUE_GLOVE, BLUE_GLOVE_DK, BLUE_SHORTS, BLUE_TRIM, false),
    };

    // Apply lean (whole-body horizontal slide) + bob (vertical bounce).
    let ox = base_x + pose.lean;
    let oy = base_y + pose.bob;

    // Tiny helper: paint a rect from signed coords, clipping anything off the
    // top/left of the canvas (PixelBuf::set already clamps the right/bottom).
    let px = |buf: &mut PixelBuf, x: i32, y: i32, w: i32, h: i32, c: Color| {
        if w <= 0 || h <= 0 {
            return;
        }
        // Right/bottom edges (exclusive) in signed space.
        let x1 = x + w;
        let y1 = y + h;
        // Clip the left/top to 0; if the whole rect is off-canvas, bail.
        let x0 = x.max(0);
        let y0 = y.max(0);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        buf.fill_rect(x0 as usize, y0 as usize, (x1 - x0) as usize, (y1 - y0) as usize, Some(c));
    };

    // The core figure is 8px wide. The "lead" (striking) side faces the
    // opponent: RED leads with its right side, BLUE with its left.
    // Coordinates below are relative to (ox, oy) for the figure's bounding box.

    // --- Head (rows 0..5): hair cap, face, jaw shading ---------------------
    px(buf, ox + 2, oy + 0, 4, 1, HAIR); // hair top
    px(buf, ox + 1, oy + 1, 6, 1, HAIR); // hair sides
    px(buf, ox + 1, oy + 2, 6, 2, SKIN); // face
    px(buf, ox + 2, oy + 4, 4, 1, SKIN_DK); // jaw/chin shading
    // Eyes: a couple darker pixels on the facing side.
    if faces_right {
        px(buf, ox + 5, oy + 2, 1, 1, HAIR);
    } else {
        px(buf, ox + 2, oy + 2, 1, 1, HAIR);
    }

    // --- Torso (rows 5..11): tank top over chest --------------------------
    px(buf, ox + 1, oy + 5, 6, 6, TORSO);
    // A shaded vertical seam down the middle for a little depth.
    px(buf, ox + 3, oy + 6, 1, 4, Color::Rgb(200, 204, 212));

    // --- Shorts / waistband (rows 11..15) ---------------------------------
    px(buf, ox + 1, oy + 11, 6, 1, trim); // bright waistband
    px(buf, ox + 1, oy + 12, 6, 3, shorts); // shorts body

    // --- Legs (rows 15..22) -----------------------------------------------
    // Two legs; the lead leg can extend forward into a kick.
    let kick = pose.kick_ext;
    if faces_right {
        // Rear leg planted, lead (right) leg may kick out to the right.
        px(buf, ox + 1, oy + 15, 2, 7, SKIN); // rear leg
        if kick > 0 {
            // Thigh angles up, shin shoots toward opponent.
            px(buf, ox + 4, oy + 14, 3, 2, SKIN_DK); // raised thigh
            px(buf, ox + 6, oy + 15, kick, 2, SKIN); // extended shin
            px(buf, ox + 6 + kick, oy + 15, 1, 2, SKIN_DK); // foot
        } else {
            px(buf, ox + 5, oy + 15, 2, 7, SKIN); // lead leg planted
        }
    } else {
        // Mirrored: rear leg on the right, lead (left) leg kicks out to the left.
        px(buf, ox + 5, oy + 15, 2, 7, SKIN); // rear leg
        if kick > 0 {
            px(buf, ox + 1, oy + 14, 3, 2, SKIN_DK); // raised thigh
            px(buf, ox + 1 - kick, oy + 15, kick, 2, SKIN); // extended shin
            px(buf, ox - kick, oy + 15, 1, 2, SKIN_DK); // foot
        } else {
            px(buf, ox + 1, oy + 15, 2, 7, SKIN); // lead leg planted
        }
    }

    // --- Arms / gloves (rows 6..12) ---------------------------------------
    // Rear (guard) glove stays up by the chin; lead glove jabs toward opponent.
    let ext = pose.lead_ext;
    if faces_right {
        // Rear glove guards the left of the face.
        px(buf, ox - 1, oy + 6, 2, 2, glove_dk);
        px(buf, ox - 1, oy + 6, 2, 1, glove);
        // Lead arm + glove on the right, extends with `ext`.
        let arm_x = ox + 7;
        px(buf, arm_x, oy + 8, 1 + ext, 2, SKIN); // forearm reaching out
        let gx = arm_x + ext;
        px(buf, gx, oy + 7, 3, 4, glove_dk); // glove shadow
        px(buf, gx, oy + 7, 3, 2, glove); // glove highlight
    } else {
        // Rear glove guards the right of the face.
        px(buf, ox + 7, oy + 6, 2, 2, glove_dk);
        px(buf, ox + 7, oy + 6, 2, 1, glove);
        // Lead arm + glove on the left, extends LEFT with `ext`.
        let arm_x = ox - ext;
        px(buf, arm_x, oy + 8, 1 + ext, 2, SKIN); // forearm reaching out
        let gx = arm_x - 3;
        px(buf, gx, oy + 7, 3, 4, glove_dk); // glove shadow
        px(buf, gx, oy + 7, 3, 2, glove); // glove highlight
    }
}

/// Two little COLORED half-block boxers mid-choreography for `frame`.
///
/// Returns exactly [`FIGHTER_ROWS`] lines, each [`FIGHTER_WIDTH`] cells wide, so
/// the loading panel never jumps. The choreography loops seamlessly with period
/// [`FIGHTER_CYCLE`]. PURE.
///
/// RED corner (left, faces right) vs BLUE corner (right, faces left), painted as
/// chunky truecolor sprites (hair/face, tank top, colored gloves + shorts,
/// legs) into a [`PixelBuf`] and rasterized to half-block (`▀`) cells. Limb
/// poses are tweened from the shared [`fighter_phase`] timeline: both bob on
/// their feet → RED jabs (lead glove extends) while BLUE leans back → reset →
/// BLUE kicks (lead leg out) while RED steps back → reset → loop.
pub fn fighters_frame(frame: usize) -> Vec<Line<'static>> {
    let (red_pose, blue_pose) = poses(frame);
    let mut buf = PixelBuf::new();

    // Figures are an 8px core; give the lead glove/leg room to extend toward
    // center. RED sits on the left third, BLUE on the right third, leaving a gap
    // in the middle that the jab/kick reaches across. base_y keeps a 1px ground
    // margin (24px tall canvas, 22px figure + bounce).
    let base_y = 1;
    draw_boxer(&mut buf, 7, base_y, Corner::Red, &red_pose);
    draw_boxer(&mut buf, SPRITE_W as i32 - 15, base_y, Corner::Blue, &blue_pose);

    buf.rasterize()
}

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

    // --- fighters (COLORED half-block API) ---------------------------------

    /// The rendered cell width of a `▀`-rasterized line: one cell per span (the
    /// rasterizer emits exactly one span per pixel column). Used to assert the
    /// fixed panel width without re-deriving the glyph encoding.
    fn line_cells(line: &Line<'static>) -> usize {
        line.spans.len()
    }

    /// A frame's pixel signature: which (row, col) cells carry color, so two
    /// frames can be compared for "did the art move" without caring about exact
    /// RGB. A cell is "lit" when its span is not a plain space.
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

    #[test]
    fn fighters_frame_is_pure() {
        assert_eq!(lit_signature(&fighters_frame(3)), lit_signature(&fighters_frame(3)));
        assert_eq!(
            lit_signature(&fighters_frame(0)),
            lit_signature(&fighters_frame(FIGHTER_CYCLE))
        );
    }

    #[test]
    fn fighters_frame_has_fixed_dimensions() {
        for f in 0..(FIGHTER_CYCLE * 3 + 7) {
            let lines = fighters_frame(f);
            assert_eq!(lines.len(), FIGHTER_ROWS, "row count at frame {f}");
            for line in &lines {
                assert_eq!(line_cells(line), FIGHTER_WIDTH, "width at frame {f}");
            }
        }
    }

    #[test]
    fn fighters_frame_loops_seamlessly() {
        // One full period returns to the start (by lit-cell signature).
        for f in 0..FIGHTER_CYCLE {
            assert_eq!(
                lit_signature(&fighters_frame(f)),
                lit_signature(&fighters_frame(f + FIGHTER_CYCLE)),
                "loop mismatch at frame {f}"
            );
        }
    }

    #[test]
    fn fighters_frame_actually_changes_across_the_loop() {
        // The art must take on several DISTINCT lit-cell layouts over a loop, or
        // it would not read as a fight. (Placeholder bobs + lean/reach/kick give
        // multiple distinct frames; the Art phase only adds more.)
        let mut seen = std::collections::HashSet::new();
        for f in 0..FIGHTER_CYCLE {
            seen.insert(lit_signature(&fighters_frame(f)));
        }
        assert!(
            seen.len() >= 4,
            "expected several distinct fighter frames over a loop, got {}",
            seen.len()
        );
    }

    #[test]
    fn fighters_frame_uses_truecolor() {
        // At least one cell in the loop carries a truecolor (Rgb) foreground,
        // proving the API surfaces color (red/blue corners).
        let mut saw_rgb = false;
        for f in 0..FIGHTER_CYCLE {
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
    fn fighter_phase_covers_the_choreography_beats() {
        // Over a loop we must hit the bob/jab/kick/reset beats.
        let mut phases = std::collections::HashSet::new();
        for f in 0..FIGHTER_CYCLE {
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
