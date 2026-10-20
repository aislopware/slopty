// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The chosen wallpaper, decoded once off the UI thread and cached as a source-space image; a
//! separate alpha mask follows layout.
// slopty: the dither, ASCII, halftone and scanline effects are gone; the glyph field
// (`new_thread_background_field`) draws the hero when no wallpaper is chosen.
use crate::settings::NewThreadBackgroundAdjustment;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Hold the displayed artwork during loading, then crossfade to the ready image.
/// Finish each blend before adopting another image to keep rapid changes smooth.
#[derive(Default)]
pub(crate) struct Readiness {
    current: Option<Arc<gpui::RenderImage>>,
    previous: Option<Arc<gpui::RenderImage>>,
    current_adjustment: NewThreadBackgroundAdjustment,
    previous_adjustment: NewThreadBackgroundAdjustment,
    current_path: Option<PathBuf>,
    previous_path: Option<PathBuf>,
    started: Option<Instant>,
}

pub(crate) struct ArtworkFrame {
    pub current: Option<Arc<gpui::RenderImage>>,
    pub previous: Option<Arc<gpui::RenderImage>>,
    pub current_adjustment: NewThreadBackgroundAdjustment,
    pub previous_adjustment: NewThreadBackgroundAdjustment,
    pub mix: f32,
    pub active: bool,
}

fn crossfade_mix(start: Instant, now: Instant) -> f32 {
    let spec = crate::motion::WALLPAPER_CROSSFADE;
    let duration = spec.total().mul_f32(crate::motion::speed_scale());
    spec.progress(now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32())
}

impl Readiness {
    pub fn frame(
        &mut self,
        image: Option<Arc<gpui::RenderImage>>,
        path: Option<&Path>,
        adjustment: NewThreadBackgroundAdjustment,
        enabled: bool,
        reduced: bool,
        now: Instant,
    ) -> ArtworkFrame {
        let progress = self.started.map_or(1.0, |start| crossfade_mix(start, now));
        if reduced || progress >= 1.0 {
            self.previous = None;
            self.previous_path = None;
            self.started = None;
        }
        // The image may still be loading, but a crop belongs to the source path.
        // Replacement images keep their own framing.
        if path.is_some() && path == self.current_path.as_deref() {
            self.current_adjustment = adjustment.normalized();
        }
        if path.is_some() && path == self.previous_path.as_deref() {
            self.previous_adjustment = adjustment.normalized();
        }
        // None while enabled means "still loading", not "remove artwork".
        if self.started.is_none() && (!enabled || image.is_some()) {
            let target = if enabled { image } else { None };
            if self.current.as_ref().map(|image| image.id) != target.as_ref().map(|image| image.id)
            {
                self.previous = self.current.take();
                self.previous_adjustment = self.current_adjustment;
                self.previous_path = self.current_path.take();
                self.current = target;
                self.current_adjustment = adjustment.normalized();
                self.current_path = path.map(Path::to_path_buf);
                if reduced {
                    self.previous = None;
                    self.previous_path = None;
                } else {
                    self.started = Some(now);
                }
            }
        }
        ArtworkFrame {
            current: self.current.clone(),
            previous: self.previous.clone(),
            current_adjustment: self.current_adjustment,
            previous_adjustment: self.previous_adjustment,
            mix: self.started.map_or(1.0, |start| crossfade_mix(start, now)),
            active: self.started.is_some(),
        }
    }
}

type Pending = Arc<Mutex<Option<Arc<gpui::RenderImage>>>>;
type Cache = Vec<(PathBuf, Pending)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

/// The image the hero draws for a wallpaper's bytes: at most 2048 pt a side, in the BGRA order
/// `RenderImage` reads.
fn decode_artwork(bytes: &[u8]) -> Option<Arc<gpui::RenderImage>> {
    let image = crate::new_thread_background_image::decode(bytes).ok()?;
    let mut pixels = image.thumbnail(2048, 2048).to_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(gpui::RenderImage::new([image::Frame::new(pixels)])))
}

/// Safe to call on both routes: loading and decoding happen once off-thread, before the hero is
/// requested, and never depend on composer/sidebar geometry. `None` while the file decodes.
pub(super) fn prepare(path: &Path, cx: &mut gpui::App) -> Option<Arc<gpui::RenderImage>> {
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Some(pending) = cache
        .lock()
        .ok()?
        .iter()
        .find_map(|(key, pending)| (key == path).then(|| pending.clone()))
    {
        return pending.lock().ok()?.clone();
    }
    let pending = Pending::default();
    {
        let mut cache = cache.lock().ok()?;
        cache.push((path.to_path_buf(), pending.clone()));
        if cache.len() > 4 {
            cache.remove(0);
        }
    }
    let path = path.to_path_buf();
    cx.spawn(async move |cx| {
        let artwork = cx
            .background_executor()
            .spawn(async move { decode_artwork(&std::fs::read(path).ok()?) })
            .await;
        cx.update(|cx| {
            *pending.lock().unwrap() = artwork;
            cx.refresh_windows();
        });
    })
    .detach();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artwork() -> Arc<gpui::RenderImage> {
        Arc::new(gpui::RenderImage::new([image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }

    #[test]
    fn wallpaper_crossfade_holds_while_loading_and_handles_rapid_changes() {
        let first = artwork();
        let next = artwork();
        let latest = artwork();
        let mut ready = Readiness::default();
        let now = Instant::now();
        let path = Some(Path::new("background.png"));
        let adjustment = NewThreadBackgroundAdjustment::default();
        ready.frame(Some(first.clone()), path, adjustment, true, true, now);
        let loading = ready.frame(None, path, adjustment, true, false, now);
        assert_eq!(loading.current.unwrap().id, first.id);
        assert!(!loading.active);
        let start = ready.frame(Some(next.clone()), path, adjustment, true, false, now);
        assert_eq!(start.mix, 0.0);
        assert_eq!(start.previous.unwrap().id, first.id);
        let halfway = now
            + std::time::Duration::from_secs_f32(
                crate::motion::WALLPAPER_CROSSFADE.total().as_secs_f32()
                    * crate::motion::speed_scale()
                    * 0.5,
            );
        let frame = ready.frame(Some(latest.clone()), path, adjustment, true, false, halfway);
        assert!((frame.mix - 0.875).abs() < 0.001);
        assert_eq!(frame.current.unwrap().id, next.id);
        let done = now + std::time::Duration::from_secs(1);
        let frame = ready.frame(Some(latest.clone()), path, adjustment, true, false, done);
        assert_eq!(frame.previous.unwrap().id, next.id);
        assert_eq!(frame.current.unwrap().id, latest.id);
        assert_eq!(frame.mix, 0.0);
        let frame = ready.frame(Some(latest), path, adjustment, true, true, done);
        assert!(!frame.active);
        assert!(frame.previous.is_none());
        assert_eq!(frame.mix, 1.0);
        let removed = ready.frame(None, None, adjustment, false, true, done);
        assert!(removed.current.is_none() && removed.previous.is_none());
    }

    #[test]
    fn wallpaper_replacement_and_removal_keep_the_departing_crop() {
        let first = artwork();
        let next = artwork();
        let first_path = Some(Path::new("first.png"));
        let next_path = Some(Path::new("next.png"));
        let cropped = NewThreadBackgroundAdjustment {
            focal_x: 0.2,
            focal_y: 0.8,
            zoom: 2.0,
        };
        let centered = NewThreadBackgroundAdjustment::default();
        let mut ready = Readiness::default();
        let now = Instant::now();
        ready.frame(Some(first.clone()), first_path, cropped, true, true, now);
        let loading = ready.frame(None, next_path, centered, true, false, now);
        assert_eq!(loading.current.as_ref().unwrap().id, first.id);
        assert_eq!(loading.current_adjustment, cropped);
        let start = ready.frame(Some(next), next_path, centered, true, false, now);
        assert_eq!(start.previous_adjustment, cropped);
        assert_eq!(start.current_adjustment, centered);
        let latest = artwork();
        let latest_path = Some(Path::new("latest.png"));
        let waiting = ready.frame(Some(latest.clone()), latest_path, cropped, true, false, now);
        assert_eq!(waiting.previous_adjustment, cropped);
        assert_eq!(waiting.current_adjustment, centered);
        let done = now + std::time::Duration::from_secs(1);
        let adopted = ready.frame(Some(latest), latest_path, cropped, true, false, done);
        assert_eq!(adopted.previous_adjustment, centered);
        assert_eq!(adopted.current_adjustment, cropped);

        let mut ready = Readiness::default();
        ready.frame(Some(first), first_path, cropped, true, true, now);
        let removed = ready.frame(None, None, centered, false, false, now);
        assert!(removed.current.is_none());
        assert!(removed.previous.is_some());
        assert_eq!(removed.previous_adjustment, cropped);
    }

    #[test]
    fn applying_crop_during_loading_updates_the_same_source_without_a_fade() {
        let first = artwork();
        let loaded = artwork();
        let path = Some(Path::new("background.png"));
        let centered = NewThreadBackgroundAdjustment::default();
        let cropped = NewThreadBackgroundAdjustment {
            zoom: 2.0,
            ..centered
        };
        let latest = NewThreadBackgroundAdjustment {
            zoom: 3.0,
            ..centered
        };
        let mut ready = Readiness::default();
        let now = Instant::now();
        ready.frame(Some(first), path, centered, true, true, now);
        let loading = ready.frame(None, path, cropped, true, false, now);
        assert_eq!(loading.current_adjustment, cropped);
        assert!(!loading.active);
        let start = ready.frame(Some(loaded.clone()), path, cropped, true, false, now);
        assert_eq!(start.previous_adjustment, cropped);
        assert_eq!(start.current_adjustment, cropped);
        let applied = ready.frame(Some(loaded.clone()), path, latest, true, false, now);
        assert_eq!(applied.previous_adjustment, latest);
        assert_eq!(applied.current_adjustment, latest);
        let done = now + std::time::Duration::from_secs(1);
        let applied = ready.frame(Some(loaded), path, cropped, true, false, done);
        assert_eq!(applied.current_adjustment, cropped);
        assert!(!applied.active);
    }

    #[gpui::test]
    fn prewarming_decodes_off_thread_and_reuses_artwork_without_hero_geometry(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("background.png");
        image::RgbaImage::from_pixel(32, 24, image::Rgba([173, 89, 231, 180]))
            .save(&path)
            .unwrap();
        cx.update(|cx| assert!(prepare(&path, cx).is_none()));
        cx.update(|cx| assert!(prepare(&path, cx).is_none(), "one decode, still running"));
        cx.run_until_parked();
        cx.update(|cx| {
            let image = prepare(&path, cx).unwrap();
            assert_eq!(image.as_bytes(0).unwrap()[..4], [231, 89, 173, 180]);
            for _ in 0..20 {
                assert!(Arc::ptr_eq(&image, &prepare(&path, cx).unwrap()));
            }
        });
    }
}
