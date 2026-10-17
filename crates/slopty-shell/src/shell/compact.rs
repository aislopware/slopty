//! slopty: the phone's anatomy, after zeron's iOS app (`apps/ios`), built from the shell's own
//! parts. A phone shows one page at a time: Sessions (the large title, the sidebar's list at the
//! page's width, the "New session" capsule with what is running, and the tab bar of Sessions,
//! Settings and Search) or the chat that was opened from it, whose sidebar button goes back.
//! Every page keeps clear of the system's edges (the status bar, the home indicator) and of the
//! keyboard. Wider windows (an iPad's, a Mac's) keep the shell's columns.
//!
//! The chat or the new-session page wears no desktop chrome there: the titlebar's sidebar and
//! history cluster give way to a navigation bar (zeron iOS's `SessionViewController`: a back
//! chevron to Sessions and the session's title over "project @ device"; `NewSessionViewController`:
//! "New session" with a close button). A session row takes zeron iOS's gestures
//! (`SessionListController`): a leading swipe pins it, a trailing swipe archives it (or brings it
//! back from Archived), and a long press opens its menu.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Global, IntoElement, LongPressEvent, Pixels, ScrollWheelEvent,
    SharedString, TouchPhase, Window, div, prelude::*, px,
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
/// The selected tab's glass pill, inset in its third of the bar.
const TAB_PILL_INSET: f32 = 6.0;
/// The capsule's status glyph, before the summary (zeron's 12 pt `StatusGlyph`).
const CAPSULE_GLYPH: f32 = 12.0;
/// The navigation bar's back and close targets.
const NAV_TARGET: f32 = 34.0;
/// A row's swipe: travel before its axis is decided, travel that commits on release, the reach
/// past which it resists, and the return to rest.
const SWIPE_DECIDE: f32 = 10.0;
const SWIPE_COMMIT: f32 = 76.0;
const SWIPE_REACH: f32 = 112.0;
const SWIPE_SETTLE: Duration = Duration::from_millis(220);

/// Which page the phone shows.
#[derive(Default)]
struct Pages {
    /// The Sessions page is up (the phone opens on it).
    sessions: Option<bool>,
    /// The chat selected when last drawn: a new selection opens it.
    selected: Option<String>,
}

impl Global for Pages {}

/// What a finished swipe does to its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwipeAct {
    /// The leading swipe: pin, or unpin a pinned row.
    Pin,
    /// The trailing swipe: archive, or bring back an archived row.
    Archive,
}

/// One row's swipe in flight: the travel so far, and whether it was judged sideways.
#[derive(Debug)]
struct Swipe {
    chat: String,
    x: f32,
    y: f32,
    sideways: Option<bool>,
}

/// What one scroll step of a swipe means for the list under the row.
#[derive(Debug, PartialEq)]
enum SwipeStep {
    /// Not the row's: the list scrolls.
    Pass,
    /// The row's: it follows the finger.
    Held,
    /// Released at this travel: what it does, if it went far enough.
    Done(Option<SwipeAct>, f32),
}

/// The rows' swipes: the one under the finger, and the one gliding back to rest.
#[derive(Default)]
struct Swipes {
    active: Option<Swipe>,
    settling: Option<(String, f32, Instant)>,
}

impl Global for Swipes {}

/// A pan step over row `chat`, as the touch recognizer sends it (axis-locked pixel deltas,
/// phased). Momentum after a release is never a swipe.
fn swipe_step(
    active: &mut Option<Swipe>,
    chat: &str,
    dx: f32,
    dy: f32,
    phase: TouchPhase,
    momentum: bool,
) -> SwipeStep {
    if momentum {
        return SwipeStep::Pass;
    }
    if phase == TouchPhase::Started || active.as_ref().is_none_or(|swipe| swipe.chat != chat) {
        if phase != TouchPhase::Started && phase != TouchPhase::Moved {
            return SwipeStep::Pass;
        }
        *active = Some(Swipe { chat: chat.to_owned(), x: 0.0, y: 0.0, sideways: None });
    }
    let Some(swipe) = active.as_mut() else { return SwipeStep::Pass };
    swipe.x += dx;
    swipe.y += dy;
    if swipe.sideways.is_none() && swipe.x.abs().max(swipe.y.abs()) >= SWIPE_DECIDE {
        swipe.sideways = Some(swipe.x.abs() > swipe.y.abs());
    }
    match phase {
        TouchPhase::Ended | TouchPhase::Cancelled => {
            let swipe = active.take().expect("a swipe in flight");
            if swipe.sideways != Some(true) {
                return SwipeStep::Pass;
            }
            if phase == TouchPhase::Cancelled {
                return SwipeStep::Done(None, swipe.x);
            }
            let act = if swipe.x >= SWIPE_COMMIT {
                Some(SwipeAct::Pin)
            } else if swipe.x <= -SWIPE_COMMIT {
                Some(SwipeAct::Archive)
            } else {
                None
            };
            SwipeStep::Done(act, swipe.x)
        }
        _ if swipe.sideways == Some(true) => SwipeStep::Held,
        _ => SwipeStep::Pass,
    }
}

/// Where a row sits for a finger's travel: with it up to the reach, then a quarter as far.
fn swipe_offset(travel: f32) -> f32 {
    let reach = travel.abs();
    let at = if reach <= SWIPE_REACH { reach } else { SWIPE_REACH + (reach - SWIPE_REACH) * 0.25 };
    at.copysign(travel)
}

#[cfg(test)]
thread_local! {
    /// Tests lay the phone out on a Mac.
    static FORCE_COMPACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this build lays a narrow window out as a phone: iOS, or a test that asks.
fn phone_platform() -> bool {
    #[cfg(test)]
    if FORCE_COMPACT.with(std::cell::Cell::get) {
        return true;
    }
    cfg!(target_os = "ios")
}

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
    pub(super) fn compact(&self) -> bool {
        phone_platform() && self.viewport_width < PHONE_BELOW
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

    /// The capsule over the tab bar: a plus, "New session", and what is running, on the glass
    /// zeron's accessory sits on (`AskAnythingAccessory`), the status glyph before the words.
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
        let popup = theme.for_popup();
        let glass = popup.is_frost();
        // zeron's glyph: a spinner while anything works, else the input dot.
        let glyph = (!said.is_empty()).then(|| {
            let cell = div()
                .flex_none()
                .size(px(CAPSULE_GLYPH))
                .flex()
                .items_center()
                .justify_center();
            if working > 0 {
                cell.child(crate::loaders::mini_glyph_spinner(
                    "compact-capsule-working",
                    2.0,
                    theme.glyph,
                    cx.entity_id(),
                    cx,
                ))
            } else {
                cell.child(div().size(px(7.0)).rounded_full().bg(theme.warning))
            }
        });
        let plate = div()
            .flex_none()
            .size(px(PLATE))
            .rounded_full()
            .bg(theme.accent_wash)
            .flex()
            .items_center()
            .justify_center()
            .child(icon(icons::PLUS).size(px(15.0)).text_color(theme.accent));
        let capsule = div()
            .id("compact-new-session")
            .role(gpui::Role::Button)
            .aria_label(label)
            .h(px(CAPSULE))
            .w_full()
            .pl(px(PLATE_INSET))
            .pr(px(16.0))
            .rounded_full()
            .bg(if glass {
                crate::popover::surface_bg(&popup)
            } else {
                popup.surface_raised
            })
            .border_1()
            .border_color(popup.border)
            .when(!glass, |el| el.shadow_md())
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
                    .text_color(popup.text)
                    .child("New session"),
            )
            .children(glyph.map(|glyph| glyph.mr(px(-4.0))))
            .when(!said.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(popup.text_muted)
                        .child(said),
                )
            });
        div()
            .flex_none()
            .px(px(16.0))
            .pb(px(8.0))
            .child(crate::frost::frosted(CAPSULE / 2.0, crate::frost::MENU_BLUR, capsule))
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
            // zeron tints the bar in `text`: the selected tab reads through its glass pill and
            // its weight, never the accent.
            let tone = if current { theme.text } else { theme.text_muted };
            let pill = current.then(|| {
                div()
                    .absolute()
                    .top(px(TAB_PILL_INSET * 0.5))
                    .bottom(px(TAB_PILL_INSET * 0.5))
                    .left(px(TAB_PILL_INSET))
                    .right(px(TAB_PILL_INSET))
                    .child(crate::frost::frosted(
                        (TAB_BAR - TAB_PILL_INSET) / 2.0,
                        crate::frost::MENU_BLUR,
                        div().size_full().rounded_full().bg(crate::theme::wash(0.10)),
                    ))
            });
            div()
                .id(SharedString::from(format!("compact-tab-{}", tab.label())))
                .role(gpui::Role::Tab)
                .aria_label(tab.label())
                .aria_selected(current)
                .relative()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(2.0))
                .cursor_pointer()
                .children(pill)
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

    /// The phone's navigation bar over a chat or the new-session page, in place of the
    /// titlebar and its sidebar and history cluster; `None` off a phone. A chat: a back
    /// chevron to Sessions, its title over "project @ device". The new-session page: "New
    /// session" and a close button.
    pub(super) fn render_compact_nav_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.compact() {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let (title, subtitle): (SharedString, Option<SharedString>) = {
            let state = self.state.read(cx);
            match state.selected_chat_row() {
                Some(chat) => {
                    let title = crate::transcript::single_line(
                        chat.title.as_deref().filter(|title| !title.trim().is_empty()).unwrap_or("New session"),
                    );
                    let project = state.space_for_chat(chat).map(|space| space.display_name().to_owned());
                    let device = state.device_name(&chat.device_id).map(str::to_owned);
                    let subtitle = match (project, device) {
                        (Some(project), Some(device)) => Some(format!("{project} @ {device}")),
                        (Some(project), None) => Some(project),
                        (None, Some(device)) => Some(device),
                        (None, None) => None,
                    };
                    (title.into(), subtitle.map(SharedString::from))
                }
                None => ("New session".into(), None),
            }
        };
        let on_canvas = self.state.read(cx).selected_chat.is_none();
        let back = |this: &mut Self, cx: &mut Context<Self>| {
            Self::set_compact_sessions(true, cx);
            this.close_chat_menu(cx);
            cx.notify();
        };
        let lead = if on_canvas {
            div()
                .id("compact-close")
                .debug_selector(|| "compact-close".into())
                .role(gpui::Role::Button)
                .aria_label("Close")
                .size(px(NAV_TARGET))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(|el| el.bg(crate::theme::wash(0.06)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| back(this, cx)))
                .child(icon(icons::CLOSE).size(px(18.0)).text_color(theme.text))
        } else {
            div()
                .id("compact-back")
                .debug_selector(|| "compact-back".into())
                .role(gpui::Role::Button)
                .aria_label("Back to Sessions")
                .h(px(NAV_TARGET))
                .flex_none()
                .pr(px(8.0))
                .rounded_full()
                .flex()
                .items_center()
                .gap(px(2.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| back(this, cx)))
                .child(icon(icons::ALT_ARROW_LEFT).size(px(22.0)).text_color(theme.text))
                .child(
                    div()
                        .text_size(ui_rems(16.0))
                        .text_color(theme.text)
                        .child("Sessions"),
                )
        };
        // The title centres on the bar, whatever the lead's width; the lead's side reserves
        // room on both edges so a long title never runs under it.
        let heading = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(96.0))
            .right(px(96.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("compact-nav-title")
                    .role(gpui::Role::Heading)
                    .aria_label(title.clone())
                    .max_w_full()
                    .truncate()
                    .text_size(ui_rems(if subtitle.is_some() { 16.0 } else { 17.0 }))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(title),
            )
            .children(subtitle.map(|subtitle| {
                div()
                    .max_w_full()
                    .truncate()
                    .text_size(ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(subtitle)
            }));
        Some(
            div()
                .id("compact-nav-bar")
                .role(gpui::Role::Group)
                .aria_label("Navigation")
                .relative()
                .w_full()
                .h(px(Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD))
                .flex()
                .items_center()
                .px(px(8.0))
                .child(heading)
                .child(lead)
                .into_any_element(),
        )
    }

    /// `row`, as a phone's session row: it follows a sideways finger with the action it would
    /// take showing behind it (pin on the leading side, archive on the trailing), takes that
    /// action when let go past [`SWIPE_COMMIT`], and opens the row's menu on a long press.
    /// Off a phone the row is as it was.
    pub(super) fn compact_session_row(
        &self,
        row: AnyElement,
        chat_id: &str,
        archived: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.compact() {
            return row;
        }
        let theme = Theme::of(cx).clone();
        let pinned = self.active_sidebar_pins(cx).iter().any(|pin| pin == chat_id);
        let offset = {
            let now = Instant::now();
            let reduced = cx.reduce_motion();
            let swipes = cx.default_global::<Swipes>();
            match (&swipes.active, &swipes.settling) {
                (Some(swipe), _) if swipe.chat == chat_id && swipe.sideways == Some(true) => {
                    swipe_offset(swipe.x)
                }
                (_, Some((chat, from, at))) if chat == chat_id => {
                    let t = now.saturating_duration_since(*at).as_secs_f32()
                        / SWIPE_SETTLE.as_secs_f32();
                    if reduced || t >= 1.0 {
                        swipes.settling = None;
                        0.0
                    } else {
                        self.motion_active.set(true);
                        let eased = 1.0 - (1.0 - t).powi(3);
                        from * (1.0 - eased)
                    }
                }
                _ => 0.0,
            }
        };
        let (glyph, words, fill) = if offset >= 0.0 {
            (icons::PIN, if pinned { "Unpin" } else { "Pin" }, theme.accent_strong)
        } else if archived {
            (icons::ARCHIVE_UP_MINIMALISTIC, "Unarchive", theme.accent_strong)
        } else {
            (icons::ARCHIVE_MINIMALISTIC, "Archive", theme.danger_strong)
        };
        // What the swipe would do, under the row and on the side it uncovers; it lights fully
        // once letting go would take it.
        let reveal = (offset.abs() > 0.5).then(|| {
            let ready = offset.abs() >= SWIPE_COMMIT;
            div()
                .absolute()
                .inset_0()
                .rounded(px(8.0))
                .bg(fill.opacity(if ready { 1.0 } else { 0.72 }))
                .flex()
                .items_center()
                .when(offset > 0.0, |el| el.justify_start().pl(px(16.0)))
                .when(offset < 0.0, |el| el.justify_end().pr(px(16.0)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.on_accent)
                        .child(icon(glyph).size(px(16.0)).text_color(theme.on_accent))
                        .child(words),
                )
        });
        let shell = cx.weak_entity();
        let chat = chat_id.to_owned();
        let pressed = chat_id.to_owned();
        div()
            .id(SharedString::from(format!("compact-row-{chat_id}")))
            .debug_selector({
                let chat = chat_id.to_owned();
                move || format!("compact-row-{chat}")
            })
            .relative()
            .rounded(px(8.0))
            .children(reveal)
            .child(div().relative().left(px(offset)).child(row))
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(px(16.0));
                let step = swipe_step(
                    &mut cx.default_global::<Swipes>().active,
                    &chat,
                    f32::from(delta.x),
                    f32::from(delta.y),
                    event.touch_phase,
                    event.momentum_phase.is_some(),
                );
                match step {
                    SwipeStep::Pass => return,
                    SwipeStep::Held => {}
                    SwipeStep::Done(act, travel) => {
                        this.finish_compact_swipe(chat.clone(), act, travel, archived, pinned, cx);
                    }
                }
                cx.stop_propagation();
                window.refresh();
                cx.notify();
            }))
            .child(
                gpui::canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let shell = shell.clone();
                        let chat = pressed.clone();
                        window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                            if phase != gpui::DispatchPhase::Bubble
                                || event.phase != TouchPhase::Started
                                || !bounds.contains(&event.start_position)
                            {
                                return;
                            }
                            let opened = shell
                                .update(cx, |this, cx| {
                                    this.chat_menu.open(super::ChatMenuState {
                                        tab: None,
                                        chat_id: chat.clone(),
                                        position: event.start_position,
                                        page: super::ChatMenuPage::Root,
                                    });
                                    cx.notify();
                                })
                                .is_ok();
                            if opened {
                                window.prevent_default();
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }

    /// A swipe let go: its action, if it went far enough, then the row glides back to rest.
    fn finish_compact_swipe(
        &mut self,
        chat: String,
        act: Option<SwipeAct>,
        travel: f32,
        archived: bool,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        cx.default_global::<Swipes>().settling = Some((chat.clone(), swipe_offset(travel), Instant::now()));
        match act {
            Some(SwipeAct::Pin) => self.set_chat_pinned(chat, !pinned, cx),
            Some(SwipeAct::Archive) => self.set_chat_archived(chat, !archived, cx),
            None => {}
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pan(active: &mut Option<Swipe>, steps: &[(f32, f32)]) -> Vec<SwipeStep> {
        let last = steps.len().saturating_sub(1);
        steps
            .iter()
            .enumerate()
            .map(|(ix, (dx, dy))| {
                let phase = match ix {
                    0 => TouchPhase::Started,
                    ix if ix == last => TouchPhase::Ended,
                    _ => TouchPhase::Moved,
                };
                swipe_step(active, "chat", *dx, *dy, phase, false)
            })
            .collect()
    }

    // A sideways pan past the commit travel pins (leading) or archives (trailing); a short one
    // springs back with nothing done; a vertical one is the list's scroll, never the row's.
    #[test]
    fn a_rows_swipe_pins_or_archives_past_its_travel() {
        let mut active = None;
        let steps = pan(&mut active, &[(0.0, 0.0), (30.0, 2.0), (30.0, 1.0), (30.0, 0.0)]);
        assert_eq!(steps[1], SwipeStep::Held);
        assert_eq!(steps[3], SwipeStep::Done(Some(SwipeAct::Pin), 90.0));
        assert!(active.is_none(), "the swipe ends with its touch");
        let steps = pan(&mut active, &[(0.0, 0.0), (-40.0, 0.0), (-45.0, 0.0)]);
        assert_eq!(steps[2], SwipeStep::Done(Some(SwipeAct::Archive), -85.0));
        let steps = pan(&mut active, &[(0.0, 0.0), (20.0, 0.0), (20.0, 0.0)]);
        assert_eq!(steps[2], SwipeStep::Done(None, 40.0), "short of the travel");
        let steps = pan(&mut active, &[(0.0, 0.0), (2.0, 30.0), (60.0, 40.0), (40.0, 0.0)]);
        assert!(steps.iter().all(|step| *step == SwipeStep::Pass), "the list scrolls: {steps:?}");
        // Momentum after a release is never a swipe.
        assert_eq!(swipe_step(&mut active, "chat", 90.0, 0.0, TouchPhase::Moved, true), SwipeStep::Pass);
        // A cancelled pan springs back with nothing done.
        let mut active = None;
        swipe_step(&mut active, "chat", 0.0, 0.0, TouchPhase::Started, false);
        swipe_step(&mut active, "chat", 100.0, 0.0, TouchPhase::Moved, false);
        assert_eq!(
            swipe_step(&mut active, "chat", 0.0, 0.0, TouchPhase::Cancelled, false),
            SwipeStep::Done(None, 100.0)
        );
    }

    // The row follows the finger to its reach, then resists.
    #[test]
    fn a_row_resists_past_its_reach() {
        assert_eq!(swipe_offset(50.0), 50.0);
        assert_eq!(swipe_offset(-SWIPE_REACH), -SWIPE_REACH);
        assert_eq!(swipe_offset(SWIPE_REACH + 40.0), SWIPE_REACH + 10.0);
        assert_eq!(swipe_offset(-(SWIPE_REACH + 40.0)), -(SWIPE_REACH + 10.0));
    }

    /// The phone's navigation bar and its session list, from a shell with no engine.
    struct PhoneHost(gpui::Entity<Shell>);

    impl gpui::Render for PhoneHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |shell, cx| {
                shell.viewport_width = 390.0;
                let bar = shell.render_compact_nav_bar(cx);
                div()
                    .w(px(390.0))
                    .h(px(800.0))
                    .children(bar)
                    .child(shell.render_chat_sidebar(&Theme::default(), cx))
            })
        }
    }

    fn phone(cx: &mut gpui::TestAppContext) -> (gpui::Entity<Shell>, &mut gpui::VisualTestContext) {
        FORCE_COMPACT.with(|force| force.set(true));
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        let (host, cx) = cx.add_window_view(move |_, cx| {
            PhoneHost(cx.new(|cx| {
                let state = cx.new(|_| crate::state::AppState::new());
                let shell = Shell::new(
                    state,
                    crate::state::EngineBootConfig {
                        data_dir: path.clone(),
                        default_harness: slopty_view_proto::HarnessId::MOCK,
                    },
                    cx,
                );
                shell.state.update(cx, |state, _| {
                    state.workspace_scope = Some(slopty_view_proto::WorkspaceScope::Local);
                    state.local_device_id = Some("local".into());
                    state.chats = vec![serde_json::from_value(serde_json::json!({
                        "id": "fix", "title": "Fix the build", "deviceId": "local",
                        "archived": false, "createdAt": chrono::Utc::now(),
                    }))
                    .unwrap()];
                });
                shell
            }))
        });
        let shell = host.read_with(cx, |host, _| host.0.clone());
        (shell, cx)
    }

    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    // On a phone the new-session page wears a title and a close, the chat a back chevron to
    // Sessions; the rows take a swipe and a long press. Off a phone there is no bar at all.
    #[gpui::test]
    fn the_phone_new_session_page_has_a_close_and_no_history_arrows(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = phone(cx);
        redraw(cx);
        assert!(cx.debug_bounds("compact-row-fix").is_some(), "the row takes gestures");
        // The new-session page: a close, no back.
        assert!(cx.debug_bounds("compact-close").is_some(), "the new-session page's close");
        assert!(cx.debug_bounds("compact-back").is_none());
        // A chat: a back chevron to Sessions, which brings the Sessions page up.
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| state.select_chat(Some("fix".into()), cx));
        });
        redraw(cx);
        assert!(cx.debug_bounds("compact-close").is_none());
        let back = cx.debug_bounds("compact-back").expect("the chat's back chevron").center();
        cx.simulate_click(back, gpui::Modifiers::default());
        cx.update(|_, cx| {
            assert_eq!(cx.default_global::<Pages>().sessions, Some(true), "Sessions is up");
        });
        // A long press opens the row's menu.
        let row = cx.debug_bounds("compact-row-fix").unwrap().center();
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::LongPress(LongPressEvent {
                    phase: TouchPhase::Started,
                    start_position: row,
                    position: row,
                }),
                cx,
            );
        });
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.chat_menu.get().map(|menu| menu.chat_id.as_str()), Some("fix"));
        });
        FORCE_COMPACT.with(|force| force.set(false));
        shell.update(cx, |shell, cx| assert!(shell.render_compact_nav_bar(cx).is_none()));
    }

    // slopty: an empty Sessions page says its own noun and offers no key: a phone has none.
    #[gpui::test]
    fn an_empty_sessions_page_offers_no_key(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = phone(cx);
        shell.update(cx, |shell, cx| shell.state.update(cx, |state, _| state.chats.clear()));
        redraw(cx);
        let page = (
            cx.debug_bounds("sidebar-empty: No sessions yet").is_some(),
            cx.debug_bounds("sidebar-empty-key").is_some(),
        );
        assert_eq!(page, (true, false), "the phone's empty Sessions page");
        FORCE_COMPACT.with(|force| force.set(false));
        cx.simulate_resize(gpui::size(gpui::px(1320.0), gpui::px(880.0)));
        redraw(cx);
        let sidebar = (
            cx.debug_bounds("sidebar-empty: No chats yet").is_some(),
            cx.debug_bounds("sidebar-empty-key").is_some(),
        );
        assert_eq!(sidebar, (true, true), "the desktop sidebar names its key");
        assert_eq!(crate::keys::combo_text_on(true, "mod-n"), "⌘N", "a Mac's key");
    }

    #[test]
    fn the_capsule_says_what_runs_and_what_waits() {
        assert_eq!(summary(0, 0), "");
        assert_eq!(summary(2, 0), "2 working");
        assert_eq!(summary(0, 1), "1 needs you");
        assert_eq!(summary(2, 3), "2 working · 3 need you");
    }
}
