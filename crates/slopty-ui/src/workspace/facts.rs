//! What the strip and the chrome show of a body that changes on its own: a shell's command and
//! title, a stream's first frame and sound, a face's turn and approval. Copied out of the body
//! each time it changes, and news for the views that show it only when the copy changes.
//!
//! GPUI draws a view again when an entity it read changed. A shell changes with every line of
//! output, a stream with every frame, a face with every streamed word; a header or a navigator
//! row that read them would be built again as often, for a title or a mark that stayed as it
//! was. So the strip and the chrome read these facts, which are the workspace's, and never the
//! bodies themselves.

use std::time::{Duration, Instant};

use gpui::{App, Context};
use slopty_core::{ItemId, SessionId};
use slopty_proto::screen::SourceState;

use super::WorkspaceView;
use crate::conversation::{ConversationView, HeaderChips};
use crate::screen::{ScreenView, StreamHeader};
use crate::terminal::TerminalView;

/// How often the running readouts' clock moves: they count whole seconds.
const READOUT_TICK: Duration = Duration::from_secs(1);

/// What the workspace shows of a shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ShellFacts {
    /// The title its program set (OSC 0/2).
    pub title: Option<String>,
    /// The command it runs.
    pub running: Option<String>,
    /// Since when this client has seen it run, when it saw it start.
    pub started: Option<Instant>,
    /// The command it ran before the newest prompt.
    pub last: Option<String>,
    /// How the command before the newest prompt ended.
    pub exit: Option<u8>,
    /// That command failed and its block shows in the grid ([`TerminalView::failure_in_view`]).
    pub failure_in_view: bool,
    /// This client's size rules the PTY.
    pub driving: bool,
}

impl ShellFacts {
    pub(super) fn of(view: &TerminalView) -> Self {
        let state = view.state();
        let exit = state
            .prompt_before(slopty_grid::LineIndex(u64::MAX))
            .and_then(|prompt| state.line(prompt)?.mark.exit());
        Self {
            title: view.title().map(str::to_owned),
            running: state.running_command().map(str::to_owned),
            started: view.command_started(),
            last: state.last_command(),
            exit,
            failure_in_view: view.failure_in_view(),
            driving: view.driving(),
        }
    }
}

/// What the workspace shows of a remote window's or display's stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ScreenFacts {
    /// Asked for and live, with no frame drawn yet.
    pub waiting: bool,
    /// A frame has been drawn.
    pub drawn: bool,
    /// The picture's size, in pixels.
    pub size: (u32, u32),
    /// The worker has sent sound for it.
    pub has_audio: bool,
    /// Its sound is silenced here.
    pub muted: bool,
    /// The system's shortcuts go to the worker.
    pub system_keys: bool,
    /// What its header shows of it.
    pub header: StreamHeader,
}

impl ScreenFacts {
    pub(super) fn of(view: &ScreenView) -> Self {
        let drawn = view.frames() > 0;
        Self {
            waiting: !drawn && view.source_state() == SourceState::Live,
            drawn,
            size: view.size(),
            has_audio: view.has_audio(),
            muted: view.muted(),
            system_keys: view.system_keys(),
            header: view.header(),
        }
    }
}

/// What the workspace shows of a face.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct FaceFacts {
    /// The transcript shows the agent mid-turn ([`ConversationView::mid_turn`]).
    pub mid_turn: bool,
    /// Its first prompt's first line, which names a tile its agent has not titled.
    pub first_prompt: Option<String>,
    /// An approval is open in it.
    pub asks: bool,
    /// One line of what the agent does ([`ConversationView::summary`]).
    pub summary: Option<String>,
    /// What the tile's header shows of it.
    pub chips: HeaderChips,
}

impl FaceFacts {
    pub(super) fn of(view: &ConversationView) -> Self {
        Self {
            mid_turn: view.mid_turn(),
            first_prompt: view.first_prompt(),
            asks: view.approvals().prompt().is_some(),
            summary: view.summary(),
            chips: view.header_chips_state(),
        }
    }
}

impl WorkspaceView {
    /// `session`'s shell as last copied.
    pub(super) fn shell(&self, session: SessionId) -> Option<&ShellFacts> {
        self.facts.shells.get(&session)
    }

    /// Item `id`'s stream as last copied.
    pub(super) fn stream(&self, id: ItemId) -> Option<&ScreenFacts> {
        self.facts.screens.get(&id)
    }

    /// `session`'s face as last copied.
    pub(super) fn face(&self, session: SessionId) -> Option<&FaceFacts> {
        self.facts.faces.get(&session)
    }

    /// Copy `session`'s shell again: what changed, if anything.
    pub(super) fn copy_shell(
        &mut self,
        session: SessionId,
        cx: &App,
    ) -> Option<(ShellFacts, ShellFacts)> {
        let now = ShellFacts::of(self.terminals.get(&session)?.read(cx));
        let was = self.facts.shells.insert(session, now.clone()).unwrap_or_default();
        (was != now).then_some((was, now))
    }

    /// Copy item `id`'s stream again. When it changed, everything that shows it is drawn again:
    /// a stream's facts change a few times in its life, not with its frames.
    pub(super) fn stream_changed(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.screens.get(&id) else { return };
        let now = ScreenFacts::of(view.read(cx));
        if self.facts.screens.insert(id, now) != Some(now) {
            cx.notify();
        }
    }

    /// Copy `session`'s face again: what changed, if anything.
    pub(super) fn copy_face(
        &mut self,
        session: SessionId,
        cx: &App,
    ) -> Option<(FaceFacts, FaceFacts)> {
        let now = FaceFacts::of(self.faces.views.get(&session)?.read(cx));
        let was = self.facts.faces.insert(session, now.clone()).unwrap_or_default();
        (was != now).then_some((was, now))
    }

    /// Copy `session`'s face again. Its turn, its first prompt or an approval is news for
    /// every mark and title; its line, for the navigator and the strip's overview; its
    /// readouts, for its tile's header. Nothing else it streams is anybody's news.
    pub(super) fn face_changed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some((was, now)) = self.copy_face(session, cx) else { return };
        if (was.mid_turn, &was.first_prompt, was.asks)
            != (now.mid_turn, &now.first_prompt, now.asks)
        {
            cx.notify();
            return;
        }
        if was.summary != now.summary {
            App::notify(cx, self.chrome.navigator.entity_id());
            App::notify(cx, self.strip_host.entity_id());
        } else if was.chips != now.chips {
            App::notify(cx, self.strip_host.entity_id());
        }
    }

    /// Now, as a running readout counts it (a command's time, an agent's turn): the last tick,
    /// a second at most behind the clock. Built from the clock itself, a readout drawn from
    /// the last frame would show the second it was built in, and one built again another.
    pub(super) const fn ticked(&self) -> Option<(Instant, u64)> {
        self.facts.ticked
    }

    /// Keep the readouts' clock moving, once a second, while a command runs or an agent works,
    /// each tick news for the views that count: the strip's headers, the navigator's rows and
    /// the status bar.
    pub(super) fn keep_time(&mut self, cx: &Context<Self>) {
        if self.facts.ticking || !self.readouts_run() {
            return;
        }
        self.facts.ticking = true;
        // Nothing counted before this: the clock moves with no news for anybody.
        self.facts.ticked = Some(Self::readout_now());
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(READOUT_TICK).await;
                let going = this.update(cx, |this, cx| {
                    this.facts.ticking = this.readouts_run();
                    if this.facts.ticking {
                        this.tick_readouts(cx);
                    }
                    this.facts.ticking
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// Move the readouts' clock to now, and tell the views that show a count: the navigator for
    /// an agent's turn or a command's time, the strip for a header's, the status bar for the
    /// focused shell's.
    pub(super) fn tick_readouts(&mut self, cx: &mut Context<Self>) {
        self.facts.ticked = Some(Self::readout_now());
        let counting = |session: &SessionId| self.running_for(*session).is_some();
        let shells = self.facts.shells.keys().any(counting);
        let focused = self.focused().and_then(|tile| self.item(tile)).is_some_and(|item| {
            matches!(&item.kind, slopty_proto::items::ItemKind::Terminal { session } if counting(session))
        });
        if shells || self.agents_work() {
            App::notify(cx, self.chrome.navigator.entity_id());
        }
        if shells {
            App::notify(cx, self.strip_host.entity_id());
        }
        if focused {
            App::notify(cx, self.chrome.statusbar.entity_id());
        }
    }

    /// The clocks a readout counts by, read now.
    fn readout_now() -> (Instant, u64) {
        (Instant::now(), slopty_core::WallMs::now().as_millis())
    }

    /// Whether an agent works, which its navigator row counts.
    fn agents_work(&self) -> bool {
        self.agents.values().any(|agent| {
            matches!(
                agent.status,
                slopty_proto::agent::AgentStatus::Working
                    | slopty_proto::agent::AgentStatus::Tool { .. }
            )
        })
    }

    /// Whether a readout counts: a command runs, or an agent works.
    fn readouts_run(&self) -> bool {
        self.facts.shells.values().any(|shell| shell.running.is_some()) || self.agents_work()
    }

    /// Forget the facts of bodies no longer kept.
    pub(super) fn prune_facts(&mut self) {
        let Self { facts, terminals, screens, faces, .. } = self;
        facts.shells.retain(|session, _| terminals.contains_key(session));
        facts.screens.retain(|id, _| screens.contains_key(id));
        facts.faces.retain(|session, _| faces.views.contains_key(session));
    }
}

/// The facts of every body the workspace keeps, by kind, and the clock their running readouts
/// count by.
#[derive(Debug, Default)]
pub(super) struct Facts {
    shells: std::collections::HashMap<SessionId, ShellFacts>,
    screens: std::collections::HashMap<ItemId, ScreenFacts>,
    faces: std::collections::HashMap<SessionId, FaceFacts>,
    /// The readouts' last tick ([`WorkspaceView::keep_time`]): the monotonic clock, and the
    /// wall clock in Unix milliseconds for what a worker stamped.
    ticked: Option<(Instant, u64)>,
    /// A tick is on its way.
    ticking: bool,
}

impl Facts {
    /// How many facts are kept of each kind, for the footprint.
    #[cfg(test)]
    pub(super) fn lens(&self) -> [(&'static str, usize); 3] {
        [
            ("facts.shells", self.shells.len()),
            ("facts.screens", self.screens.len()),
            ("facts.faces", self.faces.len()),
        ]
    }
}
