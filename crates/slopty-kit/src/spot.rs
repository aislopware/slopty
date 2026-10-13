//! Whether a view is what the window shows at a point.
//!
//! A drag from another app reaches the window as a point, never as a mouse event an element
//! hears. A [`Spot`] laid over a view's body takes a hitbox each frame the body is drawn,
//! retained frames included, and [`Spot::at`] asks the frame the window last drew whether that
//! hitbox is the top one there: a view not drawn, or one under a menu, is not at the point.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Hitbox, HitboxBehavior, IntoElement, ParentElement as _, Pixels, Point, Styled as _, Window,
    canvas, div,
};

/// A body's place in the frame the window last drew.
#[derive(Clone, Debug, Default)]
pub struct Spot(Rc<RefCell<Option<Hitbox>>>);

impl Spot {
    /// The element that takes the body's place: laid over it, filling it.
    pub fn element(&self) -> impl IntoElement + use<> {
        let slot = Rc::clone(&self.0);
        div().absolute().inset_0().child(
            canvas(
                move |bounds, window, _cx| {
                    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                    *slot.borrow_mut() = Some(hitbox);
                },
                |_, (), _, _| {},
            )
            .size_full(),
        )
    }

    /// Whether the body is the top of what the window last drew at `position`.
    #[must_use]
    pub fn at(&self, position: Point<Pixels>, window: &Window) -> bool {
        self.0.borrow().as_ref().is_some_and(|hitbox| hitbox.is_hovered_at(position, window))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, InteractiveElement as _, Render, TestAppContext, point, px};

    use super::*;

    struct Body {
        spot: Spot,
        covered: bool,
    }

    impl Render for Body {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let cover = self
                .covered
                .then(|| div().id("menu").absolute().top_0().left_0().size(px(50.0)).occlude());
            div().size(px(200.0)).relative().child(self.spot.element()).children(cover)
        }
    }

    /// The body is at a point inside it, not outside it, and not under what covers it.
    #[gpui::test]
    fn a_body_is_at_a_point_it_shows(cx: &mut TestAppContext) {
        let spot = Spot::default();
        let shown = spot.clone();
        let (view, cx) = cx.add_window_view(|_, _| Body { spot: shown, covered: true });
        cx.run_until_parked();
        let at = |cx: &mut gpui::VisualTestContext, x: f32, y: f32| {
            cx.update(|window, _| spot.at(point(px(x), px(y)), window))
        };
        assert!(at(cx, 100.0, 100.0), "inside");
        assert!(!at(cx, 300.0, 100.0), "outside");
        assert!(!at(cx, 10.0, 10.0), "under the menu");
        view.update(cx, |body, cx| {
            body.covered = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(at(cx, 10.0, 10.0), "the menu gone");
    }
}
