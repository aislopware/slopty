//! The jump pill at the foot of a transcript that no longer follows its end: an arrow,
//! and how many steps arrived below since it let go ("↓ 12 new steps"; `docs/decisions/
//! inspector.md`, "Follow and anchor"). A press goes back to the end, following it.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
};

use crate::motion::{self, AnimationExt as _, HoverFade as _};
use crate::theme::Theme;

// The jump button's shape and arrival (opencode's, `.research/opencode-study-2026-10-19.md`
// item 18): 32 × 28 with corners of 8, rising 8 px into place over 200 ms on an ease-out.
const JUMP_WIDTH: f32 = 32.0;
const JUMP_HEIGHT: f32 = 28.0;
const JUMP_RADIUS: f32 = 8.0;
const JUMP_RISE: f32 = 8.0;
// In on the quint ease-out over 200 ms, out over 300 ms on (0.7, 0, 0.84, 0), gathering
// speed as it sinks back the way it came (HextaUI's `MessageScrollerButton`;
// design-inspiration item 11). Its 0.95 scale is left out: a div has no scale here.
const JUMP_IN: motion::MotionSpec = motion::MotionSpec::new(200, motion::EASE_OUT_QUINT);
const JUMP_OUT: motion::MotionSpec =
    motion::MotionSpec::new(300, motion::CubicBezier::new(0.7, 0.0, 0.84, 0.0));
/// The arrow's side margins: with no count the button, its 1 px border with them, is
/// [`JUMP_WIDTH`] wide.
const JUMP_PAD: f32 = (JUMP_WIDTH - 16.0 - 2.0) / 2.0;

thread_local! {
    /// Each jump button's last showing, when it began to leave, and how many leaves it
    /// has begun.
    static JUMP_SHOWN: std::cell::RefCell<JumpMarks> = std::cell::RefCell::new(JumpMarks::new());
}

type JumpMarks = std::collections::HashMap<&'static str, (bool, Option<std::time::Instant>, u64)>;

/// Whether `key`'s jump button is drawn at `now`: `None` gone, `Some(None)` shown, and
/// `Some(Some(n))` leaving, `n` naming this leave so its animation plays once.
pub(super) fn presence(key: &'static str, shown: bool, now: std::time::Instant) -> Option<Option<u64>> {
    JUMP_SHOWN.with(|marks| {
        let mut marks = marks.borrow_mut();
        let (was, leave, leaves) = marks.entry(key).or_insert((false, None, 0));
        let result = if shown {
            *leave = None;
            Some(None)
        } else {
            if *was {
                *leave = Some(now);
                *leaves += 1;
            }
            let leaving = JUMP_OUT.total().mul_f32(motion::speed_scale());
            leave
                .filter(|at| now.saturating_duration_since(*at) < leaving)
                .map(|_| Some(*leaves - 1))
        };
        *was = shown;
        result
    })
}

/// The jump button leaving: faded out and sunk [`JUMP_RISE`] over [`JUMP_OUT`].
pub(super) fn leave(key: &'static str, leaving: Option<u64>, button: AnyElement) -> AnyElement {
    let Some(n) = leaving else {
        return button;
    };
    div()
        .child(button)
        .with_animation(
            gpui::ElementId::NamedInteger(SharedString::from(format!("{key}-out")), n),
            JUMP_OUT.animation(),
            |el, t| el.relative().opacity(1.0 - t).top(px(JUMP_RISE * t)),
        )
        .into_any_element()
}

/// The button that takes a transcript back to its end, `on_click`.
pub(super) fn button(
    anim_key: &'static str,
    hover_key: &'static str,
    unseen: usize,
    (reduced, now): (bool, std::time::Instant),
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    // The jump button is opencode's (`.research/opencode-study-2026-10-19.md` item 18): the
    // arrow alone, 32 × 28 with corners of 8, on the popover's frosted surface, lifted and
    // ringed by a halo of the page's own colour so the text scrolling under it never touches
    // its edge; it says what it does to a screen reader and in its tooltip.
    let theme = theme.for_popup();
    let glass = theme.is_frost();
    // On frost the hover's wash rides a layer of its own over the glass (a div has one fill).
    let wash = if glass {
        motion::hover_blend(hover_key, gpui::transparent_black(), theme.glass_hover())
    } else {
        gpui::transparent_black()
    };
    let page = if glass { theme.glass() } else { theme.surface };
    let button = div()
        .id(anim_key)
        .role(gpui::Role::Button)
        .aria_label("Scroll to bottom")
        .tooltip(crate::settings::widgets::text_tooltip("Scroll to bottom"))
        .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
        // As wide as the arrow and the count it says; with none, [`JUMP_WIDTH`].
        .min_w(px(JUMP_WIDTH))
        .h(px(JUMP_HEIGHT))
        .flex_none()
        .rounded(px(JUMP_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(if glass {
            crate::popover::surface_bg(&theme)
        } else {
            motion::hover_blend(hover_key, theme.surface_raised, theme.surface_raised_hover)
        })
        .when(!glass, |button| {
            let shadow = |color, y: f32, blur: f32, spread: f32| gpui::BoxShadow {
                color,
                offset: gpui::point(px(0.0), px(y)),
                blur_radius: px(blur),
                spread_radius: px(spread),
                inset: false,
            };
            // The halo first, so the lift's shade lies over it.
            button.shadow(vec![
                shadow(page, 2.0, 8.0, 0.0),
                shadow(gpui::hsla(0.0, 0.0, 0.0, 0.1), 4.0, 6.0, -1.0),
                shadow(gpui::hsla(0.0, 0.0, 0.0, 0.1), 2.0, 4.0, -2.0),
            ])
        })
        .hover_fade(hover_key)
        .cursor_pointer()
        .on_click(on_click)
        .child(
            div()
                .h_full()
                .px(px(JUMP_PAD))
                .rounded(px(JUMP_RADIUS - 1.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(wash)
                .child(crate::icons::icon(crate::icons::ARROW_DOWN).size(px(16.0)).text_color(theme.text_muted))
                // "↓ 3 new steps", the count in the accent as new content is, each part
                // rolled in and out (`roll_text`), the button's width gliding with them.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .text_size(crate::typography::ui_rems(12.5))
                        .child(div().text_color(theme.accent).child(crate::roll_text::roll_text(
                            format!("{anim_key}-count"),
                            if unseen > 0 { format!(" {unseen}").into() } else { SharedString::default() },
                            reduced,
                            now,
                        )))
                        .child(div().text_color(theme.text_muted).child(crate::roll_text::roll_text(
                            format!("{anim_key}-new"),
                            match unseen {
                                0 => SharedString::default(),
                                1 => " new step".into(),
                                _ => " new steps".into(),
                            },
                            reduced,
                            now,
                        ))),
                ),
        );
    let button = arrive(anim_key, button.debug_selector(move || anim_key.to_owned()));
    crate::frost::frosted(JUMP_RADIUS, crate::frost::MENU_BLUR, button).into_any_element()
}

/// The jump button arriving: faded in and risen [`JUMP_RISE`] into place.
fn arrive<E>(id: impl Into<gpui::ElementId>, element: E) -> gpui::AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, JUMP_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(JUMP_RISE * (1.0 - t)))
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use std::time::Duration;

    use super::*;
    use gpui::{Context, Render, TestAppContext};

    struct Host(Rc<std::cell::Cell<u32>>, usize);

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let clicks = Rc::clone(&self.0);
            let unseen = self.1;
            // As the transcript lays it out: a row that centres it at its own width.
            div().size_full().flex().justify_center().child(button("jump-test", "jump-test-hover", unseen, (true, std::time::Instant::now()), Theme::of(cx), move |_, _, _| {
                clicks.set(clicks.get() + 1);
            }))
        }
    }

    /// The jump to the end is the arrow alone on a 32 × 28 button (opencode's), named
    /// for a screen reader, and a click on it jumps.
    #[gpui::test]
    fn the_jump_button_is_an_arrow_that_jumps(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let clicks = Rc::new(std::cell::Cell::new(0));
        let host = Rc::clone(&clicks);
        let (view, cx) = cx.add_window_view(move |_, _| Host(host, 0));
        cx.run_until_parked();
        let bounds = cx.debug_bounds("jump-test").expect("the button");
        assert_eq!((f32::from(bounds.size.width), f32::from(bounds.size.height)), (JUMP_WIDTH, JUMP_HEIGHT));
        let named = cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            window
                .a11y_tree()
                .is_some_and(|tree| tree.nodes.iter().any(|(_, node)| node.label() == Some("Scroll to bottom")))
        });
        assert!(named, "named for a screen reader");
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        assert_eq!(clicks.get(), 1, "a click jumps");

        // Rows that arrived while the person read above widen it to say so (item 11).
        view.update(cx, |host, cx| {
            host.1 = 3;
            cx.notify();
        });
        cx.run_until_parked();
        let wide = cx.debug_bounds("jump-test").expect("the button");
        assert!(f32::from(wide.size.width) > JUMP_WIDTH + 20.0, "it says \"3 new steps\": {wide:?}");
    }

    /// The jump button leaves over 300 ms, and plays its leave once.
    #[test]
    fn the_jump_button_plays_one_leave_then_goes() {
        let start = std::time::Instant::now();
        assert_eq!(presence("jump-leave-test", false, start), None, "never shown");
        assert_eq!(presence("jump-leave-test", true, start), Some(None));
        assert_eq!(presence("jump-leave-test", false, start), Some(Some(0)), "leaving");
        let mid = start + Duration::from_millis(150);
        assert_eq!(presence("jump-leave-test", false, mid), Some(Some(0)), "the same leave");
        assert_eq!(presence("jump-leave-test", false, start + Duration::from_millis(301)), None, "gone");
        assert_eq!(presence("jump-leave-test", true, mid), Some(None));
        assert_eq!(presence("jump-leave-test", false, mid), Some(Some(1)), "a new leave");
    }
}
