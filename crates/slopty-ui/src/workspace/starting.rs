//! A thread on its way: the tile an agent's thread will fill, there from the moment the person
//! chose it. A start from the palette opens it with a field for the first message, which goes
//! as the start's prompt (`Start::prompt`), so the agent's first turn begins as it boots; ↵ on
//! an empty field starts it bare. Once sent it says "Starting Codex" and where, until the
//! machine answers with the thread, which takes the tile's place and keyboard, or says why not
//! and leaves.
//!
//! The tile is the layout's alone: the item comes with the thread, under the tile's own id, so
//! nothing moves when it lands.

use std::collections::HashMap;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::layout::{Placed, Placement, TileRef, WorkerKey};
use slopty_core::ItemId;
use slopty_proto::thread::{AgentId, ThreadId};

use super::WorkspaceView;
use super::projects::agent_label;
use super::tile::{Chrome, SHAPES_BELOW, title_ink};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Glyph, Status};
use crate::kit;

/// The tiles of threads on their way, by the id their item will have.
#[derive(Default)]
pub(super) struct Starts {
    tiles: HashMap<ItemId, Starting>,
    /// The tile whose field takes the keyboard in the next frame.
    focus: Option<ItemId>,
}

impl Starts {
    /// Whether `item` is a thread on its way.
    pub(super) fn has(&self, item: ItemId) -> bool {
        self.tiles.contains_key(&item)
    }

    /// The thread on its way in `item`'s tile.
    pub(super) fn get(&self, item: ItemId) -> Option<&Starting> {
        self.tiles.get(&item)
    }
}

/// One thread on its way.
pub(super) struct Starting {
    /// The machine it starts on.
    pub worker: WorkerKey,
    /// Its agent.
    pub agent: AgentId,
    /// The folder it starts in, as the machine takes it (`~` its home).
    pub cwd: String,
    /// The first message's field, until the start is sent.
    pub field: Option<StartField>,
    /// Whether the start went to the machine.
    pub sent: bool,
}

impl Starting {
    /// A thread of `agent` on `worker` in `cwd` on its way, with `field` for its first message
    /// or none when the start goes at once.
    pub(super) const fn new(
        worker: WorkerKey,
        agent: AgentId,
        cwd: String,
        field: Option<StartField>,
    ) -> Self {
        Self { worker, agent, cwd, field, sent: false }
    }
}

/// The first message's field, a view of its own so its caret's blink draws it alone.
pub(super) struct StartField {
    input: Entity<InputState>,
    view: Entity<FieldView>,
    _events: Subscription,
}

struct FieldView {
    input: Entity<InputState>,
    label: SharedString,
}

impl gpui::Render for FieldView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        Input::new(&self.input).appearance(false).aria_label(self.label.clone())
    }
}

/// What the first message's field asks, for `agent`.
pub(crate) fn asks(agent: &AgentId) -> String {
    format!("What should {} do?", agent_label(agent))
}

impl WorkspaceView {
    /// Open the tile of a thread of `agent` on `worker` in `cwd`, focused, its field for the
    /// first message taking the keyboard: nothing goes to the machine until ↵.
    pub(super) fn begin_start(
        &mut self,
        worker: WorkerKey,
        agent: AgentId,
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = ItemId::new();
        let label = SharedString::from(asks(&agent));
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(label.clone()));
        let events = cx.subscribe_in(&input, window, move |this, _input, event, _window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.send_first_message(item, cx);
            }
        });
        let view = cx.new(|_| FieldView { input: input.clone(), label });
        let field = StartField { input, view, _events: events };
        self.open_starting(item, Starting::new(worker, agent, cwd, Some(field)), cx);
        self.starting.focus = Some(item);
    }

    /// Open the tile of `starting`, focused.
    pub(super) fn open_starting(
        &mut self,
        item: ItemId,
        starting: Starting,
        cx: &mut Context<Self>,
    ) {
        let worker = starting.worker;
        self.starting.tiles.insert(item, starting);
        self.tick();
        self.layout.open(TileRef { worker, item }, Placement::Local);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// ↵ in a thread's first-message field: the start goes with what was typed as its prompt,
    /// or bare. A machine out of reach keeps the field and says so.
    fn send_first_message(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let Some(starting) = self.starting.tiles.get(&item) else { return };
        if let Some(w) = self.workers.get(&starting.worker).filter(|w| w.link.is_none()) {
            let text = format!("{} is {}: nothing was sent", w.name, w.status.text());
            self.show_notice(text, cx);
            return;
        }
        let prompt = starting.field.as_ref().map(|f| f.input.read(cx).value().trim().to_owned());
        let prompt = prompt.filter(|p| !p.is_empty());
        self.send_start(item, prompt, cx);
    }

    /// The start of `item`'s thread goes to its machine; the tile says it is starting.
    pub(super) fn starting_sent(&mut self, item: ItemId, cx: &mut Context<Self>) {
        if let Some(starting) = self.starting.tiles.get_mut(&item) {
            starting.field = None;
            starting.sent = true;
        }
        // The keyboard leaves the field it was in for the workspace, until the thread's own
        // composer is there to take it.
        if self.focused().is_some_and(|t| t.item == item) {
            self.pending_focus_self = true;
        }
        cx.notify();
    }

    /// The machine answered `item`'s start with `thread`: the thread's tile takes the start's
    /// place, under its id, and its composer the keyboard when the start had it. A tile closed
    /// while it started opens nothing, and the thread is said to be there.
    pub(super) fn start_landed(
        &mut self,
        key: WorkerKey,
        item: ItemId,
        thread: ThreadId,
        agent: &AgentId,
        cx: &mut Context<Self>,
    ) {
        let tile = TileRef { worker: key, item };
        if self.starting.tiles.remove(&item).is_none() {
            let text = format!("{} started on {}", agent_label(agent), self.worker_name(key));
            self.show_notice(text, cx);
            return;
        }
        if self.tile_of_thread(thread).is_some() {
            self.drop_starting_tile(tile, cx);
            self.open_thread(key, thread, cx);
            return;
        }
        self.open_thread_as(key, thread, item, cx);
    }

    /// The machine would not start `item`'s thread: its tile goes, and `why` is said.
    pub(super) fn start_failed(
        &mut self,
        key: WorkerKey,
        item: ItemId,
        why: String,
        cx: &mut Context<Self>,
    ) {
        if self.starting.tiles.remove(&item).is_some() {
            self.drop_starting_tile(TileRef { worker: key, item }, cx);
        }
        self.show_notice(why, cx);
    }

    /// The link to `key` went: a start sent there may never be answered, so its tile goes and
    /// says so; one not sent yet stays, its field kept.
    pub(super) fn starts_unlinked(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let lost: Vec<(ItemId, AgentId)> = self
            .starting
            .tiles
            .iter()
            .filter(|(_, s)| s.worker == key && s.sent)
            .map(|(item, s)| (*item, s.agent.clone()))
            .collect();
        for (item, agent) in lost {
            let why = format!(
                "{} went out of reach before {} started: start it again once it is back",
                self.worker_name(key),
                agent_label(&agent)
            );
            self.start_failed(key, item, why, cx);
        }
    }

    /// ⌘W on a thread on its way: its tile goes. A start already sent still makes its thread,
    /// which is said when it comes.
    pub(super) fn close_starting(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        let Some(starting) = self.starting.tiles.remove(&tile.item) else { return };
        tracing::debug!(item = %tile.item, sent = starting.sent, "close a thread on its way");
        self.drop_starting_tile(tile, cx);
    }

    fn drop_starting_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        self.tick();
        self.layout.remove(tile);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// Give the keyboard to the field of the start that asked for it.
    pub(super) fn settle_starting_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.starting.focus.take() else { return };
        if let Some(field) = self.starting.tiles.get(&item).and_then(|s| s.field.as_ref()) {
            field.input.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// The tile of a thread on its way, as the frame places it; `None` for any other tile
    /// with no item.
    pub(super) fn render_starting(
        &self,
        placed: &Placed,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let starting = self.starting.tiles.get(&tile.item)?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let id = tile.item;
        let label = agent_label(&starting.agent);
        let title = SharedString::from(format!("New {label} thread"));
        let place = format!(
            "on {} in {}",
            self.worker_name(starting.worker),
            super::tile::cwd_tail(&starting.cwd, self.home_of(starting.worker))
        );
        let shapes = k < SHAPES_BELOW;
        let ink = hsla(title_ink(theme, placed.focused));
        let status = starting.sent.then_some(Status::Working);
        let header = div()
            .id("title")
            .debug_selector(move || format!("title-{}", id.as_uuid()))
            .role(Role::Heading)
            .aria_label(title.clone())
            .h(px(theme.density.header * k))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .px(px(theme.spacing.inset() * k))
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(hsla(theme.content()))
            .text_size(px(theme.typography.ui_size * k))
            .text_color(ink)
            .font_family(theme.typography.ui_family.clone())
            .when(!shapes, |el| {
                el.child(crate::palette::status_slot(
                    theme,
                    Glyph::agent(&starting.agent.0),
                    status,
                    ink,
                    k,
                ))
                .child(title.clone())
            });
        let body = (!shapes).then(|| {
            if let Some(field) = &starting.field {
                self.first_message(id, field, &place, k).into_any_element()
            } else {
                let mark = crate::icons::status_icon(
                    theme,
                    Status::Working,
                    px(theme.typography.icon() * k),
                    hsla(s.text_secondary),
                );
                let said = SharedString::from(format!("Starting {label} {place}\u{2026}"));
                kit::notice(theme, k, mark, format!("Starting {label}"), Some(place.into()))
                    .id("starting")
                    .debug_selector(move || format!("starting-{}", id.as_uuid()))
                    .role(Role::Status)
                    .aria_label(said)
                    .into_any_element()
            }
        });
        let rect = placed.rect;
        let (width, height) = (rect.w * placed.scale, rect.h * placed.scale);
        let (left, top) = (rect.x + (rect.w - width) / 2.0, rect.y + (rect.h - height) / 2.0);
        Some(
            div()
                .id(ElementId::Uuid(*id.as_uuid()))
                .debug_selector(move || format!("item-{}", id.as_uuid()))
                .role(Role::Group)
                .aria_label(title)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height))
                .opacity(placed.alpha)
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(hsla(theme.content()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(theme.spacing.lg * k))
                        .children(body),
                )
                .into_any_element(),
        )
    }

    /// The first message's field, with where the thread will start under it.
    fn first_message(&self, id: ItemId, field: &StartField, place: &str, k: f32) -> gpui::Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        kit::elevate(div(), theme)
            .debug_selector(move || format!("first-message-{}", id.as_uuid()))
            .w_full()
            .max_w(px(super::strip::EMPTY_W * k))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm * k))
            .p(px(theme.spacing.md * k))
            .rounded(px(theme.radii.lg * k))
            .child(div().text_size(px(theme.typography.title() * k)).child(field.view.clone()))
            .child(
                div()
                    .ml(px(kit::FIELD_INSET))
                    .text_size(px(theme.typography.small() * k))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(crate::palette::sentence_case(place))),
            )
    }
}
