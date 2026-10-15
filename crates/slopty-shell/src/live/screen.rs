//! A remote window or desktop as a right-pane surface: first the chat's machine's desktops and
//! windows to choose from, in the surface picker's rows, then the chosen one's live picture,
//! Slopty's own view of it ([`crate::live::screen`]). The pane's takeover makes it the window.

use gpui::{
    Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window, div,
    prelude::*, px,
};
use serde::Deserialize;

use crate::icons::{self, icon};
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

enum Stage {
    Listing,
    Choices(Vec<Choice>),
    Failed(SharedString),
    Opening(SharedString),
    Showing { id: String, title: SharedString },
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
    _watch: Option<Subscription>,
    _task: Option<Task<()>>,
}

impl ScreenSurface {
    /// A surface for the selected chat's machine, listing what it can show.
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let chat = state.read(cx).panel_session_key();
        let target = state.read(cx).terminal_target_device(&chat);
        let mut surface = Self {
            state,
            chat,
            target,
            stage: Stage::Listing,
            chosen: None,
            again: false,
            focus_pending: false,
            _watch: None,
            _task: None,
        };
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
        let chat = state.read(cx).panel_session_key();
        let target = state.read(cx).terminal_target_device(&chat);
        let mut surface = Self {
            state,
            chat,
            target,
            stage: Stage::Listing,
            chosen: None,
            again: false,
            focus_pending: false,
            _watch: None,
            _task: None,
        };
        let choice = Choice { id: screen, title, app: String::new(), kind: String::new() };
        surface.again = true;
        surface.choose(choice, cx);
        surface
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

    fn list(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.stage = Stage::Failed("Not connected".into());
            return;
        };
        let params = self.params(serde_json::json!({}));
        self._task = Some(cx.spawn(async move |this, cx| {
            let listed = engine.client().call_as::<Vec<Choice>>(calls::LIST, params).await;
            let _ = this.update(cx, |this, cx| {
                this.stage = match listed {
                    Ok(choices) if choices.is_empty() => {
                        Stage::Failed("No windows to show".into())
                    }
                    Ok(choices) => Stage::Choices(choices),
                    Err(err) => Stage::Failed(err.to_string().into()),
                };
                cx.notify();
            });
        }));
    }

    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
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
                match opened
                    .ok()
                    .and_then(|value| value.get("id")?.as_str().map(str::to_owned))
                {
                    Some(id) => this.show(id, title, cx),
                    // What the app showed before is gone: what there is to show now is listed.
                    None if std::mem::take(&mut this.again) => {
                        this.chosen = None;
                        this.list(cx);
                    }
                    None => this.stage = Stage::Failed("The window could not be shown".into()),
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

    fn note(&self, text: SharedString, theme: &Theme) -> gpui::AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .text_size(crate::typography::ui_rems(13.0))
            .text_color(theme.text_muted)
            .child(text)
            .into_any_element()
    }

    /// The machine's desktops and windows, as the surface picker's rows.
    fn choices(&self, choices: &[Choice], theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let text = theme.text;
        let muted = theme.text_muted;
        let border = theme.border;
        let border_strong = theme.border_strong;
        let rows = choices.iter().enumerate().map(|(ix, choice)| {
            let glyph = if choice.kind == "display" {
                icons::MONITOR
            } else {
                icons::WINDOW_RESTORE
            };
            let label: SharedString = choice.title.clone().into();
            let app = (!choice.app.is_empty() && choice.app != choice.title)
                .then(|| SharedString::from(choice.app.clone()));
            let picked = choice.clone();
            div()
                .id(("screen-choice", ix))
                .role(gpui::accesskit::Role::Button)
                .aria_label(label.clone())
                .w_full()
                .h(px(44.0))
                .flex_none()
                .px(px(14.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(border)
                .bg(crate::theme::ink(0.02))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .hover(move |s| s.bg(crate::theme::ink(0.05)).border_color(border_strong))
                .on_click(cx.listener(move |this, _, _, cx| this.choose(picked.clone(), cx)))
                .child(icon(glyph).size(px(15.0)).flex_none().text_color(muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(text)
                        .child(label),
                )
                .children(app.map(|app| {
                    div()
                        .flex_none()
                        .max_w(px(120.0))
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(muted)
                        .child(app)
                }))
        });
        div()
            .id("screen-choices")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .p(px(16.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(360.0))
                    .my_auto()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .children(rows),
            )
            .into_any_element()
    }
}

impl Render for ScreenSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        match &self.stage {
            Stage::Listing => self.note("Looking for windows…".into(), &theme),
            Stage::Failed(why) => self.note(why.clone(), &theme),
            Stage::Opening(_) => self.note("Connecting…".into(), &theme),
            Stage::Choices(choices) => {
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
                    None => self.note("Connecting…".into(), &theme),
                }
            }
        }
    }
}

