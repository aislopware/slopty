//! The chrome's glyphs: GitHub's Octicons (`assets/icons/octicons/`, MIT, credited in its
//! `NOTICE`), one family on every platform, filled outlines on a 16 grid whose 1.5-unit line is
//! the 1.31 px stem of the IBM Plex beside it when drawn at 14 (`docs/decisions/ui.md`, "The
//! chrome's icons are Octicons at 14"). Chevrons and a tab's close are Octicons' 12-grid art;
//! the laptop is our own, drawn on Octicons' grid (`assets/icons/ours/`).
//!
//! A glyph is drawn by us from its outline ([`slopty_platform::outline::rasterize_on_grid`]):
//! its grid scaled to a whole number of device pixels and filled. Each becomes an alpha mask
//! painted in the ink of the words beside it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui::SharedString;
use parking_lot::RwLock;
use slopty_platform::outline::{Outline, rasterize_on_grid};

use super::Kept;

/// Declares [`Symbol`] and the file that draws each in one list.
macro_rules! glyphs {
    ($($variant:ident => $file:literal,)+) => {
        /// One glyph the chrome draws, by the name the chrome knows it under; its file is
        /// [`Symbol::name`], and the `NOTICE` beside the files lists the pairs.
        ///
        /// The list is closed: a glyph the chrome draws is added here with its file, and
        /// `every_glyph_reads` checks each one reads and every file is drawn.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Symbol {
            $(#[doc = concat!("`", $file, ".svg`.")] $variant,)+
        }

        impl Symbol {
            /// Every glyph, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// Its file's path under `assets/icons/`, which names its atlas key.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $file,)+
                }
            }

            /// Whether its art is Octicons' 12 grid (a chevron, the small cross), drawn at its
            /// own 12 rather than the 16 grid's 14.
            #[must_use]
            pub fn small_art(self) -> bool {
                self.name().ends_with("-12")
            }

            /// Its file's markup.
            const fn svg(self) -> &'static str {
                match self {
                    $(Self::$variant => include_str!(
                        concat!("../../assets/icons/", $file, ".svg")
                    ),)+
                }
            }
        }
    };
}

glyphs! {
    AiAgent => "octicons/ai-model-16",
    ArrowClockwise => "octicons/sync-16",
    ArrowDown => "octicons/arrow-down-16",
    ArrowDownToLine => "octicons/move-to-bottom-16",
    ArrowLeft => "octicons/arrow-left-16",
    ArrowRight => "octicons/arrow-right-16",
    ArrowTriangleBranch => "octicons/git-branch-16",
    ArrowTriangleMerge => "octicons/git-merge-16",
    ArrowTrianglePull => "octicons/git-pull-request-16",
    ArrowUp => "octicons/arrow-up-16",
    ArrowUpAndDown => "octicons/arrow-both-16",
    ArrowUpRight => "octicons/link-external-16",
    ArrowUpToLine => "octicons/move-to-top-16",
    ArrowUturnBackward => "octicons/undo-16",
    Bell => "octicons/bell-16",
    BellSlash => "octicons/bell-slash-16",
    CharacterCursorIbeam => "octicons/typography-16",
    Checklist => "octicons/checklist-16",
    Checkmark => "octicons/check-16",
    CheckmarkCircle => "octicons/check-circle-16",
    CheckmarkCircleFill => "octicons/check-circle-fill-16",
    ChevronDown => "octicons/chevron-down-12",
    ChevronLeft => "octicons/chevron-left-12",
    ChevronLeftForwardslashChevronRight => "octicons/code-16",
    ChevronRight => "octicons/chevron-right-12",
    ChevronUp => "octicons/chevron-up-12",
    CircleInsetFilled => "octicons/dot-fill-16",
    Clock => "octicons/clock-16",
    Command => "octicons/command-palette-16",
    Cursorarrow => "octicons/cursor-16",
    Display => "octicons/device-desktop-16",
    Doc => "octicons/file-16",
    DocBadgePlus => "octicons/file-added-16",
    DocOnClipboard => "octicons/paste-16",
    DocOnDoc => "octicons/copy-16",
    Ellipsis => "octicons/kebab-horizontal-16",
    ExclamationmarkTriangle => "octicons/alert-16",
    ExclamationmarkTriangleFill => "octicons/alert-fill-16",
    Eye => "octicons/eye-16",
    EyeSlash => "octicons/eye-closed-16",
    FileBinary => "octicons/file-binary-16",
    FileCode => "octicons/file-code-16",
    FileMedia => "octicons/file-media-16",
    FileZip => "octicons/file-zip-16",
    Flag => "octicons/goal-16",
    Fold => "octicons/fold-16",
    Folder => "octicons/file-directory-16",
    Gearshape => "octicons/gear-16",
    GitCommit => "octicons/git-commit-16",
    GitRepo => "octicons/repo-16",
    Globe => "octicons/globe-16",
    InfoCircle => "octicons/info-16",
    Iphone => "octicons/device-mobile-16",
    Laptopcomputer => "ours/laptop-16",
    Link => "octicons/link-16",
    Lock => "octicons/lock-16",
    Macwindow => "octicons/browser-16",
    Magnifyingglass => "octicons/search-16",
    Markdown => "octicons/markdown-16",
    Minus => "octicons/dash-16",
    Paperclip => "octicons/paperclip-16",
    PauseCircle => "octicons/pause-16",
    Pencil => "octicons/pencil-16",
    Person => "octicons/person-16",
    Photo => "octicons/image-16",
    Pin => "octicons/pin-16",
    Plus => "octicons/plus-16",
    PlusForwardslashMinus => "octicons/diff-16",
    PullClosed => "octicons/git-pull-request-closed-16",
    PullDraft => "octicons/git-pull-request-draft-16",
    PuzzlepieceExtension => "octicons/plug-16",
    QuestionmarkBubble => "octicons/question-16",
    RectangleSplit3x1 => "octicons/columns-16",
    Send => "octicons/paper-airplane-16",
    ServerRack => "octicons/server-16",
    SidebarExpand => "octicons/sidebar-expand-16",
    SidebarLeft => "octicons/sidebar-collapse-16",
    SpeakerSlash => "octicons/mute-16",
    SpeakerWave2 => "octicons/unmute-16",
    SquareGrid2x2 => "octicons/apps-16",
    Stop => "octicons/stop-16",
    StopFill => "octicons/square-fill-16",
    Terminal => "octicons/terminal-16",
    TextBubble => "octicons/comment-discussion-16",
    Trash => "octicons/trash-16",
    Unfold => "octicons/unfold-16",
    WifiSlash => "octicons/cloud-offline-16",
    WrenchAndScrewdriver => "octicons/tools-16",
    Xmark => "octicons/x-16",
    XmarkCircle => "octicons/x-circle-16",
    XmarkSmall => "octicons/x-12",
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

/// Reads a glyph's file: its view box's side and its paths, which are filled.
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

    /// The side of `symbol`'s grid in its own units: 12 for the small art, else 16.
    fn grid_of(symbol: Symbol) -> f64 {
        if symbol.small_art() { 12.0 } else { 16.0 }
    }

    /// A lint as a test: every glyph's file reads on its grid (Octicons' 16, or 12 for the
    /// small art) and draws ink there; every file kept is some glyph's, and the `NOTICE` names
    /// each glyph with its file.
    #[test]
    fn every_glyph_reads() {
        let notice = include_str!("../../assets/icons/octicons/NOTICE");
        for &symbol in Symbol::ALL {
            let drawing = read(symbol.svg()).unwrap_or_else(|e| panic!("{symbol:?}: {e}"));
            let grid = grid_of(symbol);
            assert!((drawing.grid - grid).abs() < f64::EPSILON, "{symbol:?} on the {grid} grid");
            #[expect(clippy::cast_possible_truncation, reason = "a grid of 12 or 16")]
            #[expect(clippy::cast_sign_loss, reason = "a grid is positive")]
            let side = grid as u32;
            let (m, _) = mask(symbol, side).unwrap_or_else(|| panic!("{symbol:?} is drawn"));
            assert_eq!((m.width, m.height), (side, side), "{symbol:?} is its grid's size");
            assert!(m.alpha.iter().any(|a| *a > 0), "{symbol:?} has ink");
            let file = symbol
                .name()
                .strip_prefix("octicons/")
                .map_or_else(|| format!("../{}.svg", symbol.name()), |name| format!("{name}.svg"));
            let listed = notice.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some(&format!("{symbol:?}")) && words.next() == Some(&file)
            });
            assert!(listed, "the NOTICE names {symbol:?} with {file}");
        }
        for dir in ["octicons", "ours"] {
            let path = format!("{}/assets/icons/{dir}", env!("CARGO_MANIFEST_DIR"));
            let files: Vec<String> = std::fs::read_dir(&path)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok()?.file_name().into_string().ok())
                        .filter_map(|n| n.strip_suffix(".svg").map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            assert!(!files.is_empty(), "the glyphs are kept in {path}");
            for file in files {
                let name = format!("{dir}/{file}");
                assert!(
                    Symbol::ALL.iter().any(|s| s.name() == name),
                    "{name}.svg is drawn by none"
                );
            }
        }
    }

    /// What a first frame pays to draw its glyphs, so none is drawn ahead of it: every glyph
    /// read and drawn uncached at 14 and 12 and twice that, printed for `docs/MEASUREMENTS.md`
    /// ("Octicons glyphs drawn on demand"). A screen of chrome draws a few dozen.
    #[test]
    fn a_glyph_is_cheap_to_draw_on_demand() {
        let started = std::time::Instant::now();
        let drawings: Vec<Drawing> =
            Symbol::ALL.iter().filter_map(|s| read(s.svg()).ok()).collect();
        let read_all = started.elapsed();
        let mut each = Vec::new();
        for drawing in &drawings {
            for side in [12, 14, 24, 28] {
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
