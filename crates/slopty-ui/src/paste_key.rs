//! The system's paste button over the key bar's Paste, on iPhone and iPad.
//!
//! Reading another app's clipboard on iOS asks the person each time, unless a paste they made
//! does the reading. A tap on `UIPasteControl` is such a paste
//! (`slopty_platform::paste_control`), so the key bar's Paste is that button: UIKit draws it over
//! the cap, and what a tap hands over goes where the cap's paste goes
//! ([`crate::workspace::WorkspaceView::paste_made`]), with no alert. The cap stays under it, so
//! where the button cannot be made the cap pastes as before and iOS asks (`docs/decisions/ui.md`,
//! "Paste on iPhone and iPad through the system's button").

use gpui::{Bounds, Pixels};
#[cfg(target_os = "ios")]
pub use ios::PasteKey;
use slopty_platform::web::Frame;
use slopty_theme::Rgb;

#[cfg(target_os = "ios")]
use crate::workspace::WorkspaceView;

/// Where the button goes over a cap at `cap`, in the view's points.
///
/// `None` unless the whole cap is inside `clip`, the part of the key row in view: a UIKit view
/// is not clipped by the row's scrolling, so a cap scrolled partly out would put the button
/// over its neighbours.
#[must_use]
pub fn frame_over(cap: Bounds<Pixels>, clip: Bounds<Pixels>) -> Option<Frame> {
    let edges = |b: Bounds<Pixels>| {
        let (x, y) = (f32::from(b.origin.x), f32::from(b.origin.y));
        (x, y, x + f32::from(b.size.width), y + f32::from(b.size.height))
    };
    let (left, top, right, bottom) = edges(cap);
    let (clip_left, clip_top, clip_right, clip_bottom) = edges(clip);
    let inside =
        left >= clip_left && top >= clip_top && right <= clip_right && bottom <= clip_bottom;
    (inside && right > left && bottom > top).then(|| Frame {
        x: f64::from(left),
        y: f64::from(top),
        w: f64::from(right - left),
        h: f64::from(bottom - top),
    })
}

/// A theme colour as the system takes one: red, green, blue and alpha from 0 to 1, opaque.
#[must_use]
pub fn rgba(color: Rgb) -> [f64; 4] {
    let unit = |c: u8| f64::from(c) / f64::from(u8::MAX);
    [unit(color.r), unit(color.g), unit(color.b), 1.0]
}

#[cfg(target_os = "ios")]
mod ios {
    use std::rc::Rc;

    use gpui::{App, Bounds, Pixels, Task, WeakEntity, Window};
    use slopty_platform::paste_control::{Mode, PasteButton, Pasted, Style};
    use slopty_theme::Theme;

    use super::{WorkspaceView, frame_over, rgba};

    /// The system's paste button of one window, and the task that hands its pastes on.
    pub struct PasteKey {
        button: PasteButton,
        /// How it is drawn, which only a new button changes.
        style: Style,
        _pastes: Task<()>,
    }

    /// The key bar's caps as the system draws its button: `theme`'s text on its raised plate,
    /// its small radius, the word alone.
    fn style(theme: &Theme) -> Style {
        let s = &theme.surfaces;
        Style {
            mode: Mode::Label,
            foreground: rgba(s.text),
            background: rgba(s.raised),
            corner_radius: f64::from(theme.radii.sm),
        }
    }

    impl std::fmt::Debug for PasteKey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PasteKey").field("button", &self.button).finish_non_exhaustive()
        }
    }

    impl PasteKey {
        /// A hidden button in `window`, drawn as the key bar's caps are in `theme`; each paste
        /// goes to `workspace`. `None` where the window has no UIKit view to hold it.
        #[must_use]
        pub fn new(
            window: &Window,
            theme: &Theme,
            workspace: WeakEntity<WorkspaceView>,
            cx: &App,
        ) -> Option<Self> {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let RawWindowHandle::UiKit(handle) =
                HasWindowHandle::window_handle(window).ok()?.as_raw()
            else {
                return None;
            };
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Pasted>();
            let sink: Rc<dyn Fn(Pasted)> = Rc::new(move |pasted| {
                let _sent = tx.send(pasted);
            });
            let style = style(theme);
            let button = PasteButton::new(handle.ui_view, style, sink)?;
            let pastes = cx.spawn(async move |cx| {
                while let Some(pasted) = rx.recv().await {
                    let board = pasted.board();
                    if workspace.update(cx, |v, cx| v.paste_made(&board, cx)).is_err() {
                        break;
                    }
                }
            });
            Some(Self { button, style, _pastes: pastes })
        }

        /// Whether it is drawn as `theme` would draw it; a theme that changed wants a new one.
        #[must_use]
        pub fn drawn_as(&self, theme: &Theme) -> bool {
            self.style == style(theme)
        }

        /// Put the button over the cap at `cap`, while the whole cap is inside `clip`.
        pub fn place(&self, cap: Bounds<Pixels>, clip: Bounds<Pixels>) {
            match frame_over(cap, clip) {
                Some(frame) => self.button.show(frame),
                None => self.button.hide(),
            }
        }

        /// Take the button away: no cap of the key bar pastes this frame.
        pub fn hide(&self) {
            self.button.hide();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::*;

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
    }

    /// The button covers the cap exactly while the cap is wholly in view, and not at all once
    /// the row has scrolled any of it out.
    #[test]
    fn the_button_sits_on_a_cap_in_view_and_nowhere_else() {
        let row = bounds(0.0, 700.0, 390.0, 44.0);
        let frame = frame_over(bounds(120.0, 704.0, 56.0, 36.0), row);
        assert_eq!(frame.map(|f| (f.x, f.y, f.w, f.h)), Some((120.0, 704.0, 56.0, 36.0)));
        assert!(frame_over(bounds(360.0, 704.0, 56.0, 36.0), row).is_none(), "half out");
        assert!(frame_over(bounds(-10.0, 704.0, 56.0, 36.0), row).is_none(), "scrolled past");
        assert!(frame_over(bounds(120.0, 704.0, 0.0, 36.0), row).is_none(), "not laid out");
    }

    #[test]
    fn a_theme_colour_is_opaque_and_in_unit_range() {
        let [r, g, b, a] = rgba(Rgb::hex(0xff_00_80));
        let near = |x: f64, want: f64| (x - want).abs() < 1e-9;
        assert!(near(r, 1.0) && near(g, 0.0) && near(b, 128.0 / 255.0) && near(a, 1.0));
    }
}
