//! slopty: what a side pane offers to open. The empty pane's list and the strip's `+` menu read
//! one list, in one order and under one set of names (T3's "Open a surface"; Claude desktop's
//! empty panes say what fills them).

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
    px,
};

use super::Shell;
use crate::icons;
use crate::motion::{self, HoverFade as _};
use crate::theme::Theme;

/// One surface a side pane can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneOffer {
    Changes,
    Files,
    Terminal,
    Browser,
    Screen,
}

impl PaneOffer {
    /// Every offer, in the order both lists show them.
    const ALL: [Self; 5] = [Self::Changes, Self::Files, Self::Terminal, Self::Browser, Self::Screen];

    /// The offer's name, as both lists and a screen reader say it.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Changes => "Changes",
            Self::Files => "Files",
            Self::Terminal => "Terminal",
            Self::Browser => "Browser",
            Self::Screen => "Remote window",
        }
    }

    /// The offer's glyph, the one its tab wears.
    pub(super) const fn icon(self) -> &'static str {
        match self {
            Self::Changes => icons::FILE_DIFF,
            Self::Files => icons::FOLDER_WITH_FILES,
            Self::Terminal => icons::TERMINAL,
            Self::Browser => icons::GLOBE,
            Self::Screen => icons::MONITOR,
        }
    }

    /// The offer's part of its rows' element ids.
    const fn key(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Files => "files",
            Self::Terminal => "terminal",
            Self::Browser => "browser",
            Self::Screen => "screen",
        }
    }
}

/// The empty pane's row height.
const ROW_HEIGHT: f32 = 32.0;

impl Shell {
    /// What this pane offers now: Changes only where there is git (terminals and the rest work
    /// anywhere).
    pub(super) fn pane_offers(&self, cx: &App) -> Vec<PaneOffer> {
        let git = self.space_git_detected(cx);
        PaneOffer::ALL.into_iter().filter(|offer| git || *offer != PaneOffer::Changes).collect()
    }

    /// Opens `offer` as a tab of the pane.
    pub(super) fn open_pane_offer(
        &mut self,
        offer: PaneOffer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match offer {
            PaneOffer::Changes => self.add_diff_surface(window, cx),
            PaneOffer::Files => self.add_files_surface(window, cx),
            PaneOffer::Terminal => {
                self.add_terminal_surface(cx);
            }
            PaneOffer::Browser => self.add_browser_surface(None, window, cx),
            PaneOffer::Screen => self.add_screen_surface(cx),
        }
    }

    /// The empty pane: a heading and one 32 pt row per offer, each a keyboard stop.
    pub(super) fn render_surface_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let rows = self.pane_offers(cx).into_iter().map(|offer| {
            let id = SharedString::from(format!("surface-card-{}", offer.key()));
            let fade = format!("{id}-fade");
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(offer.label())
                .w_full()
                .h(px(ROW_HEIGHT))
                .px(px(Theme::SPACE_SM))
                .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                .cursor_pointer()
                .bg(motion::hover_blend(&fade, crate::theme::wash(0.0), crate::theme::hover_wash()))
                .hover_fade(fade)
                .active(|style| style.bg(crate::theme::pressed_wash()))
                .map(|el| crate::surface_chrome::key_stop(el, &theme))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_pane_offer(offer, window, cx);
                }))
                .child(icons::icon(offer.icon()).size(px(16.0)).flex_none().text_color(theme.text_muted))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text)
                        .child(SharedString::from(offer.label())),
                )
        });
        div()
            .size_full()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .p(px(Theme::SPACE_LG))
            .child(
                div()
                    .w_full()
                    .max_w(px(280.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .id("surface-picker-heading")
                            .px(px(Theme::SPACE_SM))
                            .pb(px(Theme::SPACE_SM))
                            .role(gpui::Role::Heading)
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child("Open in this pane"),
                    )
                    .children(rows),
            )
            .into_any_element()
    }

    /// The strip's `+` menu rows: the same offers as [`Self::render_surface_picker`].
    pub(super) fn render_pane_offer_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.pane_offers(cx)
            .into_iter()
            .map(|offer| {
                let key = format!("right-plus-{}", offer.key());
                let selector = format!("{key}-row");
                crate::popover::menu_row(theme, false, key)
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector)
                    .role(gpui::Role::MenuItem)
                    .aria_label(offer.label())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_pane_offer(offer, window, cx);
                        this.close_right_plus(cx);
                    }))
                    .child(icons::icon(offer.icon()).size(px(14.0)).text_color(theme.text_muted))
                    .child(SharedString::from(offer.label()))
                    .into_any_element()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::PaneOffer;

    /// Both lists offer the same surfaces, in the order the pane uses them most, under the name
    /// the rest of the app gives each.
    #[test]
    fn the_pane_offers_one_list_under_one_name_each() {
        let labels: Vec<_> = PaneOffer::ALL.iter().map(|offer| offer.label()).collect();
        assert_eq!(labels, ["Changes", "Files", "Terminal", "Browser", "Remote window"]);
        assert_eq!(PaneOffer::Changes.icon(), crate::icons::FILE_DIFF, "not the menu's LIST");
        assert_eq!(PaneOffer::Browser.icon(), crate::icons::GLOBE);
    }
}
