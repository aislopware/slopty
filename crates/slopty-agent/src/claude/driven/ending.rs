//! What kind of end a Claude Code that ended on its own had, said in plain words with the one
//! thing to do next, from its exit status and the end of its standard error
//! (`docs/decisions/agents.md`, "A Claude Code that ends unasked says what kind of end it was").
//!
//! Its own words decide first where they name the cause (out of memory, a flag it does not know,
//! a sign-in, the network), since an abort for memory ends on a signal too; then the signal that
//! ended it; then its exit code with its last line.

/// The notice kind of an end of Claude Code's that nobody asked for: a failure the chat draws in
/// the words given here.
pub const ENDED: &str = slopty_proto::thread::Notice::ENDED;

/// What kind of end it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// It ran out of memory.
    OutOfMemory,
    /// Something outside Slopty ended it: the signal (`SIGKILL`, `SIGTERM`…).
    Killed(i32),
    /// It crashed: the signal (`SIGSEGV`, `SIGABRT`…).
    Crashed(i32),
    /// It is not signed in, or its sign-in is refused.
    SignIn,
    /// It could not reach its service.
    Network,
    /// It did not know how it was started: too old for Slopty.
    TooOld,
    /// It exited with a code of its own, when one is known.
    Failed(Option<i32>),
}

/// An end said: its kind, and the words the chat shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ending {
    /// What kind of end it was.
    pub kind: Kind,
    /// What happened, then the one thing to do.
    pub words: String,
}

/// What the person does after any end: the thread goes on by its next message.
const AGAIN: &str = "Send a message to start it again.";

/// How many of its last lines name a cause other than memory: an older warning higher up in the
/// tail (a retry that then went through) is not why it ended.
const LAST_LINES: usize = 5;

/// The end of a Claude Code that exited with `status` (its code, or the signal negated, as ptyd
/// reports it; `None` when unknown), whose standard error ended with `stderr`.
#[must_use]
pub fn ending(status: Option<i32>, stderr: &str) -> Ending {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let last = lines.last().copied();
    let recent = lines
        .get(lines.len().saturating_sub(LAST_LINES)..)
        .unwrap_or_default()
        .join("\n")
        .to_lowercase();
    let signal = status.and_then(signal_of);
    let kind = if says(&stderr.to_lowercase(), OUT_OF_MEMORY) {
        Kind::OutOfMemory
    } else if let Some(signal) = signal {
        if KILLED.contains(&signal) { Kind::Killed(signal) } else { Kind::Crashed(signal) }
    } else if says(&recent, TOO_OLD) {
        Kind::TooOld
    } else if says(&recent, SIGN_IN) {
        Kind::SignIn
    } else if says(&recent, NETWORK) {
        Kind::Network
    } else {
        Kind::Failed(status)
    };
    let words = match kind {
        Kind::OutOfMemory => format!(
            "Claude Code ran out of memory. Close what else runs on that machine, then {}",
            lower_first(AGAIN)
        ),
        Kind::Killed(signal) if signal == SIGKILL => format!(
            "Claude Code was killed ({}), often by the system when memory runs short. {AGAIN}",
            name(signal)
        ),
        Kind::Killed(signal) => {
            format!("Claude Code was stopped from outside Slopty ({}). {AGAIN}", name(signal))
        }
        Kind::Crashed(signal) => format!("Claude Code crashed ({}). {AGAIN}", name(signal)),
        Kind::SignIn => "Claude Code is not signed in, or its sign-in expired. Sign it in on \
                         that machine, then send a message."
            .to_owned(),
        Kind::Network => "Claude Code could not reach its service. Check that machine's \
                          network, then send a message."
            .to_owned(),
        Kind::TooOld => "Claude Code is too old to be driven: it did not know how Slopty started \
                         it. Update Claude Code on that machine, then send a message."
            .to_owned(),
        Kind::Failed(code) => {
            let how = code.map(|code| format!(" with code {code}")).unwrap_or_default();
            match last {
                Some(line) => {
                    format!("Claude Code ended{how}: {}. {AGAIN}", line.trim_end_matches('.'))
                }
                None => format!("Claude Code ended{how}. {AGAIN}"),
            }
        }
    };
    Ending { kind, words }
}

/// How Claude Code, Node and Bun say memory ran out.
const OUT_OF_MEMORY: &[&str] = &[
    "out of memory",
    "heap limit",
    "allocation failed",
    "cannot allocate memory",
    "enomem",
    "outofmemory",
];

/// How Claude Code, and the parser of its arguments, refuse a flag they do not know.
const TOO_OLD: &[&str] = &[
    "unknown option",
    "unknown argument",
    "unexpected argument",
    "unrecognized option",
    "unrecognised option",
];

/// How a sign-in missing, expired or refused is said.
const SIGN_IN: &[&str] = &[
    "not logged in",
    "please run /login",
    "invalid api key",
    "authentication",
    "unauthorized",
    "oauth token",
    "login required",
];

/// How a service out of reach is said.
const NETWORK: &[&str] = &[
    "econnrefused",
    "econnreset",
    "enotfound",
    "etimedout",
    "eai_again",
    "ehostunreach",
    "enetunreach",
    "getaddrinfo",
    "fetch failed",
    "socket hang up",
    "connection error",
    "unable to connect",
    "network",
];

/// Whether `text` (lowercase) says any of `words`.
fn says(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.contains(word))
}

/// `text` with its first letter lowercase.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// The signal an exit `status` names: negated, as ptyd reports a signal, or `128 + n`, as a shell
/// that ran it reports one.
const fn signal_of(status: i32) -> Option<i32> {
    match status {
        ..0 => status.checked_neg(),
        129..=192 => status.checked_sub(128),
        _ => None,
    }
}

const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGQUIT: i32 = 3;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;

/// The signals by which something outside ends a process, rather than its crashing.
const KILLED: [i32; 5] = [SIGHUP, SIGINT, SIGQUIT, SIGKILL, SIGTERM];

/// The signals by name, as the machine Claude Code runs on numbers them (the worker's own).
const NAMES: &[(i32, &str)] = &[
    (SIGHUP, "SIGHUP"),
    (SIGINT, "SIGINT"),
    (SIGQUIT, "SIGQUIT"),
    (4, "SIGILL"),
    (5, "SIGTRAP"),
    (6, "SIGABRT"),
    (8, "SIGFPE"),
    (SIGKILL, "SIGKILL"),
    (if cfg!(target_os = "linux") { 7 } else { 10 }, "SIGBUS"),
    (11, "SIGSEGV"),
    (if cfg!(target_os = "linux") { 31 } else { 12 }, "SIGSYS"),
    (13, "SIGPIPE"),
    (SIGTERM, "SIGTERM"),
];

/// Signal `signal` by name, else by number.
fn name(signal: i32) -> String {
    NAMES
        .iter()
        .find(|(number, _)| *number == signal)
        .map_or_else(|| format!("signal {signal}"), |(_, name)| (*name).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each kind of end is told from what Claude Code left: its words where they name the cause,
    /// else its signal, else its code with its last line; each says the one thing to do.
    #[test]
    fn each_kind_of_end_is_told_apart() {
        let heap = "<--- Last few GCs --->\nFATAL ERROR: Reached heap limit Allocation failed - \
                    JavaScript heap out of memory\n 1: 0x10 node::Abort()\n 2: 0x20 v8::Fatal";
        let cases: [(Option<i32>, &str, Kind, &str); 11] = [
            (Some(-6), heap, Kind::OutOfMemory, "ran out of memory"),
            (Some(134), heap, Kind::OutOfMemory, "ran out of memory"),
            (Some(-9), "", Kind::Killed(SIGKILL), "killed (SIGKILL)"),
            (Some(143), "", Kind::Killed(SIGTERM), "from outside Slopty (SIGTERM)"),
            (Some(-11), "network warning, retrying", Kind::Crashed(11), "crashed (SIGSEGV)"),
            (Some(1), "error: unknown option '--include-hook-events'", Kind::TooOld, "too old"),
            (Some(1), "Invalid API key · Please run /login", Kind::SignIn, "not signed in"),
            (
                Some(1),
                "Error: connect ECONNREFUSED 127.0.0.1:443",
                Kind::Network,
                "could not reach",
            ),
            (
                Some(3),
                "claude: crashed mid-turn",
                Kind::Failed(Some(3)),
                "with code 3: claude: crashed mid-turn.",
            ),
            (Some(2), "", Kind::Failed(Some(2)), "ended with code 2."),
            (None, "it went", Kind::Failed(None), "Claude Code ended: it went."),
        ];
        for (status, stderr, kind, said) in cases {
            let end = ending(status, stderr);
            assert_eq!(end.kind, kind, "{status:?} {stderr:?}");
            assert!(end.words.contains(said), "{status:?}: {}", end.words);
            assert!(
                end.words.ends_with("send a message to start it again.")
                    || end.words.ends_with("Send a message to start it again.")
                    || end.words.ends_with("then send a message."),
                "one next step: {}",
                end.words
            );
        }
    }

    /// A warning high in the tail is not why it ended: only its last lines name a cause, so a
    /// network blip it recovered from leaves its real last line said.
    #[test]
    fn an_old_warning_is_not_the_cause() {
        let tail = "network error, retrying\nok\none\ntwo\nthree\nfour\nTypeError: x is undefined";
        let end = ending(Some(1), tail);
        assert_eq!(end.kind, Kind::Failed(Some(1)));
        assert!(end.words.contains("TypeError: x is undefined"), "{}", end.words);
    }

    /// An unknown signal is named by its number, and an exit code is never read as one.
    #[test]
    fn a_signal_is_named_and_a_code_is_not_one() {
        assert!(ending(Some(-60), "").words.contains("(signal 60)"));
        assert_eq!(ending(Some(1), "").kind, Kind::Failed(Some(1)));
        assert_eq!(ending(Some(128), "").kind, Kind::Failed(Some(128)));
        assert_eq!(ending(Some(193), "").kind, Kind::Failed(Some(193)));
    }
}
