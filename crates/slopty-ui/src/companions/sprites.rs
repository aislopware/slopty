//! The companions' pixels: every body and prop as an eight-by-eight grid of [`Ink`].
//!
//! They are written as ASCII so a diff shows the drawing. [`grid`] reads the rows in a `const`
//! item, so a row of the wrong width or a cell that is no ink fails the build.
//!
//! One character a cell:
//!
//! | cell | ink |
//! |---|---|
//! | `.` | nothing |
//! | `b` | the body: the kind's colour |
//! | `s` | its shade, or pi's second colour |
//! | `a` | its accent: a tuft, braces, a frame, an accessory, a lit key; pi's third colour |
//! | `o` | the outline: feet, a closed eye, a mouth |
//! | `e` | an eye |
//! | `g` | a glint in an eye (the large grid makes these) |
//! | `k` | a prop: a keyboard, a page, a terminal |
//! | `c` | the cue: the state's tone (a hand raised in amber, a plaster in red) |
//! | `m` | a quiet mark: a thought, a "z", a clock |
//! | `_` | (overlays only) clears what is under it |

/// What a sprite cell is drawn in: a slot the palette fills from the theme at paint.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Ink {
    /// Nothing is drawn.
    #[default]
    Clear,
    /// The kind's colour.
    Body,
    /// The body's shade, or pi's second colour.
    Shade,
    /// The kind's accent, or pi's third colour.
    Accent,
    /// Feet, a closed eye, a mouth: `text` under Increase Contrast.
    Outline,
    /// An eye: the plane or the text, whichever reads on the body.
    Eye,
    /// The light in an eye: the other of the two.
    Glint,
    /// What the companion holds or works at.
    Prop,
    /// The state's own tone.
    Cue,
    /// A quiet mark beside it.
    Muted,
    /// Only in an overlay: clears the cell under it.
    Erase,
}

/// The side of the small grid, in cells.
pub const SIDE: usize = 8;

/// A sprite or an overlay on the small grid, rows top first.
pub type Grid = [[Ink; SIDE]; SIDE];

/// The ink a cell's character names, or none.
const fn ink(c: u8) -> Option<Ink> {
    Some(match c {
        b'.' => Ink::Clear,
        b'b' => Ink::Body,
        b's' => Ink::Shade,
        b'a' => Ink::Accent,
        b'o' => Ink::Outline,
        b'e' => Ink::Eye,
        b'g' => Ink::Glint,
        b'k' => Ink::Prop,
        b'c' => Ink::Cue,
        b'm' => Ink::Muted,
        b'_' => Ink::Erase,
        _ => return None,
    })
}

/// The whole grid `rows` draw, read when a `const` item is evaluated.
pub const fn grid(rows: [&str; SIDE]) -> Grid {
    lay(0, &rows)
}

/// A grid clear but for `rows`, laid from row `at` down: an overlay on part of the grid.
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "evaluated in `const` items only, where an index out of range, an overflow or a \
              malformed row fails the build"
)]
pub const fn lay(at: usize, rows: &[&str]) -> Grid {
    let mut out = [[Ink::Clear; SIDE]; SIDE];
    let mut r = 0;
    while r < rows.len() {
        let bytes = rows[r].as_bytes();
        assert!(bytes.len() == SIDE, "a sprite row is eight cells");
        let mut c = 0;
        while c < SIDE {
            let Some(cell) = ink(bytes[c]) else {
                panic!("a sprite cell is one of . b s a o e g k c m _")
            };
            out[at + r][c] = cell;
            c += 1;
        }
        r += 1;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Bodies, standing. Each kind has a silhouette of its own (`docs/decisions/brand.md`,
// "Companions"): none is a vendor's mascot or mark, but pi's, whose licence allows it.
// ---------------------------------------------------------------------------------------------

/// Claude Code's: a round body under a four-point sparkle tuft, on two stubby feet. No arms, no
/// rectangle, never four legs.
#[rustfmt::skip]
pub const EMBER: Grid = grid([
    "....a...",
    "...aaa..",
    "..bbab..",
    ".bbbbbb.",
    ".bebbeb.",
    ".bbbbbb.",
    "..ssss..",
    "..o..o..",
]);

/// Codex's: a capsule with a visor, its arms a pair of braces.
#[rustfmt::skip]
pub const BRACE: Grid = grid([
    "........",
    "..bbbb..",
    ".abbbba.",
    "a.eeee.a",
    "a.bbbb.a",
    ".abbbba.",
    "..ssss..",
    "..o..o..",
]);

/// pi's: its own four-by-four mark at twice the size, in its three colours, with eyes on the bar.
#[rustfmt::skip]
pub const PI: Grid = grid([
    "bbbbbb..",
    "bebbeb..",
    "ss..bb..",
    "ss..bb..",
    "ssss..aa",
    "ssss..aa",
    "ss....aa",
    "ss....aa",
]);

/// `OpenCode`'s: a box in its mark's frame, the lower half filled as the mark's is.
#[rustfmt::skip]
pub const OP: Grid = grid([
    ".aaaaaa.",
    ".abbbba.",
    ".aebbea.",
    ".abbbba.",
    ".assssa.",
    ".assssa.",
    ".aaaaaa.",
    "..o..o..",
]);

/// Any other agent's: a soft dome, with an accessory of its own on top ([`ACCESSORIES`]).
#[rustfmt::skip]
pub const BLOB: Grid = grid([
    "........",
    "........",
    "...bb...",
    "..bbbb..",
    ".bebbeb.",
    ".bbbbbb.",
    "bbbbbbbb",
    ".ssssss.",
]);

/// Slopty's own: the mark's three-by-three grid, the chevron lit, the cursor its one eye.
#[rustfmt::skip]
pub const DOT: Grid = grid([
    "bb.ss.ss",
    "bb.ss.ss",
    "........",
    "ss.bb.ss",
    "ss.bb.ss",
    "........",
    "bb.ss.ee",
    "bb.ss.ee",
]);

/// What an unknown agent's dome wears, picked by its name. Open: a new one is a row here.
#[rustfmt::skip]
pub const ACCESSORIES: [Grid; 8] = [
    // An antenna.
    lay(0, &[
        "....a...",
        "....o...",
    ]),
    // A cap.
    lay(0, &[
        "...aa...",
        "..aaaaa.",
    ]),
    // A bow.
    lay(0, &[
        "..a.a...",
        "...a....",
    ]),
    // A horn.
    lay(0, &[
        ".....a..",
        "....a...",
    ]),
    // A sprout.
    lay(0, &[
        "...a.a..",
        "....o...",
    ]),
    // Ears.
    lay(1, &["..b..b.."]),
    // A halo.
    lay(0, &["..aaaa.."]),
    // A curl.
    lay(0, &[
        "..a.....",
        "...a....",
    ]),
];

// ---------------------------------------------------------------------------------------------
// Props and marks, laid over a body. Each is a short run of frames a pose steps through.
// ---------------------------------------------------------------------------------------------

/// Typing: a keyboard on the lap, one key lit in turn in the kind's accent.
pub const KEYBOARD: [Grid; 4] =
    [lay(7, &[".kakkkk."]), lay(7, &[".kkkakk."]), lay(7, &[".kkakkk."]), lay(7, &[".kkkkak."])];

/// Reading: a page at its side, a line read down it.
#[rustfmt::skip]
pub const PAGE: [Grid; 4] = [
    lay(3, &[
        "......kk",
        "......ok",
        "......kk",
        "......kk",
        "......kk",
    ]),
    lay(3, &[
        "......kk",
        "......kk",
        "......ok",
        "......kk",
        "......kk",
    ]),
    lay(3, &[
        "......kk",
        "......kk",
        "......kk",
        "......ok",
        "......kk",
    ]),
    lay(3, &[
        "......kk",
        "......kk",
        "......ok",
        "......kk",
        "......kk",
    ]),
];

/// Running a command: a little screen beside it, its cursor blinking.
#[rustfmt::skip]
pub const TERMINAL: [Grid; 4] = [
    lay(5, &[
        ".....kkk",
        ".....kak",
        ".....kkk",
    ]),
    lay(5, &[
        ".....kkk",
        ".....kak",
        ".....kkk",
    ]),
    lay(5, &[
        ".....kkk",
        ".....kkk",
        ".....kkk",
    ]),
    lay(5, &[
        ".....kkk",
        ".....kkk",
        ".....kkk",
    ]),
];

/// Thinking with no call running: a thought rising off it.
pub const THOUGHT: [Grid; 4] =
    [lay(2, &[".......m"]), lay(1, &[".......m"]), lay(0, &[".......m"]), lay(0, &["........"])];

/// A subagent started: a little one, in the accent, hops off its side.
#[rustfmt::skip]
pub const MINI: [Grid; 4] = [
    lay(6, &[
        "......ae",
        "......aa",
    ]),
    lay(5, &[
        "......ae",
        "......aa",
    ]),
    lay(6, &[
        "......ae",
        "......aa",
    ]),
    lay(6, &[
        "......ae",
        "......aa",
    ]),
];

/// Keeping a plan or a task list: a list beside it, ticked down.
#[rustfmt::skip]
pub const LIST: [Grid; 4] = [
    lay(3, &[
        "......kk",
        "......kk",
        "......kk",
        "......kk",
        "......kk",
    ]),
    lay(3, &[
        "......kk",
        "......ak",
        "......kk",
        "......kk",
        "......kk",
    ]),
    lay(3, &[
        "......kk",
        "......ak",
        "......ak",
        "......kk",
        "......kk",
    ]),
    lay(3, &[
        "......kk",
        "......ak",
        "......ak",
        "......ak",
        "......kk",
    ]),
];

/// Needs you: one arm up, the hand in the state's amber; the second frame reaches higher.
#[rustfmt::skip]
pub const WAVE: [Grid; 2] = [
    lay(1, &[
        ".......c",
        ".......b",
        ".......b",
    ]),
    lay(0, &[
        ".......c",
        ".......b",
        ".......b",
        ".......b",
    ]),
];

/// A turn done: both arms up, the hands in the state's tone.
#[rustfmt::skip]
pub const CHEER: Grid = lay(1, &[
    "c......c",
    "b......b",
    "b......b",
]);

/// Changes to review: a page held up.
#[rustfmt::skip]
pub const HELD_PAGE: Grid = lay(0, &[
    "......kk",
    "......ok",
    "......kk",
    ".......b",
]);

/// Waiting on its own work: a little clock above it.
#[rustfmt::skip]
pub const CLOCK: Grid = lay(0, &[
    "......mm",
    "......mm",
]);

/// Asleep: a "z" rising in three steps.
pub const SNORE: [Grid; 3] = [lay(2, &[".......m"]), lay(1, &[".......m"]), lay(0, &[".......m"])];
