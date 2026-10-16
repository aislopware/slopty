//! Which of a window's pages are live, warm or cold, under the caps.
//!
//! A page is cold while it has no web view, warm while it has one GPUI does not place (GPUI
//! draws its snapshot), and live while GPUI places its view. Live pages are capped by the
//! device (three on a Mac, two on an iPad, one on an iPhone) and mounted ones, warm and live
//! together, at [`MOUNTED_CAP`]: each mounted page is a `WebContent` process of its own. The
//! page used least lately gives way first.

use super::PageId;

/// How a page is shown now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presence {
    /// No web view: its last snapshot, or its place, stands for it.
    Cold,
    /// A web view GPUI does not place: GPUI draws its last snapshot, which frosts, fades and
    /// clips as any image does.
    Warm,
    /// GPUI places its web view, which takes the pointer and the keyboard.
    Live,
}

/// What kind of device the window is on, which sets how many pages may be live at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Device {
    /// A Mac.
    Mac,
    /// An iPad.
    Pad,
    /// An iPhone.
    Phone,
}

impl Device {
    /// How many pages may be live at once in one window.
    #[must_use]
    pub const fn live_cap(self) -> usize {
        match self {
            Self::Mac => 3,
            Self::Pad => 2,
            Self::Phone => 1,
        }
    }

    /// What MCP Apps calls the platform (`hostContext.platform`).
    #[must_use]
    pub const fn platform(self) -> &'static str {
        match self {
            Self::Mac => "desktop",
            Self::Pad | Self::Phone => "mobile",
        }
    }

    /// Whether the device is used by touch, and whether it hovers
    /// (`hostContext.deviceCapabilities`).
    #[must_use]
    pub const fn touch_and_hover(self) -> (bool, bool) {
        match self {
            Self::Mac => (false, true),
            Self::Pad => (true, true),
            Self::Phone => (true, false),
        }
    }
}

/// How many pages one window keeps mounted at most, live and warm together.
pub const MOUNTED_CAP: usize = 8;

/// A page moved from one presence to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    /// Which page.
    pub page: PageId,
    /// Where it was.
    pub from: Presence,
    /// Where it is now.
    pub to: Presence,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    page: PageId,
    presence: Presence,
    /// When it was last asked for, on the stage's own clock.
    used: u64,
}

/// The presence of every page of a window.
#[derive(Debug)]
pub(crate) struct Stage {
    device: Device,
    slots: Vec<Slot>,
    clock: u64,
}

impl Stage {
    pub(crate) const fn new(device: Device) -> Self {
        Self { device, slots: Vec::new(), clock: 0 }
    }

    /// A page the stage knows from now on, cold.
    pub(crate) fn add(&mut self, page: PageId) {
        if self.slot(page).is_none() {
            let used = self.tick();
            self.slots.push(Slot { page, presence: Presence::Cold, used });
        }
    }

    /// Forget `page`; where it was.
    pub(crate) fn remove(&mut self, page: PageId) -> Option<Presence> {
        let at = self.slots.iter().position(|s| s.page == page)?;
        Some(self.slots.remove(at).presence)
    }

    pub(crate) fn presence(&self, page: PageId) -> Option<Presence> {
        self.slot(page).map(|s| s.presence)
    }

    /// How many pages are `presence` now.
    pub(crate) fn count(&self, presence: Presence) -> usize {
        self.slots.iter().filter(|s| s.presence == presence).count()
    }

    /// Give `page` a web view (cold to warm), cooling the page mounted least lately when that
    /// passes the cap. A page already mounted is only marked as used.
    pub(crate) fn mount(&mut self, page: PageId) -> Vec<Change> {
        let mut changes = Vec::new();
        match self.presence(page) {
            None => {}
            Some(Presence::Cold) => {
                self.set(page, Presence::Warm, &mut changes);
                self.enforce(page, &mut changes);
            }
            Some(Presence::Warm | Presence::Live) => self.touch(page),
        }
        changes
    }

    /// Make `page` live, mounting it first when it is cold; the live page used least lately
    /// goes warm when that passes the device's cap.
    pub(crate) fn engage(&mut self, page: PageId) -> Vec<Change> {
        let mut changes = Vec::new();
        if self.presence(page).is_some_and(|p| p != Presence::Live) {
            self.set(page, Presence::Live, &mut changes);
            self.enforce(page, &mut changes);
        } else {
            self.touch(page);
        }
        changes
    }

    /// A live page goes warm.
    pub(crate) fn rest(&mut self, page: PageId) -> Vec<Change> {
        let mut changes = Vec::new();
        if self.presence(page) == Some(Presence::Live) {
            self.set(page, Presence::Warm, &mut changes);
        }
        changes
    }

    /// Every live page goes warm: the app went to the background.
    pub(crate) fn rest_all(&mut self) -> Vec<Change> {
        let live: Vec<PageId> =
            self.slots.iter().filter(|s| s.presence == Presence::Live).map(|s| s.page).collect();
        let mut changes = Vec::new();
        for page in live {
            self.set(page, Presence::Warm, &mut changes);
        }
        changes
    }

    /// `page` loses its web view.
    pub(crate) fn unmount(&mut self, page: PageId) -> Vec<Change> {
        let mut changes = Vec::new();
        if self.presence(page).is_some_and(|p| p != Presence::Cold) {
            self.set(page, Presence::Cold, &mut changes);
        }
        changes
    }

    fn slot(&self, page: PageId) -> Option<&Slot> {
        self.slots.iter().find(|s| s.page == page)
    }

    const fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    fn touch(&mut self, page: PageId) {
        let used = self.tick();
        if let Some(slot) = self.slots.iter_mut().find(|s| s.page == page) {
            slot.used = used;
        }
    }

    fn set(&mut self, page: PageId, to: Presence, changes: &mut Vec<Change>) {
        let used = self.tick();
        if let Some(slot) = self.slots.iter_mut().find(|s| s.page == page) {
            let from = slot.presence;
            slot.presence = to;
            slot.used = used;
            if from != to {
                changes.push(Change { page, from, to });
            }
        }
    }

    /// Hold both caps, giving way from the page used least lately, never `keep`.
    fn enforce(&mut self, keep: PageId, changes: &mut Vec<Change>) {
        while self.count(Presence::Live) > self.device.live_cap() {
            let Some(page) = self.least_used(keep, |p| p == Presence::Live) else { break };
            self.set(page, Presence::Warm, changes);
        }
        while self.count(Presence::Warm).saturating_add(self.count(Presence::Live)) > MOUNTED_CAP {
            let Some(page) = self.least_used(keep, |p| p == Presence::Warm) else { break };
            self.set(page, Presence::Cold, changes);
        }
    }

    fn least_used(&self, keep: PageId, of: impl Fn(Presence) -> bool) -> Option<PageId> {
        self.slots
            .iter()
            .filter(|s| s.page != keep && of(s.presence))
            .min_by_key(|s| s.used)
            .map(|s| s.page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(n: u64) -> Vec<PageId> {
        (1..=n).map(PageId).collect()
    }

    /// Each device holds its own count of live pages; the next one engaged cools the one used
    /// least lately to warm, never to cold.
    #[test]
    fn live_pages_are_capped_per_device_and_the_least_used_rests() {
        for (device, cap) in [(Device::Mac, 3), (Device::Pad, 2), (Device::Phone, 1)] {
            let mut stage = Stage::new(device);
            let all = pages(5);
            for page in &all {
                stage.add(*page);
            }
            for page in &all {
                let _changes = stage.engage(*page);
            }
            assert_eq!(stage.count(Presence::Live), cap, "{device:?}");
            assert_eq!(stage.count(Presence::Warm), 5_usize.saturating_sub(cap), "{device:?}");
            assert_eq!(stage.presence(PageId(5)), Some(Presence::Live), "the last engaged");
            assert_eq!(stage.presence(PageId(1)), Some(Presence::Warm), "the first rests");
        }
    }

    /// Engaging a page names its own change first, then the page it cooled.
    #[test]
    fn an_engage_past_the_cap_names_what_it_cooled() {
        let mut stage = Stage::new(Device::Phone);
        stage.add(PageId(1));
        stage.add(PageId(2));
        assert_eq!(
            stage.engage(PageId(1)),
            [Change { page: PageId(1), from: Presence::Cold, to: Presence::Live }]
        );
        assert_eq!(
            stage.engage(PageId(2)),
            [
                Change { page: PageId(2), from: Presence::Cold, to: Presence::Live },
                Change { page: PageId(1), from: Presence::Live, to: Presence::Warm },
            ]
        );
        assert!(stage.engage(PageId(2)).is_empty(), "already live");
    }

    /// No more than eight pages are mounted; the ninth cools the warm page used least lately,
    /// and a live page is never the one cooled.
    #[test]
    fn at_most_eight_pages_are_mounted() {
        let mut stage = Stage::new(Device::Mac);
        let all = pages(12);
        for page in &all {
            stage.add(*page);
        }
        let _live = stage.engage(PageId(1));
        for page in all.iter().skip(1) {
            let _changes = stage.mount(*page);
        }
        let mounted = stage.count(Presence::Warm).saturating_add(stage.count(Presence::Live));
        assert_eq!(mounted, MOUNTED_CAP);
        assert_eq!(stage.presence(PageId(1)), Some(Presence::Live), "the live page stays");
        assert_eq!(stage.presence(PageId(2)), Some(Presence::Cold), "the oldest warm cooled");
        assert_eq!(stage.presence(PageId(12)), Some(Presence::Warm));
        assert_eq!(stage.count(Presence::Cold), 4);
    }

    /// Resting and unmounting move one page; the background rests every live page.
    #[test]
    fn rest_unmount_and_the_background() {
        let mut stage = Stage::new(Device::Mac);
        for page in pages(3) {
            stage.add(page);
            let _changes = stage.engage(page);
        }
        assert_eq!(stage.rest(PageId(1)).len(), 1);
        assert!(stage.rest(PageId(1)).is_empty(), "already warm");
        assert_eq!(stage.rest_all().len(), 2);
        assert_eq!(stage.count(Presence::Live), 0);
        assert_eq!(
            stage.unmount(PageId(2)),
            [Change { page: PageId(2), from: Presence::Warm, to: Presence::Cold }]
        );
        assert_eq!(stage.remove(PageId(2)), Some(Presence::Cold));
        assert_eq!(stage.presence(PageId(2)), None);
        assert!(stage.engage(PageId(9)).is_empty(), "a page the stage never knew");
    }
}
