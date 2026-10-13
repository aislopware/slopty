//! slopty: the phone's anatomy, after zeron's iOS app (`apps/ios`), built from the shell's own
//! parts. A phone shows one page at a time: Sessions (the large title, the sidebar's list at the
//! page's width, the "New session" capsule with what is running, and the tab bar of Sessions,
//! Settings and Search) or the chat that was opened from it, whose sidebar button goes back.
//! Every page keeps clear of the system's edges (the status bar, the home indicator) and of the
//! keyboard. Wider windows (an iPad's, a Mac's) keep the shell's columns.

use gpui::{
    AnyElement, App, Context, Global, IntoElement, Pixels, SharedString, Window, div,
    prelude::*, px,
};

use super::Shell;
use crate::icons::{self, icon};
use crate::theme::Theme;
use crate::typography::ui_rems;
use crate::state::Indicator;

/// A window narrower than this is a phone's, as the iOS e2e reads it.
const PHONE_BELOW: f32 = 700.0;
/// The large title's row, under the status bar.
const TITLE_ROW: f32 = 52.0;
/// The large title's size (zeron's navigation bar: Geist semibold 30).
const TITLE_SIZE: f32 = 30.0;
/// The "New session" capsule: its height, the plus's plate and inset.
const CAPSULE: f32 = 48.0;
const PLATE: f32 = 34.0;
const PLATE_INSET: f32 = 7.0;
/// The tab bar's row above the home indicator.
const TAB_BAR: f32 = 49.0;

/// Which page the phone shows.
#[derive(Default)]
struct Pages {
    /// The Sessions page is up (the phone opens on it).
    sessions: Option<bool>,
    /// The chat selected when last drawn: a new selection opens it.
    selected: Option<String>,
}

impl Global for Pages {}

/// The phone's tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Sessions,
    Settings,
    Search,
}

impl Tab {
    const ALL: [Self; 3] = [Self::Sessions, Self::Settings, Self::Search];

    const fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Settings => "Settings",
            Self::Search => "Search",
        }
    }

    const fn glyph(self) -> &'static str {
        match self {
            Self::Sessions => icons::CHAT_ROUND_LINE,
            Self::Settings => icons::SETTINGS,
            Self::Search => icons::MAGNIFER,
        }
    }
}

/// What is running, as the capsule says it: "2 working · 1 needs you".
fn summary(working: usize, waiting: usize) -> String {
    let mut parts = Vec::new();
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if waiting > 0 {
        let verb = if waiting == 1 { "needs" } else { "need" };
        parts.push(format!("{waiting} {verb} you"));
    }
    parts.join(" · ")
}

impl Shell {
    /// The window is a phone's.
    fn compact(&self) -> bool {
        cfg!(target_os = "ios") && self.viewport_width < PHONE_BELOW
    }

    /// The Sessions page is up: the phone opens on it, a chat chosen from it leaves it.
    fn compact_sessions_up(&self, cx: &mut App) -> bool {
        let selected = self.state.read(cx).selected_chat.clone();
        let pages = cx.default_global::<Pages>();
        let changed = pages.selected != selected;
        pages.selected = selected;
        if changed && pages.selected.is_some() {
            pages.sessions = Some(false);
        }
        *pages.sessions.get_or_insert(true)
    }

    fn set_compact_sessions(up: bool, cx: &mut App) {
        cx.default_global::<Pages>().sessions = Some(up);
    }

    /// On a phone the sidebar button goes back to Sessions; `false` elsewhere.
    pub(super) fn compact_back(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.compact() {
            return false;
        }
        Self::set_compact_sessions(true, cx);
        cx.notify();
        true
    }

    /// The sidebar's width while it is the Sessions page: the page's.
    pub(super) fn compact_sidebar_width(&self) -> Option<f32> {
        self.compact().then_some(self.viewport_width)
    }

    /// `page` kept clear of the system's edges and the keyboard, on iOS.
    pub(super) fn compact_frame(
        &self,
        page: impl IntoElement,
        window: &Window,
        cx: &App,
    ) -> gpui::Div {
        let frame = div().size_full().child(page);
        if !cfg!(target_os = "ios") {
            return frame;
        }
        let insets = window.insets();
        let safe = insets.safe_area;
        frame
            // A phone's chat has no sidebar: the window's own fill over the hidden column's
            // edge, which would otherwise stand as a hairline down the screen's left.
            .when(self.compact(), |frame| frame.bg(Theme::of(cx).glass()))
            .pt(safe.top)
            .pb(insets.effective().bottom)
            .pl(safe.left)
            .pr(safe.right)
    }

    /// The phone's Sessions page, when it is up; `None` leaves the window to the columns.
    pub(super) fn render_compact_sessions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.compact() || !self.compact_sessions_up(cx) {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let insets = window.insets();
        let safe = insets.safe_area;
        let title = div()
            .flex_none()
            .h(px(TITLE_ROW))
            // On the list's leading edge (its 8 pt inset and its rows' 8), less the glyph's own
            // side bearing at this size.
            .px(px(15.0))
            .flex()
            .items_end()
            .pb(px(6.0))
            .text_size(ui_rems(TITLE_SIZE))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child("Sessions");
        let list = div()
            .id("compact-sessions")
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.sidebar_pane.clone());
        let overlays = self.render_overlays(window.viewport_size(), window, cx);
        Some(
            div()
                .id("compact-sessions-page")
                .role(gpui::Role::Group)
                .aria_label("Sessions")
                .size_full()
                .relative()
                .flex()
                .flex_col()
                .pt(safe.top)
                .pl(safe.left)
                .pr(safe.right)
                .child(title)
                .child(list)
                .child(self.render_compact_capsule(&theme, cx))
                .child(self.render_compact_tabs(&theme, safe.bottom, cx))
                .children(overlays)
                .into_any_element(),
        )
    }

    /// The capsule over the tab bar: a plus, "New session", and what is running.
    fn render_compact_capsule(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (working, waiting) = {
            let state = self.state.read(cx);
            let now = chrono::Utc::now();
            state
                .chats
                .iter()
                .filter(|chat| !chat.archived && chat.is_top_level())
                .map(|chat| state.indicator_for(&chat.id, now))
                .fold((0_usize, 0_usize), |(working, waiting), indicator| match indicator {
                    Indicator::Working => (working.saturating_add(1), waiting),
                    Indicator::AwaitingInput => (working, waiting.saturating_add(1)),
                    Indicator::None | Indicator::Errored => (working, waiting),
                })
        };
        let said = summary(working, waiting);
        let label: SharedString = if said.is_empty() {
            "New session".into()
        } else {
            format!("New session, {}", said.replace(" · ", ", ")).into()
        };
        let plate = div()
            .flex_none()
            .size(px(PLATE))
            .rounded_full()
            .bg(theme.accent_wash)
            .flex()
            .items_center()
            .justify_center()
            .child(icon(icons::PLUS).size(px(15.0)).text_color(theme.accent));
        div()
            .flex_none()
            .px(px(16.0))
            .pb(px(8.0))
            .child(
                div()
                    .id("compact-new-session")
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .h(px(CAPSULE))
                    .w_full()
                    .pl(px(PLATE_INSET))
                    .pr(px(16.0))
                    .rounded_full()
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap(px(11.0))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        Self::set_compact_sessions(false, cx);
                        this.open_new_session(None, cx);
                    }))
                    .child(plate)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(16.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("New session"),
                    )
                    .when(!said.is_empty(), |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(ui_rems(13.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text_muted)
                                .child(said),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The tab bar: Sessions (this page), Settings, Search (the command palette).
    fn render_compact_tabs(
        &self,
        theme: &Theme,
        bottom: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let items = Tab::ALL.into_iter().map(|tab| {
            let current = tab == Tab::Sessions;
            let tone = if current { theme.text } else { theme.text_muted };
            div()
                .id(SharedString::from(format!("compact-tab-{}", tab.label())))
                .role(gpui::Role::Tab)
                .aria_label(tab.label())
                .aria_selected(current)
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(2.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| match tab {
                    Tab::Sessions => {}
                    Tab::Settings => this.open_last_settings(cx),
                    Tab::Search => this.toggle_command_palette(window, cx),
                }))
                .child(icon(tab.glyph()).size(px(22.0)).text_color(tone))
                .child(
                    div()
                        .text_size(ui_rems(10.0))
                        .font_weight(if current {
                            gpui::FontWeight::SEMIBOLD
                        } else {
                            gpui::FontWeight::MEDIUM
                        })
                        .text_color(tone)
                        .child(tab.label()),
                )
        });
        div()
            .id("compact-tabs")
            .role(gpui::Role::TabList)
            .aria_label("Tabs")
            .flex_none()
            .w_full()
            .border_t_1()
            .border_color(theme.border)
            .pb(bottom)
            .child(div().h(px(TAB_BAR)).w_full().flex().flex_row().children(items))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::summary;

    #[test]
    fn the_capsule_says_what_runs_and_what_waits() {
        assert_eq!(summary(0, 0), "");
        assert_eq!(summary(2, 0), "2 working");
        assert_eq!(summary(0, 1), "1 needs you");
        assert_eq!(summary(2, 3), "2 working · 3 need you");
    }
}
