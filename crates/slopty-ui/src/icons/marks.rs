//! Each agent's own mark: Claude Code wears the Claude spark, Codex `OpenAI`'s Blossom, pi its
//! 4 × 4 cells, and any other agent the neutral [`super::AGENT`] glyph
//! (`docs/decisions/brand.md`, "Each agent wears its owner's mark").
//!
//! A mark is drawn by us, not by the OS: the spark's and the Blossom's published outlines
//! (`assets/agents/`, credited in its `NOTICE`) through Core Graphics
//! ([`slopty_platform::outline`]), pi's cells straight onto whole device pixels. Each becomes
//! the same alpha mask an SF Symbol is, painted in the ink of the words beside it and never in
//! a brand's colour.
//!
//! A mark is sized by its ink, not its box. A radial mark's ink box takes its slot less
//! [`slopty_theme::Spacing::xxs`] (twice that in an empty state's larger slot); pi's filled
//! square reads larger than a radial mark as wide, so it takes [`PI_SHARE`] of that, on whole
//! cells.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::SharedString;
use parking_lot::RwLock;
use slopty_platform::outline::{Outline, rasterize_outline};
use slopty_platform::symbols::{MaskRect, SymbolMask};
use slopty_proto::thread::AgentId;

use super::Kept;

/// Which mark an agent wears.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AgentMark {
    /// Claude Code: the Claude spark.
    Claude,
    /// Codex: `OpenAI`'s Blossom.
    Blossom,
    /// pi: its ten cells on a 4 × 4 grid.
    Pi,
    /// Any other agent, an ACP agent among them: the neutral [`super::AGENT`] glyph, its name
    /// in words.
    Neutral,
}

/// The marks shown as words alone, with the neutral glyph: an owner who asks us not to show
/// theirs is honoured by adding it here, and nothing else changes.
const WORDS_ONLY: &[AgentMark] = &[];

impl AgentMark {
    /// The marks drawn by us, every one but the neutral glyph.
    pub const OWNED: [Self; 3] = [Self::Claude, Self::Blossom, Self::Pi];

    /// The mark the agent named `agent` (an [`AgentId`]'s name) wears.
    #[must_use]
    pub fn of(agent: &str) -> Self {
        let mark = match agent {
            AgentId::CLAUDE_CODE => Self::Claude,
            AgentId::CODEX => Self::Blossom,
            AgentId::PI => Self::Pi,
            _ => Self::Neutral,
        };
        if WORDS_ONLY.contains(&mark) { Self::Neutral } else { mark }
    }

    /// The agent it names, for a screen reader and a tooltip; `None` for the neutral glyph,
    /// whose agent is named in words beside it.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("Claude Code"),
            Self::Blossom => Some("Codex"),
            Self::Pi => Some("pi"),
            Self::Neutral => None,
        }
    }

    /// Its name in an atlas key.
    const fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Blossom => "blossom",
            Self::Pi => "pi",
            Self::Neutral => "neutral",
        }
    }
}

/// The share of a radial mark's ink pi's square takes: a filled square reads about a seventh
/// larger than a radial mark as wide.
const PI_SHARE: f32 = 0.86;

/// pi's cells on its 4 × 4 grid, as (column, row) from the top left: three across the top,
/// the first and third under them, a stem on the left the whole way down and a foot hanging at
/// the right, as pi's own one-colour badge (`pi.dev/favicon.svg`) and its source lay them.
const PI_CELLS: [(usize, usize); 10] =
    [(0, 0), (1, 0), (2, 0), (0, 1), (2, 1), (0, 2), (1, 2), (3, 2), (0, 3), (3, 3)];

/// pi's grid, cells to a side.
const PI_GRID: usize = 4;

/// The fewest device pixels a pi cell is drawn on: under it the cells' gaps close.
const PI_LEAST_CELL: u32 = 2;

/// The spark's and the Blossom's outlines, read once from the files `assets/agents/` keeps; a
/// file that does not read is a miss, said once.
static OUTLINES: LazyLock<[Option<Outline>; 2]> = LazyLock::new(|| {
    let read = |name: &str, svg: &str| {
        Outline::parse(&path_data(svg))
            .inspect_err(|error| tracing::warn!(%error, name, "an agent's mark did not read"))
            .ok()
    };
    [
        read("claude", include_str!("../../assets/agents/claude.svg")),
        read("openai", include_str!("../../assets/agents/openai.svg")),
    ]
});

/// Every path's data (`d="…"`) in `svg`, in order.
pub(super) fn path_data(svg: &str) -> Vec<&str> {
    svg.split(" d=\"").skip(1).filter_map(|rest| rest.split('"').next()).collect()
}

/// The masks drawn so far, by mark and device pixels (a radial mark's ink side, pi's cell).
static KEPT: LazyLock<RwLock<HashMap<(AgentMark, u32), Kept>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// How many device pixels `mark` is drawn on for a radial ink box `ink` points across on a
/// display of `device` pixels to the point: a radial mark's ink side, or pi's cell.
#[must_use]
pub(super) fn pixels(mark: AgentMark, ink: f32, device: f32) -> u32 {
    let at = |points: f32| {
        let pixels = (points * device).round();
        #[expect(clippy::cast_possible_truncation, reason = "a mark is a few dozen pixels")]
        #[expect(clippy::cast_sign_loss, reason = "a negative size is caught as no size")]
        let pixels = if pixels.is_finite() && pixels > 0.0 { pixels as u32 } else { 0 };
        pixels
    };
    match mark {
        #[expect(clippy::cast_precision_loss, reason = "the grid is four cells")]
        AgentMark::Pi => at(ink * PI_SHARE / PI_GRID as f32).max(PI_LEAST_CELL),
        _ => at(ink),
    }
}

/// `mark` with a radial ink box `ink` points across, for a display of `device` pixels to the
/// point, and its atlas key; `None` for the neutral glyph, which is a symbol.
pub(super) fn mask(mark: AgentMark, ink: f32, device: f32) -> Kept {
    let pixels = pixels(mark, ink, device);
    if mark == AgentMark::Neutral || pixels == 0 {
        return None;
    }
    let at = (mark, pixels);
    if let Some(kept) = KEPT.read().get(&at) {
        return kept.clone();
    }
    let [spark, blossom] = &*OUTLINES;
    let drawn = match mark {
        AgentMark::Claude => spark.as_ref().and_then(|o| rasterize_outline(o, pixels)),
        AgentMark::Blossom => blossom.as_ref().and_then(|o| rasterize_outline(o, pixels)),
        AgentMark::Pi => pi(pixels),
        AgentMark::Neutral => None,
    };
    let kept =
        drawn.map(|m| (Arc::new(m), SharedString::from(format!("mark:{}:{pixels}", mark.key()))));
    KEPT.write().entry(at).or_insert(kept).clone()
}

/// pi's mark on cells `cell` device pixels square: every pixel wholly ink or wholly clear.
fn pi(cell: u32) -> Option<SymbolMask> {
    let cell = usize::try_from(cell).ok()?;
    let side = cell.checked_mul(PI_GRID)?;
    let inked = |x: usize, y: usize| {
        x.checked_div(cell).zip(y.checked_div(cell)).is_some_and(|at| PI_CELLS.contains(&at))
    };
    let alpha = (0..side)
        .flat_map(|y| (0..side).map(move |x| if inked(x, y) { u8::MAX } else { 0 }))
        .collect();
    let width = u32::try_from(side).ok()?;
    #[expect(clippy::cast_precision_loss, reason = "a mark is a few dozen pixels")]
    let edge = side as f32;
    Some(SymbolMask {
        width,
        height: width,
        alpha,
        alignment: MaskRect { x: 0.0, y: 0.0, width: edge, height: edge },
        baseline: edge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lint as a test: every agent the wire names wears a mark of its own, and nothing else
    /// does. A new name in `slopty_proto::AgentId` is listed here, with its mark, or with the
    /// neutral glyph on purpose.
    #[test]
    fn every_agent_wears_a_mark() {
        let named = [
            (AgentId::CLAUDE_CODE, AgentMark::Claude),
            (AgentId::CODEX, AgentMark::Blossom),
            (AgentId::PI, AgentMark::Pi),
        ];
        for (agent, mark) in named {
            assert_eq!(AgentMark::of(agent), mark, "{agent}");
            assert!(mark.label().is_some(), "{agent} is named for a screen reader");
        }
        let proto = include_str!("../../../slopty-proto/src/thread.rs");
        let body = proto.split("impl AgentId {").nth(1).and_then(|r| r.split("\n}\n").next());
        let consts = body
            .unwrap_or_default()
            .lines()
            .filter(|l| l.trim_start().starts_with("pub const ") && !l.contains("ACP_PREFIX"))
            .count();
        assert_eq!(consts, named.len(), "an agent named on the wire has no mark listed here");
        for other in ["acp:gemini", "acp:", "goose", ""] {
            assert_eq!(AgentMark::of(other), AgentMark::Neutral, "{other:?}");
        }
        for mark in AgentMark::OWNED {
            for device in [1.0, 2.0, 3.0] {
                let (m, _) = mask(mark, 14.0, device).unwrap();
                let want = pixels(mark, 14.0, device);
                let side = m.width.max(m.height);
                let want = if mark == AgentMark::Pi { want.saturating_mul(4) } else { want };
                assert_eq!(side, want, "{mark:?} at {device}x is its size across");
                assert!(m.alpha.iter().any(|a| *a > 0), "{mark:?} at {device}x has ink");
            }
        }
        assert!(mask(AgentMark::Neutral, 14.0, 1.0).is_none(), "the neutral glyph is a symbol");
    }

    /// The study's sizes at 1x and 2x: spark and Blossom 14 px in a row's 16 pt slot, pi 12 px
    /// on 3 px cells; the chip's 12 pt ink and the notice's 24 pt.
    #[test]
    fn a_mark_is_drawn_at_its_optical_size() {
        let px = |mark, ink, device| pixels(mark, ink, device);
        assert_eq!(px(AgentMark::Claude, 14.0, 1.0), 14);
        assert_eq!(px(AgentMark::Blossom, 14.0, 2.0), 28);
        assert_eq!(px(AgentMark::Pi, 14.0, 1.0), 3, "12 px on 3 px cells");
        assert_eq!(px(AgentMark::Pi, 12.0, 1.0), 3, "the chip's 12 px");
        assert_eq!(px(AgentMark::Pi, 12.0, 2.0), 5, "the chip's 20 px at 2x");
        assert_eq!(px(AgentMark::Pi, 24.0, 1.0), 5, "the notice's 20 px");
        assert_eq!(px(AgentMark::Pi, 24.0, 2.0), 10, "the notice's 40 px at 2x");
        assert_eq!(px(AgentMark::Pi, 1.0, 1.0), PI_LEAST_CELL, "never under two pixels a cell");
    }

    /// The centroid of `m`'s ink, from its top left, in pixels.
    fn centroid(m: &SymbolMask) -> (f64, f64) {
        let width = usize::try_from(m.width).unwrap();
        let (mut sum, mut x, mut y) = (0.0, 0.0, 0.0);
        for (row, line) in m.alpha.chunks(width).enumerate() {
            for (column, a) in line.iter().enumerate() {
                let a = f64::from(*a);
                #[expect(clippy::cast_precision_loss, reason = "a mask is a few dozen pixels")]
                let (px, py) = (column as f64 + 0.5, row as f64 + 0.5);
                sum += a;
                x = a.mul_add(px, x);
                y = a.mul_add(py, y);
            }
        }
        (x / sum, y / sum)
    }

    /// A radial mark's mask is its ink box, so centring the mask centres the ink: its centroid
    /// lies within half a pixel of the mask's centre at 1x. pi's grid box is what the eye
    /// centres, and the mask is exactly it.
    #[test]
    fn a_mark_is_centred_on_its_ink() {
        for mark in [AgentMark::Claude, AgentMark::Blossom] {
            let (m, _) = mask(mark, 14.0, 1.0).unwrap();
            let (x, y) = centroid(&m);
            let (cx, cy) = (f64::from(m.width) / 2.0, f64::from(m.height) / 2.0);
            assert!(
                (x - cx).abs() <= 0.5 && (y - cy).abs() <= 0.5,
                "{mark:?}: {x},{y} of {cx},{cy}"
            );
            let width = usize::try_from(m.width).unwrap_or(0);
            let rows: Vec<&[u8]> = m.alpha.chunks(width).collect();
            let inked = |line: &[u8]| line.iter().any(|a| *a > 0);
            assert!(rows.first().is_some_and(|r| inked(r)), "{mark:?} reaches its top");
            assert!(rows.last().is_some_and(|r| inked(r)), "{mark:?} reaches its foot");
            assert!(rows.iter().any(|r| r.first().is_some_and(|a| *a > 0)), "{mark:?} left");
            assert!(rows.iter().any(|r| r.last().is_some_and(|a| *a > 0)), "{mark:?} right");
        }
        let (m, _) = mask(AgentMark::Pi, 14.0, 1.0).unwrap();
        assert_eq!((m.width, m.height), (12, 12), "pi's grid box is its mask");
    }

    /// pi lands on whole pixels: every pixel wholly ink or wholly clear, at 1x, 2x and 3x, and
    /// as many inked as its ten cells cover.
    #[test]
    fn pi_lands_on_whole_pixels() {
        for device in [1.0, 2.0, 3.0] {
            let cell = pixels(AgentMark::Pi, 14.0, device);
            let (m, _) = mask(AgentMark::Pi, 14.0, device).unwrap();
            assert!(m.alpha.iter().all(|a| *a == 0 || *a == u8::MAX), "{device}x is solid");
            let inked: usize = m.alpha.iter().map(|a| usize::from(*a == u8::MAX)).sum();
            let cell = usize::try_from(cell).unwrap_or(0);
            assert_eq!(
                inked,
                PI_CELLS.len().saturating_mul(cell.saturating_mul(cell)),
                "{device}x"
            );
        }
    }

    /// The marks are crisp at 1x through Core Graphics: crisp (Σα²/Σα, 1 when every inked
    /// pixel is whole) and solid (the share of inked pixels at α ≥ 0.9) at the sizes they are
    /// drawn at, printed for `docs/MEASUREMENTS.md`. The spark at its row size is as crisp as
    /// the SF Symbols beside it (about 0.73 at 13 pt), the Blossom's thin inner strokes a little
    /// softer, and pi whole.
    #[test]
    fn the_marks_are_crisp_at_1x() {
        let measure = |m: &SymbolMask| {
            let (mut a2, mut a1, mut inked, mut solid) = (0.0, 0.0, 0_u32, 0_u32);
            for a in &m.alpha {
                let a = f64::from(*a) / 255.0;
                a2 = a.mul_add(a, a2);
                a1 += a;
                inked = inked.saturating_add(u32::from(a > 0.0));
                solid = solid.saturating_add(u32::from(a >= 0.9));
            }
            (a2 / a1, f64::from(solid) / f64::from(inked))
        };
        let mut rows = Vec::new();
        for mark in [AgentMark::Claude, AgentMark::Blossom] {
            for ink in [12.0, 13.0, 14.0, 16.0, 24.0] {
                let (crisp, solid) = measure(&mask(mark, ink, 1.0).unwrap().0);
                eprintln!("{mark:?} {ink} px: crisp {crisp:.3} solid {solid:.2}");
                rows.push((mark, ink, crisp));
            }
        }
        let (pi, _) = measure(&mask(AgentMark::Pi, 14.0, 1.0).unwrap().0);
        eprintln!("Pi 12 px: crisp {pi:.3}");
        let at = |mark, size: f32| {
            rows.iter().find(|r| r.0 == mark && (r.1 - size).abs() < 0.5).map(|r| r.2)
        };
        let (spark, blossom) = (at(AgentMark::Claude, 14.0), at(AgentMark::Blossom, 14.0));
        assert!(spark.unwrap() > 0.72, "the spark at 14 px: {spark:?}");
        assert!(blossom.unwrap() > 0.6, "the Blossom at 14 px: {blossom:?}");
        assert!((pi - 1.0).abs() < f64::EPSILON, "pi whole: {pi}");
    }

    /// What a mark costs to read and draw, for `docs/MEASUREMENTS.md`: the outlines parsed
    /// from their files, and each mark drawn afresh at a row's 14 px and at 2x.
    #[test]
    #[ignore = "a measurement: cargo test --release -p slopty-ui --lib measure_mark_rasters -- --ignored --nocapture"]
    fn measure_mark_rasters() {
        const RUNS: u32 = 200;
        let started = std::time::Instant::now();
        for _ in 0..RUNS {
            let svg = include_str!("../../assets/agents/claude.svg");
            Outline::parse(&path_data(svg)).unwrap();
            let svg = include_str!("../../assets/agents/openai.svg");
            Outline::parse(&path_data(svg)).unwrap();
        }
        eprintln!("both outlines parsed: {:?}", started.elapsed() / RUNS);
        let [spark, blossom] = &*OUTLINES;
        for (name, outline) in [("spark", spark), ("blossom", blossom)] {
            let outline = outline.as_ref().unwrap();
            for pixels in [14, 28] {
                let started = std::time::Instant::now();
                for _ in 0..RUNS {
                    assert!(rasterize_outline(outline, pixels).is_some());
                }
                eprintln!("{name} at {pixels} px: {:?}", started.elapsed() / RUNS);
            }
        }
        let started = std::time::Instant::now();
        for _ in 0..RUNS {
            assert!(pi(6).is_some());
        }
        eprintln!("pi at 24 px: {:?}", started.elapsed() / RUNS);
    }
}
