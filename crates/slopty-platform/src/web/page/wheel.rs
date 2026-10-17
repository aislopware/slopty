//! A live page's wheel on the Mac, handed on as a browser hands on a nested scroll: the page
//! scrolls while it can, and what it cannot take goes to the window's GPUI view, which scrolls
//! the chat under the card (`docs/decisions/platform.md`, "Pages run in a sandbox of their own").
//!
//! Two doors, both `WebKit`'s own word on the wheel, so nothing is guessed in a script and no
//! wheel waits on one:
//! - **What `WebKit` did not take.** `-[WKUIDelegatePrivate _webView:didNotHandleWheelEvent:]`
//!   (`WKUIDelegatePrivate.h`; `WebPageProxy::wheelEventHandlingCompleted`) hands back each wheel
//!   event that neither the scrolling tree nor the page's process used: a page with nothing to
//!   scroll, one at its edge in the wheel's direction, the view's frame and the host document
//!   alike, momentum included. The delegate passes it on ([`pass_on`]).
//! - **A gesture that began elsewhere.** A swipe that began over the chat stays the chat's when a
//!   card comes under the pointer, as a browser latches a scroll to where it began: the web view
//!   ([`PageWebView`]) hands on the events of a gesture whose start it never saw, without showing
//!   them to `WebKit` ([`Gesture`]).
//!
//! `WebKit` may fold wheel events that queue while one is in flight into one, and hands back
//! only the last of them when that one goes unused; at the trackpad's rate a reply comes before
//! the next event, so none is lost in practice.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol as _};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventPhase, NSResponder, NSView};
use objc2_core_foundation::CGRect;
use objc2_foundation::NSObject;
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};

/// The class of the view GPUI draws a window into (`gpui_macos`'s `window.rs`), a sibling of
/// the containers its natives sit in.
const GPUI_VIEW: &std::ffi::CStr = c"GPUIView";

/// Where a wheel event goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// To `WebKit`, which hands back what it does not take.
    Page,
    /// Straight on to the window's GPUI view.
    Window,
}

/// How the trackpad gesture the view last heard stands, as far as this view saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Gesture {
    /// It began somewhere else, or ended.
    #[default]
    Elsewhere,
    /// Fingers that touched down over the page are on the trackpad.
    Touching,
    /// They lifted; the momentum may follow.
    Lifted,
    /// The momentum of a gesture that began here.
    Coasting,
}

impl Gesture {
    /// Where an event of `phase` and `momentum` goes, and how the gesture stands after it. A
    /// wheel's notch has neither phase and is the page's, like a gesture that begins over it;
    /// the rest of a gesture goes where its start went.
    pub(super) fn route(self, phase: NSEventPhase, momentum: NSEventPhase) -> (Self, Route) {
        let began = NSEventPhase::Began | NSEventPhase::MayBegin;
        let moved = NSEventPhase::Changed | NSEventPhase::Stationary;
        let ended = NSEventPhase::Ended | NSEventPhase::Cancelled;
        let ours = |from: Self, then: Self| {
            if self == from { (then, Route::Page) } else { (Self::Elsewhere, Route::Window) }
        };
        if momentum.is_empty() {
            if phase.intersects(began) {
                (Self::Touching, Route::Page)
            } else if phase.intersects(moved) {
                ours(Self::Touching, Self::Touching)
            } else if phase.intersects(ended) {
                ours(Self::Touching, Self::Lifted)
            } else {
                (self, Route::Page)
            }
        } else if momentum.intersects(NSEventPhase::Began) {
            ours(Self::Lifted, Self::Coasting)
        } else if momentum.intersects(moved) {
            ours(Self::Coasting, Self::Coasting)
        } else if momentum.intersects(ended) {
            ours(Self::Coasting, Self::Elsewhere)
        } else {
            (self, Route::Page)
        }
    }
}

define_class!(
    // SAFETY:
    // - `WKWebView` may be subclassed; its designated initialiser is
    //   `initWithFrame:configuration:`, which `PageWebView::new` calls with the ivars set.
    // - `PageWebView` does not implement `Drop`.
    #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyPageWebView"]
    #[ivars = Cell<Gesture>]
    pub(super) struct PageWebView;

    impl PageWebView {
        /// A wheel event over the page: `WebKit`'s, unless its gesture began elsewhere.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (gesture, route) = self.ivars().get().route(event.phase(), event.momentumPhase());
            self.ivars().set(gesture);
            match route {
                // SAFETY: AppKit rule: `scrollWheel:` is `NSResponder`'s, taking the event.
                Route::Page => unsafe {
                    let () = msg_send![super(self), scrollWheel: event];
                },
                Route::Window => pass_on(self, event),
            }
        }
    }
);

impl PageWebView {
    /// A page's web view, `frame` big, made with `config`.
    pub(super) fn new(
        frame: CGRect,
        config: &WKWebViewConfiguration,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(Gesture::Elsewhere));
        // SAFETY: WebKit rule: `initWithFrame:configuration:` is `WKWebView`'s designated
        // initialiser, which copies the configuration.
        unsafe { msg_send![super(this), initWithFrame: frame, configuration: config] }
    }
}

/// Hand `event`, a wheel event over the page `web`, to the GPUI view of its window: the one
/// beside the container `web` is placed in. Nothing while `web` is in no window.
pub(super) fn pass_on(web: &AnyObject, event: &NSEvent) {
    let Some(class) = AnyClass::get(GPUI_VIEW) else { return };
    // SAFETY: AppKit rule: `superview` is any view's, nil outside a view; the page's view is
    // an `NSView` (a `WKWebView`).
    let container: Option<Retained<NSView>> = unsafe { msg_send![web, superview] };
    // SAFETY: as above; the hierarchy is the main thread's and stays put for the call.
    let Some(content) = container.and_then(|container| unsafe { container.superview() }) else {
        return;
    };
    let Some(gpui) = content.subviews().iter().find(|view| view.isKindOfClass(class)) else {
        return;
    };
    // SAFETY: AppKit rule: `scrollWheel:` is `NSResponder`'s, taking a wheel event; GPUI's view
    // reads its position in the window, as for an event AppKit sent it.
    let () = unsafe { msg_send![&*gpui, scrollWheel: event] };
}

#[cfg(test)]
mod tests {
    use objc2::{ClassType as _, sel};

    use super::*;

    const NONE: NSEventPhase = NSEventPhase::None;
    const BEGAN: NSEventPhase = NSEventPhase::Began;
    const MAY_BEGIN: NSEventPhase = NSEventPhase::MayBegin;
    const CHANGED: NSEventPhase = NSEventPhase::Changed;
    const ENDED: NSEventPhase = NSEventPhase::Ended;
    const CANCELLED: NSEventPhase = NSEventPhase::Cancelled;

    /// Where each event of a run goes, from a gesture that began nowhere.
    fn routes(events: &[(NSEventPhase, NSEventPhase)]) -> Vec<Route> {
        let mut gesture = Gesture::default();
        events
            .iter()
            .map(|&(phase, momentum)| {
                let (next, route) = gesture.route(phase, momentum);
                gesture = next;
                route
            })
            .collect()
    }

    /// A swipe that begins over the page is the page's to the end of its momentum: `WebKit`
    /// hands back what it cannot take.
    #[test]
    fn a_swipe_begun_over_the_page_is_the_pages() {
        let swipe =
            [(MAY_BEGIN, NONE), (BEGAN, NONE), (CHANGED, NONE), (CHANGED, NONE), (ENDED, NONE)];
        let momentum = [(NONE, BEGAN), (NONE, CHANGED), (NONE, CHANGED), (NONE, ENDED)];
        let all: Vec<_> = swipe.iter().chain(&momentum).copied().collect();
        assert_eq!(routes(&all), vec![Route::Page; all.len()]);
    }

    /// A swipe begun over the chat stays the chat's when the page comes under the pointer,
    /// fingers or momentum; the next one begun over the page is the page's again.
    #[test]
    fn a_swipe_begun_elsewhere_goes_on_to_the_window() {
        assert_eq!(
            routes(&[
                (CHANGED, NONE),
                (CHANGED, NONE),
                (ENDED, NONE),
                (NONE, BEGAN),
                (NONE, CHANGED),
                (NONE, ENDED)
            ]),
            vec![Route::Window; 6],
        );
        assert_eq!(
            routes(&[(NONE, CHANGED), (NONE, ENDED)]),
            vec![Route::Window; 2],
            "momentum alone"
        );
        assert_eq!(
            routes(&[(NONE, CHANGED), (BEGAN, NONE), (CHANGED, NONE)]),
            vec![Route::Window, Route::Page, Route::Page],
        );
    }

    /// A gesture that ended here without momentum leaves the next one, begun elsewhere, to the
    /// window; fingers that rest and lift without scrolling leave nothing behind.
    #[test]
    fn a_gesture_ended_here_does_not_hold_the_next() {
        assert_eq!(
            routes(&[(BEGAN, NONE), (ENDED, NONE), (CHANGED, NONE), (NONE, BEGAN)]),
            vec![Route::Page, Route::Page, Route::Window, Route::Window],
        );
        assert_eq!(
            routes(&[(MAY_BEGIN, NONE), (CANCELLED, NONE), (NONE, BEGAN)]),
            vec![Route::Page, Route::Page, Route::Page],
            "a cancelled touch lifts like an ended one",
        );
    }

    /// A mouse wheel's notches have no gesture: each is the page's, whatever came before.
    #[test]
    fn a_wheels_notch_is_the_pages() {
        assert_eq!(routes(&[(NONE, NONE), (NONE, NONE)]), vec![Route::Page; 2]);
        assert_eq!(routes(&[(CHANGED, NONE), (NONE, NONE)]), vec![Route::Window, Route::Page]);
    }

    /// The view is a `WKWebView` that answers the wheel itself.
    #[test]
    fn the_page_view_takes_the_wheel() {
        let class = PageWebView::class();
        assert!(class.instance_method(sel!(scrollWheel:)).is_some());
        assert_eq!(class.superclass().map(AnyClass::name), Some(c"WKWebView"));
    }
}
