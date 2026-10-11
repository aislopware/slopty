//! A window's frame as the layout saves it ([`WindowFrame`]): read from a window, and turned
//! back into the bounds a window opens at, on the screen it stood on.

use gpui::{App, Bounds, DisplayId, Pixels, Window, WindowBounds, point, px, size};
use slopty_client::layout::WindowFrame;

/// Where `window` stands now, as the layout keeps it; `None` for a frame not worth opening
/// again ([`WindowFrame::sane`]).
#[must_use]
pub fn of(window: &Window, cx: &App) -> Option<WindowFrame> {
    let (bounds, fullscreen) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => (bounds, false),
        WindowBounds::Fullscreen(bounds) => (bounds, true),
    };
    let display = window.display(cx).and_then(|d| d.uuid().ok()).map(|u| u.to_string());
    let frame = WindowFrame {
        display,
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        fullscreen,
    };
    frame.sane().then_some(frame)
}

/// Where a window saved as `frame` opens, and on which display.
///
/// Its own display while it is attached, else the main one. Either way it is moved and shrunk
/// to lie inside that display's visible area, so a window saved on a screen since unplugged, or
/// under a menu bar that grew, never opens out of reach.
#[must_use]
pub fn bounds(frame: &WindowFrame, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    let own = frame.display.as_ref().and_then(|uuid| {
        cx.displays().into_iter().find(|d| d.uuid().is_ok_and(|u| u.to_string() == *uuid))
    });
    let display = own.or_else(|| cx.primary_display());
    let visible = display.as_ref().map(|d| {
        let (whole, visible) = (d.bounds(), d.visible_bounds());
        // A window's bounds are its display's; the display's are the desktop's.
        Bounds::new(
            point(visible.origin.x - whole.origin.x, visible.origin.y - whole.origin.y),
            visible.size,
        )
    });
    let placed = visible.map_or_else(|| bounds_of(frame), |visible| inside(frame, visible));
    let bounds = if frame.fullscreen {
        WindowBounds::Fullscreen(placed)
    } else {
        WindowBounds::Windowed(placed)
    };
    (bounds, display.map(|d| d.id()))
}

/// `frame`'s rectangle as it was saved.
fn bounds_of(frame: &WindowFrame) -> Bounds<Pixels> {
    Bounds::new(point(px(frame.x), px(frame.y)), size(px(frame.width), px(frame.height)))
}

/// `frame`'s rectangle moved and shrunk, as little as it takes, to lie inside `visible`.
fn inside(frame: &WindowFrame, visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let (vx, vy) = (f32::from(visible.origin.x), f32::from(visible.origin.y));
    let (vw, vh) = (f32::from(visible.size.width), f32::from(visible.size.height));
    let (w, h) = (frame.width.min(vw), frame.height.min(vh));
    let x = frame.x.clamp(vx, vx + vw - w);
    let y = frame.y.clamp(vy, vy + vh - h);
    Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x: f32, y: f32, width: f32, height: f32) -> WindowFrame {
        WindowFrame { display: None, x, y, width, height, fullscreen: false }
    }

    /// A frame inside the screen opens where it was; one hanging off an edge comes back on it
    /// by the least move; one bigger than the screen shrinks to it.
    #[test]
    fn a_frame_opens_where_it_was_or_as_near_as_the_screen_allows() {
        let visible = Bounds::new(point(px(0.0), px(25.0)), size(px(1512.0), px(920.0)));
        // Whole points in, whole points out: compared as integers.
        #[expect(clippy::cast_possible_truncation, reason = "whole points, well in range")]
        let at = |f: &WindowFrame| {
            let b = inside(f, visible);
            [b.origin.x, b.origin.y, b.size.width, b.size.height].map(|v| f32::from(v) as i32)
        };
        assert_eq!(at(&frame(100.0, 80.0, 1280.0, 800.0)), [100, 80, 1280, 800]);
        assert_eq!(at(&frame(900.0, 80.0, 1280.0, 800.0)), [232, 80, 1280, 800]);
        assert_eq!(at(&frame(-3000.0, 0.0, 800.0, 600.0)), [0, 25, 800, 600]);
        assert_eq!(at(&frame(10.0, 10.0, 2560.0, 1440.0)), [0, 25, 1512, 920]);
    }
}
