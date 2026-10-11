//! Codex as a project's task (`docs/decisions/projects.md`, "A task's Codex runs on the person's
//! daemon"): a `codex` command line an agent runs in a terminal is judged for words that would
//! give it more than the person allows, and a Codex thread's settings, as its row says them, are
//! judged against the project's level.
//!
//! As with Claude Code, a word is let through only when Codex documents it as asking the person
//! no less ([`SAFE_SWITCHES`], [`SAFE_VALUED`], a sandbox or approval policy that asks). Anything
//! else loosens: a profile or a config key that could name a looser policy, a tool server or a
//! hook, another working root, a subcommand, or `--`. A word that keeps Codex's TUI off the
//! person's daemon ([`off_daemon`]) is refused before that, for what it hides rather than what it
//! allows.

use slopty_proto::project::Autonomy;

/// Codex's program, and its name among a worker's `agents` facts.
pub(super) const PROGRAM: &str = "codex";

/// Flags that take no value and give nothing the person would be asked for: a session in a new
/// git worktree of its own among them.
const SAFE_SWITCHES: [&str; 2] = ["--worktree", "--no-alt-screen"];

/// Flags whose value gives nothing the person would be asked for.
const SAFE_VALUED: [&str; 4] = ["--model", "-m", "--image", "-i"];

/// Sandbox policies that keep the agent's writes to its workspace.
const SAFE_SANDBOXES: [&str; 2] = ["read-only", "workspace-write"];

/// Approval policies that ask the person no less than Codex's default.
const SAFE_APPROVALS: [&str; 2] = ["untrusted", "on-request"];

/// Words that keep Codex's TUI off the person's daemon, as `--no-daemon` says outright: a
/// configuration override of any key, the model's included (`codex-rs/tui/src/daemon_startup.rs`,
/// `config_exclusion`, 0.162).
///
/// Off the daemon, the TUI runs an app-server of its own. The worker follows the daemon alone, so
/// it would keep no row of the thread: nothing would show what it asks the person, and nothing
/// would hold its policy or sandbox to its autonomy (`Hub::held_settings`). The model goes as
/// `--model`, which the TUI forwards to the daemon with the thread's start.
const OFF_DAEMON: [&str; 3] = ["--no-daemon", "-c", "--config"];

/// The first word in `args`, Codex's own arguments, that keeps its TUI off the person's daemon.
pub(super) fn off_daemon(args: &[String]) -> Option<String> {
    args.iter()
        .take_while(|word| *word != "--")
        .find(|word| {
            let flag = word.split_once('=').map_or(word.as_str(), |(flag, _)| flag);
            OFF_DAEMON.contains(&flag)
        })
        .cloned()
}

/// The first word in `args`, Codex's own arguments, that may loosen what it asks the person.
pub(super) fn loosening(args: &[String]) -> Option<String> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (word.as_str(), None),
        };
        if !flag.starts_with('-') || flag == "--" {
            return Some(word.clone());
        }
        if SAFE_SWITCHES.contains(&flag) && inline.is_none() {
            continue;
        }
        let judged: fn(&str) -> bool = match flag {
            f if SAFE_VALUED.contains(&f) => |_| true,
            "--sandbox" | "-s" => |v| SAFE_SANDBOXES.contains(&v),
            "--ask-for-approval" | "-a" => |v| SAFE_APPROVALS.contains(&v),
            _ => return Some(word.clone()),
        };
        let Some(value) = inline.or_else(|| words.next().cloned()) else {
            return Some(word.clone());
        };
        if !judged(&value) {
            return Some(format!("{flag} {value}"));
        }
    }
    None
}

/// Codex's own arguments in a command line that runs it directly.
pub(super) fn args_of(argv: &[String]) -> Option<&[String]> {
    let program = argv.first()?;
    (program.rsplit('/').next() == Some(PROGRAM)).then(|| argv.get(1..).unwrap_or_default())
}

/// What in a Codex thread's settings, as its row says them (its mode, Codex's approval policy,
/// and its `sandbox` fact, by the app-server's names), asks the person less than a task held to
/// `level` may: a policy past `on-request` (or past `never` at [`Autonomy::Own`]), or a sandbox
/// past writing in the workspace. A setting the row does not say yet is not judged.
pub(super) fn looser_settings(
    level: Autonomy,
    mode: Option<&str>,
    sandbox: Option<&str>,
) -> Option<String> {
    let allowed = |m: &str| SAFE_APPROVALS.contains(&m) || (level == Autonomy::Own && m == "never");
    if let Some(mode) = mode.filter(|m| !allowed(m)) {
        return Some(format!("approval policy {mode}"));
    }
    sandbox.filter(|s| !["readOnly", "workspaceWrite"].contains(s)).map(|s| format!("sandbox {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    /// Only what asks the person no less goes through: the model, images, a worktree, a
    /// sandbox that keeps writes to the workspace, a policy that asks. A bypass, a looser
    /// sandbox or policy, a profile, a config key, another root, a subcommand and a flag
    /// missing its value all loosen, and the refusal names the word.
    #[test]
    fn only_what_asks_the_person_no_less_goes_through() {
        let fine = [
            &["--model", "o3", "-m", "gpt-5", "-i", "a.png", "--worktree"][..],
            &["-s", "workspace-write", "--sandbox=read-only", "-a", "untrusted"],
            &["--ask-for-approval=on-request", "--no-alt-screen"],
            &[],
        ];
        for args in fine {
            assert_eq!(loosening(&words(args)), None, "{args:?}");
        }
        let loose = [
            (
                &["--dangerously-bypass-approvals-and-sandbox"][..],
                "--dangerously-bypass-approvals-and-sandbox",
            ),
            (&["--yolo"], "--yolo"),
            (&["--full-auto"], "--full-auto"),
            (&["--approve-for-me"], "--approve-for-me"),
            (&["-s", "danger-full-access"], "-s danger-full-access"),
            (&["--sandbox=danger-full-access"], "--sandbox danger-full-access"),
            (&["-a", "never"], "-a never"),
            (&["-c", "approval_policy=\"never\""], "-c"),
            (&["-c", "mcp_servers.x.command=\"sh\""], "-c"),
            (&["-p", "loose"], "-p"),
            (&["-C", "/"], "-C"),
            (&["--add-dir", "/"], "--add-dir"),
            (&["--enable", "x"], "--enable"),
            (&["exec"], "exec"),
            (&["--model"], "--model"),
            (&["--", "-a"], "--"),
            (&["--worktree=x"], "--worktree=x"),
        ];
        for (args, said) in loose {
            assert_eq!(loosening(&words(args)).as_deref(), Some(said), "{args:?}");
        }
        let model = words(&["-c", "model=\"o3\""]);
        assert_eq!(loosening(&model).as_deref(), Some("-c"), "even the model's key");
        let never = words(&["-a", "never"]);
        assert_eq!(args_of(&words(&["/opt/bin/codex", "-a", "never"])), Some(never.as_slice()));
        assert_eq!(args_of(&words(&["codexx"])), None);
    }

    /// A word that keeps the TUI off the daemon is found wherever it stands before `--`: the
    /// switch, and an override of any key, inline or not. Codex's TUI on the daemon goes by.
    #[test]
    fn what_keeps_codex_off_the_daemon_is_found() {
        for (args, said) in [
            (&["--no-daemon"][..], "--no-daemon"),
            (&["-m", "o3", "-c", "model=\"o3\""], "-c"),
            (&["--config=model_verbosity=\"low\""], "--config=model_verbosity=\"low\""),
            (&["resume", "--config", "x=1"], "--config"),
        ] {
            assert_eq!(off_daemon(&words(args)).as_deref(), Some(said), "{args:?}");
        }
        for args in
            [&["--model", "o3", "--worktree", "go"][..], &["resume", "x"], &["--", "-c"], &[]]
        {
            assert_eq!(off_daemon(&words(args)), None, "{args:?}");
        }
    }

    /// A Codex thread's settings, as its row says them, are judged as its arguments are: a
    /// policy that asks and a sandbox that keeps writes to the workspace pass; never, a granular
    /// policy, no sandbox or another's are looser. What the row does not say is not judged.
    #[test]
    fn a_codex_thread_s_settings_are_judged_as_its_arguments_are() {
        for (mode, sandbox) in [
            (Some("on-request"), Some("workspaceWrite")),
            (Some("untrusted"), Some("readOnly")),
            (None, None),
        ] {
            assert_eq!(looser_settings(Autonomy::Ask, mode, sandbox), None, "{mode:?} {sandbox:?}");
        }
        for (mode, sandbox, said) in [
            (Some("never"), Some("workspaceWrite"), "approval policy never"),
            (Some("granular"), None, "approval policy granular"),
            (Some("on-request"), Some("dangerFullAccess"), "sandbox dangerFullAccess"),
            (None, Some("externalSandbox"), "sandbox externalSandbox"),
        ] {
            assert_eq!(looser_settings(Autonomy::Ask, mode, sandbox).as_deref(), Some(said));
        }
        let own = looser_settings(Autonomy::Own, Some("never"), Some("workspaceWrite"));
        assert_eq!(own, None, "a project on its own lets Codex go without asking");
        let past = looser_settings(Autonomy::Own, Some("never"), Some("dangerFullAccess"));
        assert_eq!(past.as_deref(), Some("sandbox dangerFullAccess"), "never past its workspace");
    }
}
