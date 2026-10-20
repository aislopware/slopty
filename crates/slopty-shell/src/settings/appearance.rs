// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Settings → Appearance: system behavior, independent light/dark variants,
//! and the optional interactive accent overlay.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    KeyDownEvent, ObjectFit, Pixels, Point, Render, ScrollDelta, SharedString, StyledImage as _,
    Subscription, Window, div, img, point, prelude::*, px,
};
use slopty_shell_theme::{
    AccentPreset, AccentSelection, SurfacePreference, SurfaceTreatment, ThemeRegistry, ThemeSelection,
};

use crate::appearance::{self, AppearanceMode};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons;
use crate::popover::{self, Popup};
use crate::settings::widgets::{self, ActionHint as _};
use crate::theme::{Appearance, Theme};
use crate::typography::{self, FontAvailability, UiFontFamily, UiFontSize};

struct BackgroundAdjustmentDialog {
    focus: FocusHandle,
    zoom_focus: FocusHandle,
    end_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    path: String,
    source: Option<Arc<gpui::RenderImage>>,
    draft: crate::settings::NewThreadBackgroundAdjustment,
    preview_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    zoom_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    drag: Option<(
        Point<Pixels>,
        crate::settings::NewThreadBackgroundAdjustment,
    )>,
    pinch: Option<(f32, f32)>,
}

#[derive(Clone)]
struct BackgroundAdjustmentDrag;

impl Render for BackgroundAdjustmentDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

#[derive(Clone)]
struct BackgroundAdjustmentZoomDrag;

impl Render for BackgroundAdjustmentZoomDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

const BACKGROUND_ZOOM_STEP: f32 = 0.1;

fn background_pinch_factor(factor: f32, delta: f32, relative_delta: bool) -> f32 {
    if !delta.is_finite() {
        return factor;
    }
    // Direct Manipulation reports a ratio to the last scale; the other
    // native backends report an additive change to the gesture's scale.
    let next = if relative_delta {
        factor * (1.0 + delta)
    } else {
        factor + delta
    };
    next.max(0.001)
}

fn background_zoom_fraction(zoom: f32) -> f32 {
    let minimum = crate::settings::NewThreadBackgroundAdjustment::MIN_ZOOM;
    let maximum = crate::settings::NewThreadBackgroundAdjustment::MAX_ZOOM;
    ((zoom - minimum) / (maximum - minimum)).clamp(0.0, 1.0)
}

fn background_zoom_at_x(bounds: Bounds<Pixels>, x: Pixels) -> f32 {
    let minimum = crate::settings::NewThreadBackgroundAdjustment::MIN_ZOOM;
    let maximum = crate::settings::NewThreadBackgroundAdjustment::MAX_ZOOM;
    let fraction = ((f32::from(x - bounds.left()) - 7.0)
        / (f32::from(bounds.size.width) - 14.0).max(1.0))
    .clamp(0.0, 1.0);
    minimum + fraction * (maximum - minimum)
}

fn background_preview_size(
    viewport: gpui::Size<Pixels>,
    sidebar_width: f32,
    sidebar_collapsed: bool,
) -> gpui::Size<Pixels> {
    let hero_width = (f32::from(viewport.width)
        - if sidebar_collapsed {
            0.0
        } else {
            sidebar_width
        })
    .max(1.0);
    let hero_height =
        crate::shell::new_thread_background_height(f32::from(viewport.height)).max(1.0);
    let max_width = (f32::from(viewport.width) - 84.0).clamp(1.0, 556.0);
    let scale = (max_width / hero_width).min(258.0 / hero_height);
    gpui::size(px(hero_width * scale), px(hero_height * scale))
}

/// The three independently configurable font slots. Interface and code/diff
/// draw from the whole catalog — proportional faces are legal there. The
/// terminal draws from the fixed-width subset only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontKind {
    Ui,
    Terminal,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppearanceSettingsEvent {
    CodeFontSizeChanged(f32),
}

impl FontKind {
    const ALL: [Self; 3] = [Self::Ui, Self::Terminal, Self::Code];

    fn slug(self) -> &'static str {
        match self {
            Self::Ui => "interface",
            Self::Terminal => "terminal",
            Self::Code => "code",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ui => "Interface font",
            Self::Terminal => "Terminal font",
            Self::Code => "Code & diff font",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Ui => "Menus and conversations",
            Self::Terminal => "Terminal output · monospace only",
            Self::Code => "Code, diffs, and files",
        }
    }

    /// The catalog this slot may pick from. Only the terminal narrows: its
    /// renderer positions cursor, selection, and hit-testing on an `m`-wide
    /// cell grid, which a proportional family silently breaks.
    fn choices_for(self, availability: &FontAvailability) -> &[UiFontFamily] {
        match self {
            Self::Terminal => availability.fixed_width_choices(),
            _ => availability.choices(),
        }
    }

    fn is_available_for(self, availability: &FontAvailability, family: &UiFontFamily) -> bool {
        match self {
            Self::Terminal => availability.is_fixed_width_available(family),
            _ => availability.is_available(family),
        }
    }

    fn requested(self, cx: &gpui::App) -> UiFontFamily {
        match self {
            Self::Ui => typography::requested(cx),
            Self::Terminal => typography::terminal_requested(cx),
            Self::Code => typography::code_requested(cx),
        }
    }

    fn effective(self, cx: &gpui::App) -> UiFontFamily {
        match self {
            Self::Ui => typography::effective(cx),
            Self::Terminal => typography::terminal_effective(cx),
            Self::Code => typography::code_effective(cx),
        }
    }

    fn apply_family(self, family: UiFontFamily, cx: &mut gpui::App) {
        match self {
            Self::Ui => typography::set_family(family, cx),
            Self::Terminal => typography::set_terminal_family(family, cx),
            Self::Code => typography::set_code_family(family, cx),
        };
    }

    // slopty: what each slot starts at, so its picker marks the default (M10).
    fn default_family(self) -> UiFontFamily {
        match self {
            Self::Ui => UiFontFamily::Geist,
            Self::Terminal | Self::Code => UiFontFamily::GeistMono,
        }
    }

    /// The default size's place on this kind's ladder.
    fn default_size_ix(self) -> usize {
        match self {
            Self::Ui => UiFontSize::ALL
                .iter()
                .position(|size| *size == UiFontSize::default())
                .unwrap_or_default(),
            Self::Terminal => nearest_mono_ix(typography::TERMINAL_FONT_SIZE_DEFAULT),
            Self::Code => nearest_mono_ix(typography::CODE_FONT_SIZE_DEFAULT),
        }
    }

    fn pixel_size(self, cx: &gpui::App) -> f32 {
        match self {
            Self::Ui => typography::font_size(cx).pixels(),
            Self::Terminal => typography::terminal_font_size(cx),
            Self::Code => typography::code_font_size(cx),
        }
    }

    /// Labels for this kind's size ladder, in ladder order. Both ladders read
    /// as plain pixel values, so all three dropdowns look the same.
    fn size_labels(self) -> Vec<SharedString> {
        match self {
            Self::Ui => UiFontSize::ALL.iter().map(|size| size.label()).collect(),
            _ => MONO_FONT_SIZES
                .iter()
                .map(|size| SharedString::from(format_px(*size)))
                .collect(),
        }
    }

    fn size_count(self) -> usize {
        match self {
            Self::Ui => UiFontSize::ALL.len(),
            _ => MONO_FONT_SIZES.len(),
        }
    }

    /// Ladder position of the committed size. Terminal and code sizes are
    /// stored as free pixels (older settings, hand-edited files), so they snap
    /// to the nearest rung rather than falling off the list.
    fn size_ix(self, cx: &gpui::App) -> usize {
        match self {
            Self::Ui => UiFontSize::ALL
                .iter()
                .position(|size| *size == typography::font_size(cx))
                .unwrap_or_default(),
            _ => nearest_mono_ix(self.pixel_size(cx)),
        }
    }
}

/// Pixel ladder behind the terminal and code size dropdowns. Both defaults
/// (terminal 13, code 12.5) are rungs, so today's rendering is reachable.
const MONO_FONT_SIZES: [f32; 10] = [10.0, 11.0, 12.0, 12.5, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0];

fn nearest_mono_ix(size: f32) -> usize {
    MONO_FONT_SIZES
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (**a - size).abs().total_cmp(&(**b - size).abs()))
        .map(|(ix, _)| ix)
        .unwrap_or_default()
}

pub struct AppearancePage {
    conversation_width_select: widgets::SelectState,
    scroll: crate::settings::widgets::PageScroll,
    selected_font: UiFontFamily,
    selected_terminal_font: UiFontFamily,
    selected_code_font: UiFontFamily,
    font_focus: FocusHandle,
    terminal_font_focus: FocusHandle,
    code_font_focus: FocusHandle,
    font_menu: Popup<()>,
    terminal_font_menu: Popup<()>,
    code_font_menu: Popup<()>,
    /// Floating rail for each family dropdown (the menu-scrollbar treatment,
    /// on the menu's own scroll host). One per kind: the menus scroll
    /// independently, so sharing a state would carry one menu's offset and
    /// rail timers into the next one opened.
    font_list: widgets::PageScroll,
    terminal_font_list: widgets::PageScroll,
    code_font_list: widgets::PageScroll,
    size_select: widgets::SelectState,
    terminal_size_select: widgets::SelectState,
    code_size_select: widgets::SelectState,
    font_menu_dismissed_at: Option<std::time::Instant>,
    terminal_font_menu_dismissed_at: Option<std::time::Instant>,
    code_font_menu_dismissed_at: Option<std::time::Instant>,
    /// Filter for whichever family menu is open — a device can carry hundreds
    /// of families, so the list narrows as you type instead of asking you to
    /// scroll. One input serves all three kinds: only one menu is ever open.
    font_search: Entity<ComposerInput>,
    _font_search_events: Subscription,
    light_theme_select: widgets::SelectState,
    dark_theme_select: widgets::SelectState,
    surface_select: widgets::SelectState,
    background_adjustment_dialog: Option<BackgroundAdjustmentDialog>,
    background_error: Option<SharedString>,
}

impl AppearancePage {
    // slopty: three presets in place of a pixel slider (Amp's; prune P9).
    fn render_conversation_width(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::settings::ConversationWidth;
        let current = crate::settings::conversation_width(cx);
        let control = widgets::select(
            "conversation-width",
            "Conversation width",
            theme,
            |page: &mut Self| &mut page.conversation_width_select,
        )
        .options(
            ConversationWidth::ALL.into_iter().map(|width| {
                widgets::SelectOption::new(width.label())
                    .standing(widgets::Standing::of(width == ConversationWidth::default(), false))
            }),
            ConversationWidth::ALL
                .into_iter()
                .position(|width| width == current)
                .unwrap_or_default(),
        )
        .width(128.0)
        .on_select(|_, ix, _, cx| {
            crate::settings::set_conversation_width(ConversationWidth::ALL[ix], cx);
            cx.notify();
        })
        .render(&self.conversation_width_select, cx);
        widgets::card_row(theme, false)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .child(widgets::row_title(theme, "Conversation width"))
                    .child(widgets::meta_line(
                        theme,
                        vec![div().child(current.description()).into_any_element()],
                    )),
            )
            .children(self.reset_row(
                "conversation-width-reset",
                "Conversation width",
                current == ConversationWidth::default(),
                theme,
                cx,
                |_, _, cx| crate::settings::set_conversation_width(ConversationWidth::default(), cx),
            ))
            .child(control)
            .into_any_element()
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        // `PaletteSearch` binds text-editing keys only — arrows/Enter/Escape
        // stay unbound and bubble from the input to the menu card's own key
        // handler. `Submitted` never fires here, so Enter has exactly one path.
        let font_search =
            cx.new(|cx| ComposerInput::with_context("Search fonts", "PaletteSearch", cx));
        let font_search_events = cx.subscribe(&font_search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.on_font_search_edited(cx);
            }
        });
        Self {
            conversation_width_select: widgets::SelectState::default(),
            scroll: crate::settings::widgets::PageScroll::default(),
            selected_font: typography::effective(cx),
            selected_terminal_font: typography::terminal_effective(cx),
            selected_code_font: typography::code_effective(cx),
            font_focus: cx.focus_handle(),
            terminal_font_focus: cx.focus_handle(),
            code_font_focus: cx.focus_handle(),
            font_menu: Popup::default(),
            terminal_font_menu: Popup::default(),
            code_font_menu: Popup::default(),
            font_list: widgets::PageScroll::default(),
            terminal_font_list: widgets::PageScroll::default(),
            code_font_list: widgets::PageScroll::default(),
            size_select: widgets::SelectState::default(),
            terminal_size_select: widgets::SelectState::default(),
            code_size_select: widgets::SelectState::default(),
            font_menu_dismissed_at: None,
            terminal_font_menu_dismissed_at: None,
            code_font_menu_dismissed_at: None,
            font_search,
            _font_search_events: font_search_events,
            light_theme_select: widgets::SelectState::default(),
            dark_theme_select: widgets::SelectState::default(),
            surface_select: widgets::SelectState::default(),
            background_adjustment_dialog: None,
            background_error: None,
        }
    }

    fn font_menu(&self, kind: FontKind) -> &Popup<()> {
        match kind {
            FontKind::Ui => &self.font_menu,
            FontKind::Terminal => &self.terminal_font_menu,
            FontKind::Code => &self.code_font_menu,
        }
    }

    fn font_menu_mut(&mut self, kind: FontKind) -> &mut Popup<()> {
        match kind {
            FontKind::Ui => &mut self.font_menu,
            FontKind::Terminal => &mut self.terminal_font_menu,
            FontKind::Code => &mut self.code_font_menu,
        }
    }

    fn font_list_mut(&mut self, kind: FontKind) -> &mut widgets::PageScroll {
        match kind {
            FontKind::Ui => &mut self.font_list,
            FontKind::Terminal => &mut self.terminal_font_list,
            FontKind::Code => &mut self.code_font_list,
        }
    }

    fn selected_font(&self, kind: FontKind) -> &UiFontFamily {
        match kind {
            FontKind::Ui => &self.selected_font,
            FontKind::Terminal => &self.selected_terminal_font,
            FontKind::Code => &self.selected_code_font,
        }
    }

    fn set_selected_font(&mut self, kind: FontKind, family: UiFontFamily) {
        match kind {
            FontKind::Ui => self.selected_font = family,
            FontKind::Terminal => self.selected_terminal_font = family,
            FontKind::Code => self.selected_code_font = family,
        }
    }

    fn font_focus(&self, kind: FontKind) -> &FocusHandle {
        match kind {
            FontKind::Ui => &self.font_focus,
            FontKind::Terminal => &self.terminal_font_focus,
            FontKind::Code => &self.code_font_focus,
        }
    }

    fn font_dismissed_at(&mut self, kind: FontKind) -> &mut Option<std::time::Instant> {
        match kind {
            FontKind::Ui => &mut self.font_menu_dismissed_at,
            FontKind::Terminal => &mut self.terminal_font_menu_dismissed_at,
            FontKind::Code => &mut self.code_font_menu_dismissed_at,
        }
    }

    fn commit_font(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        let family = self.selected_font(kind).clone();
        if kind.is_available_for(&typography::availability(cx), &family) {
            kind.apply_family(family, cx);
            let effective = kind.effective(cx);
            self.set_selected_font(kind, effective);
            self.close_font_menu(kind, cx);
            cx.notify();
        }
    }

    fn size_select(&self, kind: FontKind) -> &widgets::SelectState {
        match kind {
            FontKind::Ui => &self.size_select,
            FontKind::Terminal => &self.terminal_size_select,
            FontKind::Code => &self.code_size_select,
        }
    }

    fn size_select_mut(&mut self, kind: FontKind) -> &mut widgets::SelectState {
        match kind {
            FontKind::Ui => &mut self.size_select,
            FontKind::Terminal => &mut self.terminal_size_select,
            FontKind::Code => &mut self.code_size_select,
        }
    }

    /// Apply rung `ix` of this kind's size ladder.
    fn commit_size(
        &mut self,
        kind: FontKind,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = ix.min(kind.size_count() - 1);
        match kind {
            FontKind::Ui => {
                typography::set_font_size(UiFontSize::ALL[ix], window, cx);
            }
            FontKind::Terminal => {
                typography::set_terminal_font_size(MONO_FONT_SIZES[ix], cx);
            }
            FontKind::Code => {
                let next = MONO_FONT_SIZES[ix];
                typography::set_code_font_size(next, cx);
                // Open file editors only learn the new size through this event.
                cx.emit(AppearanceSettingsEvent::CodeFontSizeChanged(next));
            }
        }
        cx.notify();
    }

    /// This kind's catalog, narrowed and ranked by the typed query.
    fn visible_choices(
        &self,
        kind: FontKind,
        availability: &FontAvailability,
        cx: &gpui::App,
    ) -> Vec<UiFontFamily> {
        filter_families(
            self.font_search.read(cx).text(),
            kind.choices_for(availability),
        )
    }

    /// The kind whose menu is open, if any. Opening one closes the others.
    fn open_font_kind(&self) -> Option<FontKind> {
        FontKind::ALL
            .into_iter()
            .find(|kind| self.font_menu(*kind).is_open())
    }

    fn on_font_search_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(kind) = self.open_font_kind() {
            self.clamp_highlight(kind, cx);
        }
    }

    /// Keep the highlighted row inside the filtered list, so the next Enter
    /// cannot commit a family the query has already filtered out of existence.
    fn clamp_highlight(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        let availability = typography::availability(cx);
        let visible = self.visible_choices(kind, &availability, cx);
        if !visible.contains(self.selected_font(kind)) {
            let next = if visible.is_empty() {
                kind.effective(cx)
            } else {
                first_available(&visible, kind, &availability)
            };
            self.set_selected_font(kind, next);
        }
        cx.notify();
    }

    fn close_font_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        if !self.font_menu_mut(kind).begin_close(crate::clock::now(cx)) {
            return;
        }
        match kind {
            FontKind::Ui => popover::reap_popup(cx, |page| &mut page.font_menu),
            FontKind::Terminal => popover::reap_popup(cx, |page| &mut page.terminal_font_menu),
            FontKind::Code => popover::reap_popup(cx, |page| &mut page.code_font_menu),
        }
    }

    /// Only one of this page's six font dropdowns may be open at a time;
    /// opening any one closes the other five.
    fn close_other_menus(
        &mut self,
        keep_family: Option<FontKind>,
        keep_size: Option<FontKind>,
        cx: &mut Context<Self>,
    ) {
        for kind in FontKind::ALL {
            if Some(kind) != keep_family {
                self.close_font_menu(kind, cx);
            }
            if Some(kind) != keep_size {
                widgets::close_select(self, move |page: &mut Self| page.size_select_mut(kind), cx);
            }
        }
    }

    fn dismiss_font_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        *self.font_dismissed_at(kind) = Some(crate::clock::now(cx));
        self.close_font_menu(kind, cx);
    }

    fn toggle_font_menu(&mut self, kind: FontKind, window: &mut Window, cx: &mut Context<Self>) {
        self.close_other_menus(Some(kind), None, cx);
        let just_dismissed = self
            .font_dismissed_at(kind)
            .take()
            .is_some_and(|at| crate::clock::now(cx).saturating_duration_since(at) < std::time::Duration::from_millis(400));
        if self.font_menu(kind).is_open() {
            self.close_font_menu(kind, cx);
        } else if !just_dismissed {
            // Clear before opening: the resulting `Edited` finds no open menu,
            // so it cannot clobber the highlight we anchor on the next line.
            self.font_search
                .update(cx, |input, cx| input.set_text("", cx));
            let effective = kind.effective(cx);
            self.set_selected_font(kind, effective);
            // Every open starts at the top, and the rail's baseline with it —
            // no reopen flash from the previous session's offset.
            self.font_list_mut(kind).reset();
            self.font_menu_mut(kind).open(());
            window.focus(&self.font_search.read(cx).focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Returns whether the key was consumed. The card and its trigger both
    /// listen, and the trigger's guard re-reads `is_open` — which Enter has
    /// already flipped — so a consumed key must not bubble on.
    fn on_font_key_down(
        &mut self,
        kind: FontKind,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();

        if !self.font_menu(kind).is_open() {
            // Closed: any list key opens the menu, and the filter takes over
            // from there — no stepping through hundreds of rows by hand.
            if matches!(
                key,
                "up" | "down" | "left" | "right" | "home" | "end" | "enter" | "space"
            ) {
                *self.font_dismissed_at(kind) = None;
                self.toggle_font_menu(kind, window, cx);
                return true;
            }
            return false;
        }

        // Open: the filter input owns text and caret keys. Only navigation,
        // commit, and dismiss bubble out to us.
        let availability = typography::availability(cx);
        let choices = self.visible_choices(kind, &availability, cx);
        let modifiers = event.keystroke.modifiers;
        let next = match (
            key,
            popover::classify_key(key, modifiers.platform, modifiers.control),
        ) {
            (_, popover::MenuKey::Up) => {
                step_font(self.selected_font(kind), -1, &choices, kind, &availability)
            }
            (_, popover::MenuKey::Down) => {
                step_font(self.selected_font(kind), 1, &choices, kind, &availability)
            }
            ("home", _) => first_available(&choices, kind, &availability),
            ("end", _) => last_available(&choices, kind, &availability),
            (_, popover::MenuKey::Enter) => {
                self.commit_font(kind, cx);
                return true;
            }
            (_, popover::MenuKey::Escape) => {
                let effective = kind.effective(cx);
                self.set_selected_font(kind, effective);
                self.close_font_menu(kind, cx);
                window.focus(&self.font_focus(kind).clone(), cx);
                cx.notify();
                return true;
            }
            _ => return false,
        };
        self.set_selected_font(kind, next);
        cx.notify();
        true
    }

    fn choose_new_thread_background(&mut self, cx: &mut Context<Self>) {
        self.background_error = None;
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a new chat background".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            let _ = this.update(cx, |page, cx| {
                page.background_error =
                    crate::settings::install_new_thread_composer_background(&path, cx)
                        .err()
                        .map(SharedString::from);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_new_thread_background(&mut self, cx: &mut Context<Self>) {
        self.background_error = crate::settings::remove_new_thread_composer_background(cx)
            .err()
            .map(SharedString::from);
        cx.notify();
    }

    fn open_background_adjustment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(background) = crate::settings::current(cx).new_thread_composer_background else {
            return;
        };
        let focus = cx.focus_handle();
        let zoom_focus = cx.focus_handle();
        let return_focus = window.focused(cx);
        window.focus(&focus, cx);
        self.background_adjustment_dialog = Some(BackgroundAdjustmentDialog {
            focus,
            zoom_focus,
            end_focus: cx.focus_handle(),
            return_focus,
            path: background.path,
            source: None,
            draft: background.adjustment.normalized(),
            preview_bounds: Rc::default(),
            zoom_bounds: Rc::default(),
            drag: None,
            pinch: None,
        });
        cx.notify();
    }

    fn dismiss_background_adjustment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = self.background_adjustment_dialog.take()
            && let Some(focus) = dialog.return_focus
        {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn apply_background_adjustment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.background_adjustment_dialog.as_ref() else {
            return;
        };
        // A shuffle or replacement must not receive a draft for another image.
        if dialog.source.is_none()
            || crate::settings::current(cx)
                .new_thread_composer_background
                .as_ref()
                .is_none_or(|background| background.path != dialog.path)
        {
            return;
        }
        crate::settings::set_new_thread_background_adjustment(dialog.draft, cx);
        self.dismiss_background_adjustment(window, cx);
    }

    fn reset_background_adjustment(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.background_adjustment_dialog.as_mut() {
            dialog.draft = Default::default();
            dialog.drag = None;
            dialog.pinch = None;
        }
        cx.notify();
    }

    fn set_background_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.zoom_background_around(zoom, None, cx);
    }

    fn set_background_zoom_from_x(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(bounds) = self
            .background_adjustment_dialog
            .as_ref()
            .and_then(|dialog| dialog.zoom_bounds.get())
        else {
            return;
        };
        self.set_background_zoom(background_zoom_at_x(bounds, x), cx);
    }

    fn zoom_background_around(
        &mut self,
        zoom: f32,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.background_adjustment_dialog.as_mut() else {
            return;
        };
        if dialog.source.is_none() {
            return;
        }
        let zoom = zoom.clamp(
            crate::settings::NewThreadBackgroundAdjustment::MIN_ZOOM,
            crate::settings::NewThreadBackgroundAdjustment::MAX_ZOOM,
        );
        let next = match (dialog.preview_bounds.get(), dialog.source.as_ref()) {
            (Some(bounds), Some(source)) => {
                crate::new_thread_background_mask::zoom_adjustment_around(
                    source,
                    bounds,
                    dialog.draft,
                    zoom,
                    anchor.unwrap_or_else(|| bounds.center()),
                )
            }
            _ => crate::settings::NewThreadBackgroundAdjustment {
                zoom,
                ..dialog.draft
            },
        };
        if dialog.draft != next {
            dialog.draft = next;
            dialog.drag = None;
            cx.notify();
        }
    }

    fn pan_background(&mut self, delta: Point<f32>, cx: &mut Context<Self>) {
        let Some(dialog) = self.background_adjustment_dialog.as_mut() else {
            return;
        };
        let (Some(bounds), Some(source)) = (dialog.preview_bounds.get(), dialog.source.as_ref())
        else {
            return;
        };
        let before = dialog.draft;
        dialog.draft = crate::new_thread_background_mask::pan_adjustment(
            source,
            bounds,
            dialog.draft,
            point(px(delta.x), px(delta.y)),
        );
        if dialog.draft != before {
            cx.notify();
        }
    }

    fn drag_background(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(dialog) = self.background_adjustment_dialog.as_ref() else {
            return;
        };
        let Some((start, initial)) = dialog.drag else {
            return;
        };
        let delta = point(
            f32::from(position.x - start.x),
            f32::from(position.y - start.y),
        );
        if let Some(dialog) = self.background_adjustment_dialog.as_mut() {
            dialog.draft = initial;
        }
        self.pan_background(delta, cx);
    }

    fn finish_background_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.background_adjustment_dialog.as_mut()
            && dialog.drag.take().is_some()
        {
            cx.notify();
        }
    }

    // slopty: a row whose value is not its default offers "Reset", and only then (M10).
    fn reset_row(
        &self,
        id: impl Into<SharedString>,
        label: &str,
        is_default: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        reset: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        widgets::reset_action(theme, id, label, is_default).map(|button| {
            button.on_click(cx.listener(move |this, _, window, cx| {
                reset(this, window, cx);
                cx.notify();
            }))
        })
    }

    /// Back to `kind`'s default family and size.
    fn reset_font(&mut self, kind: FontKind, window: &mut Window, cx: &mut Context<Self>) {
        kind.apply_family(kind.default_family(), cx);
        let effective = kind.effective(cx);
        self.set_selected_font(kind, effective);
        self.commit_size(kind, kind.default_size_ix(), window, cx);
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for AppearancePage {
    // The page's rail; each font dropdown's rail goes through
    // [`widgets::rail`], which can serve further scroll hosts on the same view.
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

fn step_font(
    current: &UiFontFamily,
    delta: isize,
    choices: &[UiFontFamily],
    kind: FontKind,
    availability: &FontAvailability,
) -> UiFontFamily {
    if choices.is_empty() {
        return current.clone();
    }
    let current = choices
        .iter()
        .position(|family| family == current)
        .unwrap_or_default() as isize;
    let mut ix = current + delta.signum();
    while (0..choices.len() as isize).contains(&ix) {
        let candidate = &choices[ix as usize];
        if kind.is_available_for(availability, candidate) {
            return candidate.clone();
        }
        ix += delta.signum();
    }
    choices[current as usize].clone()
}

/// Narrow and rank families by a typed query: prefix matches first, then
/// substring matches, catalog order preserved within each rank. An empty query
/// keeps the catalog untouched, so bundled entries still sort first.
fn filter_families(query: &str, choices: &[UiFontFamily]) -> Vec<UiFontFamily> {
    if query.trim().is_empty() {
        return choices.to_vec();
    }
    let labels: Vec<&str> = choices.iter().map(UiFontFamily::label).collect();
    popover::filter_indices(query, &labels)
        .into_iter()
        .map(|ix| choices[ix].clone())
        .collect()
}

fn first_available(
    choices: &[UiFontFamily],
    kind: FontKind,
    availability: &FontAvailability,
) -> UiFontFamily {
    choices
        .iter()
        .find(|family| kind.is_available_for(availability, family))
        .cloned()
        .unwrap_or_else(|| fallback_selection(kind))
}

fn last_available(
    choices: &[UiFontFamily],
    kind: FontKind,
    availability: &FontAvailability,
) -> UiFontFamily {
    choices
        .iter()
        .rev()
        .find(|family| kind.is_available_for(availability, family))
        .cloned()
        .unwrap_or_else(|| fallback_selection(kind))
}

/// Highlight target when a kind's catalog offers nothing: System UI is
/// proportional, so the terminal cannot land there.
fn fallback_selection(kind: FontKind) -> UiFontFamily {
    match kind {
        FontKind::Terminal => UiFontFamily::GeistMono,
        _ => UiFontFamily::System,
    }
}

fn format_px(size: f32) -> String {
    if size.fract().abs() < f32::EPSILON {
        format!("{size:.0} px")
    } else {
        format!("{size:.1} px")
    }
}

fn bar(fraction: f32, tone: Hsla) -> gpui::Div {
    div()
        .h(px(5.0))
        .w(gpui::relative(fraction))
        .rounded(px(3.0))
        .bg(tone)
}

/// A swatch's name in its tooltip and to assistive tech; the default says so.
fn accent_name(accent: AccentSelection) -> String {
    match accent {
        AccentSelection::ThemeDefault => {
            format!("Theme default · {}", widgets::Standing::Default.words())
        }
        AccentSelection::Preset(preset) => preset.label().into(),
    }
}

fn accent_helper(accent: AccentSelection) -> String {
    match accent {
        AccentSelection::ThemeDefault => "Theme default".into(),
        AccentSelection::Preset(preset) => preset.label().into(),
    }
}

fn surface_label(surface: SurfacePreference) -> &'static str {
    match surface {
        SurfacePreference::ThemeDefault => "Theme default",
        SurfacePreference::Frosted => "Frosted",
        SurfacePreference::Opaque => "Opaque",
    }
}

fn treatment_word(treatment: SurfaceTreatment) -> &'static str {
    match treatment {
        SurfaceTreatment::Frosted => "Frosted",
        SurfaceTreatment::Opaque => "Opaque",
    }
}

fn surface_helper(surface: SurfacePreference, resolved: SurfaceTreatment) -> String {
    match surface {
        SurfacePreference::ThemeDefault => format!(
            "Theme default: {}",
            match resolved {
                SurfaceTreatment::Frosted => "frosted",
                SurfaceTreatment::Opaque => "opaque",
            }
        ),
        SurfacePreference::Frosted => "Translucent surfaces".into(),
        SurfacePreference::Opaque => "Solid surfaces".into(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Corners {
    All,
    Left,
    Right,
}

fn miniature(theme: &Theme, corners: Corners) -> AnyElement {
    let line = theme.text.opacity(0.22);
    let strong = theme.text.opacity(0.34);
    let r = px(widgets::OPTION_CARD_RADIUS);
    let root = div().size_full().flex().flex_row().bg(theme.surface);
    let root = match corners {
        Corners::All => root.rounded(r),
        Corners::Left => root.rounded_tl(r).rounded_bl(r),
        Corners::Right => root.rounded_tr(r).rounded_br(r),
    };
    root.child(
        div()
            .w(px(44.0))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .px(px(8.0))
            .pt(px(14.0))
            .child(bar(0.70, strong))
            .child(bar(1.0, line))
            .child(bar(0.85, line))
            .child(bar(1.0, line)),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .my(px(8.0))
            .mr(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .p(px(10.0))
            .child(bar(0.62, strong))
            .child(bar(0.88, line))
            .child(bar(0.76, line))
            .child(bar(0.52, line)),
    )
    .into_any_element()
}

fn miniature_split(
    themes: &ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
) -> AnyElement {
    let light = Theme::for_selection(Appearance::Light, &themes.light, accent, surface);
    let dark = Theme::for_selection(Appearance::Dark, &themes.dark, accent, surface);
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(
            div()
                .w_1_2()
                .h_full()
                .overflow_hidden()
                .child(miniature(&light, Corners::Left)),
        )
        .child(
            div()
                .w_1_2()
                .h_full()
                .overflow_hidden()
                .child(miniature(&dark, Corners::Right)),
        )
        .into_any_element()
}

fn preview(
    mode: AppearanceMode,
    themes: &ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
) -> AnyElement {
    match mode {
        AppearanceMode::System => miniature_split(themes, accent, surface),
        AppearanceMode::Light => miniature(
            &Theme::for_selection(Appearance::Light, &themes.light, accent, surface),
            Corners::All,
        ),
        AppearanceMode::Dark => miniature(
            &Theme::for_selection(Appearance::Dark, &themes.dark, accent, surface),
            Corners::All,
        ),
    }
}

fn model_appearance(appearance: Appearance) -> slopty_shell_theme::Appearance {
    match appearance {
        Appearance::Dark => slopty_shell_theme::Appearance::Dark,
        Appearance::Light => slopty_shell_theme::Appearance::Light,
    }
}

fn palette_preview(theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .w(px(30.0))
        .h(px(18.0))
        .rounded(px(5.0))
        .overflow_hidden()
        .border_1()
        .border_color(theme.border)
        .flex()
        .child(div().w_1_3().h_full().bg(theme.surface))
        .child(div().w_1_3().h_full().bg(theme.bg))
        .child(div().w_1_3().h_full().bg(theme.accent))
}

// slopty: a row's action reads as every settings row's (Quiet, its words alone), and is named
// by them for a screen reader.
fn compact_action(
    theme: &Theme,
    label: &str,
    id: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    widgets::text_action(theme, widgets::ActionTone::Quiet, label)
        .id(id.into())
        .aria_label(SharedString::from(label.to_owned()))
}

fn accent_swatch(
    page_theme: &Theme,
    selection: AccentSelection,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let swatch_theme = Theme::for_selection(
        page_theme.appearance,
        page_theme.variant_id.as_ref(),
        selection,
        page_theme.surface_preference,
    );
    let sample = match selection {
        AccentSelection::ThemeDefault => div()
            .size_full()
            .rounded(px(6.0))
            .bg(swatch_theme.accent_wash)
            .flex()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .child(
                div()
                    .w(px(4.0))
                    .h(px(13.0))
                    .rounded(px(2.0))
                    .bg(swatch_theme.glyph.light),
            )
            .child(
                div()
                    .w(px(4.0))
                    .h(px(16.0))
                    .rounded(px(2.0))
                    .bg(swatch_theme.glyph.mid),
            )
            .child(
                div()
                    .w(px(4.0))
                    .h(px(11.0))
                    .rounded(px(2.0))
                    .bg(swatch_theme.glyph.deep),
            ),
        AccentSelection::Preset(_) => div().size_full().rounded(px(6.0)).bg(swatch_theme.accent),
    };
    div()
        .id(SharedString::from(format!("accent-{}", selection.label())))
        .aria_label(selection.label())
        .aria_selected(selected)
        .flex_none()
        .w(px(30.0))
        .h(px(34.0))
        .pb(px(4.0))
        .border_b_2()
        .border_color(if selected {
            swatch_theme.accent
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .child(
            div()
                .size(px(30.0))
                .p(px(2.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(if selected {
                    page_theme.border_strong
                } else {
                    page_theme.border
                })
                .bg(page_theme.surface_raised.opacity(0.42))
                .child(sample),
        )
}

impl AppearancePage {
    fn render_font_picker(
        &mut self,
        kind: FontKind,
        theme: &Theme,
        availability: &FontAvailability,
        fixed: SharedString,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slug = kind.slug();
        // The scroll helpers key off `&'static str`, so the ids are spelled
        // out rather than formatted from the slug.
        let (host_id, list_id, rail_id) = match kind {
            FontKind::Ui => (
                "interface-font-host",
                "interface-font-scroll",
                "interface-font-scrollbar",
            ),
            FontKind::Terminal => (
                "terminal-font-host",
                "terminal-font-scroll",
                "terminal-font-scrollbar",
            ),
            FontKind::Code => ("code-font-host", "code-font-scroll", "code-font-scrollbar"),
        };
        let effective = kind.effective(cx);
        let selected = self.selected_font(kind).clone();
        let visible = self.visible_choices(kind, availability, cx);
        let filtered = !self.font_search.read(cx).text().trim().is_empty();
        let rows: Vec<AnyElement> = visible
            .into_iter()
            .enumerate()
            .map(|(ix, family)| {
                let available = kind.is_available_for(availability, &family);
                let is_default = family == kind.default_family();
                let active = family == effective;
                let focused = family == selected;
                let label = SharedString::from(family.label().to_owned());
                popover::picker_row(theme, active, focused, format!("{slug}-font-option-{ix}"))
                    .id(SharedString::from(format!("{slug}-font-option-{ix}")))
                    .role(gpui::Role::MenuItemRadio)
                    .aria_label(label.clone())
                    .aria_toggled(if active {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .when(available, |row| {
                        row.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.set_selected_font(kind, family.clone());
                            this.commit_font(kind, cx);
                        }))
                    })
                    .when(!available, |row| row.opacity(crate::theme::DISABLED_OPACITY))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .children(
                        is_default
                            .then(|| widgets::standing_note(theme, widgets::Standing::Default)),
                    )
                    .into_any_element()
            })
            .collect();

        let list: AnyElement = if rows.is_empty() {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.for_popup().text_faint)
                .child(SharedString::from(if filtered {
                    "No matching fonts"
                } else {
                    "No fonts"
                }))
                .into_any_element()
        } else {
            // Card-bleed scroll host (see [`popover::menu_scroll_host`]): the
            // rail mounts as a sibling of the scroller, above its clip. The
            // bleed stays horizontal — the search input sits above the list.
            let rail = widgets::rail(self.font_list_mut(kind), rail_id, theme, cx, move |page| {
                page.font_list_mut(kind)
            });
            let scroll = self.font_list_mut(kind).scroll.clone();
            popover::menu_scroll_host(host_id)
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if this.font_list_mut(kind).set_list_hovered(*hovered) {
                        cx.notify();
                    }
                }))
                .child(popover::faded_menu_list(
                    &scroll,
                    popover::menu_scroll_list(list_id, &scroll)
                        .max_h(px(widgets::dropdown_list_height(
                            viewport,
                            widgets::SELECT_HEIGHT,
                            52.0,
                        )))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(rows),
                ))
                .children(rail)
                .into_any_element()
        };

        // The key handler sits on the card, not just the trigger: focus moves
        // into the filter input, and `PaletteSearch` lets arrows/Enter/Escape
        // bubble to exactly this ancestor.
        let menu = widgets::select_menu(theme)
            .w(px(220.0))
            .font_family(fixed.clone())
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| this.dismiss_font_menu(kind, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if this.on_font_key_down(kind, event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                self.font_search.clone().into_any_element(),
            ))
            .child(list);

        let open = self.font_menu(kind).is_open();
        let closing = self.font_menu(kind).closing_at(crate::clock::now(cx));
        let value = SharedString::from(effective.label().to_owned());
        widgets::select_trigger(theme, format!("{slug}-font-dropdown"), open)
            .w(px(220.0))
            .font_family(fixed)
            .aria_label(format!("{}: {value}", kind.label()))
            .track_focus(&self.font_focus(kind).clone().tab_stop(true))
            // Only the closed state: once open, the card above owns these keys
            // and stops their propagation before they reach us.
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if !this.font_menu(kind).is_open() {
                    if this.on_font_key_down(kind, event, window, cx) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.font_focus(kind).clone(), cx);
                this.toggle_font_menu(kind, window, cx);
            }))
            .child(widgets::select_value(value))
            .child(widgets::select_chevron(theme, open))
            .when_some(self.font_menu(kind).get(), |trigger, _| {
                trigger.child(widgets::dropdown(
                    SharedString::from(format!("{slug}-font-menu")),
                    menu,
                    closing,
                    widgets::SELECT_HEIGHT,
                ))
            })
            .into_any_element()
    }

    /// The size dropdown beside the family picker. Every kind picks from a
    /// discrete ladder, so all three rows read alike.
    fn render_size_picker(
        &mut self,
        kind: FontKind,
        theme: &Theme,
        fixed: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slug = kind.slug();
        widgets::select(
            format!("{slug}-font-size-dropdown"),
            format!("{} size", kind.label()),
            theme,
            move |page: &mut Self| page.size_select_mut(kind),
        )
        .options(
            kind.size_labels()
                .into_iter()
                .enumerate()
                .map(move |(ix, label)| {
                    widgets::SelectOption::new(label)
                        .standing(widgets::Standing::of(ix == kind.default_size_ix(), false))
                }),
            kind.size_ix(cx),
        )
        .width(128.0)
        .font_family(fixed)
        .on_select(move |page, ix, window, cx| page.commit_size(kind, ix, window, cx))
        .render(self.size_select(kind), cx)
        .into_any_element()
    }

    fn render_theme_selector(
        &mut self,
        appearance_kind: Appearance,
        selections: &ThemeSelection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let registry = ThemeRegistry::builtin();
        let selected_id = selections
            .variant_id(model_appearance(appearance_kind))
            .to_owned();
        let surface = theme.surface_preference;
        let variants: Vec<_> = registry
            .variants_for(model_appearance(appearance_kind))
            .map(|variant| (variant.id.clone(), variant.name.clone()))
            .collect();
        let selected = variants
            .iter()
            .position(|(id, _)| *id == selected_id)
            .unwrap_or_default();
        let default_id = ThemeSelection::default()
            .variant_id(model_appearance(appearance_kind))
            .to_owned();
        let options = variants.iter().map(|(id, name)| {
            let sample =
                Theme::for_selection(appearance_kind, id, AccentSelection::ThemeDefault, surface);
            widgets::SelectOption::new(name.clone())
                .leading(move || palette_preview(&sample).into_any_element())
                .standing(widgets::Standing::of(*id == default_id, false))
        });
        let ids: Vec<String> = variants.iter().map(|(id, _)| id.clone()).collect();
        let (slug, label, heading) = if appearance_kind.is_light() {
            ("light", "Light theme", "Light themes")
        } else {
            ("dark", "Dark theme", "Dark themes")
        };
        widgets::select(
            format!("{slug}-theme-selector"),
            label,
            theme,
            move |page: &mut Self| match appearance_kind {
                Appearance::Light => &mut page.light_theme_select,
                Appearance::Dark => &mut page.dark_theme_select,
            },
        )
        .options(options, selected)
        .width(218.0)
        .menu_width(260.0)
        .heading(heading)
        .on_select(move |_, ix, _, cx| {
            if let Some(id) = ids.get(ix) {
                appearance::set_theme(appearance_kind, id.clone(), cx);
            }
            cx.notify();
        })
        .render(
            match appearance_kind {
                Appearance::Light => &self.light_theme_select,
                Appearance::Dark => &self.dark_theme_select,
            },
            cx,
        )
        .into_any_element()
    }

    fn render_background_adjustment_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.background_adjustment_dialog.as_ref()?;
        let focus = dialog.focus.clone();
        let zoom_focus = dialog.zoom_focus.clone();
        let end_focus = dialog.end_focus.clone();
        let tab_start = focus.clone();
        let tab_end = end_focus.clone();
        let path = dialog.path.clone();
        let draft = dialog.draft;
        let preview_bounds = dialog.preview_bounds.clone();
        let zoom_bounds = dialog.zoom_bounds.clone();
        let dragging = dialog.drag.is_some();
        let settings = crate::settings::current(cx);
        let same_image = settings
            .new_thread_composer_background
            .as_ref()
            .is_some_and(|background| background.path == path);
        let available = Path::new(&path).is_file();
        let source = (same_image && available)
            .then(|| {
                crate::new_thread_background_effects::prepare(Path::new(&path), cx)
            })
            .flatten();
        let ready = source.is_some();
        let preview_size =
            background_preview_size(viewport, settings.sidebar_width, settings.sidebar_collapsed);
        if let Some(dialog) = self.background_adjustment_dialog.as_mut() {
            dialog.source = source.clone();
        }

        let paint_source = source.clone();
        let measured_bounds = preview_bounds.clone();
        let preview = div()
            .id("new-thread-background-adjustment-preview")
            .relative()
            .w(preview_size.width + px(2.0))
            .h(preview_size.height + px(2.0))
            .overflow_hidden()
            .rounded(px(12.0))
            .border_1()
            .border_color(crate::theme::hairline(0.12))
            .bg(theme.bg)
            .cursor(if !ready {
                gpui::CursorStyle::Arrow
            } else if dragging {
                gpui::CursorStyle::ClosedHand
            } else {
                gpui::CursorStyle::OpenHand
            })
            .on_scroll_wheel(
                cx.listener(|this, event: &gpui::ScrollWheelEvent, window, cx| {
                    let delta = match event.delta {
                        ScrollDelta::Pixels(delta) => f32::from(delta.y),
                        ScrollDelta::Lines(delta) => delta.y * 40.0,
                    };
                    let Some(dialog) = this.background_adjustment_dialog.as_ref() else {
                        return;
                    };
                    let zoom = dialog.draft.zoom * (delta * 0.0025).clamp(-2.0, 2.0).exp();
                    this.zoom_background_around(zoom, Some(event.position), cx);
                    cx.stop_propagation();
                    window.prevent_default();
                }),
            )
            .on_pinch(cx.listener(|this, event: &gpui::PinchEvent, window, cx| {
                let Some(dialog) = this.background_adjustment_dialog.as_mut() else {
                    return;
                };
                if event.phase == gpui::TouchPhase::Ended {
                    dialog.pinch = None;
                    return;
                }
                if event.phase == gpui::TouchPhase::Started {
                    dialog.pinch = Some((dialog.draft.zoom, 1.0));
                }
                let current_zoom = dialog.draft.zoom;
                let (start, factor) = dialog.pinch.get_or_insert((current_zoom, 1.0));
                *factor =
                    background_pinch_factor(*factor, event.delta, cfg!(target_os = "windows"));
                let zoom = *start * *factor;
                this.zoom_background_around(zoom, Some(event.position), cx);
                cx.stop_propagation();
                window.prevent_default();
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    let Some(dialog) = this.background_adjustment_dialog.as_mut() else {
                        return;
                    };
                    if dialog.source.is_none() {
                        return;
                    }
                    window.focus(&dialog.focus, cx);
                    dialog.drag = Some((event.position, dialog.draft));
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_drag(BackgroundAdjustmentDrag, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .child(
                gpui::canvas(
                    move |bounds, _, _| measured_bounds.set(Some(bounds)),
                    move |bounds, _, window, _| {
                        if let Some(source) = paint_source.clone() {
                            crate::new_thread_background_mask::paint_adjusted(
                                source,
                                bounds,
                                draft,
                                // Canvas content masks are rectangular; round the image
                                // itself to the frame's 12px radius inside its 1px border.
                                gpui::Corners::all(px(11.0)),
                                window,
                            );
                        }
                    },
                )
                .w(preview_size.width)
                .h(preview_size.height),
            )
            .when(source.is_none(), |preview| {
                preview.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .child(if !same_image {
                            "Background changed. Close and adjust the new image."
                        } else if !available {
                            "Image unavailable. Choose a replacement or remove it."
                        } else {
                            "Preparing preview…"
                        }),
                )
            })
            .when(dragging, |preview| {
                preview
                    .child(
                        div()
                            .absolute()
                            .left(gpui::relative(1.0 / 3.0))
                            .top_0()
                            .bottom_0()
                            .w(px(1.0))
                            .bg(gpui::white().opacity(0.22)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(gpui::relative(2.0 / 3.0))
                            .top_0()
                            .bottom_0()
                            .w(px(1.0))
                            .bg(gpui::white().opacity(0.22)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(gpui::relative(1.0 / 3.0))
                            .left_0()
                            .right_0()
                            .h(px(1.0))
                            .bg(gpui::white().opacity(0.22)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(gpui::relative(2.0 / 3.0))
                            .left_0()
                            .right_0()
                            .h(px(1.0))
                            .bg(gpui::white().opacity(0.22)),
                    )
            });

        let zoom_fraction = background_zoom_fraction(draft.zoom);
        let measured_zoom_bounds = zoom_bounds.clone();
        let zoom_slider = div()
            .id("new-thread-background-adjustment-zoom")
            .track_focus(&zoom_focus)
            .tab_index(0)
            .role(gpui::Role::Slider)
            .aria_label("Background zoom")
            .aria_numeric_value(f64::from(draft.zoom * 100.0))
            .aria_min_numeric_value(f64::from(
                crate::settings::NewThreadBackgroundAdjustment::MIN_ZOOM * 100.0,
            ))
            .aria_max_numeric_value(f64::from(
                crate::settings::NewThreadBackgroundAdjustment::MAX_ZOOM * 100.0,
            ))
            .aria_numeric_value_step(f64::from(BACKGROUND_ZOOM_STEP * 100.0))
            .aria_value(format!("{:.0}%", draft.zoom * 100.0))
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .relative()
            .w(px(168.0))
            .h(px(28.0))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    if let Some(dialog) = this.background_adjustment_dialog.as_ref() {
                        window.focus(&dialog.zoom_focus, cx);
                    }
                    this.set_background_zoom_from_x(event.position.x, cx);
                    cx.stop_propagation();
                    window.prevent_default();
                }),
            )
            .on_drag(BackgroundAdjustmentZoomDrag, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let Some(current) = this
                    .background_adjustment_dialog
                    .as_ref()
                    .map(|dialog| dialog.draft.zoom)
                else {
                    return;
                };
                let next = match event.keystroke.key.as_str() {
                    "left" | "down" => current - BACKGROUND_ZOOM_STEP,
                    "right" | "up" => current + BACKGROUND_ZOOM_STEP,
                    "home" => crate::settings::NewThreadBackgroundAdjustment::MIN_ZOOM,
                    "end" => crate::settings::NewThreadBackgroundAdjustment::MAX_ZOOM,
                    _ => return,
                };
                this.set_background_zoom(next, cx);
                cx.stop_propagation();
                window.prevent_default();
            }))
            .child(
                gpui::canvas(
                    move |bounds, _, _| measured_zoom_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .left(px(7.0))
                    .right(px(7.0))
                    .top(px(12.0))
                    .h(px(4.0))
                    .rounded_full()
                    .bg(theme.border)
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(zoom_fraction))
                            .rounded_full()
                            .bg(theme.accent),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(gpui::relative(zoom_fraction))
                            .ml(px(-7.0))
                            .top(px(-5.0))
                            .size(px(14.0))
                            .rounded_full()
                            .bg(theme.accent),
                    ),
            );
        let controls = div()
            .mt(px(14.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(
                compact_action(theme, "Reset", "new-thread-background-adjustment-reset")
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .map(|el| crate::surface_chrome::focus_ring_revealing(el, theme))
                    .on_click(cx.listener(|this, _, _, cx| this.reset_background_adjustment(cx))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        compact_action(theme, "−", "new-thread-background-adjustment-zoom-out")
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label("Zoom out")
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(dialog) = this.background_adjustment_dialog.as_ref()
                                else {
                                    return;
                                };
                                this.set_background_zoom(
                                    dialog.draft.zoom - BACKGROUND_ZOOM_STEP,
                                    cx,
                                );
                            })),
                    )
                    .child(zoom_slider)
                    .child(
                        div()
                            .w(px(52.0))
                            .text_center()
                            .text_size(typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child(format!("{:.0}%", draft.zoom * 100.0)),
                    )
                    .child(
                        compact_action(theme, "+", "new-thread-background-adjustment-zoom-in")
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label("Zoom in")
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(dialog) = this.background_adjustment_dialog.as_ref()
                                else {
                                    return;
                                };
                                this.set_background_zoom(
                                    dialog.draft.zoom + BACKGROUND_ZOOM_STEP,
                                    cx,
                                );
                            })),
                    ),
            );

        let footer = div()
            .mt(px(18.0))
            .pt(px(12.0))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .child(
                compact_action(theme, "Cancel", "new-thread-background-adjustment-cancel")
                    .h(px(34.0))
                    .px(px(13.0))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .map(|el| crate::surface_chrome::focus_ring_revealing(el, theme))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_background_adjustment(window, cx)
                    })),
            )
            .child(
                widgets::text_action(theme, widgets::ActionTone::Solid, "Apply")
                    .id("new-thread-background-adjustment-apply")
                    .h(px(34.0))
                    .px(px(14.0))
                    .py(px(0.0))
                    .flex()
                    .items_center()
                    .when(!ready, |button| {
                        button.opacity(crate::theme::DISABLED_OPACITY).cursor_default()
                    })
                    .when(ready, |button| {
                        button
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, theme))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.apply_background_adjustment(window, cx)
                            }))
                    }),
            );

        let header = div()
            .flex()
            .items_start()
            .gap(px(16.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(popover::dialog_title(theme, "Adjust background"))
                    .child(
                        popover::dialog_body(theme, "Drag to reposition. Scroll or pinch to zoom.")
                            .mt(px(4.0)),
                    ),
            )
            .child(
                div()
                    .id("new-thread-background-adjustment-close")
                    .size(px(28.0))
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface_raised.opacity(0.28))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_raised_hover))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label("Close background adjustment")
                    .map(|el| crate::surface_chrome::focus_ring_revealing(el, theme))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_background_adjustment(window, cx)
                    }))
                    .child(
                        icons::icon(icons::CLOSE)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    ),
            );

        let card = popover::dialog_card(theme)
            .id("new-thread-background-adjustment-card")
            .w(px((f32::from(viewport.width) - 40.0).clamp(1.0, 600.0)))
            .p(px(20.0))
            .track_focus(&focus)
            .role(gpui::Role::Dialog)
            .aria_label("Adjust background")
            .tab_group()
            .tab_index(0)
            .tab_stop(false)
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    let step = if event.keystroke.modifiers.shift {
                        24.0
                    } else {
                        8.0
                    };
                    match popover::classify_key(
                        event.keystroke.key.as_str(),
                        event.keystroke.modifiers.platform,
                        event.keystroke.modifiers.control,
                    ) {
                        popover::MenuKey::Escape => this.dismiss_background_adjustment(window, cx),
                        popover::MenuKey::ModEnter => this.apply_background_adjustment(window, cx),
                        _ => match event.keystroke.key.as_str() {
                            "tab" => {
                                if event.keystroke.modifiers.shift {
                                    window.focus_prev(cx);
                                } else {
                                    window.focus_next(cx);
                                }
                                if !tab_start.contains_focused(window, cx) {
                                    if event.keystroke.modifiers.shift {
                                        window.focus(&tab_end, cx);
                                        window.focus_prev(cx);
                                    } else {
                                        window.focus(&tab_start, cx);
                                        window.focus_next(cx);
                                    }
                                }
                            }
                            "left" => this.pan_background(point(step, 0.0), cx),
                            "right" => this.pan_background(point(-step, 0.0), cx),
                            "up" => this.pan_background(point(0.0, step), cx),
                            "down" => this.pan_background(point(0.0, -step), cx),
                            "+" | "=" => {
                                let zoom = this
                                    .background_adjustment_dialog
                                    .as_ref()
                                    .map_or(1.0, |dialog| dialog.draft.zoom + BACKGROUND_ZOOM_STEP);
                                this.set_background_zoom(zoom, cx);
                            }
                            "-" => {
                                let zoom = this
                                    .background_adjustment_dialog
                                    .as_ref()
                                    .map_or(1.0, |dialog| dialog.draft.zoom - BACKGROUND_ZOOM_STEP);
                                this.set_background_zoom(zoom, cx);
                            }
                            _ => return,
                        },
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                }),
            )
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.dismiss_background_adjustment(window, cx)),
            )
            .child(header)
            .child(div().h(px(16.0)))
            .child(
                div()
                    .w_full()
                    .h(px(260.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(preview),
            )
            .child(controls)
            .child(footer)
            .child(div().track_focus(&end_focus).tab_index(0).tab_stop(false))
            .into_any_element();

        Some(popover::modal(
            "new-thread-background-adjustment-dialog",
            viewport,
            card,
        ))
    }
}

impl EventEmitter<AppearanceSettingsEvent> for AppearancePage {}

impl Render for AppearancePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::toast::take_error(&mut self.background_error, cx);
        let theme = Theme::of(cx).for_settings_surface();
        let availability = typography::availability(cx);
        let fixed = theme.font_sans_fixed.clone();
        let current_mode = appearance::mode(cx);
        let current_themes = appearance::themes(cx);
        let current_accent = appearance::accent(cx);
        let current_surface = appearance::surface(cx);
        let ui_settings = crate::settings::current(cx);
        let current_background = ui_settings.new_thread_composer_background;
        let reduced_motion = crate::motion::reduced_motion(cx);
        let cards = AppearanceMode::ALL
            .into_iter()
            .map(|mode| {
                let selected = mode == current_mode;
                let selection_t = widgets::tab_selection_t(
                    window,
                    format!("appearance-mode-{}-selection", mode.label()),
                    selected,
                    reduced_motion,
                );
                widgets::option_card(
                    &theme,
                    mode.icon(),
                    mode.label(),
                    selected,
                    selection_t,
                    preview(mode, &current_themes, current_accent, current_surface),
                )
                // slopty: the default card says so, on a line every card keeps (M10).
                .child(
                    div()
                        .h(px(14.0))
                        .flex()
                        .justify_center()
                        .children((mode == AppearanceMode::default()).then(|| {
                            widgets::standing_note(&theme, widgets::Standing::Default).ml_0()
                        })),
                )
                .id(SharedString::from(format!("appearance-{}", mode.label())))
                .tab_index(0)
                .role(gpui::Role::Button)
                .aria_label(mode.label())
                .aria_selected(selected)
                .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                .on_click(cx.listener(move |_, _, _, cx| {
                    appearance::set_mode(mode, cx);
                    cx.notify();
                }))
            })
            .collect::<Vec<_>>();

        let mut theme_rows = Vec::new();
        for (index, appearance_kind) in [Appearance::Light, Appearance::Dark]
            .into_iter()
            .enumerate()
        {
            let label = if appearance_kind.is_light() {
                "Light theme"
            } else {
                "Dark theme"
            };
            let selector = self.render_theme_selector(appearance_kind, &current_themes, &theme, cx);
            let default_id = ThemeSelection::default()
                .variant_id(model_appearance(appearance_kind))
                .to_owned();
            let reset = self.reset_row(
                format!("{}-theme-reset", if appearance_kind.is_light() { "light" } else { "dark" }),
                label,
                current_themes.variant_id(model_appearance(appearance_kind)) == default_id,
                &theme,
                cx,
                move |_, _, cx| appearance::set_theme(appearance_kind, default_id.clone(), cx),
            );
            // slopty: the chosen theme's one line of voice sits under the row's name (Amp's).
            let voice = ThemeRegistry::builtin()
                .resolve(&current_themes, model_appearance(appearance_kind))
                .description
                .clone();
            let slug = if appearance_kind.is_light() { "light" } else { "dark" };
            theme_rows.push(
                widgets::card_row(&theme, index == 0)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .child(widgets::row_title(&theme, label))
                            .child(widgets::meta_line(
                                &theme,
                                vec![
                                    div()
                                        .id(format!("{slug}-theme-voice"))
                                        .role(gpui::Role::Label)
                                        .aria_label(voice.clone())
                                        .child(voice)
                                        .into_any_element(),
                                ],
                            )),
                    )
                    .children(reset)
                    .child(selector)
                    .into_any_element(),
            );
        }

        let mut accent_choices = vec![AccentSelection::ThemeDefault];
        accent_choices.extend(AccentPreset::ALL.map(AccentSelection::Preset));
        let accent_controls = accent_choices
            .into_iter()
            .map(|selection| {
                let selected = selection == current_accent;
                accent_swatch(&theme, selection, selected)
                    .hint(accent_name(selection))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        appearance::set_accent(selection, cx);
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
        let surface_control =
            widgets::select("appearance-surface", "Glass", &theme, |page: &mut Self| {
                &mut page.surface_select
            })
            .options(
                SurfacePreference::ALL.into_iter().map(|surface| {
                    let option = widgets::SelectOption::new(surface_label(surface));
                    // slopty: the default names what it resolves to ("Default · Frosted").
                    if surface == SurfacePreference::default() {
                        option.detail(format!(
                            "{} · {}",
                            widgets::Standing::Default.words(),
                            treatment_word(
                                ThemeRegistry::builtin()
                                    .variant(theme.variant_id.as_ref())
                                    .map_or(theme.surface_treatment, |variant| {
                                        variant.recommended_surface_treatment
                                    })
                            )
                        ))
                    } else {
                        option
                    }
                }),
                SurfacePreference::ALL
                    .into_iter()
                    .position(|surface| surface == current_surface)
                    .unwrap_or_default(),
            )
            .width(148.0)
            .on_select(|_, ix, _, cx| {
                appearance::set_surface(SurfacePreference::ALL[ix], cx);
                cx.notify();
            })
            .render(&self.surface_select, cx);
        let mut settings_rows = theme_rows;
        settings_rows.push(
            widgets::card_row(&theme, false)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .child(widgets::row_title(&theme, "Accent color"))
                        .child(widgets::meta_line(
                            &theme,
                            vec![
                                div()
                                    .child(SharedString::from(if crate::settings::current(cx).wallpaper_theme_colors {
                                        "Wallpaper colors are enabled; this accent is used when they are off.".to_string()
                                    } else { accent_helper(current_accent) }))
                                    .into_any_element(),
                            ],
                        )),
                )
                .children(self.reset_row(
                    "accent-reset",
                    "Accent color",
                    current_accent == AccentSelection::ThemeDefault,
                    &theme,
                    cx,
                    |_, _, cx| appearance::set_accent(AccentSelection::ThemeDefault, cx),
                ))
                .child(
                    div()
                        .max_w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .children(accent_controls),
                )
                .into_any_element(),
        );
        let match_wallpaper = crate::settings::current(cx).wallpaper_theme_colors;
        settings_rows.push(
            widgets::card_row(&theme, false)
                .child(div().flex_1().min_w_0()
                    .child(widgets::row_title(&theme, "Match wallpaper colors"))
                    .child(widgets::meta_line(&theme, vec![div()
                        .child("Use wallpaper colors for accents, highlights, and subtle surface tints.")
                        .into_any_element()])))
                .children(
                    widgets::reset_action(
                        &theme,
                        "wallpaper-theme-colors-reset",
                        "Match wallpaper colors",
                        !match_wallpaper,
                    )
                    .map(|reset| {
                        reset.on_click(cx.listener(|_, _, _, cx| {
                            crate::settings::wallpaper_colors::set_enabled(false, cx);
                            cx.notify();
                        }))
                    }),
                )
                .child(widgets::toggle_switch(&theme, match_wallpaper, "wallpaper-theme-colors")
                    .id("wallpaper-theme-colors-toggle")
                    .tab_index(0)
                    .role(gpui::Role::Switch)
                    .aria_label("Match wallpaper colors")
                    .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                    .cursor_pointer()
                    .aria_toggled(if match_wallpaper { gpui::Toggled::True } else { gpui::Toggled::False })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        crate::settings::wallpaper_colors::set_enabled(!match_wallpaper, cx);
                        cx.notify();
                    })))
                .into_any_element(),
        );
        let color_rows = settings_rows;
        let mut settings_rows = Vec::new();
        settings_rows.push(
            widgets::card_row(&theme, true)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .child(widgets::row_title(&theme, "Glass"))
                        .child(widgets::meta_line(
                            &theme,
                            vec![
                                div()
                                    .child(SharedString::from(surface_helper(
                                        current_surface,
                                        theme.surface_treatment,
                                    )))
                                    .into_any_element(),
                            ],
                        )),
                )
                .children(self.reset_row(
                    "appearance-surface-reset",
                    "Glass",
                    current_surface == SurfacePreference::default(),
                    &theme,
                    cx,
                    |_, _, cx| appearance::set_surface(SurfacePreference::default(), cx),
                ))
                .child(surface_control)
                .into_any_element(),
        );
        let background_available = current_background
            .as_ref()
            .is_some_and(|background| Path::new(&background.path).is_file());
        // The chosen image previews in the row; without one the row is text only.
        let background_tile: Option<AnyElement> = current_background
            .as_ref()
            .filter(|_| background_available)
            .map(|background| {
                div()
                    .flex_none()
                    .size(px(36.0))
                    .rounded(px(10.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(crate::theme::hairline(0.10))
                    .child(
                        img(PathBuf::from(background.path.clone()))
                            .size(px(34.0))
                            .rounded(px(9.0))
                            .object_fit(ObjectFit::Cover),
                    )
                    .into_any_element()
            });
        let background_meta = match current_background.as_ref() {
            Some(background) if background_available => vec![
                div()
                    .child(SharedString::from(background.name.clone()))
                    .into_any_element(),
            ],
            Some(_) => vec![
                div().child("Image unavailable").into_any_element(),
                div()
                    .child("Choose a replacement or remove it.")
                    .into_any_element(),
            ],
            None => vec![
                div().child("No image selected").into_any_element(),
                div().child(widgets::Standing::Default.words()).into_any_element(),
            ],
        };
        settings_rows.push(
            widgets::card_row(&theme, false)
                .children(background_tile)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .child(widgets::row_title(&theme, "New chat background"))
                        .child(widgets::meta_line(&theme, background_meta)),
                )
                .child(
                    div()
                        .max_w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .when(current_background.is_some(), |actions| {
                            actions
                                .when(background_available, |actions| {
                                    actions.child(
                                        compact_action(
                                            &theme,
                                            "Adjust",
                                            "new-thread-background-adjust",
                                        )
                                        .tab_index(0)
                                        .role(gpui::Role::Button)
                                        .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.open_background_adjustment(window, cx)
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    compact_action(
                                        &theme,
                                        "Replace image",
                                        "new-thread-background-replace",
                                    )
                                    .tab_index(0)
                                    .role(gpui::Role::Button)
                                    .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                                    .on_click(cx.listener(
                                        |this, _, _, cx| this.choose_new_thread_background(cx),
                                    )),
                                )
                                .child(
                                    // slopty: a page's removal reads as Delete all's, its
                                    // words in the muted red, named by them.
                                    widgets::text_action(&theme, widgets::ActionTone::QuietDanger, "Remove")
                                        .id("new-thread-background-remove")
                                        .aria_label("Remove")
                                    .tab_index(0)
                                    .role(gpui::Role::Button)
                                    .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                                    .on_click(cx.listener(
                                        |this, _, _, cx| this.remove_new_thread_background(cx),
                                    )),
                                )
                        })
                        .when(current_background.is_none(), |actions| {
                            actions.child(
                                compact_action(
                                    &theme,
                                    "Choose image",
                                    "new-thread-background-choose",
                                )
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.choose_new_thread_background(cx),
                                )),
                            )
                        }),
                )
                .into_any_element(),
        );
        let pause_in_background = crate::motion::pause_in_background(cx);
        let motion_rows = vec![
            widgets::card_row(&theme, true)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::row_title(&theme, "Pause animations in background"))
                        .child(widgets::meta_line(
                            &theme,
                            vec![
                                div()
                                    .child(
                                        "Hold animations still while Slopty isn't the focused window.",
                                    )
                                    .into_any_element(),
                            ],
                        )),
                )
                .children(
                    widgets::reset_action(
                        &theme,
                        "pause-animations-in-background-reset",
                        "Pause animations in background",
                        !pause_in_background,
                    )
                    .map(|reset| {
                        reset.on_click(cx.listener(|_, _, _, cx| {
                            crate::motion::set_pause_in_background(false, cx);
                            cx.notify();
                        }))
                    }),
                )
                .child(
                    widgets::toggle_switch(
                        &theme,
                        pause_in_background,
                        "pause-animations-in-background",
                    )
                    .id("pause-animations-in-background-toggle")
                    .tab_index(0)
                    .role(gpui::Role::Switch)
                    .aria_label("Pause animations in background")
                    .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                    .cursor_pointer()
                    .aria_toggled(if pause_in_background {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        crate::motion::set_pause_in_background(!pause_in_background, cx);
                        cx.notify();
                    })),
                )
                .into_any_element(),
        ];
        let modal = self.render_background_adjustment_dialog(window.viewport_size(), &theme, cx);

        let ui_picker = self.render_font_picker(
            FontKind::Ui,
            &theme,
            &availability,
            fixed.clone(),
            window.viewport_size(),
            cx,
        );
        let terminal_picker = self.render_font_picker(
            FontKind::Terminal,
            &theme,
            &availability,
            fixed.clone(),
            window.viewport_size(),
            cx,
        );
        let code_picker = self.render_font_picker(
            FontKind::Code,
            &theme,
            &availability,
            fixed.clone(),
            window.viewport_size(),
            cx,
        );
        let ui_size = self.render_size_picker(FontKind::Ui, &theme, fixed.clone(), cx);
        let terminal_size = self.render_size_picker(FontKind::Terminal, &theme, fixed.clone(), cx);
        let code_size = self.render_size_picker(FontKind::Code, &theme, fixed.clone(), cx);

        let mut font_rows = widgets::section_card(&theme)
            .mt_0()
            .font_family(fixed.clone());
        for (ix, (kind, picker, size_control)) in [
            (FontKind::Ui, ui_picker, ui_size),
            (FontKind::Terminal, terminal_picker, terminal_size),
            (FontKind::Code, code_picker, code_size),
        ]
        .into_iter()
        .enumerate()
        {
            let font_default =
                kind.requested(cx) == kind.default_family() && kind.size_ix(cx) == kind.default_size_ix();
            let reset = self.reset_row(
                format!("{}-font-reset", kind.slug()),
                kind.label(),
                font_default,
                &theme,
                cx,
                move |page, window, cx| page.reset_font(kind, window, cx),
            );
            font_rows = font_rows.child(
                widgets::card_row(&theme, ix == 0)
                    .justify_between()
                    .child(
                        div()
                            .min_w(px(160.0))
                            .flex_1()
                            .child(widgets::row_title(&theme, kind.label()))
                            .child(widgets::meta_line(
                                &theme,
                                vec![div().child(kind.description()).into_any_element()],
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .children(reset)
                            .child(picker)
                            .child(size_control),
                    ),
            );
        }
        // slopty: no maths rows: formulas have one typesetter, face and size.
        font_rows = font_rows.child(self.render_conversation_width(&theme, cx));
        let mut font_section = widgets::section(&theme, "Fonts and layout", font_rows);
        for kind in FontKind::ALL {
            let (requested, effective) = (kind.requested(cx), kind.effective(cx));
            if requested != effective {
                font_section = font_section.child(
                    widgets::error_strip(
                        &theme,
                        format!(
                            "{} \"{}\" isn't available on this device. Using {}.",
                            kind.label(),
                            requested.label(),
                            effective.label()
                        ),
                    )
                    .mt_0()
                    .font_family(fixed.clone()),
                );
            }
        }

        let scrollbar = popover::rail(self, "appearance-page-scrollbar", &theme, cx);
        div()
            .id("appearance-page-host")
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<BackgroundAdjustmentDrag>, _, cx| {
                    this.drag_background(event.event.position, cx);
                },
            ))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<BackgroundAdjustmentZoomDrag>, _, cx| {
                    this.set_background_zoom_from_x(event.event.position.x, cx);
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.finish_background_drag(cx);
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.finish_background_drag(cx);
                }),
            )
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("appearance-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                // slopty: one page head (`widgets::page_head`).
                                .child(widgets::page_head(
                                    &theme,
                                    "Appearance",
                                    None,
                                    "Theme, colour, glass and type.",
                                    None,
                                ))
                                .child(
                                    widgets::section(
                                        &theme,
                                        "Color scheme",
                                        widgets::option_card_row().children(cards),
                                    )
                                    .mt(px(24.0))
                                    .gap(px(12.0)),
                                )
                                .child(
                                    widgets::section_card(&theme)
                                        .children(color_rows)
                                        .mt(px(16.0)),
                                )
                                .child(widgets::section(
                                    &theme,
                                    "Material and background",
                                    widgets::section_card(&theme).mt_0().children(settings_rows),
                                ))
                                .child(widgets::section(
                                    &theme,
                                    "Motion",
                                    widgets::section_card(&theme).mt_0().children(motion_rows),
                                ))
                                .child(font_section),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
            .children(modal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // slopty: each font slot's picker marks Geist or Geist Mono and its ladder the default size.
    #[test]
    fn each_font_slot_knows_its_default() {
        assert_eq!(FontKind::Ui.default_family(), UiFontFamily::Geist);
        assert_eq!(FontKind::Code.default_family(), UiFontFamily::GeistMono);
        assert_eq!(FontKind::Terminal.default_family(), UiFontFamily::GeistMono);
        assert_eq!(UiFontSize::ALL[FontKind::Ui.default_size_ix()], UiFontSize::default());
        assert_eq!(
            MONO_FONT_SIZES[FontKind::Code.default_size_ix()],
            typography::CODE_FONT_SIZE_DEFAULT
        );
        assert_eq!(
            MONO_FONT_SIZES[FontKind::Terminal.default_size_ix()],
            typography::TERMINAL_FONT_SIZE_DEFAULT
        );
        assert_eq!(accent_name(AccentSelection::ThemeDefault), "Theme default · Default");
    }

    fn background_settings() -> crate::settings::UiSettings {
        crate::settings::UiSettings {
            new_thread_composer_background: Some(crate::settings::NewThreadComposerBackground {
                path: "background.png".into(),
                name: "Background".into(),
                adjustment: crate::settings::NewThreadBackgroundAdjustment {
                    focal_x: 0.25,
                    focal_y: 0.75,
                    zoom: 2.0,
                },
            }),
            ..Default::default()
        }
    }

    fn ready_background_preview(page: &mut AppearancePage) {
        page.background_adjustment_dialog.as_mut().unwrap().source = Some(Arc::new(
            gpui::RenderImage::new([image::Frame::new(image::RgbaImage::new(4, 4))]),
        ));
    }

    #[gpui::test]
    fn background_adjustment_cancel_and_reset_leave_saved_settings_untouched(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let original = background_settings();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(original.clone(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| AppearancePage::new(cx));
        window
            .update(cx, |page, window, cx| {
                window.focus(&page.font_focus, cx);
                page.open_background_adjustment(window, cx);
                let dialog = page.background_adjustment_dialog.as_mut().unwrap();
                dialog.drag = Some((point(px(0.0), px(0.0)), dialog.draft));
                dialog.pinch = Some((2.0, 1.0));
                page.reset_background_adjustment(cx);
                let dialog = page.background_adjustment_dialog.as_ref().unwrap();
                assert_eq!(dialog.draft, Default::default());
                assert!(dialog.drag.is_none());
                assert!(dialog.pinch.is_none());
                assert_eq!(crate::settings::current(cx), original);
                page.dismiss_background_adjustment(window, cx);
                assert!(page.background_adjustment_dialog.is_none());
                assert!(page.font_focus.is_focused(window));
                assert_eq!(crate::settings::current(cx), original);
                assert!(!crate::settings::UiSettings::path(dir.path()).exists());
            })
            .unwrap();
    }

    #[gpui::test]
    fn background_adjustment_apply_waits_for_preview_and_persists_only_the_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let original = background_settings();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(original.clone(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| AppearancePage::new(cx));
        window
            .update(cx, |page, window, cx| {
                window.focus(&page.font_focus, cx);
                page.open_background_adjustment(window, cx);
                page.apply_background_adjustment(window, cx);
                assert!(page.background_adjustment_dialog.is_some());
                assert_eq!(crate::settings::current(cx), original);
                ready_background_preview(page);
                page.reset_background_adjustment(cx);
                assert_eq!(crate::settings::current(cx), original);
                page.apply_background_adjustment(window, cx);
                assert!(page.background_adjustment_dialog.is_none());
                assert!(page.font_focus.is_focused(window));
                let mut expected = original.clone();
                expected
                    .new_thread_composer_background
                    .as_mut()
                    .unwrap()
                    .adjustment = Default::default();
                assert_eq!(crate::settings::current(cx), expected);
                assert_eq!(crate::settings::UiSettings::load(dir.path()), expected);
            })
            .unwrap();
    }

    #[gpui::test]
    fn background_adjustment_does_not_apply_to_a_replaced_or_removed_image(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(background_settings(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| AppearancePage::new(cx));
        window
            .update(cx, |page, window, cx| {
                page.open_background_adjustment(window, cx);
                ready_background_preview(page);
                page.reset_background_adjustment(cx);
                crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                    settings
                        .new_thread_composer_background
                        .as_mut()
                        .unwrap()
                        .path = "replacement.png".into();
                });
                let replacement = crate::settings::current(cx);
                page.apply_background_adjustment(window, cx);
                assert_eq!(crate::settings::current(cx), replacement);
                assert!(page.background_adjustment_dialog.is_some());
                crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                    settings.new_thread_composer_background = None;
                });
                page.apply_background_adjustment(window, cx);
                assert!(
                    crate::settings::current(cx)
                        .new_thread_composer_background
                        .is_none()
                );
                assert!(page.background_adjustment_dialog.is_some());
            })
            .unwrap();
    }

    #[test]
    fn background_preview_matches_the_runtime_hero_aspect_ratio() {
        for viewport in [
            gpui::size(px(1440.0), px(900.0)),
            gpui::size(px(800.0), px(1200.0)),
        ] {
            for collapsed in [false, true] {
                let preview = background_preview_size(viewport, 260.0, collapsed);
                let hero_width = f32::from(viewport.width) - if collapsed { 0.0 } else { 260.0 };
                let hero_height =
                    crate::shell::new_thread_background_height(f32::from(viewport.height));
                let aspect = f32::from(preview.width) / f32::from(preview.height);
                assert!((aspect - hero_width / hero_height).abs() < 0.00001);
                assert!(f32::from(preview.width) <= 556.0);
                assert!(f32::from(preview.height) <= 260.0);
            }
        }
    }

    #[test]
    fn background_pinch_preserves_native_scale_and_round_trips() {
        for relative_delta in [false, true] {
            let mut previous = 1.0;
            let mut factor = 1.0;
            for scale in [1.5, 2.0, 1.5, 1.0] {
                let delta = if relative_delta {
                    scale / previous - 1.0
                } else {
                    scale - previous
                };
                factor = background_pinch_factor(factor, delta, relative_delta);
                assert!((factor - scale).abs() < 0.00001);
                previous = scale;
            }
            assert!((factor - 1.0).abs() < 0.00001);
        }
    }

    #[test]
    fn background_pinch_clamps_degenerate_scales_and_ignores_invalid_deltas() {
        for relative_delta in [false, true] {
            assert_eq!(background_pinch_factor(1.0, -1.0, relative_delta), 0.001);
            assert_eq!(background_pinch_factor(1.0, -2.0, relative_delta), 0.001);
            for delta in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert_eq!(background_pinch_factor(1.5, delta, relative_delta), 1.5);
            }
        }
    }

    #[test]
    fn background_zoom_slider_matches_its_inset_track_and_clamps() {
        let bounds = Bounds::new(point(px(100.0), px(20.0)), gpui::size(px(168.0), px(28.0)));
        let minimum = crate::settings::NewThreadBackgroundAdjustment::MIN_ZOOM;
        let maximum = crate::settings::NewThreadBackgroundAdjustment::MAX_ZOOM;

        assert_eq!(background_zoom_at_x(bounds, px(90.0)), minimum);
        assert_eq!(background_zoom_at_x(bounds, px(107.0)), minimum);
        assert_eq!(background_zoom_at_x(bounds, px(261.0)), maximum);
        assert_eq!(background_zoom_at_x(bounds, px(280.0)), maximum);
        assert_eq!(background_zoom_at_x(bounds, px(184.0)), 2.5);
        assert_eq!(background_zoom_fraction(minimum), 0.0);
        assert_eq!(background_zoom_fraction(2.5), 0.5);
        assert_eq!(background_zoom_fraction(maximum), 1.0);
    }

    #[test]
    fn every_mode_gets_a_card() {
        assert_eq!(AppearanceMode::ALL.len(), 3);
        for mode in AppearanceMode::ALL {
            assert!(!mode.label().is_empty());
        }
    }

    #[test]
    fn registry_offers_a_curated_few_for_each_appearance() {
        let registry = ThemeRegistry::builtin();
        assert_eq!(
            registry
                .variants_for(slopty_shell_theme::Appearance::Light)
                .count(),
            3
        );
        assert_eq!(
            registry.variants_for(slopty_shell_theme::Appearance::Dark).count(),
            6
        );
    }

    // slopty: each theme row reads the chosen theme's one line, and choosing another theme
    // changes it.
    #[gpui::test]
    fn a_theme_row_says_the_chosen_themes_line(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::settings::init(Default::default(), dir.path(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                ThemeSelection::default(),
                AccentSelection::default(),
                SurfacePreference::Opaque,
                cx,
            );
        });
        let (_page, cx) = cx.add_window_view(|_, cx| AppearancePage::new(cx));
        let voices = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.refresh();
                window.draw(cx).clear(cx);
                let tree = window.a11y_tree().expect("a tree");
                let mut voices = tree
                    .nodes
                    .iter()
                    .filter(|(_, node)| node.role() == gpui::Role::Label)
                    .filter_map(|(_, node)| node.label().map(str::to_owned))
                    .filter(|label| {
                        ThemeRegistry::builtin()
                            .families
                            .iter()
                            .flat_map(|family| &family.variants)
                            .any(|variant| variant.description == *label)
                    })
                    .collect::<Vec<_>>();
                voices.sort();
                voices
            })
        };
        assert_eq!(
            voices(cx),
            ["Paper white under the same violet.", "Soft greys under a violet accent."]
        );
        cx.update(|_, cx| appearance::set_theme(Appearance::Dark, "slopty-black", cx));
        assert_eq!(
            voices(cx),
            ["Paper white under the same violet.", "True black, for OLED screens and late nights."]
        );
    }

    #[test]
    fn accent_helper_names_the_selected_accent() {
        assert_eq!(
            accent_helper(AccentSelection::ThemeDefault),
            "Theme default"
        );
        assert_eq!(
            accent_helper(AccentSelection::Preset(AccentPreset::Pink)),
            "Pink"
        );
    }

    #[test]
    fn surface_helper_distinguishes_inherited_and_explicit_materials() {
        assert_eq!(
            surface_helper(SurfacePreference::ThemeDefault, SurfaceTreatment::Opaque),
            "Theme default: opaque"
        );
        assert_eq!(
            surface_helper(SurfacePreference::ThemeDefault, SurfaceTreatment::Frosted),
            "Theme default: frosted"
        );
        assert_eq!(
            surface_helper(SurfacePreference::Frosted, SurfaceTreatment::Opaque),
            "Translucent surfaces"
        );
        assert_eq!(
            surface_helper(SurfacePreference::Opaque, SurfaceTreatment::Frosted),
            "Solid surfaces"
        );
    }

    #[test]
    fn font_options_appear_once_in_stable_order() {
        let catalog = FontAvailability::all();
        let labels: Vec<_> = catalog.choices().iter().map(UiFontFamily::label).collect();
        assert_eq!(labels.len(), 5);
        assert_eq!(
            labels,
            ["Geist", "Geist Mono", "System UI", "Arial", "Menlo"]
        );
        let unique = labels.into_iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn only_the_terminal_catalog_is_narrowed_to_fixed_width() {
        let all = FontAvailability::all();
        let terminal: Vec<_> = FontKind::Terminal
            .choices_for(&all)
            .iter()
            .map(UiFontFamily::label)
            .collect();
        assert_eq!(terminal, ["Geist Mono", "Menlo"]);

        for kind in [FontKind::Ui, FontKind::Code] {
            assert_eq!(kind.choices_for(&all), all.choices());
            assert!(kind.is_available_for(&all, &UiFontFamily::System));
            assert!(kind.is_available_for(&all, &UiFontFamily::Installed("Arial".into())));
        }
        for proportional in [
            UiFontFamily::System,
            UiFontFamily::Geist,
            UiFontFamily::Installed("Arial".into()),
        ] {
            assert!(!FontKind::Terminal.is_available_for(&all, &proportional));
        }
        assert!(FontKind::Terminal.is_available_for(&all, &UiFontFamily::GeistMono));
        // A query that only matches proportional families leaves the terminal
        // highlight on the bundled mono face rather than on System UI.
        assert_eq!(
            first_available(&[], FontKind::Terminal, &all),
            UiFontFamily::GeistMono
        );
    }

    #[test]
    fn font_keyboard_navigation_stops_at_edges_and_skips_unavailable() {
        let all = FontAvailability::all();
        let choices = all.choices().to_vec();
        assert_eq!(
            step_font(&UiFontFamily::Geist, -1, &choices, FontKind::Ui, &all),
            UiFontFamily::Geist
        );
        assert_eq!(
            step_font(
                &UiFontFamily::Installed("Menlo".into()),
                1,
                &choices,
                FontKind::Ui,
                &all
            ),
            UiFontFamily::Installed("Menlo".into())
        );
        let without_arial = all.without(&UiFontFamily::Installed("Arial".into()));
        assert_eq!(
            step_font(
                &UiFontFamily::System,
                1,
                &choices,
                FontKind::Ui,
                &without_arial
            ),
            UiFontFamily::Installed("Menlo".into())
        );
    }

    #[test]
    fn filtering_narrows_to_matches_and_navigation_stays_inside_them() {
        let all = FontAvailability::all();
        let catalog = all.choices().to_vec();

        assert_eq!(filter_families("", &catalog), catalog);
        assert_eq!(filter_families("   ", &catalog), catalog);
        assert!(filter_families("helvetica", &catalog).is_empty());
        // Case-insensitive, and the bundled entries still lead when they match.
        assert_eq!(
            filter_families("GEIST", &catalog),
            vec![UiFontFamily::Geist, UiFontFamily::GeistMono]
        );

        // "Menlo" prefix-matches, so it outranks the "System UI" substring hit.
        let matches = filter_families("m", &catalog);
        assert_eq!(
            matches,
            vec![
                UiFontFamily::Installed("Menlo".into()),
                UiFontFamily::GeistMono,
                UiFontFamily::System,
            ]
        );
        // Stepping never escapes the filtered list.
        assert_eq!(
            step_font(&UiFontFamily::System, 1, &matches, FontKind::Ui, &all),
            UiFontFamily::System
        );
        assert_eq!(
            first_available(&matches, FontKind::Ui, &all),
            UiFontFamily::Installed("Menlo".into())
        );
        assert_eq!(
            last_available(&matches, FontKind::Ui, &all),
            UiFontFamily::System
        );
    }

    #[test]
    fn each_font_kind_gets_distinct_labels_and_element_ids() {
        let slugs: Vec<_> = FontKind::ALL.iter().map(|kind| kind.slug()).collect();
        let labels: Vec<_> = FontKind::ALL.iter().map(|kind| kind.label()).collect();
        assert_eq!(
            slugs.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
        assert_eq!(
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn pixel_sizes_render_whole_and_fractional_values() {
        assert_eq!(format_px(13.0), "13 px");
        assert_eq!(format_px(typography::CODE_FONT_SIZE_DEFAULT), "12.5 px");
        assert_eq!(
            typography::clamp_font_size(typography::FONT_SIZE_MAX + 1.0),
            typography::FONT_SIZE_MAX
        );
        assert_eq!(
            typography::clamp_font_size(typography::FONT_SIZE_MIN - 1.0),
            typography::FONT_SIZE_MIN
        );
    }

    #[test]
    fn font_size_options_are_ordered_and_include_the_default() {
        let values = UiFontSize::ALL.map(UiFontSize::pixels);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(UiFontSize::ALL.contains(&UiFontSize::default()));
    }

    #[test]
    fn mono_size_ladder_keeps_both_defaults_exactly_reachable() {
        assert!(MONO_FONT_SIZES.windows(2).all(|pair| pair[0] < pair[1]));
        for default in [
            typography::TERMINAL_FONT_SIZE_DEFAULT,
            typography::CODE_FONT_SIZE_DEFAULT,
        ] {
            assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(default)], default);
        }
        // Off-ladder values (older settings, hand-edited files) snap, never drop.
        assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(0.0)], MONO_FONT_SIZES[0]);
        assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(99.0)], 20.0);
        assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(12.4)], 12.5);
    }

    #[test]
    fn every_size_dropdown_labels_its_whole_ladder_in_pixels() {
        for kind in FontKind::ALL {
            let labels = kind.size_labels();
            assert_eq!(labels.len(), kind.size_count());
            assert!(labels.iter().all(|label| label.ends_with(" px")));
        }
        assert!(FontKind::Terminal.size_labels().contains(&"13 px".into()));
        assert!(FontKind::Code.size_labels().contains(&"12.5 px".into()));
    }

    #[gpui::test]
    fn glass_select_switches_opaque_to_frosted(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::settings::init(Default::default(), dir.path(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                ThemeSelection::default(),
                AccentSelection::default(),
                SurfacePreference::Opaque,
                cx,
            );
        });
        let (_page, cx) = cx.add_window_view(|_, cx| AppearancePage::new(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let trigger = cx
            .debug_bounds("appearance-surface")
            .expect("glass trigger");
        cx.simulate_click(trigger.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let frosted = cx
            .debug_bounds("appearance-surface-option-1")
            .expect("frosted option");
        cx.simulate_click(frosted.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(appearance::surface(cx), SurfacePreference::Frosted);
            assert_eq!(Theme::of(cx).surface_preference, SurfacePreference::Frosted);
        });
    }

    // slopty: choosing a preset from the row's menu takes and saves it, and its line follows.
    #[gpui::test]
    fn a_conversation_width_preset_is_chosen_from_its_menu(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(Default::default(), dir.path(), cx);
        });
        let (_page, cx) = cx.add_window_view(|_, cx| AppearancePage::new(cx));
        // Tall enough that the row, low on the page, is on screen.
        cx.simulate_resize(gpui::size(px(1200.0), px(4000.0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let trigger = cx.debug_bounds("conversation-width").expect("the width trigger");
        cx.simulate_click(trigger.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let wide = cx.debug_bounds("conversation-width-option-1").expect("the Wide option");
        cx.simulate_click(wide.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            use crate::settings::ConversationWidth;
            assert_eq!(crate::settings::conversation_width(cx), ConversationWidth::Wide);
            assert_eq!(crate::settings::transcript_width(2000.0, cx), 1200.0);
            crate::settings::flush(cx);
        });
        assert_eq!(
            crate::settings::UiSettings::load(dir.path()).conversation_width,
            crate::settings::ConversationWidth::Wide
        );
    }

    #[gpui::test]
    fn size_dropdowns_commit_from_the_ladder_and_enter_leaves_menus_closed(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            typography::init(
                UiFontFamily::Geist,
                UiFontSize::default(),
                UiFontFamily::GeistMono,
                typography::TERMINAL_FONT_SIZE_DEFAULT,
                UiFontFamily::GeistMono,
                typography::CODE_FONT_SIZE_DEFAULT,
                FontAvailability::all(),
                cx,
            );
        });
        let window = cx.add_window(|_, cx| AppearancePage::new(cx));
        window
            .update(cx, |page, window, cx| {
                for kind in [FontKind::Terminal, FontKind::Code] {
                    let target = kind.size_ix(cx) + 1;
                    page.commit_size(kind, target, window, cx);
                    assert_eq!(kind.pixel_size(cx), MONO_FONT_SIZES[target]);
                    assert_eq!(kind.size_ix(cx), target);
                }

                // Opening a family menu closes a size menu left open elsewhere.
                page.size_select_mut(FontKind::Ui).open(0);
                assert!(page.size_select(FontKind::Ui).is_open());
                page.toggle_font_menu(FontKind::Code, window, cx);
                assert!(!page.size_select(FontKind::Ui).is_open());
                assert!(page.font_menu(FontKind::Code).is_open());

                // Enter commits and reports the key consumed — without that the
                // trigger's `is_open` guard reads the just-closed menu and
                // reopens it on the very same event.
                let enter = KeyDownEvent {
                    keystroke: gpui::Keystroke::parse("enter").expect("valid keystroke"),
                    is_held: false,
                    prefer_character_input: false,
                };
                assert!(page.on_font_key_down(FontKind::Code, &enter, window, cx));
                assert!(!page.font_menu(FontKind::Code).is_open());
            })
            .expect("window is open");
    }

    /// A settings file written before the terminal picker was constrained can
    /// still name a proportional family; startup must not hand it to the grid.
    #[gpui::test]
    fn persisted_proportional_terminal_family_resolves_to_geist_mono(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            typography::init(
                UiFontFamily::Geist,
                UiFontSize::default(),
                UiFontFamily::Installed("Arial".into()),
                typography::TERMINAL_FONT_SIZE_DEFAULT,
                UiFontFamily::Installed("Arial".into()),
                typography::CODE_FONT_SIZE_DEFAULT,
                FontAvailability::all(),
                cx,
            );
            assert_eq!(
                typography::terminal_effective(cx),
                UiFontFamily::GeistMono,
                "proportional terminal family must fall back"
            );
            assert_eq!(
                typography::code_effective(cx),
                UiFontFamily::Installed("Arial".into()),
                "code and diffs keep proportional picks"
            );
            // The setter path rejects the same family too.
            assert!(!typography::set_terminal_family(UiFontFamily::System, cx));
            assert_eq!(typography::terminal_effective(cx), UiFontFamily::GeistMono);
        });
    }
}
