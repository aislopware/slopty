//! The chrome's glyphs: Tabler's outline icons (`assets/icons/tabler/`, MIT, credited in its
//! `NOTICE`), one family on every platform (`docs/decisions/ui.md`, "The chrome's icons are
//! Tabler's, and a file's are Material's").
//!
//! A glyph is drawn by us from its outline ([`slopty_platform::outline::rasterize_on_grid`]):
//! its 24-unit grid scaled to a whole number of device pixels, an outline glyph stroked at its
//! file's 1.75 units rounded to whole pixels, a filled one (`-filled`) filled. Each becomes an
//! alpha mask painted in the ink of the words beside it. Through GPUI's `svg()` the same glyph
//! lands softer at 1x, since nothing there fits a stroke to the pixels (`docs/MEASUREMENTS.md`,
//! "Tabler glyphs at 1x and 2x").

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::SharedString;
use parking_lot::RwLock;
use slopty_platform::outline::{Ink, Outline, rasterize_on_grid};

use super::Kept;

/// Declares [`Symbol`] and the Tabler file that draws each in one list.
macro_rules! glyphs {
    ($($variant:ident => $file:literal,)+) => {
        /// One glyph the chrome draws, by the name the chrome knows it under; its Tabler file is
        /// [`Symbol::name`], and the `NOTICE` beside the files lists the pairs.
        ///
        /// The list is closed: a glyph the chrome draws is added here with its file, and
        /// `every_glyph_reads` checks each one reads and every file is drawn.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Symbol {
            $(#[doc = concat!("Tabler's `", $file, "`.")] $variant,)+
        }

        impl Symbol {
            /// Every glyph, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// Its Tabler file's name, which names its atlas key.
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
                        concat!("../../assets/icons/tabler/", $file, ".svg")
                    ),)+
                }
            }
        }
    };
}

glyphs! {
    ArrowClockwise => "refresh",
    ArrowDown => "arrow-down",
    ArrowDownToLine => "arrow-bar-to-down",
    ArrowLeft => "arrow-left",
    ArrowRight => "arrow-right",
    ArrowTriangleBranch => "git-branch",
    ArrowTriangleMerge => "git-merge",
    ArrowTrianglePull => "git-pull-request",
    ArrowUp => "arrow-up",
    ArrowUpAndDown => "arrows-vertical",
    ArrowUpRight => "arrow-up-right",
    ArrowUpToLine => "arrow-bar-to-up",
    ArrowUturnBackward => "arrow-back-up",
    Bell => "bell",
    BellBadge => "bell-ringing",
    BellSlash => "bell-off",
    CharacterCursorIbeam => "cursor-text",
    Checklist => "list-check",
    Checkmark => "check",
    CheckmarkCircle => "circle-check",
    CheckmarkCircleFill => "circle-check-filled",
    ChevronDown => "chevron-down",
    ChevronLeft => "chevron-left",
    ChevronLeftForwardslashChevronRight => "code",
    ChevronRight => "chevron-right",
    ChevronUp => "chevron-up",
    Circle => "circle",
    CircleDashed => "circle-dashed",
    CircleInsetFilled => "circle-dot",
    Clock => "clock",
    Command => "command",
    Curlybraces => "braces",
    Cursorarrow => "pointer",
    Display => "device-desktop",
    Doc => "file",
    DocBadgePlus => "file-plus",
    DocOnClipboard => "clipboard-copy",
    DocOnDoc => "copy",
    DocRichtext => "file-text",
    DocText => "file-text",
    DocZipper => "file-zip",
    Ellipsis => "dots",
    ExclamationmarkCircleFill => "alert-circle-filled",
    ExclamationmarkTriangle => "alert-triangle",
    ExclamationmarkTriangleFill => "alert-triangle-filled",
    Eye => "eye",
    EyeSlash => "eye-off",
    Film => "movie",
    Flag => "flag",
    Fold => "fold",
    Folder => "folder",
    FolderBadgePlus => "folder-plus",
    Gearshape => "settings",
    Globe => "world",
    InfoCircle => "info-circle",
    Iphone => "device-mobile",
    Keyboard => "keyboard",
    Laptopcomputer => "device-laptop",
    Link => "link",
    Lock => "lock",
    LockDoc => "file-certificate",
    Macwindow => "app-window",
    Magnifyingglass => "search",
    Minus => "minus",
    Paperclip => "paperclip",
    Palette => "palette",
    PauseCircle => "player-pause",
    Pencil => "pencil",
    Photo => "photo",
    Pin => "pin",
    Plus => "plus",
    PlusForwardslashMinus => "plus-minus",
    Power => "power",
    PuzzlepieceExtension => "puzzle",
    QuestionmarkBubble => "message-question",
    RectangleSplit3x1 => "layout-columns",
    Scissors => "scissors",
    ServerRack => "server",
    SidebarLeft => "layout-sidebar",
    SpeakerSlash => "volume-off",
    SpeakerWave2 => "volume",
    SquareAndPencil => "edit",
    SquareGrid2x2 => "layout-grid",
    StopFill => "player-stop-filled",
    Terminal => "terminal-2",
    TextBubble => "message",
    TextMagnifyingglass => "file-search",
    Trash => "trash",
    Unfold => "arrows-move-vertical",
    Waveform => "wave-sine",
    WifiSlash => "wifi-off",
    WrenchAndScrewdriver => "tool",
    Xmark => "x",
    XmarkCircle => "circle-x",
    XmarkCircleFill => "circle-x-filled",
    GitCommit => "git-commit",
    GitPullRequestDraft => "git-pull-request-draft",
    GitPullRequestClosed => "git-pull-request-closed",
    GitRepo => "book-2",
}

/// A glyph's drawing as its file has it: the outline, the side of its grid in its own units,
/// and how it is inked.
#[derive(Debug)]
struct Drawing {
    outline: Outline,
    grid: f64,
    ink: Ink,
}

/// The value of `name="…"` in `tag`.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    tag.split(&format!(" {name}=\"")).nth(1)?.split('"').next()
}

/// Reads a Tabler file: its view box's side, filled where its root fills with the text's
/// colour, else stroked at its root's stroke width.
fn read(svg: &str) -> Result<Drawing, String> {
    let root = svg.split('>').next().unwrap_or_default();
    let grid = attribute(root, "viewBox")
        .and_then(|b| b.split_whitespace().nth(2)?.parse::<f64>().ok())
        .ok_or("no view box")?;
    let ink = if attribute(root, "fill") == Some("currentColor") {
        Ink::Fill
    } else {
        let width = attribute(root, "stroke-width").and_then(|w| w.parse::<f64>().ok());
        Ink::Stroke(width.ok_or("neither filled nor a stroke width")?)
    };
    let outline = Outline::parse(&super::marks::path_data(svg)).map_err(|e| e.to_string())?;
    Ok(Drawing { outline, grid, ink })
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
    let kept = rasterize_on_grid(&drawing.outline, drawing.grid, side, drawing.ink)
        .map(|m| (Arc::new(m), SharedString::from(format!("glyph:{}:{side}", symbol.name()))));
    KEPT.write().entry(at).or_insert(kept).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lint as a test: every glyph's file reads on Tabler's 24 grid, filled where its name
    /// says so and stroked at the chrome's 1.75 otherwise, and draws ink at a row's 14 px; every
    /// file kept is some glyph's, and the `NOTICE` names each glyph with its file.
    #[test]
    fn every_glyph_reads() {
        let notice = include_str!("../../assets/icons/tabler/NOTICE");
        for &symbol in Symbol::ALL {
            let drawing = read(symbol.svg()).unwrap_or_else(|e| panic!("{symbol:?}: {e}"));
            assert!((drawing.grid - 24.0).abs() < f64::EPSILON, "{symbol:?} on the 24 grid");
            let filled = symbol.name().ends_with("-filled");
            let want = if filled { Ink::Fill } else { Ink::Stroke(1.75) };
            assert_eq!(drawing.ink, want, "{symbol:?}");
            let (m, _) = mask(symbol, 14).unwrap_or_else(|| panic!("{symbol:?} is drawn"));
            assert_eq!((m.width, m.height), (14, 14), "{symbol:?} is its grid's size");
            assert!(m.alpha.iter().any(|a| *a > 0), "{symbol:?} has ink");
            let listed = notice.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some(&format!("{symbol:?}"))
                    && words.next() == Some(&format!("{}.svg", symbol.name()))
            });
            assert!(listed, "the NOTICE names {symbol:?} with {}.svg", symbol.name());
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/tabler");
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

    /// A glyph's grid is a whole number of device pixels, twice as many at 2x. At 1x a stroke
    /// of 1.75 on a 14 px grid is one pixel wide, so the folder's straight sides, which land on
    /// whole columns and rows, are whole pixels.
    #[test]
    fn a_glyph_is_drawn_on_whole_pixels() {
        for symbol in [Symbol::Folder, Symbol::Terminal, Symbol::ArrowTriangleBranch] {
            let (one, _) = mask(symbol, 14).unwrap_or_else(|| panic!("{symbol:?} at 1x"));
            let (two, _) = mask(symbol, 28).unwrap_or_else(|| panic!("{symbol:?} at 2x"));
            assert_eq!((one.width, two.width), (14, 28), "{symbol:?}");
        }
        let (folder, _) = mask(Symbol::Folder, 14).unwrap_or_else(|| panic!("the folder at 1x"));
        let solid = folder.alpha.iter().filter(|a| **a >= 230).count();
        assert!(solid >= 8, "the folder at 1x has {solid} whole pixels");
        assert!(mask(Symbol::Folder, 0).is_none(), "no size, no mask");
    }

    /// What a first frame pays to draw its glyphs, so none is drawn ahead of it: every glyph
    /// read and drawn uncached at a row's 14 and a lead's 16 pt, at 1x and 2x, printed for
    /// `docs/MEASUREMENTS.md` ("Tabler glyphs drawn on demand"). A screen of chrome draws a few
    /// dozen.
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
                let drawn = rasterize_on_grid(&drawing.outline, drawing.grid, side, drawing.ink);
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
