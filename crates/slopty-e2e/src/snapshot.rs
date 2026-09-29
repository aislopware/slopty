//! Golden renders: compare an app frame with the PNG under `golden/`, write the actual frame
//! and a diff image next to the artifacts when they differ.
//!
//! A frame counts as matching when at most `tolerance` of its pixels differ by more than
//! [`CHANNEL_SLACK`] in any channel: font hinting, the RTT readout and a blinking cursor
//! move a few hundred pixels, a broken layout moves a few hundred thousand. Pass `--accept`
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

/// Fraction of a Mac golden's pixels allowed to differ.
///
/// Two runs of every Mac golden, with the cursor held steady, differed by 0.051% at most
/// (`browser`; the round-trip figure and a page's anti-aliasing), so 0.2% is four times the noise.
/// At the old 1% a golden 1280 wide let ten thousand pixels through: a washed row and a word of
/// status text went unseen.
pub const MAC_TOLERANCE: f64 = 0.002;

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
            pixel.0.iter().zip(g.0.iter()).all(|(a, b)| a.abs_diff(*b) <= CHANNEL_SLACK)
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

/// Compare `actual` with `golden/<name>.png`.
///
/// When accepting, `--accept` writes missing and failing goldens, while `--accept-all`
/// rewrites every golden. Otherwise the frame is written to `<artifacts>/<name>.actual.png`
/// and, when it differs beyond `tolerance`, the diff to `<artifacts>/<name>.diff.png`, and the
/// error names both files.
///
/// # Errors
///
/// When the sizes differ, more than `tolerance` of the pixels differ, or a file cannot be
/// written.
pub fn assert_matches(
    name: &str,
    actual: &RgbaImage,
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
#[expect(clippy::print_stderr, reason = "test helper; stderr is the test log")]
pub fn assert_matches_masked(
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
