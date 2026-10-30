// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! Shared scaffolding for the settings pages.
//!
//! A centered page column, large title, small section labels over filled blocks of hairline-split
//! rows, badges and small buttons, so every page reads as the same product surface.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Pixels, ScrollHandle, SharedString, Window, div, px};

use crate::motion::HoverFade as _;
use crate::popover::{self, MenuScrollbarMetrics, MenuScrollbarState, ScrollRailHost};
use crate::theme::Theme;

/// Width of the section column beside the settings page, published by the
/// shell each frame so dropdowns and responsive pages measure the page pane.
struct SettingsSidebarWidth(f32);

impl gpui::Global for SettingsSidebarWidth {}

/// Publishes the section column's `width` for this frame, so [`dropdown`] and [`column_width`]
/// measure the page pane; the global is set only when the width moved.
pub fn set_sidebar_width(width: f32, cx: &mut gpui::App) {
    if cx.try_global::<SettingsSidebarWidth>().map(|w| w.0) != Some(width) {
        cx.set_global(SettingsSidebarWidth(width));
    }
}

fn sidebar_width(cx: &gpui::App) -> f32 {
    cx.try_global::<SettingsSidebarWidth>().map_or(crate::settings::SIDEBAR_DEFAULT, |w| w.0)
}

/// The page pane: right of the section column, below the titlebar strip.
pub fn pane_bounds(viewport: gpui::Size<Pixels>, sidebar_width: f32) -> gpui::Bounds<Pixels> {
    let left = px(sidebar_width).min(viewport.width);
    let top = px(Theme::TITLEBAR_HEIGHT).min(viewport.height);
    gpui::Bounds::new(
        gpui::point(left, top),
        gpui::size(viewport.width - left, viewport.height - top),
    )
}

/// Inner width of the [`page_column`] at the current window size, for pages
/// that choose a layout by the room they get (the Devices grid).
pub fn column_width(window: &Window, cx: &gpui::App) -> f32 {
    let pane = f32::from(pane_bounds(window.viewport_size(), sidebar_width(cx)).size.width);
    pane.min(PAGE_MAX_WIDTH) - PAGE_PAD_SIDES
}

/// Mounts a dropdown's `content` under its trigger, kept inside the page pane.
///
/// `closing` plays its exit, and the card clears a trigger `trigger_height` tall.
pub fn dropdown(
    id: impl Into<SharedString>,
    content: gpui::Div,
    closing: Option<popover::Closing>,
    trigger_height: f32,
) -> AnyElement {
    SettingsDropdown {
        id: id.into(),
        content,
        closing,
        trigger_height,
        align: popover::MenuAlign::End,
    }
    .into_any_element()
}

#[derive(IntoElement)]
struct SettingsDropdown {
    id: SharedString,
    content: gpui::Div,
    closing: Option<popover::Closing>,
    trigger_height: f32,
    align: popover::MenuAlign,
}

impl RenderOnce for SettingsDropdown {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let limits = dropdown_limits(window.viewport_size(), sidebar_width(cx));
        popover::contained_menu(
            self.id,
            self.content,
            self.closing,
            self.trigger_height,
            limits,
            self.align,
        )
    }
}

fn dropdown_limits(viewport: gpui::Size<Pixels>, sidebar_width: f32) -> gpui::Bounds<Pixels> {
    let pane = pane_bounds(viewport, sidebar_width);
    gpui::Bounds::new(
        pane.origin + gpui::point(px(8.0), px(8.0)),
        gpui::size(
            (pane.size.width - px(16.0)).max(px(1.0)),
            (pane.size.height - px(16.0)).max(px(1.0)),
        ),
    )
}

/// Scroll only a dropdown's options, keeping its frame and optional heading
/// fixed. The available height follows the same placement budget as the card.
pub fn dropdown_rows(
    id: impl Into<SharedString>,
    rows: impl IntoIterator<Item = AnyElement>,
    trigger_height: f32,
    chrome_height: f32,
) -> AnyElement {
    DropdownRows { id: id.into(), rows: rows.into_iter().collect(), trigger_height, chrome_height }
        .into_any_element()
}

/// How tall a dropdown's scrolling rows may grow in `viewport`.
///
/// Half the pane's height beside a trigger `trigger_height` tall, at most 320, less the card's
/// own `chrome_height` (its padding and any heading).
pub fn dropdown_list_height(
    viewport: gpui::Size<Pixels>,
    trigger_height: f32,
    chrome_height: f32,
) -> f32 {
    // Height alone matters here, and the pane's does not depend on the
    // section column's width.
    let limits = pane_bounds(viewport, 0.0);
    let card_height =
        ((f32::from(limits.size.height) - 16.0 - trigger_height) / 2.0 - 6.0).clamp(1.0, 320.0);
    (card_height - chrome_height).max(1.0)
}

#[derive(IntoElement)]
struct DropdownRows {
    id: SharedString,
    rows: Vec<AnyElement>,
    trigger_height: f32,
    chrome_height: f32,
}

impl RenderOnce for DropdownRows {
    fn render(self, window: &mut Window, _: &mut gpui::App) -> impl IntoElement {
        let key: SharedString = format!("{}-list-scroll", self.id).into();
        let scroll = window.with_global_id(key.into(), |id, window| {
            window.with_element_state(id, |previous: Option<ScrollHandle>, _| {
                let scroll = previous.unwrap_or_default();
                (scroll.clone(), scroll)
            })
        });
        let list = div()
            .id(format!("{}-list", self.id))
            .debug_selector(|| "settings-dropdown-list".into())
            .max_h(px(dropdown_list_height(
                window.viewport_size(),
                self.trigger_height,
                self.chrome_height,
            )))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(self.rows);
        crate::edge_fade::edge_faded(12.0, true, true, list).fade_overflow_y(&scroll)
    }
}

/// Owned scroll + floating-scrollbar state for one settings page.
///
/// This is the dedicated settings scroll container state. It wraps the same
/// `MenuScrollbarState` treatment as the model-picker (`pickers.rs`) and the
/// composer popups (`composer.rs`): the rail is hidden until hover/drag and
/// floats above content without consuming layout width.
///
/// Every settings page follows the same shape: a `scroll: PageScroll` field,
/// a [`ScrollRailHost`] impl on the page delegating here, and a root
/// `.relative()` host carrying only the list-hover `on_hover` around the
/// `.overflow_y_scroll().track_scroll(self.scroll.handle())` page, with
/// [`popover::rail`] supplying the rail and all of its listeners.
#[derive(Debug)]
pub struct PageScroll {
    /// Tracked by the page's scrolling list directly — including the appshots
    /// half of the shortcuts page, which renders from its own file.
    scroll: ScrollHandle,
    bar: MenuScrollbarState,
}

impl Default for PageScroll {
    fn default() -> Self {
        Self { scroll: ScrollHandle::new(), bar: MenuScrollbarState::default() }
    }
}

impl ScrollRailHost for PageScroll {
    fn rail_bar(&mut self) -> &mut MenuScrollbarState {
        &mut self.bar
    }

    fn rail_scroll(&self) -> Option<ScrollHandle> {
        Some(self.scroll.clone())
    }
}

impl PageScroll {
    /// The handle the page's scrolling list tracks (`track_scroll`) and fades by.
    pub const fn handle(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Notes the pointer entering or leaving the page; true when the rail changed and the page
    /// should repaint.
    pub fn set_list_hovered(&mut self, hovered: bool) -> bool {
        self.bar.set_list_hovered(hovered)
    }

    /// The `on_hover` listener of a page's host: the rail shows while the pointer is over the
    /// page whose [`PageScroll`] `scroll` reaches.
    pub fn hover_listener<V: 'static>(
        cx: &Context<V>,
        scroll: fn(&mut V) -> &mut Self,
    ) -> impl Fn(&bool, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |view, hovered: &bool, _, cx| {
            if scroll(view).set_list_hovered(*hovered) {
                cx.notify();
            }
        })
    }

    fn set_bar_hovered(&mut self, hovered: bool) -> bool {
        self.bar.set_bar_hovered(hovered)
    }

    fn begin_press(&mut self, pointer_y: Pixels) -> bool {
        self.bar.begin_press(&self.scroll, pointer_y)
    }

    fn drag_to(&self, pointer_y: Pixels) -> bool {
        self.bar.drag_to(&self.scroll, pointer_y)
    }

    fn end_press(&mut self) -> bool {
        self.bar.end_press()
    }

    /// Rewind to the top and drop the rail's activity baseline — what a page
    /// flip must do when one [`PageScroll`] is rerouted at a different list
    /// (shortcuts ↔ appshots), so the new page opens unscrolled and the
    /// offset jump is not read back as scrolling.
    pub fn reset(&mut self) {
        popover::reset_menu_scroll(&self.scroll, &mut self.bar);
    }

    /// Records scroll activity, then computes the rail geometry. Call once
    /// per render ([`rail`] pairs this with the hide-countdown scheduling).
    fn metrics(&mut self) -> Option<MenuScrollbarMetrics> {
        self.bar.note_scroll(&self.scroll);
        MenuScrollbarState::metrics(&self.scroll)
    }
}

/// [`popover::rail`] for a [`PageScroll`] that is not its view's only rail.
///
/// `popover::rail` binds to the view's single [`ScrollRailHost`] impl, and a page carrying a
/// second, menu-local scroll host (the appearance page's interface-font dropdown) cannot route
/// that impl at both. The wiring mirrors [`popover::rail`]; `reach` re-borrows the state from the
/// view inside the strip's pointer listeners, which fire with a `&mut V`.
pub fn rail<V: 'static>(
    scroll: &mut PageScroll,
    id: &'static str,
    theme: &Theme,
    cx: &Context<V>,
    reach: impl Fn(&mut V) -> &mut PageScroll + Copy + 'static,
) -> Option<AnyElement> {
    let metrics = scroll.metrics()?;
    popover::schedule_scrollbar_hide(&mut scroll.bar, move |view: &mut V| &mut reach(view).bar, cx);
    let (now, reduced) = (cx.frame_time(), cx.reduce_motion());
    let strip = scroll.bar.render_rail(theme, metrics, now, reduced)?;
    // Its fade is the shell rail's, a GPUI animation from the linger's wake.
    Some(popover::fading_rail(
        strip
            .id(id)
            .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                if reach(view).set_bar_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                    if reach(view).begin_press(event.position.y) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }),
            )
            .on_drag(popover::MenuScrollbarDrag, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| popover::MenuScrollbarDragGhost)
            })
            .on_drag_move(cx.listener(
                move |view, event: &gpui::DragMoveEvent<popover::MenuScrollbarDrag>, _, cx| {
                    if reach(view).drag_to(event.event.position.y) {
                        cx.notify();
                    }
                },
            ))
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(move |view, _: &gpui::MouseUpEvent, _, cx| {
                    reach(view).end_press();
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(move |view, _: &gpui::MouseUpEvent, _, cx| {
                    reach(view).end_press();
                    cx.notify();
                }),
            ),
        &scroll.bar,
        id,
    ))
}

/// Shared typography for a settings component's title and description. The
/// Shortcuts page established this compact rhythm; list-style settings reuse
/// it instead of drifting by page.
pub const ROW_TITLE_SIZE: f32 = 13.0;
/// The description's size under a row title.
pub const ROW_DESCRIPTION_SIZE: f32 = 12.0;
/// The air between a row title and its description.
pub const ROW_DESCRIPTION_GAP: f32 = 4.0;

/// The page column's outer max width and side padding: a centered ~680px
/// content measure with generous air on either side.
const PAGE_MAX_WIDTH: f32 = 760.0;
const PAGE_PAD_X: f32 = 40.0;
/// Both of the column's side paddings.
const PAGE_PAD_SIDES: f32 = 2.0 * PAGE_PAD_X;

/// Centered page column under the titlebar strip.
pub fn page_column() -> gpui::Div {
    div()
        .w_full()
        .max_w(px(PAGE_MAX_WIDTH))
        .mx_auto()
        .px(px(PAGE_PAD_X))
        // Titlebar clearance lives inside the scroll so content can scroll
        // up to the window edge and fade there, mirroring the bottom.
        .pt(px(Theme::TITLEBAR_HEIGHT + 16.0))
        .pb(px(48.0))
        .flex()
        .flex_col()
}

/// Page headline row: a large title, optionally followed by a muted count on
/// the same baseline. Inset to line up with the section labels.
pub fn page_header(theme: &Theme, title: &str, count: Option<usize>) -> gpui::Div {
    div()
        .px(px(SECTION_LABEL_INSET))
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(10.0))
        .child(
            div()
                .text_size(crate::typography::ui_rems(20.0))
                .line_height(crate::typography::ui_rems(26.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(title.to_owned())),
        )
        .when_some(count, |el, count| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("{count}"))),
            )
        })
}

/// Subtitle under the headline: `mt-1 text-[13px] text-muted-foreground`.
pub fn page_subtitle(theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
    div()
        .mt(px(4.0))
        .px(px(SECTION_LABEL_INSET))
        .min_w_0()
        .text_size(crate::typography::ui_rems(13.0))
        .line_height(crate::typography::ui_rems(17.0))
        .text_color(theme.text_muted)
        .child(copy.into())
}

// Every page opens one way (T3's settings header): the title on a 32-tall row with the
// page's one action centred on it at the right edge, and under it a line saying what the page
// is for. Pages put the action on three heights and explained themselves on four of nine.
/// A settings page's head: [`page_header`] on a 32 row with `action` right on it, and
/// [`page_subtitle`] under it.
pub fn page_head(
    theme: &Theme,
    title: &str,
    count: Option<usize>,
    subtitle: impl Into<SharedString>,
    action: Option<AnyElement>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .debug_selector(|| "page-head-row".to_owned())
                .min_h(px(PAGE_HEAD_ROW))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(16.0))
                .child(page_header(theme, title, count).flex_1().min_w_0())
                .children(action.map(|action| {
                    div()
                        .debug_selector(|| "page-head-action".to_owned())
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(action)
                })),
        )
        .child(page_subtitle(theme, subtitle).debug_selector(|| "page-head-subtitle".to_owned()))
}

/// The height of a settings page's title row (`page_head`).
pub const PAGE_HEAD_ROW: f32 = 32.0;

/// Small label above a group of controls (`text-[13px] font-medium`) — the
/// "Theme" caption over a picker, not a page headline.
pub fn field_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(crate::typography::ui_rems(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(label.into())
}

/// A row of equally-sized preview cards for picking one of N *visual* options.
///
/// Deliberately knows nothing about themes: the caller supplies each preview as
/// an arbitrary element and picks however many cards it wants, so the same
/// control works for a density picker, a layout picker or anything else where
/// the choice is easier to show than to describe. Pair with [`option_card`].
pub fn option_card_row() -> gpui::Div {
    div().flex().flex_row().items_start().gap(px(16.0)).w_full()
}

/// Default height of an [`option_card`] preview frame.
pub const OPTION_CARD_HEIGHT: f32 = 148.0;
/// Corner radius of the preview frame.
///
/// Public because the preview has to round *itself* to this. gpui content masks
/// are axis-aligned rectangles, so `overflow_hidden` on the frame clips to its
/// bounding box and not to its corner radius — a preview that paints its own
/// background will square off the corners and cover the frame's border with it.
pub const OPTION_CARD_RADIUS: f32 = 6.0;

/// One card in an [`option_card_row`]: a fixed-height preview frame with a quiet
/// selected edge and caption underneath. There is deliberately no outer card
/// or ring; the preview itself is the control.
///
/// `preview` fills the frame and **must round its own corners** to
/// [`OPTION_CARD_RADIUS`] if it paints a background — see that constant.
///
/// Returns a plain `Div` like the rest of this module — the caller adds `.id(..)`
/// and `.on_click(..)`, so selection behaviour stays with the page that owns the
/// state.
pub fn option_card(
    theme: &Theme,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    selection_t: f32,
    preview: AnyElement,
) -> gpui::Div {
    let selected_color = crate::motion::mix(theme.text_muted, theme.accent, selection_t);
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .child(
            div()
                .h(px(OPTION_CARD_HEIGHT))
                .flex_none()
                .w_full()
                .rounded(px(OPTION_CARD_RADIUS))
                .overflow_hidden()
                .border_1()
                .border_color(crate::motion::mix(theme.border, theme.accent, selection_t))
                .child(preview),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(if selected {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(selected_color)
                .child(crate::icons::icon(icon_path).size(px(16.0)).text_color(selected_color))
                .child(label.into()),
        )
}

/// Section labels and page titles sit a little inside the blocks' edge.
const SECTION_LABEL_INSET: f32 = 8.0;
/// Air between one section and the next.
const SECTION_GAP: f32 = 32.0;

/// The small plain label above a block ("Startup", "Fonts"): sentence case,
/// muted, never an uppercase eyebrow.
pub fn section_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .px(px(SECTION_LABEL_INSET))
        .text_size(crate::typography::ui_rems(13.0))
        .line_height(crate::typography::ui_rems(17.0))
        .text_color(theme.text_muted)
        .child(label.into())
}

/// A [`section_label`] for a group nested inside a block row.
///
/// An expanded provider's "Completion", "Accounts": flush with the row text it sits under, and as
/// tall as a small button so a trailing action never shifts it.
pub fn details_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_h(px(32.0))
        .flex()
        .flex_row()
        .items_center()
        .text_size(crate::typography::ui_rems(13.0))
        .line_height(crate::typography::ui_rems(17.0))
        .text_color(theme.text_muted)
        .child(label.into())
}

/// A labeled section: the label, then its block tucked close underneath.
/// Pass a [`section_card`] (or any block) as `block`.
pub fn section(
    theme: &Theme,
    label: impl Into<SharedString>,
    block: impl IntoElement,
) -> gpui::Div {
    div()
        .mt(px(SECTION_GAP))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(section_label(theme, label))
        .child(block)
}

/// The block's fill: a faint solid tint of the theme's ink, no outline.
pub fn block_fill(theme: &Theme) -> gpui::Hsla {
    theme.wash(0.045)
}

/// The hairline between rows inside a block.
pub fn row_divider(theme: &Theme) -> gpui::Hsla {
    theme.border.opacity(0.6)
}

/// A filled, rounded block of related settings rows. Standalone blocks keep
/// a section's worth of air above them; inside [`section`] the wrapper owns
/// the spacing and zeroes this margin.
pub fn section_card(theme: &Theme) -> gpui::Div {
    div().mt(px(24.0)).rounded(px(12.0)).bg(block_fill(theme)).overflow_hidden().flex().flex_col()
}

/// One row in a [`section_card`]: title and muted description on the left,
/// the control on the right, split from the previous row by a hairline
/// inset to the text edge.
pub fn card_row(theme: &Theme, first: bool) -> gpui::Div {
    div()
        .mx(px(16.0))
        .py(px(12.0))
        .min_h(px(60.0))
        .when(!first, |el| el.border_t_1().border_color(row_divider(theme)))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(16.0))
}

/// Bare leading glyph for rows whose icon carries identity (a device's
/// platform), sized to sit beside a two-line title.
pub fn row_tile(theme: &Theme, icon_path: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .w(px(24.0))
        .h(px(32.0))
        .flex()
        .items_center()
        .justify_center()
        .child(crate::icons::icon(icon_path).size(px(16.0)).text_color(theme.text_muted))
}

/// Row title. These metrics intentionally match the Shortcuts rows, whose
/// title/description rhythm is the reference for the other settings cards.
pub fn row_title(theme: &Theme, title: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_w_0()
        .text_size(crate::typography::ui_rems(ROW_TITLE_SIZE))
        .line_height(crate::typography::ui_rems(17.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(title.into())
}

/// The quiet meta line under a row title: `text-[12px]
/// text-muted-foreground/65` fragments joined by dots.
pub fn meta_line(theme: &Theme, fragments: Vec<AnyElement>) -> gpui::Div {
    let mut line = div()
        // 4 under the title, opencode's settings leading, where 1 pressed the two lines
        // into one block.
        .mt(px(ROW_DESCRIPTION_GAP))
        .min_w_0()
        .max_w_full()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(px(8.0))
        .gap_y(px(2.0))
        .text_size(crate::typography::ui_rems(ROW_DESCRIPTION_SIZE))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.text_muted);
    let mut first = true;
    for fragment in fragments {
        if !first {
            line = line.child(
                div().text_color(theme.text_muted.opacity(0.3)).child(SharedString::from("·")),
            );
        }
        line = line.child(div().min_w_0().max_w_full().child(fragment));
        first = false;
    }
    line
}

/// Right-anchored badge pill: a filled wash, like the settings blocks it
/// sits on (no outline).
pub fn badge(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .px(px(8.0))
        .py(px(2.0))
        .rounded_full()
        .bg(theme.wash(0.08))
        .text_size(crate::typography::ui_rems(10.5))
        .text_color(theme.text_muted)
        .child(label.into())
}

/// The width of a [`toggle_switch`]'s pill.
pub const SWITCH_WIDTH: f32 = 44.8;
const SWITCH_HEIGHT: f32 = 28.8;
pub(crate) const SWITCH_TRACK_HEIGHT: f32 = 20.8;
const SWITCH_SIDE_INSET: f32 = 1.6;
const SWITCH_THUMB_WIDTH: f32 = 24.0;
const SWITCH_THUMB_HEIGHT: f32 = SWITCH_TRACK_HEIGHT - 2.0 * SWITCH_SIDE_INSET;
/// How far the thumb travels from off to on.
const SWITCH_TRAVEL: f32 = SWITCH_WIDTH - SWITCH_THUMB_WIDTH - 2.0 * SWITCH_SIDE_INSET;
const SWITCH_MARK_SIZE: f32 = 7.2;

/// A pill switch with the on/off marks nested beneath a sliding thumb.
///
/// The caller owns activation and accessibility; only the thumb interpolates.
/// The element is the pill itself, its row's room kept by a margin, so a
/// focus ring on it hugs the track.
pub fn toggle_switch(theme: &Theme, on: bool, key: impl Into<SharedString>) -> gpui::Div {
    let key: SharedString = key.into();
    div()
        .flex_none()
        .w(px(SWITCH_WIDTH))
        .h(px(SWITCH_TRACK_HEIGHT))
        // The pill is the element, so the ring follows it (M8).
        .my(px((SWITCH_HEIGHT - SWITCH_TRACK_HEIGHT) / 2.0))
        .rounded_full()
        .child(SwitchVisual {
            theme: theme.clone(),
            on,
            key: format!("settings-switch-{key}").into(),
        })
}

#[derive(IntoElement)]
struct SwitchVisual {
    theme: Theme,
    on: bool,
    key: SharedString,
}

/// Where a two-state travel ends: 1 for on, 0 for off.
fn travel_end(on: bool) -> f32 {
    f32::from(u8::from(on))
}

struct SwitchTravel {
    from: f32,
    on: bool,
    started: std::time::Instant,
}

impl SwitchTravel {
    fn value(&self, now: std::time::Instant) -> f32 {
        let t = (now.duration_since(self.started).as_secs_f32() / 0.18).min(1.0);
        (travel_end(self.on) - self.from).mul_add(1.0 - (1.0 - t).powi(3), self.from)
    }
}

fn switch_track_color(theme: &Theme, on: bool) -> gpui::Hsla {
    let dark = theme.appearance.is_dark();
    if on {
        if dark {
            // Keep the accent saturated and opaque, but give the enabled
            // track more depth against the dark settings surface.
            crate::theme::flatten(gpui::black().opacity(0.14), theme.accent_strong)
        } else {
            // Preserve the current light opaque treatment.
            let accent = theme.accent;
            crate::theme::flatten(
                gpui::hsla(accent.h, accent.s, (1.0 - accent.l).mul_add(0.10, accent.l), 0.98),
                theme.surface,
            )
        }
    } else {
        let opacity = match (dark, theme.is_frost()) {
            (true, true) => 0.22,
            (true, false) => 0.18,
            (false, true) => 0.12,
            (false, false) => 0.10,
        };
        crate::theme::flatten(theme.ink(opacity), theme.surface)
    }
}

fn switch_thumb_color(theme: &Theme) -> gpui::Hsla {
    let white = if theme.is_frost() {
        if theme.appearance.is_dark() { 0.94 } else { 0.96 }
    } else if theme.appearance.is_dark() {
        0.96
    } else {
        1.0
    };
    crate::theme::flatten(gpui::white().opacity(white), theme.surface)
}

/// Frosted switches catch a little light across their rim and thumb. Both
/// gradient stops are composited to opaque colors before painting.
fn switch_surface_tones(theme: &Theme, base: gpui::Hsla, thumb: bool) -> (gpui::Hsla, gpui::Hsla) {
    if !theme.is_frost() {
        return (base, base);
    }
    let (light, shade) = if thumb { (0.12, 0.07) } else { (0.07, 0.09) };
    (
        crate::theme::flatten(gpui::white().opacity(light), base),
        crate::theme::flatten(gpui::black().opacity(shade), base),
    )
}

impl RenderOnce for SwitchVisual {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let now = crate::clock::now(cx);
        let on = self.on;
        let target = travel_end(on);
        let reduced = crate::motion::reduced_motion(cx);
        let position = window.with_global_id(self.key.into(), |id, window| {
            window.with_element_state(id, |previous: Option<SwitchTravel>, _| {
                let mut travel =
                    previous.unwrap_or(SwitchTravel { from: target, on, started: now });
                let current = travel.value(now);
                if travel.on != on {
                    travel = SwitchTravel { from: current, on, started: now };
                }
                if reduced {
                    travel.from = target;
                }
                (travel.value(now), travel)
            })
        });
        if (position - target).abs() > 0.001 {
            window.request_animation_frame();
        }
        let dark = self.theme.appearance.is_dark();
        let track = switch_track_color(&self.theme, self.on);
        let (track_light, track_shade) = switch_surface_tones(&self.theme, track, false);
        let thumb = switch_thumb_color(&self.theme);
        let (thumb_light, thumb_shade) = switch_surface_tones(&self.theme, thumb, true);
        let empty_width = SWITCH_WIDTH - SWITCH_THUMB_WIDTH - SWITCH_SIDE_INSET;
        let mark_padding = (empty_width - SWITCH_MARK_SIZE) / 2.0;
        let thumb_left = SWITCH_TRAVEL.mul_add(position, SWITCH_SIDE_INSET);
        let track_element = div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(SWITCH_WIDTH))
            .h(px(SWITCH_TRACK_HEIGHT))
            .rounded_full()
            .bg(gpui::linear_gradient(
                180.0,
                gpui::linear_color_stop(track_light, 0.0),
                gpui::linear_color_stop(track_shade, 1.0),
            ))
            .border_1()
            .border_color(if self.on {
                crate::theme::flatten(
                    gpui::white().opacity(if self.theme.is_frost() { 0.16 } else { 0.12 }),
                    track,
                )
            } else {
                crate::theme::flatten(self.theme.border, track)
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .px(px(mark_padding))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .size(px(SWITCH_MARK_SIZE))
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(position)
                            .child(
                                div()
                                    .w(px(1.2))
                                    .h(px(7.2))
                                    .rounded_full()
                                    .bg(gpui::white().opacity(0.96)),
                            ),
                    )
                    .child(
                        div()
                            .size(px(SWITCH_MARK_SIZE))
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(1.0 - position)
                            .child(
                                div()
                                    .size(px(6.4))
                                    .rounded_full()
                                    .border(px(1.0))
                                    .border_color(gpui::white().opacity(0.92)),
                            ),
                    ),
            );
        let thumb_element = div()
            .absolute()
            .top(px(SWITCH_SIDE_INSET))
            .left(px(thumb_left))
            .w(px(SWITCH_THUMB_WIDTH))
            .h(px(SWITCH_THUMB_HEIGHT))
            .rounded_full()
            .bg(gpui::linear_gradient(
                180.0,
                gpui::linear_color_stop(thumb_light, 0.0),
                gpui::linear_color_stop(thumb_shade, 1.0),
            ))
            .border_1()
            .border_color(crate::theme::flatten(
                gpui::black().opacity(if dark { 0.10 } else { 0.08 }),
                thumb,
            ))
            // The rim highlight only belongs to the on state; it fades with
            // the thumb's travel so switching off doesn't pop.
            .when(self.theme.is_frost() && position > 0.001, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(1.6))
                        .left(px(7.2))
                        .w(px(9.6))
                        .h(px(1.0))
                        .opacity(position)
                        .rounded_full()
                        .bg(crate::theme::flatten(gpui::white().opacity(0.45), thumb_light)),
                )
            });
        div()
            .relative()
            .w(px(SWITCH_WIDTH))
            .h(px(SWITCH_TRACK_HEIGHT))
            .child(track_element)
            .child(thumb_element)
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;

    #[test]
    fn tab_selection_reverses_from_its_current_opacity() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let forward = TabSelectionTravel { from: 0.0, on: true, started: start };
        let halfway = start + Duration::from_millis(75);
        let current = forward.value(halfway);
        let reverse = TabSelectionTravel { from: current, on: false, started: halfway };
        assert_eq!(reverse.value(halfway), current);
        assert_eq!(
            reverse.value(
                halfway + crate::motion::TAB_SLIDE.total().mul_f32(crate::motion::speed_scale())
            ),
            0.0
        );
    }

    #[test]
    fn dropdowns_stay_in_the_settings_content_pane() {
        for (width, sidebar) in [(600.0, 224.0), (1200.0, 256.0), (1200.0, 400.0)] {
            let viewport = gpui::size(px(width), px(700.0));
            let pane = pane_bounds(viewport, sidebar);
            let limits = dropdown_limits(viewport, sidebar);
            assert_eq!(pane.left(), px(sidebar));
            assert_eq!(pane.right(), px(width));
            assert!(limits.left() >= pane.left());
            assert!(limits.right() <= pane.right());
            assert!(limits.top() >= px(Theme::TITLEBAR_HEIGHT));
            assert!(limits.bottom() <= pane.bottom());
        }
    }

    #[test]
    fn switch_material_keeps_dark_accent_and_opaque_fills() {
        use slopty_shell_theme::SurfaceTreatment;

        let mut dark = Theme::dark();
        dark.surface_treatment = SurfaceTreatment::Opaque;
        let dark_on = switch_track_color(&dark, true);
        assert_eq!(dark_on.a, 1.0);
        assert!(dark_on.l < dark.accent_strong.l);
        assert_eq!(switch_track_color(&dark, false).a, 1.0);
        assert_eq!(switch_thumb_color(&dark).a, 1.0);
        assert_eq!(switch_surface_tones(&dark, dark_on, false), (dark_on, dark_on));
        let opaque_off = switch_track_color(&dark, false);

        dark.surface_treatment = SurfaceTreatment::Frosted;
        assert_eq!(switch_track_color(&dark, true), dark_on);
        assert_eq!(switch_track_color(&dark, false).a, 1.0);
        assert_eq!(switch_thumb_color(&dark).a, 1.0);
        assert_ne!(switch_track_color(&dark, false), opaque_off);
        for (base, thumb) in [(dark_on, false), (switch_thumb_color(&dark), true)] {
            let (light, shade) = switch_surface_tones(&dark, base, thumb);
            assert_eq!((light.a, shade.a), (1.0, 1.0));
            assert!(light.l > base.l && shade.l < base.l);
        }

        let mut light = Theme::light();
        light.surface_treatment = SurfaceTreatment::Opaque;
        assert_eq!(switch_track_color(&light, true).a, 1.0);
        assert!(switch_track_color(&light, true).l > light.accent.l);
        let light_on = switch_track_color(&light, true);
        assert_eq!(switch_surface_tones(&light, light_on, false), (light_on, light_on));
    }

    #[test]
    fn switch_reversal_keeps_current_position_and_settles() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let forward = SwitchTravel { from: 0.0, on: true, started: now };
        let halfway = now + Duration::from_millis(90);
        let current = forward.value(halfway);
        let reverse = SwitchTravel { from: current, on: false, started: halfway };
        assert_eq!(reverse.value(halfway), current);
        assert_eq!(reverse.value(halfway + Duration::from_millis(180)), 0.0);
        assert_eq!(forward.value(now + Duration::from_millis(180)), 1.0);
    }

    struct Switched;

    impl Render for Switched {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::dark();
            div()
                .flex()
                .flex_col()
                .items_start()
                .p(px(10.0))
                .child(
                    toggle_switch(&theme, true, "ringed")
                        .id("ringed")
                        .tab_index(0)
                        .map(|el| crate::surface_chrome::focus_ring(el, &theme)),
                )
                .child(div().w(px(10.0)).h(px(10.0)).bg(gpui::red()))
        }
    }

    /// The switch is the pill, so the keyboard's ring hugs the track 2 px clear, and the row
    /// keeps the switch's 28.8 pt (M8).
    #[gpui::test]
    fn the_switch_ring_hugs_its_pill_and_its_row_keeps_its_height(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::dark()));
        let (_, cx) = cx.add_window_view(|_, _| Switched);
        cx.simulate_keystrokes("a");
        cx.update(Window::focus_next);
        let (quads, scale) = cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            (window.painted_quads(), window.scale_factor())
        });
        let points = |b: gpui::Bounds<gpui::ScaledPixels>| {
            [
                b.origin.x.0 / scale,
                b.origin.y.0 / scale,
                b.size.width.0 / scale,
                b.size.height.0 / scale,
            ]
        };
        // Quads land on whole pixels, so 28.8 is drawn within half a pixel.
        let accent = Theme::dark().accent;
        let ring = quads.iter().find(|q| q.border_color == accent).map(|q| points(q.bounds));
        let reach = crate::surface_chrome::RING_OFFSET + crate::surface_chrome::RING_WIDTH;
        let [x, y, w, h] = ring.expect("the keyboard rings the switch");
        assert!((w - 2.0_f32.mul_add(reach, SWITCH_WIDTH)).abs() <= 0.5, "width {w}");
        assert!((h - 2.0_f32.mul_add(reach, SWITCH_TRACK_HEIGHT)).abs() <= 0.5, "height {h}");
        let top = y + reach - (SWITCH_HEIGHT - SWITCH_TRACK_HEIGHT) / 2.0;
        assert!((x + reach - 10.0).abs() <= 0.5 && (top - 10.0).abs() <= 0.5, "at {x} {y}");
        let below = quads
            .iter()
            .find(|q| q.background == gpui::Background::from(gpui::red()))
            .map(|q| points(q.bounds))
            .unwrap();
        assert!((below[1] - (10.0 + SWITCH_HEIGHT)).abs() <= 0.5, "the row keeps 28.8: {below:?}");
    }
}

struct TabSelectionTravel {
    from: f32,
    on: bool,
    started: std::time::Instant,
}

impl TabSelectionTravel {
    fn value(&self, now: std::time::Instant) -> f32 {
        let seconds = crate::motion::TAB_SLIDE.total().as_secs_f32() * crate::motion::speed_scale();
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let progress = crate::motion::TAB_SLIDE.progress((elapsed / seconds).min(1.0));
        (travel_end(self.on) - self.from).mul_add(progress, self.from)
    }
}

/// Retargetable selected-state fade for a settings tab. First paint and
/// reduced-motion changes settle immediately instead of flashing an entrance.
pub fn tab_selection_t(
    window: &mut Window,
    key: impl Into<SharedString>,
    selected: bool,
    reduced_motion: bool,
) -> f32 {
    let now = window.frame_time();
    let target = travel_end(selected);
    let value = window.with_global_id(key.into().into(), |id, window| {
        window.with_element_state(id, |previous: Option<TabSelectionTravel>, _| {
            let mut travel =
                previous.unwrap_or(TabSelectionTravel { from: target, on: selected, started: now });
            let current = travel.value(now);
            if travel.on != selected {
                travel = TabSelectionTravel { from: current, on: selected, started: now };
            }
            if reduced_motion {
                travel.from = target;
            }
            (travel.value(now), travel)
        })
    });
    if (value - target).abs() > 0.001 {
        window.request_animation_frame();
    }
    value
}

/// One treatment for settings section tabs: the selected wash and text ease
/// over Slopty's tab timing, while hover keeps the normal sidebar color fade.
pub fn section_tab(
    theme: &Theme,
    selected: bool,
    selection_t: f32,
    id: impl Into<SharedString>,
    hover_key: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let hover_key = hover_key.into();
    let base_bg =
        crate::motion::mix(crate::theme::wash(0.0), crate::theme::glass_selected_bg(), selection_t);
    let base_text = crate::motion::mix(theme.text_muted, theme.text, selection_t);
    let hover_bg = if selected { base_bg } else { theme.glass_hover() };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .rounded(px(8.0))
        .px(px(Theme::SPACE_SM))
        .py(px(6.0))
        .min_h(px(32.0))
        .flex_shrink_0()
        .text_size(crate::typography::ui_rems(13.0))
        .when(selected, |el| el.font_weight(gpui::FontWeight::MEDIUM))
        .text_color(crate::motion::hover_blend(&hover_key, base_text, theme.text))
        .bg(crate::motion::hover_blend(&hover_key, base_bg, hover_bg))
        .id(id.into())
        .hover_fade(hover_key)
}

/// Height of every settings select trigger: centered in a 60px card row.
pub const SELECT_HEIGHT: f32 = 32.0;
/// Menus never shrink below this, so short values still open a readable list.
const SELECT_MENU_MIN_WIDTH: f32 = 160.0;

/// A select's fill on a settings block: a translucent wash one step above
/// [`block_fill`], lifting while hovered or open. Washes read through frost
/// and stay tonal on solid surfaces, in both appearances.
pub(crate) fn select_fill(theme: &Theme, lifted: bool) -> gpui::Hsla {
    theme.wash(if lifted { 0.10 } else { 0.06 })
}

/// The trigger of every settings dropdown: a borderless filled pill.
///
/// The caller adds the value ([`select_value`], after any leading glyph), then
/// [`select_chevron`], plus its own listeners. Its focus ring is paint only,
/// so focusing never shifts the label.
pub fn select_trigger(
    theme: &Theme,
    id: impl Into<SharedString>,
    open: bool,
) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let hover_key: SharedString = format!("{id}-hover").into();
    let lifted = select_fill(theme, true);
    div()
        .id(id)
        .relative()
        .flex_none()
        .max_w_full()
        .h(px(SELECT_HEIGHT))
        .pl(px(10.0))
        .pr(px(8.0))
        .rounded(px(8.0))
        .bg(if open {
            lifted
        } else {
            crate::motion::hover_blend(&hover_key, select_fill(theme, false), lifted)
        })
        .hover_fade(hover_key)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(12.5))
        .text_color(theme.text)
        .tab_index(0)
        .role(gpui::Role::Button)
        .aria_expanded(open)
        .map(|el| crate::surface_chrome::focus_ring(el, theme))
}

/// The trigger's current value, truncating before the chevron.
pub fn select_value(label: impl Into<SharedString>) -> gpui::Div {
    div().flex_1().min_w_0().truncate().child(label.into())
}

/// The trigger's trailing chevron, brighter while the menu is `open`.
pub fn select_chevron(theme: &Theme, open: bool) -> gpui::Svg {
    crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
        .size(px(14.0))
        .flex_none()
        .text_color(if open { theme.text } else { theme.text_muted })
}

/// The open menu: the shared popover card, which [`dropdown`] mounts inside
/// the frosted menu layer (an opaque card where frost is off).
pub fn select_menu(theme: &Theme) -> gpui::Div {
    popover::popover_card(theme).flex().flex_col()
}

/// One option row. `highlighted` is the keyboard cursor; the caller adds the
/// label, then [`select_check`].
pub fn select_row(
    theme: &Theme,
    selected: bool,
    highlighted: bool,
    fade_key: impl Into<SharedString>,
) -> gpui::Div {
    popover::menu_row_nav(theme, selected, highlighted, fade_key)
}

/// The trailing check slot, kept on every row so labels align.
pub fn select_check(theme: &Theme, selected: bool) -> gpui::Div {
    div().w(px(18.0)).flex_none().flex().justify_end().when(selected, |slot| {
        slot.child(crate::icons::icon(crate::icons::CHECK).size(px(14.0)).text_color(theme.accent))
    })
}

/// Open state for one [`select`], owned by its page: the popup with its exit
/// phase, and the keyboard highlight.
#[derive(Debug, Default)]
pub struct SelectState {
    menu: popover::Popup<()>,
    highlighted: usize,
}

impl SelectState {
    /// Whether the menu is open (a closing menu already reads as closed).
    pub const fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Open with the keyboard highlight on option `highlighted`.
    pub fn open(&mut self, highlighted: usize) {
        self.highlighted = highlighted;
        self.menu.open(());
    }
}

/// Close a [`select`] from outside it — another menu on the page opening.
/// `reach` as in [`select`].
pub fn close_select<V: 'static>(
    view: &mut V,
    reach: impl Fn(&mut V) -> &mut SelectState + 'static,
    cx: &mut Context<V>,
) {
    let reach: SelectReach<V> = Rc::new(reach);
    close_select_rc(view, &reach, cx);
}

/// One option in a [`select`] menu.
pub struct SelectOption {
    label: SharedString,
    leading: Option<Rc<dyn Fn() -> AnyElement>>,
    detail: Option<SharedString>,
}

// By hand: the leading glyph is a closure.
impl std::fmt::Debug for SelectOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectOption")
            .field("label", &self.label)
            .field("leading", &self.leading.is_some())
            .field("detail", &self.detail)
            .finish()
    }
}

impl SelectOption {
    /// An option reading `label`, with no glyph and no note.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), leading: None, detail: None }
    }

    /// A glyph or swatch before the label, in the row and (when selected)
    /// the trigger.
    #[must_use]
    pub fn leading(mut self, leading: impl Fn() -> AnyElement + 'static) -> Self {
        self.leading = Some(Rc::new(leading));
        self
    }

    /// A quiet note after the label, in the row only.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

type SelectReach<V> = Rc<dyn Fn(&mut V) -> &mut SelectState>;
type SelectHandler<V> = Rc<dyn Fn(&mut V, usize, &mut Window, &mut Context<V>)>;

/// The settings dropdown: a [`select_trigger`] opening a [`select_menu`] of
/// options with a check on the current one.
///
/// Pointer: the trigger toggles and an outside press dismisses. Keyboard: ↑/↓ open and move the
/// highlight, Home/End jump, Enter/Space open or commit, Escape closes. `reach`
/// re-borrows the page's [`SelectState`] inside listeners; `label` names the
/// control for assistive tech, which hears "label: value".
pub fn select<V: 'static>(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    theme: &Theme,
    reach: impl Fn(&mut V) -> &mut SelectState + 'static,
) -> Select<V> {
    Select {
        id: id.into(),
        label: label.into(),
        theme: theme.clone(),
        options: Vec::new(),
        selected: 0,
        width: None,
        menu_width: None,
        font: None,
        heading: None,
        align: popover::MenuAlign::End,
        reach: Rc::new(reach),
        handler: None,
    }
}

/// A [`select`] being built: its options, sizes and handler, until [`Self::render`].
pub struct Select<V: 'static> {
    id: SharedString,
    label: SharedString,
    theme: Theme,
    options: Vec<SelectOption>,
    selected: usize,
    width: Option<f32>,
    menu_width: Option<f32>,
    font: Option<SharedString>,
    heading: Option<&'static str>,
    align: popover::MenuAlign,
    reach: SelectReach<V>,
    handler: Option<SelectHandler<V>>,
}

// By hand: `reach` and the handler are closures.
impl<V: 'static> std::fmt::Debug for Select<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Select")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("options", &self.options)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

fn close_select_rc<V: 'static>(view: &mut V, reach: &SelectReach<V>, cx: &mut Context<V>) {
    if reach(view).menu.begin_close(crate::clock::now(cx)) {
        let reach = Rc::clone(reach);
        popover::reap_popup(cx, move |view: &mut V| &mut reach(view).menu);
    }
    cx.notify();
}

fn commit_select<V: 'static>(
    view: &mut V,
    ix: usize,
    reach: &SelectReach<V>,
    handler: Option<&SelectHandler<V>>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    close_select_rc(view, reach, cx);
    if let Some(handler) = handler {
        handler(view, ix, window, cx);
    }
}

impl<V: 'static> Select<V> {
    /// The menu's `options`, the `selected` one checked and shown on the trigger.
    #[must_use]
    pub fn options(
        mut self,
        options: impl IntoIterator<Item = SelectOption>,
        selected: usize,
    ) -> Self {
        self.options = options.into_iter().collect();
        self.selected = selected;
        self
    }

    /// What committing an option does, given its index; the menu has closed by then.
    #[must_use]
    pub fn on_select(
        mut self,
        on_select: impl Fn(&mut V, usize, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        self.handler = Some(Rc::new(on_select));
        self
    }

    /// Fixed trigger width; the menu matches it unless [`Self::menu_width`]
    /// says otherwise. Without one the trigger fits its value.
    #[must_use]
    pub const fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// The menu's width when it differs from the trigger's (never under 160).
    #[must_use]
    pub const fn menu_width(mut self, width: f32) -> Self {
        self.menu_width = Some(width);
        self
    }

    /// Font for the value and the rows (key names read in mono).
    #[must_use]
    pub fn font_family(mut self, font: SharedString) -> Self {
        self.font = Some(font);
        self
    }

    /// A small heading over the rows.
    #[must_use]
    pub const fn heading(mut self, heading: &'static str) -> Self {
        self.heading = Some(heading);
        self
    }

    /// Open the menu rightward from the trigger's left edge, for triggers
    /// that lead a row rather than end it.
    #[must_use]
    pub const fn open_rightward(mut self) -> Self {
        self.align = popover::MenuAlign::Start;
        self
    }

    /// The trigger, and the menu under it while `state` holds it open.
    pub fn render(self, state: &SelectState, cx: &Context<V>) -> gpui::Stateful<gpui::Div> {
        let Self {
            id,
            label,
            theme,
            options,
            selected,
            width,
            menu_width,
            font,
            heading,
            align,
            reach,
            handler,
        } = self;
        let count = options.len();
        let open = state.menu.is_open();
        let current = options.get(selected);
        let value = current.map(|option| option.label.clone()).unwrap_or_default();
        let trigger_leading = current.and_then(|option| option.leading.clone());

        let mut trigger = select_trigger(&theme, id.clone(), open)
            .debug_selector({
                let id = id.clone();
                move || id.to_string()
            })
            .aria_label(format!("{label}: {value}"))
            .when_some(width, |el, width| el.w(px(width)))
            .when_some(font.clone(), Styled::font_family)
            .on_mouse_down(gpui::MouseButton::Left, {
                let reach = Rc::clone(&reach);
                cx.listener(move |view, _, _, _| reach(view).menu.note_trigger_press())
            })
            .on_click({
                let reach = Rc::clone(&reach);
                let handler = handler.clone();
                cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                    let state = reach(view);
                    if event.is_keyboard() && state.menu.is_open() {
                        let ix = state.highlighted;
                        commit_select(view, ix, &reach, handler.as_ref(), window, cx);
                    } else if state.menu.take_press_was_open() || state.menu.is_open() {
                        close_select_rc(view, &reach, cx);
                    } else {
                        state.open(selected);
                        cx.notify();
                    }
                })
            })
            // Enter/Space arrive as keyboard clicks above; this handles
            // movement and dismissal.
            .on_key_down({
                let reach = Rc::clone(&reach);
                cx.listener(move |view, event: &gpui::KeyDownEvent, _, cx| {
                    let key = event.keystroke.key.as_str();
                    let modifiers = event.keystroke.modifiers;
                    let menu_key =
                        popover::classify_key(key, modifiers.platform, modifiers.control);
                    let state = reach(view);
                    if !state.menu.is_open() {
                        if matches!(menu_key, popover::MenuKey::Up | popover::MenuKey::Down) {
                            state.open(selected);
                            cx.stop_propagation();
                            cx.notify();
                        }
                        return;
                    }
                    let current = Some(state.highlighted.min(count.saturating_sub(1)));
                    state.highlighted = match (key, menu_key) {
                        (_, popover::MenuKey::Up) => {
                            popover::menu_step(current, count, -1).unwrap_or_default()
                        }
                        (_, popover::MenuKey::Down) => {
                            popover::menu_step(current, count, 1).unwrap_or_default()
                        }
                        ("home", _) => 0,
                        ("end", _) => count.saturating_sub(1),
                        (_, popover::MenuKey::Escape) => {
                            cx.stop_propagation();
                            close_select_rc(view, &reach, cx);
                            return;
                        }
                        _ => return,
                    };
                    cx.stop_propagation();
                    cx.notify();
                })
            })
            .children(trigger_leading.map(|leading| leading()))
            .child(select_value(value))
            .child(select_chevron(&theme, open));

        if state.menu.get().is_some() {
            let highlighted = state.highlighted;
            let rows = options.into_iter().enumerate().map(|(ix, option)| {
                let row_id: SharedString = format!("{id}-option-{ix}").into();
                let reach = Rc::clone(&reach);
                let handler = handler.clone();
                select_row(&theme, ix == selected, ix == highlighted, row_id.clone())
                    .id(row_id.clone())
                    .debug_selector(move || row_id.to_string())
                    .role(gpui::Role::MenuItemRadio)
                    .aria_label(option.label.clone())
                    .aria_toggled(if ix == selected {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        cx.stop_propagation();
                        commit_select(view, ix, &reach, handler.as_ref(), window, cx);
                    }))
                    .children(option.leading.map(|leading| leading()))
                    .child(div().flex_1().min_w_0().truncate().child(option.label))
                    .when_some(option.detail, |row, detail| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(10.5))
                                .text_color(theme.text_muted)
                                .child(detail),
                        )
                    })
                    .child(select_check(&theme, ix == selected))
                    .into_any_element()
            });
            let menu = select_menu(&theme)
                .w(px(menu_width.or(width).unwrap_or(0.0).max(SELECT_MENU_MIN_WIDTH)))
                .when_some(font, Styled::font_family)
                .on_mouse_down_out({
                    let reach = Rc::clone(&reach);
                    cx.listener(move |view, _, _, cx| close_select_rc(view, &reach, cx))
                })
                .when_some(heading, |menu, heading| {
                    menu.child(popover::menu_heading(&theme, heading))
                })
                .child(dropdown_rows(
                    format!("{id}-list"),
                    rows,
                    SELECT_HEIGHT,
                    if heading.is_some() { 32.0 } else { 8.0 },
                ));
            trigger = trigger.child(SettingsDropdown {
                id: format!("{id}-menu").into(),
                content: menu,
                closing: state.menu.closing_at(crate::clock::now(cx)),
                trigger_height: SELECT_HEIGHT,
                align,
            });
        }
        trigger
    }
}

/// Settings actions share one size, corner radius, and hover language.
///
/// Use outlined for selectors, quiet for secondary actions, filled for row
/// actions on a block (the dropdown trigger's glass wash), solid for
/// commits, and danger for destructive actions behind a confirmation.
#[derive(Debug, Clone, Copy)]
pub enum ActionTone {
    /// A secondary action: muted words that lift on hover.
    Quiet,
    /// A page's destructive action, its words in the danger tone, no fill.
    QuietDanger,
    /// A selector: an outlined input-glass button.
    Outlined,
    /// A row action on a block, in the dropdown trigger's wash.
    Filled,
    /// A commit, in the theme's solid fill.
    Solid,
    /// A destructive action behind a confirmation, in the danger fill.
    Danger,
}

/// An enabled settings action in `tone`; the caller adds its words and click.
pub fn action_button(theme: &Theme, tone: ActionTone) -> gpui::Div {
    action_button_when(theme, tone, true)
}

/// [`action_button`] that may be disabled.
///
/// Disabled, it dims to [`crate::theme::DISABLED_OPACITY`], with the default cursor and no hover
/// lift, since a second `.hover` would replace the tone's rather than remove it. The caller says
/// so to assistive tech (`aria_disabled`) and skips the click.
pub fn action_button_when(theme: &Theme, tone: ActionTone, enabled: bool) -> gpui::Div {
    fn lift(
        button: gpui::Div,
        enabled: bool,
        hover: impl FnOnce(gpui::StyleRefinement) -> gpui::StyleRefinement,
    ) -> gpui::Div {
        if enabled { button.hover(hover) } else { button }
    }
    let button = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded(px(8.0))
        .min_h(px(32.0))
        .px(px(10.0))
        .py(px(5.0))
        .text_size(crate::typography::ui_rems(12.5))
        .map(|button| {
            if enabled {
                button.cursor_pointer()
            } else {
                button.opacity(crate::theme::DISABLED_OPACITY).cursor_default()
            }
        });
    match tone {
        ActionTone::Quiet => {
            let (wash, text) = (theme.glass_hover(), theme.text);
            lift(button.text_color(theme.text_muted), enabled, move |s| s.bg(wash).text_color(text))
        }
        ActionTone::QuietDanger => {
            let wash = theme.glass_hover();
            lift(button.text_color(theme.danger_muted), enabled, move |s| s.bg(wash))
        }
        ActionTone::Outlined => {
            let (wash, edge) = (theme.glass_hover(), theme.border_strong);
            lift(
                button
                    .bg(theme.input_glass_bg())
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.text),
                enabled,
                move |s| s.bg(wash).border_color(edge),
            )
        }
        ActionTone::Filled => {
            let lifted = select_fill(theme, true);
            lift(button.bg(select_fill(theme, false)).text_color(theme.text), enabled, move |s| {
                s.bg(lifted)
            })
        }
        ActionTone::Solid => lift(
            button.bg(theme.solid).font_weight(gpui::FontWeight::MEDIUM).text_color(theme.on_solid),
            enabled,
            |s| s.opacity(0.9),
        ),
        // The dialogs' destructive fill (`popover::btn_danger`).
        ActionTone::Danger => {
            lift(button.bg(theme.danger_strong).text_color(gpui::white()), enabled, |s| {
                s.opacity(0.9)
            })
        }
    }
}

/// An [`action_button`] in `tone` reading `label`.
pub fn text_action(theme: &Theme, tone: ActionTone, label: impl Into<SharedString>) -> gpui::Div {
    action_button(theme, tone).child(label.into())
}

/// A quiet [`action_button`]; the caller adds its glyph and words.
pub fn ghost_action(theme: &Theme) -> gpui::Div {
    action_button(theme, ActionTone::Quiet)
}

/// The dismissible red error strip (`flex items-start gap-2 rounded-xl border
/// border-red-400/20 bg-red-400/[0.06] text-red-300/90` with a leading
/// `DangerTriangle mt-0.5 size-4`).
pub fn error_strip(theme: &Theme, message: impl Into<SharedString>) -> gpui::Div {
    // One anatomy with [`error_strip_with`].
    error_strip_with(theme, message, Vec::new())
}

// The same strip holding more than its message: a failure's hint and the output lines
// that explain it, under the title.
/// [`error_strip`] with `details` under its message.
pub fn error_strip_with(
    theme: &Theme,
    message: impl Into<SharedString>,
    details: Vec<AnyElement>,
) -> gpui::Div {
    let red = theme.danger; // red-400
    let red_text = theme.danger_muted; // red-300
    div()
        .mt(px(16.0))
        .px(px(16.0))
        .py(px(12.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(red.opacity(0.2))
        .bg(red.opacity(0.06))
        .text_size(crate::typography::ui_rems(12.5))
        .text_color(red_text.opacity(0.9))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.0))
        .child(
            div().flex_none().mt(px(2.0)).child(
                crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                    .size(px(16.0))
                    .text_color(red_text.opacity(0.9)),
            ),
        )
        .child(
            div().min_w_0().flex().flex_col().gap(px(4.0)).child(message.into()).children(details),
        )
}

/// The amber warning strip (`flex items-start gap-2 border-amber-400/20
/// bg-amber-400/[0.06] text-amber-200/90` with a leading `DangerTriangle
/// mt-0.5 size-3.5`).
pub fn warning_strip(theme: &Theme, message: impl Into<SharedString>) -> gpui::Div {
    let amber = theme.warning; // amber-400
    let amber_text = theme.warning_muted; // amber-200
    div()
        .mt(px(8.0))
        .px(px(16.0))
        .py(px(10.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(amber.opacity(0.2))
        .bg(amber.opacity(0.06))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(amber_text.opacity(0.9))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.0))
        .child(
            div().flex_none().mt(px(2.0)).child(
                crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                    .size(px(14.0))
                    .text_color(amber_text.opacity(0.9)),
            ),
        )
        .child(div().min_w_0().child(message.into()))
}

/// The sidebar's paint-time overflow fade, with a persistent scroll handle per
/// settings surface. No fade is painted when the content fits or at a reached edge.
pub fn scroll_faded(
    key: impl Into<SharedString>,
    area: gpui::Stateful<gpui::Div>,
) -> impl IntoElement {
    SettingsScroll { key: key.into(), area }
}

#[derive(IntoElement)]
struct SettingsScroll {
    key: SharedString,
    area: gpui::Stateful<gpui::Div>,
}

impl RenderOnce for SettingsScroll {
    fn render(self, window: &mut Window, _: &mut gpui::App) -> impl IntoElement {
        let scroll = window.with_global_id(self.key.into(), |id, window| {
            window.with_element_state(id, |previous: Option<ScrollHandle>, _| {
                let scroll = previous.unwrap_or_default();
                (scroll.clone(), scroll)
            })
        });
        crate::edge_fade::edge_faded(16.0, true, true, self.area.track_scroll(&scroll))
            .fade_overflow_y(&scroll)
    }
}

/// A one-line hover note for settings controls (reset times, icon-only
/// actions), in the same frosted chip as the rest of the app's tooltips.
#[derive(Debug)]
pub struct TextTooltip {
    text: SharedString,
    // The action's key, resolved when the tooltip opened, and a second line.
    keys: Option<SharedString>,
    detail: Option<SharedString>,
}

// The window anchors every tooltip under the control it names (`lib.rs`,
// `set_tooltip_placement`), so the chip is drawn as it is; a control with content below it
// sets `tooltip_placement(Above)` itself.

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The card is `live::chrome::tooltip_card`, which Slopty's own views draw too.
        let theme = Theme::of(cx);
        match self.detail.clone() {
            Some(detail) => {
                tooltip_card_detail(self.text.clone(), detail, self.keys.clone(), theme)
            }
            None => {
                crate::live::chrome::tooltip_card_keyed(self.text.clone(), self.keys.clone(), theme)
            }
        }
    }
}

/// `.tooltip(...)` builder for a [`TextTooltip`].
pub fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| TextTooltip { text: text.clone(), keys: None, detail: None }).into()
}

// Tooltips name the action and its key (`docs/decisions/ui.md`, "Tooltips name the
// action and its key"). The key is looked up each time the tooltip opens, so a rebind shows at
// once and an unbound action shows its words alone.

/// How long the pointer rests on a control before its tooltip shows: one value for every hint
/// the shell draws (Claude desktop 300 ms, macOS about 400).
pub const TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(350);

/// Where a tooltip's key comes from.
#[derive(Debug)]
pub enum Keys {
    /// Whatever `action` is bound to when the tooltip opens; nothing when unbound.
    Action(Box<dyn gpui::Action>),
    /// The same, resolved in `focus`'s key context (the composer's Submit, the browser's Reload).
    ActionIn(Box<dyn gpui::Action>, gpui::FocusHandle),
    /// A key a raw handler takes ("shift-tab", "escape"): only where that handler exists.
    Fixed(&'static str),
}

impl Clone for Keys {
    fn clone(&self) -> Self {
        match self {
            Self::Action(action) => Self::Action(action.boxed_clone()),
            Self::ActionIn(action, focus) => Self::ActionIn(action.boxed_clone(), focus.clone()),
            Self::Fixed(combo) => Self::Fixed(combo),
        }
    }
}

impl Keys {
    /// The key as the tooltip shows it now, or `None` when nothing binds the action.
    pub fn text(&self, window: &Window, cx: &gpui::App) -> Option<SharedString> {
        match self {
            Self::Action(action) => crate::keys::for_action(action.as_ref(), None, window, cx),
            Self::ActionIn(action, focus) => {
                crate::keys::for_action(action.as_ref(), Some(focus), window, cx)
            }
            Self::Fixed(combo) => Some(crate::keys::combo_text(combo)),
        }
    }

    /// The same key in `aria-keyshortcuts` spelling, from the keymap alone (no `Window`).
    pub fn aria(&self, cx: &gpui::App) -> Option<SharedString> {
        match self {
            Self::Action(action) | Self::ActionIn(action, _) => {
                crate::keys::aria_for_action(action.as_ref(), cx)
            }
            Self::Fixed(combo) => Some(crate::keys::aria_combo(combo)),
        }
    }
}

/// A tooltip naming an action and, when it has one, its key.
pub fn action_tooltip(
    label: impl Into<SharedString>,
    keys: Keys,
) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + 'static {
    keyed_tooltip(label.into(), None, keys)
}

/// A two-line tooltip: the title (and its key) over a quieter detail line.
pub fn detail_tooltip(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    keys: Option<Keys>,
) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let title: SharedString = title.into();
    let detail: SharedString = detail.into();
    move |window, cx| {
        let keys = keys.as_ref().and_then(|keys| keys.text(window, cx));
        let (title, detail) = (title.clone(), detail.clone());
        cx.new(|_| TextTooltip { text: title, keys, detail: Some(detail) }).into()
    }
}

fn keyed_tooltip(
    label: SharedString,
    detail: Option<SharedString>,
    keys: Keys,
) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView + 'static {
    move |window, cx| {
        let keys = keys.text(window, cx);
        let (text, detail) = (label.clone(), detail.clone());
        cx.new(|_| TextTooltip { text, keys, detail }).into()
    }
}

/// The two-line tooltip chip ([`detail_tooltip`]; a project's icon, a pull request): the title
/// row as [`crate::live::chrome::tooltip_card_keyed`] draws it, the detail under it a step
/// quieter.
pub fn tooltip_card_detail(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    keys: Option<SharedString>,
    theme: &Theme,
) -> crate::frost::Frosted {
    let title: SharedString = title.into();
    let caps = keys.as_deref().map(|keys| crate::live::chrome::tooltip_keys(theme, keys));
    crate::live::chrome::tooltip_frame(
        theme,
        (title.clone(), keys),
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(px(crate::live::chrome::TOOLTIP_KEY_GAP))
                    .child(div().min_w_0().child(title))
                    .children(caps),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0)) // the tooltip's detail at 11
                    .text_color(theme.text_faint)
                    .child(detail.into()),
            ),
    )
}

/// Names a control for the pointer and for assistive tech from one place: its tooltip (label
/// and key, after [`TOOLTIP_DELAY`]), its `aria-label` and its `aria-keyshortcuts`.
pub trait ActionHint: StatefulInteractiveElement + Sized {
    /// Names the control `label` with `keys`' key, in its tooltip, `aria-label` and
    /// `aria-keyshortcuts`.
    #[must_use]
    fn action_hint(self, label: impl Into<SharedString>, keys: Keys, cx: &gpui::App) -> Self {
        let label: SharedString = label.into();
        let aria = keys.aria(cx);
        let mut el = self
            .aria_label(label.clone())
            .tooltip(action_tooltip(label, keys))
            .tooltip_show_delay(TOOLTIP_DELAY);
        if let Some(aria) = aria {
            el = el.aria_keyshortcuts(aria);
        }
        el
    }

    /// [`Self::action_hint`] for a control no key reaches: its tooltip and `aria-label`.
    #[must_use]
    fn hint(self, label: impl Into<SharedString>) -> Self {
        let label: SharedString = label.into();
        self.aria_label(label.clone())
            .tooltip(text_tooltip(label))
            .tooltip_show_delay(TOOLTIP_DELAY)
    }
}

impl<E: StatefulInteractiveElement + Sized> ActionHint for E {}

// Every choice marks its default and the one Slopty recommends, in plain words after the
// option (`docs/decisions/ui.md`, "Every choice marks its default and the recommended one").

/// What an option is to the person: the default, the one Slopty recommends, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The value a fresh install starts with.
    Default,
    /// The value Slopty recommends.
    Recommended,
    /// Both the default and the recommended value.
    DefaultRecommended,
}

impl Standing {
    /// The standing of an option that is (or is not) the default and recommended; `None` for
    /// neither.
    pub const fn of(is_default: bool, is_recommended: bool) -> Option<Self> {
        match (is_default, is_recommended) {
            (true, true) => Some(Self::DefaultRecommended),
            (true, false) => Some(Self::Default),
            (false, true) => Some(Self::Recommended),
            (false, false) => None,
        }
    }

    /// The standing in the plain words its note shows.
    pub const fn words(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Recommended => "Recommended",
            Self::DefaultRecommended => "Default · Recommended",
        }
    }
}

/// The quiet note after an option's label, in the key hints' voice.
pub fn standing_note(theme: &Theme, standing: Standing) -> gpui::Div {
    div()
        .flex_none()
        .ml(px(6.0))
        .whitespace_nowrap()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_faint)
        .child(SharedString::from(standing.words()))
}

impl SelectOption {
    /// Marks the option as the default and/or recommended one (its row's note).
    #[must_use]
    pub fn standing(self, standing: Option<Standing>) -> Self {
        match standing {
            Some(standing) => self.detail(standing.words()),
            None => self,
        }
    }
}

/// "Reset" on a settings row whose value differs from its default.
///
/// Nothing shows at the default, so the absence of "Reset" is the mark (Zed's rule). The caller
/// adds the click. It lays out as tall as its words and reaches 4 px above and below them, so the
/// row keeps its height however compact it is, whether "Reset" shows or not, and the target stays
/// 24 px.
pub fn reset_action(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: &str,
    is_default: bool,
) -> Option<gpui::Stateful<gpui::Div>> {
    if is_default {
        return None;
    }
    let id: SharedString = id.into();
    let selector = id.clone();
    let hover = theme.text;
    Some(crate::surface_chrome::focus_ring(
        div()
            .id(gpui::ElementId::Name(id))
            .debug_selector(move || selector.to_string())
            .role(gpui::Role::Button)
            .aria_label(format!("Reset {label} to default"))
            .flex_none()
            .min_h(px(16.0))
            .py(px(4.0))
            .my(px(-4.0))
            .px(px(4.0))
            .rounded(px(4.0))
            .flex()
            .items_center()
            .tab_index(0)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .cursor_pointer()
            .hover(move |s| s.text_color(hover))
            .child(SharedString::from("Reset")),
        theme,
    ))
}

/// A page's "Restore defaults", dimmed and inert while nothing differs from its default. The
/// caller adds the click when `any_changed`.
pub fn restore_defaults(theme: &Theme, id: &str, any_changed: bool) -> gpui::Stateful<gpui::Div> {
    let selector: SharedString = id.to_owned().into();
    ghost_action(theme)
        .id(gpui::ElementId::Name(selector.clone()))
        .debug_selector(move || selector.to_string())
        .role(gpui::Role::Button)
        .aria_label("Restore defaults")
        .flex_none()
        .when(any_changed, |el| el.tab_index(0))
        .when(!any_changed, |el| el.opacity(crate::theme::DISABLED_OPACITY).cursor_default())
        .child(
            crate::icons::icon(crate::icons::RESTART).size(px(14.0)).text_color(theme.text_muted),
        )
        .child(SharedString::from("Restore defaults"))
}

// Truncated text always has a way out: the whole of it in a tooltip, only when cut.

/// A one-line label that fades its tail when the room runs out (the sidebar's fade) and, only
/// then, shows the whole text in a tooltip.
pub fn truncate_with_tip(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> impl IntoElement {
    let id: SharedString = id.into();
    let text: SharedString = text.into();
    let overflow = ScrollHandle::new();
    let probe = overflow.clone();
    let tip = text.clone();
    let selector = id.clone();
    crate::edge_fade::edge_faded(
        20.0,
        false,
        false,
        div()
            .id(gpui::ElementId::Name(id))
            .debug_selector(move || selector.to_string())
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .track_scroll(&overflow)
            .flex()
            .tooltip(move |window, cx| overflow_tooltip(&probe, tip.clone(), window, cx))
            .tooltip_show_delay(TOOLTIP_DELAY)
            .child(div().flex_none().whitespace_nowrap().child(text)),
    )
    .fade_right(true)
    .fade_label_overflow(&overflow)
}

/// The whole text of a cut label, or an empty view when it fits (gpui asks for a view either way).
fn overflow_tooltip(
    overflow: &ScrollHandle,
    text: SharedString,
    _window: &mut Window,
    cx: &mut gpui::App,
) -> gpui::AnyView {
    let cut = overflow.max_offset().x > px(0.5);
    cx.new(|_| OverflowTooltip { text: cut.then_some(text) }).into()
}

struct OverflowTooltip {
    text: Option<SharedString>,
}

impl Render for OverflowTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(text) = self.text.clone() else {
            return div().into_any_element();
        };
        crate::live::chrome::tooltip_card(text, Theme::of(cx)).into_any_element()
    }
}

/// Where a middle-cut value splits: everything before its last `tail` characters, and those
/// (t3code's `MiddleTruncate`). A value no longer than `tail` is all tail.
pub fn split_tail(value: &str, tail: usize) -> (&str, &str) {
    let Some(head) = value.chars().count().checked_sub(tail).filter(|&head| head > 0) else {
        return ("", value);
    };
    let at = value.char_indices().nth(head).map_or(value.len(), |(ix, _)| ix);
    value.split_at(at)
}

/// A path or branch cut in the middle: its head ends in "…" when the room runs out, its last
/// `tail` characters always show, and the whole value is in the tooltip when cut.
pub fn middle_truncate(
    id: impl Into<SharedString>,
    value: impl Into<SharedString>,
    tail: usize,
) -> impl IntoElement {
    let id: SharedString = id.into();
    let value: SharedString = value.into();
    let (head, end) = split_tail(&value, tail);
    let (head, end) = (SharedString::from(head.to_owned()), SharedString::from(end.to_owned()));
    let overflow = ScrollHandle::new();
    let probe = overflow.clone();
    let tip = value.clone();
    let selector = id.clone();
    div()
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector.to_string())
        .aria_label(value.clone())
        .min_w_0()
        .flex()
        .flex_row()
        .whitespace_nowrap()
        .tooltip(move |window, cx| overflow_tooltip(&probe, tip.clone(), window, cx))
        .tooltip_show_delay(TOOLTIP_DELAY)
        .child(
            // The head ends in "…" when cut; an unseen copy at its full width is what tells the
            // tooltip it was.
            div()
                .id("head")
                .relative()
                .min_w_0()
                .flex_shrink(1.0)
                .overflow_hidden()
                .track_scroll(&overflow)
                .child(div().truncate().child(head.clone()))
                .child(div().absolute().top_0().left_0().invisible().flex_none().child(head)),
        )
        .child(div().flex_none().child(end))
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn standing_reads_in_plain_words() {
        assert_eq!(Standing::of(false, false), None);
        assert_eq!(Standing::of(true, false).map(Standing::words), Some("Default"));
        assert_eq!(Standing::of(false, true).map(Standing::words), Some("Recommended"));
        assert_eq!(Standing::of(true, true).map(Standing::words), Some("Default · Recommended"));
    }

    #[test]
    fn a_middle_cut_keeps_the_tail_whole() {
        assert_eq!(split_tail("~/code/slopty/crates", 6), ("~/code/slopty/", "crates"));
        assert_eq!(split_tail("main", 6), ("", "main"));
        assert_eq!(split_tail("añb→ç", 2), ("añb", "→ç"));
    }

    #[test]
    fn a_row_at_its_default_offers_no_reset() {
        let theme = Theme::default();
        assert!(reset_action(&theme, "width-reset", "Conversation width", true).is_none());
        assert!(reset_action(&theme, "width-reset", "Conversation width", false).is_some());
    }

    mod actions {
        #![expect(
            clippy::derive_partial_eq_without_eq,
            reason = "gpui::actions! derives PartialEq only"
        )]
        gpui::actions!(hint_test, [Probe]);
    }
    use actions::Probe;

    struct Hinted;

    impl Render for Hinted {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size(px(200.0)).child(
                div()
                    .id("hinted")
                    .size(px(24.0))
                    .debug_selector(|| "hinted".into())
                    .role(gpui::Role::Button)
                    .action_hint("Show files panel", Keys::Action(Box::new(Probe)), cx),
            )
        }
    }

    #[gpui::test]
    fn the_hint_names_its_key_and_follows_a_rebind(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.bind_keys([gpui::KeyBinding::new("cmd-e", Probe, None)]);
        });
        let (_view, cx) = cx.add_window_view(|_, _| Hinted);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let keys = Keys::Action(Box::new(Probe));
            assert_eq!(keys.text(window, cx), Some(crate::keys::combo_text("cmd-e")));
            assert_eq!(keys.aria(cx).as_deref(), Some("Meta+E"));
            cx.clear_key_bindings();
            cx.bind_keys([gpui::KeyBinding::new("cmd-shift-e", Probe, None)]);
            assert_eq!(keys.text(window, cx), Some(crate::keys::combo_text("cmd-shift-e")));
            cx.clear_key_bindings();
            assert_eq!(keys.text(window, cx), None);
            assert_eq!(
                Keys::Fixed("shift-tab").text(window, cx),
                Some(crate::keys::combo_text("shift-tab"))
            );
        });
    }
}

#[cfg(test)]
mod page_head_tests {
    use super::*;

    struct Page;

    impl Render for Page {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx);
            div().w(px(600.0)).child(page_head(
                theme,
                "Notifications",
                None,
                "What Slopty tells you, and how.",
                Some(text_action(theme, ActionTone::Quiet, "Restore defaults").into_any_element()),
            ))
        }
    }

    // Every page opens one way: its action centred on the title's 32 row at its right,
    // the line saying what the page is for under it.
    #[gpui::test]
    fn a_page_head_centres_its_action_on_the_title_row(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (_page, cx) = cx.add_window_view(|_, _| Page);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let row = cx.debug_bounds("page-head-row").expect("the title row");
        let action = cx.debug_bounds("page-head-action").expect("the action");
        let subtitle = cx.debug_bounds("page-head-subtitle").expect("the subtitle");
        assert!(f32::from(row.size.height) >= PAGE_HEAD_ROW - 0.5, "{row:?}");
        assert!(
            (f32::from(row.center().y - action.center().y)).abs() < 0.5,
            "centred: {row:?} {action:?}"
        );
        assert!(
            (f32::from(row.right() - action.right())).abs() < 0.5,
            "at the right edge: {row:?} {action:?}"
        );
        assert!(subtitle.top() >= row.bottom(), "the line under it: {row:?} {subtitle:?}");
    }

    struct Row;

    impl Render for Row {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx);
            div()
                .w(px(600.0))
                .flex()
                .flex_col()
                .child(row_title(theme, "Theme").debug_selector(|| "row-title".to_owned()))
                .child(
                    meta_line(theme, vec!["Follows the system".into_any_element()])
                        .debug_selector(|| "row-description".to_owned()),
                )
        }
    }

    // A row's description stands 4 under its title on every page (opencode's settings
    // leading), not pressed against it.
    #[gpui::test]
    fn a_rows_description_stands_clear_of_its_title(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (_row, cx) = cx.add_window_view(|_, _| Row);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let title = cx.debug_bounds("row-title").expect("the title");
        let description = cx.debug_bounds("row-description").expect("the description");
        let gap = f32::from(description.top() - title.bottom());
        assert!((gap - ROW_DESCRIPTION_GAP).abs() < 0.5, "{title:?} {description:?}");
    }
}
