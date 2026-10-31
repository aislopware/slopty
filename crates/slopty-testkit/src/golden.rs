//! Golden frames: a picture of the interface held to the PNG it was accepted as, judged by its
//! worst window, and what it says held to its golden words.
//!
//! Two kinds of test draw these pictures: the app's self-test (`slopty-e2e`), which asks the
//! running app for its window's frame, and the shell's in-process tests, which draw the same
//! window in a GPUI test window through the same Metal renderer. Both lay the frame over
//! [`BACKDROP`] and hold it to its golden here, so a golden means the same in both.
//!
//! A pixel differs when a channel is more than [`CHANNEL_SLACK`] apart, unless it lies on an edge
//! both pictures' neighbourhoods explain (`edge_explained`): one macOS or GPU shades a glyph's
//! edge a little apart from another. A frame fails on its worst [`WINDOW`]-square window, not on
//! its total ([`WINDOW_NOISE`]): GPU dithering and anti-aliasing scatter stray pixels, which never
//! fill one small square, while a changed icon, word or swatch changes a compact area whatever
//! share of the frame it is (pixelmatch v8's `windowSize`). A straight run of [`SEAM_RUN`]
//! differing pixels fails too, however thin: a rule or border that came, went or moved. And a
//! frame fails past a share of its pixels ([`MAC_SHARE`], [`SIMULATOR_SHARE`]) however they lie: a
//! change spread too thin to fill a window, a weight or a hinting across a page's text.
//!
//! A frame with no golden fails, as an insta snapshot does, unless the run accepts ([`Accept`]),
//! which writes the goldens a frame differs from, by one pixel or one word.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use image::{Rgba, RgbaImage};

/// Per-channel difference under which two pixels count as equal.
///
/// Small, because a theme's surfaces sit close together: `canvas` and `panel` are 11 apart in
/// the light theme, and at 24 a band moved from one to the other still matched.
pub const CHANNEL_SLACK: u8 = 4;

/// The most a glyph's edge may be shaded apart between two macOS releases and still match,
/// when both pictures' neighbourhoods explain it (`edge_explained`).
///
/// CI's macOS 26 renders against this Mac's goldens (CI e2e run 37390615720): of `thread`'s
/// 1 655 pixels more than [`CHANNEL_SLACK`] apart, 1 628 were within 64 and none past 150,
/// while a changed title or path moved its pixels by up to 240. A word swapped for another
/// moves ink against ground, 150 to 255 apart, so it still counts.
pub const EDGE_SLACK: u8 = 64;

/// The most of a Mac frame's pixels that may differ, wherever they lie: four times the most two
/// runs of a Mac golden differed by (0.051 %, `browser`).
///
/// A backstop beside the windows: every golden that passed it before the windows did still does,
/// and a change spread too thin to fill a window (a weight or a hinting across a page's text)
/// still fails.
pub const MAC_SHARE: f64 = 0.002;

/// The most of a simulator frame's pixels that may differ: five times the noise between two runs.
///
/// Kept apart from [`MAC_SHARE`] until a simulator's diffs are measured against the windows,
/// which are in device pixels, two or three to the point there.
pub const SIMULATOR_SHARE: f64 = 0.0005;

/// The side of the square window a frame is judged by, in device pixels.
///
/// About a glyph's height at 1x, and an icon's quarter: small enough that a changed icon, word
/// or swatch fills one, large enough that scattered noise never does.
pub const WINDOW: u32 = 16;

/// The most differing pixels any [`WINDOW`]-square window may hold, of its 256.
///
/// Measured on the 36 diffs of the self-test's renders that matched within [`MAC_SHARE`] alone
/// (`target/remote/*/artifacts/*.diff.png`, 2026-10-31): the GPU's and the OS's noise filled at
/// most 15 of a window (a stream's edge in `surface-live-screen-takeover-dark`), and the smallest
/// real change, a line of status text in `settings-devices-invite`, 46. Past 24 were only real
/// changes and two of Slopty's own bugs: a stream's picture past its mask (39 to 174) and the
/// terminal's rail caught as it faded (48). A one-pixel rule moved by a pixel differs along both
/// its rows, 32 of a window it crosses.
pub const WINDOW_NOISE: u32 = 24;

/// The longest straight run of differing pixels, across or down, that a golden still matches
/// with.
///
/// Glyph edges another macOS shades apart differ in short broken runs, one glyph's height at
/// most, while a rule, a border or a band that came, went or moved differs along its whole
/// length: a 1-pt hairline across a terminal 1 064 pt wide was 0.1 % of a 2× frame
/// (2026-10-13).
pub const SEAM_RUN: u32 = 64;

/// What a render shows behind a window that lets its backdrop through (frosted surfaces): a
/// fixed slate, where on a Mac the desktop would be, so a frosted frame is the same on every
/// machine.
pub const BACKDROP: [u8; 3] = [72, 96, 128];

/// Composite `image`, whose colours are premultiplied by their alpha as GPUI draws them, over
/// [`BACKDROP`]: every pixel comes out opaque, and an opaque one unchanged.
pub fn over_backdrop(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let clear = 255_u16.saturating_sub(u16::from(pixel[3]));
        for (channel, backdrop) in pixel.0.iter_mut().zip(BACKDROP) {
            let under = clear.saturating_mul(u16::from(backdrop)).saturating_add(127) / 255;
            *channel = u8::try_from(u16::from(*channel).saturating_add(under)).unwrap_or(u8::MAX);
        }
        pixel[3] = u8::MAX;
    }
}

/// A node of a frame's accessibility tree, as its words say it.
#[derive(Clone, Copy, Debug)]
pub struct Spoken<'a> {
    /// The accesskit role as `Debug` prints it: `Button`, `Terminal`, `Heading`, …
    pub role: &'a str,
    /// The label, if any.
    pub label: Option<&'a str>,
    /// The value, if any (a terminal's cursor row, a text field's text).
    pub value: Option<&'a str>,
    /// The node holds the keyboard focus.
    pub focused: bool,
}

/// What a frame says in words: a line per node, in reading order.
///
/// Each line is the role, the label and the value, and a mark on the node with the keyboard.
/// Where a node sits is the picture's business, so no bounds: the words change only when a word
/// does.
#[must_use]
pub fn words<'a>(nodes: impl IntoIterator<Item = Spoken<'a>>) -> String {
    let mut text = String::new();
    for node in nodes {
        text.push_str(node.role);
        for part in [node.label, node.value].into_iter().flatten().filter(|p| !p.is_empty()) {
            text.push_str(" \u{2502} ");
            text.push_str(&part.replace('\n', "\u{23ce}"));
        }
        if node.focused {
            text.push_str(" (focused)");
        }
        text.push('\n');
    }
    text
}

/// The outcome of a comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Diff {
    /// Pixels that differ beyond the slack.
    pub differing: u64,
    /// Pixels compared.
    pub total: u64,
    /// The longest straight run of differing pixels, across or down ([`SEAM_RUN`]).
    pub longest_run: u32,
    /// The most differing pixels any [`WINDOW`]-square window holds ([`WINDOW_NOISE`]).
    pub worst_window: u32,
    /// That window's top left corner, in device pixels.
    pub worst_at: (u32, u32),
}

impl Diff {
    /// Differing pixels as a fraction of the whole.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            #[expect(clippy::cast_precision_loss, reason = "pixel counts fit f64 exactly")]
            let f = self.differing as f64 / self.total as f64;
            f
        }
    }

    /// Whether the frame matches: no window holds more than noise could, no rule or border
    /// differs along its length, and no more than `share` of its pixels differ.
    #[must_use]
    pub fn passes(self, share: f64) -> bool {
        self.worst_window <= WINDOW_NOISE && self.longest_run < SEAM_RUN && self.fraction() <= share
    }

    /// The comparison in words, for a test's log.
    #[must_use]
    pub fn describe(self) -> String {
        let (x, y) = self.worst_at;
        format!(
            "{} of {} pixels differ ({:.3}%); the worst {WINDOW}×{WINDOW} window, at ({x}, {y}), \
             holds {} (noise holds at most {WINDOW_NOISE}); the longest run is {} (a seam from \
             {SEAM_RUN})",
            self.differing,
            self.total,
            self.fraction() * 100.0,
            self.worst_window,
            self.longest_run,
        )
    }
}

/// A rectangle of a frame in device pixels: left, top, width, height.
pub type PixelRect = [u32; 4];

/// Whether `rect` holds the pixel at `(x, y)`.
#[must_use]
pub const fn holds(rect: &PixelRect, x: u32, y: u32) -> bool {
    let [left, top, width, height] = *rect;
    x >= left && y >= top && x.saturating_sub(left) < width && y.saturating_sub(top) < height
}

/// The colour the diff paints a differing pixel.
pub const DIFFERING: Rgba<u8> = Rgba([220, 30, 30, 255]);

/// Pixel-wise comparison; `diff` gets every differing pixel painted [`DIFFERING`] on a faded copy
/// of `actual`.
#[must_use]
pub fn compare(actual: &RgbaImage, golden: &RgbaImage) -> (Diff, RgbaImage) {
    compare_masked(actual, golden, &[])
}

/// [`compare`] outside `masks`.
///
/// A masked pixel is neither counted nor compared, and the diff paints it blue. What a mask covers
/// is a moving picture, whose pixels no two runs share ([`luma_distance`] compares those).
#[must_use]
pub fn compare_masked(
    actual: &RgbaImage,
    golden: &RgbaImage,
    masks: &[PixelRect],
) -> (Diff, RgbaImage) {
    let (w, h) = actual.dimensions();
    let mut diff = RgbaImage::new(w, h);
    let pixels = u64::from(w).saturating_mul(u64::from(h));
    let mut differs = vec![false; usize::try_from(pixels).unwrap_or(0)];
    let (mut differing, mut total) = (0_u64, 0_u64);
    // The run ending at the pixel before, across, and above, down each column.
    let (mut across, mut down) = (0_u32, vec![0_u32; usize::try_from(w).unwrap_or(0)]);
    let mut longest_run = 0_u32;
    for ((x, y, pixel), differs) in actual.enumerate_pixels().zip(differs.iter_mut()) {
        if x == 0 {
            across = 0;
        }
        let column = down.get_mut(usize::try_from(x).unwrap_or(usize::MAX));
        if masks.iter().any(|m| holds(m, x, y)) {
            diff.put_pixel(x, y, Rgba([150, 180, 230, 255]));
            across = 0;
            if let Some(column) = column {
                *column = 0;
            }
            continue;
        }
        total = total.saturating_add(1);
        let fade = |c: u8| (c / 3).saturating_add(170);
        let faded = Rgba([fade(pixel[0]), fade(pixel[1]), fade(pixel[2]), 255]);
        let same = golden.get_pixel_checked(x, y).is_some_and(|g| {
            let apart = pixel.0.iter().zip(g.0.iter()).map(|(a, b)| a.abs_diff(*b)).max();
            let apart = apart.unwrap_or(0);
            apart <= CHANNEL_SLACK
                || (apart <= EDGE_SLACK
                    && edge_explained(*pixel, golden, x, y)
                    && edge_explained(*g, actual, x, y))
        });
        if same {
            diff.put_pixel(x, y, faded);
            across = 0;
            if let Some(column) = column {
                *column = 0;
            }
        } else {
            *differs = true;
            differing = differing.saturating_add(1);
            diff.put_pixel(x, y, DIFFERING);
            across = across.saturating_add(1);
            longest_run = longest_run.max(across);
            if let Some(column) = column {
                *column = column.saturating_add(1);
                longest_run = longest_run.max(*column);
            }
        }
    }
    let (worst_window, worst_at) = worst_window(&differs, w, h, WINDOW);
    (Diff { differing, total, longest_run, worst_window, worst_at }, diff)
}

/// The most `true`s any `side`-square window of the `w` × `h` grid `differs` (row by row) holds,
/// and that window's top left corner; a grid smaller than the window is one window, whole.
///
/// Sums each column over the last `side` rows as it goes down, then slides along each row: one
/// pass, two additions and two subtractions a pixel.
#[must_use]
pub fn worst_window(differs: &[bool], w: u32, h: u32, side: u32) -> (u32, (u32, u32)) {
    let (side_x, side_y) = (side.min(w), side.min(h));
    let width = usize::try_from(w).unwrap_or(0);
    let at = |x: u32, y: u32| {
        let i = u64::from(y).saturating_mul(u64::from(w)).saturating_add(u64::from(x));
        usize::try_from(i).ok().and_then(|i| differs.get(i)).copied().map_or(0, u32::from)
    };
    let mut columns = vec![0_u32; width];
    let (mut worst, mut worst_at) = (0_u32, (0_u32, 0_u32));
    for y in 0..h {
        for (x, column) in (0..w).zip(columns.iter_mut()) {
            *column = column.saturating_add(at(x, y));
            if y >= side_y {
                *column = column.saturating_sub(at(x, y.saturating_sub(side_y)));
            }
        }
        if y.saturating_add(1) < side_y {
            continue;
        }
        let top = y.saturating_add(1).saturating_sub(side_y);
        let mut window = 0_u32;
        for (x, column) in (0..w).zip(columns.iter()) {
            window = window.saturating_add(*column);
            if x >= side_x {
                let left = usize::try_from(x.saturating_sub(side_x)).unwrap_or(usize::MAX);
                window = window.saturating_sub(columns.get(left).copied().unwrap_or(0));
            }
            if x.saturating_add(1) >= side_x && window > worst {
                worst = window;
                worst_at = (x.saturating_add(1).saturating_sub(side_x), top);
            }
        }
    }
    (worst, worst_at)
}

/// Whether `pixel` lies, in every channel, within the range of `other`'s 3 × 3 neighbourhood
/// around `(x, y)`, give or take [`CHANNEL_SLACK`].
///
/// A glyph's edge is coverage between its ink and its ground, and macOS 26 and 27 shade it a
/// little apart: CI's renders of `thread` differed from this Mac's in 0.23 % of their pixels, all
/// on glyph edges, with the same words (CI e2e run 37390615720). Such a pixel lies between
/// colours both pictures have beside it, so asked both ways it is explained. A line or a word
/// that came or went is not: where one picture has a colour the other has nowhere near, one of
/// the two asks fails.
fn edge_explained(pixel: Rgba<u8>, other: &RgbaImage, x: u32, y: u32) -> bool {
    let (w, h) = other.dimensions();
    let (mut low, mut high) = ([u8::MAX; 4], [u8::MIN; 4]);
    for ny in y.saturating_sub(1)..=y.saturating_add(1).min(h.saturating_sub(1)) {
        for nx in x.saturating_sub(1)..=x.saturating_add(1).min(w.saturating_sub(1)) {
            let Some(near) = other.get_pixel_checked(nx, ny) else { continue };
            for (c, v) in near.0.iter().enumerate() {
                if let (Some(lo), Some(hi)) = (low.get_mut(c), high.get_mut(c)) {
                    *lo = (*lo).min(*v);
                    *hi = (*hi).max(*v);
                }
            }
        }
    }
    pixel.0.iter().zip(low.iter().zip(high.iter())).all(|(v, (lo, hi))| {
        *v >= lo.saturating_sub(CHANNEL_SLACK) && *v <= hi.saturating_add(CHANNEL_SLACK)
    })
}

/// Luma buckets of [`luma_histogram`].
pub const LUMA_BUCKETS: usize = 16;

/// How many of `rect`'s pixels fall in each sixteenth of the luma range (BT.709 weights): what
/// a picture is made of, wherever its moving parts happen to be.
#[must_use]
pub fn luma_histogram(img: &RgbaImage, rect: PixelRect) -> [u64; LUMA_BUCKETS] {
    let mut buckets = [0_u64; LUMA_BUCKETS];
    let [left, top, width, height] = rect;
    for y in top..top.saturating_add(height).min(img.height()) {
        for x in left..left.saturating_add(width).min(img.width()) {
            let [r, g, b, _a] = img.get_pixel(x, y).0;
            let weighted = [(2126_u32, r), (7152, g), (722, b)]
                .iter()
                .fold(0_u32, |sum, &(w, c)| sum.saturating_add(w.saturating_mul(u32::from(c))));
            let luma = weighted / 10_000;
            let bucket = usize::try_from(luma / 16).unwrap_or(0).min(LUMA_BUCKETS - 1);
            if let Some(count) = buckets.get_mut(bucket) {
                *count = count.saturating_add(1);
            }
        }
    }
    buckets
}

/// How far apart two pictures' luma histograms are.
///
/// It is the share of pixels that would have to change bucket to turn one into the other
/// (total variation distance): 0 for the same mix, 1 for none in common. A page of text that
/// scrolled is the same mix; a picture gone black, torn or scaled wrong is not.
#[must_use]
pub fn luma_distance(a: &[u64; LUMA_BUCKETS], b: &[u64; LUMA_BUCKETS]) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "pixel counts fit f64 exactly")]
    let share = |h: &[u64; LUMA_BUCKETS]| {
        let total = h.iter().sum::<u64>().max(1) as f64;
        h.map(|c| c as f64 / total)
    };
    let (a, b) = (share(a), share(b));
    a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum::<f64>() / 2.0
}

/// Pixels within 8 of `rgb` on every channel: how much of a flat colour the frame shows.
#[must_use]
pub fn pixels_near(img: &RgbaImage, rgb: [u8; 3]) -> usize {
    img.pixels().filter(|p| p.0.iter().zip(rgb.iter()).all(|(a, b)| a.abs_diff(*b) <= 8)).count()
}

/// The fraction of pixels that are not close to the top-left pixel's colour: a blank frame is
/// a renderer failure, not a layout to compare.
#[must_use]
pub fn foreground_fraction(img: &RgbaImage) -> f64 {
    let bg = img.get_pixel_checked(0, 0).copied().unwrap_or(Rgba([0, 0, 0, 0]));
    let differing = img
        .pixels()
        .filter(|p| p.0.iter().zip(bg.0.iter()).any(|(a, b)| a.abs_diff(*b) > 24))
        .count();
    let total = u64::from(img.width()).saturating_mul(u64::from(img.height()));
    if total == 0 {
        return 0.0;
    }
    #[expect(clippy::cast_precision_loss, reason = "pixel counts fit f64 exactly")]
    let f = differing as f64 / total as f64;
    f
}

/// Policy for accepting golden renders.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accept {
    /// Keep matching goldens and fail on changes.
    Off,
    /// Write every golden the frame differs from, by one pixel or one word; keep the rest.
    Changed,
    /// Rewrite every golden encountered.
    All,
    /// Write no golden and fail on none: a changed frame leaves its render and diff under the
    /// artifacts for a design review, so one run renders every golden a test reaches.
    Review,
}

impl Accept {
    /// Parse the accept mode from an environment variable value.
    #[must_use]
    pub fn parse(val: Option<&str>) -> Self {
        match val {
            Some("1" | "changed") => Self::Changed,
            Some("all") => Self::All,
            Some("review") => Self::Review,
            _ => Self::Off,
        }
    }

    /// The self-test's accept mode, from `SLOPTY_E2E_ACCEPT`.
    #[must_use]
    pub fn from_env() -> Self {
        let val = std::env::var("SLOPTY_E2E_ACCEPT").ok();
        Self::parse(val.as_deref())
    }

    /// An in-process test's accept mode, as its insta snapshots': `INSTA_UPDATE=always` (what
    /// `cargo xtask remote --accept-snapshots` runs the tests with) writes every golden a frame
    /// differs from, and anything else compares (CI's `INSTA_UPDATE=no` among them).
    #[must_use]
    pub fn from_insta_env() -> Self {
        match std::env::var("INSTA_UPDATE").ok().as_deref() {
            Some("always") => Self::Changed,
            _ => Self::Off,
        }
    }

    /// This mode for a golden another test writes: an accept compares instead, so a frame that
    /// differs from the writer's fails loudly; a review still only reviews.
    #[must_use]
    pub const fn without_writing(self) -> Self {
        match self {
            Self::Changed | Self::All => Self::Off,
            Self::Off | Self::Review => self,
        }
    }
}

/// Action to take on a golden snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GoldenAction {
    /// Write or overwrite the golden.
    Write,
    /// Keep the existing golden unchanged.
    Keep,
    /// Fail the snapshot comparison.
    Fail,
    /// Leave the golden, report the change and carry on.
    Report,
}

/// How a frame compares with its golden.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Likeness {
    /// No golden yet.
    Missing,
    /// No pixel and no word differs.
    Same,
    /// Some pixels differ, as noise does: the frame passes, but the golden is not it.
    Near,
    /// A window or a run past noise, another size, or a word changed.
    Apart,
}

/// Decide what action to take for a golden snapshot.
///
/// An accept rewrites a golden whenever the frame differs from it at all, not only when it
/// fails: a golden kept because its frame stayed within the noise would go on showing a past
/// word or shade, and a review of the goldens would review that past. A run that does not accept
/// writes nothing and fails on a golden that is not there, as insta fails on a snapshot: a golden
/// written by a comparing run (CI's, into a tree it throws away) would pass unseen.
#[must_use]
pub const fn golden_action(accept: Accept, likeness: Likeness) -> GoldenAction {
    match (accept, likeness) {
        (Accept::Review, Likeness::Missing | Likeness::Near | Likeness::Apart) => {
            GoldenAction::Report
        }
        (Accept::All, _)
        | (Accept::Changed, Likeness::Missing | Likeness::Near | Likeness::Apart) => {
            GoldenAction::Write
        }
        (Accept::Off, Likeness::Missing | Likeness::Apart) => GoldenAction::Fail,
        (Accept::Off, Likeness::Near)
        | (Accept::Off | Accept::Changed | Accept::Review, Likeness::Same) => GoldenAction::Keep,
    }
}

/// Where a golden lives and what a check of it does: its directory, the run's artifacts
/// directory, the accept mode and the share of a frame that may differ.
#[derive(Clone, Copy, Debug)]
pub struct Goldens<'a> {
    /// The directory of the goldens, `<name>.png` and `<name>.txt`.
    pub dir: &'a Path,
    /// Where a frame's render, diff and words go for a review.
    pub artifacts: &'a Path,
    /// What a frame that differs does to its golden.
    pub accept: Accept,
    /// The most of a frame's pixels that may differ ([`MAC_SHARE`], [`SIMULATOR_SHARE`]).
    pub share: f64,
}

impl Goldens<'_> {
    /// Compare `actual` with `<dir>/<name>.png` outside `masks`.
    ///
    /// A frame that differs by any pixel, or has no golden, is written to
    /// `<artifacts>/<name>.actual.png` and its diff to `<artifacts>/<name>.diff.png`, which a
    /// failure names; one that matches leaves neither. An accept writes the golden
    /// ([`golden_action`]).
    ///
    /// # Errors
    ///
    /// When there is no golden and the run does not accept, the sizes differ, the frame differs
    /// past noise ([`Diff::passes`]), or a file cannot be written.
    #[expect(clippy::print_stderr, reason = "test helper; stderr is the test log")]
    pub fn pixels(&self, name: &str, actual: &RgbaImage, masks: &[PixelRect]) -> Result<Diff> {
        let golden_path = self.dir.join(format!("{name}.png"));
        let actual_path = self.artifacts.join(format!("{name}.actual.png"));
        let diff_path = self.artifacts.join(format!("{name}.diff.png"));
        let write_actual = || -> Result<()> {
            std::fs::create_dir_all(self.artifacts)?;
            actual.save(&actual_path).with_context(|| format!("write {}", actual_path.display()))
        };

        if !golden_path.exists() {
            write_actual()?;
            let total = u64::from(actual.width()).saturating_mul(u64::from(actual.height()));
            let all =
                Diff { differing: total, total, longest_run: 0, worst_window: 0, worst_at: (0, 0) };
            return match golden_action(self.accept, Likeness::Missing) {
                GoldenAction::Report => {
                    eprintln!(
                        "golden {name}: REVIEW no golden yet; render at {}",
                        actual_path.display()
                    );
                    Ok(all)
                }
                GoldenAction::Write => {
                    std::fs::create_dir_all(self.dir)?;
                    actual
                        .save(&golden_path)
                        .with_context(|| format!("write {}", golden_path.display()))?;
                    eprintln!("golden {name}: wrote {}", golden_path.display());
                    Ok(Diff { differing: 0, ..all })
                }
                GoldenAction::Keep | GoldenAction::Fail => bail!(
                    "golden {name}: no golden at {}; an accept writes it (the frame: {})",
                    golden_path.display(),
                    actual_path.display()
                ),
            };
        }

        let golden = image::open(&golden_path)
            .with_context(|| format!("read {}", golden_path.display()))?
            .into_rgba8();
        let (diff, image) = compare_masked(actual, &golden, masks);
        let same_size = golden.dimensions() == actual.dimensions();
        let likeness = if !same_size || !diff.passes(self.share) {
            Likeness::Apart
        } else if diff.differing > 0 {
            Likeness::Near
        } else {
            Likeness::Same
        };
        // A render or a diff left from an older run would show what no longer differs.
        if likeness == Likeness::Same {
            for stale in [&actual_path, &diff_path] {
                if stale.exists() {
                    std::fs::remove_file(stale)?;
                }
            }
        } else {
            write_actual()?;
            if same_size {
                image.save(&diff_path).with_context(|| format!("write {}", diff_path.display()))?;
            } else if diff_path.exists() {
                std::fs::remove_file(&diff_path)?;
            }
        }
        match golden_action(self.accept, likeness) {
            GoldenAction::Write => {
                std::fs::create_dir_all(self.dir)?;
                actual
                    .save(&golden_path)
                    .with_context(|| format!("write {}", golden_path.display()))?;
                eprintln!("golden {name}: wrote {} ({})", golden_path.display(), diff.describe());
                Ok(diff)
            }
            GoldenAction::Keep => {
                eprintln!("golden {name}: {}", diff.describe());
                Ok(diff)
            }
            GoldenAction::Report => {
                eprintln!(
                    "golden {name}: REVIEW {}\n  actual: {}\n  diff:   {}",
                    diff.describe(),
                    actual_path.display(),
                    diff_path.display()
                );
                Ok(diff)
            }
            GoldenAction::Fail if !same_size => bail!(
                "golden {name}: the golden is {:?}, the frame {:?} (see {})",
                golden.dimensions(),
                actual.dimensions(),
                actual_path.display()
            ),
            GoldenAction::Fail => bail!(
                "golden {name}: {}; a frame may differ in at most {:.3}% of its pixels\n  \
                 actual: {}\n  diff:   {}\n  golden: {}",
                diff.describe(),
                self.share * 100.0,
                actual_path.display(),
                diff_path.display(),
                golden_path.display()
            ),
        }
    }

    /// `text` against `<dir>/<name>.txt`, exactly, under the same accept as the picture. Text that
    /// differs, or has no golden, is written to `<artifacts>/<name>.actual.txt`, and a changed line
    /// fails with the lines around it.
    ///
    /// # Errors
    ///
    /// When there is no golden and the run does not accept, a line differs, or a file cannot be
    /// read or written.
    #[expect(clippy::print_stderr, reason = "test helper; stderr is the test log")]
    pub fn text(&self, name: &str, text: &str) -> Result<()> {
        let golden_path = self.dir.join(format!("{name}.txt"));
        let actual_path = self.artifacts.join(format!("{name}.actual.txt"));
        let golden = match std::fs::read_to_string(&golden_path) {
            Ok(golden) => Some(golden),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("read {}", golden_path.display())),
        };
        let likeness = match golden.as_deref() {
            None => Likeness::Missing,
            Some(golden) if golden == text => Likeness::Same,
            Some(_) => Likeness::Apart,
        };
        if likeness == Likeness::Same {
            if actual_path.exists() {
                std::fs::remove_file(&actual_path)?;
            }
        } else {
            std::fs::create_dir_all(self.artifacts)?;
            std::fs::write(&actual_path, text)
                .with_context(|| format!("write {}", actual_path.display()))?;
        }
        match golden_action(self.accept, likeness) {
            GoldenAction::Keep => Ok(()),
            GoldenAction::Write => {
                std::fs::write(&golden_path, text)
                    .with_context(|| format!("write {}", golden_path.display()))?;
                eprintln!("golden {name}: wrote text {}", golden_path.display());
                Ok(())
            }
            GoldenAction::Report => {
                eprintln!(
                    "golden {name}: REVIEW its words changed\n{}",
                    changed_lines(golden.as_deref(), text)
                );
                Ok(())
            }
            GoldenAction::Fail if golden.is_none() => bail!(
                "golden {name}: no words at {}; an accept writes them (the frame's: {})",
                golden_path.display(),
                actual_path.display()
            ),
            GoldenAction::Fail => bail!(
                "golden {name}: its words changed (golden {}, actual {})\n{}",
                golden_path.display(),
                actual_path.display(),
                changed_lines(golden.as_deref(), text)
            ),
        }
    }
}

/// The lines `actual` changed from `golden`, a line of context round each, `-` the golden's
/// and `+` the frame's.
#[must_use]
pub fn changed_lines(golden: Option<&str>, actual: &str) -> String {
    similar::TextDiff::from_lines(golden.unwrap_or_default(), actual)
        .unified_diff()
        .context_radius(1)
        .header("golden", "actual")
        .to_string()
}

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "a test's pixel coordinates, far inside u32")]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    /// A deterministic stream of numbers, so noise lands in the same places every run.
    fn lcg(seed: &mut u64) -> u64 {
        *seed =
            seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        *seed >> 33
    }

    /// The settings page's ground with rows of words on it: dark ink on a light ground in short
    /// strokes, as text is, over a frame the size of the self-test's window.
    fn page() -> RgbaImage {
        let mut page = solid(1320, 880, [242, 242, 244]);
        for row in 0..30_u32 {
            for x in (300..1100).step_by(9) {
                for dy in 0..9 {
                    page.put_pixel(x, 40 + row * 28 + dy, Rgba([40, 40, 44, 255]));
                    page.put_pixel(x + 1, 40 + row * 28 + dy, Rgba([140, 140, 146, 255]));
                }
            }
        }
        page
    }

    #[test]
    fn identical_frames_do_not_differ() {
        let a = solid(4, 4, [10, 20, 30]);
        let (diff, _) = compare(&a, &a);
        assert_eq!(
            diff,
            Diff { differing: 0, total: 16, longest_run: 0, worst_window: 0, worst_at: (0, 0) }
        );
        assert!(diff.passes(MAC_SHARE));
    }

    #[test]
    fn small_channel_noise_is_ignored_and_real_change_counted() {
        let a = solid(4, 4, [100, 100, 100]);
        let mut b = solid(4, 4, [100 + CHANNEL_SLACK, 100, 100]);
        b.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
        b.put_pixel(3, 3, Rgba([255, 255, 255, 255]));
        let (diff, image) = compare(&a, &b);
        assert_eq!(diff.differing, 2);
        assert_eq!(image.get_pixel(0, 0), &DIFFERING);
        assert_ne!(image.get_pixel(1, 1), &DIFFERING);
    }

    /// A 40 × 40 icon that changed is 0.14 % of the self-test's frame, under the 0.2 % the old
    /// whole-frame rule allowed; its windows are full, so it fails, and the diff says where.
    #[test]
    fn a_changed_icon_fails_however_small_a_share_of_the_frame() {
        let golden = page();
        let mut actual = golden.clone();
        for y in 600..640 {
            for x in 1200..1240 {
                actual.put_pixel(x, y, Rgba([90, 70, 220, 255]));
            }
        }
        let (diff, _) = compare(&actual, &golden);
        assert_eq!(diff.differing, 1600);
        assert!(diff.fraction() < 0.002, "{}", diff.describe());
        assert_eq!(diff.worst_window, WINDOW * WINDOW, "{}", diff.describe());
        assert!(!diff.passes(MAC_SHARE), "{}", diff.describe());
        let (x, y) = diff.worst_at;
        assert!((1200..=1224).contains(&x) && (600..=624).contains(&y), "{}", diff.describe());

        // A glyph swapped for another in one place: half an icon's box, as a changed state glyph.
        let mut glyph = golden.clone();
        for y in 600..612 {
            for x in 1200..1212 {
                if (x + y) % 2 == 0 {
                    glyph.put_pixel(x, y, Rgba([40, 40, 44, 255]));
                }
            }
        }
        let (diff, _) = compare(&glyph, &golden);
        assert_eq!(diff.differing, 72);
        assert!(!diff.passes(MAC_SHARE), "{}", diff.describe());
    }

    /// GPU noise at today's measured level, the most two runs of a Mac golden differed by (0.051 %
    /// of the frame, about 590 pixels), in colours no edge explains and scattered over the words
    /// where anti-aliasing lands, never fills a window: it passes. Ten times that fills no window
    /// either, and fails on its share: a change spread over a page's text, as a weight or a
    /// hinting is, too thin for any window.
    #[test]
    fn scattered_noise_passes_and_a_change_spread_thin_fails_on_its_share() {
        let golden = page();
        for (pixels, label, passes) in
            [(592_u32, "today's 0.051 %", true), (5_920, "ten times that", false)]
        {
            let mut noisy = golden.clone();
            let mut seed = u64::from(pixels);
            let mut placed = 0;
            while placed < pixels {
                let x = 300 + u32::try_from(lcg(&mut seed) % 800).unwrap();
                let y = 40 + u32::try_from(lcg(&mut seed) % 840).unwrap();
                // Past what either picture has nearby, so no edge explains it.
                let shade = u8::try_from(lcg(&mut seed) % 64).unwrap();
                noisy.put_pixel(x, y, Rgba([255, shade, 255, 255]));
                placed += 1;
            }
            let (diff, _) = compare(&noisy, &golden);
            assert!(diff.differing > u64::from(pixels) * 9 / 10, "{label}: {}", diff.describe());
            assert!(
                diff.worst_window <= WINDOW_NOISE && diff.longest_run < SEAM_RUN,
                "{label}: scattered, {}",
                diff.describe()
            );
            assert_eq!(diff.passes(MAC_SHARE), passes, "{label}: {}", diff.describe());
        }
    }

    /// A one-pixel rule moved down a pixel differs along both its rows, which fills a window
    /// past noise however short the rule; one that went from across the frame is a seam.
    #[test]
    fn a_one_pixel_line_that_moved_or_went_is_caught() {
        let ground = [242, 242, 244];
        let rule = Rgba([120, 120, 124, 255]);
        let mut golden = solid(400, 100, ground);
        for x in 100..124 {
            golden.put_pixel(x, 50, rule);
        }
        let mut moved = solid(400, 100, ground);
        for x in 100..124 {
            moved.put_pixel(x, 51, rule);
        }
        let (diff, _) = compare(&moved, &golden);
        assert_eq!(diff.differing, 48);
        assert_eq!(diff.worst_window, 32, "{}", diff.describe());
        assert!(!diff.passes(MAC_SHARE), "a 24-pixel rule a pixel lower: {}", diff.describe());

        let mut divider = solid(400, 100, ground);
        for x in 0..400 {
            divider.put_pixel(x, 50, rule);
        }
        let (diff, _) = compare(&solid(400, 100, ground), &divider);
        assert_eq!(diff.worst_window, WINDOW, "a line fills a window's row alone");
        assert_eq!(diff.longest_run, 400);
        assert!(!diff.passes(MAC_SHARE), "a divider that went: {}", diff.describe());
    }

    /// The worst window is found wherever it lies, the frame's edges included, and a frame
    /// smaller than a window is one window.
    #[test]
    fn the_worst_window_is_found_anywhere() {
        let (w, h) = (40_u32, 30_u32);
        let mut grid = vec![false; 1200];
        for (x, y) in [(39, 29), (38, 29), (39, 28), (24, 14), (0, 0)] {
            grid[usize::try_from(y * w + x).unwrap()] = true;
        }
        assert_eq!(worst_window(&grid, w, h, 16), (4, (24, 14)));
        assert_eq!(worst_window(&grid, w, h, 4), (3, (36, 26)));
        assert_eq!(worst_window(&grid, w, h, 64).0, 5, "the whole grid, smaller than a window");
        assert_eq!(worst_window(&[], 0, 0, 16), (0, (0, 0)));
    }

    /// An edge a shade apart, between colours both pictures have beside it, matches; a thin line
    /// that went, and a word's ink where the other has ground, still count.
    #[test]
    fn a_glyphs_edge_shaded_apart_matches_and_a_lost_line_does_not() {
        let ground = [240, 240, 240];
        let ink = [40, 40, 40];
        let mut golden = solid(5, 5, ground);
        for y in 0..5 {
            golden.put_pixel(1, y, Rgba([ink[0], ink[1], ink[2], 255]));
            golden.put_pixel(2, y, Rgba([140, 140, 140, 255]));
        }
        let mut actual = golden.clone();
        for y in 0..5 {
            actual.put_pixel(2, y, Rgba([180, 180, 180, 255]));
        }
        assert_eq!(compare(&actual, &golden).0.differing, 0, "an edge shaded 40 apart");

        let lost = solid(5, 5, ground);
        let (diff, _) = compare(&lost, &golden);
        assert_eq!(diff.differing, 10, "the line and its edge went");

        let mut inked = golden.clone();
        inked.put_pixel(3, 2, Rgba([ink[0], ink[1], ink[2], 255]));
        assert_eq!(compare(&inked, &golden).0.differing, 1, "ink where the golden has ground");
    }

    /// A hairline across the frame is a seam however few of its pixels it is; the same number
    /// of pixels scattered, as glyph edges are, is not one.
    #[test]
    fn a_hairline_is_a_long_run_and_scattered_pixels_are_not() {
        let golden = solid(200, 40, [13, 13, 13]);
        let mut seamed = golden.clone();
        for x in 0..200 {
            seamed.put_pixel(x, 20, Rgba([232, 232, 234, 255]));
        }
        let (diff, _) = compare(&seamed, &golden);
        assert_eq!((diff.differing, diff.longest_run), (200, 200), "across");
        let mut post = golden.clone();
        for y in 0..40 {
            post.put_pixel(7, y, Rgba([232, 232, 234, 255]));
        }
        assert_eq!(compare(&post, &golden).0.longest_run, 40, "down");

        let mut scattered = golden.clone();
        for i in 0..200 {
            scattered.put_pixel((i * 7) % 200, (i * 3) % 40, Rgba([232, 232, 234, 255]));
        }
        let (diff, _) = compare(&scattered, &golden);
        assert!(diff.longest_run < SEAM_RUN, "{diff:?}");
        let (diff, _) = compare_masked(&seamed, &golden, &[[100, 0, 1, 40]]);
        assert_eq!(diff.longest_run, 100, "a mask breaks a run");
    }

    #[test]
    fn a_missing_golden_pixel_counts_as_different() {
        let a = solid(2, 2, [1, 1, 1]);
        let b = solid(1, 1, [1, 1, 1]);
        let (diff, _) = compare(&a, &b);
        assert_eq!(diff.differing, 3);
    }

    #[test]
    fn a_masked_pixel_is_neither_compared_nor_counted() {
        let a = solid(4, 4, [100, 100, 100]);
        let mut b = a.clone();
        b.put_pixel(1, 1, Rgba([0, 0, 0, 255]));
        b.put_pixel(3, 3, Rgba([0, 0, 0, 255]));
        let (diff, _) = compare_masked(&a, &b, &[[0, 0, 2, 2]]);
        assert_eq!((diff.differing, diff.total, diff.longest_run), (1, 12, 1));
        let (diff, _) = compare_masked(&a, &b, &[[0, 0, 2, 2], [3, 3, 9, 9]]);
        assert_eq!((diff.differing, diff.total, diff.longest_run), (0, 11, 0));
    }

    #[test]
    fn a_scrolled_mix_is_near_and_a_blank_picture_is_far() {
        let mut page = solid(8, 8, [24, 24, 24]);
        for x in 0..8 {
            page.put_pixel(x, 2, Rgba([225, 225, 225, 255]));
        }
        let mut scrolled = solid(8, 8, [24, 24, 24]);
        for x in 0..8 {
            scrolled.put_pixel(x, 5, Rgba([225, 225, 225, 255]));
        }
        let whole = [0, 0, 8, 8];
        let (a, b) = (luma_histogram(&page, whole), luma_histogram(&scrolled, whole));
        assert!(luma_distance(&a, &b) < f64::EPSILON, "{a:?} {b:?}");
        let black = luma_histogram(&solid(8, 8, [0, 0, 0]), whole);
        assert!(luma_distance(&a, &black) > 0.99, "{a:?} {black:?}");
        let half = luma_histogram(&page, [0, 0, 8, 4]);
        assert_eq!(half.iter().sum::<u64>(), 32, "the rect bounds the count");
    }

    /// A premultiplied frame over the backdrop: clear shows the slate, opaque stays as it is,
    /// half-covered blends the two.
    #[test]
    fn a_frame_over_the_backdrop_is_opaque() {
        let mut frame =
            RgbaImage::from_raw(3, 1, vec![0, 0, 0, 0, 200, 100, 50, 255, 100, 50, 25, 128])
                .unwrap();
        over_backdrop(&mut frame);
        assert_eq!(frame.get_pixel(0, 0).0, [72, 96, 128, 255]);
        assert_eq!(frame.get_pixel(1, 0).0, [200, 100, 50, 255]);
        assert_eq!(frame.get_pixel(2, 0).0, [136, 98, 89, 255]);
    }

    /// A frame's words are a line a node, its mark on the focused one, a newline in a label kept
    /// on its line.
    #[test]
    fn a_frames_words_are_a_line_a_node() {
        let spoken = |role, label, value, focused| Spoken { role, label, value, focused };
        let text = words([
            spoken("Tab", Some("Workspace 1"), None, true),
            spoken("TextInput", Some("Message"), Some("two\nlines"), false),
            spoken("Group", Some(""), None, false),
        ]);
        assert_eq!(
            text,
            "Tab \u{2502} Workspace 1 (focused)\nTextInput \u{2502} Message \u{2502} \
             two\u{23ce}lines\nGroup\n"
        );
    }

    /// A golden that differs fails with the diff written beside the render; an accept writes it
    /// and passes; a frame that matches leaves no diff from the run before.
    #[test]
    fn a_golden_fails_on_a_change_and_an_accept_writes_it() {
        let dir = std::env::temp_dir().join(format!("slopty-golden-{}", std::process::id()));
        let artifacts = dir.join("artifacts");
        let check = |accept| Goldens { dir: &dir, artifacts: &artifacts, accept, share: MAC_SHARE };
        let golden = page();
        let error = check(Accept::Off).pixels("page", &golden, &[]).unwrap_err().to_string();
        assert!(error.contains("no golden"), "{error}");
        assert!(!dir.join("page.png").exists(), "a comparing run writes no golden");
        assert!(artifacts.join("page.actual.png").exists(), "the frame is left for a review");
        check(Accept::Changed).pixels("page", &golden, &[]).unwrap();
        assert!(dir.join("page.png").exists(), "an accept writes it");
        check(Accept::Off).pixels("page", &golden, &[]).unwrap();
        assert!(!artifacts.join("page.actual.png").exists(), "a frame that matches leaves none");

        let mut changed = golden;
        for y in 600..640 {
            for x in 1200..1240 {
                changed.put_pixel(x, y, Rgba([90, 70, 220, 255]));
            }
        }
        let error = check(Accept::Off).pixels("page", &changed, &[]).unwrap_err().to_string();
        assert!(error.contains("the worst 16×16 window"), "{error}");
        assert!(artifacts.join("page.diff.png").exists(), "{error}");
        let masked = check(Accept::Off).pixels("page", &changed, &[[1200, 600, 40, 40]]).unwrap();
        assert_eq!(masked.differing, 0, "masked away");
        assert!(!artifacts.join("page.diff.png").exists(), "no stale diff");

        check(Accept::Changed).pixels("page", &changed, &[]).unwrap();
        check(Accept::Off).pixels("page", &changed, &[]).unwrap();

        check(Accept::Off).text("page", "Button \u{2502} New\n").unwrap_err();
        check(Accept::Changed).text("page", "Button \u{2502} New\n").unwrap();
        check(Accept::Off).text("page", "Button \u{2502} New\n").unwrap();
        let error = check(Accept::Off).text("page", "Button \u{2502} Old\n").unwrap_err();
        assert!(error.to_string().contains("+Button \u{2502} Old"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn accept_parsing() {
        assert_eq!(Accept::parse(Some("1")), Accept::Changed);
        assert_eq!(Accept::parse(Some("changed")), Accept::Changed);
        assert_eq!(Accept::parse(Some("all")), Accept::All);
        assert_eq!(Accept::parse(Some("review")), Accept::Review);
        assert_eq!(Accept::parse(None), Accept::Off);
        assert_eq!(Accept::parse(Some("")), Accept::Off);
        assert_eq!(Accept::parse(Some("0")), Accept::Off);
        assert_eq!(Accept::parse(Some("other")), Accept::Off);
    }

    /// Every accept mode against every likeness. An accept rewrites a golden its frame
    /// differs from by any pixel, within the noise too, so no golden goes on showing what
    /// the app no longer draws; a frame within the noise still passes without one.
    #[test]
    fn an_accept_rewrites_a_golden_on_any_differing_pixel() {
        use GoldenAction::{Fail, Keep, Report, Write};
        use Likeness::{Apart, Missing, Near, Same};
        let table = [
            (Accept::Off, [Fail, Keep, Keep, Fail]),
            (Accept::Changed, [Write, Keep, Write, Write]),
            (Accept::All, [Write, Write, Write, Write]),
            (Accept::Review, [Report, Keep, Report, Report]),
        ];
        for (accept, actions) in table {
            for (likeness, action) in [Missing, Same, Near, Apart].into_iter().zip(actions) {
                assert_eq!(golden_action(accept, likeness), action, "{accept:?} {likeness:?}");
            }
        }
    }
}
