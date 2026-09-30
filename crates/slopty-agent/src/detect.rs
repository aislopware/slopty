//! Recognising a Claude Code process from its name and arguments.
//!
//! The worker reads the foreground process of every session's tty (see
//! `slopty_pty::process::foreground`); this module decides, from the name and `argv` alone,
//! whether that process is a coding agent. It is deliberately a pure function over strings so
//! the rules can be tested without a process table, and so the platform lookup stays the only
//! part that needs a syscall.
//!
//! Claude Code appears in three shapes: the native launcher (`claude`), the npm install run
//! through a JavaScript runtime (`node …/@anthropic-ai/claude-code/cli.js`), and the local
//! install under `~/.claude/local` (`node …/.claude/local/node_modules/.bin/claude`). A
//! `node` running anything else — a dev server, a test runner — is not an agent, so the
//! runtime alone never counts.
//!
//! Both the executable's name and `argv[0]` are read, because they disagree often enough to
//! matter: a shell wrapper on the `PATH` is `bash` by executable and `claude` by `argv[0]`,
//! and `/bin/sh` on macOS is `bash` by executable and `/bin/sh` by `argv[0]`.

/// The foreground program of a session, as the platform named it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Program {
    /// Executable name (the last path component of the executable, not `argv[0]`).
    pub name: String,
    /// The whole command line, `argv[0]` first; empty when the platform would not say.
    pub argv: Vec<String>,
}

impl Program {
    /// A program with a name and no arguments (the shape most tests want).
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self { name: name.to_owned(), argv: Vec::new() }
    }

    /// Whether this is a Claude Code process.
    #[must_use]
    pub fn is_claude(&self) -> bool {
        is_claude(&self.name, &self.argv)
    }
}

/// Programs that run Claude Code rather than being it: the JavaScript runtimes the npm
/// install needs, and the shells that wrap it (`~/.claude/local/claude` is a script, and
/// `slopty_pty::pty` itself starts a bare `claude` it cannot find through the login shell).
/// One of these counts only when its arguments name the agent.
const RUNTIMES: [&str; 8] = ["node", "bun", "deno", "sh", "bash", "zsh", "fish", "dash"];

/// The runtimes that take a command to run as the argument of a `-c` flag.
const SHELLS: [&str; 5] = ["sh", "bash", "zsh", "fish", "dash"];

/// Whether a process with this executable name and command line is Claude Code.
#[must_use]
pub fn is_claude(name: &str, argv: &[String]) -> bool {
    let argv0 = argv.first().map(String::as_str).unwrap_or_default();
    if base(name) == "claude" || base(argv0) == "claude" {
        return true;
    }
    // Only the name the process was invoked under decides which interpreter is running: an
    // executable called `bash` invoked as `node` is not a JavaScript runtime and vice versa.
    let program = if argv0.is_empty() { base(name) } else { base(argv0) };
    if !RUNTIMES.contains(&program) {
        return false;
    }
    let shell = SHELLS.contains(&program);
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(flags) = arg.strip_prefix('-') {
            // `sh -c '<command>'`: the command word decides, and only it.
            if shell && flags.contains('c') {
                return args.next().is_some_and(|command| is_claude_command(command));
            }
            continue;
        }
        // The first argument that is not a flag is the script the runtime runs, and it alone
        // says what this process is: `node server.js --model claude` runs a server.
        return is_claude_script(arg);
    }
    false
}

/// The arguments Claude Code itself was given, out of a command line [`is_claude`] accepts.
///
/// They are what follows `claude`, or the script a runtime runs. Empty when they cannot be
/// told apart from the rest, as inside a shell's `-c` command.
#[must_use]
pub fn agent_args(argv: &[String]) -> &[String] {
    let Some((argv0, rest)) = argv.split_first() else { return &[] };
    let program = base(argv0);
    if program == "claude" {
        return rest;
    }
    if !RUNTIMES.contains(&program) {
        return &[];
    }
    let shell = SHELLS.contains(&program);
    let mut words = rest.iter();
    while let Some(arg) = words.next() {
        if let Some(flags) = arg.strip_prefix('-') {
            if shell && flags.contains('c') {
                return &[];
            }
            continue;
        }
        return if is_claude_script(arg) { words.as_slice() } else { &[] };
    }
    &[]
}

/// The command a shell runs, when `argv` is a shell handed one (`sh -c '<command>'`).
#[must_use]
pub fn shell_command(argv: &[String]) -> Option<&str> {
    let (argv0, rest) = argv.split_first()?;
    if !SHELLS.contains(&base(argv0)) {
        return None;
    }
    let mut words = rest.iter();
    while let Some(flags) = words.next()?.strip_prefix('-') {
        if flags.contains('c') {
            return words.next().map(String::as_str);
        }
    }
    None
}

/// Whether the command a shell was handed (`sh -c '<command>'`) starts the agent.
fn is_claude_command(command: &str) -> bool {
    shell_agent_args(command).is_some()
}

/// The arguments the agent gets from a shell command line (`sh -c '<command>'`).
///
/// They are the words after `claude` where it is the program of one of the line's simple
/// commands, split and unquoted as a POSIX shell splits them. `cd repo && FOO=1 exec claude --x`
/// gives `--x`; `echo claude` gives none, since there `claude` is an argument.
///
/// Expansions (`$VAR`, `$(…)`, globs) are not performed, so a flag spelled through one is not
/// seen: this reads what a person or an agent wrote, not what a shell would compute.
#[must_use]
pub fn shell_agent_args(command: &str) -> Option<Vec<String>> {
    let words = shell_split(command);
    words.split(|w| matches!(w, Word::Operator)).find_map(|simple| {
        let mut plain = simple.iter().map(|w| match w {
            Word::Plain(text) => text.as_str(),
            Word::Operator => "",
        });
        let program = plain.find(|w| !is_assignment(w) && !PREFIXES.contains(w))?;
        (base(program) == "claude").then(|| plain.map(str::to_owned).collect())
    })
}

/// The words of `line` when it is one simple command, split and unquoted as a shell does;
/// none when it chains or pipes several.
#[must_use]
pub fn simple_command(line: &str) -> Option<Vec<String>> {
    shell_split(line)
        .into_iter()
        .map(|word| match word {
            Word::Plain(text) => Some(text),
            Word::Operator => None,
        })
        .collect()
}

/// Words that run the command after them: `exec claude`, `env FOO=1 claude`.
const PREFIXES: [&str; 5] = ["exec", "command", "env", "nohup", "builtin"];

/// `NAME=value` before a program sets its environment.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    })
}

/// A word of a shell line, or an operator between simple commands.
#[derive(Debug, PartialEq, Eq)]
enum Word {
    Plain(String),
    Operator,
}

/// Split `line` into words as a POSIX shell does: blanks separate, single quotes keep
/// everything, double quotes keep all but `\` before `"`, `\`, `$` or `` ` ``, a backslash
/// outside quotes keeps the next character, and an unquoted `;`, `&`, `|` or newline ends a
/// simple command.
fn shell_split(line: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let text = word.get_or_insert_with(String::new);
                text.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                let text = word.get_or_insert_with(String::new);
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' if chars
                            .peek()
                            .is_some_and(|n| matches!(n, '"' | '\\' | '$' | '`')) =>
                        {
                            text.extend(chars.next());
                        }
                        other => text.push(other),
                    }
                }
            }
            '\\' => {
                // A backslash before a newline joins the lines; before anything else it quotes.
                if let Some(next) = chars.next().filter(|&n| n != '\n') {
                    word.get_or_insert_with(String::new).push(next);
                }
            }
            ';' | '&' | '|' | '\n' => {
                words.extend(word.take().map(Word::Plain));
                if words.last() != Some(&Word::Operator) {
                    words.push(Word::Operator);
                }
            }
            c if c.is_whitespace() => words.extend(word.take().map(Word::Plain)),
            other => word.get_or_insert_with(String::new).push(other),
        }
    }
    words.extend(word.map(Word::Plain));
    words
}

/// Whether a script path is Claude Code's entry point.
fn is_claude_script(arg: &str) -> bool {
    if arg.starts_with('-') {
        return false;
    }
    let file = base(arg);
    if file == "claude" || file == "claude.js" {
        return true;
    }
    // The npm package's entry point is a bare `cli.js`; only its directory identifies it.
    file == "cli.js" && (arg.contains("claude-code") || arg.contains("/.claude/"))
}

/// The last path component of `path` (the whole string when there is no separator).
fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn the_native_launcher_is_an_agent_whatever_its_arguments() {
        assert!(is_claude("claude", &[]));
        assert!(is_claude("claude", &argv(&["claude", "--resume"])));
        assert!(is_claude("/opt/homebrew/bin/claude", &[]));
        assert!(Program::named("claude").is_claude());
    }

    #[test]
    fn a_runtime_is_an_agent_only_when_it_runs_the_cli() {
        let npm = argv(&[
            "node",
            "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            "--resume",
        ]);
        assert!(is_claude("node", &npm));
        let local =
            argv(&["node", "/Users/x/.claude/local/node_modules/@anthropic-ai/claude-code/cli.js"]);
        assert!(is_claude("node", &local));
        let bin = argv(&["node", "/Users/x/.local/bin/claude"]);
        assert!(is_claude("bun", &bin));
        assert!(is_claude("deno", &bin));
    }

    #[test]
    fn a_shell_wrapper_is_recognised_by_what_it_runs() {
        // A `claude` script on the PATH: the kernel's executable is the interpreter.
        assert!(is_claude("bash", &argv(&["/Users/x/bin/claude", "--resume"])));
        // The kernel rewrites `argv` for a `#!` script, so the script is the second word.
        assert!(is_claude("bash", &argv(&["/bin/sh", "/Users/x/.claude/local/claude"])));
        // A login shell started to run a `claude` the daemon could not find on its own PATH
        // (see `slopty_pty::pty`) is that agent's session too.
        assert!(is_claude("zsh", &argv(&["/bin/zsh", "-lic", "claude"])));
        assert!(is_claude("zsh", &argv(&["/bin/zsh", "-lic", "claude --resume"])));
        // A plain login shell is not, and neither is a command that only mentions the agent.
        assert!(!is_claude("zsh", &argv(&["-zsh"])));
        assert!(!is_claude("bash", &argv(&["/bin/sh", "-c", "echo claude"])));
        assert!(!is_claude("bash", &argv(&["/bin/sh", "-c", "ls ~/.claude/local/claude"])));
    }

    #[test]
    fn only_the_script_a_runtime_runs_counts_never_a_later_argument() {
        // The agent's own name as the value of somebody else's flag proves nothing.
        assert!(!is_claude("node", &argv(&["node", "server.js", "--model", "claude"])));
        assert!(!is_claude("node", &argv(&["node", "server.js", "/usr/bin/claude"])));
        assert!(!is_claude("bun", &argv(&["bun", "run", "--", "claude"])), "`run` is the script");
        // Flags before the script are stepped over, so the script is still found.
        assert!(is_claude("node", &argv(&["node", "--enable-source-maps", "/usr/bin/claude"])));
    }

    /// The agent's own arguments follow `claude` or the script a runtime runs; inside a
    /// shell's `-c` command they are not told apart.
    #[test]
    fn the_agents_own_arguments_follow_its_program_or_script() {
        let words = |line: &str| argv(&line.split(' ').collect::<Vec<_>>());
        assert_eq!(agent_args(&words("/opt/bin/claude --model x")), words("--model x"));
        let cli = "/n/@anthropic-ai/claude-code/cli.js";
        assert_eq!(agent_args(&words(&format!("node --x {cli} -c"))), words("-c"));
        assert_eq!(
            agent_args(&words("/bin/sh /Users/x/bin/claude --effort high")),
            words("--effort high")
        );
        assert!(agent_args(&words("/bin/zsh -lic claude")).is_empty());
        assert!(agent_args(&words("node server.js --model x")).is_empty());
    }

    /// A shell's command starts the agent when `claude` is the program of any of its simple
    /// commands, after assignments or `exec`; quoting is undone as the shell undoes it.
    #[test]
    fn a_shell_line_runs_the_agent_wherever_it_is_a_program() {
        let args = |line: &str| shell_agent_args(line);
        let owned = |words: &[&str]| Some(words.iter().map(|w| (*w).to_owned()).collect());
        assert_eq!(args("claude --resume"), owned(&["--resume"]));
        assert_eq!(args("cd ~/w && FOO=1 exec claude -c"), owned(&["-c"]));
        assert_eq!(
            args("true; env A=b claude 'two words' \"x\\\"y\""),
            owned(&["two words", "x\"y"])
        );
        assert_eq!(args("make|/usr/bin/claude a\\ b"), owned(&["a b"]));
        assert_eq!(args("echo claude"), None);
        assert_eq!(args("cat ~/.claude/local/claude"), None);
        assert_eq!(args("'claude'"), owned(&[]), "a quoted program is still the program");
        assert!(is_claude("bash", &argv(&["/bin/sh", "-c", "cd x && claude"])));
        assert!(!is_claude("bash", &argv(&["/bin/sh", "-c", "cd x && echo claude"])));
        assert_eq!(shell_command(&argv(&["/bin/zsh", "-lic", "claude"])), Some("claude"));
        assert_eq!(shell_command(&argv(&["claude", "-c", "x"])), None, "not a shell");
        assert_eq!(shell_command(&argv(&["-zsh"])), None);
    }

    #[test]
    fn only_a_shell_hands_over_to_its_command_word() {
        // `-c` means "command" to a shell; to a runtime it is a flag like any other, and the
        // package entry point that follows is still the script that decides.
        let cli = "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js";
        assert!(is_claude("node", &argv(&["node", "--check", cli])));
        assert!(!is_claude("node", &argv(&["node", "-c", "claude-code/other.js", cli])));
        assert!(is_claude("sh", &argv(&["sh", "-c", "claude --resume"])));
    }

    #[test]
    fn other_programs_are_not_agents() {
        assert!(!is_claude("node", &argv(&["node", "server.js"])));
        assert!(!is_claude("node", &[]), "a runtime with no argv says nothing");
        assert!(!is_claude("zsh", &argv(&["-zsh"])));
        assert!(!is_claude("cargo", &argv(&["cargo", "test", "-p", "claude"])));
        assert!(!is_claude("vim", &argv(&["vim", "claude.md"])));
        // A generic `cli.js` outside the package is somebody else's tool.
        assert!(!is_claude("node", &argv(&["node", "/usr/local/lib/other/cli.js"])));
        // The runtime's own flags never name a script.
        assert!(!is_claude("node", &argv(&["node", "--claude"])));
    }

    #[test]
    fn claudette_and_friends_are_not_claude() {
        assert!(!is_claude("claudette", &[]));
        assert!(!is_claude("node", &argv(&["node", "/opt/claudette/cli.js"])));
    }
}
