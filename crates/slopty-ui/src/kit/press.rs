//! The press that opens a thing's own menu: a right click, or a long press under a finger.
//!
//! A Mac person right-clicks first, as in Finder, Linear and Things; an iPad person holds. Both
//! open the same menu where the press landed, and the innermost thing pressed has it, so a
//! message in a tile opens the message's menu and not the tile's.

use std::rc::Rc;

use gpui::{
    App, InteractiveElement, LongPressEvent, MouseButton, ParentElement, Pixels, Point, Styled,
    TouchPhase, Window, canvas,
};

/// `el` opening a menu at the press through `open`, on a right click or a long press. `el`
/// becomes `relative`, since what hears the long press covers it.
pub fn menu_press<E>(el: E, open: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static) -> E
where
    E: InteractiveElement + ParentElement + Styled,
{
    let open = Rc::new(open);
    let held = Rc::clone(&open);
    let heard = canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let open = Rc::clone(&held);
            window.on_mouse_event(move |ev: &LongPressEvent, phase, window, cx| {
                if phase.bubble()
                    && ev.phase == TouchPhase::Started
                    && bounds.contains(&ev.position)
                    && !window.default_prevented()
                {
                    window.prevent_default();
                    cx.stop_propagation();
                    open(ev.position, window, cx);
                }
            });
        },
    )
    .absolute()
    .inset_0();
    el.relative()
        .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
            cx.stop_propagation();
            open(ev.position, window, cx);
        })
        .child(heard)
}
