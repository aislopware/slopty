//! Codex and pi the person typed into a terminal, brought back after a reboot.
//!
//! As Claude Code is ([`crate::resume`]): which of the two a foreground process is, and which
//! of its own sessions it holds, so the reopened shell can be given the line that takes that
//! session up again.
//!
//! Each holds its session in its own way. A command line that names one (`codex resume <id>`,
//! `pi --session <id>`) says it outright. Otherwise the worker finds the one the process began:
//! Codex's thread in the process's folder begun after it started, on the person's daemon; pi's
//! session file in its folder's directory written after it started. A process that holds no
//! session of its own (`codex exec`, `pi --print`, `pi --no-session`) is none of this.
//!
//! Sans-IO: the worker reads the process table, the daemon's threads and pi's directory; this
//! names what they say.

use serde::{Deserialize, Serialize};

/// An agent other than Claude Code that runs in a terminal and comes back after a reboot.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Codex's TUI.
    Codex,
    /// pi's TUI.
    Pi,
}

impl Agent {
    /// The program, as the person's shell finds it.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }
}

/// A session to take up again in a reopened terminal.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Typed {
    /// Whose session.
    pub agent: Agent,
    /// The session: Codex's thread id, pi's session id or file.
    pub session: String,
    /// The folder it ran in.
    pub cwd: String,
}

impl Typed {
    /// The words that take it up again, the program first.
    #[must_use]
    pub fn words(&self) -> Vec<String> {
        let resume = match self.agent {
            Agent::Codex => "resume",
            Agent::Pi => crate::pi::sessions::SESSION_FLAG,
        };
        vec![self.agent.program().to_owned(), resume.to_owned(), self.session.clone()]
    }
}

/// What a foreground process says of the session it holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Holds {
    /// It is not one of these agents, or one that holds no session of its own.
    Nothing,
    /// Its command line names the session.
    Named(String),
    /// It holds the one it began, or took up from a picker: found by the worker.
    Begun,
}

/// Which agent the process named `name` with command line `argv` is, and what it holds.
#[must_use]
pub fn holds(name: &str, argv: &[String]) -> Option<(Agent, Holds)> {
    let (agent, args) = agent_of(name, argv)?;
    let held = match agent {
        Agent::Codex => codex(args),
        Agent::Pi => pi(args),
    };
    Some((agent, held))
}

/// The runtimes that run the npm installs' scripts.
const RUNTIMES: [&str; 3] = ["node", "bun", "deno"];

/// The agent and its own arguments, out of a process's name and command line: the native
/// program, or a runtime running its npm package's script.
fn agent_of<'a>(name: &str, argv: &'a [String]) -> Option<(Agent, &'a [String])> {
    let named = |word: &str| match base(word) {
        "codex" => Some(Agent::Codex),
        "pi" => Some(Agent::Pi),
        _ => None,
    };
    let (argv0, rest) = argv.split_first().map_or(("", &[][..]), |(a, r)| (a.as_str(), r));
    if let Some(agent) = named(argv0) {
        return Some((agent, rest));
    }
    if argv0.is_empty() {
        return named(name).map(|agent| (agent, rest));
    }
    if !RUNTIMES.contains(&base(argv0)) {
        return None;
    }
    // The first word that is no flag is the script the runtime runs, and it alone decides.
    let at = rest.iter().position(|word| !word.starts_with('-'))?;
    let script = rest.get(at)?;
    let args = rest.get(at.saturating_add(1)..).unwrap_or_default();
    let agent = named(script).or_else(|| {
        if script.contains("/@openai/codex/") {
            Some(Agent::Codex)
        } else if script.contains("/pi-coding-agent/") {
            Some(Agent::Pi)
        } else {
            None
        }
    })?;
    Some((agent, args))
}

/// Codex's subcommands that run no conversation of their own in the TUI (`codex --help`,
/// 0.162.0).
const CODEX_ELSEWHERE: [&str; 27] = [
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "app",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "queue",
    "archive",
    "delete",
    "migrate-rollouts",
    "unarchive",
    "cloud",
    "exec-server",
    "features",
    "help",
    "agents",
];

/// Codex's flags that take a value, so the word after one is not a subcommand.
const CODEX_VALUED: [&str; 14] = [
    "-c",
    "--config",
    "-m",
    "--model",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-p",
    "--profile",
    "-C",
    "--cd",
    "-i",
    "--image",
];

/// What Codex's TUI with `args` holds: `resume <id>` names its thread; a fresh one, a
/// `resume` from the picker or `--last`, and a `fork` hold one the daemon says.
fn codex(args: &[String]) -> Holds {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--help" || word == "-h" || word == "--version" || word == "-V" {
            return Holds::Nothing;
        }
        if word.starts_with('-') {
            if CODEX_VALUED.contains(&word.as_str()) {
                words.next();
            }
            continue;
        }
        if CODEX_ELSEWHERE.contains(&word.as_str()) {
            return Holds::Nothing;
        }
        if word == "resume" {
            // `resume <id>`; `--last`, `--all` or nothing is the picker's or the newest.
            return match words.find(|w| !w.starts_with('-')) {
                Some(id) if is_thread_id(id) => Holds::Named(id.clone()),
                _ => Holds::Begun,
            };
        }
        // `fork`, or the first prompt.
        return Holds::Begun;
    }
    Holds::Begun
}

/// Whether `id` is a Codex thread id: a UUID, as its rollouts are named.
fn is_thread_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// pi's flags that run no conversation kept as a session.
const PI_NOTHING: [&str; 9] =
    ["--no-session", "-p", "--print", "--export", "--mode", "-h", "--help", "-v", "--version"];

/// pi's flags that name the session.
const PI_NAMED: [&str; 2] = [crate::pi::sessions::SESSION_FLAG, "--session-id"];

/// What pi's TUI with `args` holds: `--session` or `--session-id` names it; otherwise the one
/// it began or `--continue`d or `--resume`d is found in its folder's directory.
fn pi(args: &[String]) -> Holds {
    let mut named = None;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (word.as_str(), None),
        };
        if PI_NOTHING.contains(&flag) {
            return Holds::Nothing;
        }
        if PI_NAMED.contains(&flag) {
            let value = inline.map(str::to_owned).or_else(|| words.next().cloned());
            named = value.filter(|v| !v.is_empty() && !v.starts_with('-'));
        }
    }
    named.map_or(Holds::Begun, Holds::Named)
}

/// The session file pi began in its folder's directory, as its id.
///
/// Of the files `listed` there (each name and when it was last written, in ms), the newest
/// written at or after `started`, the moment the process began. `None` when there is none, or
/// when `others` pi processes run in the same folder, any of which could have written it.
#[must_use]
pub fn pi_begun(listed: &[(String, u64)], started_ms: u64, others: usize) -> Option<String> {
    if others > 0 {
        return None;
    }
    listed
        .iter()
        .filter(|(_, written)| *written >= started_ms)
        .filter_map(|(name, written)| Some((crate::pi::sessions::id_of(name)?, *written)))
        .max_by_key(|(_, written)| *written)
        .map(|(id, _)| id.to_owned())
}

/// The Codex thread a TUI began.
///
/// Of the threads in its folder (each id and when it began, in ms), the one begun at or after
/// `started`, the moment the process began. `None` when there is none, or more than one, or
/// when `others` Codex TUIs run in the same folder.
#[must_use]
pub fn codex_begun(threads: &[(String, u64)], started_ms: u64, others: usize) -> Option<String> {
    if others > 0 {
        return None;
    }
    let mut begun = threads.iter().filter(|(_, at)| *at >= started_ms);
    let (id, _) = begun.next()?;
    begun.next().is_none().then(|| id.clone())
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    const ID: &str = "01a120d0-87f0-7cc3-b4dc-052b22902bc5";

    /// The native programs and the npm installs' scripts are told apart from anything else a
    /// runtime runs, and what each holds is read from its command line.
    #[test]
    fn codex_and_pi_are_known_by_name_and_say_what_they_hold() {
        type Case<'a> = (&'a str, &'a [&'a str], Option<(Agent, Holds)>);
        let cases: &[Case<'_>] = &[
            ("codex", &["codex"], Some((Agent::Codex, Holds::Begun))),
            (
                "codex",
                &["/usr/local/bin/codex", "-m", "gpt-5", "fix it"],
                Some((Agent::Codex, Holds::Begun)),
            ),
            ("codex", &["codex", "resume", ID], Some((Agent::Codex, Holds::Named(ID.to_owned())))),
            ("codex", &["codex", "resume", "--last"], Some((Agent::Codex, Holds::Begun))),
            (
                "codex",
                &["codex", "--sandbox", "exec", "resume"],
                Some((Agent::Codex, Holds::Begun)),
            ),
            ("codex", &["codex", "exec", "hi"], Some((Agent::Codex, Holds::Nothing))),
            ("codex", &["codex", "app-server"], Some((Agent::Codex, Holds::Nothing))),
            (
                "node",
                &["node", "/n/lib/node_modules/@openai/codex/bin/codex.js"],
                Some((Agent::Codex, Holds::Begun)),
            ),
            ("node", &["node", "/home/me/.bun/bin/pi"], Some((Agent::Pi, Holds::Begun))),
            (
                "node",
                &["node", "/g/@earendil-works/pi-coding-agent/dist/bundle/cli.js", "-c"],
                Some((Agent::Pi, Holds::Begun)),
            ),
            (
                "pi",
                &["pi", "--session", "abc-1"],
                Some((Agent::Pi, Holds::Named("abc-1".to_owned()))),
            ),
            ("pi", &["pi", "--session-id=s2"], Some((Agent::Pi, Holds::Named("s2".to_owned())))),
            ("pi", &["pi", "--no-session"], Some((Agent::Pi, Holds::Nothing))),
            ("pi", &["pi", "-p", "hi"], Some((Agent::Pi, Holds::Nothing))),
            ("pi", &["pi", "--mode", "rpc"], Some((Agent::Pi, Holds::Nothing))),
            ("pi", &[], Some((Agent::Pi, Holds::Begun))),
            ("node", &["node", "server.js", "codex"], None),
            ("zsh", &["-zsh"], None),
            ("picocom", &["picocom"], None),
        ];
        for (name, words, want) in cases {
            assert_eq!(holds(name, &argv(words)), *want, "{name} {words:?}");
        }
    }

    /// The words that take a session up again are each agent's own.
    #[test]
    fn a_session_is_taken_up_in_each_agents_own_words() {
        let codex = Typed { agent: Agent::Codex, session: ID.to_owned(), cwd: "/w".to_owned() };
        assert_eq!(codex.words(), ["codex", "resume", ID]);
        let pi = Typed { agent: Agent::Pi, session: "s1".to_owned(), cwd: "/w".to_owned() };
        assert_eq!(pi.words(), ["pi", "--session", "s1"]);
    }

    /// The session a process began is the one written, or begun, after it started, and only
    /// when no other process of the same agent in the folder could have begun it.
    #[test]
    fn the_session_begun_is_the_newest_since_the_start_and_alone() {
        let listed = [
            ("2026-10-11T08-00-00_old.jsonl".to_owned(), 900),
            ("2026-10-11T09-00-00_mine.jsonl".to_owned(), 1_500),
            ("2026-10-11T09-00-01_later.jsonl".to_owned(), 1_200),
            ("notes.txt".to_owned(), 2_000),
        ];
        assert_eq!(pi_begun(&listed, 1_000, 0).as_deref(), Some("mine"));
        assert_eq!(pi_begun(&listed, 1_000, 1), None, "another pi in the folder");
        assert_eq!(pi_begun(&listed, 1_600, 0), None, "nothing written since");

        let threads = [("t-old".to_owned(), 900), ("t-new".to_owned(), 1_100)];
        assert_eq!(codex_begun(&threads, 1_000, 0).as_deref(), Some("t-new"));
        assert_eq!(codex_begun(&threads, 800, 0), None, "two begun since: which is not known");
        assert_eq!(codex_begun(&threads, 1_000, 1), None, "another Codex in the folder");
    }
}
