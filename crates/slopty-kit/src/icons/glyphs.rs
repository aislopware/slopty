//! The chrome's glyphs: IBM Carbon's icons (`assets/icons/carbon/`, Apache-2.0, credited in its
//! `NOTICE`), one family on every platform, the companion of the IBM Plex the chrome is set in
//! (`docs/decisions/ui.md`, "The chrome's icons are IBM Carbon's at 16").
//!
//! A glyph is drawn by us from its outline ([`slopty_platform::outline::rasterize_on_grid`]):
//! its 32-unit grid scaled to a whole number of device pixels and filled, since Carbon draws
//! its two-unit lines as shapes. At the 16 pt slot a line is one device pixel at 1x and two at
//! 2x, on the pixel grid. Each becomes an alpha mask painted in the ink of the words beside it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::SharedString;
use parking_lot::RwLock;
use slopty_platform::outline::{Outline, rasterize_on_grid};

use super::Kept;

/// Declares [`Symbol`] and the Carbon file that draws each in one list.
macro_rules! glyphs {
    ($($variant:ident => $file:literal,)+) => {
        /// One glyph the chrome draws, by the name the chrome knows it under; its Carbon file is
        /// [`Symbol::name`], and the `NOTICE` beside the files lists the pairs.
        ///
        /// The list is closed: a glyph the chrome draws is added here with its file, and
        /// `every_glyph_reads` checks each one reads and every file is drawn.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Symbol {
            $(#[doc = concat!("Carbon's `", $file, "`.")] $variant,)+
        }

        impl Symbol {
            /// Every glyph, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// Its Carbon file's name, which names its atlas key.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $file,)+
                }
            }

            /// Its file's markup.
            const fn svg(self) -> &'static str {
                match self {
                    $(Self::$variant => include_str!(
                        concat!("../../assets/icons/carbon/", $file, ".svg")
                    ),)+
                }
            }
        }
    };
}

glyphs! {
    AiAgent => "ai-agent",
    ArrowClockwise => "renew",
    ArrowDown => "arrow--down",
    ArrowDownToLine => "down-to-bottom",
    ArrowLeft => "arrow--left",
    ArrowRight => "arrow--right",
    ArrowTriangleBranch => "branch",
    ArrowTriangleMerge => "merge",
    ArrowTrianglePull => "pull-request",
    ArrowUp => "arrow--up",
    ArrowUpAndDown => "arrows--vertical",
    ArrowUpRight => "arrow--up-right",
    ArrowUpToLine => "up-to-top",
    ArrowUturnBackward => "undo",
    Bell => "notification",
    BellBadge => "notification--new",
    BellSlash => "notification--off",
    CharacterCursorIbeam => "pointer-text",
    Checklist => "list--checked",
    Checkmark => "checkmark",
    CheckmarkCircle => "checkmark--outline",
    CheckmarkCircleFill => "checkmark--filled",
    ChevronDown => "chevron--down",
    ChevronLeft => "chevron--left",
    ChevronLeftForwardslashChevronRight => "code",
    ChevronRight => "chevron--right",
    ChevronUp => "chevron--up",
    Circle => "circle--outline",
    CircleDashed => "circle-dash",
    CircleInsetFilled => "radio-button--checked",
    Clock => "time",
    Command => "mac--command",
    Cursorarrow => "cursor--1",
    Display => "screen",
    Doc => "document--blank",
    DocBadgePlus => "document--add",
    DocOnClipboard => "copy--to-clipboard",
    DocOnDoc => "copy",
    DocText => "document",
    Ellipsis => "overflow-menu--horizontal",
    ExclamationmarkTriangle => "warning--alt",
    ExclamationmarkTriangleFill => "warning--alt--filled",
    Eye => "view",
    EyeSlash => "view--off",
    Flag => "flag",
    Fold => "row--collapse",
    Folder => "folder",
    FolderBadgePlus => "folder--add",
    Globe => "globe",
    InfoCircle => "information",
    Iphone => "mobile",
    Keyboard => "keyboard",
    Laptopcomputer => "laptop",
    Link => "link",
    Lock => "locked",
    Macwindow => "application",
    Magnifyingglass => "search",
    Minus => "subtract",
    Paperclip => "attachment",
    Palette => "color-palette",
    PauseCircle => "pause--outline",
    Person => "user",
    Pencil => "edit",
    Photo => "image",
    Pin => "pin",
    Plus => "add",
    PlusForwardslashMinus => "compare",
    Power => "power",
    PuzzlepieceExtension => "plug",
    QuestionmarkBubble => "help",
    RectangleSplit3x1 => "split-screen",
    Scissors => "cut",
    Send => "send--alt",
    ServerRack => "bare-metal-server",
    SidebarLeft => "side-panel--open",
    SpeakerSlash => "volume--mute",
    SpeakerWave2 => "volume--up",
    SquareGrid2x2 => "grid",
    StopFill => "stop--filled",
    Terminal => "terminal",
    TextBubble => "chat",
    Trash => "trash-can",
    Unfold => "row--expand",
    WifiSlash => "wifi--off",
    WrenchAndScrewdriver => "tool-kit",
    Xmark => "close",
    XmarkCircle => "close--outline",
    GitCommit => "commit",
    GitRepo => "git--repo",
}

/// A glyph's drawing as its file has it: the outline and the side of its grid in its own units.
#[derive(Debug)]
struct Drawing {
    outline: Outline,
    grid: f64,
}

/// The value of `name="…"` in `tag`.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    tag.split(&format!(" {name}=\"")).nth(1)?.split('"').next()
}

/// Reads a Carbon file: its view box's side and its paths, which are filled.
fn read(svg: &str) -> Result<Drawing, String> {
    let root = svg.split('>').next().unwrap_or_default();
    let grid = attribute(root, "viewBox")
        .and_then(|b| b.split_whitespace().nth(2)?.parse::<f64>().ok())
        .ok_or("no view box")?;
    let outline = Outline::parse(&super::marks::path_data(svg)).map_err(|e| e.to_string())?;
    Ok(Drawing { outline, grid })
}

/// Every glyph's drawing, read once from its file; a file that does not read is a miss, said
/// once.
static DRAWINGS: LazyLock<HashMap<Symbol, Drawing>> = LazyLock::new(|| {
    Symbol::ALL
        .iter()
        .filter_map(|&symbol| {
            read(symbol.svg())
                .inspect_err(|error| {
                    tracing::warn!(%error, name = symbol.name(), "a glyph did not read");
                })
                .ok()
                .map(|drawing| (symbol, drawing))
        })
        .collect()
});

/// The masks drawn so far, by glyph and the grid's side in device pixels.
static KEPT: LazyLock<RwLock<HashMap<(Symbol, u32), Kept>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// `symbol` with its grid `side` device pixels square, and its atlas key.
pub(super) fn mask(symbol: Symbol, side: u32) -> Kept {
    if side == 0 {
        return None;
    }
    let at = (symbol, side);
    if let Some(kept) = KEPT.read().get(&at) {
        return kept.clone();
    }
    let drawing = DRAWINGS.get(&symbol)?;
    let kept = rasterize_on_grid(&drawing.outline, drawing.grid, side)
        .map(|m| (Arc::new(m), SharedString::from(format!("glyph:{}:{side}", symbol.name()))));
    KEPT.write().entry(at).or_insert(kept).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lint as a test: every glyph's file reads on Carbon's 32 grid and draws ink at the
    /// 16 px slot; every file kept is some glyph's, and the `NOTICE` names each glyph with its
    /// file.
    #[test]
    fn every_glyph_reads() {
        let notice = include_str!("../../assets/icons/carbon/NOTICE");
        for &symbol in Symbol::ALL {
            let drawing = read(symbol.svg()).unwrap_or_else(|e| panic!("{symbol:?}: {e}"));
            assert!((drawing.grid - 32.0).abs() < f64::EPSILON, "{symbol:?} on the 32 grid");
            let (m, _) = mask(symbol, 16).unwrap_or_else(|| panic!("{symbol:?} is drawn"));
            assert_eq!((m.width, m.height), (16, 16), "{symbol:?} is its grid's size");
            assert!(m.alpha.iter().any(|a| *a > 0), "{symbol:?} has ink");
            let listed = notice.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some(&format!("{symbol:?}"))
                    && words.next() == Some(&format!("{}.svg", symbol.name()))
            });
            assert!(listed, "the NOTICE names {symbol:?} with {}.svg", symbol.name());
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/carbon");
        let files: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .filter_map(|n| n.strip_suffix(".svg").map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        assert!(!files.is_empty(), "the glyphs are kept in {dir}");
        for file in files {
            assert!(Symbol::ALL.iter().any(|s| s.name() == file), "{file}.svg is drawn by none");
        }
    }

    /// A glyph's grid is a whole number of device pixels, twice as many at 2x. At 1x Carbon's
    /// two-unit line on a 16 px grid is one pixel wide and lands on whole columns and rows, so
    /// the folder's straight sides and the terminal's frame are solid pixels.
    #[test]
    fn a_glyph_is_drawn_on_whole_pixels() {
        for symbol in [Symbol::Folder, Symbol::Terminal, Symbol::ArrowTriangleBranch] {
            let (one, _) = mask(symbol, 16).unwrap_or_else(|| panic!("{symbol:?} at 1x"));
            let (two, _) = mask(symbol, 32).unwrap_or_else(|| panic!("{symbol:?} at 2x"));
            assert_eq!((one.width, two.width), (16, 32), "{symbol:?}");
        }
        for symbol in [Symbol::Folder, Symbol::Terminal] {
            let (m, _) = mask(symbol, 16).unwrap_or_else(|| panic!("{symbol:?} at 1x"));
            let solid = m.alpha.iter().filter(|a| **a >= 230).count();
            let soft = m.alpha.iter().filter(|a| (40..230).contains(*a)).count();
            assert!(solid >= 30 && soft < solid, "{symbol:?} at 1x: {solid} solid, {soft} soft");
        }
        assert!(mask(Symbol::Folder, 0).is_none(), "no size, no mask");
    }

    /// What a first frame pays to draw its glyphs, so none is drawn ahead of it: every glyph
    /// read and drawn uncached at 14 and the slot's 16 pt, at 1x and 2x, printed for
    /// `docs/MEASUREMENTS.md` ("Tabler glyphs drawn on demand", "Carbon glyphs drawn on
    /// demand"). A screen of chrome draws a few dozen.
    #[test]
    fn a_glyph_is_cheap_to_draw_on_demand() {
        let started = std::time::Instant::now();
        let drawings: Vec<Drawing> =
            Symbol::ALL.iter().filter_map(|s| read(s.svg()).ok()).collect();
        let read_all = started.elapsed();
        let mut each = Vec::new();
        for drawing in &drawings {
            for side in [14, 16, 28, 32] {
                let at = std::time::Instant::now();
                let drawn = rasterize_on_grid(&drawing.outline, drawing.grid, side);
                each.push(at.elapsed());
                assert!(drawn.is_some());
            }
        }
        each.sort();
        let total: std::time::Duration = each.iter().sum();
        let at = |q: usize| each.get(each.len().saturating_mul(q) / 100).copied();
        eprintln!(
            "read {} glyphs in {read_all:?}; {} draws in {total:?}, p50 {:?}, p99 {:?}, max {:?}",
            drawings.len(),
            each.len(),
            at(50),
            at(99),
            each.last()
        );
        assert!(at(50) < Some(std::time::Duration::from_micros(200)), "{:?}", at(50));
    }
}
