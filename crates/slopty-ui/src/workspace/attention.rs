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
//! [`Attention`] decides and hands what it decided to a [`Notifier`]; the app owns one and
//! feeds it a [`Look`] after every change of the workspace, and each finished command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context, Entity};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_platform::notify::{Note, Notifier, Tap};
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
        Self { notifier, active: true, asking: HashSet::new(), posted: HashMap::new(), badge: None }
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
        }
        self.active = active;
    }

    /// The workspace changed: an agent that has just started to wait notifies while the app is
    /// away, one that stopped takes its note back, and the badge follows the inbox.
    pub fn look(&mut self, look: &Look) {
        let now: HashSet<SessionId> = look.asking.iter().map(|a| a.route.session).collect();
        if !self.active {
            let started: Vec<&Asking> =
                look.asking.iter().filter(|a| !self.asking.contains(&a.route.session)).collect();
            for asking in started {
                let note = Note {
                    id: asking.route.session.to_string(),
                    title: asking.title.clone(),
                    body: asking.body.clone(),
                    info: asking.route.info(),
                };
                self.post(asking.route.session, Why::Asks, note);
            }
        }
        let stopped: Vec<SessionId> = self.asking.difference(&now).copied().collect();
        for session in stopped {
            if self.posted.get(&session) == Some(&Why::Asks) {
                self.posted.remove(&session);
                self.notifier.withdraw(&session.to_string());
            }
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
        let note = Note { id: route.session.to_string(), title, body, info: route.info() };
        self.post(route.session, Why::Finished, note);
    }

    /// A program in `route`'s session asked for a desktop notification: it notifies while the
    /// app is away. In front, the tile's own attention mark is enough.
    pub fn program(&mut self, route: Route, title: String, body: String) {
        if self.active {
            return;
        }
        let note = Note { id: route.session.to_string(), title, body, info: route.info() };
        self.post(route.session, Why::Program, note);
    }

    fn post(&mut self, session: SessionId, why: Why, note: Note) {
        tracing::debug!(%session, ?why, "attention note");
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
                Some(Asking { route, title: self.route_title(route, cx), body })
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
    /// An agent with no tile here gets one.
    pub fn open_notification(&mut self, tap: &Tap, cx: &mut Context<Self>) {
        let route = Route::of_tap(tap);
        tracing::debug!(id = tap.id, ?route, "note opened");
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
    fn route_title(&self, route: Route, cx: &App) -> String {
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
