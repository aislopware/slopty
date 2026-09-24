//! The orchestration verbs as subcommands: each connects to the server, sends one verb (after
//! any lookups its names need) and prints the answer as text, or as JSON with `--json`.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use slopty_net::client::bind_client;
use slopty_net::known::KnownWorkers;
use slopty_proto::handshake::ClientKind;
use slopty_proto::orchestration::{Input, WaitUntil, Waited};
use slopty_proto::server::Role;
use slopty_tools::ops::{self, DEFAULT_MAX_LINES, DEFAULT_WAIT_MS, Spec};
use slopty_tools::resolve::Resolver;
use slopty_tools::view;
use tokio::io::AsyncReadExt as _;

use crate::link::{self, Link};

/// A terminal: `worker/session`, the worker by id or name and the session by id or a unique
/// prefix of it, or a session id (prefix) alone.
const TERM_HELP: &str = "Terminal: worker/session (worker id or name; session id or a unique \
                         prefix), or a session alone";

/// Verbs that drive workers through the server.
#[derive(Subcommand, Debug)]
pub enum VerbCmd {
    /// The workers the server knows: liveness, address, OS, terminals, agents waiting on you.
    Workers,
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
        /// Program and arguments after `--` (the login shell when omitted).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Coding agents in terminals.
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
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
    /// Print a file on a worker.
    Cat {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Absolute path, or `~/…`.
        path: String,
    },
    /// Replace a file on a worker with standard input.
    Put {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Absolute path, or `~/…`.
        path: String,
    },
    /// TCP ports listening in a worker's terminals.
    Ports {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
    },
}

/// `slopty agent …`.
#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    /// Start Claude Code in a new terminal and print its TERM.
    Spawn {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// Working directory, usually a repository.
        #[arg(long)]
        cwd: String,
        /// A first prompt, typed once the agent is ready.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// The agent in a terminal and what it is doing.
    Status {
        #[arg(help = TERM_HELP)]
        term: String,
    },
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
    /// The agent in the terminal needs a human or goes idle.
    #[arg(long)]
    agent_input: bool,
}

impl UntilArgs {
    fn until(self) -> Result<WaitUntil> {
        let Self { output, quiet, command_done, exit, agent_input } = self;
        match (output, quiet, command_done, exit, agent_input) {
            (Some(pattern), None, false, false, false) => Ok(WaitUntil::Output(pattern)),
            (None, Some(ms), false, false, false) => Ok(WaitUntil::Quiet { ms }),
            (None, None, true, false, false) => Ok(WaitUntil::CommandDone),
            (None, None, false, true, false) => Ok(WaitUntil::Exit),
            (None, None, false, false, true) => Ok(WaitUntil::AgentNeedsInput),
            _ => bail!("give one of --output, --quiet, --command-done, --exit, --agent-input"),
        }
    }
}

/// This machine's name, for the server's logs.
pub fn machine_name() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

/// Connect to the server, run `cmd`, print its answer.
pub async fn run(cmd: VerbCmd, server: Option<&str>, data_dir: &Path, json: bool) -> Result<()> {
    let address = link::locate(server, data_dir)?;
    let client = KnownWorkers::open_in(data_dir)?.client();
    let role = Role::Client {
        client,
        kind: ClientKind::Tool,
        name: format!("slopty @ {}", machine_name()),
    };
    let endpoint = bind_client()?;
    let result = match Link::connect(&endpoint, &address, role).await {
        Ok(link) => execute(cmd, &link, json).await,
        Err(e) => Err(e),
    };
    crate::client::close_endpoint(&endpoint).await;
    result
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn execute(cmd: VerbCmd, link: &Link, json: bool) -> Result<()> {
    let mut res = Resolver::new(link);
    match cmd {
        VerbCmd::Workers => {
            let overview = ops::overview(link).await?;
            if json {
                print_json(&overview.json())?;
            } else {
                print!("{}", overview.text());
            }
        }
        VerbCmd::Terminals { worker } => {
            let (workers, terminals) = ops::terminals(&mut res, worker.as_deref()).await?;
            if json {
                print_json(&view::terminals_json(&workers, &terminals))?;
            } else {
                print!("{}", view::terminals_text(&workers, &terminals));
            }
        }
        VerbCmd::Open { worker, cwd, name, command } => {
            let spec = Spec { cwd, command, env: Vec::new(), name };
            let term = ops::open(&mut res, worker.as_deref(), spec).await?;
            print_term(term, json)?;
        }
        VerbCmd::Agent { cmd: AgentCmd::Spawn { worker, cwd, prompt } } => {
            let term = ops::spawn_agent(&mut res, worker.as_deref(), cwd, prompt).await?;
            print_term(term, json)?;
        }
        VerbCmd::Agent { cmd: AgentCmd::Status { term } } => {
            let agent = ops::agent_status(&mut res, &term).await?;
            if json {
                print_json(&view::agent(agent.as_ref()))?;
            } else {
                println!("{}", view::agent_text(agent.as_ref()));
            }
        }
        VerbCmd::Send { term, input } => {
            ops::send(&mut res, &term, input.input()?).await?;
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
            let waited = ops::wait(link, term, until, timeout).await?;
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
            ops::close(&mut res, &term).await?;
            print_done(json)?;
        }
        VerbCmd::Cat { worker, path } => {
            let bytes = ops::read_file(&mut res, worker.as_deref(), path.clone()).await?;
            if json {
                print_json(&view::file(&path, &bytes))?;
            } else {
                let mut out = std::io::stdout().lock();
                out.write_all(&bytes)?;
                out.flush()?;
            }
        }
        VerbCmd::Put { worker, path } => {
            let mut bytes = Vec::new();
            tokio::io::stdin().read_to_end(&mut bytes).await.context("read stdin")?;
            ops::write_file(&mut res, worker.as_deref(), path, bytes).await?;
            print_done(json)?;
        }
        VerbCmd::Ports { worker } => {
            let (worker, ports) = ops::ports(&mut res, worker.as_deref()).await?;
            if json {
                print_json(&view::ports(worker, &ports))?;
            } else {
                print!("{}", view::ports_text(worker, &ports));
            }
        }
    }
    Ok(())
}

fn print_term(term: slopty_proto::orchestration::TermRef, json: bool) -> Result<()> {
    if json {
        print_json(&view::opened(term))
    } else {
        println!("{}", view::term_string(term));
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
        let VerbCmd::Open { worker, cwd, name, command } = cmd.unwrap() else { panic!() };
        assert_eq!(worker.as_deref(), Some("studio"));
        assert_eq!(cwd.as_deref(), Some("/tmp"));
        assert_eq!(name, None);
        assert_eq!(command, ["ls", "-la"]);
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
        assert_eq!(until(&["wait", "t", "--agent-input"]).0, WaitUntil::AgentNeedsInput);
        parse(&["wait", "t"]).unwrap_err();
        parse(&["wait", "t", "--exit", "--command-done"]).unwrap_err();
    }

    #[test]
    fn agent_spawn_needs_a_directory() {
        let VerbCmd::Agent { cmd: AgentCmd::Spawn { worker, cwd, prompt } } =
            parse(&["agent", "spawn", "--cwd", "~/src/app", "--prompt", "fix the build"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (worker, cwd.as_str(), prompt.as_deref()),
            (None, "~/src/app", Some("fix the build"))
        );
        parse(&["agent", "spawn"]).unwrap_err();
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
