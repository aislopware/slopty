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
pub const CHANNEL_SLACK: u8 = 24;

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

/// Pixel-wise comparison; `diff` gets every differing pixel painted red on a faded copy of
/// `actual`.
#[must_use]
pub fn compare(actual: &RgbaImage, golden: &RgbaImage) -> (Diff, RgbaImage) {
    let (w, h) = actual.dimensions();
    let mut diff = RgbaImage::new(w, h);
    let mut differing = 0_u64;
    for (x, y, pixel) in actual.enumerate_pixels() {
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
    (Diff { differing, total: u64::from(w).saturating_mul(u64::from(h)) }, diff)
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
}

impl Accept {
    /// Parse the accept mode from an environment variable value.
    #[must_use]
    pub fn parse(val: Option<&str>) -> Self {
        match val {
            Some("1" | "changed") => Self::Changed,
            Some("all") => Self::All,
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
}

/// Decide what action to take for a golden snapshot.
#[must_use]
pub const fn golden_action(
    accept: Accept,
    golden_exists: bool,
    within_tolerance: bool,
) -> GoldenAction {
    if !golden_exists {
        return GoldenAction::Write;
    }
    match (accept, within_tolerance) {
        (Accept::All, _) | (Accept::Changed, false) => GoldenAction::Write,
        (Accept::Changed | Accept::Off, true) => GoldenAction::Keep,
        (Accept::Off, false) => GoldenAction::Fail,
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
#[expect(clippy::print_stderr, reason = "test helper; stderr is the test log")]
pub fn assert_matches(
    name: &str,
    actual: &RgbaImage,
    tolerance: f64,
    artifacts: &Path,
) -> Result<Diff> {
    let golden_path = golden_dir().join(format!("{name}.png"));
    std::fs::create_dir_all(artifacts)?;
    let actual_path = artifacts.join(format!("{name}.actual.png"));
    actual.save(&actual_path).with_context(|| format!("write {}", actual_path.display()))?;

    let accept = Accept::from_env();
    let golden_exists = golden_path.exists();
    if !golden_exists {
        let _action = golden_action(accept, false, false);
        std::fs::create_dir_all(golden_dir())?;
        actual.save(&golden_path).with_context(|| format!("write {}", golden_path.display()))?;
        eprintln!("snapshot {name}: wrote golden {}", golden_path.display());
        return Ok(Diff {
            differing: 0,
            total: u64::from(actual.width()).saturating_mul(u64::from(actual.height())),
        });
    }

    let golden = image::open(&golden_path)
        .with_context(|| format!("read {}", golden_path.display()))?
        .into_rgba8();
    let (diff, image) = compare(actual, &golden);
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
    }
}
