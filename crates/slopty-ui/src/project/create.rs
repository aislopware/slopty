//! The sheet "New goal…" opens: one goal handed over, and a project made around the agent that
//! takes it (the orchestrator-first study, item 9).
//!
//! The person writes the goal; the folder it is worked in comes filled from the focus, else the
//! last start, else the latest repository work stood in, and the agent and the machine from the
//! last start, said in a line under the folder. The other recent repositories wait under the
//! folder as chips, and a press of one moves the folder, and the machine, there. "More" opens what
//! is rarely changed: which agent on which machine, the branch the work lands on, the command
//! that says a task's work is right, and how far the project's agents go before they ask. The
//! project's name is drawn from the goal. ↵ in the goal (⇧↵ for a new line) or in any field, or
//! Create, says it to the workspace; Esc, Cancel or a click outside lets it go.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_client::layout::WorkerKey;
use slopty_kit::a11y::tab_stop;
use slopty_kit::colors::hsla;
use slopty_kit::icons::{Symbol, icon};
use slopty_kit::kit::{self, ButtonKind};
use slopty_proto::project::Autonomy;
use slopty_proto::thread::AgentId;
use slopty_theme::Theme;

use super::view::{AUTONOMY, VERIFIER, VERIFIER_HINT};

/// What the sheet makes: the goal, and the orchestrator that takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewGoal {
    /// The goal, as the person wrote it: the orchestrator's first message.
    pub goal: String,
    /// The folder the orchestrator starts in, as its machine spells it (`~` its home).
    pub folder: String,
    /// The machine it runs on.
    pub worker: WorkerKey,
    /// Which agent orchestrates.
    pub agent: AgentId,
    /// The branch finished work lands on; blank for a branch of the project's own, which the
    /// server makes off what the folder has checked out.
    pub target: String,
    /// The command that passes when a task's work is right; none when blank.
    pub verifier: Option<String>,
    /// How far the project's agents go before they ask the person.
    pub autonomy: Autonomy,
}

/// An agent that can orchestrate, and the machines that can start it, each with its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starter {
    /// The agent.
    pub agent: AgentId,
    /// Its name, as people read it.
    pub label: String,
    /// The machines that can start it, the one to start on first.
    pub machines: Vec<(WorkerKey, String)>,
}

/// A repository work stood in lately, which the sheet offers as a chip under the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The machine it is on.
    pub worker: WorkerKey,
    /// Its path there, as the folder field takes it.
    pub folder: String,
    /// What it is called: the repository and the path within it.
    pub name: String,
}

/// What the sheet opens holding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filled {
    /// The folder, from the focus, else the last start, else the latest of [`Self::places`].
    pub folder: String,
    /// The repositories work stood in lately, the latest first, on machines that can start
    /// one of [`Self::starters`].
    pub places: Vec<Place>,
    /// The verifier guessed from the repository's own scripts, when one reads as a check.
    pub verifier: Option<String>,
    /// The agents that can orchestrate, the last started first.
    pub starters: Vec<Starter>,
}

/// Where a goal would start, as the sheet asks its machine about it: the machine, the folder
/// and the target branch (blank for one of the goal's own).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalPlace {
    /// The machine the folder is on.
    pub worker: WorkerKey,
    /// The folder, trimmed.
    pub folder: String,
    /// The target branch, trimmed; blank for a branch of the goal's own.
    pub target: String,
}

/// What the sheet tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetEvent {
    /// Start this goal.
    Create(NewGoal),
    /// Ask the place's machine whether a goal can start there (`Verb::GoalCheck`); the
    /// answer comes back through [`GoalSheet::place_checked`].
    Check(GoalPlace),
    /// Let the sheet go, making nothing.
    Cancel,
}

/// The title the sheet wears.
pub const TITLE: &str = "New goal";

/// What the goal field says while empty.
pub const GOAL_HINT: &str = "What should get done";

/// The goal field's name.
pub const GOAL: &str = "Goal";

/// The folder field's name.
pub const FOLDER: &str = "Folder";

/// The target field's name.
pub const TARGET: &str = "Target branch";

/// What the target field says while blank: the goal's work lands on a branch of its own.
pub const TARGET_HINT: &str = "Blank: a branch of its own, off the checkout";

/// The disclosure that opens the rarely changed settings.
pub const MORE: &str = "More";

/// The most recent repositories the sheet offers under the folder.
pub const PLACES: usize = 4;

/// The most lines the goal field grows to before it scrolls.
pub(crate) const GOAL_ROWS: usize = 8;

/// The sheet's fields.
struct Fields {
    goal: Entity<TextareaState>,
    folder: Entity<InputState>,
    target: Entity<InputState>,
    verifier: Entity<InputState>,
}

/// The "New goal" sheet.
pub struct GoalSheet {
    theme: Theme,
    fields: Fields,
    starters: Vec<Starter>,
    places: Vec<Place>,
    /// Which of [`Self::starters`] starts.
    agent: usize,
    /// On which machine.
    machine: Option<WorkerKey>,
    autonomy: Autonomy,
    /// "More" is open.
    more: bool,
    /// What the place's machine said of it: `Ok` where a goal can start, else why not.
    checked: Option<(GoalPlace, Result<(), String>)>,
    focus: FocusHandle,
    _enters: [Subscription; 6],
}

impl std::fmt::Debug for GoalSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalSheet").field("agent", &self.agent).finish_non_exhaustive()
    }
}

impl EventEmitter<SheetEvent> for GoalSheet {}

impl Focusable for GoalSheet {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GoalSheet {
    /// A sheet holding `filled`; the keyboard in the goal.
    pub fn new(theme: Theme, filled: Filled, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let goal = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(GOAL_HINT)
                .auto_grow(3, GOAL_ROWS)
                .submit_on_enter(true)
        });
        let mut field = |text: String, hint: String| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(hint));
            input.update(cx, |input, cx| input.set_value(text, window, cx));
            input
        };
        let fields = Fields {
            goal,
            folder: field(filled.folder, "The folder the orchestrator starts in".to_owned()),
            target: field(String::new(), TARGET_HINT.to_owned()),
            verifier: field(filled.verifier.unwrap_or_default(), VERIFIER_HINT.to_owned()),
        };
        let goal_enter = cx.subscribe(&fields.goal, |this, _input, event, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.create(cx);
            }
        });
        let mut moved = |input: &Entity<InputState>| {
            cx.subscribe(input, |this, _input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.ask_check(cx);
                }
            })
        };
        let (folder_moved, target_moved) = (moved(&fields.folder), moved(&fields.target));
        let mut enter = |input: &Entity<InputState>| {
            cx.subscribe(input, |this, _input, event, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.create(cx);
                }
            })
        };
        let enters = [
            goal_enter,
            enter(&fields.folder),
            enter(&fields.target),
            enter(&fields.verifier),
            folder_moved,
            target_moved,
        ];
        fields.goal.update(cx, |input, cx| input.focus(window, cx));
        let machine = filled.starters.first().and_then(|s| s.machines.first()).map(|(k, _)| *k);
        Self {
            theme,
            fields,
            starters: filled.starters,
            places: filled.places,
            agent: 0,
            machine,
            autonomy: Autonomy::default(),
            more: false,
            checked: None,
            focus: cx.focus_handle(),
            _enters: enters,
        }
    }

    /// Where the sheet would start the goal now: its machine, its folder and its target, each
    /// trimmed; none while it names no machine or no folder.
    #[must_use]
    pub fn place(&self, cx: &gpui::App) -> Option<GoalPlace> {
        let folder = self.fields.folder.read(cx).value().trim().to_owned();
        let target = self.fields.target.read(cx).value().trim().to_owned();
        let worker = self.machine?;
        (!folder.is_empty()).then_some(GoalPlace { worker, folder, target })
    }

    /// Ask the workspace whether a goal can start where the sheet holds now.
    fn ask_check(&self, cx: &mut Context<Self>) {
        if let Some(place) = self.place(cx) {
            cx.emit(SheetEvent::Check(place));
        }
        cx.notify();
    }

    /// What the machine said of `place`: kept while the sheet still holds that place.
    pub fn place_checked(
        &mut self,
        place: &GoalPlace,
        said: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if self.place(cx).as_ref() == Some(place) {
            self.checked = Some((place.clone(), said));
            cx.notify();
        }
    }

    /// What is known of the place held now: `Ok` where a goal can start, else why not; none
    /// until its machine has said.
    #[must_use]
    pub fn place_is(&self, cx: &gpui::App) -> Option<Result<(), String>> {
        let place = self.place(cx)?;
        self.checked.as_ref().filter(|(p, _)| *p == place).map(|(_, said)| said.clone())
    }

    /// Put `words` in the goal, as written elsewhere before the sheet opened.
    pub fn set_goal(&self, words: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.fields.goal.update(cx, |input, cx| input.set_value(words.to_owned(), window, cx));
    }

    /// Put `folder` in the folder field, as the person types it, and ask what it is.
    #[cfg(test)]
    pub fn set_folder(&self, folder: &str, window: &mut Window, cx: &mut Context<Self>) {
        let folder = folder.to_owned();
        self.fields.folder.update(cx, |input, cx| input.set_value(folder, window, cx));
        self.ask_check(cx);
    }

    /// The agent that starts, and its machines.
    fn starter(&self) -> Option<&Starter> {
        self.starters.get(self.agent)
    }

    /// What the sheet holds now, trimmed; none while it names no machine to start on.
    #[must_use]
    pub fn typed(&self, cx: &gpui::App) -> Option<NewGoal> {
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let starter = self.starter()?;
        let verifier = read(&self.fields.verifier);
        Some(NewGoal {
            goal: self.fields.goal.read(cx).value().trim().to_owned(),
            folder: read(&self.fields.folder),
            worker: self.machine?,
            agent: starter.agent.clone(),
            target: read(&self.fields.target),
            verifier: (!verifier.is_empty()).then_some(verifier),
            autonomy: self.autonomy,
        })
    }

    /// Create, once the goal is written and its place's machine said a goal can start there.
    fn create(&self, cx: &mut Context<Self>) {
        if self.place_is(cx) != Some(Ok(())) {
            return;
        }
        if let Some(goal) = self.typed(cx) {
            cx.emit(SheetEvent::Create(goal));
        }
    }

    /// Start `agent`, the `ix`th, on the machine already picked where it can, else its first.
    fn pick_agent(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(starter) = self.starters.get(ix) else { return };
        let kept = self.machine.filter(|m| starter.machines.iter().any(|(k, _)| k == m));
        self.machine = kept.or_else(|| starter.machines.first().map(|(k, _)| *k));
        self.agent = ix;
        self.ask_check(cx);
    }

    /// Move the folder to `place`, and the machine with it: the agent stays where it can start
    /// there, else the first that can takes it.
    fn pick_place(&mut self, place: &Place, window: &mut Window, cx: &mut Context<Self>) {
        let on = |s: &Starter| s.machines.iter().any(|(k, _)| *k == place.worker);
        let agent = if self.starter().is_some_and(on) {
            Some(self.agent)
        } else {
            self.starters.iter().position(on)
        };
        let Some(agent) = agent else { return };
        self.agent = agent;
        self.machine = Some(place.worker);
        let folder = place.folder.clone();
        self.fields.folder.update(cx, |input, cx| input.set_value(folder, window, cx));
        self.ask_check(cx);
    }

    /// The recent repositories as chips under the folder, but for the one it holds: a press
    /// moves the folder there. A chip names its machine while the places span more than one.
    fn places_row(&self, cx: &Context<Self>) -> Option<gpui::Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let sp = theme.spacing;
        let held = self.fields.folder.read(cx).value().trim().to_owned();
        let spans =
            self.places.iter().any(|p| Some(p.worker) != self.places.first().map(|f| f.worker));
        let shown: Vec<&Place> = self
            .places
            .iter()
            .filter(|p| !(p.folder == held && Some(p.worker) == self.machine))
            .collect();
        if shown.is_empty() {
            return None;
        }
        let chips = shown.into_iter().enumerate().map(|(ix, place)| {
            let machine = self
                .starters
                .iter()
                .flat_map(|st| &st.machines)
                .find(|(k, _)| spans && *k == place.worker);
            let words = machine.map_or_else(
                || place.name.clone(),
                |(_, name)| format!("{} \u{b7} {name}", place.name),
            );
            let picked = place.clone();
            let el = kit::typed(div(), theme.roles().metadata)
                .id(("goal-sheet-place", ix))
                .debug_selector(move || format!("goal-sheet-place-{ix}"))
                .role(Role::Button)
                .aria_label(SharedString::from(words.clone()))
                .aria_description(SharedString::from(place.folder.clone()))
                .flex_none()
                .max_w_full()
                .flex()
                .items_center()
                .gap(px(sp.xxs))
                .h(px(theme.density.chip))
                .px(px(sp.xs))
                .rounded(px(theme.radii.sm))
                .border(kit::HAIR)
                .border_color(hsla(s.border))
                .cursor_pointer()
                .overflow_hidden()
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                .child(icon(theme, Symbol::Folder, hsla(s.text_secondary)))
                .child(div().min_w_0().truncate().child(SharedString::from(words)));
            tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, window, cx| {
                this.pick_place(&picked, window, cx);
            }))
        });
        Some(
            div()
                .debug_selector(|| "goal-sheet-places".to_owned())
                .flex()
                .flex_wrap()
                .gap(px(sp.xxs))
                .children(chips),
        )
    }

    /// Why a goal cannot start where the sheet holds, in the machine's words, under the
    /// folder: no such folder, not one, not in a git repository, a bare one, no commit yet, or
    /// no such branch. Create waits for a place it can.
    fn refusal(&self, cx: &Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let Some(Err(why)) = self.place_is(cx) else { return None };
        let why = SharedString::from(why);
        Some(
            kit::meta(div(), &self.theme)
                .id("goal-sheet-refusal")
                .debug_selector(|| "goal-sheet-refusal".to_owned())
                .role(Role::Status)
                .aria_label(why.clone())
                .text_color(hsla(self.theme.surfaces.warn))
                .child(why),
        )
    }

    /// The line under the folder: which agent starts, on which machine.
    fn who(&self) -> String {
        let Some(starter) = self.starter() else { return String::new() };
        let machine = starter
            .machines
            .iter()
            .find(|(k, _)| Some(*k) == self.machine)
            .map_or("", |(_, name)| name.as_str());
        format!("{} on {machine} orchestrates it", starter.label)
    }

    /// One segmented choice, its `id` and `label`: `options`' words in a ringed track, the
    /// `chosen` one on the selected wash; a press of another runs `pick` with its place.
    fn choice(
        &self,
        (id, label): (&'static str, &'static str),
        options: Vec<String>,
        chosen: usize,
        pick: fn(&mut Self, usize, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let segments = options.into_iter().enumerate().map(|(ix, word)| {
            let on = ix == chosen;
            let el = kit::typed(div(), theme.roles().metadata)
                .id((id, ix))
                .debug_selector(move || format!("{id}-{ix}"))
                .role(Role::RadioButton)
                .aria_label(SharedString::from(word.clone()))
                .aria_selected(on)
                .flex_none()
                .flex()
                .items_center()
                .h(px(theme.density.chip))
                .px(px(theme.spacing.sm))
                .rounded(px(kit::thumb_radius(theme)))
                .cursor_pointer()
                .child(word)
                .when(on, |el| el.bg(hsla(s.selected)).text_color(hsla(s.text)))
                .when(!on, |el| {
                    el.text_color(hsla(s.text_secondary))
                        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                });
            tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _w, cx| pick(this, ix, cx)))
        });
        kit::track(theme)
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::RadioGroup)
            .aria_label(label)
            .flex_none()
            .children(segments)
    }

    /// What "More" opens: the agent and its machine where there is a choice, the target, the
    /// verifier and the autonomy.
    fn more_fields(&self, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let code = |input: &Entity<InputState>, name: &'static str| {
            div().font_family(mono.clone()).child(Input::new(input).aria_label(name))
        };
        let agents = (self.starters.len() > 1).then(|| {
            let words = self.starters.iter().map(|s| s.label.clone()).collect();
            let pick: fn(&mut Self, usize, &mut Context<Self>) = Self::pick_agent;
            let el = self.choice(("goal-sheet-agent", "Agent"), words, self.agent, pick, cx);
            field(theme, "Agent", el)
        });
        let machines = self.starter().filter(|s| s.machines.len() > 1).map(|starter| {
            let words = starter.machines.iter().map(|(_, name)| name.clone()).collect();
            let at = starter.machines.iter().position(|(k, _)| Some(*k) == self.machine);
            let pick: fn(&mut Self, usize, &mut Context<Self>) = |this, ix, cx| {
                this.machine = this.starter().and_then(|s| s.machines.get(ix)).map(|(k, _)| *k);
                this.ask_check(cx);
            };
            let el =
                self.choice(("goal-sheet-machine", "Machine"), words, at.unwrap_or(0), pick, cx);
            field(theme, "Machine", el)
        });
        let words = LEVELS.iter().map(|l| level_word(*l).to_owned()).collect();
        let at = LEVELS.iter().position(|l| *l == self.autonomy).unwrap_or(0);
        let pick: fn(&mut Self, usize, &mut Context<Self>) = |this, ix, cx| {
            if let Some(level) = LEVELS.get(ix) {
                this.autonomy = *level;
                cx.notify();
            }
        };
        let autonomy = self.choice(("goal-sheet-autonomy", AUTONOMY), words, at, pick, cx);
        div()
            .id("goal-sheet-more-fields")
            .debug_selector(|| "goal-sheet-more-fields".to_owned())
            .flex()
            .flex_col()
            .gap(px(theme.spacing.md))
            .children(agents)
            .children(machines)
            .child(field(theme, TARGET, code(&self.fields.target, TARGET)))
            .child(field(theme, VERIFIER, code(&self.fields.verifier, VERIFIER)))
            .child(field(theme, AUTONOMY, autonomy))
    }
}

/// The autonomy levels, in the order the sheet offers them.
const LEVELS: [Autonomy; 3] = [Autonomy::Ask, Autonomy::Edits, Autonomy::Own];

/// A level's word on the sheet.
const fn level_word(level: Autonomy) -> &'static str {
    match level {
        Autonomy::Ask => "Ask",
        Autonomy::Edits => "Edits",
        Autonomy::Own => "Own",
    }
}

/// A field under its name.
fn field(theme: &Theme, name: &'static str, el: impl IntoElement) -> gpui::Div {
    div().flex().flex_col().gap(px(theme.spacing.xxs)).child(kit::label(theme, name)).child(el)
}

impl Render for GoalSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let sp = theme.spacing;
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let goal = Textarea::new(&self.fields.goal).with_size(Size::Small).aria_label(GOAL);
        let folder =
            div().font_family(mono).child(Input::new(&self.fields.folder).aria_label(FOLDER));
        let open = self.more;
        let chevron = if open { Symbol::ChevronDown } else { Symbol::ChevronRight };
        let more = div()
            .id("goal-sheet-more")
            .debug_selector(|| "goal-sheet-more".to_owned())
            .role(Role::Button)
            .aria_label(MORE)
            .aria_expanded(open)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(sp.xxs))
            .h(px(theme.density.chip))
            .px(px(sp.xs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .child(MORE)
            .child(icon(theme, chevron, hsla(s.text_secondary)));
        let more = tab_stop(more, s.focus).on_click(cx.listener(|this, _ev, _w, cx| {
            this.more = !this.more;
            cx.notify();
        }));
        let who = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(sp.sm))
            .child(
                kit::meta(div(), theme)
                    .debug_selector(|| "goal-sheet-who".to_owned())
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(self.who())),
            )
            .child(more);
        let cancel = kit::button(theme, "goal-sheet-cancel", "Cancel", ButtonKind::Ghost)
            .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(SheetEvent::Cancel)));
        let ready = self.place_is(cx) == Some(Ok(()));
        let create = kit::button(theme, "goal-sheet-create", "Create", ButtonKind::Primary)
            .when(!ready, |el| el.opacity(slopty_theme::alpha::PRESSED).cursor_default())
            .on_click(cx.listener(|this, _ev, _w, cx| this.create(cx)));
        kit::dialog(theme, kit::Overlay::Editor)
            .id("goal-sheet")
            .debug_selector(|| "goal-sheet".to_owned())
            .track_focus(&self.focus)
            .role(Role::Dialog)
            .aria_label(TITLE)
            .on_action(cx.listener(|_this, _: &Escape, _w, cx| cx.emit(SheetEvent::Cancel)))
            .child(kit::dialog_head(theme).child(kit::title(theme, TITLE)))
            .child(
                kit::dialog_section(theme)
                    .gap(px(sp.md))
                    .pb(px(sp.md))
                    .child(field(theme, GOAL, goal))
                    .child(
                        field(theme, FOLDER, folder)
                            .children(self.refusal(cx))
                            .children(self.places_row(cx)),
                    )
                    .child(who)
                    .when(open, |el| el.child(self.more_fields(cx))),
            )
            .child(kit::dialog_foot(theme).child(cancel).child(create))
    }
}
