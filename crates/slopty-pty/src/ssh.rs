//! `ssh` out of a Slopty shell, so the far side knows the terminal (ghostty's `+ssh`, its
//! `ssh-env` and `ssh-terminfo` features together).
//!
//! A Slopty shell has `TERM=xterm-ghostty`, which a host without that terminfo entry does not
//! know: vim, less and htop there would run against an unknown terminal. The shell integration
//! defines an `ssh` function that runs `slopty ssh -- <args>`, which calls [`prepare`]:
//!
//! 1. An `ssh` that opens no interactive session (a remote command without `-t`, `-N`, `-W`, `-O`,
//!    `-G`, `-V`, `-Q`) or a `TERM` that is not ours passes through untouched.
//! 2. `ssh -G <args>` names the destination (`user@hostname`, `~/.ssh/config` applied).
//! 3. Unless the cache says the destination has this very entry, the entry is piped to `tic -x -`
//!    there over one `ssh` of its own; success is cached.
//! 4. The session runs with `TERM=xterm-ghostty` when the entry is there, else `xterm-256color`,
//!    and asks to send `COLORTERM` and `TERM_PROGRAM` along.
//!
//! [`NO_TERMINFO`] keeps remote hosts untouched: the session gets `xterm-256color`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt as _;

use crate::terminfo;

/// Set (to anything but `0` or empty) to never install the entry on a remote host.
pub const NO_TERMINFO: &str = "SLOPTY_NO_SSH_TERMINFO";

/// The `TERM` a host without our entry is given: what every host knows.
pub const FALLBACK_TERM: &str = "xterm-256color";

/// The variables the session asks to send; a server lists them in `AcceptEnv` to take them.
const SEND_ENV: [&str; 3] = ["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];

/// Run on the remote host with the entry on its standard input. `sh -c` so a fish or csh login
/// shell reads it the same; nothing when `tic` is missing, which fails the install.
const REMOTE_TIC: &str = "sh -c 'command -v tic >/dev/null 2>&1 && mkdir -p \"$HOME/.terminfo\" && tic -x - 2>/dev/null'";

/// ssh's options that take a value (OpenSSH 10's `getopt` string, the letters before a `:`).
const TAKES_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// Options after which no session with a terminal follows: they query, control or forward.
const NO_SESSION: [char; 6] = ['G', 'N', 'O', 'Q', 'V', 'W'];

/// How `slopty ssh` runs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Options {
    /// The `ssh` to run: a path, or a name looked up on `PATH`.
    pub ssh: PathBuf,
    /// The file recording which destinations have which entry.
    pub cache: PathBuf,
    /// Install the entry on a host that lacks it; `false` gives every host [`FALLBACK_TERM`].
    pub install: bool,
    /// The shell's `TERM`: only ours is changed.
    pub term: Option<String>,
}

impl Options {
    /// `ssh` from `PATH`, the cache under `data_dir`, and the environment's `TERM` and
    /// [`NO_TERMINFO`].
    #[must_use]
    pub fn from_env(data_dir: &Path) -> Self {
        let opted_out = std::env::var(NO_TERMINFO).is_ok_and(|v| !v.is_empty() && v != "0");
        Self {
            ssh: PathBuf::from("ssh"),
            cache: data_dir.join("ssh-terminfo"),
            install: !opted_out,
            term: std::env::var("TERM").ok(),
        }
    }
}

/// An `ssh` command line cut where ssh cuts it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Split {
    /// The options and the destination, in their order.
    pub connect: Vec<OsString>,
    /// Whether a destination was found.
    pub destination: bool,
    /// The remote command, empty for a login shell.
    pub command: Vec<OsString>,
    /// Option letters given, values aside.
    pub flags: String,
}

impl Split {
    /// Whether the session gets a terminal whose `TERM` the remote side reads.
    #[must_use]
    pub fn interactive(&self) -> bool {
        let tty = self.command.is_empty() || self.flags.contains('t');
        self.destination && tty && !self.flags.contains('T') && !self.flags.contains(NO_SESSION)
    }
}

/// Cut `args` the way ssh reads them: options, the destination, more options (ssh parses a
/// second round after the destination), then the command. `--` ends the options.
#[must_use]
pub fn split(args: &[OsString]) -> Split {
    let mut out = Split::default();
    let mut options_done = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let bytes = arg.as_encoded_bytes();
        if !options_done && bytes == b"--" {
            options_done = true;
            out.connect.push(arg.clone());
            continue;
        }
        let is_option = !options_done && bytes.len() > 1 && bytes.first() == Some(&b'-');
        if !is_option {
            if out.destination {
                out.command.push(arg.clone());
                out.command.extend(rest.cloned());
                break;
            }
            out.destination = true;
            out.connect.push(arg.clone());
            continue;
        }
        out.connect.push(arg.clone());
        let letters = bytes.get(1..).unwrap_or_default();
        for (at, &letter) in letters.iter().enumerate() {
            out.flags.push(char::from(letter));
            if TAKES_VALUE.as_bytes().contains(&letter) {
                // The value is the rest of the cluster, else the next argument.
                if at.saturating_add(1) == letters.len()
                    && let Some(value) = rest.next()
                {
                    out.connect.push(value.clone());
                }
                break;
            }
        }
    }
    out
}

/// `user@hostname` from `ssh -G`'s output, `None` without both.
#[must_use]
pub fn destination(config: &str) -> Option<String> {
    let value = |key: &str| {
        config.lines().find_map(|line| {
            let (k, v) = line.split_once(' ')?;
            (k == key && !v.is_empty()).then_some(v)
        })
    };
    Some(format!("{}@{}", value("user")?, value("hostname")?))
}

/// What the cache records for the entry as it is now: a digest of its source, so a changed
/// entry is installed again.
#[must_use]
pub fn entry_version() -> String {
    let digest = blake3::hash(terminfo::source().as_bytes());
    digest.to_hex().get(..16).unwrap_or_default().to_owned()
}

/// Whether `cache` says `dest` has entry `version`.
#[must_use]
pub fn cached(cache: &Path, dest: &str, version: &str) -> bool {
    std::fs::read_to_string(cache)
        .is_ok_and(|text| text.lines().any(|line| line.split_once(' ') == Some((dest, version))))
}

/// Record that `dest` has entry `version`, replacing what was known of it.
///
/// # Errors
///
/// When the file cannot be written.
pub fn remember(cache: &Path, dest: &str, version: &str) -> io::Result<()> {
    let text = std::fs::read_to_string(cache).unwrap_or_default();
    let mut lines: Vec<&str> =
        text.lines().filter(|line| line.split_once(' ').is_none_or(|(d, _)| d != dest)).collect();
    let line = format!("{dest} {version}");
    lines.push(&line);
    if let Some(dir) = cache.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(cache, text)
}

/// What [`prepare`] decided, for the tests and a verbose word.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Not an interactive session, or not our `TERM`: ssh as typed.
    Untouched,
    /// The entry was there already (by the cache).
    Cached(String),
    /// The entry was installed now.
    Installed(String),
    /// No entry there: [`FALLBACK_TERM`], with why.
    Fallback(String),
}

/// The `ssh` to run for `args`, and what was decided.
///
/// The entry is installed on the destination first when it was missing. `note` gets what the
/// person should read, before the install (which may ask for a password) and after a failure.
pub async fn prepare(
    opts: &Options,
    args: &[OsString],
    note: &mut dyn FnMut(&str),
) -> (std::process::Command, Decision) {
    let mut ssh = std::process::Command::new(&opts.ssh);
    let parts = split(args);
    let ours = opts.term.as_deref() == Some(terminfo::NAMES[0]);
    if !ours || !parts.interactive() {
        ssh.args(args);
        return (ssh, Decision::Untouched);
    }
    let decision = decide(opts, &parts, note).await;
    let term = match decision {
        Decision::Cached(_) | Decision::Installed(_) => terminfo::NAMES[0],
        Decision::Untouched | Decision::Fallback(_) => FALLBACK_TERM,
    };
    ssh.env("TERM", term);
    for var in SEND_ENV {
        ssh.arg("-o").arg(format!("SendEnv={var}"));
    }
    ssh.args(args);
    (ssh, decision)
}

async fn decide(opts: &Options, parts: &Split, note: &mut dyn FnMut(&str)) -> Decision {
    if !opts.install {
        return Decision::Fallback(format!("{NO_TERMINFO} is set"));
    }
    let Some(dest) = resolve(&opts.ssh, &parts.connect).await else {
        return Decision::Fallback("ssh -G named no destination".to_owned());
    };
    let version = entry_version();
    if cached(&opts.cache, &dest, &version) {
        return Decision::Cached(dest);
    }
    note(&format!("setting up the xterm-ghostty terminfo on {dest}"));
    match install(&opts.ssh, &parts.connect).await {
        Ok(()) => {
            if let Err(e) = remember(&opts.cache, &dest, &version) {
                note(&format!("could not note it in {}: {e}", opts.cache.display()));
            }
            Decision::Installed(dest)
        }
        Err(e) => {
            note(&format!("{e}; this session uses {FALLBACK_TERM}"));
            Decision::Fallback(e.to_string())
        }
    }
}

/// `user@hostname` for `connect`, as `ssh -G` resolves it.
async fn resolve(ssh: &Path, connect: &[OsString]) -> Option<String> {
    let out = tokio::process::Command::new(ssh)
        .arg("-G")
        .args(connect)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    destination(&String::from_utf8_lossy(&out.stdout))
}

/// Compile the entry into the remote user's database over one `ssh` of its own.
async fn install(ssh: &Path, connect: &[OsString]) -> io::Result<()> {
    let mut child = tokio::process::Command::new(ssh)
        .args(connect)
        .arg(REMOTE_TIC)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        // ssh asks for a password or a host key on the terminal itself, not on these.
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A host that closes early answers through the exit status below.
        let _written: io::Result<()> = stdin.write_all(terminfo::source().as_bytes()).await;
        let _closed: io::Result<()> = stdin.shutdown().await;
    }
    let status = child.wait().await?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("the remote tic failed ({status})")))
    }
}

/// `args` as `OsString`s, for tests and callers holding `&str`s.
#[must_use]
pub fn os_args<S: AsRef<OsStr>>(args: &[S]) -> Vec<OsString> {
    args.iter().map(|a| a.as_ref().to_owned()).collect()
}

#[cfg(test)]
mod tests;
