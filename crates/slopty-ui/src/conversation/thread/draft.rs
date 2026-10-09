//! A thread not started yet: the start tile's composer is the thread's own, drawn on this
//! state rather than on a mirror (`docs/decisions/ui.md`, "The first message is written in the
//! thread's composer").
//!
//! Nothing goes to the worker while it is a draft. The chips switch the model, the mode and the
//! effort here, on the draft's meters, and ↵ hands the start what was chosen with the message
//! and its attachments ([`DraftSent`]). The chips offer what the machine says a new thread of
//! the agent can start with (`InstalledAgent::offers`): its models, modes, efforts and
//! commands. Where it offers none, the chip offers nothing and the agent starts as it would.
//! Until a mode is chosen, the mode chip names the one the machine says a start begins in
//! (Claude Code's settings, else auto mode), and the start asks for none.
//!
//! The place chip switches where it starts, in a folder that is a repository: in the folder
//! itself or in a new worktree of it, and the branch that worktree starts from
//! (`docs/decisions/agents.md`, "A start picks its worktree's base branch").

use gpui::{Context, EventEmitter};
use slopty_core::WallMs;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{AgentId, Cap, Drive, Offers, ThreadId, ThreadMeta, ThreadState};

/// What a draft's ↵ hands its start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DraftSent {
    /// The first message.
    pub text: String,
    /// The files attached to it, as `Intent::Send::attachments` holds them.
    pub attachments: Vec<String>,
    /// The model chosen, by the agent's id; its default when `None`.
    pub model: Option<String>,
    /// The permission mode chosen, by the agent's id; its default when `None`.
    pub mode: Option<String>,
    /// The effort chosen, by the agent's id; its default when `None`.
    pub effort: Option<String>,
    /// The other agents the same message starts on, each in a new worktree of its own
    /// ([`Draft::toggle_also`]), at their defaults.
    pub also: Vec<AgentId>,
    /// It starts in a new worktree of its folder ([`Draft::set_worktree`]).
    pub worktree: bool,
    /// The branch that worktree starts from, by name; the clone's checked-out one when `None`
    /// ([`Draft::set_base`]).
    pub base: Option<String>,
}

/// Where a thread works, by name: its folder's, its machine's, and whether it is a new
/// worktree of that folder.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Place {
    /// The folder by its name: the project's, else its own, `~` at home; none when unknown.
    pub folder: Option<String>,
    /// The machine by its name.
    pub machine: String,
    /// In a new worktree of the folder's repository.
    pub worktree: bool,
    /// The pull request that worktree checks out, by number.
    pub pull: Option<u32>,
    /// The branch that worktree starts from, when one was chosen; the clone's checked-out one
    /// else. A pull request's worktree checks the pull request out instead.
    pub base: Option<String>,
}

impl Place {
    /// Where, as the question over an empty thread asks it: "in slopty", "in a new worktree of
    /// slopty", "in a new worktree of slopty from develop"; nothing when the folder is not
    /// known.
    #[must_use]
    pub fn within(&self) -> Option<String> {
        let folder = self.folder.as_deref()?;
        let with = self.pull.map(|n| format!("with #{n} ")).unwrap_or_default();
        let from = match (&self.base, self.pull) {
            (Some(base), None) => format!(" from {base}"),
            _ => String::new(),
        };
        Some(if self.worktree {
            format!("{with}in a new worktree of {folder}{from}")
        } else {
            format!("in {folder}")
        })
    }

    /// Where, as a line says it: "in slopty on studio".
    #[must_use]
    pub fn said(&self) -> String {
        match self.within() {
            Some(within) if self.machine.is_empty() => within,
            Some(within) => format!("{within} on {}", self.machine),
            None => format!("on {}", self.machine),
        }
    }
}

/// A thread on its way, until its start goes.
pub struct Draft {
    state: ThreadState,
    /// Where it will start.
    place: Place,
    /// The start went: the thread says it is starting until it lands.
    sent: bool,
    /// First messages sent before, newest first: what ↑ brings back.
    recall: Vec<String>,
    /// The other agents the machine can start the same message on beside it, offered while it
    /// starts in a new worktree ([`Self::others`]).
    others: Vec<AgentId>,
    /// Those of [`Self::others`] chosen to run it too, in the order chosen.
    also: Vec<AgentId>,
    /// Its mode was chosen, by a switch or the last start's chips. Until then its mode chip
    /// names the mode the machine says a start begins in (`Offers::mode`), and the start asks
    /// for none, so the folder's own settings still have their say.
    mode_chosen: bool,
}

impl EventEmitter<DraftSent> for Draft {}

impl std::fmt::Debug for Draft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Draft")
            .field("agent", &self.state.meta.agent)
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

impl Draft {
    /// A draft of a thread of `agent` in `cwd`, starting `place`, its chips offering what
    /// `offers` says the agent can start with on that machine, with `recall` for ↑.
    #[must_use]
    pub fn new(
        agent: AgentId,
        cwd: String,
        offers: Offers,
        place: Place,
        recall: Vec<String>,
    ) -> Self {
        let Offers { models, modes, mode, efforts, commands } = offers;
        // The doors a draft opens are the choices its start takes: a list offered is a chip
        // that switches.
        let caps = [
            (!models.is_empty()).then_some(Cap::SET_MODEL),
            (!modes.is_empty()).then_some(Cap::SET_MODE),
            (!efforts.is_empty()).then_some(Cap::SET_EFFORT),
        ];
        let mut state = ThreadState::new(ThreadMeta {
            id: ThreadId::new(),
            agent,
            agent_version: String::new(),
            native: String::new(),
            cwd,
            title: String::new(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::DRIVEN),
            caps: caps.into_iter().flatten().map(Cap::named).collect(),
            models,
            modes,
            efforts,
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        });
        state.commands = commands;
        state.meters.mode = mode;
        Self {
            state,
            place,
            sent: false,
            recall,
            others: Vec::new(),
            also: Vec::new(),
            mode_chosen: false,
        }
    }

    /// The same draft with its chips on `chips`, as a switch of each would set them: where a
    /// start of its agent last left them ([`slopty_client::starts::Starts::seed`]).
    #[must_use]
    pub fn seeded(mut self, chips: slopty_client::starts::Chips) -> Self {
        let slopty_client::starts::Chips { model, mode, effort } = chips;
        if let Some(model) = model {
            self.set_model(model);
        }
        let meters = &mut self.state.meters;
        if mode.is_some() {
            meters.mode = mode;
            self.mode_chosen = true;
        }
        if effort.is_some() {
            meters.effort = effort;
        }
        self
    }

    /// Its model chip on `model`, by the agent's id, named as the agent names it.
    fn set_model(&mut self, model: String) {
        let label = self.state.meta.models.iter().find(|m| m.id == model);
        let meters = &mut self.state.meters;
        meters.model = Some(label.map_or_else(|| model.clone(), |m| m.label.clone()));
        meters.model_id = Some(model);
    }

    /// The same draft, offering to start its message on `others` too, each in a worktree of
    /// its own, while it starts in a new worktree itself.
    #[must_use]
    pub fn with_others(self, others: Vec<AgentId>) -> Self {
        let mine = self.state.meta.agent.clone();
        let others = others.into_iter().filter(|a| *a != mine).collect();
        Self { others, ..self }
    }

    /// The other agents it can start its message on as well: none while it starts in the
    /// folder itself, where two agents would share a tree.
    #[must_use]
    pub fn others(&self) -> &[AgentId] {
        if self.place.worktree { &self.others } else { &[] }
    }

    /// Start in a new worktree of the folder, or in the folder itself. In the folder, no other
    /// agent runs the message too and no base is kept.
    pub fn set_worktree(&mut self, worktree: bool, cx: &mut Context<Self>) {
        self.place.worktree = worktree;
        if !worktree {
            self.also.clear();
            self.place.base = None;
        }
        cx.notify();
    }

    /// Its new worktree starts from branch `base`; from the clone's checked-out one when
    /// `None`.
    pub fn set_base(&mut self, base: Option<String>, cx: &mut Context<Self>) {
        self.place.base = base;
        cx.notify();
    }

    /// The other agents chosen to run its message too.
    #[must_use]
    pub fn also(&self) -> &[AgentId] {
        &self.also
    }

    /// Choose `agent` to run the message too, or no longer; one it does not offer is passed
    /// over.
    pub fn toggle_also(&mut self, agent: &AgentId, cx: &mut Context<Self>) {
        if !self.others.contains(agent) {
            return;
        }
        if let Some(at) = self.also.iter().position(|a| a == agent) {
            self.also.remove(at);
        } else {
            self.also.push(agent.clone());
        }
        cx.notify();
    }

    /// The state the view draws.
    #[must_use]
    pub const fn state(&self) -> &ThreadState {
        &self.state
    }

    /// Where it will start.
    #[must_use]
    pub const fn place(&self) -> &Place {
        &self.place
    }

    /// Whether its start went.
    #[must_use]
    pub const fn sent(&self) -> bool {
        self.sent
    }

    /// First messages sent before, newest first.
    #[must_use]
    pub fn recall(&self) -> &[String] {
        &self.recall
    }

    /// The composer asked `intent` of the thread: a switch sets the draft's meters, and a
    /// message starts it, once; nothing else is asked of a thread that is not there yet.
    pub(super) fn take(&mut self, intent: Intent, cx: &mut Context<Self>) {
        match intent {
            Intent::SetModel { model } => self.set_model(model),
            Intent::SetMode { mode } => {
                self.state.meters.mode = Some(mode);
                self.mode_chosen = true;
            }
            Intent::SetEffort { effort } => self.state.meters.effort = Some(effort),
            Intent::Send { text, attachments, .. } if !self.sent => {
                self.sent = true;
                let meters = &self.state.meters;
                cx.emit(DraftSent {
                    text,
                    attachments,
                    model: meters.model_id.clone(),
                    mode: meters.mode.clone().filter(|_| self.mode_chosen),
                    effort: meters.effort.clone(),
                    also: self.also.clone(),
                    worktree: self.place.worktree,
                    base: self.place.base.clone().filter(|_| self.place.worktree),
                });
            }
            _ => return,
        }
        cx.notify();
    }

    /// The start was refused: the draft can go again.
    pub fn unsent(&mut self, cx: &mut Context<Self>) {
        self.sent = false;
        cx.notify();
    }

    /// It checks out pull request `number` in its new worktree, as its place says.
    pub fn set_pull(&mut self, number: u32, cx: &mut Context<Self>) {
        self.place.pull = Some(number);
        cx.notify();
    }

    /// The start went again as it was, after its worktree's setup failed: the thread says it
    /// is starting once more, its message kept as the start holds it.
    pub fn resent(&mut self, cx: &mut Context<Self>) {
        self.sent = true;
        cx.notify();
    }
}
