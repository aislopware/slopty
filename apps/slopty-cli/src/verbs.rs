//! The orchestration verbs as subcommands: each connects to the server, sends one verb (after
//! any lookups its names need) and prints the answer as text, or as JSON with `--json`.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use slopty_core::{DisplayId, SessionId, WindowId};
use slopty_net::client::bind_client;
use slopty_proto::folder::FsOp;
use slopty_proto::orchestration::{
    EventFilter, Happening, IdempotencyKey, Input, Outcome, Size, ThreadView, Verb, WaitUntil,
    Waited,
};
use slopty_proto::screen::CaptureTarget;
use slopty_proto::search::SearchQuery;
use slopty_proto::server::{Role, Vouch};
use slopty_tools::bulk::Here;
use slopty_tools::ops::{
    self, AgentWanted, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_LINES, DEFAULT_MAX_MATCHES,
    DEFAULT_WAIT_MS, Spec,
};
use slopty_tools::resolve::Resolver;
use slopty_tools::{Dispatch as _, ToolError, bulk, view};

use crate::link::{self, Link};

/// A terminal: `worker/session`, the worker by id or name and the session by id or a unique
/// prefix of it, or a session id (prefix) alone.
const TERM_HELP: &str = "Terminal: worker/session (worker id or name; session id or a unique \
                         prefix), or a session alone";

/// Verbs that drive workers through the server.
#[derive(Subcommand, Debug)]
pub enum VerbCmd {
    /// The workers the server knows: liveness, address, OS, terminals, agents waiting on you.
    Workers {
        #[command(subcommand)]
        cmd: Option<WorkersCmd>,
    },
    /// Wake a sleeping worker: the server, or an online worker on the same LAN, sends it the
    /// magic packet (Wake-on-LAN). It shows online in `slopty workers` once it is up.
    Wake {
        /// Worker id or name.
        worker: String,
    },
    /// Terminals on one worker, or on all of them.
    Terminals {
        /// Worker id or name.
        #[arg(long)]
        worker: Option<String>,
    },
    /// Start a terminal on a worker and print its TERM.
    Open {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Working directory on the worker (its home when omitted).
        #[arg(long)]
        cwd: Option<String>,
        /// A name for the terminal's tile.
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        size: SizeArgs,
        /// Program and arguments after `--` (the login shell when omitted).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Resize a terminal no client shows.
    Resize {
        #[arg(help = TERM_HELP)]
        term: String,
        /// Columns (10-1000).
        #[arg(long)]
        cols: u16,
        /// Rows (2-500).
        #[arg(long)]
        rows: u16,
    },
    /// What happens across every worker: agents, terminals, workers. Prints the events after
    /// `--since` (from now when omitted, 0 for all the server holds), waiting up to
    /// `--timeout` for the first; exits non-zero when none came. The cursor to go on from goes
    /// to stderr.
    Events {
        /// Cursor: the `next` of an earlier call.
        #[arg(long)]
        since: Option<u64>,
        /// Wait this many milliseconds for a first event.
        #[arg(long, default_value_t = DEFAULT_WAIT_MS)]
        timeout: u32,
        /// Keep printing events as they come, until interrupted.
        #[arg(long)]
        follow: bool,
        /// Only agents that come to need a human or go idle, on any worker.
        #[arg(long)]
        agent_input: bool,
    },
    /// Coding agents: start one on any worker, tell it, read it, wait for it, stop it.
    Agent {
        #[command(subcommand)]
        cmd: Box<AgentCmd>,
    },
    /// Every agent on the fleet, under the agent that started it.
    Agents,
    /// Copy a branch from a clone on one worker into a clone on another.
    BranchBring {
        /// The worker the branch is on, by id or name.
        #[arg(long)]
        from: String,
        /// The clone or worktree it is in there.
        #[arg(long)]
        repo: String,
        /// The branch.
        #[arg(long)]
        branch: String,
        /// The worker it goes to, by id or name.
        #[arg(long)]
        to: String,
        /// The clone it lands in there; that worker's clone of the same repository when
        /// omitted.
        #[arg(long)]
        into: Option<String>,
        /// The name it lands as; its own when omitted.
        #[arg(long)]
        name: Option<String>,
    },
    /// Type into a terminal.
    Send {
        #[arg(help = TERM_HELP)]
        term: String,
        #[command(flatten)]
        input: InputArgs,
    },
    /// Print the screen as it is drawn now.
    Screen {
        #[arg(help = TERM_HELP)]
        term: String,
    },
    /// Print scrollback and screen lines; the index to continue from goes to stderr.
    Output {
        #[arg(help = TERM_HELP)]
        term: String,
        /// First absolute line index (the oldest retained when omitted).
        #[arg(long)]
        since: Option<u64>,
        /// At most this many lines.
        #[arg(long, default_value_t = DEFAULT_MAX_LINES)]
        max: u32,
    },
    /// Commands run in the terminal (OSC 133) with their exit codes.
    Commands {
        #[arg(help = TERM_HELP)]
        term: String,
        /// Only commands whose prompt is at or after this absolute line.
        #[arg(long)]
        since: Option<u64>,
    },
    /// Block until something happens in a terminal; exits non-zero on a timeout or when the
    /// terminal closes first.
    Wait {
        #[arg(help = TERM_HELP)]
        term: String,
        #[command(flatten)]
        until: UntilArgs,
        /// Give up after this many milliseconds.
        #[arg(long, default_value_t = DEFAULT_WAIT_MS)]
        timeout: u32,
    },
    /// Close a terminal (hang up its program).
    Close {
        #[arg(help = TERM_HELP)]
        term: String,
    },
    /// List a directory on a worker.
    Ls {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Absolute path, or `~/…`.
        path: String,
        /// At most this many entries (at most 10000).
        #[arg(long, default_value_t = DEFAULT_MAX_ENTRIES)]
        max: u32,
    },
    /// What is at a path on a worker (links followed).
    Stat {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Absolute path, or `~/…`.
        path: String,
    },
    /// Make one empty folder on a worker, in a folder that exists; refused when anything is
    /// there already. Prints the new folder's path.
    Mkdir {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Absolute path, or `~/…`.
        path: String,
    },
    /// Move or rename a file or folder on a worker, within one volume. Nothing is replaced:
    /// refused when anything is at the destination. Prints where it now is.
    Mv {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// What to move: absolute path, or `~/…`.
        from: String,
        /// Where it goes, its new name last.
        to: String,
    },
    /// Put a file or folder on a worker in its OS's trash, where it can be put back; nothing
    /// is deleted. Prints its path in the trash.
    Trash {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Absolute path, or `~/…`.
        path: String,
    },
    /// Search the files under a directory on a worker, as ripgrep does: .gitignore honoured,
    /// binary files skipped.
    Search {
        /// What to look for: the text as it is, unless `--regex`.
        pattern: String,
        /// The directory to search, absolute or `~/…`.
        #[arg(default_value = "~")]
        root: String,
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Only files matching this glob (`*.rs`), or not matching one led by `!`
        /// (`!tests/**`); repeat for more.
        #[arg(long = "glob", short = 'g', value_name = "GLOB")]
        globs: Vec<String>,
        /// Read the pattern as a regular expression.
        #[arg(long)]
        regex: bool,
        /// Tell upper and lower case apart.
        #[arg(long, short = 's')]
        case_sensitive: bool,
        /// Match whole words only.
        #[arg(long, short = 'w')]
        word: bool,
        /// Lines of context before and after each match (at most 5).
        #[arg(long, short = 'C', default_value_t = 0)]
        context: u32,
        /// At most this many matching lines (at most 2000).
        #[arg(long, default_value_t = DEFAULT_MAX_MATCHES)]
        max: u32,
    },
    /// TCP ports listening in a worker's terminals.
    Ports {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
    },
    /// The windows and displays a worker can stream, for `slopty capture`.
    Windows {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
    },
    /// Write one still picture of a window or a display on a worker as a PNG file.
    Capture {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        #[command(flatten)]
        target: TargetArgs,
        /// The PNG file to write.
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Send a file of any size, or standard input, to a worker in parts; it replaces what is
    /// there once whole.
    Push {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// The file here, or `-` for standard input.
        #[arg(value_parser = here)]
        local: Here,
        /// Where it goes on the worker: absolute, or `~/…`.
        path: String,
    },
    /// Bring a file of any size from a worker in parts, to a file here or to standard output.
    Pull {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// The file on the worker: absolute, or `~/…`.
        path: String,
        /// Where it goes here, a directory taking it under its own name; `-` for standard
        /// output, where `--json` prints one read of at most 8 MiB.
        #[arg(value_parser = here)]
        local: Here,
        /// First byte (to standard output only).
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// At most this many bytes (to standard output only).
        #[arg(long)]
        length: Option<u64>,
    },
}

/// A window or a display: exactly one.
#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
pub struct TargetArgs {
    /// A window, by its id from `slopty windows`.
    #[arg(long)]
    window: Option<u32>,
    /// A display, by its id from `slopty windows`.
    #[arg(long)]
    display: Option<u32>,
}

impl TargetArgs {
    fn target(&self) -> Result<CaptureTarget> {
        match (self.window, self.display) {
            (Some(id), None) => Ok(CaptureTarget::Window(WindowId(id))),
            (None, Some(id)) => Ok(CaptureTarget::Display(DisplayId(id))),
            _ => bail!("give one of --window, --display"),
        }
    }
}

/// Which thread: exactly one of an agent's (or a thread's) id and a terminal.
#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
pub struct ThreadWhich {
    /// An agent's id, or a subagent's thread id.
    #[arg(long, alias = "thread")]
    agent: Option<String>,
    /// The terminal an agent runs in: worker/session.
    #[arg(long)]
    term: Option<String>,
}

impl ThreadWhich {
    fn arg(&self) -> ops::ThreadArg<'_> {
        ops::ThreadArg { thread: self.agent.as_deref(), term: self.term.as_deref() }
    }
}

/// `slopty workers …`.
#[derive(Subcommand, Debug)]
pub enum WorkersCmd {
    /// Remove a worker that is not online from the server's list.
    Forget {
        /// Worker id or name.
        worker: String,
    },
}

/// `slopty agent …`.
#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    /// Start an agent on a worker and print its id. Inside an agent's terminal, that agent
    /// is its parent.
    Start {
        /// Worker id or name.
        worker: String,
        /// The folder it works in, absolute or `~/…`.
        #[arg(long)]
        folder: Option<String>,
        /// A remote repository's address: the worker's clone of it is the folder, made first
        /// when there is none.
        #[arg(long)]
        repo: Option<String>,
        /// A fresh git worktree of the folder's clone, by this name.
        #[arg(long)]
        worktree: Option<String>,
        /// `claude` (the default), `codex` or `pi`.
        #[arg(long)]
        agent: Option<String>,
        /// The model, by the agent's own id.
        #[arg(long)]
        model: Option<String>,
        /// How hard it thinks, by the agent's own name.
        #[arg(long)]
        effort: Option<String>,
        /// What it is told first.
        #[arg(required = true, num_args = 1..)]
        prompt: Vec<String>,
    },
    /// Type an agent words in its terminal and submit them; from inside an agent's terminal
    /// they are that agent's. Refused while it waits on you.
    Tell {
        /// The agent's id.
        agent: String,
        /// What to say.
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
    },
    /// Wait until agents come to rest, need you, or end.
    Wait {
        /// The agents' ids.
        #[arg(required = true, num_args = 1..)]
        agents: Vec<String>,
        /// Wait for every one, not the first.
        #[arg(long)]
        all: bool,
        /// Give up after this many milliseconds.
        #[arg(long, default_value_t = DEFAULT_WAIT_MS)]
        timeout: u32,
    },
    /// Interrupt an agent at work and every agent at work under it, as Esc does in its
    /// terminal.
    Stop {
        /// The agent's id.
        agent: String,
    },
    /// The agent in a terminal and what it is doing.
    Status {
        #[arg(help = TERM_HELP)]
        term: String,
    },
    /// What an agent's thread did, whatever its agent, whole turns at a time, with what it
    /// waits on.
    Read {
        #[command(flatten)]
        which: ThreadWhich,
        /// Every tool call and notice too, not only what was said.
        #[arg(long)]
        activity: bool,
        /// The last turn already read: the `next` a read printed.
        #[arg(long)]
        after: Option<u32>,
    },
}

/// A new terminal's size, both or neither.
#[derive(Args, Debug)]
pub struct SizeArgs {
    /// Columns (10-1000); 120 when omitted, until a client shows it.
    #[arg(long, requires = "rows")]
    cols: Option<u16>,
    /// Rows (2-500); 36 when omitted.
    #[arg(long, requires = "cols")]
    rows: Option<u16>,
}

impl SizeArgs {
    pub fn size(&self) -> Option<Size> {
        Some(Size { cols: self.cols?, rows: self.rows? })
    }
}

/// What to type: exactly one form.
#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
pub struct InputArgs {
    /// Text typed as-is; a newline presses Enter.
    #[arg(long)]
    text: Option<String>,
    /// Text delivered as a paste (bracketed when the program asked for it).
    #[arg(long)]
    paste: Option<String>,
    /// Named keys, each `[mods+]key`: `enter`, `ctrl+c`, `up`, `shift+tab`, `escape`.
    #[arg(long, num_args = 1..)]
    keys: Option<Vec<String>>,
}

impl InputArgs {
    fn input(self) -> Result<Input> {
        match (self.text, self.paste, self.keys) {
            (Some(text), None, None) => Ok(Input::Text(text)),
            (None, Some(paste), None) => Ok(Input::Paste(paste)),
            (None, None, Some(keys)) => Ok(Input::Keys(keys)),
            _ => bail!("give one of --text, --paste, --keys"),
        }
    }
}

/// What to wait for: exactly one condition.
#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
pub struct UntilArgs {
    /// A new line of output matches this regular expression.
    #[arg(long, value_name = "REGEX")]
    output: Option<String>,
    /// No output for this many milliseconds.
    #[arg(long, value_name = "MS")]
    quiet: Option<u32>,
    /// The running command finishes (or the next one does, if none runs).
    #[arg(long)]
    command_done: bool,
    /// The terminal's program exits.
    #[arg(long)]
    exit: bool,
}

impl UntilArgs {
    fn until(self) -> Result<WaitUntil> {
        let Self { output, quiet, command_done, exit } = self;
        match (output, quiet, command_done, exit) {
            (Some(pattern), None, false, false) => Ok(WaitUntil::Output(pattern)),
            (None, Some(ms), false, false) => Ok(WaitUntil::Quiet { ms }),
            (None, None, true, false) => Ok(WaitUntil::CommandDone),
            (None, None, false, true) => Ok(WaitUntil::Exit),
            _ => bail!("give one of --output, --quiet, --command-done, --exit"),
        }
    }
}

/// Connect to the server, run `cmd` (under `key` when it changes something), print its answer.
pub async fn run(
    cmd: VerbCmd,
    server: Option<&str>,
    data_dir: &Path,
    json: bool,
    key: Option<IdempotencyKey>,
) -> Result<()> {
    let endpoint = bind_client()?;
    let address = link::locate(server, data_dir, &endpoint).await?;
    let result = match Link::connect(&endpoint, &address, role()).await {
        Ok(link) => execute(cmd, &link, json, key).await,
        Err(e) => Err(e),
    };
    crate::client::close_endpoint(&endpoint).await;
    result
}

/// Who this CLI speaks for: inside a Slopty terminal the server decides by what runs there
/// (an agent's shell never speaks for the person); outside one, the person. A terminal's
/// variables that do not name one (a session id that does not parse, a token without its
/// session) are no proof of being outside, so they speak for an agent.
fn role() -> Role {
    let name = format!("slopty @ {}", crate::client::machine_name());
    match session() {
        Some(session) => Role::Shell { name, session, token: token() },
        None if std::env::var_os(slopty_proto::ctl::SESSION_ENV).is_some() || token().is_some() => {
            Role::Agent { name, vouch: None }
        }
        None => Role::Client { name },
    }
}

/// The Slopty terminal this runs in (`SLOPTY_SESSION`), if any.
fn session() -> Option<SessionId> {
    std::env::var(slopty_proto::ctl::SESSION_ENV).ok()?.trim().parse().ok()
}

/// The token the worker gave the terminal this runs in (`SLOPTY_SESSION_TOKEN`), if any.
fn token() -> Option<String> {
    std::env::var(slopty_proto::ctl::SESSION_TOKEN_ENV).ok().filter(|t| !t.trim().is_empty())
}

/// The proof of the terminal an agent's tools speak from, when they run in one.
pub fn vouch() -> Option<Vouch> {
    Some(Vouch { session: session()?, token: token()? })
}

pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn execute(cmd: VerbCmd, link: &Link, json: bool, key: Option<IdempotencyKey>) -> Result<()> {
    let mut res = Resolver::new(link);
    match cmd {
        VerbCmd::Workers { cmd: None } => {
            let overview = ops::overview(link).await?;
            if json {
                print_json(&overview.json())?;
            } else {
                print!("{}", overview.text());
            }
        }
        VerbCmd::Workers { cmd: Some(WorkersCmd::Forget { worker }) } => {
            let id = ops::forget_worker(&mut res, &worker, key).await?;
            if json {
                print_json(&view::DONE)?;
            } else {
                println!("forgot {id}");
            }
        }
        VerbCmd::Wake { worker } => wake(&mut res, &worker, json).await?,
        VerbCmd::Terminals { worker } => {
            let ops::Listing { workers, terminals, agents } =
                ops::terminals(&mut res, worker.as_deref()).await?;
            if json {
                print_json(&view::terminals_json(&workers, &terminals, &agents))?;
            } else {
                print!("{}", view::terminals_text(&workers, &terminals, &agents));
            }
        }
        VerbCmd::Open { worker, cwd, name, size, command } => {
            let spec = Spec { cwd, command, env: Vec::new(), name, size: size.size() };
            let term = ops::open(&mut res, worker.as_deref(), spec, key).await?;
            print_term(term, json)?;
        }
        VerbCmd::Agent { cmd } => agent(*cmd, &mut res, json, key).await?,
        VerbCmd::Agents => {
            let agents = ops::agents(link).await?;
            print_agents(link, &agents, false, json).await?;
        }
        VerbCmd::BranchBring { from, repo, branch, to, into, name } => {
            let from = (from.as_str(), repo, branch);
            let (branch, head) = ops::branch_bring(&mut res, from, (&to, into, name), key).await?;
            if json {
                print_json(&serde_json::json!({ "branch": branch, "head": head }))?;
            } else {
                println!("{branch} at {head}");
            }
        }
        VerbCmd::Resize { term, cols, rows } => {
            ops::resize(&mut res, &term, Size { cols, rows }, key).await?;
            print_done(json)?;
        }
        VerbCmd::Events { since, timeout, follow, agent_input } => {
            let filter = if agent_input { EventFilter::AgentNeedsInput } else { EventFilter::All };
            events(link, since, timeout, follow, filter, json).await?;
        }
        VerbCmd::Send { term, input } => {
            ops::send(&mut res, &term, input.input()?, key).await?;
            print_done(json)?;
        }
        VerbCmd::Screen { term } => {
            let screen = ops::screen(&mut res, &term).await?;
            if json {
                print_json(&view::screen(&screen))?;
            } else {
                for line in &screen.lines {
                    println!("{}", line.text);
                }
            }
        }
        VerbCmd::Output { term, since, max } => {
            let (lines, next) = ops::output(&mut res, &term, since, max).await?;
            if json {
                print_json(&view::output(&lines, next))?;
            } else {
                for line in &lines {
                    println!("{}", line.text);
                }
                eprintln!("next: {next}");
            }
        }
        VerbCmd::Commands { term, since } => {
            let commands = ops::commands(&mut res, &term, since).await?;
            if json {
                print_json(&view::commands(&commands))?;
            } else {
                print!("{}", view::commands_text(&commands));
            }
        }
        VerbCmd::Wait { term, until, timeout } => {
            let until = until.until()?;
            let term = res.term(&term).await?;
            let waited = ops::wait(link, term, until, timeout, key).await?;
            if json {
                print_json(&view::waited(&waited))?;
            }
            match waited {
                Waited::Met { line } => {
                    if let (Some(line), false) = (line, json) {
                        println!("{}", line.text);
                    }
                }
                Waited::TimedOut => bail!("timed out after {timeout} ms"),
                Waited::Closed => bail!("the terminal closed first"),
            }
        }
        VerbCmd::Close { term } => {
            ops::close(&mut res, &term, key).await?;
            print_done(json)?;
        }
        VerbCmd::Ls { worker, path, max } => {
            let (entries, total) =
                ops::list_dir(&mut res, worker.as_deref(), path.clone(), max).await?;
            if json {
                print_json(&view::dir(&path, &entries, total))?;
            } else {
                print!("{}", view::dir_text(&entries, total));
            }
        }
        VerbCmd::Stat { worker, path } => {
            let found = ops::stat(&mut res, worker.as_deref(), path.clone()).await?;
            if json {
                print_json(&view::stat(&path, found.as_ref()))?;
            } else {
                print!("{}", view::stat_text(&path, found.as_ref()));
            }
        }
        VerbCmd::Mkdir { worker, path } => {
            let (parent, name) = ops::parent_and_name(&path)?;
            let op = FsOp::MakeDir { parent, name };
            print_placed(&ops::fs_change(&mut res, worker.as_deref(), op, key).await?, json)?;
        }
        VerbCmd::Mv { worker, from, to } => {
            let op = FsOp::Move { from, to };
            print_placed(&ops::fs_change(&mut res, worker.as_deref(), op, key).await?, json)?;
        }
        VerbCmd::Trash { worker, path } => {
            let op = FsOp::Trash { path };
            print_placed(&ops::fs_change(&mut res, worker.as_deref(), op, key).await?, json)?;
        }
        VerbCmd::Search {
            pattern,
            root,
            worker,
            globs,
            regex,
            case_sensitive,
            word,
            context,
            max,
        } => {
            let query = SearchQuery {
                pattern,
                regex,
                match_case: case_sensitive,
                whole_word: word,
                globs,
                context,
            };
            let (files, summary) =
                ops::search(&mut res, worker.as_deref(), root.clone(), query, max).await?;
            if json {
                print_json(&view::search(&root, &files, &summary))?;
            } else {
                print!("{}", view::search_text(&files, &summary));
            }
        }
        VerbCmd::Ports { worker } => {
            let (worker, ports) = ops::ports(&mut res, worker.as_deref()).await?;
            if json {
                print_json(&view::ports(worker, &ports))?;
            } else {
                print!("{}", view::ports_text(worker, &ports));
            }
        }
        VerbCmd::Windows { worker } => {
            let (_worker, windows, displays) = ops::windows(&mut res, worker.as_deref()).await?;
            if json {
                print_json(&view::screens(&windows, &displays))?;
            } else {
                print!("{}", view::screens_text(&windows, &displays));
            }
        }
        VerbCmd::Capture { worker, target, out } => {
            let still = ops::capture_still(&mut res, worker.as_deref(), target.target()?).await?;
            tokio::fs::write(&out, &still.png)
                .await
                .with_context(|| format!("write {}", out.display()))?;
            let path = out.to_string_lossy();
            let shown = view::still(Some(&path), still.width, still.height, still.png.len());
            if json {
                print_json(&shown)?;
            } else {
                println!("{path}: {}x{} PNG", still.width, still.height);
            }
        }
        VerbCmd::Push { worker, local, path } => {
            let moved = bulk::push(&mut res, worker.as_deref(), &local, path, key).await?;
            print_moved(&moved, json)?;
        }
        VerbCmd::Pull { worker, path, local: Here::Stdio, offset, length } if json => {
            let worker = res.worker(worker.as_deref()).await?;
            let chunk = ops::read_file(link, worker, path.clone(), offset, length).await?;
            print_json(&view::file(&path, &chunk))?;
        }
        VerbCmd::Pull { worker, path, local, offset, length } => {
            let range = (offset, length);
            let moved = bulk::pull(&mut res, worker.as_deref(), path, &local, range).await?;
            if local != Here::Stdio {
                print_moved(&moved, json)?;
            }
        }
    }
    Ok(())
}

/// `slopty agent …`.
async fn agent(
    cmd: AgentCmd,
    res: &mut Resolver<'_, Link>,
    json: bool,
    key: Option<IdempotencyKey>,
) -> Result<()> {
    match cmd {
        AgentCmd::Start { worker, folder, repo, worktree, agent, model, effort, prompt } => {
            let prompt = prompt.join(" ");
            let wanted =
                AgentWanted { worker, folder, prompt, agent, model, effort, worktree, repo };
            let (id, worktree) = ops::agent_start(res, wanted, key).await?;
            if json {
                print_json(&serde_json::json!({ "agent": id.to_string(), "worktree": worktree }))?;
            } else {
                println!("{id}");
            }
        }
        AgentCmd::Tell { agent, words } => {
            let agent = ops::agent_named(&agent)?;
            ops::agent_tell(res.dispatch(), agent, words.join(" "), key).await?;
            print_done(json)?;
        }
        AgentCmd::Wait { agents, all, timeout } => {
            let agents =
                agents.iter().map(|a| ops::agent_named(a)).collect::<Result<Vec<_>, _>>()?;
            let (agents, timed_out) = ops::agent_wait(res.dispatch(), agents, all, timeout).await?;
            print_agents(res.dispatch(), &agents, timed_out, json).await?;
        }
        AgentCmd::Stop { agent } => {
            ops::agent_stop(res.dispatch(), ops::agent_named(&agent)?, key).await?;
            print_done(json)?;
        }
        AgentCmd::Status { term } => {
            let agent = ops::agent_status(res, &term).await?;
            if json {
                print_json(&view::agent(agent.as_ref()))?;
            } else {
                println!("{}", view::agent_text(agent.as_ref()));
            }
        }
        AgentCmd::Read { which, activity, after } => {
            let view = if activity { ThreadView::Activity } else { ThreadView::Messages };
            let read = ops::read_thread(res, which.arg(), view, after).await?;
            if json {
                print_json(&view::thread_read(&read))?;
            } else {
                print!("{}", view::thread_read_text(&read));
            }
        }
    }
    Ok(())
}

/// Agents as they stand, each worker by its name.
async fn print_agents(
    link: &Link,
    agents: &[slopty_proto::agent::AgentEntry],
    timed_out: bool,
    json: bool,
) -> Result<()> {
    let names: HashMap<slopty_core::WorkerId, String> = match link.call(Verb::ListWorkers).await {
        Outcome::Workers(list) => list.into_iter().map(|w| (w.worker, w.name)).collect(),
        _ => HashMap::new(),
    };
    if json {
        print_json(&view::agents_tree::agents(agents, timed_out, &names))
    } else {
        print!("{}", view::agents_tree::agents_text(agents, &names));
        if timed_out {
            println!("(timed out)");
        }
        Ok(())
    }
}

/// A place here: `-` for standard input or output, else a path.
#[expect(clippy::unnecessary_wraps, reason = "clap's value parser returns a result")]
fn here(arg: &str) -> Result<Here, std::convert::Infallible> {
    Ok(Here::parse(arg))
}

fn print_moved(moved: &bulk::Moved, json: bool) -> Result<()> {
    if json {
        print_json(&view::moved(moved))
    } else {
        println!("{} bytes: {} and {}", moved.size, moved.local.display(), moved.remote);
        Ok(())
    }
}

/// Print the server's events after `since`: one answer, or answer after answer with `follow`.
/// Text names workers as the directory does; JSON while following is one event per line.
async fn events(
    link: &Link,
    since: Option<u64>,
    timeout: u32,
    follow: bool,
    filter: EventFilter,
    json: bool,
) -> Result<()> {
    let mut names: HashMap<slopty_core::WorkerId, String> = HashMap::new();
    if !json {
        let workers = Resolver::new(link).workers().await?.to_vec();
        names.extend(workers.into_iter().map(|w| (w.worker, w.name)));
    }
    let mut cursor = since;
    loop {
        let page = ops::events(link, cursor, timeout, filter).await?;
        if page.missed > 0 {
            eprintln!("missed {} events the server no longer holds", page.missed);
        }
        for e in &page.events {
            if let Happening::Worker { worker, name, .. } = &e.what {
                names.insert(*worker, name.clone());
            }
            if !json {
                println!("{}", view::event_text(e, &names));
            } else if follow {
                println!("{}", serde_json::to_string(&view::event(e))?);
            }
        }
        if !follow {
            if json {
                print_json(&view::events(&page))?;
            } else {
                eprintln!("next: {}", page.next);
            }
            if page.events.is_empty() {
                bail!("no event within {timeout} ms");
            }
            return Ok(());
        }
        std::io::stdout().flush()?;
        cursor = Some(page.next);
    }
}

pub fn print_term(term: slopty_proto::orchestration::TermRef, json: bool) -> Result<()> {
    if json {
        print_json(&view::opened(term))
    } else {
        println!("{}", view::term_string(term));
        Ok(())
    }
}

/// Ask the server to wake `worker`, and say which machine sent the packet.
async fn wake(res: &mut Resolver<'_, Link>, worker: &str, json: bool) -> Result<()> {
    let id = res.worker(Some(worker)).await?;
    let known = res.workers().await?.iter().find(|w| w.worker == id).cloned();
    let (by, to) = match res.dispatch().call(Verb::Wake { worker: id }).await {
        Outcome::WakeSent { by, to } => (by, to),
        other => return Err(ToolError::unexpected(other).into()),
    };
    let name = known.as_ref().map_or_else(|| id.to_string(), |w| w.name.clone());
    let ignores = known.is_some_and(|w| w.caps.wake_on_lan == Some(false));
    if json {
        #[derive(Serialize)]
        struct Sent<'a> {
            worker: String,
            by: &'a str,
            to: &'a [String],
            wake_on_lan_off: bool,
        }
        return print_json(&Sent {
            worker: id.to_string(),
            by: &by,
            to: &to,
            wake_on_lan_off: ignores,
        });
    }
    println!("{by} sent {name} the wake packet ({}); it shows online once it is up", to.join(", "));
    if ignores {
        eprintln!(
            "{name} said Wake for network access is off, so it may sleep on: turn it on there \
             (`sudo pmset -a womp 1`)"
        );
    }
    Ok(())
}

/// Where a change to the files left its entry.
fn print_placed(path: &str, json: bool) -> Result<()> {
    if json {
        print_json(&view::placed(path))
    } else {
        println!("{path}");
        Ok(())
    }
}

fn print_done(json: bool) -> Result<()> {
    if json { print_json(&view::DONE) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser, Debug)]
    struct Cli {
        #[command(subcommand)]
        cmd: VerbCmd,
    }

    fn parse(args: &[&str]) -> Result<VerbCmd, clap::Error> {
        Cli::try_parse_from(std::iter::once("slopty").chain(args.iter().copied())).map(|c| c.cmd)
    }

    #[test]
    fn open_takes_its_command_after_a_double_dash() {
        let cmd = parse(&["open", "--worker", "studio", "--cwd", "/tmp", "--", "ls", "-la"]);
        let VerbCmd::Open { worker, cwd, name, size, command } = cmd.unwrap() else { panic!() };
        assert_eq!(worker.as_deref(), Some("studio"));
        assert_eq!(cwd.as_deref(), Some("/tmp"));
        assert_eq!(name, None);
        assert_eq!(size.size(), None);
        assert_eq!(command, ["ls", "-la"]);
        let VerbCmd::Open { size, .. } = parse(&["open", "--cols", "200", "--rows", "50"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(size.size(), Some(Size { cols: 200, rows: 50 }));
        parse(&["open", "--cols", "200"]).unwrap_err();
    }

    #[test]
    fn send_takes_exactly_one_input() {
        let VerbCmd::Send { term, input } =
            parse(&["send", "s/1", "--keys", "ctrl+c", "enter"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(term, "s/1");
        assert_eq!(input.input().unwrap(), Input::Keys(vec!["ctrl+c".into(), "enter".into()]));
        let VerbCmd::Send { input, .. } = parse(&["send", "t", "--text", "ls\n"]).unwrap() else {
            panic!()
        };
        assert_eq!(input.input().unwrap(), Input::Text("ls\n".into()));
        let VerbCmd::Send { input, .. } = parse(&["send", "t", "--paste", "a\nb"]).unwrap() else {
            panic!()
        };
        assert_eq!(input.input().unwrap(), Input::Paste("a\nb".into()));
        parse(&["send", "t"]).unwrap_err();
        parse(&["send", "t", "--text", "a", "--paste", "b"]).unwrap_err();
    }

    #[test]
    fn wait_takes_exactly_one_condition() {
        let until = |args: &[&str]| {
            let VerbCmd::Wait { until, timeout, .. } = parse(args).unwrap() else { panic!() };
            (until.until().unwrap(), timeout)
        };
        assert_eq!(
            until(&["wait", "t", "--output", "^done$"]),
            (WaitUntil::Output("^done$".into()), DEFAULT_WAIT_MS)
        );
        assert_eq!(
            until(&["wait", "t", "--quiet", "500", "--timeout", "9"]),
            (WaitUntil::Quiet { ms: 500 }, 9)
        );
        assert_eq!(until(&["wait", "t", "--command-done"]).0, WaitUntil::CommandDone);
        assert_eq!(until(&["wait", "t", "--exit"]).0, WaitUntil::Exit);
        parse(&["wait", "t", "--agent-input"]).unwrap_err();
        parse(&["wait", "t"]).unwrap_err();
        parse(&["wait", "t", "--exit", "--command-done"]).unwrap_err();
    }

    /// The agent verbs, as `slopty agent …` parses them: a start names its worker and takes
    /// its prompt as the rest of the line, and no harness arguments.
    fn agent(args: &[&str]) -> Result<AgentCmd, clap::Error> {
        let mut line = vec!["agent"];
        line.extend_from_slice(args);
        match parse(&line)? {
            VerbCmd::Agent { cmd } => Ok(*cmd),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_agent_starts_on_a_worker_with_its_prompt() {
        let AgentCmd::Start { worker, folder, worktree, agent: harness, prompt, .. } = agent(&[
            "start",
            "studio",
            "--folder",
            "~/src/app",
            "--worktree",
            "login",
            "--agent",
            "codex",
            "fix",
            "the",
            "login",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!((worker.as_str(), folder.as_deref()), ("studio", Some("~/src/app")));
        assert_eq!((worktree.as_deref(), harness.as_deref()), (Some("login"), Some("codex")));
        assert_eq!(prompt.join(" "), "fix the login");
        agent(&["start", "studio"]).unwrap_err();
        let AgentCmd::Wait { agents, all, timeout } =
            agent(&["wait", "a1", "a2", "--all"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((agents.len(), all, timeout), (2, true, DEFAULT_WAIT_MS));
        let AgentCmd::Tell { agent: to, words } = agent(&["tell", "a1", "go", "on"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((to.as_str(), words.join(" ").as_str()), ("a1", "go on"));
        let VerbCmd::Agents = parse(&["agents"]).unwrap() else { panic!() };
        let VerbCmd::BranchBring { from, to, into, .. } =
            parse(&["branch-bring", "--from", "a", "--repo", "/r", "--branch", "b", "--to", "c"])
                .unwrap()
        else {
            panic!()
        };
        assert_eq!((from.as_str(), to.as_str(), into), ("a", "c", None));
    }

    #[test]
    fn events_workers_forget_and_the_file_verbs_parse() {
        let VerbCmd::Events { since, timeout, follow, agent_input } =
            parse(&["events", "--since", "7", "--follow", "--agent-input"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((since, timeout, follow, agent_input), (Some(7), DEFAULT_WAIT_MS, true, true));
        let VerbCmd::Workers { cmd: None } = parse(&["workers"]).unwrap() else { panic!() };
        let VerbCmd::Workers { cmd: Some(WorkersCmd::Forget { worker }) } =
            parse(&["workers", "forget", "old-mac"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(worker, "old-mac");
        let VerbCmd::Wake { worker } = parse(&["wake", "studio"]).unwrap() else { panic!() };
        assert_eq!(worker, "studio");
        parse(&["wake"]).unwrap_err();
        let VerbCmd::Ls { max, .. } = parse(&["ls", "~"]).unwrap() else { panic!() };
        assert_eq!(max, DEFAULT_MAX_ENTRIES);
        let VerbCmd::Mv { from, to, .. } = parse(&["mv", "~/a", "~/b"]).unwrap() else { panic!() };
        assert_eq!((from.as_str(), to.as_str()), ("~/a", "~/b"));
        parse(&["mv", "~/a"]).unwrap_err();
        let VerbCmd::Mkdir { worker, .. } = parse(&["mkdir", "--worker", "box", "~/n"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(worker.as_deref(), Some("box"));
        let VerbCmd::Trash { path, .. } = parse(&["trash", "~/old"]).unwrap() else { panic!() };
        assert_eq!(path, "~/old");
        let VerbCmd::Resize { cols, rows, .. } =
            parse(&["resize", "t", "--cols", "100", "--rows", "30"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((cols, rows), (100, 30));
        parse(&["resize", "t", "--cols", "100"]).unwrap_err();
    }

    #[test]
    fn the_agent_screen_and_file_verbs_parse() {
        let AgentCmd::Read { which, activity, after } =
            agent(&["read", "--agent", "a1", "--activity", "--after", "4"]).unwrap()
        else {
            panic!()
        };
        let of = which.arg();
        assert_eq!((of.thread, of.term, activity, after), (Some("a1"), None, true, Some(4)));
        agent(&["read"]).unwrap_err();
        agent(&["read", "--agent", "a1", "--term", "t"]).unwrap_err();

        let VerbCmd::Capture { target, out, .. } =
            parse(&["capture", "--display", "1", "--out", "/tmp/d.png"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (target.target().unwrap(), out.to_str()),
            (CaptureTarget::Display(DisplayId(1)), Some("/tmp/d.png"))
        );
        parse(&["capture", "--window", "1", "--display", "2", "--out", "x"]).unwrap_err();
        let VerbCmd::Push { local, path, .. } = parse(&["push", "a.tar", "~/a.tar"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((local, path.as_str()), (Here::Path("a.tar".into()), "~/a.tar"));
        let VerbCmd::Push { local, .. } = parse(&["push", "-", "~/a.txt"]).unwrap() else {
            panic!()
        };
        assert_eq!(local, Here::Stdio, "standard input");
        let VerbCmd::Pull { path, local, offset, length, .. } =
            parse(&["pull", "~/a.tar", "."]).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (path.as_str(), local, offset, length),
            ("~/a.tar", Here::Path(".".into()), 0, None)
        );
        let VerbCmd::Pull { local, offset, length, .. } =
            parse(&["pull", "/f", "-", "--offset", "10", "--length", "4"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((local, offset, length), (Here::Stdio, 10, Some(4)));
        parse(&["pull", "/f"]).unwrap_err();
        parse(&["cat", "/f"]).unwrap_err();
        parse(&["put", "/f"]).unwrap_err();
    }

    #[test]
    fn output_pages_with_since_and_max() {
        let VerbCmd::Output { since, max, .. } =
            parse(&["output", "t", "--since", "1200", "--max", "50"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((since, max), (Some(1200), 50));
    }
}
