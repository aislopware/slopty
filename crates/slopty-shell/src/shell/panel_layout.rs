// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! The sidebar's fit beside the session's panes (`docs/decisions/layout.md`, "No permanent right
//! column"): whether the window has room for it, how wide it may be, and its glide when the
//! window grows too narrow for it or wide enough again. Everything else beside the terminal is
//! a pane of the session's own view (`panes.rs`), laid out in the room the sidebar leaves.
use super::{CHAT_PANEL_MIN, Context, Route, SIDEBAR_DEFAULT, SIDEBAR_MIN, Shell, sidebar};

/// Whether the window has room for the sidebar this frame, and the widest it may be there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HorizontalPanelFit {
    /// Whether the sidebar has room beside the session.
    pub sidebar: bool,
    /// The widest the sidebar may be.
    pub sidebar_limit: f32,
}

/// The sidebar fits while the session keeps [`CHAT_PANEL_MIN`] beside it at the sidebar's
/// least: its rail when it is put away, [`SIDEBAR_MIN`] when it is open. Open, it is as wide as
/// the person made it, within what the session leaves.
pub(super) fn horizontal_panel_fit(
    viewport: f32,
    sidebar_width: f32,
    put_away: bool,
) -> HorizontalPanelFit {
    let viewport = viewport.max(0.0);
    let least = if put_away { sidebar::SIDEBAR_RAIL_WIDTH } else { SIDEBAR_MIN };
    let sidebar = least + CHAT_PANEL_MIN <= viewport;
    let sidebar_limit = if sidebar { (viewport - CHAT_PANEL_MIN).min(sidebar_width) } else { 0.0 };
    HorizontalPanelFit { sidebar, sidebar_limit }
}

impl Shell {
    /// Whether the window has room for the sidebar this frame.
    pub(super) fn horizontal_fit(&self) -> HorizontalPanelFit {
        if !matches!(self.route, Route::Chat) {
            return HorizontalPanelFit {
                sidebar: true,
                sidebar_limit: self.settings.sidebar_width,
            };
        }
        horizontal_panel_fit(
            self.viewport_width,
            self.settings.sidebar_width,
            self.settings.sidebar_collapsed,
        )
    }

    /// Follow the fit from frame to frame: a sidebar the fit newly hides eases out from the
    /// width it last painted, its content held at the width it laid out at, and one it brings
    /// back returns from zero. A live window resize or a drag is direct manipulation and eases
    /// nothing.
    pub(super) fn track_horizontal_fit(&mut self) {
        if !matches!(self.route, Route::Chat) {
            self.painted_sidebar = None;
            return;
        }
        let fit = self.horizontal_fit();
        let direct = self.viewport_resized || self.pane_resize_dragging.is_some();
        if direct {
            self.fit_exit = None;
            if self.sidebar_tween.is_some_and(|tween| tween.from > 0.5 && tween.to > 0.5) {
                self.sidebar_tween = None;
            }
        }
        if let Some((previous, (width, content))) = self.painted_sidebar.filter(|_| !direct) {
            if fit.sidebar {
                self.fit_exit = None;
            } else if previous.sidebar && width > 0.5 {
                self.fit_exit = Some((self.panel_tween(width, 0.0), content));
            }
            let opening = self
                .sidebar_tween
                .is_some_and(|tween| self.tween_active(Some(tween)) && tween.to > 0.5);
            if fit.sidebar && !previous.sidebar && !opening {
                self.sidebar_tween = Some(self.panel_tween(width, self.sidebar_target()));
            }
            // The window narrowing or widening past the sidebar's width eases it between the two
            // limits, from what it paints, mid-flight included.
            let closing = self
                .sidebar_tween
                .is_some_and(|tween| self.tween_active(Some(tween)) && tween.to <= 0.0);
            if fit.sidebar
                && previous.sidebar
                && !self.settings.sidebar_collapsed
                && !closing
                && (fit.sidebar_limit - previous.sidebar_limit).abs() > 0.5
            {
                let from = self
                    .eval_tween(self.sidebar_tween, previous.sidebar_limit)
                    .min(self.sidebar_tween_limit(previous.sidebar_limit));
                self.sidebar_tween = Some(self.panel_tween(from, fit.sidebar_limit));
            }
        }
        // Measuring is not drawing: leave frame scheduling to what paints this frame.
        let animating = self.motion_active.get();
        let painted = (self.sidebar_now(), self.fit_exit_content().unwrap_or(fit.sidebar_limit));
        self.motion_active.set(animating);
        self.painted_sidebar = Some((fit, painted));
    }

    /// How much narrower than the room the panes are laid out in the session's terminal
    /// fits its grid while the sidebar glides: the room now, less the narrower of where the glide
    /// started and where it lands. So a glide reflows the TUI once, to that narrower grid, never
    /// once a frame: an opening sidebar glides over ground the terminal already gave up, and a
    /// closing one uncovers ground the terminal fills when it lands (`docs/decisions/ui.md`, "A
    /// glide reflows the TUI once"). A drag tracks the pointer and holds nothing.
    pub(super) fn glide_hold(&self) -> f32 {
        let now = self.sidebar_now();
        let (started, lands) = if self.horizontal_fit().sidebar {
            self.active_tween_endpoints(self.sidebar_tween)
                .map_or((0.0, 0.0), |(from, _)| (from - now, self.sidebar_target() - now))
        } else {
            self.fit_exit.map_or((0.0, 0.0), |(exit, _)| {
                if self.tween_active(Some(exit)) { (exit.from - now, -now) } else { (0.0, 0.0) }
            })
        };
        started.max(lands).max(0.0)
    }

    /// The sidebar's width while the fit slides it out.
    pub(super) fn fit_exit_width(&self) -> Option<f32> {
        self.fit_exit
            .filter(|(exit, _)| self.tween_active(Some(*exit)))
            .map(|(exit, _)| self.eval_tween(Some(exit), 0.0))
    }

    /// The width the sidebar keeps laying out at while the fit slides it out.
    fn fit_exit_content(&self) -> Option<f32> {
        self.fit_exit.filter(|(exit, _)| self.tween_active(Some(*exit))).map(|(_, content)| content)
    }

    /// The width the sidebar lays its content out at: the fit's allowance, or what it had while
    /// the fit slides it out. Below the saved width when the window squeezes it.
    pub(super) fn sidebar_content_width(&self) -> f32 {
        // On a phone's Sessions page the list is the page.
        if let Some(width) = self.compact_sidebar_width() {
            return width;
        }
        self.fit_exit_content().unwrap_or_else(|| self.horizontal_fit().sidebar_limit)
    }

    /// Double-clicking the sidebar's seam brings it back to its default width, easing from what
    /// is on screen.
    pub(super) fn reset_sidebar_width(&mut self, cx: &mut Context<Self>) {
        let from = self.sidebar_now();
        self.settings.sidebar_width = SIDEBAR_DEFAULT;
        self.sidebar_edge_bounce = None;
        self.sidebar_tween = Some(self.panel_tween(from, self.sidebar_target()));
        self.schedule_save(cx);
        cx.notify();
    }

    /// A running sidebar tween may start above a limit that just dropped; it eases down to that
    /// limit rather than being cut on its first frame.
    pub(super) fn sidebar_tween_limit(&self, limit: f32) -> f32 {
        self.sidebar_tween
            .filter(|tween| self.tween_active(Some(*tween)))
            .map_or(limit, |tween| limit.max(tween.from.min(self.settings.sidebar_width)))
    }

    /// Record this frame's window width. A change is direct manipulation: it drops the sidebar's
    /// glide, so nothing lags the window edge.
    pub(super) fn observe_viewport_width(&mut self, viewport: f32) {
        self.viewport_resized = (self.viewport_width - viewport).abs() > 1.0;
        self.viewport_width = viewport;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sidebar shows while the session keeps its least beside the sidebar's least, its rail
    /// when put away; open, it is as wide as the person made it within what the session leaves.
    #[test]
    fn the_sidebar_fits_beside_the_session_or_steps_away() {
        let open = horizontal_panel_fit(1200.0, 280.0, false);
        assert_eq!(open, HorizontalPanelFit { sidebar: true, sidebar_limit: 280.0 });
        let squeezed = horizontal_panel_fit(CHAT_PANEL_MIN + SIDEBAR_MIN + 10.0, 400.0, false);
        assert!(squeezed.sidebar);
        assert!((squeezed.sidebar_limit - (SIDEBAR_MIN + 10.0)).abs() < 0.01);
        let narrow = horizontal_panel_fit(CHAT_PANEL_MIN + SIDEBAR_MIN - 1.0, 280.0, false);
        assert_eq!(narrow, HorizontalPanelFit { sidebar: false, sidebar_limit: 0.0 });
        // Put away, its rail still fits where the open sidebar would not.
        assert!(horizontal_panel_fit(CHAT_PANEL_MIN + SIDEBAR_MIN - 1.0, 280.0, true).sidebar);
        assert!(
            !horizontal_panel_fit(CHAT_PANEL_MIN + sidebar::SIDEBAR_RAIL_WIDTH - 1.0, 280.0, true)
                .sidebar
        );
    }
}
