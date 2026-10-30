//! What shows over the grid for a moment and takes nothing from it (ghostty's transient
//! overlays, `docs/decisions/terminal.md`, "Overlays that come and go"): the grid's new size
//! after a resize, and the target of the link under the pointer.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, div, px, relative,
};
use slopty_shell::live::chrome;
use slopty_shell::popover;

use super::TerminalView;
use crate::url;

/// How long a resize shows the grid's size: ghostty's `resize-overlay-duration`.
pub(super) const RESIZE_SHOWN: Duration = Duration::from_millis(750);

/// How long after the first layout, and after the keyboard comes or goes, a new size is the
/// layout settling rather than the person resizing: ghostty's.
pub(super) const SETTLING: Duration = Duration::from_millis(500);

/// The link banner's padding and its one round corner: ghostty's `URLHoverBanner`.
const BANNER_PADDING: f32 = 5.0;
const BANNER_RADIUS: f32 = 9.0;

/// When the grid's size shows.
#[derive(Default)]
pub(super) struct Resize {
    /// The size last laid out, `None` before the first layout.
    laid: Option<(u16, u16)>,
    /// When the grid was first laid out.
    first: Option<Instant>,
    /// When the keyboard or the focus last came or went.
    focus: Option<Instant>,
    /// Whether the view was focused when last drawn, `None` before it was.
    focused: Option<bool>,
    /// The size on show, and until when.
    shown: Option<((u16, u16), Instant)>,
    /// What draws the view again when the size goes.
    hide: Option<Task<()>>,
}

impl std::fmt::Debug for Resize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resize")
            .field("laid", &self.laid)
            .field("first", &self.first)
            .field("focus", &self.focus)
            .field("focused", &self.focused)
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

impl Resize {
    /// The grid was laid out at `grid` (columns, rows) at `now`: whether its size shows. Never
    /// on the first layout or a size it already had, nor within [`SETTLING`] of the first
    /// layout or a change of focus, when a pane is still finding its size.
    pub(super) fn laid_out(&mut self, grid: (u16, u16), now: Instant) -> bool {
        let Some(was) = self.laid.replace(grid) else {
            self.first = Some(now);
            return false;
        };
        let settling =
            |at: Option<Instant>| at.is_some_and(|at| now.saturating_duration_since(at) < SETTLING);
        if was == grid || settling(self.first) || settling(self.focus) {
            return false;
        }
        self.shown = now.checked_add(RESIZE_SHOWN).map(|until| (grid, until));
        self.shown.is_some()
    }

    /// The keyboard came to the view or left it at `now`.
    pub(super) const fn focus_changed(&mut self, now: Instant) {
        self.focus = Some(now);
    }

    /// The view is drawn at `now`, `focused` or not. Drawn before its grid is laid out, so a
    /// focus that moved in the same frame as a size settles that size too, before the focus's
    /// listeners would hear of it.
    pub(super) fn drawn(&mut self, focused: bool, now: Instant) {
        if self.focused.replace(focused).is_some_and(|was| was != focused) {
            self.focus = Some(now);
        }
    }

    /// The size on show at `now`.
    pub(super) fn showing(&self, now: Instant) -> Option<(u16, u16)> {
        self.shown.filter(|(_, until)| now < *until).map(|(grid, _)| grid)
    }
}

/// How the overlay says a grid of `cols` by `rows`: "120 ⨯ 40", ghostty's sign.
pub(super) fn size_words((cols, rows): (u16, u16)) -> String {
    format!("{cols} \u{2a2f} {rows}")
}

impl TerminalView {
    /// The grid was laid out at `grid`: show its size if [`Resize::laid_out`] says so, and
    /// draw again once it has stood for [`RESIZE_SHOWN`].
    pub(super) fn note_size(&mut self, grid: (u16, u16), cx: &mut Context<Self>) {
        let now = slopty_shell::clock::now(cx);
        if !self.resize.laid_out(grid, now) {
            return;
        }
        self.resize.hide = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_SHOWN).await;
            // A gone view shows nothing.
            let _gone = this.update(cx, |_, cx| cx.notify());
        }));
        Self::drawn_again(cx);
    }

    /// The keyboard came or went: a size laid out within [`SETTLING`] of now is not shown.
    pub(super) fn focus_moved(&mut self, cx: &Context<Self>) {
        self.resize.focus_changed(slopty_shell::clock::now(cx));
    }

    /// The view is drawn `focused` or not: a focus that moved settles the size laid out next.
    pub(super) fn note_focus(&mut self, focused: bool, cx: &Context<Self>) {
        self.resize.drawn(focused, slopty_shell::clock::now(cx));
    }

    /// "120 ⨯ 40" in the middle of the grid while a resize is fresh: ghostty's overlay, 5 pt
    /// padding, 4 pt corners, on the shell's floating card. The pointer passes through it.
    pub(super) fn render_resize(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let grid = self.resize.showing(slopty_shell::clock::now(cx))?;
        let words = SharedString::from(size_words(grid));
        let popup = chrome::theme(cx).for_popup();
        let (cols, rows) = grid;
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    popover::popover_card(&popup)
                        .id("terminal-resize")
                        .debug_selector(|| "terminal-resize".to_owned())
                        .role(gpui::Role::Status)
                        .aria_label(SharedString::from(format!("{cols} columns, {rows} rows")))
                        .p(px(5.0))
                        .rounded(px(4.0))
                        .font_family(popup.font_sans.clone())
                        .font_features(slopty_shell::typography::tabular())
                        .child(words),
                )
                .into_any_element(),
        )
    }

    /// Where the link under the pointer goes, in a banner at the grid's bottom left (ghostty's
    /// `URLHoverBanner`): one line, cut in the middle, shown so it cannot pass for another
    /// ([`url::display`]). While the pointer is over it, it stands at the bottom right instead,
    /// out of the way of what the pointer is on.
    pub(super) fn render_banner(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let target = self.link_target()?;
        let shown = SharedString::from(url::display(&target));
        let popup = chrome::theme(cx).for_popup();
        let flipped = self.banner_flipped;
        let side = |right: bool| {
            div()
                .absolute()
                .bottom_0()
                .when(right, |el| el.right_0().rounded_tl(px(BANNER_RADIUS)))
                .when(!right, |el| el.left_0().rounded_tr(px(BANNER_RADIUS)))
                // Half the pane at most, so the other side is always clear of it.
                .max_w(relative(0.5))
                .p(px(BANNER_PADDING))
                .bg(popover::surface_bg(&popup))
                .font_family(popup.font_sans.clone())
                .text_size(slopty_shell::typography::ui_rems(12.0))
                .text_color(popup.text)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis_middle()
                .child(shown.clone())
        };
        let left = side(false)
            .id("terminal-link")
            .debug_selector(|| "terminal-link".to_owned())
            .role(gpui::Role::Status)
            .aria_label(shown.clone())
            // Out of sight while flipped, but still where the pointer is: leaving it flips back.
            .when(flipped, |el| el.opacity(0.0))
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if this.banner_flipped != *hovered {
                    this.banner_flipped = *hovered;
                    cx.notify();
                }
            }));
        let right =
            flipped.then(|| side(true).debug_selector(|| "terminal-link-flipped".to_owned()));
        Some(
            div()
                .absolute()
                .size_full()
                .top_0()
                .left_0()
                .child(left)
                .children(right)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start.checked_add(Duration::from_millis(ms)).unwrap()
    }

    /// A size shows only when the person resized: not the first layout, not one the grid
    /// already had, not while it settles after the first layout or a focus change; then for
    /// 750 ms.
    #[test]
    fn a_resize_shows_only_once_the_layout_has_settled() {
        let t = Instant::now();
        let mut resize = Resize::default();
        assert!(!resize.laid_out((80, 24), t), "the first layout");
        assert!(!resize.laid_out((100, 30), at(t, 400)), "still settling");
        assert!(!resize.laid_out((100, 30), at(t, 900)), "the size it had");
        assert!(resize.laid_out((120, 40), at(t, 1000)), "a resize");
        assert_eq!(resize.showing(at(t, 1000)), Some((120, 40)));
        assert_eq!(resize.showing(at(t, 1749)), Some((120, 40)));
        assert_eq!(resize.showing(at(t, 1750)), None, "750 ms");
        resize.focus_changed(at(t, 2000));
        assert!(!resize.laid_out((90, 40), at(t, 2499)), "a focus change settles");
        assert!(resize.laid_out((91, 40), at(t, 2500)));
        resize.drawn(true, at(t, 2600));
        resize.drawn(true, at(t, 3000));
        assert!(resize.laid_out((92, 40), at(t, 3100)), "drawn as it was: nothing moved");
        resize.drawn(false, at(t, 3200));
        assert!(!resize.laid_out((93, 40), at(t, 3200)), "the focus left in this frame");
        assert_eq!(size_words((120, 40)), "120 \u{2a2f} 40");
    }
}
