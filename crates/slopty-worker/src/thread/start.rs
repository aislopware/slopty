//! Starting an agent: its own TUI in a terminal ptyd keeps (`docs/decisions/agents.md`,
//! "TUI-first").
//!
//! The person's own program runs as they would run it, in the start's folder (or the worktree
//! made for it), with Slopty's tools through the agent's own config door:
//!
//! - **Claude Code** on a session Slopty names (`--session-id`, or `--resume` to take one up
//!   again), with only the choices the start made, its first prompt, Slopty's tools
//!   (`--mcp-config`) and Slopty's hook relay (`--settings`, merged with the caller's). Its thread
//!   is the session's, which [`super::claude`] reads from then on.
//! - **Codex** and **pi** as their own TUIs with Slopty's tools (Codex's `-c mcp_servers.…`, pi's
//!   tools extension). Their thread is a row only: its title, its terminal, live while that runs.
//!
//! The thread names its terminal ([`ThreadMeta::TERMINAL_FACT`]); the answer is
//! `Outcome::Started` with it. A start is acted on once per intent ([`Host::start`]'s record).
//! One whose terminal does not open takes back what it did, and only that: a thread it made is
//! gone, and one held already names again what it named before, unless its observer named the
//! terminal its session runs in meanwhile.
//!
//! The terminal keeps what started it ([`Started`]). When its agent has ended and the person
//! starts the terminal again ([`Starter::again`]), the agent takes its thread up again in its own
//! words, with the choices the start made and at the same seat, and is told nothing: what it was
//! first told is in its conversation. A Codex thread Slopty began names no Codex session it
//! knows, so Codex's own list of the folder's sessions opens, for the person to pick.

use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use slopty_agent::claude::{self as agent, Begin, Tui};
use slopty_core::{SessionId, WallMs};
use slopty_proto::terminal::OpenSession;
use slopty_proto::thread::wire::{Outcome, Start};
use slopty_proto::thread::{
    Action, AgentId, Drive, IntentId, Liveness, Phase, Status, ThreadId, ThreadMeta,
};

use super::claude::{Claude, Place};
use super::{Host, Seated};
use crate::restore::Started;

/// What a start opens its terminal through: the worker's sessions, announced to every client as
/// any terminal is.
pub trait Terminals: Send + Sync + std::fmt::Debug {
    /// Open terminal `id` as `req` says for the agent `started`, and tell every client of it.
    fn open(
        &self,
        id: SessionId,
        req: OpenSession,
        started: Started,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    /// Run `req` again in terminal `id`, whose program ended and which is held for its restart
    /// (`Worker::restarting`), for the agent `started`.
    fn again(
        &self,
        id: SessionId,
        req: OpenSession,
        started: Started,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    /// The variables terminal `id` runs with besides the request's: its session, its token, the
    /// worker's socket and server.
    fn env(&self, id: SessionId) -> Vec<(String, String)>;
}

/// Starts agents in terminals. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Starter {
    host: Host,
    claude: Claude,
    place: Arc<Place>,
    terminals: Arc<dyn Terminals>,
    machine: String,
}

/// Why a start that names more than a session to take up again is refused.
const ONLY_RESUME: &str = "a start takes no arguments here but the session it takes up again";

/// What [`Starter::made`] did to a start's thread, for [`Starter::unmade`] to take back.
#[derive(Debug)]
enum Made {
    /// It made the thread.
    New,
    /// It named the start's terminal and folder on the thread held already, which named the
    /// terminal `had` and ran in `cwd` before.
    Held { had: Option<String>, cwd: String },
}

/// Where a start's agent runs.
#[derive(Clone, Copy, Debug)]
enum Opening {
    /// In a new terminal, for this intent.
    New(IntentId),
    /// Again in its thread's terminal, whose program ended.
    Again(ThreadId, SessionId),
}

impl Starter {
    /// Starts on `host`, observed by `claude`, their terminals opened through `terminals`, on
    /// the machine named `machine`.
    #[must_use]
    pub fn new(
        host: Host,
        claude: Claude,
        place: Place,
        terminals: Arc<dyn Terminals>,
        machine: String,
    ) -> Self {
        Self { host, claude, place: Arc::new(place), terminals, machine }
    }

    /// Start `start` for intent `id`, once, at `seated`'s seat when another agent starts it.
    pub async fn start(&self, id: IntentId, start: &Start, seated: Option<&Seated>) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        let started = match Self::checked(start) {
            Err(why) => Err(why),
            Ok(()) if start.agent.is(AgentId::CLAUDE_CODE) => {
                self.claude_code(Opening::New(id), start, seated).await
            }
            Ok(()) => self.other(Opening::New(id), start, seated).await,
        };
        let outcome = match started {
            Ok(thread) => Outcome::Started { thread },
            Err(reason) => Outcome::Refused { reason },
        };
        self.host.record_start(id, outcome)
    }

    /// Start the agent `started` opened terminal `terminal` for again, its program having ended
    /// and the terminal held for it: its thread taken up again in its own words (Claude Code's
    /// `--resume`, Codex's `resume`, pi's `--session`), with the choices its start made, at its
    /// seat when it had one, told nothing.
    pub async fn again(&self, terminal: SessionId, started: Started) -> Result<(), String> {
        let Started { thread, start } = started;
        let Some((meta, _status)) = self.host.meta(thread) else {
            return Err("its thread is gone".to_owned());
        };
        let args = if start.agent.is(AgentId::CLAUDE_CODE) {
            vec![agent::RESUME_FLAG.to_owned(), meta.native.clone()]
        } else if start.agent.is(AgentId::CODEX) {
            // Only a start that took a Codex thread up names one Codex knows.
            slopty_agent::codex::resumed(&start.args)
                .map(slopty_agent::codex::resume_args)
                .unwrap_or_default()
        } else {
            slopty_agent::pi::sessions::resume_args(&meta.native)
        };
        let start = Start { cwd: meta.cwd, args, prompt: None, worktree: None, ..start };
        let seated = self.host.seat_of(thread);
        let opening = Opening::Again(thread, terminal);
        let again = if start.agent.is(AgentId::CLAUDE_CODE) {
            self.claude_code(opening, &start, seated.as_ref()).await
        } else {
            self.other(opening, &start, seated.as_ref()).await
        };
        again.map(|_thread| ())
    }

    fn checked(start: &Start) -> Result<(), String> {
        let known = [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::PI];
        if !known.iter().any(|a| start.agent.is(a)) {
            return Err(format!("{} is no agent this machine starts", start.agent.0));
        }
        if !start.args.is_empty() && Self::resumed(start).is_none() {
            return Err(ONLY_RESUME.to_owned());
        }
        Ok(())
    }

    /// The session `start` takes up again, when its arguments name one as its agent takes it.
    fn resumed(start: &Start) -> Option<&str> {
        let args = start.args.as_slice();
        if start.agent.is(AgentId::CLAUDE_CODE) {
            match args {
                [flag, id] if flag == agent::RESUME_FLAG && agent::is_session_id(id) => Some(id),
                _ => None,
            }
        } else if start.agent.is(AgentId::CODEX) {
            slopty_agent::codex::resumed(args)
        } else {
            slopty_agent::pi::sessions::resumed(args)
                .filter(|(_, rest)| rest.is_empty())
                .map(|(id, _)| id)
        }
    }

    async fn claude_code(
        &self,
        opening: Opening,
        start: &Start,
        seated: Option<&Seated>,
    ) -> Result<ThreadId, String> {
        let resumed = Self::resumed(start).map(str::to_owned);
        if let Some(native) = &resumed {
            let thread = agent::thread_of(native);
            let running = self.host.meta(thread).filter(|(meta, status)| {
                meta.terminal().is_some() && status.liveness == Liveness::Live
            });
            // Its own terminal, ended, may not have said so yet.
            if running.is_some() && matches!(opening, Opening::New(_)) {
                return Ok(thread);
            }
            let (home, session) = (self.place.home.clone(), native.clone());
            let holder =
                tokio::task::spawn_blocking(move || super::claude::held::holder(&home, &session))
                    .await
                    .ok()
                    .flatten();
            if let Some(holder) = holder {
                return Err(holder.words(&self.machine));
            }
        }
        let Some(installed) = crate::facts::installed("claude", self.place.path.clone()).await
        else {
            return Err("Claude Code is not installed on this machine".to_owned());
        };
        let (native, terminal) = match opening {
            Opening::New(id) => (
                resumed.clone().unwrap_or_else(|| id.to_string()),
                seated.map_or_else(|| SessionId::from_uuid(*id.as_uuid()), |s| s.seat),
            ),
            Opening::Again(thread, terminal) => {
                let Some(native) = resumed.clone() else {
                    return Err(format!("{thread} names no session to take up again"));
                };
                (native, terminal)
            }
        };
        let tui = Tui {
            session: resumed.map_or_else(|| Begin::New(native.clone()), Begin::Resume),
            model: start.model.clone(),
            mode: start.mode.clone(),
            effort: start.effort.clone(),
            system: seated.and_then(|s| s.system.clone()),
            prompt: start.prompt.clone(),
        };
        let mut args = tui.args();
        if let Some(cli) = self.host.tools_cli() {
            args = slopty_agent::mcp::with_mcp(args, &cli);
            args = slopty_agent::hooks::with_relay(args, &cli, Path::new(&start.cwd));
            // The mod that shows the TUI's prompt box in the inspector, relaying through the
            // same CLI.
            match agent::draft::install(&self.place.data, &cli) {
                Ok(dir) => args = agent::draft::with_mod(args, &dir),
                Err(e) => tracing::warn!(error = %e, "Claude Code's prompt-box mod unwritten"),
            }
        }
        let version = installed.version.clone().unwrap_or_default();
        let thread = agent::thread_of(&native);
        let mut history = slopty_agent::claude::history::History::new(
            &native,
            &version,
            &start.cwd,
            WallMs::now(),
        );
        let meta = history.meta().clone();
        drop(history.drain());
        let made = self.made(thread, meta, start, terminal, seated);
        let command = std::iter::once(installed.program.to_string_lossy().into_owned()).chain(args);
        let (to, path) = ((opening, thread, terminal), &installed.path);
        if let Err(why) = self.open(to, start, command.collect(), path, Vec::new()).await {
            self.unmade(thread, terminal, start, made);
            return Err(why);
        }
        self.claude.begun(terminal, native, start.cwd.clone(), version);
        Ok(thread)
    }

    /// A Codex or pi TUI, whose thread is a row: its title and its terminal.
    async fn other(
        &self,
        opening: Opening,
        start: &Start,
        seated: Option<&Seated>,
    ) -> Result<ThreadId, String> {
        let codex = start.agent.is(AgentId::CODEX);
        let program = if codex { "codex" } else { "pi" };
        let Some(installed) = crate::facts::installed(program, self.place.path.clone()).await
        else {
            return Err(format!("{program} is not installed on this machine"));
        };
        let (thread, terminal, session) = match opening {
            Opening::New(id) => (
                ThreadId::from_uuid(*id.as_uuid()),
                seated.map_or_else(|| SessionId::from_uuid(*id.as_uuid()), |s| s.seat),
                id.to_string(),
            ),
            Opening::Again(thread, terminal) => (thread, terminal, String::new()),
        };
        // Taken up again, it is told nothing: the system text went ahead of its first message.
        let prompt = match (opening, seated) {
            (Opening::Again(..), _) => None,
            (Opening::New(_), Some(seated)) => seated.ahead(start.prompt.as_deref()),
            (Opening::New(_), None) => start.prompt.clone(),
        };
        let resumed = Self::resumed(start).map(str::to_owned);
        let tools = self.host.tools_cli();
        let env = self.terminals.env(terminal);
        let (args, extra) = if codex {
            let choices = slopty_agent::codex::Choices {
                model: start.model.as_deref(),
                approval: start.mode.as_deref(),
                effort: start.effort.as_deref(),
            };
            let tools = tools.as_deref().map(|cli| (cli, env.as_slice()));
            let mut args =
                slopty_agent::codex::tui(resumed.as_deref(), &choices, tools, prompt.as_deref());
            if resumed.is_none() && matches!(opening, Opening::Again(..)) {
                // No Codex thread known to take up: Codex's own list of the folder's.
                args.insert(0, slopty_agent::codex::RESUME.to_owned());
            }
            (args, Vec::new())
        } else {
            let tools = tools.as_deref().and_then(|cli| {
                let file = slopty_agent::pi::install_tools(&self.place.data)
                    .inspect_err(|e| tracing::warn!(error = %e, "pi's tools extension unwritten"))
                    .ok()?;
                Some((file, slopty_agent::pi::tools(cli, &env).to_string()))
            });
            let file = tools.as_ref().map(|(file, _)| file.as_path());
            let model = start.model.as_deref();
            let args =
                slopty_agent::pi::tui(resumed.as_deref(), &session, model, file, prompt.as_deref());
            let extra = tools.map(|(_, json)| (slopty_agent::pi::TOOLS_ENV.to_owned(), json));
            (args, extra.into_iter().collect())
        };
        let native = resumed.unwrap_or_else(|| session.clone());
        let meta = ThreadMeta {
            id: thread,
            agent: start.agent.clone(),
            agent_version: installed.version.clone().unwrap_or_default(),
            native,
            cwd: start.cwd.clone(),
            title: start.prompt.as_deref().map(slopty_agent::title::title_of).unwrap_or_default(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::OBSERVED),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::now(),
        };
        let made = self.made(thread, meta, start, terminal, seated);
        let command = std::iter::once(installed.program.to_string_lossy().into_owned()).chain(args);
        let (to, path) = ((opening, thread, terminal), &installed.path);
        if let Err(why) = self.open(to, start, command.collect(), path, extra).await {
            self.unmade(thread, terminal, start, made);
            return Err(why);
        }
        let live = Status {
            phase: Phase::Idle,
            wait: None,
            liveness: Liveness::Live,
            since_ms: WallMs::now(),
        };
        self.host.apply(thread, vec![Action::Status(live)]);
        self.claude.row(terminal, thread);
        Ok(thread)
    }

    /// Hold thread `thread` as `meta` says, naming `terminal`, seated at `seated`: a new one, or
    /// the one held of the session taken up again, whose terminal and folder change in one step
    /// with the look at what they were. What it did, for [`Self::unmade`].
    fn made(
        &self,
        thread: ThreadId,
        mut meta: ThreadMeta,
        start: &Start,
        terminal: SessionId,
        seated: Option<&Seated>,
    ) -> Made {
        let named = terminal.to_string();
        let held = self.host.update(thread, |state| {
            let mut held = state.meta.clone();
            let had = held.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), named.clone());
            let cwd = std::mem::replace(&mut held.cwd, start.cwd.clone());
            let changed = held != state.meta;
            let actions = if changed { vec![Action::Meta(Box::new(held))] } else { Vec::new() };
            (actions, Made::Held { had, cwd })
        });
        let made = held.unwrap_or_else(|| {
            meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), named);
            if let Some(seated) = seated {
                ThreadMeta::ORCHESTRATED.clone_into(&mut meta.origin);
                meta.facts.extend(seated.facts.iter().cloned());
            }
            if let Err(e) = self.host.create(meta) {
                tracing::warn!(%thread, "a started agent's thread could not be made: {e}");
            }
            Made::New
        });
        if let Some(seated) = seated {
            self.host.seated(thread, seated);
        }
        made
    }

    /// Take back what [`Self::made`] did for `start`, whose terminal `terminal` did not open, in
    /// one step with the look at the thread: a thread it made is gone, and one held already names
    /// again the terminal it named before, in its folder before. A terminal named since stays:
    /// the session's observer named the one it was found running in (a start of it just before
    /// this one).
    fn unmade(&self, thread: ThreadId, terminal: SessionId, start: &Start, made: Made) {
        let Made::Held { had, cwd } = made else {
            if let Err(e) = self.host.remove(thread) {
                tracing::warn!(%thread, "a failed start's thread stayed: {e}");
            }
            return;
        };
        let named = terminal.to_string();
        self.host.update(thread, |state| {
            let mut meta = state.meta.clone();
            if meta.facts.get(ThreadMeta::TERMINAL_FACT) != Some(&named) {
                return (Vec::new(), ());
            }
            match had {
                Some(had) => drop(meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), had)),
                None => drop(meta.facts.remove(ThreadMeta::TERMINAL_FACT)),
            }
            if meta.cwd == start.cwd {
                meta.cwd = cwd;
            }
            let changed = meta != state.meta;
            (if changed { vec![Action::Meta(Box::new(meta))] } else { Vec::new() }, ())
        });
    }

    /// Open terminal `terminal` for `start` of `thread` as `opening` says, running `command` on
    /// `path`, with `extra` beside.
    async fn open(
        &self,
        (opening, thread, terminal): (Opening, ThreadId, SessionId),
        start: &Start,
        command: Vec<String>,
        path: &OsString,
        extra: Vec<(String, String)>,
    ) -> Result<(), String> {
        let mut env = vec![("PATH".to_owned(), path.to_string_lossy().into_owned())];
        env.extend(extra);
        let title = match start.agent.0.as_str() {
            AgentId::CLAUDE_CODE => "Claude Code",
            AgentId::CODEX => "Codex",
            _ => "pi",
        };
        let req = OpenSession {
            size: crate::orchestrate::ORCHESTRATED_SIZE,
            cwd: Some(start.cwd.clone()),
            command,
            env,
            title: Some(title.to_owned()),
            attach: false,
        };
        let started = Started::of(thread, start);
        match opening {
            Opening::New(_) => {
                tracing::info!(%terminal, agent = %start.agent.0, cwd = start.cwd, "an agent starts in a terminal");
                self.terminals.open(terminal, req, started).await
            }
            Opening::Again(..) => {
                tracing::info!(%terminal, agent = %start.agent.0, cwd = start.cwd, "an agent starts again in its terminal");
                self.terminals.again(terminal, req, started).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(agent: &str, args: &[&str]) -> Start {
        Start {
            agent: AgentId::named(agent),
            cwd: "/work".to_owned(),
            drive: None,
            prompt: None,
            model: None,
            mode: None,
            effort: None,
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            worktree: None,
        }
    }

    /// A start names an agent this machine starts, and no arguments but the session it takes
    /// up again, in that agent's own words: anything else is refused before a terminal opens.
    #[test]
    fn a_start_takes_only_a_known_agent_and_the_session_it_takes_up_again() {
        let session = "0198f3c2-7b1a-7c3e-9d4f-5a6b7c8d9e0f";
        for agent in [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::PI] {
            assert_eq!(Starter::checked(&start(agent, &[])), Ok(()), "{agent}");
            assert_eq!(Starter::resumed(&start(agent, &[])), None, "{agent}");
        }
        let taken_up = [
            (AgentId::CLAUDE_CODE, [agent::RESUME_FLAG, session]),
            (AgentId::CODEX, [slopty_agent::codex::RESUME, session]),
            (AgentId::PI, [slopty_agent::pi::sessions::SESSION_FLAG, session]),
        ];
        for (agent, args) in taken_up {
            let asked = start(agent, &args);
            assert_eq!(Starter::checked(&asked), Ok(()), "{agent}: {args:?}");
            assert_eq!(Starter::resumed(&asked), Some(session), "{agent}: {args:?}");
        }

        let unknown = Starter::checked(&start("aider", &[])).unwrap_err();
        assert_eq!(unknown, "aider is no agent this machine starts");
        let refused = [
            (AgentId::CLAUDE_CODE, vec!["--model", "opus"]),
            (AgentId::CLAUDE_CODE, vec![agent::RESUME_FLAG, "a session/../of another"]),
            (AgentId::CLAUDE_CODE, vec![agent::RESUME_FLAG, session, "--model"]),
            (AgentId::CLAUDE_CODE, vec![slopty_agent::codex::RESUME, session]),
            (AgentId::CODEX, vec![agent::RESUME_FLAG, session]),
            (AgentId::PI, vec![slopty_agent::pi::sessions::SESSION_FLAG, session, "-p"]),
        ];
        for (agent, args) in refused {
            let asked = start(agent, &args);
            assert_eq!(Starter::resumed(&asked), None, "{agent}: {args:?}");
            assert_eq!(Starter::checked(&asked), Err(ONLY_RESUME.to_owned()), "{agent}: {args:?}");
        }
    }
}
