//! The sheet "New project…" opens before the project is made.
//!
//! It asks the project's name, the repository and the branch its work lands on, and the
//! verifier that says a task is done, each filled from the terminal where it can be. Whether a
//! merge is pushed to the forge is set in one place, the board's head.
//!
//! The orchestrator is the agent in the terminal the sheet was opened from, on that
//! terminal's machine; the sheet names both and asks nothing of them. ↵ in any field or
//! Create says it to the workspace; Esc, Cancel or a click outside lets it go.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use slopty_theme::Theme;

use super::view::{VERIFIER, VERIFIER_HINT};
use crate::kit::{self, ButtonKind};

/// What the sheet makes a project of.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NewProject {
    /// Its name, as people read it.
    pub title: String,
    /// The repository its work is in, as the orchestrator's machine names it.
    pub repo: String,
    /// The branch its work lands on.
    pub target: String,
    /// The command that passes when a task's work is right; none when blank.
    pub verifier: Option<String>,
}

/// What the sheet tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetEvent {
    /// Make this project.
    Create(NewProject),
    /// Let the sheet go, making nothing.
    Cancel,
}

/// The title the sheet wears.
pub const TITLE: &str = "New project";

/// The sheet's fields, in the order they stand.
struct Fields {
    name: Entity<InputState>,
    repo: Entity<InputState>,
    target: Entity<InputState>,
    verifier: Entity<InputState>,
}

/// The "New project" sheet.
pub struct ProjectSheet {
    theme: Theme,
    /// Who orchestrates it and where: "Claude Code on studio, in ~/src/board".
    orchestrator: String,
    fields: Fields,
    focus: FocusHandle,
    _enters: [Subscription; 4],
}

impl std::fmt::Debug for ProjectSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectSheet")
            .field("orchestrator", &self.orchestrator)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<SheetEvent> for ProjectSheet {}

impl Focusable for ProjectSheet {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProjectSheet {
    /// A sheet holding `filled`, for the orchestrator `orchestrator` names; the keyboard in
    /// its name.
    pub fn new(
        theme: Theme,
        filled: NewProject,
        orchestrator: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut field = |text: String, hint: &'static str| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(hint));
            input.update(cx, |input, cx| input.set_value(text, window, cx));
            input
        };
        let fields = Fields {
            name: field(filled.title, "What the project is called"),
            repo: field(filled.repo, "The repository's path on the orchestrator's machine"),
            target: field(filled.target, "main"),
            verifier: field(filled.verifier.unwrap_or_default(), VERIFIER_HINT),
        };
        let mut enter = |input: &Entity<InputState>| {
            cx.subscribe(input, |this, _input, event, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.create(cx);
                }
            })
        };
        let enters = [
            enter(&fields.name),
            enter(&fields.repo),
            enter(&fields.target),
            enter(&fields.verifier),
        ];
        fields.name.update(cx, |input, cx| input.focus(window, cx));
        Self { theme, orchestrator, fields, focus: cx.focus_handle(), _enters: enters }
    }

    /// What the sheet holds now, trimmed; a blank verifier is none.
    #[must_use]
    pub fn typed(&self, cx: &gpui::App) -> NewProject {
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let verifier = read(&self.fields.verifier);
        NewProject {
            title: read(&self.fields.name),
            repo: read(&self.fields.repo),
            target: read(&self.fields.target),
            verifier: (!verifier.is_empty()).then_some(verifier),
        }
    }

    fn create(&self, cx: &mut Context<Self>) {
        cx.emit(SheetEvent::Create(self.typed(cx)));
    }
}

impl Render for ProjectSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let sp = theme.spacing;
        // A path, a branch and a command: what is typed is code or close to it.
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let field = |name: &'static str, input: &Entity<InputState>, code: bool| {
            div().flex().flex_col().gap(px(sp.xxs)).child(kit::label(theme, name)).child(
                div()
                    .when(code, |el| el.font_family(mono.clone()))
                    .child(Input::new(input).aria_label(name)),
            )
        };
        let cancel = kit::button(theme, "project-sheet-cancel", "Cancel", ButtonKind::Ghost)
            .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(SheetEvent::Cancel)));
        let create = kit::button(theme, "project-sheet-create", "Create", ButtonKind::Primary)
            .on_click(cx.listener(|this, _ev, _w, cx| this.create(cx)));
        kit::dialog(theme, kit::Overlay::List)
            .id("project-sheet")
            .debug_selector(|| "project-sheet".to_owned())
            .track_focus(&self.focus)
            .role(Role::Dialog)
            .aria_label(TITLE)
            .p(px(sp.lg))
            .gap(px(sp.md))
            .on_action(cx.listener(|_this, _: &Escape, _w, cx| cx.emit(SheetEvent::Cancel)))
            .child(
                div().flex().flex_col().gap(px(sp.xxs)).child(kit::title(theme, TITLE)).child(
                    kit::meta(div(), theme)
                        .debug_selector(|| "project-sheet-orchestrator".to_owned())
                        .child(SharedString::from(self.orchestrator.clone())),
                ),
            )
            .child(field("Name", &self.fields.name, false))
            .child(field("Repository", &self.fields.repo, true))
            .child(field("Target branch", &self.fields.target, true))
            .child(field(VERIFIER, &self.fields.verifier, true))
            .child(
                div().flex().justify_end().gap(px(sp.xs)).pt(px(sp.xs)).child(cancel).child(create),
            )
    }
}
