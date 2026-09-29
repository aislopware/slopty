//! Attention on a pocketed phone: which moments become a system notification while the app is
//! not in front, and where a tapped one leads.
//!
//! Two moments notify. An agent starts to need the human (a permission, a question, input it
//! asks for), or a shell command that ran at least the slow-command time ends. Nothing notifies
//! while the app is in front, since the inbox says it there. A tile has at most one
//! notification up: the note's identifier is its session's, so a newer one replaces the older,
//! and an agent answered anywhere takes its own back. Coming back to the app takes back every
//! note it posted, because the inbox now shows the same things. The icon badge is the inbox's
//! unread count.
//!
//! An agent that waits on a yes or no held for this client (`inbox::approvals`) is posted with
//! the approval buttons (`notify::APPROVAL`): "Allow" and "Deny" answer it where the note is,
//! "Show" opens its tile as a tap does. The prompt is held a moment after the agent's status
//! says it waits, so the note already up is replaced, silently, once the prompt comes, and
//! again without the buttons once it is no longer held (the terminal asks by then). A prompt
//! this client answered takes its note away instead: the agent still reads as waiting until its
//! worker says the prompt settled, and the answer is not news.
//!
//! [`Attention`] decides and hands what it decided to a [`Notifier`]; the app owns one and
//! feeds it a [`Look`] after every change of the workspace, and each finished command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context, Entity};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_platform::notify::{self, APPROVAL, Note, Notifier, Tap};
use slopty_proto::conversation::Verdict;
use slopty_proto::items::ItemKind;

use super::agents::{agent_ask_line, agent_status_word};
use super::{Finished, WorkspaceView};
use crate::terminal::TerminalView;

/// The `userInfo` key of the worker a note is about.
const WORKER: &str = "worker";
/// The `userInfo` key of the tile's item.
const ITEM: &str = "item";
/// The `userInfo` key of the session.
const SESSION: &str = "session";
/// The `userInfo` key of the permission prompt an approval note answers.
const ASK: &str = "ask";

/// What a note's answer says when its prompt was no longer held: answered elsewhere, or the
/// terminal asks by now.
pub(super) const NO_LONGER_WAITING: &str = "That prompt is no longer waiting";

/// Where a notification leads: the worker, its tile when it has one here, and the session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
    /// The worker the session runs on.
    pub worker: WorkerKey,
    /// The tile's item; `None` for an agent the server reported with no tile here.
    pub item: Option<ItemId>,
    /// The session.
    pub session: SessionId,
}

impl Route {
    /// The route as a note's `userInfo` carries it.
    fn info(self) -> BTreeMap<String, String> {
        let mut info = BTreeMap::from([
            (WORKER.to_owned(), self.worker.value().to_string()),
            (SESSION.to_owned(), self.session.to_string()),
        ]);
        if let Some(item) = self.item {
            info.insert(ITEM.to_owned(), item.to_string());
        }
        info
    }

    /// The route a tapped note carries; `None` when it carries none (a note this module did not
    /// post).
    #[must_use]
    pub fn of_tap(tap: &Tap) -> Option<Self> {
        let worker = WorkerKey::new(tap.info.get(WORKER)?.parse().ok()?);
        let session = tap.info.get(SESSION)?.parse().ok()?;
        let item = tap.info.get(ITEM).and_then(|i| i.parse().ok());
        Some(Self { worker, item, session })
    }
}

/// An agent waiting on the human, as its note would say it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asking {
    /// Where it runs.
    pub route: Route,
    /// The tile's name, else the worker's.
    pub title: String,
    /// What it asks (`agent_ask_line`), else its state in a word or two.
    pub body: String,
    /// The yes-or-no prompt held for this client that the note's buttons answer.
    pub approval: Option<u64>,
    /// The prompt this client answered that its worker has not yet said is settled.
    pub answered: Option<u64>,
}

impl Asking {
    /// Its note: the approval buttons while a prompt is held, and a sound unless `silent`.
    fn note(&self, silent: bool) -> Note {
        let mut info = self.route.info();
        if let Some(ask) = self.approval {
            info.insert(ASK.to_owned(), ask.to_string());
        }
        Note {
            id: self.route.session.to_string(),
            title: self.title.clone(),
            body: self.body.clone(),
            info,
            category: self.approval.map(|_| APPROVAL),
            silent,
        }
    }
}

/// What notifications follow in the workspace, at one moment.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Look {
    /// The agents waiting on the human.
    pub asking: Vec<Asking>,
    /// The inbox's unread count: the icon badge.
    pub unread: usize,
}

/// Why a note is up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Why {
    Asks,
    Finished,
    Program,
    Unanswered,
}

/// Decides which moments notify and hands them to a [`Notifier`].
pub struct Attention {
    notifier: Rc<dyn Notifier>,
    /// The app is in front.
    active: bool,
    /// The sessions whose agent was waiting at the last look.
    asking: HashSet<SessionId>,
    /// The notes up, by session.
    posted: HashMap<SessionId, Why>,
    /// The prompt each agent's note up answers, by session.
    answers: HashMap<SessionId, Option<u64>>,
    /// The badge last set.
    badge: Option<usize>,
}

impl std::fmt::Debug for Attention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attention")
            .field("active", &self.active)
            .field("asking", &self.asking)
            .field("posted", &self.posted)
            .field("badge", &self.badge)
            .finish_non_exhaustive()
    }
}

impl Attention {
    /// Notes go to `notifier`. The app starts in front.
    #[must_use]
    pub fn new(notifier: Rc<dyn Notifier>) -> Self {
        Self {
            notifier,
            active: true,
            asking: HashSet::new(),
            posted: HashMap::new(),
            answers: HashMap::new(),
            badge: None,
        }
    }

    /// Whether the app is in front now.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    /// The app came to the front or left it. Coming back takes back every note up.
    pub fn set_active(&mut self, active: bool) {
        if active && !self.active {
            for (session, _) in self.posted.drain() {
                self.notifier.withdraw(&session.to_string());
            }
            self.answers.clear();
        }
        self.active = active;
    }

    /// The workspace changed: an agent that has just started to wait notifies while the app is
    /// away, one that stopped takes its note back, and the badge follows the inbox.
    pub fn look(&mut self, look: &Look) {
        let now: HashSet<SessionId> = look.asking.iter().map(|a| a.route.session).collect();
        if !self.active {
            for asking in &look.asking {
                let session = asking.route.session;
                let up = self.posted.get(&session) == Some(&Why::Asks);
                let was = self.answers.get(&session).copied().flatten();
                if !self.asking.contains(&session) {
                    self.post(session, Why::Asks, asking.note(false));
                } else if up && asking.approval.is_none() && was.is_some() && was == asking.answered
                {
                    self.posted.remove(&session);
                    self.notifier.withdraw(&session.to_string());
                    self.answers.insert(session, None);
                    continue;
                } else if up && self.answers.get(&session) != Some(&asking.approval) {
                    self.post(session, Why::Asks, asking.note(true));
                } else {
                    continue;
                }
                self.answers.insert(session, asking.approval);
            }
        }
        let stopped: Vec<SessionId> = self.asking.difference(&now).copied().collect();
        for session in stopped {
            if self.posted.get(&session) == Some(&Why::Asks) {
                self.posted.remove(&session);
                self.notifier.withdraw(&session.to_string());
            }
            self.answers.remove(&session);
        }
        self.asking = now;
        if self.badge != Some(look.unread) {
            self.badge = Some(look.unread);
            self.notifier.set_badge(look.unread);
        }
    }

    /// A shell command ended in `route`'s session after running `done.elapsed`: it notifies when
    /// that is at least `slow` and the app is away. `title` is the tile's name.
    pub fn command_finished(
        &mut self,
        route: Route,
        title: String,
        done: &Finished,
        slow: Duration,
    ) {
        if self.active || done.elapsed < slow {
            return;
        }
        let command = done.command.trim();
        let body = if command.is_empty() {
            done.label()
        } else {
            format!("{command} \u{b7} {}", done.label())
        };
        let note = Note {
            id: route.session.to_string(),
            title,
            body,
            info: route.info(),
            ..Note::default()
        };
        self.post(route.session, Why::Finished, note);
    }

    /// A program in `route`'s session asked for a desktop notification: it notifies while the
    /// app is away. In front, the tile's own attention mark is enough.
    pub fn program(&mut self, route: Route, title: String, body: String) {
        if self.active {
            return;
        }
        let note = Note {
            id: route.session.to_string(),
            title,
            body,
            info: route.info(),
            ..Note::default()
        };
        self.post(route.session, Why::Program, note);
    }

    /// A note's "Allow" or "Deny" for `route`'s agent found no prompt to answer (`why`): said
    /// in a note of its own while the app is away. `title` is the tile's name.
    pub fn unanswered(&mut self, route: Route, title: String, why: &str) {
        if self.active {
            return;
        }
        let note = Note {
            id: route.session.to_string(),
            title,
            body: why.to_owned(),
            info: route.info(),
            ..Note::default()
        };
        self.post(route.session, Why::Unanswered, note);
    }

    fn post(&mut self, session: SessionId, why: Why, note: Note) {
        tracing::debug!(%session, ?why, "attention note");
        if why != Why::Asks {
            self.answers.remove(&session);
        }
        self.posted.insert(session, why);
        self.notifier.post(note);
    }
}

impl WorkspaceView {
    /// What notifications follow now: the agents waiting on the human, with their tile's name
    /// and what they ask, and the inbox's unread count.
    #[must_use]
    pub fn attention_look(&self, cx: &App) -> Look {
        let asking = self
            .needs_you()
            .into_iter()
            .filter_map(|w| {
                let agent = self.agent_state(w.session)?;
                let body = agent_ask_line(agent).unwrap_or_else(|| agent_status_word(agent));
                let route =
                    Route { worker: w.worker, item: w.tile.map(|t| t.item), session: w.session };
                let approval = self.approval(w.session).map(|prompt| prompt.ask);
                let answered = self.answered_here(w.session);
                let title = self.route_title(route, cx);
                Some(Asking { route, title, body, approval, answered })
            })
            .collect();
        Look { asking, unread: self.inbox_count() }
    }

    /// Where a note about `session` leads, and the name its title says; `None` for a session
    /// with no tile here.
    #[must_use]
    pub fn attention_route(&self, session: SessionId, cx: &App) -> Option<(Route, String)> {
        let tile = self.tile_of_session(session)?;
        let route = Route { worker: tile.worker, item: Some(tile.item), session };
        Some((route, self.route_title(route, cx)))
    }

    /// How long a command runs before its end is worth a word.
    #[must_use]
    pub const fn slow_command(&self) -> Duration {
        self.slow_command
    }

    /// Every terminal's session and view, for a watcher of their commands.
    #[must_use]
    pub fn terminal_views(&self) -> Vec<(SessionId, Entity<TerminalView>)> {
        self.terminals.iter().map(|(s, v)| (*s, v.clone())).collect()
    }

    /// The human tapped a note: focus the tile it names and give it the keyboard. A note without
    /// a route (one another part of the app posted, tagged by its session) reveals that session.
    /// An agent with no tile here gets one. An approval note's "Allow" or "Deny" answers its
    /// prompt and leaves the workspace where it is; "Show" is a tap.
    pub fn open_notification(&mut self, tap: &Tap, cx: &mut Context<Self>) {
        let route = Route::of_tap(tap);
        tracing::debug!(id = tap.id, ?route, action = ?tap.action, "note opened");
        let verdict = match tap.action.as_deref() {
            Some(notify::ALLOW) => Some(Verdict::Allow),
            Some(notify::DENY) => Some(Verdict::Deny { message: String::new(), interrupt: false }),
            Some(_) | None => None,
        };
        if let Some(verdict) = verdict {
            let ask = tap.info.get(ASK).and_then(|ask| ask.parse().ok());
            if let (Some(route), Some(ask)) = (route, ask) {
                self.verdict_tapped(route, ask, verdict, cx);
            }
            return;
        }
        let Some(route) = route else {
            if let Ok(session) = tap.id.parse::<SessionId>() {
                self.reveal_session(session, cx);
            }
            return;
        };
        let tile = route.item.map(|item| TileRef { worker: route.worker, item });
        match tile.and_then(|t| self.item(t)).map(|i| i.kind.clone()) {
            Some(kind) => {
                if let Some(item) = route.item {
                    self.go_to(item, cx);
                }
                if let ItemKind::Terminal { session } = kind {
                    self.pending_focus = Some(session);
                }
            }
            None if self.tile_of_session(route.session).is_some() => {
                self.reveal_session(route.session, cx);
            }
            None => self.show_untiled(route.worker, route.session, cx),
        }
    }

    /// What a note's title says: the tile's name, else the worker's.
    #[must_use]
    pub fn route_title(&self, route: Route, cx: &App) -> String {
        route
            .item
            .map(|item| TileRef { worker: route.worker, item })
            .and_then(|tile| self.item(tile).map(|i| self.tile_title(i, cx)))
            .or_else(|| self.workers.get(&route.worker).map(|w| w.name.clone()))
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "tests/attention.rs"]
mod tests;
