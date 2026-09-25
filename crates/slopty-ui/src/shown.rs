//! When a paint reaches the display.
//!
//! A paint runs a refresh or more before its frame is on the glass: presentation is
//! vsync-synced and macOS composites a window about a refresh after its drawable is submitted
//! (MEASUREMENTS, "submit to glass"). A keystroke or a video frame timed at its paint, or at
//! the next display tick, reads low by that much. [`after_paint`] queues work to run once the
//! first frame holding the paint reached the display, with when that frame was submitted and
//! shown ([`Shown`]), from the window's own presentation reports (`Window::on_frame_presented`).
//! The report subscription lives only while something waits: reporting costs a little on every
//! frame.
//!
//! The iOS simulator reports no presentation, so there the next display tick stands in.

use std::time::Instant;

use gpui::{App, Window};

/// Where one paint went: when it ran, when the frame holding it was handed to the GPU, and
/// when that frame reached the display.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shown {
    /// The paint.
    pub painted: Instant,
    /// The first frame after the paint was submitted.
    pub submitted: Instant,
    /// That frame was on the glass.
    pub presented: Instant,
}

/// Run `then` with where the frame holding what is being painted now went, once it reached
/// the display. Call it from a paint.
#[cfg(all(target_os = "ios", target_abi = "sim"))]
pub fn after_paint(window: &Window, _cx: &mut App, then: impl FnOnce(Shown, &mut App) + 'static) {
    let painted = Instant::now();
    window.on_next_frame(move |_window, cx| {
        let now = Instant::now();
        then(Shown { painted, submitted: now, presented: now }, cx);
    });
}

/// Run `then` with where the frame holding what is being painted now went, once it reached
/// the display. Call it from a paint.
#[cfg(not(all(target_os = "ios", target_abi = "sim")))]
pub fn after_paint(window: &Window, cx: &mut App, then: impl FnOnce(Shown, &mut App) + 'static) {
    let painted = Instant::now();
    let waiting = cx.default_global::<Waiting>();
    waiting.queue.push((painted, Box::new(then)));
    if waiting.reports.is_none() {
        let reports = window.on_frame_presented(|frame, _window, cx| presented(frame, cx));
        cx.global_mut::<Waiting>().reports = Some(reports);
    }
}

#[cfg(not(all(target_os = "ios", target_abi = "sim")))]
type Waiter = Box<dyn FnOnce(Shown, &mut App)>;

/// Work waiting for its paint to be shown, oldest first, and the subscription that feeds it.
#[cfg(not(all(target_os = "ios", target_abi = "sim")))]
#[derive(Default)]
struct Waiting {
    queue: Vec<(Instant, Waiter)>,
    reports: Option<gpui::Subscription>,
}

#[cfg(not(all(target_os = "ios", target_abi = "sim")))]
impl gpui::Global for Waiting {}

/// A frame of the window was shown (or dropped unshown): run what its paints were waiting for.
/// The window's presentation reports call it; tests call it for the platform.
#[cfg(not(all(target_os = "ios", target_abi = "sim")))]
pub(crate) fn presented(frame: gpui::PresentedFrame, cx: &mut App) {
    // A frame dropped for a newer one holds nothing the newer one does not.
    let Some(at) = frame.presented_at else { return };
    let waiting = cx.default_global::<Waiting>();
    // The frame holds every paint made before it was handed to the GPU.
    let split = waiting.queue.partition_point(|(painted, _)| *painted <= frame.submitted_at);
    let ready: Vec<(Instant, Waiter)> = waiting.queue.drain(..split).collect();
    if waiting.queue.is_empty() {
        waiting.reports = None;
    }
    for (painted, then) in ready {
        then(Shown { painted, submitted: frame.submitted_at, presented: at }, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use gpui::{Empty, IntoElement, PresentedFrame, Render, TestAppContext};

    use super::*;

    struct Blank;

    impl Render for Blank {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            Empty
        }
    }

    /// A paint waits for the first frame submitted after it that reached the glass: an
    /// earlier frame and a frame dropped unshown do not count, and once nothing waits the
    /// window's reports are let go.
    #[gpui::test]
    fn a_paint_is_timed_at_the_first_frame_after_it_that_is_shown(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, _| Blank);
        let before = Instant::now();
        let seen = Rc::new(RefCell::new(Vec::new()));
        window
            .update(cx, |_, window, cx| {
                let seen = Rc::clone(&seen);
                after_paint(window, cx, move |shown, _| seen.borrow_mut().push(shown.presented));
            })
            .unwrap();
        let after = Instant::now().checked_add(Duration::from_millis(1)).unwrap();
        let glass = after.checked_add(Duration::from_millis(16)).unwrap();
        let listening = |cx: &mut TestAppContext| {
            cx.update(|cx| cx.try_global::<Waiting>().is_some_and(|w| w.reports.is_some()))
        };
        assert!(listening(cx), "the window reports frames while a paint waits");

        let earlier = PresentedFrame { submitted_at: before, presented_at: Some(before) };
        cx.update(|cx| presented(earlier, cx));
        assert!(seen.borrow().is_empty(), "a frame submitted before the paint holds none of it");

        let dropped = PresentedFrame { submitted_at: after, presented_at: None };
        cx.update(|cx| presented(dropped, cx));
        assert!(seen.borrow().is_empty(), "a frame dropped unshown showed nothing");

        let shown = PresentedFrame { submitted_at: after, presented_at: Some(glass) };
        cx.update(|cx| presented(shown, cx));
        assert_eq!(*seen.borrow(), [glass], "timed at the glass");
        assert!(!listening(cx), "with nothing waiting the reports are let go");

        cx.update(|cx| presented(shown, cx));
        assert_eq!(seen.borrow().len(), 1, "nothing runs twice");
    }
}
