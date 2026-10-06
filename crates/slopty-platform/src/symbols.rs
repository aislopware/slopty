//! The chrome's icons: SF Symbols, drawn by the OS into alpha masks at exact device pixels.
//!
//! A [`Symbol`] is one of a closed list of SF Symbols names, the set the chrome draws
//! (`docs/decisions/ui.md`, "The chrome's icons are SF Symbols"). [`rasterize`] draws one at a
//! [`SymbolSize`] (the point size, weight and scale of the text beside it) and a display's
//! scale into a [`SymbolMask`]: one alpha byte per device pixel, with the symbol's alignment
//! rectangle and baseline, which the caller places it by. The OS draws it, so a symbol sharpens
//! and greys with the system font beside it.
//!
//! A first raster costs about half a millisecond (`docs/MEASUREMENTS.md`, "SF Symbols as
//! masks"), too much to spend on a first frame, so [`Masks`] keeps every mask drawn and
//! [`Masks::prewarm`] draws a list of them on a background thread at launch.
//!
//! Drawing happens on any thread. Each raster makes its own image and its own bitmap context
//! and makes them current only on the calling thread, which is what AppKit and UIKit ask of a
//! drawing off the main thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::RwLock;

/// Declares [`Symbol`] and its SF Symbols names in one list.
macro_rules! symbols {
    ($($(#[$meta:meta])* $variant:ident => $name:literal,)+) => {
        /// One SF Symbol the chrome draws, by its name in the system's catalogue.
        ///
        /// The list is closed: a symbol the chrome draws is added here, and the presence test
        /// checks every one resolves on the running OS.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Symbol {
            $(#[doc = concat!("`", $name, "`.")] $(#[$meta])* $variant,)+
        }

        impl Symbol {
            /// Every symbol, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The symbol's name in the SF Symbols catalogue.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }

            /// The symbol named `name` in the SF Symbols catalogue, if the list has it.
            #[must_use]
            pub fn named(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

symbols! {
    ArrowClockwise => "arrow.clockwise",
    ArrowDown => "arrow.down",
    ArrowDownToLine => "arrow.down.to.line",
    ArrowRight => "arrow.right",
    ArrowTriangleBranch => "arrow.triangle.branch",
    ArrowTriangleMerge => "arrow.triangle.merge",
    ArrowTrianglePull => "arrow.triangle.pull",
    ArrowUp => "arrow.up",
    ArrowUpAndDown => "arrow.up.and.down",
    ArrowUpLeftAndArrowDownRight => "arrow.up.left.and.arrow.down.right",
    ArrowUpRight => "arrow.up.right",
    ArrowUpToLine => "arrow.up.to.line",
    ArrowUturnBackward => "arrow.uturn.backward",
    Bell => "bell",
    BellBadge => "bell.badge",
    CharacterCursorIbeam => "character.cursor.ibeam",
    Checklist => "checklist",
    Checkmark => "checkmark",
    CheckmarkCircle => "checkmark.circle",
    CheckmarkCircleFill => "checkmark.circle.fill",
    ChevronDown => "chevron.down",
    ChevronLeft => "chevron.left",
    /// A file of code.
    ChevronLeftForwardslashChevronRight => "chevron.left.forwardslash.chevron.right",
    ChevronRight => "chevron.right",
    ChevronUp => "chevron.up",
    Circle => "circle",
    CircleDashed => "circle.dashed",
    CircleInsetFilled => "circle.inset.filled",
    Clock => "clock",
    ClockArrowCirclepath => "clock.arrow.circlepath",
    /// The Command key, where a state is about the keyboard's shortcuts.
    Command => "command",
    /// A file of data: JSON, TOML, YAML.
    Curlybraces => "curlybraces",
    Cursorarrow => "cursorarrow",
    Display => "display",
    Doc => "doc",
    DocBadgePlus => "doc.badge.plus",
    DocOnClipboard => "doc.on.clipboard",
    DocOnDoc => "doc.on.doc",
    /// A PDF.
    DocRichtext => "doc.richtext",
    DocText => "doc.text",
    /// An archive.
    DocZipper => "doc.zipper",
    Ellipsis => "ellipsis",
    ExclamationmarkCircleFill => "exclamationmark.circle.fill",
    ExclamationmarkTriangle => "exclamationmark.triangle",
    ExclamationmarkTriangleFill => "exclamationmark.triangle.fill",
    Eye => "eye",
    /// A video.
    Film => "film",
    Flag => "flag",
    Folder => "folder",
    FolderBadgePlus => "folder.badge.plus",
    Gearshape => "gearshape",
    Globe => "globe",
    InfoCircle => "info.circle",
    Iphone => "iphone",
    /// A laptop worker.
    Laptopcomputer => "laptopcomputer",
    Line3HorizontalDecrease => "line.3.horizontal.decrease",
    Link => "link",
    Lock => "lock",
    /// A lock file.
    LockDoc => "lock.doc",
    Macwindow => "macwindow",
    Magnifyingglass => "magnifyingglass",
    Minus => "minus",
    NoteText => "note.text",
    Paperclip => "paperclip",
    PauseCircle => "pause.circle",
    Pencil => "pencil",
    Photo => "photo",
    Plus => "plus",
    PlusForwardslashMinus => "plus.forwardslash.minus",
    Power => "power",
    PuzzlepieceExtension => "puzzlepiece.extension",
    QuestionmarkBubble => "questionmark.bubble",
    RectangleSplit3x1 => "rectangle.split.3x1",
    RectangleStack => "rectangle.stack",
    Scissors => "scissors",
    ServerRack => "server.rack",
    SidebarLeft => "sidebar.left",
    SpeakerSlash => "speaker.slash",
    SpeakerWave2 => "speaker.wave.2",
    /// Something new to write: a new agent.
    SquareAndPencil => "square.and.pencil",
    SquareGrid2x2 => "square.grid.2x2",
    StopFill => "stop.fill",
    Terminal => "terminal",
    /// A conversation: an agent's thread, and the toggle that shows one.
    TextBubble => "text.bubble",
    TextMagnifyingglass => "text.magnifyingglass",
    Trash => "trash",
    /// An audio file.
    Waveform => "waveform",
    WifiSlash => "wifi.slash",
    WrenchAndScrewdriver => "wrench.and.screwdriver",
    Xmark => "xmark",
    XmarkCircle => "xmark.circle",
    XmarkCircleFill => "xmark.circle.fill",
}

/// A symbol's stroke weight, matched to the text beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    /// Beside large light headings: empty states and the first run.
    Light,
    /// Beside body text, the chrome's default.
    Regular,
    /// Beside a focused or selected title.
    Medium,
    /// Disclosure chevrons and the composer's send disc, Apple's weight for both.
    Semibold,
}

impl Weight {
    const ALL: [Self; 4] = [Self::Light, Self::Regular, Self::Medium, Self::Semibold];

    const fn word(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Regular => "regular",
            Self::Medium => "medium",
            Self::Semibold => "semibold",
        }
    }
}

/// A symbol's size relative to its point size, as SF Symbols defines it: the same stroke at
/// a smaller or larger drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scale {
    /// Disclosure chevrons, beside captions.
    Small,
    /// Beside text, the default.
    Medium,
    /// Icon-only buttons and empty states.
    Large,
}

impl Scale {
    const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    const fn word(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}

/// How a symbol is drawn: the point size and weight of the text beside it, and its scale.
#[derive(Clone, Copy, Debug)]
pub struct SymbolSize {
    /// The point size of the text beside it.
    pub point: f32,
    /// The weight of the text beside it.
    pub weight: Weight,
    /// Its drawing relative to the point size.
    pub scale: Scale,
}

impl SymbolSize {
    /// A symbol at `point` and `weight`, at the medium scale.
    #[must_use]
    pub const fn new(point: f32, weight: Weight) -> Self {
        Self { point, weight, scale: Scale::Medium }
    }

    /// The same size at another scale.
    #[must_use]
    pub const fn scaled(self, scale: Scale) -> Self {
        Self { scale, ..self }
    }
}

impl PartialEq for SymbolSize {
    fn eq(&self, other: &Self) -> bool {
        self.point.to_bits() == other.point.to_bits()
            && self.weight == other.weight
            && self.scale == other.scale
    }
}

impl Eq for SymbolSize {}

impl std::hash::Hash for SymbolSize {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.point.to_bits().hash(state);
        self.weight.hash(state);
        self.scale.hash(state);
    }
}

/// A rectangle in a mask's device pixels, from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskRect {
    /// The left edge.
    pub x: f32,
    /// The top edge.
    pub y: f32,
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
}

/// One symbol drawn at one size and display scale.
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolMask {
    /// The width in device pixels.
    pub width: u32,
    /// The height in device pixels.
    pub height: u32,
    /// The coverage of each device pixel, a byte each, `width` to a row, the top row first.
    pub alpha: Vec<u8>,
    /// The symbol's alignment rectangle: the box's width, and from the baseline up to the cap
    /// height of the text it is sized to. A slot centres on it, as the text beside it does,
    /// and not on the box, whose padding is uneven.
    pub alignment: MaskRect,
    /// How far below the top the symbol's baseline lies, in device pixels; an inline symbol
    /// puts it on the text's baseline.
    pub baseline: f32,
}

/// Draws `symbol` at `size` for a display of `device_scale` device pixels to the point.
///
/// `None` when the running OS has no such symbol, or when it draws to nothing at this size.
#[must_use]
pub fn rasterize(symbol: Symbol, size: SymbolSize, device_scale: f32) -> Option<SymbolMask> {
    if !(size.point.is_finite()
        && size.point > 0.0
        && device_scale.is_finite()
        && device_scale > 0.0)
    {
        return None;
    }
    // The image, its configured copy and the graphics context are autoreleased. A pool around
    // each raster frees them as it ends; without one they pile up until the thread ends, and a
    // prewarm left its peak in the footprint for the life of the app (`docs/MEASUREMENTS.md`,
    // "the footprint at rest").
    objc2::rc::autoreleasepool(|_| raster::draw(symbol.name(), size, f64::from(device_scale)))
}

/// Whether the running OS has `symbol`.
#[must_use]
pub fn exists(symbol: Symbol) -> bool {
    objc2::rc::autoreleasepool(|_| raster::exists(symbol.name()))
}

/// The masks drawn so far, shared by every view of one app and the prewarm thread.
///
/// A symbol the OS lacks is kept as a miss, so it is not asked for again on every frame.
#[derive(Clone)]
pub struct Masks {
    drawn: Arc<RwLock<HashMap<Key, Option<Arc<SymbolMask>>>>>,
    /// What painters asked for, each once, until [`Masks::remember`] writes it down: what a
    /// launch draws, for the next launch's prewarm.
    asked: Arc<RwLock<Option<Vec<Key>>>>,
}

impl Default for Masks {
    fn default() -> Self {
        Self { drawn: Arc::default(), asked: Arc::new(RwLock::new(Some(Vec::new()))) }
    }
}

type Key = (Symbol, SymbolSize, u32);

/// One mask a prewarm draws: a symbol, its size, and the display scale.
pub type Wanted = (Symbol, SymbolSize, f32);

impl std::fmt::Debug for Masks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Masks")
            .field("drawn", &self.drawn.read().len())
            .field("asked", &self.asked.read().as_ref().map(Vec::len))
            .finish()
    }
}

impl Masks {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The mask of `symbol` at `size` and `device_scale`, drawn now unless it already was.
    #[must_use]
    pub fn get(
        &self,
        symbol: Symbol,
        size: SymbolSize,
        device_scale: f32,
    ) -> Option<Arc<SymbolMask>> {
        let key = (symbol, size, device_scale.to_bits());
        let new = self.asked.read().as_ref().is_some_and(|asked| !asked.contains(&key));
        if new
            && let Some(asked) = self.asked.write().as_mut()
            && !asked.contains(&key)
            && asked.len() < REMEMBERED
        {
            asked.push(key);
        }
        self.draw(key)
    }

    /// The mask for `key`, drawn now unless it already was.
    fn draw(&self, key: Key) -> Option<Arc<SymbolMask>> {
        if let Some(kept) = self.drawn.read().get(&key) {
            return kept.clone();
        }
        let (symbol, size, device_scale) = (key.0, key.1, f32::from_bits(key.2));
        let drawn = rasterize(symbol, size, device_scale).map(Arc::new);
        // Two threads may draw the same mask at once; both draws are the same bytes, and the
        // first kept is the one every later caller shares.
        self.drawn.write().entry(key).or_insert(drawn).clone()
    }

    /// The mask kept for `symbol` at `size` and `device_scale`, without drawing it.
    #[must_use]
    pub fn kept(
        &self,
        symbol: Symbol,
        size: SymbolSize,
        device_scale: f32,
    ) -> Option<Arc<SymbolMask>> {
        self.drawn.read().get(&(symbol, size, device_scale.to_bits())).cloned().flatten()
    }

    /// How many masks, and misses, are kept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.drawn.read().len()
    }

    /// Whether nothing is kept yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.drawn.read().is_empty()
    }

    /// Draws every mask `wanted` returns on a background thread at utility `QoS`, so the first
    /// frame finds them drawn. `wanted` runs on that thread too, so a list read from a file is
    /// read off the caller's. A mask asked for before the thread reaches it is drawn by the
    /// asker, and the thread then finds it kept. A prewarm's own drawings are not what
    /// painters asked for, so [`Masks::remember`] leaves them out.
    ///
    /// # Errors
    ///
    /// When the thread cannot be started.
    pub fn prewarm(
        &self,
        wanted: impl FnOnce() -> Vec<Wanted> + Send + 'static,
    ) -> std::io::Result<JoinHandle<()>> {
        let masks = self.clone();
        std::thread::Builder::new().name("slopty-symbols".into()).spawn(move || {
            utility_thread();
            for (symbol, size, device_scale) in wanted() {
                drop(masks.draw((symbol, size, device_scale.to_bits())));
            }
        })
    }

    /// Writes what painters have asked for so far to `path` and stops noting it: what a
    /// launch's first frames drew, for the next launch's prewarm ([`remembered`]).
    ///
    /// The list is kept by display scale. The masks of a scale drawn this launch replace that
    /// scale's lines, and another scale's lines stay, so a window back on another display
    /// warms as it last drew there. At most [`REMEMBERED`] lines are kept. The file is written
    /// beside `path` and renamed over it, so a reader never finds half of one.
    ///
    /// Each SF Symbol drawn holds its share of the OS's symbol data for the life of the
    /// process, so a prewarm of only these keeps the rest out of the footprint
    /// (`docs/MEASUREMENTS.md`, "the footprint at rest").
    ///
    /// # Errors
    ///
    /// When `path` cannot be written.
    pub fn remember(&self, path: &std::path::Path) -> std::io::Result<()> {
        let asked = self.asked.write().take().unwrap_or_default();
        let scales: Vec<u32> = asked.iter().map(|&(_, _, scale)| scale).collect();
        let kept =
            read(path).into_iter().filter(|(_, _, scale)| !scales.contains(&scale.to_bits()));
        let lines: Vec<Wanted> = asked
            .into_iter()
            .map(|(symbol, size, scale)| (symbol, size, f32::from_bits(scale)))
            .chain(kept)
            .take(REMEMBERED)
            .collect();
        let mut text = String::new();
        for (symbol, size, scale) in lines {
            text.push_str(symbol.name());
            for word in
                [&size.point.to_string(), size.weight.word(), size.scale.word(), &scale.to_string()]
            {
                text.push(' ');
                text.push_str(word);
            }
            text.push('\n');
        }
        let mut written = path.as_os_str().to_owned();
        written.push(".new");
        std::fs::write(&written, text)?;
        std::fs::rename(&written, path)
    }
}

/// The most masks [`Masks::remember`] keeps: the chrome at a handful of sizes on two
/// displays, with room to spare.
pub const REMEMBERED: usize = 512;

/// The masks to prewarm for a window on a display of `scale`, from what [`Masks::remember`]
/// wrote to `path`.
///
/// That scale's lines, or, when it has none, the symbols another scale drew, at this one.
/// With `None` for a scale not known yet, every line as written. Empty when there is no such
/// file or nothing in it reads.
#[must_use]
pub fn remembered(path: &std::path::Path, scale: Option<f32>) -> Vec<Wanted> {
    let lines = read(path);
    let Some(scale) = scale else {
        return lines;
    };
    let mut here: Vec<Wanted> =
        lines.iter().copied().filter(|&(_, _, at)| at.to_bits() == scale.to_bits()).collect();
    if here.is_empty() {
        for (symbol, size, _) in lines {
            if !here.iter().any(|&(s, z, _)| s == symbol && z == size) {
                here.push((symbol, size, scale));
            }
        }
    }
    here
}

/// The lines of a list [`Masks::remember`] wrote, at most [`REMEMBERED`] of them. A line that
/// does not read (a symbol since renamed, a file cut short) is passed over.
fn read(path: &std::path::Path) -> Vec<Wanted> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut words = line.split(' ');
            let symbol = Symbol::named(words.next()?)?;
            let point = words.next()?.parse::<f32>().ok()?;
            let weight = words.next()?;
            let weight = Weight::ALL.into_iter().find(|w| w.word() == weight)?;
            let scale = words.next()?;
            let scale = Scale::ALL.into_iter().find(|s| s.word() == scale)?;
            let device = words.next()?.parse::<f32>().ok()?;
            let fine = point.is_finite() && point > 0.0 && device.is_finite() && device > 0.0;
            (fine && words.next().is_none()).then_some((
                symbol,
                SymbolSize { point, weight, scale },
                device,
            ))
        })
        .take(REMEMBERED)
        .collect()
}

/// The display scale of the screen a new window opens on: the main screen on macOS, the
/// first window scene's screen on iOS. Main thread only; `None` off it, and before a screen
/// or scene exists.
#[must_use]
pub fn main_display_scale() -> Option<f32> {
    let mtm = objc2::MainThreadMarker::new()?;
    #[cfg(target_os = "macos")]
    let scale = objc2_app_kit::NSScreen::mainScreen(mtm)?.backingScaleFactor();
    #[cfg(target_os = "ios")]
    let scale = {
        let scenes = objc2_ui_kit::UIApplication::sharedApplication(mtm).connectedScenes();
        let scene = scenes.iter().find_map(|s| s.downcast::<objc2_ui_kit::UIWindowScene>().ok())?;
        scene.screen().scale()
    };
    #[expect(clippy::cast_possible_truncation, reason = "a display's scale is 1, 2 or 3")]
    Some(scale as f32)
}

/// Puts the calling thread at utility `QoS`: the prewarm yields to the frame and to input.
fn utility_thread() {
    // SAFETY: `pthread_set_qos_class_self_np` (pthread/qos.h) changes only the calling thread's
    // own class, and a relative priority of 0 is within every class's range
    // (`QOS_MIN_RELATIVE_PRIORITY` is -15).
    let refused =
        unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
    if refused != 0 {
        tracing::warn!(error = refused, "utility QoS refused");
    }
}

/// The OS's drawing into a bitmap of our own, and the alpha taken out of it.
mod raster {
    use std::ffi::c_void;

    use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo};

    use super::{MaskRect, SymbolMask};

    /// A drawing's box in points and its alignment rectangle's insets and baseline, in points
    /// from the box's top-left corner.
    struct Layout {
        size: CGSize,
        alignment: MaskRect,
        baseline: f32,
    }

    /// The bitmap a drawing lands in, `width` by `height` device pixels, and its alpha bytes.
    ///
    /// `draw` gets a context whose user space is points with the origin at the bottom left,
    /// AppKit's and Core Graphics' own; the box's top edge is the bitmap's top row.
    fn bitmap(
        layout: &Layout,
        scale: f64,
        draw: impl FnOnce(&CGContext, CGRect),
    ) -> Option<SymbolMask> {
        let width = device(layout.size.width, scale)?;
        let height = device(layout.size.height, scale)?;
        let row = width.checked_mul(4)?;
        let mut pixels = vec![0_u8; row.checked_mul(height)?];
        let space = CGColorSpace::new_device_rgb()?;
        // An alpha-only bitmap would be a quarter of this, but AppKit draws nothing into one
        // (it finds no image representation for a context without a colour space), so the
        // symbol lands in premultiplied RGBA and its alpha is taken out after.
        //
        // SAFETY: `CGBitmapContextCreate` (CGBitmapContext.h) draws into `data` for the
        // context's life: `pixels` holds `bytes_per_row × height` bytes and outlives the
        // context, which is dropped before `pixels` is read. 8-bit RGBA with premultiplied alpha
        // last is one of its supported formats.
        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast::<c_void>(),
                width,
                height,
                8,
                row,
                Some(&space),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }?;
        CGContext::scale_ctm(Some(&context), scale, scale);
        // The bitmap is whole device pixels and the box rarely is, so the box hangs from the
        // top row and the spare fraction of a pixel is at the bottom.
        #[expect(clippy::cast_precision_loss, reason = "a mask is a few hundred pixels high")]
        let top = height as CGFloat / scale;
        let rect = CGRect::new(CGPoint::new(0.0, top - layout.size.height), layout.size);
        draw(&context, rect);
        drop(context);
        let alpha = pixels.as_chunks::<4>().0.iter().map(|&[_, _, _, alpha]| alpha).collect();
        #[expect(clippy::cast_possible_truncation, reason = "a display's scale is 1, 2 or 3")]
        let ratio = scale as f32;
        Some(SymbolMask {
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            alpha,
            alignment: MaskRect {
                x: layout.alignment.x * ratio,
                y: layout.alignment.y * ratio,
                width: layout.alignment.width * ratio,
                height: layout.alignment.height * ratio,
            },
            baseline: layout.baseline * ratio,
        })
    }

    /// `points` at `scale`, rounded up to whole device pixels; `None` for nothing.
    fn device(points: CGFloat, scale: f64) -> Option<usize> {
        let pixels = (points * scale).ceil();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite and positive, and a symbol is far below `usize::MAX` pixels"
        )]
        (pixels.is_finite() && pixels >= 1.0).then_some(pixels as usize)
    }

    #[cfg(target_os = "ios")]
    pub(super) use ios::{draw, exists};
    #[cfg(target_os = "macos")]
    pub(super) use mac::{draw, exists};

    #[cfg(target_os = "macos")]
    mod mac {
        use objc2::rc::Retained;
        use objc2_app_kit::{
            NSFontWeight, NSFontWeightLight, NSFontWeightMedium, NSFontWeightRegular,
            NSFontWeightSemibold, NSGraphicsContext, NSImage, NSImageSymbolConfiguration,
            NSImageSymbolScale,
        };
        use objc2_core_foundation::CGFloat;
        use objc2_foundation::NSString;

        use super::{Layout, bitmap};
        use crate::symbols::{MaskRect, Scale, SymbolMask, SymbolSize, Weight};

        fn image(name: &str) -> Option<Retained<NSImage>> {
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(name),
                None,
            )
        }

        pub(in super::super) fn exists(name: &str) -> bool {
            image(name).is_some()
        }

        fn weight(weight: Weight) -> NSFontWeight {
            // SAFETY: the `NSFontWeight*` constants (NSFontDescriptor.h) are immutable values
            // AppKit sets before any code of ours runs.
            unsafe {
                match weight {
                    Weight::Light => NSFontWeightLight,
                    Weight::Regular => NSFontWeightRegular,
                    Weight::Medium => NSFontWeightMedium,
                    Weight::Semibold => NSFontWeightSemibold,
                }
            }
        }

        const fn scale(scale: Scale) -> NSImageSymbolScale {
            match scale {
                Scale::Small => NSImageSymbolScale::Small,
                Scale::Medium => NSImageSymbolScale::Medium,
                Scale::Large => NSImageSymbolScale::Large,
            }
        }

        pub(in super::super) fn draw(
            name: &str,
            size: SymbolSize,
            device: f64,
        ) -> Option<SymbolMask> {
            let configuration = NSImageSymbolConfiguration::configurationWithPointSize_weight_scale(
                CGFloat::from(size.point),
                weight(size.weight),
                scale(size.scale),
            );
            let symbol = image(name)?.imageWithSymbolConfiguration(&configuration)?;
            let box_size = symbol.size();
            // An `NSImage`'s alignment rectangle is in its own points from the bottom left; a
            // symbol's bottom edge is its baseline (SF Symbols sit on the text's baseline, and
            // AppKit sets the rectangle from the baseline to the cap height).
            let aligned = symbol.alignmentRect();
            #[expect(clippy::cast_possible_truncation, reason = "a symbol's points fit an f32")]
            let layout = Layout {
                size: box_size,
                alignment: MaskRect {
                    x: aligned.origin.x as f32,
                    y: (box_size.height - aligned.origin.y - aligned.size.height) as f32,
                    width: aligned.size.width as f32,
                    height: aligned.size.height as f32,
                },
                baseline: (box_size.height - aligned.origin.y) as f32,
            };
            bitmap(&layout, device, |context, rect| {
                let graphics =
                    NSGraphicsContext::graphicsContextWithCGContext_flipped(context, false);
                // The current context is the calling thread's own (NSGraphicsContext.h), so a
                // raster off the main thread touches no other thread's drawing; the one it
                // replaces is put back.
                let before = NSGraphicsContext::currentContext();
                NSGraphicsContext::setCurrentContext(Some(&graphics));
                symbol.drawInRect(rect);
                NSGraphicsContext::setCurrentContext(before.as_deref());
            })
        }
    }

    #[cfg(target_os = "ios")]
    mod ios {
        use objc2::rc::Retained;
        use objc2_core_foundation::{CGFloat, CGPoint, CGRect};
        use objc2_core_graphics::CGContext;
        use objc2_foundation::NSString;
        use objc2_ui_kit::{
            UIGraphicsPopContext, UIGraphicsPushContext, UIImage, UIImageSymbolConfiguration,
            UIImageSymbolScale, UIImageSymbolWeight,
        };

        use super::{Layout, bitmap};
        use crate::symbols::{MaskRect, Scale, SymbolMask, SymbolSize, Weight};

        fn image(name: &str, size: Option<SymbolSize>) -> Option<Retained<UIImage>> {
            let name = NSString::from_str(name);
            match size {
                None => UIImage::systemImageNamed(&name),
                Some(size) => {
                    let configuration =
                        UIImageSymbolConfiguration::configurationWithPointSize_weight_scale(
                            CGFloat::from(size.point),
                            weight(size.weight),
                            scale(size.scale),
                        );
                    UIImage::systemImageNamed_withConfiguration(&name, Some(&configuration))
                }
            }
        }

        pub(in super::super) fn exists(name: &str) -> bool {
            image(name, None).is_some()
        }

        const fn weight(weight: Weight) -> UIImageSymbolWeight {
            match weight {
                Weight::Light => UIImageSymbolWeight::Light,
                Weight::Regular => UIImageSymbolWeight::Regular,
                Weight::Medium => UIImageSymbolWeight::Medium,
                Weight::Semibold => UIImageSymbolWeight::Semibold,
            }
        }

        const fn scale(scale: Scale) -> UIImageSymbolScale {
            match scale {
                Scale::Small => UIImageSymbolScale::Small,
                Scale::Medium => UIImageSymbolScale::Medium,
                Scale::Large => UIImageSymbolScale::Large,
            }
        }

        pub(in super::super) fn draw(
            name: &str,
            size: SymbolSize,
            device: f64,
        ) -> Option<SymbolMask> {
            let symbol = image(name, Some(size))?;
            // SAFETY: a `UIImage` is immutable and safe to read from any thread (UIImage.h,
            // "Image objects are immutable"), and this one is ours alone; this and the three
            // reads below are plain property reads.
            let box_size = unsafe { symbol.size() };
            // SAFETY: as above.
            let insets = unsafe { symbol.alignmentRectInsets() };
            // SAFETY: as above.
            let baseline = if unsafe { symbol.hasBaseline() } {
                // SAFETY: as above.
                unsafe { symbol.baselineOffsetFromBottom() }
            } else {
                insets.bottom
            };
            #[expect(clippy::cast_possible_truncation, reason = "a symbol's points fit an f32")]
            let layout = Layout {
                size: box_size,
                alignment: MaskRect {
                    x: insets.left as f32,
                    y: insets.top as f32,
                    width: (box_size.width - insets.left - insets.right) as f32,
                    height: (box_size.height - insets.top - insets.bottom) as f32,
                },
                baseline: (box_size.height - baseline) as f32,
            };
            bitmap(&layout, device, |context, rect| {
                // UIKit draws from the top left: put the origin on the box's top edge and turn
                // the vertical axis down.
                CGContext::translate_ctm(Some(context), 0.0, rect.origin.y + rect.size.height);
                CGContext::scale_ctm(Some(context), 1.0, -1.0);
                // SAFETY: `UIGraphicsPushContext` (UIGraphics.h) makes a live context current
                // for the calling thread only, which UIKit allows from any thread since iOS 4,
                // and `UIGraphicsPopContext` takes it off that thread's stack again before the
                // context is dropped.
                unsafe {
                    UIGraphicsPushContext(context);
                }
                symbol.drawInRect(CGRect::new(CGPoint::ZERO, rect.size));
                // SAFETY: as above; the context pushed on this thread is the one popped.
                unsafe {
                    UIGraphicsPopContext();
                }
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::device;

        #[test]
        fn a_box_takes_whole_device_pixels() {
            assert_eq!(device(15.3, 1.0), Some(16));
            assert_eq!(device(15.3, 2.0), Some(31));
            assert_eq!(device(16.0, 2.0), Some(32));
            assert_eq!(device(0.0, 2.0), None);
            assert_eq!(device(f64::NAN, 2.0), None);
        }
    }
}
