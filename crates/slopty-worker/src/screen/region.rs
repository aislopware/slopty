//! A zoomed picture's region (`Quality::region`): what the capture samples, and which region
//! each capture shows.
//!
//! The region is in the target's native pixels on the wire and in its points where
//! ScreenCaptureKit takes it (`CaptureConfig::region`). A change of region is a configuration
//! update like a window's move on the crop path. ScreenCaptureKit takes it some milliseconds
//! after it is asked (its completion handler), and a capture whose display time falls between
//! the two may show either region. [`RegionClock`] says which region a capture shows from its
//! display time, and that it cannot tell for one in between, which is then not sent: a frame
//! placed by the wrong region would draw the old picture in the new place for a frame.

use slopty_capture::Crop;
use slopty_proto::screen::Region;

/// The region of the target in its own points, as `CaptureConfig::region` takes it, for a
/// target of `point_scale` pixels a point.
#[must_use]
pub(super) fn to_points(region: Region, point_scale: f64) -> Crop {
    let scale = if point_scale > 0.0 { point_scale } else { 1.0 };
    Crop {
        x: f64::from(region.x) / scale,
        y: f64::from(region.y) / scale,
        w: f64::from(region.w) / scale,
        h: f64::from(region.h) / scale,
    }
}

/// The region a configuration's points stand for, back in the target's native pixels: the one
/// [`to_points`] made them from.
#[must_use]
pub(super) fn to_pixels(points: Crop, point_scale: f64) -> Region {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
    let px = |v: f64| (v * point_scale).round().clamp(0.0, f64::from(u16::MAX)) as u16;
    Region { x: px(points.x), y: px(points.y), w: px(points.w), h: px(points.h) }
}

/// `region` in one word, for an atomic: zero for the whole target.
#[must_use]
pub(super) fn pack(region: Option<Region>) -> u64 {
    region.map_or(0, |Region { x, y, w, h }| {
        u64::from(x) | (u64::from(y) << 16) | (u64::from(w) << 32) | (u64::from(h) << 48)
    })
}

/// The region [`pack`] made `word` from.
#[must_use]
pub(super) fn unpack(word: u64) -> Option<Region> {
    let part = |shift: u32| u16::try_from(word.checked_shr(shift).unwrap_or(0) & 0xffff).ok();
    let region = Region { x: part(0)?, y: part(16)?, w: part(32)?, h: part(48)? };
    (region.w > 0 && region.h > 0).then_some(region)
}

/// What a capture shows, by its display time ([`RegionClock::shows`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shows {
    /// This region of the target; `None` for all of it.
    Region(Option<Region>),
    /// The old region or the new one: it was displayed while a change was on its way.
    Either,
}

/// Which region the captures of a stream show, by their display time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct RegionClock {
    /// The region shown before the change in force was asked.
    before: Option<Region>,
    /// The region last asked for.
    after: Option<Region>,
    /// When the change in force was asked, on the capture clock.
    asked_us: u64,
    /// When ScreenCaptureKit said it took it; `None` while it has not.
    taken_us: Option<u64>,
    /// Asks not answered yet: the region is known again once every one of them is.
    waiting: u8,
}

impl RegionClock {
    /// A stream started showing `region`: every capture shows it.
    pub(super) const fn steady(region: Option<Region>) -> Self {
        Self { before: region, after: region, asked_us: 0, taken_us: Some(0), waiting: 0 }
    }

    /// A configuration showing `region` was asked of the capture at `now_us`. Asking for the
    /// region already asked for, with nothing on its way, changes nothing. A change asked while
    /// another is still on its way keeps the earlier ask's time, as either may still be what a
    /// capture shows, and the region is known again once both are answered.
    pub(super) fn asked(&mut self, region: Option<Region>, now_us: u64) {
        if region == self.after && self.waiting == 0 {
            return;
        }
        if self.taken_us.is_some() {
            self.before = self.after;
            self.asked_us = now_us;
        }
        self.after = region;
        self.taken_us = None;
        self.waiting = self.waiting.saturating_add(1);
    }

    /// ScreenCaptureKit answered an ask at `now_us`: took it (`ok`), or refused it. Once every
    /// ask is answered, the captures from then on show the last region asked for, or the one
    /// they showed before when any ask was refused (the configuration is then the old one, or
    /// one the next geometry tick asks for again).
    pub(super) fn answered(&mut self, ok: bool, now_us: u64) {
        if self.waiting == 0 {
            return;
        }
        self.waiting = self.waiting.saturating_sub(1);
        if !ok {
            self.after = self.before;
        }
        if self.waiting == 0 {
            self.taken_us = Some(now_us.max(self.asked_us));
        }
    }

    /// The region a capture displayed at `captured_us` shows.
    pub(super) const fn shows(&self, captured_us: u64) -> Shows {
        match self.taken_us {
            Some(taken) if captured_us >= taken => Shows::Region(self.after),
            _ if captured_us < self.asked_us => Shows::Region(self.before),
            _ => Shows::Either,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Region = Region { x: 0, y: 0, w: 640, h: 360 };
    const B: Region = Region { x: 320, y: 180, w: 640, h: 360 };

    /// A capture before the ask shows the old region, one after the answer the new, and one in
    /// between is not told; a refused change leaves the old region on every capture.
    #[test]
    fn a_capture_shows_the_region_of_its_time() {
        let mut clock = RegionClock::steady(Some(A));
        assert_eq!(clock.shows(5), Shows::Region(Some(A)));
        clock.asked(Some(B), 100);
        assert_eq!(clock.shows(99), Shows::Region(Some(A)), "before the ask");
        assert_eq!(clock.shows(100), Shows::Either, "on its way");
        assert_eq!(clock.shows(10_000), Shows::Either, "still on its way");
        clock.answered(true, 120);
        assert_eq!(clock.shows(110), Shows::Either, "between the ask and the answer");
        assert_eq!(clock.shows(120), Shows::Region(Some(B)));
        assert_eq!(clock.shows(50), Shows::Region(Some(A)), "a late capture of before");

        clock.asked(None, 200);
        clock.answered(false, 230);
        assert_eq!(clock.shows(199), Shows::Region(Some(B)));
        assert_eq!(clock.shows(210), Shows::Either, "on its way until refused");
        assert_eq!(clock.shows(300), Shows::Region(Some(B)), "refused: the old region");
    }

    /// Asking again for the region asked for is no change, and a second change on top of one
    /// on its way keeps the first ask's time: a capture after it may show any of the three.
    #[test]
    fn a_change_on_top_of_one_on_its_way_waits_for_both() {
        let mut clock = RegionClock::steady(None);
        clock.asked(None, 50);
        assert_eq!(clock.shows(60), Shows::Region(None), "no change asked");
        clock.asked(Some(A), 100);
        clock.asked(Some(B), 140);
        assert_eq!(clock.shows(99), Shows::Region(None));
        assert_eq!(clock.shows(130), Shows::Either);
        clock.answered(true, 160);
        assert_eq!(clock.shows(170), Shows::Either, "the second ask is still on its way");
        clock.answered(true, 180);
        assert_eq!(clock.shows(170), Shows::Either);
        assert_eq!(clock.shows(180), Shows::Region(Some(B)));
        clock.answered(true, 400);
        assert_eq!(
            clock.shows(200),
            Shows::Region(Some(B)),
            "an answer nobody waits for moves nothing"
        );
    }

    /// A region packs into a word and back; the whole target is zero.
    #[test]
    fn a_region_packs_into_a_word() {
        for region in [None, Some(A), Some(B), Some(Region { x: 1, y: 2, w: 3, h: u16::MAX })] {
            assert_eq!(unpack(pack(region)), region);
        }
        assert_eq!(pack(None), 0);
    }

    /// Points and pixels go back and forth exactly at the scales a Mac has.
    #[test]
    fn a_region_survives_its_points() {
        let region = Region { x: 1681, y: 563, w: 1757, h: 989 };
        for scale in [1.0, 2.0, 3.0] {
            assert_eq!(to_pixels(to_points(region, scale), scale), region, "{scale}");
        }
        let points = to_points(B, 2.0);
        assert_eq!((points.x, points.y, points.w, points.h), (160.0, 90.0, 320.0, 180.0));
    }
}
