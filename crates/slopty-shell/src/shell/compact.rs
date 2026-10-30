//! slopty: the phone's anatomy, after zeron's iOS app (`apps/ios`), built from the shell's own
//! parts. A phone shows one page at a time: Sessions (the large title, the sidebar's list at the
//! page's width, the "New session" capsule with what is running, and the tab bar of Sessions,
//! Settings and Search) or the chat that was opened from it, whose sidebar button goes back.
//! Every page keeps clear of the system's edges (the status bar, the home indicator) and of the
//! keyboard. Wider windows (an iPad's, a Mac's) keep the shell's columns.
//!
//! The chat or the new-session page wears no desktop chrome there: the titlebar's sidebar and
//! history cluster give way to a navigation bar (zeron iOS's `SessionViewController`: a back
//! chevron to Sessions and the chat's title over "project @ device"; `NewSessionViewController`:
//! "New session" with a close button). A chat row takes zeron iOS's gestures
//! (`SessionListController`): a leading swipe pins it, a trailing swipe archives it (or brings it
//! back from Archived), and a long press opens its menu.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Global, IntoElement, LongPressEvent, Pixels, ScrollEvent,
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
/// The room between the capsule and the tab bar.
const CAPSULE_BELOW: f32 = 8.0;
const PLATE: f32 = 34.0;
const PLATE_INSET: f32 = 7.0;
/// The tab bar's row above the home indicator.
const TAB_BAR: f32 = 49.0;
/// slopty: the narrowest a sidebar's foot holds the toasts; a narrower one hands them to the top
/// right, so a toast never spills over the session beside it.
const TOAST_COLUMN_MIN: f32 = 200.0;
/// The selected tab's glass pill, inset in its third of the bar.
const TAB_PILL_INSET: f32 = 6.0;
/// The capsule's status glyph, before the summary (zeron's 12 pt `StatusGlyph`).
const CAPSULE_GLYPH: f32 = 12.0;
/// The navigation bar's back and close targets.
const NAV_TARGET: f32 = 34.0;
/// The navigation bar's title and subtitle lines, at the default text size.
const NAV_TITLE_LINE: f32 = 20.0;
const NAV_SUBTITLE_LINE: f32 = 15.0;
/// A row's swipe: travel before its axis is decided, travel that commits on release, the reach
/// past which it resists, and the return to rest.
const SWIPE_DECIDE: f32 = 10.0;
const SWIPE_COMMIT: f32 = 76.0;
const SWIPE_REACH: f32 = 112.0;
const SWIPE_SETTLE: Duration = Duration::from_millis(220);
/// slopty: the inspector's sheet on a phone, UIKit's sheet with its medium and large detents:
/// the large detent's gap under the status bar, the share of the screen the medium detent
/// leaves above it, the top corners, the grabber's pill and the header row it heads.
const SHEET_LARGE_GAP: f32 = 10.0;
const SHEET_MEDIUM: f32 = 0.5;
const SHEET_RADIUS: f32 = 24.0;
const SHEET_GRABBER: (f32, f32) = (36.0, 5.0);
const SHEET_HEADER: f32 = 52.0;
/// How far past the medium detent, as a share of what it covers there, a sheet let go is put
/// away; and the last step of a finger's travel read as a flick, which moves it one detent its
/// way whatever the travel.
const SHEET_DISMISS: f32 = 0.3;
const SHEET_FLICK: f32 = 9.0;
/// The glide to rest: the theme's sheet pace on the drawer's curve, which follows a flick.
const SHEET_GLIDE: Duration = Duration::from_millis(300);
const SHEET_CURVE: crate::motion::CubicBezier = crate::motion::CubicBezier::new(0.32, 0.72, 0.0, 1.0);
/// The scrim over what the full sheet leaves above it, in dark-mode terms
/// ([`crate::theme::scrim`]); it deepens as the sheet rises from the medium detent.
const SHEET_DIM: f32 = 0.3;

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

/// slopty: where the phone's inspector sheet rests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Detent {
    /// Half the screen, the session's terminal above it: where it comes up.
    #[default]
    Medium,
    /// The whole screen under the status bar.
    Large,
}

/// slopty: the phone's inspector sheet: where it rests, a finger's travel while it holds it,
/// and its glide to rest or away.
#[derive(Default)]
struct Sheet {
    detent: Detent,
    drag: Option<SheetDrag>,
    glide: Option<Glide>,
    /// Whether the last frame had it up: a sheet put up rises from the screen's foot and one
    /// put away sinks there, whatever put it up or away (⌘I, the bar's button, its own close).
    up: bool,
    /// Its top in the last frame, where a glide or a drag starts.
    top: Option<f32>,
    /// A finger let go of it: the momentum the platform carries on with is its own, not the
    /// scroll of what is now under where the pan started.
    fling: bool,
}

impl Global for Sheet {}

/// A finger on the sheet's header: its travel from the detent, and its last step.
#[derive(Clone, Copy, Debug)]
struct SheetDrag {
    travel: f32,
    last: f32,
}

/// The sheet gliding from `from` since `at`, to its detent, or off the screen when `away`.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: f32,
    at: Instant,
    away: bool,
}

/// Where the sheet's top rests at `detent`, in a window `height` tall whose safe area starts
/// `safe_top` down.
fn detent_top(detent: Detent, height: f32, safe_top: f32) -> f32 {
    let large = safe_top + SHEET_LARGE_GAP;
    match detent {
        Detent::Large => large,
        Detent::Medium => (height * SHEET_MEDIUM).round().max(large),
    }
}

/// Where the sheet's top follows a finger's `travel` from `detent`: with it below the large
/// detent, a quarter as far above it.
fn dragged_top(detent: Detent, travel: f32, height: f32, safe_top: f32) -> f32 {
    let large = detent_top(Detent::Large, height, safe_top);
    let top = detent_top(detent, height, safe_top) + travel;
    if top < large { large - (large - top) * 0.25 } else { top }
}

/// Where a sheet let go at `top` rests, its last step `last` down: a flick moves it one detent
/// its way, else it takes the nearer detent, or `None` (put away) past the medium detent by
/// [`SHEET_DISMISS`] of what it covers there.
fn sheet_rest(top: f32, last: f32, height: f32, safe_top: f32) -> Option<Detent> {
    let large = detent_top(Detent::Large, height, safe_top);
    let medium = detent_top(Detent::Medium, height, safe_top);
    if last >= SHEET_FLICK {
        return (top < medium).then_some(Detent::Medium);
    }
    if last <= -SHEET_FLICK {
        return Some(Detent::Large);
    }
    if top > (height - medium).mul_add(SHEET_DISMISS, medium) {
        None
    } else if top - large <= medium - top {
        Some(Detent::Large)
    } else {
        Some(Detent::Medium)
    }
}

#[cfg(test)]
thread_local! {
    /// Tests lay the phone out on a Mac.
    static FORCE_COMPACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// slopty: a test elsewhere lays its views out as a touch screen's (`on`) or a Mac's.
#[cfg(test)]
pub(crate) fn force_touch(on: bool) {
    FORCE_COMPACT.with(|force| force.set(on));
}

/// Whether this build lays a narrow window out as a phone: iOS, or a test that asks.
fn phone_platform() -> bool {
    touch_screen()
}

/// Whether this build runs on a touch screen, a phone's or an iPad's: iOS, or a test that asks.
/// There focusing a field raises the software keyboard over the page, so opening or landing on a
/// chat leaves the composer unfocused and the keyboard comes when the person taps it, as in
/// Messages (`docs/decisions/ui.md`, "On a touch screen the keyboard waits for a tap").
pub(crate) fn touch_screen() -> bool {
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

/// slopty: one of the phone's navigation bar's icon buttons, a finger's target, named for a
/// screen reader.
fn nav_icon(id: &'static str, glyph: &'static str, label: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    nav_button(id, icon(glyph).size(px(19.0)).text_color(theme.text), label)
}

/// slopty: [`nav_icon`] around a drawn glyph.
fn nav_button(id: &'static str, glyph: impl IntoElement, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Button)
        .aria_label(label)
        .size(px(NAV_TARGET))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .hover(|el| el.bg(crate::theme::wash(0.06)))
        .cursor_pointer()
        .child(glyph)
}

impl Shell {
    /// The window is a phone's.
    pub(super) fn compact(&self) -> bool {
        phone_platform() && self.viewport_width < PHONE_BELOW
    }

    /// The Sessions page is up: the phone opens on it, a chat chosen from it leaves it.
    pub(super) fn compact_sessions_up(&self, cx: &mut App) -> bool {
        let selected = self.state.read(cx).selected_chat.clone();
        let pages = cx.default_global::<Pages>();
        let changed = pages.selected != selected;
        pages.selected = selected;
        if changed && pages.selected.is_some() {
            pages.sessions = Some(false);
        }
        *pages.sessions.get_or_insert(true)
    }

    /// slopty: where the toasts stand, never over a session's terminal, whose TUI draws its
    /// prompt and status at the bottom: on a phone above its Sessions page's tab bar and capsule,
    /// or the home indicator or the keyboard; else in the sidebar's foot, above its bottom row,
    /// or with no sidebar to take them at the top right, under the title strip
    /// (`docs/decisions/ui.md`, "Toasts keep off the terminal").
    pub(super) fn toast_place(&self, window: &Window, cx: &mut App) -> crate::toast::ToastPlace {
        use crate::toast::ToastPlace;
        let insets = window.insets();
        let safe = insets.safe_area;
        if self.compact() {
            let lift = if self.compact_sessions_up(cx) {
                f32::from(safe.bottom) + TAB_BAR + CAPSULE + CAPSULE_BELOW
            } else if self.keyboard_band_held(window) {
                f32::from(insets.effective().bottom)
            } else {
                f32::from(safe.bottom)
            };
            return ToastPlace::Floor { lift };
        }
        let width = self.sidebar_now() - 2.0 * Theme::SPACE_SM;
        if width >= TOAST_COLUMN_MIN {
            let left = f32::from(safe.left) + Theme::SPACE_SM;
            // The sidebar's foot: its bottom row inside a small step all round.
            let row = crate::surface_chrome::touch_row_height(super::SIDEBAR_FOOTER_BUTTON_SIZE);
            let foot = f32::from(safe.bottom) + row + 2.0 * Theme::SPACE_SM;
            return ToastPlace::Column { left, width, foot };
        }
        ToastPlace::Top { top: f32::from(safe.top) + Theme::TITLEBAR_HEIGHT }
    }

    /// Bring the phone's Sessions page up, or take it down for the chat page.
    pub(super) fn set_compact_sessions(up: bool, cx: &mut App) {
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

    /// slopty: whether a field holds the keyboard: the focus is on a text field, the only
    /// thing the soft keyboard comes for. A pressed button (Send) or one of the shell's own
    /// handles holds the focus with no keyboard up.
    pub(super) fn keyboard_band_held(&self, window: &Window) -> bool {
        window.text_input_focused()
    }

    /// `page` kept clear of the system's edges and the keyboard, on iOS. `session`: the page is
    /// a session's column, whose ground the bands it keeps clear take.
    pub(super) fn compact_frame(
        &self,
        page: impl IntoElement,
        session: bool,
        window: &Window,
        cx: &App,
    ) -> gpui::Div {
        if !cfg!(target_os = "ios") {
            return div().size_full().child(page);
        }
        let insets = window.insets();
        let safe = insets.safe_area;
        // slopty: the keyboard's band stays only while a field takes what is typed: once the
        // keyboard drops (a send lets the composer go) the page reaches the safe area again,
        // whatever keyboard frame UIKit last reported.
        let bottom = if self.keyboard_band_held(window) {
            insets.effective().bottom
        } else {
            safe.bottom
        };
        // slopty: under a session the band over the home indicator, and the keyboard's while
        // it rises or stands over it, continues the terminal's ground rather than standing as an
        // empty strip of the window's; on a tablet so does the status bar's band over its title
        // strip, right of the sidebar. A phone's status bar band continues its navigation bar.
        let theme = Theme::of(cx);
        let left = safe.left + px(self.sidebar_now());
        let band = |top: bool, height: Pixels| {
            (session && height > Pixels::ZERO).then(|| {
                let band = div().absolute().left(left).right_0().h(height);
                let band = band.bg(theme.session_ground());
                if top { band.top_0() } else { band.bottom_0() }
            })
        };
        div()
            .size_full()
            .relative()
            // A phone's chat has no sidebar: the window's own fill over the hidden column's
            // edge, which would otherwise stand as a hairline down the screen's left.
            .when(self.compact(), |frame| frame.bg(theme.glass()))
            .children(band(true, safe.top).filter(|_| !self.compact()))
            .children(band(false, bottom))
            .child(page)
            .pt(safe.top)
            .pb(bottom)
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
            let now = crate::clock::utc(cx);
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
        // slopty: before the first chat can start, the capsule offers the step it needs, as the
        // desktop's canvas does, never a new chat that has nowhere to go.
        let step = self.first_step(cx);
        let words = step.map_or("New session", super::spaces::FirstStep::action);
        let label: SharedString = if said.is_empty() {
            words.into()
        } else {
            format!("{words}, {}", said.replace(" · ", ", ")).into()
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
            .child(
                icon(step.map_or(icons::PLUS, super::spaces::FirstStep::glyph))
                    .size(px(15.0))
                    .text_color(theme.accent),
            );
        let capsule = div()
            .id("compact-new-session")
            .debug_selector(|| "compact-new-session".into())
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
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(step) = step {
                    step.take(this, cx);
                    return;
                }
                // TUI-first: the picker is the whole new session, so it stands over Sessions, as
                // the project picker does; the session it starts takes the screen (a chat chosen
                // leaves Sessions), and put away it leaves Sessions as it was, never an empty page.
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
                    .child(words),
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
            .pb(px(CAPSULE_BELOW))
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

    /// The navigation bar's words: the chat's title, and "project @ device" under it.
    fn compact_nav_words(&self, cx: &App) -> (SharedString, Option<SharedString>) {
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
    }

    /// The phone's navigation bar over a chat or the new-session page, in place of the
    /// titlebar and its sidebar and history cluster; `None` off a phone. A chat: a back
    /// chevron to Sessions, its title over "project @ device". The new-chat page: "New session"
    /// and a close button.
    pub(super) fn render_compact_nav_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.compact() {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let (title, subtitle) = self.compact_nav_words(cx);
        let on_canvas = self.state.read(cx).selected_chat.is_none();
        let back = |this: &mut Self, cx: &mut Context<Self>| {
            Self::set_compact_sessions(true, cx);
            this.close_chat_menu(cx);
            // slopty: the inspector's sheet is the session's, and goes with it.
            if this.inspector_shown(cx) {
                this.set_surfaces_open(false, cx);
            }
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
                    .line_height(ui_rems(NAV_TITLE_LINE))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(title),
            )
            .children(subtitle.map(|subtitle| {
                div()
                    .debug_selector(|| "compact-nav-subtitle".into())
                    .max_w_full()
                    .truncate()
                    .text_size(ui_rems(11.5))
                    .line_height(ui_rems(NAV_SUBTITLE_LINE))
                    .text_color(theme.text_muted)
                    .child(subtitle)
            }));
        // slopty: a session's files and its inspector, a finger's reach away (an iPad's titlebar
        // has them; a phone's bar stands in for it). The session is its terminal, so the bar
        // has no terminal of its own to show.
        let inspects = crate::inspector::inspects(self.state.read(cx), &self.active_chat);
        let inspector_up = self.inspector_shown(cx);
        let running_lanes = {
            let state = self.state.read(cx);
            let now = crate::clock::utc(cx);
            state.selected_chat.as_deref().map_or(0, |chat| state.running_subagents_for(chat, now))
        };
        let trail = (!on_canvas).then(|| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(8.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(
                    nav_icon("compact-files", icons::FOLDER_TREE, "Show files", &theme).on_click(cx.listener(
                        |this, _, window, cx| {
                            if !this.files_panel_open(cx) {
                                this.add_files_surface(window, cx);
                            }
                            cx.notify();
                        },
                    )),
                )
                .when(inspects, |trail| {
                    trail.child(
                        nav_button(
                            "compact-inspector",
                            icons::corner_card_glyph(19.0, theme.text),
                            if inspector_up { "Hide inspector" } else { "Show inspector" },
                        )
                        .aria_toggled(if inspector_up {
                            gpui::accesskit::Toggled::True
                        } else {
                            gpui::accesskit::Toggled::False
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_inspector(window, cx)))
                        // The session's running subagents, counted as the titlebar's toggle counts
                        // them.
                        .when(running_lanes > 0, |button| {
                            crate::running_pill::mark_titlebar_button(button, running_lanes, &theme)
                        }),
                    )
                })
        });
        // slopty: zeron's backdrop for what scrolls under a bar: frosted glass in the window's
        // tint (opaque where nothing is frosted), so a card under the title never reads through.
        let bar = div()
            .id("compact-nav-bar")
            .debug_selector(|| "compact-nav-bar".into())
            .role(gpui::Role::Group)
            .aria_label("Navigation")
            .relative()
            .w_full()
            .h(px(Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD))
            .bg(if theme.is_frost() { theme.glass() } else { gpui::Hsla { a: 1.0, ..theme.glass() } })
            .flex()
            .items_center()
            .px(px(8.0))
            .child(heading)
            .child(lead)
            .children(trail);
        Some(crate::frost::frosted(0.0, crate::frost::MENU_BLUR, bar).into_any_element())
    }

    /// slopty: a phone's page over its session for what an iPad shows beside it: the right
    /// pane's surface (a file, the changes, a page) when it is open, else the files explorer,
    /// under a bar whose back goes to the files a file was opened from, else to the inspector
    /// the session's strip holds (a change opened from it), else to the session. The inspector
    /// itself is a sheet over the session ([`Self::render_compact_inspector`]). `None` off a
    /// phone or with neither open.
    pub(super) fn render_compact_side(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.compact() || self.compact_sessions_up(cx) || self.active_chat.is_empty() {
            return None;
        }
        let active = self.resolved_right_active(cx);
        let surface = self.right_pane_open(cx) && active != super::RightSurface::Inspector;
        let files = self.files_panel_open(cx);
        if !surface && !files {
            return None;
        }
        let to_inspector = surface
            && !files
            && crate::inspector::inspects(self.state.read(cx), &self.active_chat)
            && self.right_surface_rows(cx).iter().any(|(s, ..)| *s == super::RightSurface::Inspector);
        let theme = Theme::of(cx).clone();
        let (title, body, back_to): (SharedString, AnyElement, &'static str) = if surface {
            let title = match active {
                super::RightSurface::File(id) => self
                    .file_surfaces
                    .get(&id)
                    .map_or_else(|| "File".into(), |file| file.read(cx).tab_title()),
                super::RightSurface::Diff(_) => "Changes".into(),
                super::RightSurface::Terminal(_) => "Terminal".into(),
                super::RightSurface::Browser(_) => "Browser".into(),
                super::RightSurface::Inspector => "Inspector".into(),
                super::RightSurface::Screen(_) => "Screen".into(),
                super::RightSurface::Page(_) => "Page".into(),
                super::RightSurface::Picker => "Open".into(),
            };
            let back_to = if to_inspector {
                "Inspector"
            } else if files {
                "Files"
            } else {
                "Session"
            };
            (title, self.right_surface_content(cx), back_to)
        } else {
            let key = self.panel_key(cx);
            let Some(explorer) = self.files.get(&key).cloned() else {
                // A pane kept open across launches has its flag, not its explorer yet.
                cx.defer_in(window, |shell, window, cx| shell.add_files_surface(window, cx));
                return None;
            };
            explorer.update(cx, |files, cx| {
                files.ensure_loaded(cx);
                files.ensure_git_status(cx);
            });
            ("Files".into(), explorer.into_any_element(), "Session")
        };
        let page_label = title.clone();
        let back = div()
            .id("compact-side-back")
            .debug_selector(|| "compact-side-back".into())
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Back to {back_to}")))
            .h(px(NAV_TARGET))
            .flex_none()
            .pr(px(8.0))
            .rounded_full()
            .flex()
            .items_center()
            .gap(px(2.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if to_inspector {
                    this.set_right_active(super::RightSurface::Inspector, cx);
                } else if surface {
                    this.set_surfaces_open(false, cx);
                } else {
                    this.close_files_panel(cx);
                }
                cx.notify();
            }))
            .child(icon(icons::ALT_ARROW_LEFT).size(px(22.0)).text_color(theme.text))
            .child(div().text_size(ui_rems(16.0)).text_color(theme.text).child(back_to));
        let bar = div()
            .id("compact-side-bar")
            .role(gpui::Role::Group)
            .aria_label("Navigation")
            .relative()
            .flex_none()
            .w_full()
            .h(px(Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD))
            .bg(if theme.is_frost() { theme.glass() } else { gpui::Hsla { a: 1.0, ..theme.glass() } })
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .px(px(8.0))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(96.0))
                    .right(px(96.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("compact-side-title")
                            .role(gpui::Role::Heading)
                            .aria_label(title.clone())
                            .max_w_full()
                            .truncate()
                            .text_size(ui_rems(17.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(title),
                    ),
            )
            .child(back);
        let insets = window.insets();
        let safe = insets.safe_area;
        let bottom =
            if self.keyboard_band_held(window) { insets.effective().bottom } else { safe.bottom };
        let overlays = self.render_overlays(window.viewport_size(), window, cx);
        Some(
            div()
                .id("compact-side-page")
                .role(gpui::Role::Group)
                .aria_label(page_label)
                .size_full()
                .relative()
                .flex()
                .flex_col()
                .bg(theme.panel_bg())
                .when(cfg!(target_os = "ios"), |el| el.pt(safe.top).pb(bottom).pl(safe.left).pr(safe.right))
                .child(bar)
                .child(div().flex_1().min_h_0().w_full().child(body))
                .children(overlays)
                .into_any_element(),
        )
    }

    /// slopty: the sheet's pan, which a finger starts on its header (`on`): from then on the
    /// gesture is the sheet's, wherever the header has moved from the place the pan started,
    /// which a touch pan's every step is hit-tested at, through the momentum after it is let go.
    fn sheet_pan(&self, on: bool, height: f32, safe_top: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shell = cx.weak_entity();
        gpui::canvas(
            move |bounds, window, _| on.then(|| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)),
            move |_, hitbox, window, _| {
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Capture {
                        return;
                    }
                    let sheet = cx.default_global::<Sheet>();
                    let owned = sheet.drag.is_some() || (sheet.fling && event.momentum_phase.is_some());
                    let starts = event.momentum_phase.is_none()
                        && event.touch_phase == TouchPhase::Started
                        && hitbox.as_ref().is_some_and(|hitbox| hitbox.should_handle_scroll(window));
                    if !owned && !starts {
                        if event.momentum_phase.is_none() {
                            sheet.fling = false;
                        }
                        return;
                    }
                    let _ = shell.update(cx, |this, cx| this.pan_compact_inspector(event, height, safe_top, cx));
                    cx.stop_propagation();
                    window.refresh();
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// One step of a finger on the phone's inspector sheet ([`Self::sheet_pan`]): the sheet
    /// follows it, and comes to rest where [`sheet_rest`] says when let go.
    fn pan_compact_inspector(&mut self, event: &ScrollEvent, height: f32, safe_top: f32, cx: &mut Context<Self>) {
        let (at, glide) = (crate::clock::now(cx), !cx.reduce_motion());
        let sheet = cx.default_global::<Sheet>();
        if let Some(momentum) = event.momentum_phase {
            sheet.fling = momentum != TouchPhase::Ended;
            return;
        }
        let step = f32::from(event.delta.pixel_delta(px(16.0)).y);
        match event.touch_phase {
            TouchPhase::Started | TouchPhase::Moved => {
                if event.touch_phase == TouchPhase::Started {
                    sheet.drag = None;
                }
                // From where it is, mid-glide or at rest.
                let travel = sheet.top.map_or(0.0, |top| top - detent_top(sheet.detent, height, safe_top));
                let drag = sheet.drag.get_or_insert(SheetDrag { travel, last: 0.0 });
                drag.travel += step;
                drag.last = step;
                sheet.glide = None;
                sheet.fling = false;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(drag) = sheet.drag.take() else { return };
                let from = dragged_top(sheet.detent, drag.travel + step, height, safe_top);
                let last = if step == 0.0 { drag.last } else { step };
                let rest = if event.touch_phase == TouchPhase::Cancelled {
                    Some(sheet.detent)
                } else {
                    sheet_rest(from, last, height, safe_top)
                };
                sheet.top = Some(from);
                sheet.fling = true;
                if let Some(detent) = rest {
                    sheet.detent = detent;
                    sheet.glide = glide.then_some(Glide { from, at, away: false });
                } else {
                    // Sinking from where the finger left it.
                    sheet.up = false;
                    sheet.glide = glide.then_some(Glide { from, at, away: true });
                    self.set_surfaces_open(false, cx);
                }
            }
        }
        self.motion_active.set(true);
        cx.notify();
    }

    /// slopty: the inspector on a phone, a sheet up from the screen's foot over the session's
    /// terminal, as UIKit puts an inspector at a compact width. It comes up at half the screen,
    /// the terminal above it, and its header follows a finger to the whole screen under the
    /// status bar or down out of the way; a press on its grabber changes detent. Its close,
    /// ⌘I, the bar's button or a press on what the full sheet dims put it away. Drawn in the
    /// page's own box, which on iOS starts at the safe area, so it reaches the screen's edges
    /// past it. `None` off a phone, or with the inspector neither up nor sinking.
    pub(super) fn render_compact_inspector(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.compact() || self.compact_sessions_up(cx) || self.active_chat.is_empty() {
            let sheet = cx.default_global::<Sheet>();
            (sheet.up, sheet.glide, sheet.drag, sheet.top) = (false, None, None, None);
            return None;
        }
        let up = self.inspector_shown(cx);
        let now = crate::clock::now(cx);
        let reduced = cx.reduce_motion();
        let insets = window.insets();
        let safe = insets.safe_area;
        let ios = cfg!(target_os = "ios");
        let height = f32::from(window.viewport_size().height);
        let safe_top = if ios { f32::from(safe.top) } else { 0.0 };
        // The page's own box on iOS: inside the safe area, and above the keyboard while a
        // field holds it (`compact_frame`).
        let (page_top, page_bottom, page_left, page_right) = if ios {
            let bottom = if self.keyboard_band_held(window) { insets.effective().bottom } else { safe.bottom };
            (safe.top, bottom, safe.left, safe.right)
        } else {
            (px(0.0), px(0.0), px(0.0), px(0.0))
        };
        let sheet = cx.default_global::<Sheet>();
        if up && !sheet.up {
            // Up from the foot, or back up from wherever it was sinking.
            let from = sheet.top.filter(|_| sheet.glide.is_some_and(|glide| glide.away)).unwrap_or(height);
            sheet.detent = Detent::Medium;
            sheet.drag = None;
            sheet.glide = (!reduced).then_some(Glide { from, at: now, away: false });
        } else if !up && sheet.up {
            sheet.drag = None;
            sheet.glide = (!reduced).then(|| Glide { from: sheet.top.unwrap_or(height), at: now, away: true });
        }
        sheet.up = up;
        let rest = detent_top(sheet.detent, height, safe_top);
        let top = if let Some(drag) = sheet.drag {
            dragged_top(sheet.detent, drag.travel, height, safe_top)
        } else if let Some(glide) = sheet.glide {
            let to = if glide.away { height } else { rest };
            let t = now.saturating_duration_since(glide.at).as_secs_f32()
                / SHEET_GLIDE.mul_f32(crate::motion::speed_scale()).as_secs_f32();
            if t >= 1.0 {
                sheet.glide = None;
                to
            } else {
                (to - glide.from).mul_add(SHEET_CURVE.eval(t), glide.from)
            }
        } else {
            rest
        };
        if !up && sheet.glide.is_none() {
            sheet.top = None;
            // A flick that put it away keeps its momentum from what was under it.
            let fling = sheet.fling;
            return fling.then(|| self.sheet_pan(false, height, safe_top, cx).into_any_element());
        }
        sheet.top = Some(top);
        let detent = sheet.detent;
        if sheet.glide.is_some() {
            self.motion_active.set(true);
        }
        let large = detent_top(Detent::Large, height, safe_top);
        let medium = detent_top(Detent::Medium, height, safe_top);
        let dim = if medium > large { ((medium - top) / (medium - large)).clamp(0.0, 1.0) } else { 0.0 };
        let theme = Theme::of(cx).clone();
        let put_away = |this: &mut Self, cx: &mut Context<Self>| {
            this.set_surfaces_open(false, cx);
            cx.notify();
        };
        // What the full sheet dims takes no press but the one that puts it away.
        let scrim = (up && dim > 0.01).then(|| {
            div()
                .id("compact-inspector-scrim")
                .debug_selector(|| "compact-inspector-scrim".into())
                .absolute()
                .top(-page_top)
                .bottom(-page_bottom)
                .left(-page_left)
                .right(-page_right)
                .bg(crate::theme::scrim(SHEET_DIM * dim))
                .occlude()
                .on_click(cx.listener(move |this, _, _, cx| put_away(this, cx)))
        });
        let grabber = div()
            .id("compact-inspector-grabber")
            .debug_selector(|| "compact-inspector-grabber".into())
            .role(gpui::Role::Button)
            .aria_label(if detent == Detent::Large { "Collapse inspector" } else { "Expand inspector" })
            .absolute()
            .top_0()
            .left(px(0.0))
            .right(px(0.0))
            .mx_auto()
            .w(px(SHEET_GRABBER.0 * 3.0))
            .h(px(26.0))
            .flex()
            .justify_center()
            .pt(px(6.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                let (at, glide) = (crate::clock::now(cx), !cx.reduce_motion());
                let sheet = cx.default_global::<Sheet>();
                let from = sheet.top.unwrap_or(medium);
                sheet.detent = if sheet.detent == Detent::Large { Detent::Medium } else { Detent::Large };
                sheet.glide = glide.then_some(Glide { from, at, away: false });
                this.motion_active.set(true);
                cx.notify();
            }))
            .child(
                div()
                    .w(px(SHEET_GRABBER.0))
                    .h(px(SHEET_GRABBER.1))
                    .rounded_full()
                    .bg(theme.text_muted.opacity(0.35)),
            );
        let close = nav_icon("compact-inspector-close", icons::CLOSE, "Close inspector", &theme)
            .bg(crate::theme::wash(0.07))
            .on_click(cx.listener(move |this, _, _, cx| put_away(this, cx)));
        let header = div()
            .id("compact-inspector-header")
            .debug_selector(|| "compact-inspector-header".into())
            .relative()
            .flex_none()
            .w_full()
            .h(px(SHEET_HEADER))
            .flex()
            .items_center()
            .justify_center()
            // The header follows a finger: up to the whole screen, down to half or away.
            .child(self.sheet_pan(true, height, safe_top, cx))
            .child(grabber)
            .child(
                div()
                    .id("compact-inspector-title")
                    .role(gpui::Role::Heading)
                    .aria_label("Inspector")
                    .text_size(ui_rems(17.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("Inspector"),
            )
            .child(div().absolute().top_0().bottom_0().right(px(12.0)).flex().items_center().child(close));
        let fill = if theme.is_frost() { theme.glass() } else { gpui::Hsla { a: 1.0, ..theme.glass() } };
        let body = div()
            .id("compact-inspector-sheet")
            .debug_selector(|| "compact-inspector-sheet".into())
            .role(gpui::Role::Dialog)
            .aria_label("Inspector")
            .size_full()
            .flex()
            .flex_col()
            .rounded_t(px(SHEET_RADIUS))
            .bg(fill)
            .border_t_1()
            .border_color(theme.border)
            .shadow(theme.elevation(crate::theme::Elevation::Floating))
            // Its own presses are its own: nothing under it takes them.
            .occlude()
            .child(header)
            .child(div().flex_1().min_h_0().w_full().child(self.inspector.clone()))
            // The home indicator's room.
            .when(ios, |el| el.pb(safe.bottom));
        let sheet = div()
            .absolute()
            .top(px(top) - page_top)
            .bottom(-page_bottom)
            .left(-page_left)
            .right(-page_right)
            .child(crate::frost::frosted(SHEET_RADIUS, crate::frost::MENU_BLUR, body));
        Some(div().absolute().inset_0().children(scrim).child(sheet).into_any_element())
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
            let now = crate::clock::now(cx);
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
            .on_scroll(cx.listener(move |this, event: &ScrollEvent, window, cx| {
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
        cx.default_global::<Swipes>().settling = Some((chat.clone(), swipe_offset(travel), crate::clock::now(cx)));
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
        let read = a11y(cx);
        assert!(
            read.contains(&(gpui::Role::Label, "No sessions yet".to_owned())),
            "a screen reader reads the empty line: {read:?}"
        );
        FORCE_COMPACT.with(|force| force.set(false));
        cx.simulate_resize(gpui::size(gpui::px(1320.0), gpui::px(880.0)));
        redraw(cx);
        let sidebar = (
            cx.debug_bounds("sidebar-empty: No sessions yet").is_some(),
            cx.debug_bounds("sidebar-empty-key").is_some(),
        );
        assert_eq!(sidebar, (true, true), "the desktop sidebar names its key");
        assert_eq!(crate::keys::combo_text_on(true, "mod-n"), "⌘N", "a Mac's key");
    }

    // slopty: a touch screen's new-session picker offers no keys to press, none at its foot;
    // with a keyboard they stand there.
    #[gpui::test]
    fn a_touch_screens_new_session_picker_offers_no_keys(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = ipad(cx);
        cx.simulate_resize(gpui::size(px(390.0), px(844.0)));
        shell.update(cx, |shell, cx| shell.open_new_session_picker(None, cx));
        redraw(cx);
        let keys = |cx: &mut gpui::VisualTestContext| {
            let mut shown = |id| cx.debug_bounds(id).is_some();
            (shown("new-session-picker"), shown("key-cap-Esc"))
        };
        assert_eq!(keys(cx), (true, false), "the phone's picker, with no key caps");
        FORCE_COMPACT.with(|force| force.set(false));
        redraw(cx);
        assert_eq!(keys(cx), (true, true), "a keyboard's picker names its keys");
    }

    // slopty: before the first chat can start, the capsule offers the step it needs, as the
    // desktop's canvas does: a device first, then a project, and a new chat only once one can.
    #[gpui::test]
    fn before_any_project_the_capsule_offers_the_step_a_chat_needs(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = ipad(cx);
        cx.simulate_resize(gpui::size(px(390.0), px(844.0)));
        shell.update(cx, |shell, cx| shell.state.update(cx, |state, _| state.chats.clear()));
        let offers = |cx: &mut gpui::VisualTestContext| {
            a11y(cx)
                .into_iter()
                .filter(|(role, label)| {
                    *role == gpui::Role::Button
                        && ["Add a", "New session"].iter().any(|start| label.starts_with(start))
                })
                .map(|(_, label)| label)
                .collect::<Vec<_>>()
        };
        assert_eq!(offers(cx), ["Add a device"], "no device: nowhere for a project yet");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "local", "name": "Studio", "platform": "macos",
                    }))
                    .unwrap(),
                ];
            });
        });
        assert_eq!(offers(cx), ["Add a project"], "a device, no project: the canvas's action");
        let read = a11y(cx);
        assert!(read.contains(&(gpui::Role::Label, "No projects yet".to_owned())), "{read:?}");
        let capsule = cx.debug_bounds("compact-new-session").expect("the capsule").center();
        cx.simulate_click(capsule, gpui::Modifiers::default());
        shell.update(cx, |shell, cx| {
            assert!(shell.add_space.is_some(), "the press opens the project picker");
            assert!(shell.compact_sessions_up(cx), "over the Sessions page, which stays");
        });
        // A screen reader reads the picker's devices as buttons, the dot's state in words.
        let read = a11y(cx);
        assert!(
            read.iter().any(|(role, label)| *role == gpui::Role::Button && label.starts_with("Studio, ")),
            "the device row: {read:?}"
        );
        // The project added, the phone lands on its new chat, as the Mac does.
        shell.update(cx, |shell, cx| {
            shell.add_space = None;
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "app", "deviceId": "local", "path": "/work/app",
                        "createdAt": chrono::Utc::now(),
                    }))
                    .unwrap(),
                ];
            });
            shell.land_in_space("app".into(), cx);
            assert!(!shell.compact_sessions_up(cx), "on the new chat's page");
            Shell::set_compact_sessions(true, cx);
        });
        assert_eq!(offers(cx), ["New session"], "back on Sessions, a chat can start");
        // Its press puts the new-session picker over Sessions, and put away it leaves Sessions up.
        let capsule = cx.debug_bounds("compact-new-session").expect("the capsule").center();
        cx.simulate_click(capsule, gpui::Modifiers::default());
        let picked = a11y(cx);
        shell.update(cx, |shell, cx| {
            assert!(shell.new_session.is_some(), "the press opens the new-session picker");
            assert!(shell.compact_sessions_up(cx), "over the Sessions page, which stays");
            shell.close_new_session_picker(cx);
            assert!(shell.compact_sessions_up(cx), "put away, Sessions is as it was");
        });
        assert!(picked.contains(&(gpui::Role::TabList, "Agent".to_owned())), "{picked:?}");
        assert!(picked.contains(&(gpui::Role::Group, "Sessions".to_owned())), "{picked:?}");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces.clear();
                state.no_project = true;
            });
        });
        assert_eq!(offers(cx), ["New session"], "and outside any project");
        FORCE_COMPACT.with(|force| force.set(false));
    }

    /// The full shell on a touch screen laid out as an iPad's: the sidebar beside the chat.
    fn ipad(cx: &mut gpui::TestAppContext) -> (gpui::Entity<Shell>, &mut gpui::VisualTestContext) {
        FORCE_COMPACT.with(|force| force.set(true));
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let path = dir.keep(); // the shell keeps it past this setup
        let (shell, cx) = cx.add_window_view(move |_, cx| {
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
                state.chats = ["fix", "docs"]
                    .into_iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "title": format!("Chat {id}"), "deviceId": "local",
                            "archived": false, "createdAt": chrono::Utc::now(),
                        }))
                        .unwrap()
                    })
                    .collect();
            });
            shell
        });
        // An iPad's screen: the sidebar beside the chat, not the phone's pages.
        cx.simulate_resize(gpui::size(px(1366.0), px(1024.0)));
        shell.update(cx, |shell, cx| {
            shell.debug_gate = Some(crate::state::GatePhase::Ready);
            shell.splash = super::super::SplashPhase::Gone;
            crate::motion::set_reduced_motion(cx, true);
        });
        (shell, cx)
    }

    // slopty: the phone's navigation bar paints a backdrop over its whole width and height, so
    // what scrolls under its title never reads through.
    #[gpui::test]
    fn the_phones_navigation_bar_has_a_backdrop(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = phone(cx);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| state.select_chat(Some("fix".into()), cx));
        });
        redraw(cx);
        let bar = cx.debug_bounds("compact-nav-bar").expect("the navigation bar");
        let (painted, scale, theme) = cx.update(|window, cx| {
            (window.painted_primitives(), window.scale_factor(), Theme::of(cx).clone())
        });
        let fill = if theme.is_frost() { theme.glass() } else { gpui::Hsla { a: 1.0, ..theme.glass() } };
        assert!(fill.a > 0.0, "a fill that hides what is under it");
        let size = format!(
            "size: Size {{ {}px (scaled) × {}px (scaled) }}",
            f32::from(bar.size.width) * scale,
            f32::from(bar.size.height) * scale
        );
        let background = format!("background: Solid({fill:?})");
        let backdrop = painted
            .iter()
            .any(|line| line.contains("Quad") && line.contains(&size) && line.contains(&background));
        FORCE_COMPACT.with(|force| force.set(false));
        assert!(backdrop, "no {background} over the bar's {size}");
    }

    // slopty: on an iPad every control of the shell's own takes a finger's 44 pt both ways
    // (#23); on a Mac the same controls stay as drawn.
    #[gpui::test]
    fn an_ipads_controls_take_a_fingers_press(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = ipad(cx);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| state.select_chat(Some("docs".into()), cx));
        });
        let buttons = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.draw(cx).clear(cx);
                let scale = window.scale_factor();
                let tree = window.a11y_tree().expect("a tree");
                tree.nodes
                    .iter()
                    .filter(|(_, node)| node.role() == gpui::Role::Button)
                    .filter_map(|(_, node)| {
                        let label = node.label()?.to_owned();
                        let rect = node.bounds()?;
                        let (w, h) = ((rect.x1 - rect.x0) as f32 / scale, (rect.y1 - rect.y0) as f32 / scale);
                        Some((label, w, h))
                    })
                    .collect::<Vec<_>>()
            })
        };
        let ours = [
            "Hide sidebar",
            "Back",
            "Forward",
            "New session",
            "Sidebar view options",
            "Show chat details",
            "Show files panel",
            "Show side pane",
            "Settings",
        ];
        // Under Reduce Motion every entrance has landed: the controls stand where they rest.
        cx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
        let touch = buttons(cx);
        let mine: Vec<_> = touch.iter().filter(|(label, _, _)| ours.contains(&label.as_str())).collect();
        let small: Vec<_> = mine.iter().filter(|(_, w, h)| w.min(*h) < 43.9).collect();
        assert!(mine.len() >= 6, "the shell's controls drawn: {touch:?}");
        assert!(small.is_empty(), "under a finger's 44 pt: {small:?}");
        FORCE_COMPACT.with(|force| force.set(false));
        let mac = buttons(cx);
        let back = mac.iter().find(|(label, _, _)| label == "Back").expect("Back on a Mac");
        assert!((back.1 - 24.0).abs() < 0.5 && (back.2 - 24.0).abs() < 0.5, "as drawn on a Mac: {back:?}");
    }

    #[test]
    fn the_capsule_says_what_runs_and_what_waits() {
        assert_eq!(summary(0, 0), "");
        assert_eq!(summary(2, 0), "2 working");
        assert_eq!(summary(0, 1), "1 needs you");
        assert_eq!(summary(2, 3), "2 working · 3 need you");
    }

    // slopty: on a phone a toast stands above the tab bar and the New session capsule, hiding
    // neither; off a phone it rises in the sidebar's foot, above its bottom row, off the
    // terminal (`chrome_tests::a_toast_keeps_off_the_terminal`).
    #[gpui::test]
    fn a_phones_toast_stands_over_its_tabs_and_capsule(cx: &mut gpui::TestAppContext) {
        let (_shell, cx) = ipad(cx);
        cx.simulate_resize(gpui::size(px(390.0), px(844.0)));
        cx.update(|_, cx| {
            cx.set_reduce_motion(true);
            crate::toast::info(cx, "Link copied");
        });
        let bounds = |cx: &mut gpui::VisualTestContext| {
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update(|window, cx| {
                    window.set_a11y_active(true);
                    window.refresh();
                    window.draw(cx).clear(cx);
                });
            }
            cx.update(|window, _| {
                let scale = f64::from(window.scale_factor());
                let tree = window.a11y_tree().expect("a tree");
                let find = |role: gpui::Role, starts: &str| {
                    tree.nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == role
                                && node.label().is_some_and(|label| label.starts_with(starts))
                        })
                        .and_then(|(_, node)| node.bounds())
                        .map(|rect| ((rect.y0 / scale) as f32, (rect.y1 / scale) as f32))
                };
                (
                    find(gpui::Role::Status, "Link copied"),
                    // The capsule: before any device, it offers to add one.
                    find(gpui::Role::Button, "New session").or_else(|| find(gpui::Role::Button, "Add a ")),
                    find(gpui::Role::TabList, "Tabs"),
                    f32::from(window.viewport_size().height),
                )
            })
        };
        let (toast, capsule, tabs, _) = bounds(cx);
        let (toast, capsule, tabs) =
            (toast.expect("the toast"), capsule.expect("the capsule"), tabs.expect("the tabs"));
        assert!(toast.1 <= capsule.0 && toast.1 <= tabs.0, "toast {toast:?}, capsule {capsule:?}, tabs {tabs:?}");
        FORCE_COMPACT.with(|force| force.set(false));
        cx.simulate_resize(gpui::size(px(1320.0), px(880.0)));
        let (toast, _, _, height) = bounds(cx);
        let toast = toast.expect("the toast on a Mac");
        let foot = super::super::SIDEBAR_FOOTER_BUTTON_SIZE + 3.0 * Theme::SPACE_SM;
        assert!((height - toast.1 - foot).abs() < 0.5, "over the sidebar's foot: {toast:?} in {height}");
    }

    /// The last frame's accessibility tree as (role, label), drawn with the tree on.
    fn a11y(cx: &mut gpui::VisualTestContext) -> Vec<(gpui::Role, String)> {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.refresh();
                window.draw(cx).clear(cx);
            });
        }
        cx.update(|window, _| {
            let tree = window.a11y_tree().expect("a tree");
            tree.nodes
                .iter()
                .map(|(_, node)| (node.role(), node.label().unwrap_or_default().to_owned()))
                .collect()
        })
    }

    // slopty: a page that is not shown gives no nodes: on a phone's chat page the Sessions list is
    // off screen, so a screen reader reads none of its rows nor its History button; back on
    // Sessions it reads them again.
    #[gpui::test]
    fn a_phones_chat_page_reads_no_hidden_rows(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = ipad(cx);
        cx.simulate_resize(gpui::size(px(390.0), px(844.0)));
        let rows = |read: &[(gpui::Role, String)]| {
            read.iter()
                .filter(|(role, label)| {
                    *role == gpui::Role::Button && (label.contains("Chat fix") || label.contains("Chat docs"))
                })
                .count()
        };
        let chats = a11y(cx);
        assert!(rows(&chats) >= 2, "the Sessions page reads its rows: {chats:?}");
        shell.update(cx, |shell, cx| shell.open_chat("fix".into(), cx));
        let chat = a11y(cx);
        let back = chat.iter().any(|(_, label)| label == "Back to Sessions");
        let hidden = rows(&chat);
        shell.update(cx, |shell, cx| assert!(shell.compact_back(cx)));
        let again = a11y(cx);
        FORCE_COMPACT.with(|force| force.set(false));
        assert!(back, "the chat page: {chat:?}");
        assert_eq!(hidden, 0, "no hidden row on the chat page: {chat:?}");
        assert!(rows(&again) >= 2, "back on Sessions: {again:?}");
    }

    // slopty: a row's status says what it asks of the person: "Needs you" for a chat waiting on
    // them, "Background work" for an agent at rest waiting on its own command.
    #[gpui::test]
    fn a_rows_status_says_whom_it_waits_on(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = ipad(cx);
        shell.update(cx, |shell, cx| {
            let now = crate::clock::utc(cx);
            shell.state.update(cx, |state, _| {
                let session = |chat: &str, status| slopty_view_proto::Session {
                    last_completed_turn: None,
                    chat_id: chat.into(),
                    device_id: "local".into(),
                    status,
                    started_at: Some(now),
                    updated_at: now,
                    running_subagents: 0,
                    status_since: None,
                    waiting_on: None,
                };
                state.sessions = vec![
                    session("fix", slopty_view_proto::SessionStatus::AwaitingInput),
                    session("docs", slopty_view_proto::SessionStatus::Waiting),
                ];
            });
        });
        let read = a11y(cx);
        FORCE_COMPACT.with(|force| force.set(false));
        let statuses: Vec<&str> = read
            .iter()
            .filter(|(role, _)| *role == gpui::Role::Status)
            .map(|(_, label)| label.as_str())
            .collect();
        assert!(statuses.contains(&"Needs you"), "{statuses:?}");
        assert!(statuses.contains(&"Background work"), "{statuses:?}");
        assert!(!statuses.iter().any(|label| *label == "Input" || *label == "Waiting"), "{statuses:?}");
    }

    // slopty: a phone's session reaches its files and its inspector by touch, as an iPad's
    // titlebar does: the bar's Files opens a page of them whose back returns to the session,
    // and its Inspector puts up a sheet over the session, at half the screen, that a finger
    // takes to the whole screen and down out of the way, and its close puts away. The prompt
    // box is docked there from the start.
    #[gpui::test]
    fn a_phones_session_reaches_its_files_and_its_inspector_sheet(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = ipad(cx);
        cx.simulate_resize(gpui::size(px(390.0), px(844.0)));
        shell.update(cx, |shell, cx| shell.open_chat("docs".into(), cx));
        let has = |read: &[(gpui::Role, String)], role: gpui::Role, label: &str| {
            read.iter().any(|(r, l)| *r == role && l == label)
        };
        let chat = a11y(cx);
        let tap = |cx: &mut gpui::VisualTestContext, selector: &'static str| {
            let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector}")).center();
            cx.simulate_click(at, gpui::Modifiers::default());
        };
        let top = |cx: &mut gpui::VisualTestContext| {
            a11y(cx);
            cx.debug_bounds("compact-inspector-sheet").map(|bounds| f32::from(bounds.origin.y))
        };
        let pan = |cx: &mut gpui::VisualTestContext, steps: &[f32]| {
            let at = cx.debug_bounds("compact-inspector-header").expect("the sheet's header").center();
            let last = steps.len() - 1;
            for (ix, y) in steps.iter().enumerate() {
                cx.simulate_event(ScrollEvent {
                    position: at,
                    delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(*y))),
                    modifiers: gpui::Modifiers::default(),
                    touch_phase: match ix {
                        0 => TouchPhase::Started,
                        ix if ix == last => TouchPhase::Ended,
                        _ => TouchPhase::Moved,
                    },
                    momentum_phase: None,
                    source: gpui::ScrollSource::Touch,
                });
            }
        };
        tap(cx, "compact-files");
        let files = a11y(cx);
        tap(cx, "compact-side-back");
        let back = a11y(cx);
        tap(cx, "compact-inspector");
        let sheet = a11y(cx);
        let medium = top(cx);
        // A flick up takes it to the whole screen, and dims what it leaves above.
        pan(cx, &[0.0, -150.0, -150.0, 0.0]);
        let large = top(cx);
        let full = a11y(cx);
        let dims = cx.debug_bounds("compact-inspector-scrim").is_some();
        // Its grabber takes it back to half.
        tap(cx, "compact-inspector-grabber");
        let again = top(cx);
        // A slow pull well past half puts it away.
        pan(cx, &[0.0, 150.0, 150.0, 2.0, 0.0]);
        let pulled = top(cx);
        let shown_after_pull = shell.read_with(cx, |shell, cx| shell.inspector_shown(cx));
        tap(cx, "compact-inspector");
        let reopened = top(cx);
        tap(cx, "compact-inspector-close");
        let closed = top(cx);
        let shown_after_close = shell.read_with(cx, |shell, cx| shell.inspector_shown(cx));
        FORCE_COMPACT.with(|force| force.set(false));
        assert!(has(&chat, gpui::Role::Button, "Show files"), "{chat:?}");
        assert!(has(&chat, gpui::Role::Button, "Show inspector"), "{chat:?}");
        assert!(!has(&chat, gpui::Role::Button, "Show terminal"), "the session is its terminal: {chat:?}");
        assert!(has(&files, gpui::Role::Heading, "Files"), "the files page: {files:?}");
        assert!(has(&files, gpui::Role::Button, "Back to Session"), "{files:?}");
        assert!(!has(&files, gpui::Role::Button, "Back to Sessions"), "the chat's page gives way: {files:?}");
        assert!(has(&back, gpui::Role::Button, "Back to Sessions"), "back on the chat: {back:?}");
        assert!(!has(&back, gpui::Role::Heading, "Files"), "{back:?}");
        assert!(has(&sheet, gpui::Role::Dialog, "Inspector"), "the sheet: {sheet:?}");
        assert!(has(&sheet, gpui::Role::Button, "Hide inspector"), "{sheet:?}");
        assert!(has(&sheet, gpui::Role::Button, "Back to Sessions"), "over the session's page: {sheet:?}");
        assert!(has(&sheet, gpui::Role::Button, "Expand inspector"), "{sheet:?}");
        assert_eq!(medium, Some(422.0), "up at half the screen");
        assert_eq!(large, Some(SHEET_LARGE_GAP), "the whole screen");
        assert!(has(&full, gpui::Role::Button, "Collapse inspector"), "{full:?}");
        assert!(dims, "what the full sheet leaves above is dimmed");
        assert_eq!(again, Some(422.0), "the grabber's press takes it back to half");
        assert_eq!((pulled, shown_after_pull), (None, false), "pulled away");
        assert_eq!(reopened, Some(422.0), "up again at half");
        assert_eq!((closed, shown_after_close), (None, false), "closed");
    }

    // slopty: a sheet let go rests at the nearer detent, or one detent the way it was flicked,
    // and goes when pulled well past half; above the whole screen it resists.
    #[test]
    fn an_inspector_sheet_rests_where_a_finger_leaves_it() {
        let (height, safe) = (844.0, 47.0);
        let large = detent_top(Detent::Large, height, safe);
        let medium = detent_top(Detent::Medium, height, safe);
        assert_eq!((large, medium), (57.0, 422.0));
        assert_eq!(sheet_rest(200.0, 1.0, height, safe), Some(Detent::Large), "nearer the top");
        assert_eq!(sheet_rest(300.0, -1.0, height, safe), Some(Detent::Medium), "nearer half");
        assert_eq!(sheet_rest(500.0, 1.0, height, safe), Some(Detent::Medium), "short of the pull");
        assert_eq!(sheet_rest(600.0, 1.0, height, safe), None, "pulled away");
        assert_eq!(sheet_rest(400.0, -12.0, height, safe), Some(Detent::Large), "flicked up");
        assert_eq!(sheet_rest(100.0, 12.0, height, safe), Some(Detent::Medium), "flicked down from the top");
        assert_eq!(sheet_rest(430.0, 12.0, height, safe), None, "flicked down from half");
        assert_eq!(dragged_top(Detent::Medium, 100.0, height, safe), 522.0, "with the finger");
        assert_eq!(dragged_top(Detent::Large, -40.0, height, safe), 47.0, "a quarter as far above the top");
    }
}
