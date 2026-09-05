//! Golden renders: compare an app frame with the PNG under `golden/`, write the actual frame
//! and a diff image next to the artifacts when they differ.
//!
//! A frame counts as matching when at most `tolerance` of its pixels differ by more than
//! [`CHANNEL_SLACK`] in any channel: font hinting, the RTT readout and a blinking cursor
//! move a few hundred pixels, a broken layout moves a few hundred thousand. Set
//! `SLOPTY_E2E_ACCEPT=1` (`cargo xtask e2e app --accept`) to (re)write the goldens.

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

/// Compare `actual` with `golden/<name>.png`.
///
/// Missing golden or `SLOPTY_E2E_ACCEPT=1`: the frame becomes the golden and the call
/// succeeds. Otherwise the frame is written to `<artifacts>/<name>.actual.png` and, when it
/// differs beyond `tolerance`, the diff to `<artifacts>/<name>.diff.png`, and the error names
/// both files.
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

    let accept = std::env::var_os("SLOPTY_E2E_ACCEPT").is_some_and(|v| v == "1");
    if accept || !golden_path.exists() {
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
    if golden.dimensions() != actual.dimensions() {
        bail!(
            "snapshot {name}: golden is {:?}, frame is {:?} (see {})",
            golden.dimensions(),
            actual.dimensions(),
            actual_path.display()
        );
    }
    let (diff, image) = compare(actual, &golden);
    let fraction = diff.fraction();
    eprintln!(
        "snapshot {name}: {} of {} pixels differ ({:.3}%)",
        diff.differing,
        diff.total,
        fraction * 100.0
    );
    if fraction > tolerance {
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
    Ok(diff)
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
}
