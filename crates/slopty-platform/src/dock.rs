//! The Dock tile's progress bar: the programs' `OSC 9;4` reports, gathered into one bar under
//! the app icon, the way Finder and Safari show a copy or a download.
//!
//! The bar is the system's `NSProgressIndicator` drawn into the tile's content view, so it
//! looks like every other app's. The tile is a snapshot redrawn only when asked, so an
//! indeterminate bar stands still there; the tiles themselves carry the motion.

use slopty_proto::terminal::{Progress, ProgressState};

/// What the Dock tile's bar shows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DockProgress {
    /// Determinate, as the fraction done (0 to 1).
    Done(f64),
    /// Working with no measure of how far along.
    Busy,
}

impl DockProgress {
    /// One bar for every terminal's report, `None` when no program is reporting.
    ///
    /// The bar is the one furthest behind, so it fills only when everything is done. A
    /// running report without a figure, or a pause, counts as busy; a failure is left to the
    /// tile's own red bar and the finished-command note, since the Dock bar has no tone.
    #[must_use]
    pub fn gather(reports: impl IntoIterator<Item = Progress>) -> Option<Self> {
        let mut least: Option<u8> = None;
        let mut busy = false;
        for report in reports {
            match (report.state, report.percent) {
                (ProgressState::Set | ProgressState::Paused, Some(percent)) => {
                    let percent = percent.min(100);
                    least = Some(least.map_or(percent, |seen| seen.min(percent)));
                }
                (ProgressState::Set | ProgressState::Paused | ProgressState::Indeterminate, _) => {
                    busy = true;
                }
                (ProgressState::None | ProgressState::Error, _) => {}
            }
        }
        match least {
            Some(percent) => Some(Self::Done(f64::from(percent) / 100.0)),
            None => busy.then_some(Self::Busy),
        }
    }
}

/// Show `progress` under the Dock icon, or take the bar away with `None`.
///
/// Main thread only (AppKit); called from GPUI's main-thread callbacks. Off it, and on iOS,
/// this is a no-op: the iPhone's equivalent is a Live Activity, which needs an extension.
#[cfg_attr(target_os = "ios", expect(clippy::missing_const_for_fn, reason = "a no-op here"))]
pub fn set_progress(progress: Option<DockProgress>) {
    #[cfg(target_os = "macos")]
    macos::show(progress);
    #[cfg(target_os = "ios")]
    let _: Option<DockProgress> = progress;
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly as _};
    use objc2_app_kit::{
        NSApplication, NSImageView, NSProgressIndicator, NSProgressIndicatorStyle, NSView,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    use super::DockProgress;

    /// The share of the tile's width the bar spans, centred.
    const WIDTH: f64 = 0.8;
    /// The bar's height as a share of the tile's: the system's regular bar on a 128 pt tile.
    const HEIGHT: f64 = 0.14;
    /// How far above the tile's bottom edge the bar sits, as a share of the tile's height.
    const LIFT: f64 = 0.06;

    thread_local! {
        /// The bar while the tile shows one; AppKit objects stay on the main thread.
        static BAR: RefCell<Option<Retained<NSProgressIndicator>>> = const { RefCell::new(None) };
    }

    pub(super) fn show(progress: Option<DockProgress>) {
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!(?progress, "dock progress skipped: not on the main thread");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let tile = app.dockTile();
        BAR.with_borrow_mut(|bar| {
            let Some(progress) = progress else {
                if bar.take().is_some() {
                    tile.setContentView(None);
                    tile.display();
                }
                return;
            };
            let indicator = bar.get_or_insert_with(|| {
                let size = tile.size();
                let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
                let icon = app.applicationIconImage();
                let content: Retained<NSView> = match icon {
                    Some(icon) => {
                        let view = NSImageView::imageViewWithImage(&icon, mtm);
                        view.setFrame(frame);
                        Retained::into_super(Retained::into_super(view))
                    }
                    None => NSView::initWithFrame(NSView::alloc(mtm), frame),
                };
                let bar = NSRect::new(
                    NSPoint::new(size.width * (1.0 - WIDTH) / 2.0, size.height * LIFT),
                    NSSize::new(size.width * WIDTH, size.height * HEIGHT),
                );
                let indicator =
                    NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), bar);
                indicator.setStyle(NSProgressIndicatorStyle::Bar);
                indicator.setMinValue(0.0);
                indicator.setMaxValue(1.0);
                content.addSubview(&indicator);
                tile.setContentView(Some(&content));
                indicator
            });
            match progress {
                DockProgress::Done(done) => {
                    indicator.setIndeterminate(false);
                    indicator.setDoubleValue(done.clamp(0.0, 1.0));
                }
                DockProgress::Busy => indicator.setIndeterminate(true),
            }
            tile.display();
            tracing::trace!(?progress, "dock progress");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn report(state: ProgressState, percent: Option<u8>) -> Progress {
        Progress { state, percent }
    }

    #[test]
    fn nothing_reporting_takes_the_bar_away() {
        assert_eq!(DockProgress::gather([]), None);
        let quiet = [report(ProgressState::None, None), report(ProgressState::Error, Some(40))];
        assert_eq!(DockProgress::gather(quiet), None, "a failure is the tile's to show");
    }

    #[test]
    fn the_bar_is_the_report_furthest_behind() {
        let reports = [
            report(ProgressState::Set, Some(80)),
            report(ProgressState::Paused, Some(30)),
            report(ProgressState::Indeterminate, None),
            report(ProgressState::Set, Some(250)),
        ];
        assert_eq!(DockProgress::gather(reports), Some(DockProgress::Done(0.3)));
    }

    #[test]
    fn reports_without_a_figure_are_busy() {
        let reports =
            [report(ProgressState::Indeterminate, None), report(ProgressState::Set, None)];
        assert_eq!(DockProgress::gather(reports), Some(DockProgress::Busy));
    }
}
