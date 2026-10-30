//! Attention: which moments become a system notification, and where a tapped one leads
//! (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a feed").
//!
//! Four moments notify. An agent starts to need the human (a permission, a question, input it
//! asks for) while this device hears its worker, a program asks for a notification (`OSC 9`),
//! an agent's turn that ran at least the slow-command time ends, or a shell command that ran
//! that long ends. A moment notifies when
//! its session is in the background: the app is not in front, or it is and shows another
//! session ([`Attention::set_on_screen`]). The session on screen never notifies, since the
//! person sees it. A session has at most one notification up: the note's identifier is its
//! session's, so a newer one replaces the older, and an agent answered anywhere takes its own
//! back. Once the person looks at a session, its note goes. The icon badge counts the sessions
//! that need the person and those that failed.
//!
//! A note answers nothing and sends nothing to the agent (TUI-first, `docs/decisions/agents.md`):
//! a tap opens the session, whose TUI the person answers in.
//!
//! Nothing notifies either while the person is at another of their devices
//! ([`Attention::set_present_elsewhere`]): at the Mac, the phone in their pocket stays quiet, and
//! what it had up is taken back, since the Mac in front of them says it.
//!
//! Linked to a server, the server decides ([`Attention::set_server_led`]): it ranks every
//! thread on one ladder and picks the clients a moment goes to by where the person is, so two
//! devices never both say it. Its notices are then the only agent moments that post here
//! ([`Attention::notice`], made by [`WorkspaceView::heard`]); the workspace's own look still
//! takes back what no longer waits, and keeps the badge.
//! A shell's moments and a program's notes are this client's own and post as before: the
//! server never hears them.
//!
//! A note of an agent that needs the person is Time Sensitive (`Note::urgent`), so it reaches
//! them through a Focus and past the notification summary; a finished turn, a failure, a
//! shell's command and a program's own note wait there like any other app's.
//!
//! With notifications turned off in the system's settings, a note is dropped where nobody sees
//! it. So the first time one goes unsaid in a run, coming back to the app says notifications
//! are off ([`Attention::unsaid_while_off`], [`NOTES_OFF`]), once.
//!
//! [`Attention`] decides and hands what it decided to a [`Notifier`]; the app owns one and
//! feeds it a [`Look`] after every change of the workspace, and each finished command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::Entity;
use slopty_client::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_platform::notify::info::{ITEM, SESSION, THREAD, WORKER};
use slopty_platform::notify::{self, Alerts, Note, Notifier, Tap};
use slopty_proto::thread::ThreadId;
use slopty_proto::thread::attention::{Notice, NoticeKind, Subject};
use slopty_terminal::TerminalView;

use super::agents::{agent_ask_text, agent_status_word};
use super::{Finished, WorkspaceView};

/// What a note is about: a terminal, whose shell or program it speaks of, or an agent's
/// thread.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum About {
    /// A terminal's session.
    Session(SessionId),
    /// An agent's thread.
    Thread(ThreadId),
}

impl About {
    /// The note's identifier: a terminal's is its session's, so a note another part of the app
    /// posted for it is the same note.
    fn note_id(self) -> String {
        match self {
            Self::Session(session) => session.to_string(),
            Self::Thread(thread) => format!("{THREAD}-{thread}"),
        }
    }
}

/// Where a notification leads: the worker, its tile when it has one here, and what it is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
    /// The worker it runs on.
    pub worker: WorkerKey,
    /// The tile's item; `None` for an agent the server reported with no tile here.
    pub item: Option<ItemId>,
    /// The terminal or thread.
    pub about: About,
}

impl Route {
    /// The terminal it is about; `None` for a thread with none.
    #[must_use]
    pub const fn session(self) -> Option<SessionId> {
        match self.about {
            About::Session(session) => Some(session),
            About::Thread(_) => None,
        }
    }

    /// The route as a note's `userInfo` carries it.
    fn info(self) -> BTreeMap<String, String> {
        let about = match self.about {
            About::Session(session) => (SESSION.to_owned(), session.to_string()),
            About::Thread(thread) => (THREAD.to_owned(), thread.to_string()),
        };
        let mut info =
            BTreeMap::from([(WORKER.to_owned(), self.worker.value().to_string()), about]);
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
        let about = match tap.info.get(SESSION) {
            Some(session) => About::Session(session.parse().ok()?),
            None => About::Thread(tap.info.get(THREAD)?.parse().ok()?),
        };
        let item = tap.info.get(ITEM).and_then(|i| i.parse().ok());
        Some(Self { worker, item, about })
    }
}

/// An agent waiting on the human, as its note would say it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asking {
    /// Where it runs.
    pub route: Route,
    /// The tile's name, else the worker's.
    pub title: String,
    /// What it asks (`agent_ask_text`), else its state in a word or two.
    pub body: String,
    /// It started to wait while this device heard its worker. One already waiting when the
    /// worker was first heard here is no moment: the list shows it, and no note says it.
    pub fresh: bool,
}

impl Asking {
    /// Its note, which opens the session it is about.
    fn note(&self) -> Note {
        Note {
            id: self.route.about.note_id(),
            title: self.title.clone(),
            body: self.body.clone(),
            info: self.route.info(),
            urgent: true,
        }
    }
}

/// An agent's turn that finished unwatched, as its note would say it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Turn {
    /// Where it runs.
    pub route: Route,
    /// The tile's name, else the worker's.
    pub title: String,
    /// What it said it did, and how long the turn ran.
    pub body: String,
}

/// What notifications follow in the workspace, at one moment.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Look {
    /// The agents waiting on the human.
    pub asking: Vec<Asking>,
    /// The agents whose turn finished unwatched while this device heard their worker, and is
    /// unread.
    pub turns: Vec<Turn>,
    /// The sessions that need the person, and those that failed: the icon badge.
    pub unread: usize,
}

/// A notice the server picked this client for, as its note says it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Heard {
    /// Where it leads.
    pub route: Route,
    /// Why it was sent.
    pub kind: NoticeKind,
    /// The thread's title, else the tile's name.
    pub title: String,
    /// What it wants or said, named by the subagent it came from.
    pub body: String,
}

/// Why a note is up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Why {
    Asks,
    Finished,
    Program,
}

/// What the app says, once a run, on coming back after a note went unsaid because
/// notifications are off.
pub const NOTES_OFF: &str = if cfg!(target_os = "ios") {
    "Notifications are off. Turn them on in Settings."
} else {
    "Notifications are off. Turn them on in System Settings."
};

/// Decides which moments notify and hands them to a [`Notifier`].
pub struct Attention {
    notifier: Rc<dyn Notifier>,
    /// The app is in front.
    active: bool,
    /// The session the app shows, which never notifies while the app is in front.
    on_screen: Option<About>,
    /// Notes are wanted at all (Settings → Notifications' Desktop switch).
    enabled: bool,
    /// The person is at another of their devices.
    elsewhere: bool,
    /// The server's notices decide which agent moments post.
    server_led: bool,
    /// The agents waiting at the last look.
    asking: HashSet<About>,
    /// The agents whose finished turn was unread at the last look.
    turns: HashSet<About>,
    /// The notes up.
    posted: HashMap<About, Why>,
    /// The badge last set.
    badge: Option<usize>,
    /// A note went out while notifications were off, since the app last came back.
    unsaid: bool,
    /// The app has said this run that notifications are off.
    said_off: bool,
    /// The app still hears the server's notices on its link: it has not told the server it is
    /// about to be suspended, after which the server pushes instead.
    listening: bool,
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
            on_screen: None,
            enabled: true,
            elsewhere: false,
            server_led: false,
            asking: HashSet::new(),
            turns: HashSet::new(),
            posted: HashMap::new(),
            badge: None,
            unsaid: false,
            said_off: false,
            listening: true,
        }
    }

    /// Whether the app, just back in front, should say [`NOTES_OFF`]: a note went unsaid
    /// while it was away because notifications are off, and it has not said so this run.
    /// `true` once.
    pub fn unsaid_while_off(&mut self) -> bool {
        let say = std::mem::take(&mut self.unsaid)
            && !self.said_off
            && self.notifier.alerts() == Alerts::Denied;
        self.said_off |= say;
        say
    }

    /// Hand `note` to the notifier, noting when notifications are off that it goes unsaid.
    fn send(&mut self, note: Note) {
        self.unsaid |= self.notifier.alerts() == Alerts::Denied;
        self.notifier.post(note);
    }

    /// Whether the app is in front now.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    /// The app came to the front or left it. Coming back takes back the note of the session it
    /// shows, which the person now sees; every other note stays, pointing at a session in the
    /// background, until that session is looked at, answered or read. The notes pushed while it
    /// was away are the system's to list ([`Self::delivered`]).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        self.withdraw_on_screen();
    }

    /// The app shows `about`'s session now, or none: it never notifies while the app is in
    /// front, and its note goes once the person looks at it.
    pub fn set_on_screen(&mut self, about: Option<About>) {
        self.on_screen = about;
        self.withdraw_on_screen();
    }

    /// Take back the note of the session the person looks at.
    fn withdraw_on_screen(&mut self) {
        let Some(about) = self.on_screen.filter(|_| self.active) else { return };
        if self.posted.remove(&about).is_some() {
            self.notifier.withdraw(&about.note_id());
        }
    }

    /// The notes the system shows now, by identifier, as it listed them once the app came back;
    /// a push put up those the app never posted. One whose moment still stands (an ask still
    /// waiting, a turn still unread) is followed as the app's own, and goes as they do; any
    /// other goes now, and so does the note of the session on screen. Nothing goes while the
    /// app is away again by the time the list came.
    pub fn delivered(&mut self, ids: &[String]) {
        if !self.active {
            return;
        }
        for id in ids {
            if self.posted.keys().any(|about| about.note_id() == *id) {
                continue;
            }
            let of = |set: &HashSet<About>| set.iter().find(|a| a.note_id() == *id).copied();
            match (of(&self.asking), of(&self.turns)) {
                (Some(about), _) => {
                    self.posted.insert(about, Why::Asks);
                }
                (None, Some(about)) => {
                    self.posted.insert(about, Why::Finished);
                }
                (None, None) => self.notifier.withdraw(id),
            }
        }
        self.withdraw_on_screen();
    }

    /// Notes are wanted or not: turned off, every note up goes and none posts until they are
    /// wanted again.
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled && self.enabled {
            self.withdraw_all();
        }
        self.enabled = enabled;
    }

    /// The person is at another of their devices, or has left it. Arriving there takes back
    /// every note up here: the device in front of them says it.
    pub fn set_present_elsewhere(&mut self, elsewhere: bool) {
        if elsewhere && !self.elsewhere {
            self.withdraw_all();
        }
        self.elsewhere = elsewhere;
    }

    /// A server's notices reach this client, or stopped: while they do, they are the only agent
    /// moments that post, the server having chosen this client by where the person is.
    pub const fn set_server_led(&mut self, led: bool) {
        self.server_led = led;
    }

    /// The app told the server it is about to be suspended (`listening`: false), or came back.
    /// Until it comes back, the server pushes what it hears and the app posts none of its
    /// notices, so a moment is never said twice.
    pub const fn set_listening(&mut self, listening: bool) {
        self.listening = listening;
    }

    /// Whether the server's notices decide which agent moments post.
    #[must_use]
    pub const fn server_led(&self) -> bool {
        self.server_led
    }

    /// The server picked this client to say `heard`: posted unless its session is on screen,
    /// where the shell already shows it.
    pub fn notice(&mut self, heard: &Heard) {
        let session = heard.route.about;
        if !self.listening || !self.due(session) {
            return;
        }
        let note = Note {
            id: session.note_id(),
            title: heard.title.clone(),
            body: heard.body.clone(),
            info: heard.route.info(),
            ..Note::default()
        };
        let why = if heard.kind == NoticeKind::NeedsYou { Why::Asks } else { Why::Finished };
        self.post(session, why, note);
    }

    /// Whether a moment in `about`'s session is worth a note now: the session is in the
    /// background (the app is not in front, or shows another), and the person is not at
    /// another device.
    fn due(&self, about: About) -> bool {
        let looked_at = self.active && self.on_screen == Some(about);
        !self.elsewhere && !looked_at
    }

    fn withdraw_all(&mut self) {
        for (about, _) in self.posted.drain() {
            self.notifier.withdraw(&about.note_id());
        }
    }

    /// The workspace changed: an agent in the background that has just started to wait
    /// notifies, one that stopped takes its note back, and the badge follows what needs the
    /// person.
    pub fn look(&mut self, look: &Look) {
        let now: HashSet<About> = look.asking.iter().map(|a| a.route.about).collect();
        for asking in &look.asking {
            let session = asking.route.about;
            if asking.fresh
                && self.due(session)
                && !self.asking.contains(&session)
                && !self.server_led
            {
                self.post(session, Why::Asks, asking.note());
            }
        }
        let stopped: Vec<About> = self.asking.difference(&now).copied().collect();
        for session in stopped {
            if self.posted.get(&session) == Some(&Why::Asks) {
                self.posted.remove(&session);
                self.notifier.withdraw(&session.note_id());
            }
        }
        self.asking = now;
        let turns: HashSet<About> = look.turns.iter().map(|t| t.route.about).collect();
        if !self.server_led {
            let fresh: Vec<&Turn> = look
                .turns
                .iter()
                .filter(|t| self.due(t.route.about) && !self.turns.contains(&t.route.about))
                .collect();
            for turn in fresh {
                let note = Note {
                    id: turn.route.about.note_id(),
                    title: turn.title.clone(),
                    body: turn.body.clone(),
                    info: turn.route.info(),
                    ..Note::default()
                };
                self.post(turn.route.about, Why::Finished, note);
            }
        }
        // A turn read since, here or on another device, takes its note back.
        let read: Vec<About> = self.turns.difference(&turns).copied().collect();
        for about in read {
            if self.posted.get(&about) == Some(&Why::Finished) {
                self.posted.remove(&about);
                self.notifier.withdraw(&about.note_id());
            }
        }
        self.turns = turns;
        if self.badge != Some(look.unread) {
            self.badge = Some(look.unread);
            self.notifier.set_badge(look.unread);
        }
    }

    /// A shell command ended in `route`'s session after running `done.elapsed`: it notifies when
    /// that is at least `slow` and the session is in the background. `title` is the tile's name.
    pub fn command_finished(
        &mut self,
        route: Route,
        title: String,
        done: &Finished,
        slow: Duration,
    ) {
        if !self.due(route.about) || done.elapsed < slow {
            return;
        }
        let command = done.command.lines().next().unwrap_or_default().trim();
        let body = if command.is_empty() {
            done.label()
        } else {
            format!("{command} \u{b7} {}", done.label())
        };
        let note =
            Note { id: route.about.note_id(), title, body, info: route.info(), ..Note::default() };
        self.post(route.about, Why::Finished, note);
    }

    /// A program in `route`'s session asked for a desktop notification: it notifies while its
    /// session is in the background. On screen, the program's own output is enough.
    pub fn program(&mut self, route: Route, title: String, body: String) {
        if !self.due(route.about) {
            return;
        }
        let note =
            Note { id: route.about.note_id(), title, body, info: route.info(), ..Note::default() };
        self.post(route.about, Why::Program, note);
    }

    fn post(&mut self, about: About, why: Why, mut note: Note) {
        if !self.enabled {
            return;
        }
        tracing::debug!(?about, ?why, "attention note");
        // Only an agent that needs the person breaks through a Focus.
        note.urgent = why == Why::Asks;
        self.posted.insert(about, why);
        self.send(note);
    }
}

impl WorkspaceView {
    /// What notifications follow now: the agents waiting on the human, with their thread's
    /// name and what they ask, and the badge's count: what needs the person and what failed.
    #[must_use]
    pub fn attention_look(&self) -> Look {
        let asking = self
            .threads_waiting()
            .into_iter()
            .filter_map(|w| {
                let stand = self.thread_stand(w.thread)?;
                let body = agent_ask_text(stand).unwrap_or_else(|| agent_status_word(stand));
                let route = Route { worker: w.worker, item: None, about: About::Thread(w.thread) };
                let title = self.thread_title(w.thread);
                let fresh = !self.waited_when_heard(w.thread);
                Some(Asking { route, title, body, fresh })
            })
            .collect();
        let turns = self
            .agent_turns()
            .filter_map(|thread| {
                let about = About::Thread(thread);
                let worker = self.thread_stand(thread)?.worker;
                let (route, title) =
                    (Route { worker, item: None, about }, self.thread_title(thread));
                let done = self.finished.get(&about)?;
                // One found unread as its worker was first heard was told by whoever heard it.
                if !done.turn.is_some_and(|t| t.fresh) {
                    return None;
                }
                let command = done.command.lines().next().unwrap_or_default().trim();
                let body = format!("{command} \u{b7} {}", done.label());
                Some(Turn { route, title, body })
            })
            .collect();
        let failed = self.threads_on(slopty_proto::thread::attention::Rung::Failed).len();
        Look { asking, turns, unread: self.needs_you_count().saturating_add(failed) }
    }

    /// The note the server's `notice` makes here; `None` for a finished turn shorter than the
    /// slow-command time.
    #[must_use]
    pub fn heard(&self, notice: &Notice) -> Option<Heard> {
        if notice.kind == NoticeKind::Finished
            && notice.worked_ms.is_some_and(|ms| Duration::from_millis(ms) < self.slow_command)
        {
            return None;
        }
        // A program's records are posted here as they come; the server only pushes them.
        let Subject::Thread(at) = &notice.about else { return None };
        let worker = super::keys::worker_key(at.worker);
        let route = Route { worker, item: None, about: About::Thread(at.thread) };
        let title = Some(notice.title.trim())
            .filter(|t| !t.is_empty())
            .map_or_else(|| self.route_title(route), str::to_owned);
        let body = notify::pushed::notice_body(notice);
        Some(Heard { route, kind: notice.kind, title, body })
    }

    /// Where a note about `session` leads, and the name its title says; `None` for a session
    /// no worker here runs.
    #[must_use]
    pub fn attention_route(&self, session: SessionId) -> Option<(Route, String)> {
        let worker = self.worker_of_session(session)?;
        let route = Route { worker, item: None, about: About::Session(session) };
        Some((route, self.route_title(route)))
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

    /// The chat a tapped note is about, as the interface knows it: its agent's thread; `None`
    /// for a note about a terminal.
    #[must_use]
    pub fn tapped_chat(tap: &Tap) -> Option<String> {
        let About::Thread(thread) = Route::of_tap(tap)?.about else { return None };
        Some(thread.to_string())
    }

    /// What a note's title says: its agent's thread's name, else the worker's.
    #[must_use]
    pub fn route_title(&self, route: Route) -> String {
        let thread = match route.about {
            About::Session(_) => None,
            About::Thread(thread) => Some(thread),
        };
        thread
            .map(|thread| self.thread_title(thread))
            .or_else(|| self.workers.get(&route.worker).map(|w| w.name.clone()))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use slopty_core::{SessionId, WorkerId};
    use slopty_platform::notify::Memory;

    use super::{About, Asking, Attention, Look, Route};

    fn route(session: SessionId) -> Route {
        Route {
            worker: crate::workspace::worker_key(WorkerId::new()),
            item: None,
            about: About::Session(session),
        }
    }

    /// What the workspace shows with `waiting` asking the person.
    fn asking(waiting: &[SessionId]) -> Look {
        let asking = waiting
            .iter()
            .map(|session| Asking {
                route: route(*session),
                title: "claude".to_owned(),
                body: "Allow Bash?".to_owned(),
                fresh: true,
            })
            .collect();
        Look { asking, turns: Vec::new(), unread: waiting.len() }
    }

    /// slopty: in front, the session on screen never notifies and one in the background does;
    /// its note goes once the person looks at it. Away, every session notifies.
    #[test]
    fn the_session_on_screen_stays_quiet_and_one_behind_it_notifies() {
        let memory = Rc::new(Memory::default());
        let mut attention = Attention::new(Rc::<Memory>::clone(&memory));
        let (shown, behind) = (SessionId::new(), SessionId::new());
        attention.set_on_screen(Some(About::Session(shown)));
        attention.look(&asking(&[shown]));
        assert!(memory.posted().is_empty(), "the session on screen asks quietly");
        attention.look(&asking(&[shown, behind]));
        let posted = memory.posted();
        assert_eq!(posted.len(), 1, "the one behind it notifies");
        assert_eq!(posted[0].id, About::Session(behind).note_id());
        assert!(posted[0].urgent, "a need breaks through a Focus");

        attention.set_on_screen(Some(About::Session(behind)));
        assert_eq!(memory.withdrawn(), [About::Session(behind).note_id()], "looked at, it goes");

        attention.set_active(false);
        attention.program(route(behind), "make".to_owned(), "done".to_owned());
        assert_eq!(memory.posted().len(), 2, "away, even the session on screen notifies");
        attention.set_active(true);
        assert_eq!(memory.withdrawn().len(), 2, "back in front, the one on screen goes again");
    }

    /// slopty: a wait already under way when its worker was first heard here is no moment: the
    /// list shows it, and no note says it; one that starts after notifies.
    #[test]
    fn a_wait_found_under_way_posts_no_note() {
        let memory = Rc::new(Memory::default());
        let mut attention = Attention::new(Rc::<Memory>::clone(&memory));
        let (found, started) = (SessionId::new(), SessionId::new());
        attention.set_active(false);
        let mut look = asking(&[found]);
        look.asking[0].fresh = false;
        attention.look(&look);
        assert!(memory.posted().is_empty(), "found waiting, it says nothing");
        let mut look = asking(&[found, started]);
        look.asking[0].fresh = false;
        attention.look(&look);
        let posted = memory.posted();
        assert_eq!(posted.len(), 1, "a wait that starts after notifies");
        assert_eq!(posted[0].id, About::Session(started).note_id());
    }

    /// slopty: back in front, a note pointing at a session in the background stays until that
    /// session is looked at or answered; a pushed one whose moment passed goes.
    #[test]
    fn coming_back_keeps_the_notes_that_still_point_somewhere() {
        let memory = Rc::new(Memory::default());
        let mut attention = Attention::new(Rc::<Memory>::clone(&memory));
        let (shown, behind, pushed) = (SessionId::new(), SessionId::new(), SessionId::new());
        attention.set_active(false);
        attention.look(&asking(&[shown, behind]));
        assert_eq!(memory.posted().len(), 2, "away, both notify");

        attention.set_on_screen(Some(About::Session(shown)));
        attention.set_active(true);
        assert_eq!(
            memory.withdrawn(),
            [About::Session(shown).note_id()],
            "only the one on screen goes"
        );
        attention.delivered(&[About::Session(behind).note_id(), About::Session(pushed).note_id()]);
        assert_eq!(
            memory.withdrawn()[1..],
            [About::Session(pushed).note_id()],
            "a push whose moment passed goes; a live ask's stays"
        );

        attention.look(&asking(&[shown]));
        assert_eq!(
            memory.withdrawn().last(),
            Some(&About::Session(behind).note_id()),
            "answered, it goes"
        );
    }

    /// The notes' switch off takes back every note up and posts none while away; on again,
    /// notes post as before.
    #[test]
    fn notes_switched_off_go_and_post_none_until_switched_on() {
        let memory = Rc::new(Memory::default());
        let mut attention = Attention::new(Rc::<Memory>::clone(&memory));
        let (first, second) = (SessionId::new(), SessionId::new());
        attention.set_active(false);
        attention.program(route(first), "make".to_owned(), "done".to_owned());
        assert_eq!(memory.posted().len(), 1, "away, a program's note posts");

        attention.set_enabled(false);
        assert_eq!(memory.withdrawn(), [About::Session(first).note_id()], "the note up goes");
        attention.program(route(second), "make".to_owned(), "done".to_owned());
        assert_eq!(memory.posted().len(), 1, "switched off, nothing posts");

        attention.set_enabled(true);
        attention.program(route(second), "make".to_owned(), "done".to_owned());
        assert_eq!(memory.posted().len(), 2, "switched on, notes post again");
    }
}
