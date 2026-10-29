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

/// What a start opens its terminal through: the worker's sessions, announced to every client as
/// any terminal is.
pub trait Terminals: Send + Sync + std::fmt::Debug {
    /// Open terminal `id` as `req` says, and tell every client of it.
    fn open(
        &self,
        id: SessionId,
        req: OpenSession,
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
        let outcome = match Self::checked(start) {
            Err(why) => refused(why),
            Ok(()) if start.agent.is(AgentId::CLAUDE_CODE) => {
                self.claude_code(id, start, seated).await
            }
            Ok(()) => self.other(id, start, seated).await,
        };
        self.host.record_start(id, outcome)
    }

    fn checked(start: &Start) -> Result<(), String> {
        let known = [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::PI];
        if !known.iter().any(|a| start.agent.is(a)) {
            return Err(format!("{} is no agent this machine starts", start.agent.0));
        }
        if !start.attachments.is_empty() {
            return Err("a start takes no files: name them in its prompt".to_owned());
        }
        if !start.options.is_empty() {
            return Err("a start takes no options here".to_owned());
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

    async fn claude_code(&self, id: IntentId, start: &Start, seated: Option<&Seated>) -> Outcome {
        let resumed = Self::resumed(start).map(str::to_owned);
        if let Some(native) = &resumed {
            let thread = agent::thread_of(native);
            let running = self.host.meta(thread).filter(|(meta, status)| {
                meta.terminal().is_some() && status.liveness == Liveness::Live
            });
            if running.is_some() {
                return Outcome::Started { thread };
            }
            let (home, session) = (self.place.home.clone(), native.clone());
            let holder =
                tokio::task::spawn_blocking(move || super::claude::held::holder(&home, &session))
                    .await
                    .ok()
                    .flatten();
            if let Some(holder) = holder {
                return refused(holder.words(&self.machine));
            }
        }
        let Some(installed) = crate::facts::installed("claude", self.place.path.clone()).await
        else {
            return refused("Claude Code is not installed on this machine".to_owned());
        };
        let native = resumed.clone().unwrap_or_else(|| id.to_string());
        let terminal = seated.map_or_else(|| SessionId::from_uuid(*id.as_uuid()), |s| s.seat);
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
        }
        let version = installed.version.clone().unwrap_or_default();
        let thread = agent::thread_of(&native);
        let mut history = slopty_agent::claude::history::History::new(
            &native,
            &version,
            &start.cwd,
            &[],
            WallMs::now(),
        );
        let meta = history.meta().clone();
        drop(history.drain());
        let made = self.made(thread, meta, start, terminal, seated);
        let command = std::iter::once(installed.program.to_string_lossy().into_owned()).chain(args);
        let opened =
            self.open(terminal, start, command.collect(), &installed.path, Vec::new()).await;
        if let Err(why) = opened {
            self.unmade(thread, made);
            return refused(why);
        }
        self.claude.begun(terminal, native, start.cwd.clone(), version);
        Outcome::Started { thread }
    }

    /// A Codex or pi TUI, whose thread is a row: its title and its terminal.
    async fn other(&self, id: IntentId, start: &Start, seated: Option<&Seated>) -> Outcome {
        let codex = start.agent.is(AgentId::CODEX);
        let program = if codex { "codex" } else { "pi" };
        let Some(installed) = crate::facts::installed(program, self.place.path.clone()).await
        else {
            return refused(format!("{program} is not installed on this machine"));
        };
        let terminal = seated.map_or_else(|| SessionId::from_uuid(*id.as_uuid()), |s| s.seat);
        let prompt = match seated {
            Some(seated) => seated.ahead(start.prompt.as_deref()),
            None => start.prompt.clone(),
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
            let args =
                slopty_agent::codex::tui(resumed.as_deref(), &choices, tools, prompt.as_deref());
            (args, Vec::new())
        } else {
            let tools = tools.as_deref().and_then(|cli| {
                let file = slopty_agent::pi::install_tools(&self.place.data)
                    .inspect_err(|e| tracing::warn!(error = %e, "pi's tools extension unwritten"))
                    .ok()?;
                Some((file, slopty_agent::pi::tools(cli, &env).to_string()))
            });
            let file = tools.as_ref().map(|(file, _)| file.as_path());
            let session = id.to_string();
            let model = start.model.as_deref();
            let args =
                slopty_agent::pi::tui(resumed.as_deref(), &session, model, file, prompt.as_deref());
            let extra = tools.map(|(_, json)| (slopty_agent::pi::TOOLS_ENV.to_owned(), json));
            (args, extra.into_iter().collect())
        };
        let native = resumed.unwrap_or_else(|| id.to_string());
        let thread = ThreadId::from_uuid(*id.as_uuid());
        let meta = ThreadMeta {
            id: thread,
            agent: start.agent.clone(),
            agent_version: installed.version.clone().unwrap_or_default(),
            native,
            cwd: start.cwd.clone(),
            title: start.prompt.as_deref().map(slopty_agent::title::title_of).unwrap_or_default(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::OBSERVED),
            caps: Vec::new(),
            models: Vec::new(),
            modes: Vec::new(),
            efforts: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::now(),
        };
        let made = self.made(thread, meta, start, terminal, seated);
        let command = std::iter::once(installed.program.to_string_lossy().into_owned()).chain(args);
        if let Err(why) =
            self.open(terminal, start, command.collect(), &installed.path, extra).await
        {
            self.unmade(thread, made);
            return refused(why);
        }
        let live = Status {
            phase: Phase::Idle,
            wait: None,
            liveness: Liveness::Live,
            since_ms: WallMs::now(),
        };
        self.host.apply(thread, vec![Action::Status(live)]);
        self.claude.row(terminal, thread);
        Outcome::Started { thread }
    }

    /// Hold thread `thread` as `meta` says, naming `terminal`, seated at `seated`: a new one, or
    /// the one held of the session taken up again. Whether it was made here.
    fn made(
        &self,
        thread: ThreadId,
        mut meta: ThreadMeta,
        start: &Start,
        terminal: SessionId,
        seated: Option<&Seated>,
    ) -> bool {
        let fact = (ThreadMeta::TERMINAL_FACT.to_owned(), terminal.to_string());
        let made = if let Some((mut held, _)) = self.host.meta(thread) {
            held.facts.insert(fact.0, fact.1);
            held.cwd.clone_from(&start.cwd);
            self.host.apply(thread, vec![Action::Meta(Box::new(held))]);
            false
        } else {
            meta.facts.insert(fact.0, fact.1);
            if let Some(seated) = seated {
                ThreadMeta::ORCHESTRATED.clone_into(&mut meta.origin);
                meta.facts.extend(seated.facts.iter().cloned());
            }
            if let Err(e) = self.host.create(meta) {
                tracing::warn!(%thread, "a started agent's thread could not be made: {e}");
            }
            true
        };
        if let Some(seated) = seated {
            self.host.seated(thread, seated);
        }
        made
    }

    /// Take back what [`Self::made`] did for a start whose terminal did not open.
    fn unmade(&self, thread: ThreadId, made: bool) {
        if made {
            if let Err(e) = self.host.remove(thread) {
                tracing::warn!(%thread, "a failed start's thread stayed: {e}");
            }
            return;
        }
        self.host.update(thread, |state| {
            let mut meta = state.meta.clone();
            meta.facts.remove(ThreadMeta::TERMINAL_FACT);
            (vec![Action::Meta(Box::new(meta))], ())
        });
    }

    /// Open terminal `terminal` for `start`, running `command` on `path`, with `extra` beside.
    async fn open(
        &self,
        terminal: SessionId,
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
        tracing::info!(%terminal, agent = %start.agent.0, cwd = start.cwd, "an agent starts in a terminal");
        self.terminals.open(terminal, req).await
    }
}

const fn refused(reason: String) -> Outcome {
    Outcome::Refused { reason }
}
