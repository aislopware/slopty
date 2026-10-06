//! "Remove `<machine>`…": the confirm a machine's removal asks first, from its row's menu or the
//! palette.
//!
//! The sheet says what goes and what stays: Slopty's worker there stops, its services, its own
//! files and its hooks go, and the machine is forgotten here and on the server; the person's
//! repositories, worktrees and agent sessions there stay as they are. It says too when the
//! shells and agents open there end with it, and, for this Mac, that Slopty no longer opens at
//! login. Remove runs the app's removal ([`super::HostActions::remove`]); Cancel, Esc or a click
//! outside lets the sheet go, and nothing is done.

use gpui::accesskit::Role;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use slopty_client::layout::WorkerKey;
use slopty_theme::Theme;

use super::WorkspaceView;
use super::actions::RemoveMachine;
use crate::colors::hsla;
use crate::kit::{self, ButtonKind};

/// A machine row's way to its removal.
pub(super) const REMOVE: &str = "Remove\u{2026}";

/// What the sheet says goes and stays on `machine`.
fn goes(machine: &str) -> String {
    format!(
        "Slopty stops on {machine}, and its services, its own files and its hooks go there. \
         Then {machine} is forgotten here and on the server. Your repositories, worktrees and \
         agent sessions on {machine} stay as they are."
    )
}

/// What the sheet adds for this Mac.
pub(super) const NO_LOGIN: &str = "Slopty will no longer open at login.";

/// What the sheet tells the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveEvent {
    /// Remove the machine.
    Remove,
    /// Let the sheet go, removing nothing.
    Cancel,
}

/// The sheet's words, as the workspace fills them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveWords {
    /// "Remove studio?"
    pub title: String,
    /// What goes and what stays.
    pub body: String,
    /// What else ends or changes: the shells and agents open there, the login item.
    pub also: Vec<String>,
}

impl RemoveWords {
    /// The words for `machine`, with `open` shells and agents there; `here` when it is this
    /// Mac.
    #[must_use]
    pub fn of(machine: &str, open: usize, here: bool) -> Self {
        let ends = match open {
            0 => None,
            1 => Some("Its one open shell or agent ends.".to_owned()),
            n => Some(format!("Its {n} open shells and agents end.")),
        };
        let also = ends.into_iter().chain(here.then(|| NO_LOGIN.to_owned())).collect();
        Self { title: format!("Remove {machine}?"), body: goes(machine), also }
    }
}

/// The confirm.
pub struct RemoveSheet {
    theme: Theme,
    words: RemoveWords,
    focus: FocusHandle,
}

impl std::fmt::Debug for RemoveSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoveSheet").field("words", &self.words).finish_non_exhaustive()
    }
}

impl EventEmitter<RemoveEvent> for RemoveSheet {}

impl Focusable for RemoveSheet {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl RemoveSheet {
    /// A sheet saying `words`, the keyboard on it.
    pub fn new(
        theme: Theme,
        words: RemoveWords,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self { theme, words, focus }
    }

    /// What it says.
    #[cfg(test)]
    #[must_use]
    pub const fn words(&self) -> &RemoveWords {
        &self.words
    }

    fn key_down(_this: &mut Self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            cx.emit(RemoveEvent::Cancel);
            cx.stop_propagation();
        }
    }
}

impl Render for RemoveSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (s, sp) = (theme.surfaces, theme.spacing);
        let cancel = kit::button(theme, "remove-machine-cancel", "Cancel", ButtonKind::Ghost)
            .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(RemoveEvent::Cancel)));
        let remove =
            kit::button(theme, "remove-machine-confirm", "Remove", ButtonKind::Destructive)
                .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(RemoveEvent::Remove)));
        let line = |ix: usize, text: &str| {
            let text = SharedString::from(text.to_owned());
            div()
                .id(("remove-machine-also", ix))
                .debug_selector(move || format!("remove-machine-also-{ix}"))
                .role(Role::Label)
                .aria_label(text.clone())
                .text_color(hsla(s.text))
                .child(text)
        };
        let body = SharedString::from(self.words.body.clone());
        kit::dialog(theme, kit::Overlay::List)
            .id("remove-machine")
            .debug_selector(|| "remove-machine".to_owned())
            .track_focus(&self.focus)
            .role(Role::AlertDialog)
            .aria_label(SharedString::from(self.words.title.clone()))
            .p(px(sp.lg))
            .gap(px(sp.md))
            .on_key_down(cx.listener(Self::key_down))
            .child(kit::title(theme, self.words.title.clone()))
            .child(
                div()
                    .id("remove-machine-body")
                    .debug_selector(|| "remove-machine-body".to_owned())
                    .role(Role::Label)
                    .aria_label(body.clone())
                    .text_color(hsla(s.text_secondary))
                    .child(body),
            )
            .children(self.words.also.iter().enumerate().map(|(ix, text)| line(ix, text)))
            .child(
                div().flex().justify_end().gap(px(sp.xs)).pt(px(sp.xs)).child(cancel).child(remove),
            )
    }
}

/// The open confirm, and the machine it is about.
pub(super) struct Asking {
    view: Entity<RemoveSheet>,
    worker: WorkerKey,
    /// What the dim and the sheet track: Tab stays inside ([`crate::a11y::trap`]).
    scope: FocusHandle,
    _events: Subscription,
}

impl std::fmt::Debug for Asking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asking").field("worker", &self.worker).finish_non_exhaustive()
    }
}

impl WorkspaceView {
    /// "Remove `<machine>`…" from the palette: its confirm.
    pub(super) fn remove_machine(
        &mut self,
        remove: &RemoveMachine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ask_remove_machine(remove.worker, window, cx);
    }

    /// The confirm for removing `key`, over the workspace; nothing when the app offers no
    /// removal of it.
    pub(super) fn ask_remove_machine(
        &mut self,
        key: WorkerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.host_actions(key) else { return };
        if host.remove.is_none() {
            return;
        }
        let here = host.here;
        let Some(w) = self.workers.get(&key) else { return };
        let open = w.sessions.len();
        let words = RemoveWords::of(&w.name, open, here);
        let theme = self.theme.clone();
        let view = cx.new(|cx| RemoveSheet::new(theme, words, window, cx));
        let events =
            cx.subscribe_in(&view, window, move |this, _sheet, event, window, cx| match event {
                RemoveEvent::Remove => this.confirm_remove_machine(window, cx),
                RemoveEvent::Cancel => this.close_remove_machine(cx),
            });
        let scope = cx.focus_handle();
        let home = Focusable::focus_handle(view.read(cx), cx);
        crate::a11y::hold(&scope, &home, cx);
        self.machine_remove = Some(Asking { view, worker: key, scope, _events: events });
        cx.notify();
    }

    /// Remove: the sheet goes, and the app's removal of the machine runs.
    fn confirm_remove_machine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(asking) = self.machine_remove.take() else { return };
        cx.notify();
        let run = self.host_actions(asking.worker).and_then(|h| h.remove.clone());
        if let Some(run) = run {
            run(window, cx);
        }
    }

    /// Let the confirm go, removing nothing.
    pub(super) fn close_remove_machine(&mut self, cx: &mut Context<Self>) {
        if self.machine_remove.take().is_some() {
            cx.notify();
        }
    }

    /// The confirm's words, while it is open.
    #[cfg(test)]
    pub(super) fn remove_machine_words(&self, cx: &gpui::App) -> Option<RemoveWords> {
        Some(self.machine_remove.as_ref()?.view.read(cx).words().clone())
    }

    /// "Remove `<machine>`…" for each machine the app can remove.
    pub(super) fn remove_lines(&self) -> Vec<crate::palette::PaletteItem> {
        self.workers
            .iter()
            .filter(|(key, _)| self.host_actions(**key).is_some_and(|h| h.remove.is_some()))
            .map(|(key, w)| {
                crate::palette::PaletteItem::new(
                    &format!("Remove {}\u{2026}", w.name),
                    Box::new(RemoveMachine { worker: *key }),
                    &[],
                )
            })
            .collect()
    }

    /// The confirm over the workspace, while it is open.
    pub(super) fn render_remove_machine(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let asking = self.machine_remove.as_ref()?;
        let backdrop = kit::backdrop(&self.theme, window).id("remove-machine-backdrop");
        Some(
            crate::a11y::trap(backdrop, &asking.scope)
                .occlude()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.close_remove_machine(cx);
                        cx.stop_propagation();
                    }),
                )
                .child(asking.view.clone())
                .into_any_element(),
        )
    }
}
