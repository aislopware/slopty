//! Slopty's Claude Code mod: a plugin whose function hooks pass the engine's events to the
//! worker as they happen (`assets/claude-mod`), and how an agent is started with it.
//!
//! The plugin is TypeScript because Claude Code runs it (docs/decisions/claude-code.md). The
//! worker embeds its three files and writes them under its data directory, in a directory
//! named by their digest, so an agent that is running keeps the files it loaded while a newer
//! worker writes its own beside them. An agent gets the mod with [`Installed::args`] and [`env()`].
//!
//! Nothing it says is trusted until its `hello` passes [`crate::live::gate`], which names the
//! Claude Code versions the mod was verified against ([`MOD_CLAUDE_VERSIONS`]); everywhere else
//! the hooks, the transcript and the status line are the whole picture, as they are for a
//! `claude` the mod cannot load in.

use std::io;
use std::path::{Path, PathBuf};

/// The mod's own protocol, which its `hello` names. It changes with the events' shapes.
pub const MOD_PROTOCOL: u32 = 1;

/// The Claude Code versions the mod was verified against.
///
/// `cargo xtask fixtures claude-mod` verifies one: it records
/// `crates/slopty-agent/tests/fixtures/mod` with the official build. A version joins this list
/// only with a new recording, because the plugin API is early access and changes between
/// releases.
pub const MOD_CLAUDE_VERSIONS: &[&str] = &["2.1.286"];

/// Where the mod posts its events: the worker's mod socket.
pub const SOCKET_ENV: &str = "SLOPTY_MOD_SOCKET";

/// The installed mod's directory, for the shell integration's `claude` function.
pub const DIR_ENV: &str = "SLOPTY_CLAUDE_MOD";

/// Claude Code loads a plugin's function hooks (`hooks.json` `modules`) only with this set.
pub const FUNCTION_HOOKS_ENV: &str = "CLAUDE_CODE_ENABLE_FUNCTION_HOOKS";

/// The switch that silences the mod.
///
/// With it set, Claude Code loads the mod but blocks its requests, even to a Unix socket. An
/// agent the worker starts never has it: it is set empty, which Claude Code reads as unset
/// (`xtask fixtures claude-mod` records a run with it so).
pub const NONESSENTIAL_TRAFFIC_ENV: &str = "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC";

/// The flag that loads a plugin directory. Always in its `=` form: `--plugin-dir <path>` is
/// variadic and swallows the words after it (a prompt, a subcommand).
pub const PLUGIN_DIR_FLAG: &str = "--plugin-dir";

/// The plugin's files, by their path in the plugin directory.
pub const FILES: [(&str, &str); 3] = [
    (".claude-plugin/plugin.json", include_str!("../assets/claude-mod/.claude-plugin/plugin.json")),
    ("hooks/hooks.json", include_str!("../assets/claude-mod/hooks/hooks.json")),
    ("hooks/register.ts", include_str!("../assets/claude-mod/hooks/register.ts")),
];

/// The files' digest, the name of their directory.
///
/// 16 hex digits of BLAKE3 over each path and content, each behind its length.
#[must_use]
pub fn digest() -> String {
    let mut hasher = blake3::Hasher::new();
    let mut add = |part: &str| {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part.as_bytes());
    };
    for (path, content) in FILES {
        add(path);
        add(content);
    }
    hasher.finalize().to_hex().as_str().chars().take(16).collect()
}

/// Write the mod under `data_dir` (`claude-mod/<digest>`) and return its directory.
///
/// A mod already there whole is left as it is. A partial write never takes the name: the files
/// go to a sibling that is renamed into place.
///
/// # Errors
///
/// When the directory cannot be written.
pub fn install(data_dir: &Path) -> io::Result<PathBuf> {
    let root = data_dir.join("claude-mod");
    let dir = root.join(digest());
    if FILES.iter().all(|(path, content)| {
        std::fs::read_to_string(dir.join(path)).is_ok_and(|have| have == *content)
    }) {
        return Ok(dir);
    }
    let staging = root.join(format!(".{}-{}", digest(), std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    for (path, content) in FILES {
        let file = staging.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, content)?;
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::rename(&staging, &dir)?;
    Ok(dir)
}

/// The installed mod and the worker's socket it posts to: what an agent needs to run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The plugin directory ([`install`]).
    pub dir: PathBuf,
    /// The worker's mod socket.
    pub socket: PathBuf,
}

impl Installed {
    /// `claude` arguments that load the mod too, unless they already do.
    #[must_use]
    pub fn args(&self, args: Vec<String>) -> Vec<String> {
        with_mod(args, &self.dir)
    }

    /// The environment of an agent the worker starts ([`env()`]).
    #[must_use]
    pub fn agent_env(&self) -> Vec<(String, String)> {
        env(&self.socket)
    }

    /// The environment of every session, for the shell integration's `claude` function: where
    /// the mod is and where it posts.
    #[must_use]
    pub fn session_env(&self) -> Vec<(String, String)> {
        vec![
            (DIR_ENV.to_owned(), self.dir.to_string_lossy().into_owned()),
            (SOCKET_ENV.to_owned(), self.socket.to_string_lossy().into_owned()),
        ]
    }
}

/// The flag that loads the mod at `dir`.
fn plugin_flag(dir: &Path) -> String {
    format!("{PLUGIN_DIR_FLAG}={}", dir.display())
}

/// `claude` arguments that load the mod at `dir` too, unless they already do.
fn with_mod(args: Vec<String>, dir: &Path) -> Vec<String> {
    let flag = plugin_flag(dir);
    if args.iter().take_while(|word| *word != "--").any(|word| *word == flag) {
        return args;
    }
    std::iter::once(flag).chain(args).collect()
}

/// The environment an agent needs for the mod to load and reach the worker at `socket`.
#[must_use]
pub fn env(socket: &Path) -> Vec<(String, String)> {
    vec![
        (FUNCTION_HOOKS_ENV.to_owned(), "1".to_owned()),
        (NONESSENTIAL_TRAFFIC_ENV.to_owned(), String::new()),
        (SOCKET_ENV.to_owned(), socket.to_string_lossy().into_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|w| (*w).to_owned()).collect()
    }

    /// Written once, read back whole; a second install finds it; a damaged file is rewritten.
    #[test]
    fn the_mod_is_written_under_its_digest() {
        let data = tempfile::tempdir().expect("a temp dir");
        let dir = install(data.path()).expect("installed");
        assert_eq!(dir, data.path().join("claude-mod").join(digest()));
        assert_eq!(digest().len(), 16);
        for (path, content) in FILES {
            assert_eq!(std::fs::read_to_string(dir.join(path)).expect("written"), content);
        }
        assert_eq!(install(data.path()).expect("again"), dir);
        std::fs::write(dir.join("hooks/register.ts"), "// damaged\n").expect("damage");
        assert_eq!(install(data.path()).expect("repaired"), dir);
        let register = std::fs::read_to_string(dir.join("hooks/register.ts")).expect("read");
        assert_eq!(register, FILES[2].1);
        let names: Vec<_> = std::fs::read_dir(data.path().join("claude-mod"))
            .expect("listed")
            .map(|e| e.expect("an entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(digest())], "no staging left behind");
    }

    /// The flag goes first, in its `=` form, once; words after `--` are the prompt's.
    #[test]
    fn an_agent_loads_the_mod_once() {
        let dir = Path::new("/data/claude-mod/0123");
        let flag = "--plugin-dir=/data/claude-mod/0123";
        let out = with_mod(words(&["--settings", "{}", "fix it"]), dir);
        assert_eq!(out, words(&[flag, "--settings", "{}", "fix it"]));
        assert_eq!(with_mod(out.clone(), dir), out, "already loaded");
        let prompt = words(&["--", flag]);
        assert_eq!(with_mod(prompt, dir), words(&[flag, "--", flag]), "a prompt that names it");
    }

    /// The mod loads, reaches the socket, and is never silenced by an inherited switch.
    #[test]
    fn an_agents_environment_enables_the_mod() {
        let env = env(Path::new("/tmp/slopty/worker.mod.sock"));
        let get = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        assert_eq!(get(FUNCTION_HOOKS_ENV), Some("1"));
        assert_eq!(get(NONESSENTIAL_TRAFFIC_ENV), Some(""));
        assert_eq!(get(SOCKET_ENV), Some("/tmp/slopty/worker.mod.sock"));
    }
}
