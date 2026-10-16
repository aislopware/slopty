//! A remote window or desktop as a right-pane surface: first the chat's machine's desktops and
//! windows to choose from, in a searchable picker (zeron's model picker anatomy: a search row,
//! sections, 32 pt rows, ↑ ↓ and ↩), then the chosen one's live picture, Slopty's own view of it
//! ([`crate::live::screen`]). The pane's takeover makes it the window.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Focusable as _, IntoElement, Render, ScrollHandle, SharedString,
    Subscription, Task, Window, div, px,
};
use serde::Deserialize;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::popover;
use crate::state::AppState;
use crate::theme::Theme;

/// What the shell asks of the chat's machine (Slopty's view model answers).
mod calls {
    pub(super) const LIST: &str = "slopty.ListScreens";
    pub(super) const OPEN: &str = "slopty.OpenScreen";
    pub(super) const CLOSE: &str = "slopty.CloseScreen";
}

/// The chip's title before anything is shown.
const UNCHOSEN: &str = "Remote window";

/// The picker's width: the surface picker's column.
const PICKER_WIDTH: f32 = 280.0;

/// The most the picker's list stands before it scrolls.
const LIST_MAX_HEIGHT: f32 = 360.0;

/// What a failure offers.
pub const TRY_AGAIN: &str = "Try again";

/// One desktop or window the machine can show.
#[derive(Clone, Debug, Deserialize)]
struct Choice {
    id: String,
    title: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    kind: String,
}

impl Choice {
    fn display(&self) -> bool {
        self.kind == "display"
    }

    /// The app it belongs to, when that says more than its title.
    fn app(&self) -> Option<&str> {
        (!self.app.is_empty() && self.app != self.title).then_some(self.app.as_str())
    }

    /// What a search matches: its title, then its app.
    fn label(&self) -> String {
        if self.app.is_empty() {
            self.title.clone()
        } else {
            format!("{} {}", self.title, self.app)
        }
    }
}

enum Stage {
    Listing,
    Choices(Vec<Choice>),
    Failed(SharedString),
    Opening(SharedString),
    Showing { id: String, title: SharedString },
}

/// The choices a search keeps, in the order the picker shows them: the displays, then the
/// windows grouped by their app (an app's windows in the order the machine listed them).
fn visible(choices: &[Choice], query: &str) -> (Vec<usize>, Vec<usize>) {
    let labels: Vec<String> = choices.iter().map(Choice::label).collect();
    let mut kept = popover::filter_indices(query, &labels);
    // A search ranks; with none, the machine's own order stands.
    if query.trim().is_empty() {
        kept.sort_unstable();
    }
    let displays: Vec<usize> = kept.iter().copied().filter(|ix| choices[*ix].display()).collect();
    let mut windows: Vec<usize> = kept.into_iter().filter(|ix| !choices[*ix].display()).collect();
    // Each app's windows together, the apps in the order their first window came.
    let first = |app: &str| windows.iter().position(|ix| choices[*ix].app == app);
    let order: Vec<(usize, usize)> = windows
        .iter()
        .enumerate()
        .map(|(at, ix)| (first(&choices[*ix].app).unwrap_or(at), at))
        .collect();
    let mut keyed: Vec<((usize, usize), usize)> = order.into_iter().zip(windows.clone()).collect();
    keyed.sort_by_key(|(key, _)| *key);
    windows = keyed.into_iter().map(|(_, ix)| ix).collect();
    (displays, windows)
}

pub struct ScreenSurface {
    state: Entity<AppState>,
    chat: String,
    target: Option<String>,
    stage: Stage,
    /// What the person chose to show: its id and title, kept to show it again.
    chosen: Option<(String, String)>,
    /// Showing again what the app showed when it last ran: gone, the surface lists instead.
    again: bool,
    focus_pending: bool,
    /// The picker's search.
    search: Entity<ComposerInput>,
    /// What the search says now.
    query: String,
    /// The picker's keyboard row, in the order shown.
    active: Option<usize>,
    scroll: ScrollHandle,
    // slopty: the list's floating rail, as every scrolling list has (M26).
    rail: popover::MenuScrollbarState,
    _search_events: Subscription,
    _watch: Option<Subscription>,
    _task: Option<Task<()>>,
}

impl ScreenSurface {
    /// A surface for the selected chat's machine, listing what it can show.
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut surface = Self::blank(state, cx);
        surface.list(cx);
        surface
    }

    /// A surface showing again `screen`, titled `title`, which the app showed when it last ran.
    pub fn reopen(
        state: Entity<AppState>,
        screen: String,
        title: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut surface = Self::blank(state, cx);
        let choice = Choice { id: screen, title, app: String::new(), kind: String::new() };
        surface.again = true;
        surface.choose(choice, cx);
        surface
    }

    fn blank(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let chat = state.read(cx).panel_session_key();
        let target = state.read(cx).terminal_target_device(&chat);
        let search = cx.new(|cx| {
            ComposerInput::new("Search windows", cx)
                .with_single_line()
                .with_accessibility_role(gpui::Role::SearchInput)
                .with_text_metrics(13.0, 18.0)
        });
        let events = cx.subscribe(&search, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Edited => {
                this.active = None;
                this.sync_search_keys(cx);
                cx.notify();
            }
            ComposerInputEvent::MentionNavigate(delta) => {
                let count = this.shown().len();
                this.active = popover::menu_step(this.active, count, *delta);
                if let Some(at) = this.active {
                    this.scroll.scroll_to_item(at);
                }
                cx.notify();
            }
            ComposerInputEvent::Submitted
            | ComposerInputEvent::ModifiedSubmitted
            | ComposerInputEvent::MentionAccept => this.choose_active(cx),
            ComposerInputEvent::MentionDismiss => {
                this.search.update(cx, |search, cx| search.set_text("", cx));
            }
            _ => {}
        });
        Self {
            state,
            chat,
            target,
            stage: Stage::Listing,
            chosen: None,
            again: false,
            focus_pending: false,
            search,
            query: String::new(),
            active: None,
            scroll: ScrollHandle::new(),
            rail: popover::MenuScrollbarState::default(),
            _search_events: events,
            _watch: None,
            _task: None,
        }
    }

    /// What the person chose to show, its id and title, once they chose.
    pub fn chosen(&self) -> Option<(String, String)> {
        self.chosen.clone()
    }

    fn params(&self, mut extra: serde_json::Value) -> serde_json::Value {
        if let Some(object) = extra.as_object_mut() {
            object.insert("chatId".into(), self.chat.clone().into());
            if let Some(target) = &self.target {
                object.insert("targetDeviceId".into(), target.clone().into());
            }
        }
        extra
    }

    /// Ask the machine again what it can show.
    fn list(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.stage = Stage::Failed("Not connected".into());
            return;
        };
        self.stage = Stage::Listing;
        let params = self.params(serde_json::json!({}));
        self._task = Some(cx.spawn(async move |this, cx| {
            let listed = engine.client().call_as::<Vec<Choice>>(calls::LIST, params).await;
            let _ = this.update(cx, |this, cx| {
                this.stage = match listed {
                    Ok(choices) if choices.is_empty() => Stage::Failed("No windows to show".into()),
                    Ok(choices) => Stage::Choices(choices),
                    Err(err) => Stage::Failed(err.to_string().into()),
                };
                this.active = None;
                this.focus_pending = true;
                this.sync_search_keys(cx);
                cx.notify();
            });
        }));
    }

    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.stage = Stage::Failed("Not connected".into());
            return;
        };
        let title: SharedString = choice.title.clone().into();
        self.chosen = Some((choice.id.clone(), choice.title.clone()));
        self.stage = Stage::Opening(title.clone());
        let params = self.params(serde_json::json!({
            "screen": choice.id,
            "title": choice.title,
        }));
        self._task = Some(cx.spawn(async move |this, cx| {
            let opened = engine.client().call(calls::OPEN, params).await;
            let _ = this.update(cx, |this, cx| {
                let opened =
                    opened.map_err(|err| SharedString::from(err.to_string())).and_then(|value| {
                        value
                            .get("id")
                            .and_then(|id| id.as_str().map(str::to_owned))
                            .ok_or_else(|| SharedString::from("The window could not be shown"))
                    });
                match opened {
                    Ok(id) => this.show(id, title, cx),
                    // What the app showed before is gone: what there is to show now is listed.
                    Err(_) if std::mem::take(&mut this.again) => {
                        this.chosen = None;
                        this.list(cx);
                    }
                    // The machine's own words for why, as the listing says them.
                    Err(why) => {
                        this.chosen = None;
                        this.stage = Stage::Failed(why);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn show(&mut self, id: String, title: SharedString, cx: &mut Context<Self>) {
        let surface = cx.weak_entity();
        self._watch = super::watch(
            &id,
            Box::new(move |cx| {
                let _ = surface.update(cx, |_, cx| cx.notify());
            }),
            cx,
        );
        self.stage = Stage::Showing { id, title };
        self.focus_pending = true;
    }

    /// The chip's title: the window's, once one is shown.
    pub fn title(&self) -> SharedString {
        match &self.stage {
            Stage::Showing { title, .. } | Stage::Opening(title) => title.clone(),
            _ => UNCHOSEN.into(),
        }
    }

    /// Take the keyboard once drawn.
    pub fn focus(&mut self, cx: &mut Context<Self>) {
        self.focus_pending = true;
        cx.notify();
    }

    /// The tab closed: the machine stops showing it.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        let Stage::Showing { id, .. } = &self.stage else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({ "id": id });
        cx.background_spawn(async move {
            let _ = engine.client().call(calls::CLOSE, params).await;
        })
        .detach();
    }

    /// The choices shown now, in order, as indices into the listing.
    fn shown(&self) -> Vec<usize> {
        let Stage::Choices(choices) = &self.stage else { return Vec::new() };
        let (displays, windows) = visible(choices, &self.query);
        displays.into_iter().chain(windows).collect()
    }

    /// ↑ ↓ reach the rows from the search while there are rows.
    fn sync_search_keys(&mut self, cx: &mut Context<Self>) {
        self.query = self.search.read(cx).text().to_owned();
        let rows = !self.shown().is_empty();
        self.search.update(cx, |search, cx| search.set_mention_controls(rows, rows, cx));
    }

    fn choose_active(&mut self, cx: &mut Context<Self>) {
        let shown = self.shown();
        let at = self.active.unwrap_or(0);
        let Stage::Choices(choices) = &self.stage else { return };
        if let Some(choice) = shown.get(at).and_then(|ix| choices.get(*ix)).cloned() {
            self.choose(choice, cx);
            cx.notify();
        }
    }
}

impl ScreenSurface {
    /// A failure in the machine's own words, and the way to ask again.
    fn failed(&self, why: &SharedString, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let again =
            super::chrome::button("screen-try-again", TRY_AGAIN, super::chrome::Tone::Ghost, theme)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.list(cx);
                    cx.notify();
                }));
        div()
            .id("screen-failed")
            .debug_selector(|| "screen-failed".to_owned())
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(PICKER_WIDTH))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id("screen-failed-why")
                            .role(gpui::Role::Status)
                            .aria_label(why.clone())
                            .child(popover::error_row(theme, why)),
                    )
                    .child(again),
            )
            .into_any_element()
    }

    /// The machine's desktops and windows as zeron's picker: the search, then "Displays" and
    /// "Windows" (each app's together, its name trailing), the keyboard's row washed.
    fn choices(&mut self, choices: &[Choice], theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme.for_popup();
        let (displays, windows) = visible(choices, &self.query);
        let shown = displays.len() + windows.len();
        let mut list = div()
            .id("screen-choices-list")
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.rail.set_list_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .max_h(px(LIST_MAX_HEIGHT))
            .flex()
            .flex_col()
            .gap(px(popover::MENU_GAP));
        let mut at = 0;
        for (heading, section) in [("Displays", &displays), ("Windows", &windows)] {
            if section.is_empty() {
                continue;
            }
            list = list.child(popover::menu_heading(&theme, heading));
            for &ix in section.iter() {
                let choice = &choices[ix];
                list = list.child(self.choice_row(choice, at, &theme, cx));
                at += 1;
            }
        }
        if shown == 0 {
            list = list.child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child("No matching windows"),
            );
        }
        let refresh =
            super::chrome::icon_button("screen-refresh", icons::REFRESH, "Refresh windows", &theme)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.list(cx);
                    cx.notify();
                }));
        let search = self.search.clone();
        let search_row = popover::search_row(&theme)
            .child(div().flex_1().min_w_0().child(search))
            .child(refresh);
        let card = popover::popover_card(&theme)
            .id("screen-choices")
            .debug_selector(|| "screen-choices".to_owned())
            .w_full()
            .max_w(px(PICKER_WIDTH))
            .flex()
            .flex_col()
            .child(search_row)
            .child(
                div()
                    .relative()
                    .child(popover::faded_menu_list(&self.scroll, list))
                    .children(popover::rail(self, "screen-choices-rail", &theme, cx)),
            );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .child(card)
            .into_any_element()
    }

    /// One choice's row: its kind's glyph, its title, its app.
    fn choice_row(
        &self,
        choice: &Choice,
        at: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let glyph = if choice.display() { icons::MONITOR } else { icons::WINDOW_RESTORE };
        let label: SharedString = choice.title.clone().into();
        let app = choice.app().map(|app| SharedString::from(app.to_owned()));
        let picked = choice.clone();
        let key = format!("screen-choice-{}", choice.id);
        popover::picker_row(theme, false, self.active == Some(at), key.clone())
            .id(("screen-choice", at))
            .debug_selector(move || key)
            .role(gpui::Role::Button)
            .aria_label(label.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.choose(picked.clone(), cx);
                cx.notify();
            }))
            .child(popover::picker_icon_slot(
                icon(glyph).size(px(14.0)).text_color(theme.text_muted).into_any_element(),
            ))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children(app.map(|app| {
                div()
                    .flex_none()
                    .max_w(px(110.0))
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(app)
            }))
            .into_any_element()
    }

    /// While the machine answers: zeron's loading anatomy, the same as the picture's own wait,
    /// so the place never changes size or tone.
    fn loading(&self, caption: &'static str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity_id();
        div()
            .id("screen-loading")
            .debug_selector(|| "screen-loading".to_owned())
            .role(gpui::Role::Status)
            .aria_label(caption)
            .size_full()
            .child(super::chrome::loading(caption, theme, view, cx))
            .into_any_element()
    }
}

impl popover::ScrollRailHost for ScreenSurface {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        &mut self.rail
    }

    fn rail_scroll(&self) -> Option<ScrollHandle> {
        matches!(self.stage, Stage::Choices(_)).then(|| self.scroll.clone())
    }
}

impl Render for ScreenSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        match &self.stage {
            Stage::Listing => self.loading("Looking for windows…", &theme, cx),
            Stage::Failed(why) => {
                let why = why.clone();
                self.failed(&why, &theme, cx)
            }
            Stage::Opening(_) => self.loading("Connecting…", &theme, cx),
            Stage::Choices(choices) => {
                if std::mem::take(&mut self.focus_pending) {
                    let focus = self.search.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
                let choices = choices.clone();
                self.choices(&choices, &theme, cx)
            }
            Stage::Showing { id, .. } => {
                let id = id.clone();
                match super::screen(&id, cx) {
                    Some(hosted) => {
                        if std::mem::take(&mut self.focus_pending) {
                            window.focus(&hosted.focus, cx);
                        }
                        div().size_full().child(hosted.view).into_any_element()
                    }
                    None => self.loading("Connecting…", &theme, cx),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn choice(id: &str, title: &str, app: &str, kind: &str) -> Choice {
        Choice { id: id.into(), title: title.into(), app: app.into(), kind: kind.into() }
    }

    fn listing() -> Vec<Choice> {
        vec![
            choice("w1", "notes.txt", "TextEdit", "window"),
            choice("d1", "Built-in Display", "", "display"),
            choice("w2", "Inbox", "Mail", "window"),
            choice("w3", "todo.txt", "TextEdit", "window"),
        ]
    }

    /// Displays come first, then the windows, each app's together in the machine's order; a
    /// search keeps only what it matches, by title or app.
    #[test]
    fn the_picker_shows_displays_then_each_app_s_windows() {
        let choices = listing();
        assert_eq!(visible(&choices, ""), (vec![1], vec![0, 3, 2]));
        assert_eq!(visible(&choices, "mail"), (vec![], vec![2]));
        assert_eq!(visible(&choices, "txt"), (vec![], vec![0, 3]));
        assert_eq!(visible(&choices, "zzz"), (vec![], vec![]));
    }

    fn surface(cx: &mut TestAppContext) -> (Entity<ScreenSurface>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let (surface, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            ScreenSurface::new(state, cx)
        });
        (surface, cx)
    }

    /// With no machine to ask, the surface says why and offers to ask again, which asks again.
    #[gpui::test]
    fn a_failure_says_why_and_offers_to_try_again(cx: &mut TestAppContext) {
        let (surface, cx) = surface(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("screen-failed").is_some(), "Not connected");
        let again = cx.debug_bounds("screen-try-again").expect("Try again");
        assert!(again.size.height >= px(24.0), "a dialog button: {again:?}");
        cx.simulate_click(again.center(), gpui::Modifiers::default());
        surface.read_with(cx, |s, _| assert!(matches!(s.stage, Stage::Failed(_)), "still none"));
    }

    /// Typing in the picker's search drops the rows it no longer matches.
    #[gpui::test]
    fn typing_in_the_search_narrows_the_rows(cx: &mut TestAppContext) {
        let (surface, cx) = surface(cx);
        surface.update(cx, |s, cx| {
            s.stage = Stage::Choices(listing());
            s.sync_search_keys(cx);
            cx.notify();
        });
        let rows = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            ["screen-choice-w1", "screen-choice-d1", "screen-choice-w2", "screen-choice-w3"]
                .into_iter()
                .filter(|id| cx.debug_bounds(id).is_some())
                .count()
        };
        assert_eq!(rows(cx), 4);
        let row = cx.debug_bounds("screen-choice-w2").expect("a row");
        assert_eq!(row.size.height, px(popover::PICKER_ROW_HEIGHT), "the picker's row");
        let search = surface.read_with(cx, |s, cx| s.search.read(cx).focus_handle(cx));
        cx.update(|window, cx| window.focus(&search, cx));
        cx.simulate_input("mail");
        assert_eq!(rows(cx), 1, "only Mail's window");
        surface.read_with(cx, |s, _| assert_eq!(s.shown(), [2]));
    }
}
