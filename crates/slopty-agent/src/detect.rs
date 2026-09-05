//! Recognising a Claude Code process from its name and arguments.
//!
//! The host reads the foreground process of every session's tty (see
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
    // `node <script>`: only the script decides, never a flag value, so `argv[0]` (the runtime)
    // and anything after a `--` argument that is not a path are looked at the same way.
    argv.iter().skip(1).any(|arg| is_claude_script(arg))
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
        // A plain login shell is not.
        assert!(!is_claude("zsh", &argv(&["-zsh"])));
        assert!(!is_claude("bash", &argv(&["/bin/sh", "-c", "echo claude"])));
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
