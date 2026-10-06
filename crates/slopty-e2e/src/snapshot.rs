//! Golden renders: compare an app frame with the PNG under `golden/`, write the actual frame
//! and a diff image next to the artifacts when they differ.
//!
//! A golden is held in words too: what the frame says (its accessibility tree's roles, labels
//! and values, [`crate::frame_text`]) sits beside the PNG as `golden/<name>.txt` and must match
//! it exactly. The pixel tolerance cannot see a word change, so a golden that drew the wrong
//! word would pass on its pixels; its text fails it, with the lines that changed.
//!
//! A frame counts as matching when at most `tolerance` of its pixels differ by more than
//! [`CHANNEL_SLACK`] in any channel: font hinting, the RTT readout and a blinking cursor
//! move a few hundred pixels, a broken layout moves a few hundred thousand. A pixel on an edge
//! that each picture's neighbourhood explains also matches ([`edge_explained`]): one macOS
//! rasterises a glyph's edge a shade apart from another's. Pass `--accept`
//! to write missing and failing goldens, or `--accept-all` to rewrite every golden (via
//! `SLOPTY_E2E_ACCEPT=changed` and `SLOPTY_E2E_ACCEPT=all`).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use image::{Rgba, RgbaImage};

/// Per-channel difference under which two pixels count as equal.
///
/// Small, because a theme's surfaces sit close together: `canvas` and `panel` are 11 apart in
/// the light theme, and at 24 a band moved from one to the other still matched.
pub const CHANNEL_SLACK: u8 = 4;

/// The most a glyph's edge may be shaded apart between two macOS releases and still match,
/// when both pictures' neighbourhoods explain it ([`edge_explained`]).
///
/// CI's macOS 26 renders against this Mac's goldens (CI e2e run 37390615720): of `thread`'s
/// 1 655 pixels more than [`CHANNEL_SLACK`] apart, 1 628 were within 64 and none past 150,
/// while a changed title or path moved its pixels by up to 240. A word swapped for another
/// moves ink against ground, 150 to 255 apart, so it still counts.
pub const EDGE_SLACK: u8 = 64;

/// Fraction of a Mac golden's pixels allowed to differ.
///
/// Two runs of every Mac golden, with the cursor held steady, differed by 0.051% at most
/// (`browser`; the round-trip figure and a page's anti-aliasing), so 0.2% is four times the noise.
/// At the old 1% a golden 1280 wide let ten thousand pixels through: a washed row and a word of
/// status text went unseen.
pub const MAC_TOLERANCE: f64 = 0.002;

/// A frame the app drew, and what it says in words.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The picture.
    pub image: RgbaImage,
    /// Its accessibility tree, in reading order.
    pub a11y: Vec<crate::A11yNode>,
    /// Device pixels per point: the tree is in points, the picture in pixels.
    pub scale: f32,
}

impl Frame {
    /// What the frame says ([`crate::frame_text`]), leaving out the nodes centred inside
    /// `masks`: what a masked region says moves as its picture does. A path in the run's scratch
    /// directory starts `$SCRATCH`, a port on loopback is `$PORT`, and a new note's moment is
    /// `$MOMENT`, because each machine and each run has its own.
    #[must_use]
    pub fn text(&self, masks: &[PixelRect]) -> String {
        let kept: Vec<crate::A11yNode> = self
            .a11y
            .iter()
            .filter(|node| {
                let [x, y, w, h] = node.bounds;
                let at = |v: f32| {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "pixels"
                    )]
                    let px = (v * self.scale).max(0.0).round() as u32;
                    px
                };
                let (cx, cy) = (at(w.mul_add(0.5, x)), at(h.mul_add(0.5, y)));
                !masks.iter().any(|m| holds(m, cx, cy))
            })
            .cloned()
            .collect();
        scrub_moments(&scrub_ports(&scrub_scratch(&crate::frame_text(&kept))))
    }
}

/// `text` with the moment in each new note's name (`note-2026-10-05-143210`) spelled
/// `note-$MOMENT`: a note is named for when it was made.
fn scrub_moments(text: &str) -> String {
    const NOTE: &str = "note-";
    const MOMENT: &[u8] = b"dddd-dd-dd-dddddd";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(NOTE) {
        let (before, found) = rest.split_at(at.saturating_add(NOTE.len()));
        out.push_str(before);
        let moment = found.as_bytes().get(..MOMENT.len()).is_some_and(|b| {
            b.iter().zip(MOMENT).all(|(c, m)| if *m == b'd' { c.is_ascii_digit() } else { c == m })
        });
        rest = if moment {
            out.push_str("$MOMENT");
            found.get(MOMENT.len()..).unwrap_or_default()
        } else {
            found
        };
    }
    out.push_str(rest);
    out
}

/// `text` with the port after each loopback host spelled `$PORT`: the tests' own servers bind
/// whatever port the system gives them.
fn scrub_ports(text: &str) -> String {
    ["127.0.0.1:", "[::1]:", "localhost:"].iter().fold(text.to_owned(), |text, host| {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find(host) {
            let (before, after) = rest.split_at(at.saturating_add(host.len()));
            out.push_str(before);
            let port = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            if port > 0 {
                out.push_str("$PORT");
            }
            rest = after.get(port..).unwrap_or_default();
        }
        out.push_str(rest);
        out
    })
}

/// `text` with each run's scratch directory spelled `$SCRATCH`: the system temporary
/// directory, as the app saw it raw or resolved, and the directory the run made in it, whose
/// name is random. Resolved first, since the raw root is a part of it (`/private/var/…`).
fn scrub_scratch(text: &str) -> String {
    let raw = std::env::temp_dir();
    let resolved = std::fs::canonicalize(&raw).unwrap_or_else(|_| raw.clone());
    [resolved, raw].iter().fold(text.to_owned(), |text, root| {
        let root = root.to_string_lossy();
        let root = root.trim_end_matches('/');
        if root.is_empty() { text } else { scrub_root(&text, root) }
    })
}

/// `text` with every `root/<name>` spelled `$SCRATCH`, `<name>` being the run's directory.
fn scrub_root(text: &str, root: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(root) {
        let (before, found) = rest.split_at(at);
        out.push_str(before);
        out.push_str("$SCRATCH");
        let after = found.get(root.len()..).unwrap_or_default();
        rest = after.strip_prefix('/').map_or(after, |name| {
            let end = name
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
                .unwrap_or(name.len());
            name.get(end..).unwrap_or_default()
        });
    }
    out.push_str(rest);
    out
}

impl std::ops::Deref for Frame {
    type Target = RgbaImage;

    fn deref(&self) -> &RgbaImage {
        &self.image
    }
}

/// Where the goldens live.
#[must_use]
pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden")
}

/// The outcome of a comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Diff {
    /// Pixels that differ beyond the slack.
    pub differing: u64,
    /// Pixels compared.
    pub total: u64,
}

impl Diff {
    /// Differing pixels as a fraction of the whole.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            // Precision loss is irrelevant at image sizes.
            #[expect(clippy::cast_precision_loss, reason = "pixel counts fit f64 exactly")]
            let f = self.differing as f64 / self.total as f64;
            f
        }
    }
}

/// A rectangle of a frame in device pixels: left, top, width, height.
pub type PixelRect = [u32; 4];

/// Whether `rect` holds the pixel at `(x, y)`.
const fn holds(rect: &PixelRect, x: u32, y: u32) -> bool {
    let [left, top, width, height] = *rect;
    x >= left && y >= top && x.saturating_sub(left) < width && y.saturating_sub(top) < height
}

/// Pixel-wise comparison; `diff` gets every differing pixel painted red on a faded copy of
/// `actual`.
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
    let (mut differing, mut total) = (0_u64, 0_u64);
    for (x, y, pixel) in actual.enumerate_pixels() {
        if masks.iter().any(|m| holds(m, x, y)) {
            diff.put_pixel(x, y, Rgba([150, 180, 230, 255]));
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
        } else {
            differing = differing.saturating_add(1);
            diff.put_pixel(x, y, Rgba([220, 30, 30, 255]));
        }
    }
    (Diff { differing, total }, diff)
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

/// Policy for accepting golden renders.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accept {
    /// Keep matching goldens and fail on changes.
    Off,
    /// Write missing or failing goldens; keep matching goldens.
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

    /// Read the accept mode from `SLOPTY_E2E_ACCEPT`.
    #[must_use]
    pub fn from_env() -> Self {
        let val = std::env::var("SLOPTY_E2E_ACCEPT").ok();
        Self::parse(val.as_deref())
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

/// Decide what action to take for a golden snapshot.
#[must_use]
pub const fn golden_action(
    accept: Accept,
    golden_exists: bool,
    within_tolerance: bool,
) -> GoldenAction {
    match (accept, golden_exists, within_tolerance) {
        (Accept::Review, false, _) | (Accept::Review, true, false) => GoldenAction::Report,
        (_, false, _) | (Accept::All, true, _) | (Accept::Changed, true, false) => {
            GoldenAction::Write
        }
        (Accept::Changed | Accept::Off | Accept::Review, true, true) => GoldenAction::Keep,
        (Accept::Off, true, false) => GoldenAction::Fail,
    }
}

/// Compare `actual` with `golden/<name>.png`, and its text with `golden/<name>.txt`.
///
/// When accepting, `--accept` writes missing and failing goldens, while `--accept-all`
/// rewrites every golden. Otherwise the frame is written to `<artifacts>/<name>.actual.png`
/// and, when it differs beyond `tolerance`, the diff to `<artifacts>/<name>.diff.png`, and the
/// error names both files. Its text is written to `<artifacts>/<name>.actual.txt`, and a line
/// changed fails with the lines around it.
///
/// # Errors
///
/// When the sizes differ, more than `tolerance` of the pixels differ, a word differs, or a
/// file cannot be written.
pub fn assert_matches(
    name: &str,
    actual: &Frame,
    tolerance: f64,
    artifacts: &Path,
) -> Result<Diff> {
    assert_matches_masked(name, actual, tolerance, artifacts, &[])
}

/// [`assert_matches`] outside `masks` ([`compare_masked`]): the chrome around a streamed
/// picture, held to a golden while the picture moves.
///
/// # Errors
///
/// As [`assert_matches`].
pub fn assert_matches_masked(
    name: &str,
    actual: &Frame,
    tolerance: f64,
    artifacts: &Path,
    masks: &[PixelRect],
) -> Result<Diff> {
    assert_matches_apart(name, actual, tolerance, artifacts, (masks, masks))
}

/// [`assert_matches`] with the pixels outside `pixels` and the words of the nodes outside
/// `words`.
///
/// A region masked in pixels alone says words that hold still, or that the text already scrubs
/// (a test server's port in an address), while its glyphs or its edge move from run to run.
///
/// # Errors
///
/// As [`assert_matches`].
pub fn assert_matches_apart(
    name: &str,
    actual: &Frame,
    tolerance: f64,
    artifacts: &Path,
    (pixels, words): (&[PixelRect], &[PixelRect]),
) -> Result<Diff> {
    let pixels = pixels_match(name, &actual.image, tolerance, artifacts, pixels);
    let words = text_matches(name, &actual.text(words), artifacts);
    match (pixels, words) {
        (Ok(diff), Ok(())) => Ok(diff),
        (Err(e), Ok(())) | (Ok(_), Err(e)) => Err(e),
        (Err(pixels), Err(words)) => bail!("{pixels:#}\n{words:#}"),
    }
}

/// The pixels of [`assert_matches_masked`].
#[expect(clippy::print_stderr, reason = "test helper; stderr is the test log")]
fn pixels_match(
    name: &str,
    actual: &RgbaImage,
    tolerance: f64,
    artifacts: &Path,
    masks: &[PixelRect],
) -> Result<Diff> {
    let golden_path = golden_dir().join(format!("{name}.png"));
    std::fs::create_dir_all(artifacts)?;
    let actual_path = artifacts.join(format!("{name}.actual.png"));
    actual.save(&actual_path).with_context(|| format!("write {}", actual_path.display()))?;

    let accept = Accept::from_env();
    let golden_exists = golden_path.exists();
    if !golden_exists {
        let total = u64::from(actual.width()).saturating_mul(u64::from(actual.height()));
        if golden_action(accept, false, false) == GoldenAction::Report {
            eprintln!("snapshot {name}: REVIEW no golden yet; render at {}", actual_path.display());
            return Ok(Diff { differing: total, total });
        }
        std::fs::create_dir_all(golden_dir())?;
        actual.save(&golden_path).with_context(|| format!("write {}", golden_path.display()))?;
        eprintln!("snapshot {name}: wrote golden {}", golden_path.display());
        return Ok(Diff { differing: 0, total });
    }

    let golden = image::open(&golden_path)
        .with_context(|| format!("read {}", golden_path.display()))?
        .into_rgba8();
    let (diff, image) = compare_masked(actual, &golden, masks);
    let same_size = golden.dimensions() == actual.dimensions();
    let fraction = diff.fraction();
    let within_tolerance = same_size && fraction <= tolerance;
    let action = golden_action(accept, true, within_tolerance);

    match action {
        GoldenAction::Write => {
            std::fs::create_dir_all(golden_dir())?;
            actual
                .save(&golden_path)
                .with_context(|| format!("write {}", golden_path.display()))?;
            eprintln!("snapshot {name}: wrote golden {}", golden_path.display());
            Ok(diff)
        }
        GoldenAction::Keep => {
            // Within the tolerance still leaves its picture: a golden that drifts is found
            // by where it differs, and one that matches leaves no older run's.
            let diff_path = artifacts.join(format!("{name}.diff.png"));
            if diff.differing > 0 {
                image.save(&diff_path)?;
            } else if diff_path.exists() {
                std::fs::remove_file(&diff_path)?;
            }
            eprintln!(
                "snapshot {name}: {} of {} pixels differ ({:.3}%)",
                diff.differing,
                diff.total,
                fraction * 100.0
            );
            Ok(diff)
        }
        GoldenAction::Report => {
            let diff_path = artifacts.join(format!("{name}.diff.png"));
            if same_size {
                image.save(&diff_path)?;
            }
            eprintln!(
                "snapshot {name}: REVIEW {:.3}% of pixels differ (tolerance {:.3}%)\n  actual: {}\n  diff:   {}",
                fraction * 100.0,
                tolerance * 100.0,
                actual_path.display(),
                diff_path.display()
            );
            Ok(diff)
        }
        GoldenAction::Fail => {
            if !same_size {
                bail!(
                    "snapshot {name}: golden is {:?}, frame is {:?} (see {})",
                    golden.dimensions(),
                    actual.dimensions(),
                    actual_path.display()
                );
            }
            let diff_path = artifacts.join(format!("{name}.diff.png"));
            image.save(&diff_path)?;
            bail!(
                "snapshot {name}: {:.3}% of pixels differ (tolerance {:.3}%)\n  actual: {}\n  diff:   {}\n  golden: {}",
                fraction * 100.0,
                tolerance * 100.0,
                actual_path.display(),
                diff_path.display(),
                golden_path.display()
            );
        }
    }
}

/// The text of [`assert_matches_masked`]: `text` against `golden/<name>.txt`, exactly, under
/// the same `--accept` and `--review` as the picture.
#[expect(clippy::print_stderr, reason = "test helper; stderr is the test log")]
fn text_matches(name: &str, text: &str, artifacts: &Path) -> Result<()> {
    let golden_path = golden_dir().join(format!("{name}.txt"));
    let actual_path = artifacts.join(format!("{name}.actual.txt"));
    std::fs::write(&actual_path, text)
        .with_context(|| format!("write {}", actual_path.display()))?;
    let golden = match std::fs::read_to_string(&golden_path) {
        Ok(golden) => Some(golden),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("read {}", golden_path.display())),
    };
    let same = golden.as_deref() == Some(text);
    match golden_action(Accept::from_env(), golden.is_some(), same) {
        GoldenAction::Keep => Ok(()),
        GoldenAction::Write => {
            std::fs::write(&golden_path, text)
                .with_context(|| format!("write {}", golden_path.display()))?;
            eprintln!("snapshot {name}: wrote text {}", golden_path.display());
            Ok(())
        }
        GoldenAction::Report => {
            eprintln!(
                "snapshot {name}: REVIEW its words changed\n{}",
                changed_lines(golden.as_deref(), text)
            );
            Ok(())
        }
        GoldenAction::Fail => bail!(
            "snapshot {name}: its words changed (golden {}, actual {})\n{}",
            golden_path.display(),
            actual_path.display(),
            changed_lines(golden.as_deref(), text)
        ),
    }
}

/// The lines `actual` changed from `golden`, a line of context round each, `-` the golden's
/// and `+` the frame's.
fn changed_lines(golden: Option<&str>, actual: &str) -> String {
    similar::TextDiff::from_lines(golden.unwrap_or_default(), actual)
        .unified_diff()
        .context_radius(1)
        .header("golden", "actual")
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    fn node(role: &str, label: &str, bounds: [f32; 4]) -> crate::A11yNode {
        crate::A11yNode {
            role: role.to_owned(),
            label: Some(label.to_owned()),
            value: None,
            focused: false,
            bounds,
        }
    }

    /// A frame's words are a line a node; one changed word fails with that line, the golden's
    /// and the frame's, where every pixel but a word's few would have passed.
    #[test]
    fn a_changed_word_is_a_changed_line() {
        let golden = crate::frame_text(&[
            node("Tab", "Workspace 1", [0.0, 0.0, 80.0, 20.0]),
            node("Button", "New", [90.0, 0.0, 20.0, 20.0]),
        ]);
        assert_eq!(golden, "Tab \u{2502} Workspace 1\nButton \u{2502} New\n");
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Tab", "e2e-worker", [0.0, 0.0, 80.0, 20.0]),
                node("Button", "New", [90.0, 0.0, 20.0, 20.0]),
            ],
            scale: 2.0,
        };
        let diff = changed_lines(Some(&golden), &frame.text(&[]));
        assert!(diff.contains("-Tab \u{2502} Workspace 1"), "{diff}");
        assert!(diff.contains("+Tab \u{2502} e2e-worker"), "{diff}");
        assert!(!diff.contains("-Button"), "the line that stayed is context: {diff}");
    }

    /// What a masked region says moves with its picture: its nodes leave the text.
    #[test]
    fn a_masked_node_says_nothing() {
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Status", "Frames late", [10.0, 10.0, 20.0, 10.0]),
                node("Button", "Close tile", [100.0, 10.0, 20.0, 10.0]),
            ],
            scale: 2.0,
        };
        assert_eq!(frame.text(&[[0, 0, 80, 60]]), "Button \u{2502} Close tile\n");
    }

    /// A path in a run's scratch directory, under this machine's temporary root raw or
    /// resolved, reads the same on every machine and in every run; the words round it stay.
    #[test]
    fn a_scratch_path_reads_the_same_everywhere() {
        let raw = std::env::temp_dir().join("slopty-e2e-47c78742/project/main.rs");
        let resolved = std::fs::canonicalize(std::env::temp_dir())
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("slopty-e2e-projects-8103d87d");
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Label", &format!("File {}", raw.display()), [0.0, 0.0, 1.0, 1.0]),
                node("Label", &format!("In {}. Done", resolved.display()), [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        assert_eq!(
            frame.text(&[]),
            "Label \u{2502} File $SCRATCH/project/main.rs\nLabel \u{2502} In $SCRATCH. Done\n"
        );
    }

    /// A new note's name, which is the moment it was made, reads the same in every run; any
    /// other name with "note-" in it stays.
    #[test]
    fn a_new_notes_moment_reads_the_same_in_every_run() {
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Document", "File ~/note-2026-10-05-034645.md", [0.0, 0.0, 1.0, 1.0]),
                node("Label", "note-taking, note-2026-10 and note-", [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        assert_eq!(
            frame.text(&[]),
            "Document \u{2502} File ~/note-$MOMENT.md\n\
             Label \u{2502} note-taking, note-2026-10 and note-\n"
        );
    }

    /// A test server's port, which the system picks, reads the same in every run; a port
    /// anywhere else stays.
    #[test]
    fn a_loopback_port_reads_the_same_in_every_run() {
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Button", "http://127.0.0.1:58423/", [0.0, 0.0, 1.0, 1.0]),
                node("Label", "localhost:3000 and [::1]:61 and studio:22", [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        assert_eq!(
            frame.text(&[]),
            "Button \u{2502} http://127.0.0.1:$PORT/\n\
             Label \u{2502} localhost:$PORT and [::1]:$PORT and studio:22\n"
        );
    }

    #[test]
    fn identical_frames_do_not_differ() {
        let a = solid(4, 4, [10, 20, 30]);
        let (diff, _) = compare(&a, &a);
        assert_eq!(diff, Diff { differing: 0, total: 16 });
        assert!((diff.fraction() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn small_channel_noise_is_ignored_and_real_change_counted() {
        let a = solid(4, 4, [100, 100, 100]);
        let mut b = solid(4, 4, [100 + CHANNEL_SLACK, 100, 100]);
        b.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
        b.put_pixel(3, 3, Rgba([255, 255, 255, 255]));
        let (diff, image) = compare(&a, &b);
        assert_eq!(diff.differing, 2);
        assert_eq!(image.get_pixel(0, 0), &Rgba([220, 30, 30, 255]));
        assert_ne!(image.get_pixel(1, 1), &Rgba([220, 30, 30, 255]));
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
        assert_eq!(diff, Diff { differing: 1, total: 12 });
        let (diff, _) = compare_masked(&a, &b, &[[0, 0, 2, 2], [3, 3, 9, 9]]);
        assert_eq!(diff, Diff { differing: 0, total: 11 });
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

    #[test]
    fn accept_parsing() {
        assert_eq!(Accept::parse(Some("1")), Accept::Changed);
        assert_eq!(Accept::parse(Some("changed")), Accept::Changed);
        assert_eq!(Accept::parse(Some("all")), Accept::All);
        assert_eq!(Accept::parse(None), Accept::Off);
        assert_eq!(Accept::parse(Some("")), Accept::Off);
        assert_eq!(Accept::parse(Some("0")), Accept::Off);
        assert_eq!(Accept::parse(Some("other")), Accept::Off);
    }

    #[test]
    fn golden_action_nine_combinations() {
        // Missing golden: writes regardless of accept mode.
        assert_eq!(golden_action(Accept::Off, false, false), GoldenAction::Write);
        assert_eq!(golden_action(Accept::Changed, false, false), GoldenAction::Write);
        assert_eq!(golden_action(Accept::All, false, false), GoldenAction::Write);
        assert_eq!(golden_action(Accept::Off, false, true), GoldenAction::Write);
        assert_eq!(golden_action(Accept::Changed, false, true), GoldenAction::Write);
        assert_eq!(golden_action(Accept::All, false, true), GoldenAction::Write);

        // Golden exists, within tolerance: keep unless All.
        assert_eq!(golden_action(Accept::Off, true, true), GoldenAction::Keep);
        assert_eq!(golden_action(Accept::Changed, true, true), GoldenAction::Keep);
        assert_eq!(golden_action(Accept::All, true, true), GoldenAction::Write);

        // Golden exists, beyond tolerance: fail unless accepting.
        assert_eq!(golden_action(Accept::Off, true, false), GoldenAction::Fail);
        assert_eq!(golden_action(Accept::Changed, true, false), GoldenAction::Write);
        assert_eq!(golden_action(Accept::All, true, false), GoldenAction::Write);
        assert_eq!(golden_action(Accept::Review, true, false), GoldenAction::Report);
        assert_eq!(golden_action(Accept::Review, false, false), GoldenAction::Report);
        assert_eq!(golden_action(Accept::Review, true, true), GoldenAction::Keep);
        assert_eq!(Accept::parse(Some("review")), Accept::Review);
    }
}
