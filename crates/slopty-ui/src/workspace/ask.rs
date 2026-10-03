//! The empty workspace asks for work: a composer-shaped field, "What should an agent do?", with
//! the machine and the directory the agent starts in as chips under it. ↵ starts the agent's
//! thread there (the last agent started on that machine, else the first it offers) with what
//! was typed as its first prompt; ↵ on nothing starts it bare.
//!
//! The field takes the keyboard once as the page shows, while the workspace itself holds it, so
//! a person landing on an empty workspace can type the task at once; not while a tile just
//! closed can be taken back, since ⌘Z is the workspace's then.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, px,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::layout::WorkerKey;

use super::WorkspaceView;
use super::strip::RecentPlace;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::IconName;
use crate::kit;

/// What the empty workspace asks.
pub(crate) const ASK: &str = "What should an agent do?";

/// The field as a view of its own, so its caret's blink draws the field alone and not the strip
/// it sits on, as the mark above it does.
pub(super) struct AskField {
    input: Entity<InputState>,
}

impl gpui::Render for AskField {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        Input::new(&self.input).appearance(false).aria_label(ASK)
    }
}

/// The directory the question's agent starts in.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
enum Dir {
    /// The worker's most recent place, else its own default.
    #[default]
    Latest,
    /// This one.
    At(String),
    /// The worker's own default.
    Own,
}

/// The question's field and what its chips chose.
#[derive(Default)]
pub(super) struct Ask {
    input: Option<Entity<InputState>>,
    field: Option<Entity<AskField>>,
    events: Option<Subscription>,
    /// The worker its chip chose; the one "+" chose, else the one in context, while none is.
    on: Option<WorkerKey>,
    /// The directory its chip chose.
    cwd: Dir,
    /// Whether the last frame showed the question, so the keyboard comes to it once.
    shown: bool,
}

impl WorkspaceView {
    /// Make the field once there is a window, and give it the keyboard as the page shows while
    /// the workspace holds it. `bare`: the active workspace has nothing on it.
    pub(super) fn settle_ask(&mut self, bare: bool, window: &mut Window, cx: &mut Context<Self>) {
        let showing = bare && !self.workers.is_empty();
        if showing && self.ask.input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(ASK));
            self.ask.events =
                Some(cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.ask_agent(window, cx);
                    }
                }));
            self.ask.field = Some(cx.new(|_| AskField { input: input.clone() }));
            self.ask.input = Some(input);
        }
        let arrived = showing && !self.ask.shown;
        self.ask.shown = showing;
        // A tile just closed leaves ⌘Z to take it back, which the field would take as its own.
        if arrived
            && !self.closing_offered()
            && self.focus.is_focused(window)
            && let Some(input) = &self.ask.input
        {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// Where the question's agent would start: the worker, and the directory on it.
    #[cfg(test)]
    pub(super) fn ask_target(&self) -> Option<(WorkerKey, Option<String>)> {
        let key = self.ask_worker()?;
        Some((key, self.ask_cwd(key)))
    }

    /// The worker the agent starts on.
    fn ask_worker(&self) -> Option<WorkerKey> {
        let known = |k: &WorkerKey| self.workers.contains_key(k);
        self.ask
            .on
            .filter(known)
            .or_else(|| self.new_on.filter(known))
            .or_else(|| self.context_worker())
    }

    /// Where shells stand on `key`, the most recently used first.
    fn ask_places(&self, key: WorkerKey) -> Vec<RecentPlace> {
        self.recent_places().into_iter().filter(|p| p.worker == key).collect()
    }

    /// The directory the agent starts in on `key`: the chip's choice, else the worker's most
    /// recent place, else its own default.
    fn ask_cwd(&self, key: WorkerKey) -> Option<String> {
        match &self.ask.cwd {
            Dir::At(cwd) => Some(cwd.clone()),
            Dir::Own => None,
            Dir::Latest => self.ask_places(key).into_iter().next().map(|p| p.cwd),
        }
    }

    /// The worker chip: the next worker, in its own default directory's place.
    fn ask_next_worker(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<WorkerKey> = self.workers.keys().copied().collect();
        let at = self.ask_worker().and_then(|k| keys.iter().position(|x| *x == k));
        let next = at.map_or(0, |at| at.saturating_add(1)).checked_rem(keys.len()).unwrap_or(0);
        self.ask.on = keys.get(next).copied();
        self.ask.cwd = Dir::Latest;
        cx.notify();
    }

    /// The directory chip: the worker's next place, then its own default, and round.
    fn ask_next_place(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.ask_worker() else { return };
        let mut ways: Vec<Dir> = self.ask_places(key).into_iter().map(|p| Dir::At(p.cwd)).collect();
        ways.push(Dir::Own);
        let now = self.ask_cwd(key).map_or(Dir::Own, Dir::At);
        let at = ways.iter().position(|w| *w == now);
        let next = at.map_or(0, |at| at.saturating_add(1)).checked_rem(ways.len()).unwrap_or(0);
        self.ask.cwd = ways.into_iter().nth(next).unwrap_or_default();
        cx.notify();
    }

    /// ↵ in the field: the agent's thread on the chosen machine, in the chosen directory, with
    /// what was typed as its first prompt.
    pub(super) fn ask_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.ask_worker() else { return };
        let Some(input) = self.ask.input.clone() else { return };
        if let Some(w) = self.workers.get(&key).filter(|w| w.link.is_none()) {
            let text = format!("{} is {}", w.name, w.status.text());
            self.show_notice(text, cx);
            return;
        }
        let Some(agent) = self.agent_for(key) else {
            self.show_notice(super::agent_start::NO_AGENT.to_owned(), cx);
            return;
        };
        let prompt = input.read(cx).value().trim().to_owned();
        let prompt = (!prompt.is_empty()).then_some(prompt);
        let cwd = self.ask_cwd(key).unwrap_or_else(|| "~".to_owned());
        self.start_thread(key, agent, cwd, prompt, cx);
        input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        self.ask.on = None;
        self.ask.cwd = Dir::Latest;
    }

    /// The question, with its chips; `None` before the field is made or with no worker.
    pub(super) fn render_ask(&self, cx: &Draw<'_, Self>) -> Option<gpui::AnyElement> {
        let field = self.ask.field.clone()?;
        let key = self.ask_worker()?;
        let theme = &self.theme;
        let spacing = theme.spacing;
        let worker = self.worker_name(key);
        let cwd = self.ask_cwd(key);
        let place = cwd.as_ref().map_or_else(
            || "~".to_owned(),
            |cwd| {
                self.ask_places(key)
                    .into_iter()
                    .find(|p| p.cwd == *cwd)
                    .map_or_else(|| super::tile::cwd_tail(cwd, self.home_of(key)), |p| p.name)
            },
        );
        let several = self.workers.len() > 1;
        let worker_chip = self
            .ask_chip("ask-worker", IconName::Server, worker.clone(), several)
            .aria_label(SharedString::from(format!("On {worker}")))
            .when(several, |el| {
                el.on_click(cx.listener(|this, _ev, _window, cx| this.ask_next_worker(cx)))
            });
        let place_chip = self
            .ask_chip("ask-place", IconName::Folder, place.clone(), true)
            .aria_label(SharedString::from(format!("In {place}")))
            .on_click(cx.listener(|this, _ev, _window, cx| this.ask_next_place(cx)));
        Some(
            kit::elevate(div(), theme)
                .id("ask")
                .debug_selector(|| "ask".to_owned())
                .w_full()
                .flex()
                .flex_col()
                .gap(px(spacing.sm))
                .p(px(spacing.md))
                .rounded(px(theme.radii.lg))
                .child(
                    div()
                        .debug_selector(|| "ask-field".to_owned())
                        .text_size(px(theme.typography.prose()))
                        .child(field),
                )
                // The chips' glyphs stand on the field's text edge.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(spacing.xs))
                        .ml(px(kit::FIELD_INSET - spacing.xs))
                        .child(worker_chip)
                        .child(place_chip),
                )
                .into_any_element(),
        )
    }

    /// One of the question's chips: its glyph and what it says; `pressable` brightens it under
    /// the pointer.
    fn ask_chip(
        &self,
        id: &'static str,
        icon: IconName,
        text: String,
        pressable: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(gpui::accesskit::Role::Button)
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .min_w_0()
            .h(px(kit::icon_button_side(theme)))
            .px(px(theme.spacing.xs))
            .rounded(px(theme.radii.sm))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .when(pressable, |el| el.cursor_pointer().hover(|st| st.bg(hsla(s.raised))))
            .child(crate::icons::icon(
                theme,
                icon,
                crate::icons::IconSize::Inline,
                hsla(s.text_muted),
            ))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(text)),
            )
    }
}
