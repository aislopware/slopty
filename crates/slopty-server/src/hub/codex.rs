//! Codex as a project's task (`docs/decisions/projects.md`, "Codex runs a task"): the person's
//! own `codex`, told its role as developer instructions, and judged before it starts for words
//! that would give it more than the person allows.
//!
//! As with Claude Code, a word is let through only when Codex documents it as asking the person
//! no less ([`SAFE_SWITCHES`], [`SAFE_VALUED`], a sandbox or approval policy that asks, a
//! config key of the model alone). Anything else loosens: a profile or a config key that could
//! name a looser policy, a tool server or a hook, another working root, a subcommand, or `--`.

use std::fmt::Write as _;

/// Codex's program, and its name among a worker's `agents` facts.
pub(super) const PROGRAM: &str = "codex";

/// Codex's flag that runs a session in a new git worktree of its own.
pub(super) const WORKTREE_FLAG: &str = "--worktree";

/// Flags that take no value and give nothing the person would be asked for.
const SAFE_SWITCHES: [&str; 3] = [WORKTREE_FLAG, "--no-alt-screen", "--no-daemon"];

/// Flags whose value gives nothing the person would be asked for.
const SAFE_VALUED: [&str; 4] = ["--model", "-m", "--image", "-i"];

/// Sandbox policies that keep the agent's writes to its workspace.
const SAFE_SANDBOXES: [&str; 2] = ["read-only", "workspace-write"];

/// Approval policies that ask the person no less than Codex's default.
const SAFE_APPROVALS: [&str; 2] = ["untrusted", "on-request"];

/// Config keys (`-c key=value`) that choose the model and how it reasons, and nothing else.
const SAFE_CONFIG: [&str; 4] =
    ["model", "model_reasoning_effort", "model_reasoning_summary", "model_verbosity"];

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
            "--config" | "-c" => {
                |v| v.split_once('=').is_some_and(|(key, _)| SAFE_CONFIG.contains(&key.trim()))
            }
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

/// The command line that starts a task's Codex: its role as developer instructions (Codex's
/// own place for what the person or a tool adds to its prompt), a worktree of its own when it
/// writes in a clone, the caller's arguments, and its brief as its first prompt.
pub(super) fn command(
    role: &str,
    worktree: bool,
    args: Vec<String>,
    prompt: Option<String>,
) -> Vec<String> {
    let mut command = vec![PROGRAM.to_owned(), "-c".to_owned()];
    command.push(format!("developer_instructions={}", toml_string(role)));
    if worktree && !args.iter().any(|a| a == WORKTREE_FLAG) {
        command.push(WORKTREE_FLAG.to_owned());
    }
    command.extend(args);
    command.extend(prompt.filter(|p| !p.trim().is_empty()));
    command
}

/// `text` as a TOML basic string, which is how Codex reads a `-c` value.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _infallible = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    /// Only what asks the person no less goes through: the model, images, a worktree, a
    /// sandbox that keeps writes to the workspace, a policy that asks. A bypass, a looser
    /// sandbox or policy, a profile, a config key past the model's, another root, a subcommand
    /// and a flag missing its value all loosen, and the refusal names the word.
    #[test]
    fn only_what_asks_the_person_no_less_goes_through() {
        let fine = [
            &["--model", "o3", "-m", "gpt-5", "-i", "a.png", "--worktree"][..],
            &["-s", "workspace-write", "--sandbox=read-only", "-a", "untrusted"],
            &["--ask-for-approval=on-request", "-c", "model_reasoning_effort=\"high\""],
            &["--config", "model=\"o3\"", "--no-alt-screen"],
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
            (&["-c", "approval_policy=\"never\""], "-c approval_policy=\"never\""),
            (&["-c", "mcp_servers.x.command=\"sh\""], "-c mcp_servers.x.command=\"sh\""),
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
        let never = words(&["-a", "never"]);
        assert_eq!(args_of(&words(&["/opt/bin/codex", "-a", "never"])), Some(never.as_slice()));
        assert_eq!(args_of(&words(&["codexx"])), None);
    }

    /// The role goes as a TOML basic string, escaped as TOML says, before the caller's
    /// arguments; a worktree is asked for once; the brief is the last word.
    #[test]
    fn codex_starts_with_its_role_and_its_brief() {
        let role = "You are \"task 1\"\\n\nline\ttab\u{7}é";
        let started = command(role, true, words(&["--worktree", "-m", "o3"]), Some("Go.".into()));
        assert_eq!(started[..2], ["codex", "-c"]);
        assert_eq!(
            started[2],
            r#"developer_instructions="You are \"task 1\"\\n\nline\ttab\u0007é""#
        );
        assert_eq!(started[3..], ["--worktree", "-m", "o3", "Go."]);
        let read_only = command("r", false, Vec::new(), Some("  ".into()));
        assert_eq!(read_only, ["codex", "-c", "developer_instructions=\"r\""]);
        assert_eq!(command("r", true, Vec::new(), None)[3], WORKTREE_FLAG);
    }
}
