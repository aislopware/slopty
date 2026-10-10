//! Attention on a pocketed phone: which moments become a system notification while the app is
//! not in front, and where a tapped one leads.
//!
//! Four moments notify. An agent starts to need the human (a permission, a question, input it
//! asks for), a program's status record starts to wait on them (`OSC 7501`'s `blocked`, said as
//! an agent's need is and taken back the same way), an agent's turn that ran at least the
//! slow-command time ends, or a shell command that ran that long ends. Nothing notifies while
//! the app is in front, since the bell and the navigator say it there. A tile has at most
//! one notification up: the note's identifier is its session's, so a newer one replaces the
//! older, and an agent answered anywhere takes its own back. Coming back to the app takes back
//! every note it posted, because the navigator now shows the same things. The icon badge is the
//! bell's count.
//!
//! An agent that waits on a yes or no its thread's row shows (`approvals`) is posted with the
//! approval buttons (`notify::APPROVAL`): "Allow" and "Deny" answer it where the note is,
//! "Show" opens its tile as a tap does. The row shows the request a moment after the agent's
//! status says it waits, so the note already up is replaced, silently, once the request comes,
//! and again without the buttons once it is gone (the terminal asks by then). A request this
//! client answered takes its note away instead: the agent still reads as waiting until its
//! worker's table moves past it, and the answer is not news.
//!
//! Nothing notifies either while the person is at another of their devices
//! ([`Attention::set_present_elsewhere`]): at the Mac, the phone in their pocket stays quiet, and
//! what it had up is taken back, since the Mac in front of them says it.
//!
//! Linked to a server, the server decides ([`Attention::set_server_led`]): it ranks every
//! thread on one ladder and picks the clients a moment goes to by where the person is, so two
//! devices never both say it. Its notices are then the only agent moments that post here
//! ([`Attention::notice`], made by [`WorkspaceView::heard`]); the workspace's own look still
//! adds the approval buttons to a note up, takes back what was answered, and keeps the badge.
//! A shell's moments and a program's records are this client's own and post as before: the
//! server never hears a program's records.
//!
//! A note of an agent that needs the person is Time Sensitive (`Note::urgent`), so it reaches
//! them through a Focus and past the notification summary; a finished turn, a failure, a
//! shell's command and a program's own note wait there like any other app's.
//!
//! A project's held-up work comes only as the server's notice ([`Heard::stack`]): each is a
//! note of its own, stacked with the project's others, leading to its orchestrator. With the
//! app in front the app says it as a notice instead.
//!
//! With notifications turned off in the system's settings, a note is dropped where nobody sees
//! it. So the first time one goes unsaid in a run, coming back to the app says notifications
//! are off ([`Attention::unsaid_while_off`], [`NOTES_OFF`]), once.
//!
//! [`Attention`] decides and hands what it decided to a [`Notifier`]; the app owns one and
//! feeds it a [`Look`] after every change of the workspace, and each finished command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{Context, Entity};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_platform::notify::info::{ASK, ITEM, SESSION, THREAD, WORKER};
use slopty_platform::notify::{self, APPROVAL, Alerts, Note, Notifier, REPLYING, Tap};
use slopty_proto::items::ItemKind;
use slopty_proto::project::ProjectId;
use slopty_proto::thread::attention::{Notice, NoticeKind, Subject};
use slopty_proto::thread::{AskId, ThreadId};

use super::agents::{agent_ask_text, agent_status_word};
use super::approvals::answerable;
use super::{Finished, WorkspaceView};
use crate::terminal::TerminalView;

/// What a note's answer says when its request was no longer open: answered elsewhere, or the
/// terminal asks by now.
pub(super) const NO_LONGER_WAITING: &str = "That prompt is no longer waiting";

/// How long after a program's record sounded or notified another of the same program's may.
///
/// A program is any program, and one that flips between waiting and working would otherwise
/// sound with each flip.
pub const PROGRAM_QUIET: Duration = Duration::from_secs(10);

/// Whether `tap` is a note's "Allow", "Deny" or reply, which answers where the note is and may
/// have woken the app in the background to do it.
#[must_use]
pub fn answers(tap: &Tap) -> bool {
    matches!(tap.action.as_deref(), Some(notify::ALLOW | notify::DENY | notify::REPLY))
}

/// What a note is about: a terminal, whose agent or shell it speaks of, or a thread driven
/// over a protocol with no terminal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum About {
    /// A terminal's session.
    Session(SessionId),
    /// A thread with no terminal.
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
    /// The yes or no the note's buttons answer, as the note carries it: the request's id on the
    /// thread the agent runs.
    pub approval: Option<String>,
    /// The request this client answered that its worker's table still shows open.
    pub answered: Option<String>,
    /// This client's own moment: a program waiting on the person (`OSC 7501`), which no
    /// server's ladder ranks, so it posts here even while a server leads.
    pub own: bool,
}

impl Asking {
    /// Its note: the approval buttons while a yes or no is open, and a sound unless `silent`.
    fn note(&self, silent: bool) -> Note {
        let mut info = self.route.info();
        if let Some(ask) = &self.approval {
            info.insert(ASK.to_owned(), ask.clone());
        }
        Note {
            id: self.route.about.note_id(),
            title: self.title.clone(),
            body: self.body.clone(),
            info,
            // A program's own wait has no agent to reply to.
            category: if self.approval.is_some() {
                Some(APPROVAL)
            } else {
                (!self.own).then_some(REPLYING)
            },
            silent,
            urgent: true,
            thread: None,
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
    /// The agents whose turn finished unwatched and is unread.
    pub turns: Vec<Turn>,
    /// The bell's count: the icon badge.
    pub unread: usize,
    /// The project each terminal and thread is in, by its group's key: the thread its notes
    /// stack in, so one project's notes sit together.
    pub projects: HashMap<About, String>,
    /// The projects muted in the navigator, by their group's key: their moments post nothing.
    pub muted: HashSet<String>,
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
    /// A project's notice: a note of its own, stacked with the project's others.
    pub stack: Option<Stack>,
}

/// Where a project's note sits: its own identifier, and the project its notes stack under.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Stack {
    /// The note's identifier, one per timeline entry.
    pub id: String,
    /// The project.
    pub project: String,
}

/// Why a note is up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Why {
    Asks,
    Finished,
    Program,
    Unanswered,
}

/// How long a tapped note waits for its machine's link: a cold launch dials every worker at
/// once, and a link that takes longer than this is not what the person is still waiting on.
pub const PARKED_FOR: Duration = Duration::from_secs(30);

/// What a tapped note says while its machine is still being dialled.
#[must_use]
pub fn connecting(machine: Option<&str>) -> String {
    machine.map_or_else(|| "Connecting…".to_owned(), |name| format!("Connecting to {name}…"))
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
    /// The prompt or request each agent's note up answers.
    answers: HashMap<About, Option<String>>,
    /// The badge last set.
    badge: Option<usize>,
    /// The project each terminal and thread was in at the last look.
    projects: HashMap<About, String>,
    /// The projects muted at the last look, by their group's key.
    muted: HashSet<String>,
    /// The projects' notes up, by identifier.
    project_notes: HashSet<String>,
    /// A note went out while notifications were off, since the app last came back.
    unsaid: bool,
    /// The app has said this run that notifications are off.
    said_off: bool,
    /// The app still hears the server's notices on its link: it has not told the server it is
    /// about to be suspended, after which the server pushes instead.
    listening: bool,
    /// When each program's record last posted a note: at most once in [`PROGRAM_QUIET`].
    own_posted: HashMap<About, Instant>,
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
            elsewhere: false,
            server_led: false,
            asking: HashSet::new(),
            turns: HashSet::new(),
            posted: HashMap::new(),
            answers: HashMap::new(),
            badge: None,
            projects: HashMap::new(),
            muted: HashSet::new(),
            project_notes: HashSet::new(),
            unsaid: false,
            said_off: false,
            listening: true,
            own_posted: HashMap::new(),
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

    /// The app came to the front or left it. Coming back takes back every note it put up but a
    /// live ask's, which stays to be answered from the Notification Centre and goes once it is
    /// answered. The notes pushed while it was away are the system's to list
    /// ([`Self::delivered`]).
    pub fn set_active(&mut self, active: bool) {
        if active && !self.active {
            let asking = &self.asking;
            let (kept, stale): (Vec<_>, Vec<_>) = self
                .posted
                .drain()
                .partition(|(about, why)| *why == Why::Asks && asking.contains(about));
            for (about, _) in stale {
                self.notifier.withdraw(&about.note_id());
            }
            for id in self.project_notes.drain() {
                self.notifier.withdraw(&id);
            }
            self.answers.retain(|about, _| kept.iter().any(|(k, _)| k == about));
            self.posted.extend(kept);
        }
        self.active = active;
    }

    /// The notes the system shows now, by identifier, as it listed them once the app came back:
    /// the ones a push put up while the app was away, which it never posted, go as its own do,
    /// but a live ask's, which stays and goes once answered. Nothing goes while the app is away
    /// again by the time the list came.
    pub fn delivered(&mut self, ids: &[String]) {
        if !self.active {
            return;
        }
        for id in ids {
            if let Some(about) = self.asking.iter().find(|a| a.note_id() == *id) {
                self.posted.entry(*about).or_insert(Why::Asks);
            } else if !self.posted.keys().any(|about| about.note_id() == *id) {
                self.notifier.withdraw(id);
            }
        }
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

    /// Whether the server's notices decide which agent moments post and sound.
    #[must_use]
    pub const fn server_led(&self) -> bool {
        self.server_led
    }

    /// The server picked this client to say `heard`: posted while the app is not in front,
    /// where the navigator already says it. A wait's note gets its approval buttons from the next
    /// [`Self::look`].
    pub fn notice(&mut self, heard: &Heard) {
        if self.active || !self.listening {
            return;
        }
        if let Some(stack) = &heard.stack {
            let key = slopty_client::groups::GroupKey::new(
                slopty_client::groups::fact::PROJECT,
                &stack.project,
            );
            if self.muted.contains(key.as_str()) {
                return;
            }
            tracing::debug!(id = stack.id, "project note");
            self.project_notes.insert(stack.id.clone());
            self.send(Note {
                id: stack.id.clone(),
                title: heard.title.clone(),
                body: heard.body.clone(),
                info: heard.route.info(),
                thread: Some(stack.project.clone()),
                urgent: heard.kind == NoticeKind::NeedsYou,
                ..Note::default()
            });
            return;
        }
        let session = heard.route.about;
        let note = Note {
            id: session.note_id(),
            title: heard.title.clone(),
            body: heard.body.clone(),
            info: heard.route.info(),
            category: Some(REPLYING),
            ..Note::default()
        };
        let why = match heard.kind {
            NoticeKind::NeedsYou => Why::Asks,
            NoticeKind::Failed | NoticeKind::Finished | NoticeKind::Project => Why::Finished,
        };
        self.post(session, why, note);
        if why == Why::Asks {
            self.answers.insert(session, None);
        }
    }

    /// Whether a program's record in `about` may post a note now: none posted in the last
    /// [`PROGRAM_QUIET`]. Noted as posting when it may.
    fn own_due(&mut self, about: About) -> bool {
        let now = Instant::now();
        self.own_posted.retain(|_, at| now.saturating_duration_since(*at) < PROGRAM_QUIET);
        let quiet = self
            .own_posted
            .get(&about)
            .is_some_and(|at| now.saturating_duration_since(*at) < PROGRAM_QUIET);
        if !quiet {
            self.own_posted.insert(about, now);
        }
        !quiet
    }

    /// Whether a moment is worth a note now: the app is not in front, and the person is not
    /// at another device.
    const fn away(&self) -> bool {
        !self.active && !self.elsewhere
    }

    fn withdraw_all(&mut self) {
        for (about, _) in self.posted.drain() {
            self.notifier.withdraw(&about.note_id());
        }
        for id in self.project_notes.drain() {
            self.notifier.withdraw(&id);
        }
        self.answers.clear();
    }

    /// The workspace changed: an agent that has just started to wait notifies while the app is
    /// away, one that stopped takes its note back, and the badge follows the bell.
    pub fn look(&mut self, look: &Look) {
        self.projects.clone_from(&look.projects);
        self.muted.clone_from(&look.muted);
        let now: HashSet<About> = look.asking.iter().map(|a| a.route.about).collect();
        if self.away() {
            for asking in &look.asking {
                let session = asking.route.about;
                let up = self.posted.get(&session) == Some(&Why::Asks);
                let was = self.answers.get(&session).cloned().flatten();
                if !self.asking.contains(&session) && (!self.server_led || asking.own) {
                    if asking.own && !self.own_due(session) {
                        continue;
                    }
                    self.post(session, Why::Asks, asking.note(false));
                } else if up && asking.approval.is_none() && was.is_some() && was == asking.answered
                {
                    self.posted.remove(&session);
                    self.notifier.withdraw(&session.note_id());
                    self.answers.insert(session, None);
                    continue;
                } else if up && self.answers.get(&session) != Some(&asking.approval) {
                    self.post(session, Why::Asks, asking.note(true));
                } else {
                    continue;
                }
                self.answers.insert(session, asking.approval.clone());
            }
        }
        let stopped: Vec<About> = self.asking.difference(&now).copied().collect();
        for session in stopped {
            if self.posted.get(&session) == Some(&Why::Asks) {
                self.posted.remove(&session);
                self.notifier.withdraw(&session.note_id());
            }
            self.answers.remove(&session);
        }
        self.asking = now;
        let turns: HashSet<About> = look.turns.iter().map(|t| t.route.about).collect();
        if self.away() && !self.server_led {
            let fresh: Vec<&Turn> =
                look.turns.iter().filter(|t| !self.turns.contains(&t.route.about)).collect();
            for turn in fresh {
                let note = Note {
                    id: turn.route.about.note_id(),
                    title: turn.title.clone(),
                    body: turn.body.clone(),
                    info: turn.route.info(),
                    category: Some(REPLYING),
                    ..Note::default()
                };
                self.post(turn.route.about, Why::Finished, note);
            }
        }
        self.turns = turns;
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
        if !self.away() || done.elapsed < slow {
            return;
        }
        let command = super::tile::command_words(&done.command);
        let body = if command.is_empty() {
            done.label()
        } else {
            format!("{command} \u{b7} {}", done.label())
        };
        let note =
            Note { id: route.about.note_id(), title, body, info: route.info(), ..Note::default() };
        self.post(route.about, Why::Finished, note);
    }

    /// A program in `route`'s session asked for a desktop notification: it notifies while the
    /// app is away. In front, the tile's own attention mark is enough.
    pub fn program(&mut self, route: Route, title: String, body: String) {
        if !self.away() {
            return;
        }
        let note =
            Note { id: route.about.note_id(), title, body, info: route.info(), ..Note::default() };
        self.post(route.about, Why::Program, note);
    }

    /// A note's "Allow" or "Deny" for `route`'s agent found no prompt to answer (`why`): said
    /// in a note of its own while the app is away. `title` is the tile's name.
    pub fn unanswered(&mut self, route: Route, title: String, why: &str) {
        if !self.away() {
            return;
        }
        let note = Note {
            id: route.about.note_id(),
            title,
            body: why.to_owned(),
            info: route.info(),
            ..Note::default()
        };
        self.post(route.about, Why::Unanswered, note);
    }

    fn post(&mut self, about: About, why: Why, mut note: Note) {
        note.thread = self.projects.get(&about).cloned();
        // A muted project's moments post nothing; the bell and the navigator still say them.
        if note.thread.as_ref().is_some_and(|project| self.muted.contains(project)) {
            return;
        }
        tracing::debug!(?about, ?why, "attention note");
        // Only an agent that needs the person breaks through a Focus.
        note.urgent = why == Why::Asks;
        if why != Why::Asks {
            self.answers.remove(&about);
        }
        self.posted.insert(about, why);
        self.send(note);
    }
}

impl WorkspaceView {
    /// What notifications follow now: the agents waiting on the human, with their tile's name
    /// and what they ask, and the bell's count.
    #[must_use]
    pub fn attention_look(&self) -> Look {
        let asking = self
            .needs_you()
            .into_iter()
            .filter_map(|w| {
                let item = w.tile.map(|t| t.item);
                let route = Route { worker: w.worker, item, about: About::Session(w.session) };
                let Some(agent) = self.agent_state(w.session) else {
                    // A program waiting on the person, in its record's words.
                    let body = self
                        .program_words(w.session)
                        .or_else(|| self.program_need_word(w.session))?;
                    let title = self.route_title(route);
                    return Some(Asking {
                        route,
                        title,
                        body,
                        approval: None,
                        answered: None,
                        own: true,
                    });
                };
                let body = agent_ask_text(agent).unwrap_or_else(|| agent_status_word(agent));
                let approval = self
                    .session_request(w.session)
                    .filter(|a| answerable(a))
                    .map(|a| a.id.0.clone());
                let answered = self
                    .session_thread(w.session)
                    .and_then(|t| self.thread_answered_here(t))
                    .map(|ask| ask.0.clone());
                let title = self.route_title(route);
                Some(Asking { route, title, body, approval, answered, own: false })
            })
            .chain(self.threads_waiting().into_iter().filter_map(|w| {
                let stand = self.thread_stand(w.thread)?;
                let asks = stand.asks.as_ref().map(|a| crate::markdown::plain_line(&a.title));
                let body = asks
                    .filter(|a| !a.trim().is_empty())
                    .or_else(|| stand.word().map(str::to_owned))
                    .unwrap_or_default();
                let item = w.tile.map(|t| t.item);
                let route = Route { worker: w.worker, item, about: About::Thread(w.thread) };
                let title = self.thread_title(w.thread);
                let approval =
                    stand.asks.as_ref().filter(|a| answerable(a)).map(|a| a.id.0.clone());
                let answered = self.thread_answered_here(w.thread).map(|ask| ask.0.clone());
                Some(Asking { route, title, body, approval, answered, own: false })
            }))
            .collect();
        let turns = self
            .agent_turns()
            .filter_map(|about| {
                let (route, title) = match about {
                    About::Session(session) => self.attention_route(session)?,
                    About::Thread(thread) => {
                        let worker = self.thread_stand(thread)?.worker;
                        let item = self.tile_of_thread(thread).map(|t| t.item);
                        (Route { worker, item, about }, self.thread_title(thread))
                    }
                };
                let done = self.finished.get(&about)?;
                let command = super::tile::command_words(&done.command);
                let body = format!("{command} \u{b7} {}", done.label());
                Some(Turn { route, title, body })
            })
            .collect();
        let muted = self.navigator().muted.iter().map(|k| k.as_str().to_owned()).collect();
        Look { asking, turns, unread: self.bell_count(), projects: self.note_projects(), muted }
    }

    /// The project each terminal and thread is in, by its group's key: a tile's group (its
    /// machine's where it is in no project), and a thread with no tile here where the navigator
    /// lists it.
    fn note_projects(&self) -> HashMap<About, String> {
        let projects = self.project_groups();
        let mut out = HashMap::new();
        for (tile, group) in
            projects.tiles.iter().filter_map(|t| Some((*t, projects.group_of(*t)?)))
        {
            let about = match self.item(tile).map(|i| &i.kind) {
                Some(ItemKind::Terminal { session }) => About::Session(*session),
                Some(ItemKind::Thread { thread }) => About::Thread(*thread),
                _ => continue,
            };
            out.insert(about, group.key.as_str().to_owned());
        }
        let claims = self.claims();
        for (thread, stand) in self.thread_stands() {
            if out.contains_key(&About::Thread(thread)) {
                continue;
            }
            let facts = self.thread_listing_facts(stand.worker, thread);
            let key = super::grouping::listing_group(&projects, &claims, &facts)
                .map_or_else(|| slopty_client::groups::GroupKey::machine(stand.worker), |g| g.key);
            out.insert(About::Thread(thread), key.as_str().to_owned());
        }
        out
    }

    /// The note the server's `notice` makes here; `None` for a finished turn shorter than the
    /// slow-command time. A thread with no terminal leads to its own tile.
    #[must_use]
    pub fn heard(&self, notice: &Notice) -> Option<Heard> {
        if notice.kind == NoticeKind::Finished
            && notice.worked_ms.is_some_and(|ms| Duration::from_millis(ms) < self.slow_command)
        {
            return None;
        }
        let at = match &notice.about {
            Subject::Thread(at) => at,
            Subject::Project { project, entry } => {
                return self.heard_project(notice, project, *entry);
            }
            // A program's records are posted here as they come; the server only pushes them.
            Subject::Terminal(_) => return None,
        };
        let worker = super::projects::worker_key(at.worker);
        let (about, item) = if let Some(tile) = notice.tile {
            (About::Session(tile.session), self.tile_of_session(tile.session).map(|t| t.item))
        } else {
            (About::Thread(at.thread), self.tile_of_thread(at.thread).map(|t| t.item))
        };
        let route = Route { worker, item, about };
        let title = Some(notice.title.trim())
            .filter(|t| !t.is_empty())
            .map_or_else(|| self.route_title(route), str::to_owned);
        let body = notify::pushed::notice_body(notice);
        Some(Heard { route, kind: notice.kind, title, body, stack: None })
    }

    /// A project's notice said in the workspace while the app is in front: the task and how
    /// it stands, as the server words it, named by its project only while that project's
    /// orchestrator is not on the workspace in view, where the breadcrumb already names it.
    pub fn say_project_notice(&mut self, heard: &Heard, cx: &mut Context<Self>) {
        let route = heard.route;
        let in_view = route.item.is_some_and(|item| {
            let tile = TileRef { worker: route.worker, item };
            let shown = self.layout.shown_index();
            self.layout.position(tile).is_some_and(|at| Some(at.project) == shown)
        });
        let text = if in_view || heard.title.trim().is_empty() {
            heard.body.clone()
        } else {
            format!("{}: {}", heard.title, heard.body)
        };
        self.show_notice(text, cx);
    }

    /// A project's notice, at `entry` of `project`'s timeline: a note that leads to its
    /// orchestrator's terminal, stacked with the project's others. `None` for a project with no
    /// orchestrator to lead to.
    fn heard_project(&self, notice: &Notice, project: &ProjectId, entry: u64) -> Option<Heard> {
        let tile = notice.tile?;
        let route = self.attention_route(tile.session).map_or_else(
            || Route {
                worker: super::projects::worker_key(tile.worker),
                item: None,
                about: About::Session(tile.session),
            },
            |(route, _)| route,
        );
        Some(Heard {
            route,
            kind: notice.kind,
            title: notice.title.clone(),
            body: notice.text.clone(),
            stack: Some(Stack {
                id: format!("project-{project}-{entry}"),
                project: project.as_str().to_owned(),
            }),
        })
    }

    /// Where a note about `session` leads, and the name its title says; `None` for a session
    /// with no tile here.
    #[must_use]
    pub fn attention_route(&self, session: SessionId) -> Option<(Route, String)> {
        let tile = self.tile_of_session(session)?;
        let route =
            Route { worker: tile.worker, item: Some(tile.item), about: About::Session(session) };
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

    /// The human tapped a note: focus the tile it names and give it the keyboard. A note without
    /// a route (one another part of the app posted, tagged by its session) reveals that session.
    /// An agent with no tile here gets one. An approval note's "Allow" or "Deny" answers its
    /// prompt and a reply goes to its agent, each leaving the workspace where it is; "Show" is a
    /// tap.
    pub fn open_notification(&mut self, tap: &Tap, cx: &mut Context<Self>) {
        let route = Route::of_tap(tap);
        tracing::debug!(id = tap.id, ?route, action = ?tap.action, "note opened");
        if tap.action.as_deref() == Some(notify::REPLY) {
            match (route, tap.text.as_deref().map(str::trim).filter(|t| !t.is_empty())) {
                (Some(route), Some(text)) => self.reply_tapped(route, text.to_owned(), cx),
                _ => self.settle_taps(cx),
            }
            return;
        }
        let allow = match tap.action.as_deref() {
            Some(notify::ALLOW) => Some(true),
            Some(notify::DENY) => Some(false),
            Some(_) | None => None,
        };
        if let Some(allow) = allow {
            match (route, tap.info.get(ASK)) {
                (Some(route), Some(ask)) => {
                    self.verdict_tapped(route, AskId(ask.clone()), allow, cx);
                }
                // Nothing to answer, but the tap still settles: the system waits on the app's
                // word that it is done with it.
                _ => self.settle_taps(cx),
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
        if !self.leads_here(route) && !self.worker_ready(route.worker) {
            let name = self.workers.get(&route.worker).map(|w| w.name.clone());
            self.parked_tap = Some((tap.clone(), self.clock_instant()));
            self.show_notice(connecting(name.as_deref()), cx);
            return;
        }
        match (tile.and_then(|t| self.item(t)).map(|i| i.kind.clone()), route.about) {
            (Some(kind), _) => {
                if let Some(item) = route.item {
                    self.go_to(item, cx);
                }
                if let ItemKind::Terminal { session } = kind {
                    self.pending_focus = Some(session);
                }
            }
            (None, About::Thread(thread)) => self.open_thread(route.worker, thread, cx),
            (None, About::Session(session)) if self.tile_of_session(session).is_some() => {
                self.reveal_session(session, cx);
            }
            (None, About::Session(session)) => self.show_untiled(route.worker, session, cx),
        }
    }

    /// Whether `route` leads to a tile this device already has: one it names, or one of its
    /// session or thread. Such a tap needs no link to come forward.
    fn leads_here(&self, route: Route) -> bool {
        let tile = route.item.map(|item| TileRef { worker: route.worker, item });
        tile.and_then(|t| self.item(t)).is_some()
            || match route.about {
                About::Session(session) => self.tile_of_session(session).is_some(),
                About::Thread(thread) => self.tile_of_thread(thread).is_some(),
            }
    }

    /// Whether `key` is linked and has sent what it holds: what a tap that makes a tile there
    /// needs.
    fn worker_ready(&self, key: WorkerKey) -> bool {
        self.workers.get(&key).is_some_and(|w| w.link.is_some() && !w.awaiting_snapshot)
    }

    /// `key` has sent what it holds: a tap parked for it, while it was still being dialled,
    /// goes where it leads now, unless it waited past [`PARKED_FOR`].
    pub(super) fn run_parked_tap(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some((tap, at)) = self.parked_tap.take() else { return };
        if Route::of_tap(&tap).is_none_or(|r| r.worker != key) {
            self.parked_tap = Some((tap, at));
            return;
        }
        if self.clock_instant().saturating_duration_since(at) > PARKED_FOR {
            tracing::debug!(id = tap.id, "a parked tap waited too long");
            return;
        }
        self.open_notification(&tap, cx);
    }

    /// What a note's title says: the tile's name, else the worker's.
    #[must_use]
    pub fn route_title(&self, route: Route) -> String {
        route
            .item
            .map(|item| TileRef { worker: route.worker, item })
            .and_then(|tile| self.item(tile).map(|i| self.tile_title(i)))
            .or_else(|| self.workers.get(&route.worker).map(|w| w.name.clone()))
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "tests/attention.rs"]
mod tests;
