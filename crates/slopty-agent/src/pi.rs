//! pi, driven over its RPC mode (`pi --mode rpc`), with Slopty's permission gate.
//!
//! pi has no permission system of its own, so the worker loads a small extension into every pi
//! it drives (`assets/pi-gate/gate.ts`). Its `tool_call` handler asks about each call through
//! the RPC extension UI, as a `select` whose title is the call in JSON ([`GATE_PROTOCOL`]), and
//! lets it run only on an `allow`. Anything else blocks it, and so does a handler that fails, by
//! pi's own rule. What to ask the person and what to let through is decided here, not in the
//! extension.
//!
//! Like the Claude mod, the gate is embedded and written under the worker's data directory in a
//! directory named by its digest ([`install`]), so a running pi keeps the file it loaded while a
//! newer worker writes its own beside it.

pub mod driven;
pub mod rpc;

use std::io;
use std::path::{Path, PathBuf};

/// The pi version the gate and the codec were recorded against
/// (`crates/slopty-agent/tests/fixtures/pi`, `cargo xtask pi fixtures`).
pub const VERSION: &str = "1.0.0";

/// What the gate names itself in each dialog it opens.
pub const GATE_PROTOCOL: &str = "slopty-gate/1";

/// The gate's one file.
pub const GATE: (&str, &str) = ("gate.ts", include_str!("../assets/pi-gate/gate.ts"));

/// The flag that loads an extension.
pub const EXTENSION_FLAG: &str = "--extension";

/// The gate's digest, the name of its directory: 16 hex digits of BLAKE3 over its name and
/// content, each behind its length.
#[must_use]
pub fn digest() -> String {
    let mut hasher = blake3::Hasher::new();
    for part in <[&str; 2]>::from(GATE) {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().to_hex().as_str().chars().take(16).collect()
}

/// Write the gate under `data_dir` (`pi-gate/<digest>/gate.ts`) and return the file.
///
/// A gate already there whole is left as it is. A partial write never takes the name: the file
/// goes to a sibling that is renamed into place.
///
/// # Errors
///
/// When the directory cannot be written.
pub fn install(data_dir: &Path) -> io::Result<PathBuf> {
    let root = data_dir.join("pi-gate");
    let file = root.join(digest()).join(GATE.0);
    if std::fs::read_to_string(&file).is_ok_and(|have| have == GATE.1) {
        return Ok(file);
    }
    std::fs::create_dir_all(root.join(digest()))?;
    let staging = root.join(format!(".{}-{}", digest(), std::process::id()));
    std::fs::write(&staging, GATE.1)?;
    std::fs::rename(&staging, &file)?;
    Ok(file)
}

/// pi's flags that change neither what the gate asks nor which session is driven, nor reach a
/// credential: each with whether a value follows it.
///
/// From `pi --help` of the pinned [`VERSION`]. Every other flag is refused: one that loads code
/// (`--extension`, `--approve`), names a session (`--session`, `--continue`, `--fork`), changes
/// the mode (`--print`, `--mode`), or carries a key (`--api-key`). A new flag is judged before it
/// is let through, never after.
pub const SAFE_FLAGS: [(&str, bool); 30] = [
    ("--provider", true),
    ("--model", true),
    ("--models", true),
    ("--thinking", true),
    ("--tools", true),
    ("-t", true),
    ("--exclude-tools", true),
    ("-xt", true),
    ("--no-builtin-tools", false),
    ("-nbt", false),
    ("--no-tools", false),
    ("-nt", false),
    ("--name", true),
    ("-n", true),
    ("--no-extensions", false),
    ("-ne", false),
    ("--skill", true),
    ("--no-skills", false),
    ("-ns", false),
    ("--prompt-template", true),
    ("--no-prompt-templates", false),
    ("-np", false),
    ("--no-context-files", false),
    ("-nc", false),
    ("--system-prompt", true),
    ("--append-system-prompt", true),
    ("--no-approve", false),
    ("-na", false),
    ("--offline", false),
    ("--verbose", false),
];

/// `args` for a driven pi, when each is one of [`SAFE_FLAGS`] or the value after one; else the
/// first that is not, and why.
///
/// # Errors
///
/// When an argument is no safe flag: a flag outside the list, a message (the first prompt goes
/// over RPC), a flag's value written into it (`--model=x`), or a value missing or reading as a
/// flag.
pub fn checked(args: &[String]) -> Result<&[String], String> {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match SAFE_FLAGS.iter().find(|(flag, _)| flag == arg) {
            // A value that reads as a flag may be taken as one by pi's parser.
            Some((_, true)) => {
                if rest.next().is_none_or(|value| value.starts_with('-')) {
                    return Err(format!("{arg} needs a value after it"));
                }
            }
            Some((_, false)) => {}
            None if arg.starts_with('-') => {
                return Err(format!("{arg} is not a flag Slopty passes to pi"));
            }
            None => return Err(format!("{arg} is not a flag: the first prompt goes on its own")),
        }
    }
    Ok(args)
}

/// The arguments of a pi driven over RPC with the gate at `gate`, on session `session`, before
/// `args`, the thread's own.
///
/// `--session-id` opens the session of that id in pi's own session directory, or makes it, so
/// starting a thread and taking it up again are the same words, and the person's own pi finds
/// the session where it keeps its others.
#[must_use]
pub fn args(gate: &Path, session: &str, args: &[String]) -> Vec<String> {
    let fixed = [
        "--mode".to_owned(),
        "rpc".to_owned(),
        EXTENSION_FLAG.to_owned(),
        gate.display().to_string(),
        "--session-id".to_owned(),
        session.to_owned(),
    ];
    fixed.into_iter().chain(args.iter().cloned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written once, read back whole; a second install finds it; a damaged file is rewritten,
    /// and no staging file is left behind.
    #[test]
    fn the_gate_is_written_under_its_digest() {
        let data = tempfile::tempdir().expect("a temp dir");
        let file = install(data.path()).expect("installed");
        assert_eq!(file, data.path().join("pi-gate").join(digest()).join("gate.ts"));
        assert_eq!(std::fs::read_to_string(&file).expect("written"), GATE.1);
        assert_eq!(install(data.path()).expect("again"), file);
        std::fs::write(&file, "// damaged\n").expect("damage");
        assert_eq!(install(data.path()).expect("repaired"), file);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), GATE.1);
        let names: Vec<_> = std::fs::read_dir(data.path().join("pi-gate"))
            .expect("listed")
            .map(|e| e.expect("an entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(digest())]);
    }

    /// The gate names the protocol the worker reads, and fails closed.
    #[test]
    fn the_gate_speaks_the_protocol_and_blocks_by_default() {
        assert!(GATE.1.contains(&format!("\"{GATE_PROTOCOL}\"")));
        assert!(GATE.1.contains("if (answer === \"allow\")"));
        assert!(GATE.1.contains("block: true"));
    }

    /// The thread's own flags pass when each is a safe one with its value; a flag that loads
    /// code, names a session, carries a key or is unknown is refused, and so is a message.
    #[test]
    fn only_safe_flags_reach_pi() {
        let owned = |args: &[&str]| args.iter().map(|&a| a.to_owned()).collect::<Vec<_>>();
        let fine = owned(&["--model", "anthropic/x", "-t", "read,bash", "-ne", "--offline"]);
        assert_eq!(checked(&fine), Ok(fine.as_slice()));
        assert_eq!(checked(&[]), Ok(&[][..]));
        for bad in [
            &["--extension", "/tmp/x.ts"][..],
            &["-e", "/tmp/x.ts"],
            &["--approve"],
            &["--api-key", "k"],
            &["--session", "s"],
            &["--session-id", "s"],
            &["--continue"],
            &["--fork", "s"],
            &["--mode", "json"],
            &["--print"],
            &["--model=x"],
            &["hello"],
            &["@file.md"],
        ] {
            assert!(checked(&owned(bad)).is_err(), "{bad:?}");
        }
        assert!(checked(&owned(&["--model"])).is_err(), "a flag without its value");
        let hidden = owned(&["--model", "--extension", "/tmp/x.ts"]);
        assert!(checked(&hidden).is_err(), "a flag where a value goes");
    }

    /// RPC mode, the gate and the session come first; the thread's own arguments after.
    #[test]
    fn a_driven_pi_runs_rpc_with_the_gate_on_its_session() {
        let got = args(
            Path::new("/data/pi-gate/0123/gate.ts"),
            "0190-ab",
            &["--model".to_owned(), "m".to_owned()],
        );
        let want = [
            "--mode",
            "rpc",
            "--extension",
            "/data/pi-gate/0123/gate.ts",
            "--session-id",
            "0190-ab",
            "--model",
            "m",
        ];
        assert_eq!(got, want);
    }
}
