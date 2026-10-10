//! Codex as a project's task (`docs/decisions/projects.md`, "Codex runs a task"): the person's
//! own `codex`, told its role as developer instructions, and judged before it starts for words
//! that would give it more than the person allows.
//!
//! As with Claude Code, a word is let through only when Codex documents it as asking the person
//! no less ([`SAFE_SWITCHES`], [`SAFE_VALUED`], a sandbox or approval policy that asks, a
//! config key of the model alone). Anything else loosens: a profile or a config key that could
//! name a looser policy, a tool server or a hook, another working root, a subcommand, or `--`.

use std::fmt::Write as _;

use slopty_proto::project::Autonomy;

/// Codex's program, and its name among a worker's `agents` facts.
pub(super) const PROGRAM: &str = "codex";

/// Flags that take no value and give nothing the person would be asked for: a session in a new
/// git worktree of its own among them.
const SAFE_SWITCHES: [&str; 3] = ["--worktree", "--no-alt-screen", "--no-daemon"];

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

/// Codex's flags for its approval policy.
const APPROVAL_FLAGS: [&str; 2] = ["--ask-for-approval", "-a"];
/// Codex's flags for its sandbox.
const SANDBOX_FLAGS: [&str; 2] = ["--sandbox", "-s"];
/// The sandbox a task's Codex is held to at every level: writes in its workspace.
const HELD_SANDBOX: &str = "workspace-write";

/// The command line that starts a task's Codex: its role as developer instructions (Codex's
/// own place for what the person or a tool adds to its prompt), the caller's arguments, and its
/// brief as its first prompt. A task that writes in a clone opens in a worktree the worker made
/// from the project's target, not in one of Codex's own.
///
/// Held to its project's level, it asks as that level says ([`Autonomy::codex_approval`]: on
/// request, or never at a project that goes on its own) and writes only in its workspace,
/// unless the arguments name another policy or sandbox, which [`loosening`] let through only as
/// asking no less: the person's own `config.toml`, which may say `approval_policy = "never"`,
/// does not decide for a task.
pub(super) fn command(
    role: &str,
    args: Vec<String>,
    prompt: Option<String>,
    level: Autonomy,
) -> Vec<String> {
    let mut command = vec![PROGRAM.to_owned(), "-c".to_owned()];
    command.push(format!("developer_instructions={}", toml_string(role)));
    let named = |flags: &[&str]| {
        args.iter().take_while(|w| *w != "--").any(|word| {
            flags.contains(&word.split_once('=').map_or(word.as_str(), |(flag, _)| flag))
        })
    };
    if !named(&APPROVAL_FLAGS) {
        command.extend([APPROVAL_FLAGS[0].to_owned(), level.codex_approval().to_owned()]);
    }
    if !named(&SANDBOX_FLAGS) {
        command.extend([SANDBOX_FLAGS[0].to_owned(), HELD_SANDBOX.to_owned()]);
    }
    command.extend(args);
    command.extend(prompt.filter(|p| !p.trim().is_empty()));
    command
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
    /// arguments, which are passed as they are; the brief is the last word, and a blank one
    /// is none.
    #[test]
    fn codex_starts_with_its_role_and_its_brief() {
        let role = "You are \"task 1\"\\n\nline\ttab\u{7}é";
        let args = words(&["--worktree", "-m", "o3", "-a", "untrusted", "-s", "read-only"]);
        let started = command(role, args, Some("Go.".into()), Autonomy::Ask);
        assert_eq!(started[..2], ["codex", "-c"]);
        assert_eq!(
            started[2],
            r#"developer_instructions="You are \"task 1\"\\n\nline\ttab\u0007é""#
        );
        assert_eq!(
            started[3..],
            ["--worktree", "-m", "o3", "-a", "untrusted", "-s", "read-only", "Go."]
        );
        let blank = command("r", Vec::new(), Some("  ".into()), Autonomy::Ask);
        assert_eq!(blank.len(), 7, "the policy and sandbox pinned, no prompt: {blank:?}");
    }

    /// Held to its project's level, a task's Codex asks on request (never, at a project that goes
    /// on its own) and writes only in its workspace, whatever its own configuration says, unless
    /// its arguments chose a policy or a sandbox that asks no less.
    #[test]
    fn a_task_s_codex_is_held_to_its_level() {
        let start = |args: &[&str], level| command("r", words(args), None, level);
        let pinned = ["--ask-for-approval", "on-request", "--sandbox", "workspace-write"];
        assert_eq!(start(&["-m", "o3"], Autonomy::Ask)[3..7], pinned);
        assert_eq!(start(&["-m", "o3"], Autonomy::Ask)[7..], ["-m", "o3"]);
        assert_eq!(start(&[], Autonomy::Edits)[3..7], pinned, "edits in the workspace ask too");
        assert_eq!(
            start(&["-a", "untrusted", "--sandbox=read-only"], Autonomy::Ask)[3..],
            ["-a", "untrusted", "--sandbox=read-only"],
            "their own choices stand"
        );
        let asks = start(&["-s", "read-only"], Autonomy::Ask);
        assert_eq!(asks[3..5], ["--ask-for-approval", "on-request"]);
        let own = ["--ask-for-approval", "never", "--sandbox", "workspace-write"];
        assert_eq!(start(&[], Autonomy::Own)[3..7], own, "on its own: never, in its workspace");
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
