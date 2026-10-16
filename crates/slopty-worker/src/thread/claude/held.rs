//! The Claude Code sessions another live Claude Code on this machine holds: one the person runs
//! in a terminal, or one running in the background.
//!
//! Each live Claude Code records itself in `~/.claude/sessions/<pid>.json`
//! ([`slopty_agent::claude::sessions::live`]), and Claude Code's own resume refuses a session
//! such a process holds, since two writers interleave their records in one transcript and each
//! goes on from its own copy of the conversation. Slopty refuses it the same way: a past session
//! held elsewhere is marked so ([`PAST_RUNNING`](slopty_proto::thread::wire::PAST_RUNNING)),
//! opens read from its transcript with nothing started on it, and takes no message until the
//! other ends.
//!
//! A record is believed while its process lives: signal 0 finds it, and when the record says
//! when its process started (`procStart`, `ps -o lstart=` in UTC, as Claude Code writes it and
//! checks it), the process with that pid now started then. A record left by a Claude Code that
//! was killed outright, whose pid another process took since, holds nothing.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::path::Path;
use std::sync::LazyLock;

use parking_lot::Mutex;
use slopty_agent::claude::sessions::{self, Live};

/// A live Claude Code that holds a session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Holder {
    /// Its process.
    pub pid: u32,
    /// How it runs: `interactive` in a terminal, else how it runs in the background.
    pub kind: String,
    /// When its process started, as its record says (`ps -o lstart=` in UTC).
    pub started: Option<String>,
}

impl Holder {
    /// Whether it still holds its session: its process lives, and is the one that wrote.
    #[must_use]
    pub fn lives(&self) -> bool {
        lives(self.pid, self.started.as_deref())
    }

    /// The words a thread of its session says while it holds it, `machine` named.
    #[must_use]
    pub fn words(&self, machine: &str) -> String {
        let place = if self.kind == "interactive" {
            "in another terminal".to_owned()
        } else {
            format!("in the background ({})", self.kind)
        };
        format!("This session is running {place} on {machine}. Close it there to continue here.")
    }
}

/// The sessions live Claude Codes hold under `home`, by session id.
///
/// Those in `ours`, the sessions this worker's own Claude Codes run, are left out. Blocks on
/// the disk and on `ps`.
#[must_use]
pub fn holders<S: BuildHasher>(home: &Path, ours: &HashSet<String, S>) -> HashMap<String, Holder> {
    sessions::live(home)
        .into_iter()
        .filter(|live| !ours.contains(&live.session))
        .map(|Live { pid, session, kind, started, .. }| (session, Holder { pid, kind, started }))
        .filter(|(_session, holder)| holder.lives())
        .collect()
}

/// The live Claude Code holding `session` under `home`, if one does.
///
/// Blocks on the disk and on `ps`.
#[must_use]
pub fn holder(home: &Path, session: &str) -> Option<Holder> {
    holders(home, &HashSet::new()).remove(session)
}

/// Each process found to be the one that wrote its record, by pid and start: a pid cannot be
/// used again while its process lives, so it is asked of `ps` once.
static PROVEN: LazyLock<Mutex<HashSet<(u32, String)>>> = LazyLock::new(Mutex::default);

/// Whether process `pid` lives and, when `started` says when it started, started then.
fn lives(pid: u32, started: Option<&str>) -> bool {
    let Ok(id) = i32::try_from(pid) else { return false };
    if !crate::ports::alive(id) {
        PROVEN.lock().retain(|(proven, _)| *proven != pid);
        return false;
    }
    let Some(started) = started.map(str::trim).filter(|s| !s.is_empty()) else { return true };
    let key = (pid, started.to_owned());
    if PROVEN.lock().contains(&key) {
        return true;
    }
    match start_of(pid) {
        // Not the process that wrote: its pid was used again.
        Some(now) if now != started => false,
        Some(_) => {
            let mut proven = PROVEN.lock();
            proven.retain(|(other, _)| *other != pid);
            proven.insert(key);
            true
        }
        // `ps` could not say: the process is there, and nothing proves it another.
        None => true,
    }
}

/// When process `pid` started, as Claude Code writes it: `ps -o lstart=` in UTC, in the C
/// locale.
fn start_of(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record of `pid` holding `session`, as Claude Code writes it, under `home`.
    fn record(home: &Path, pid: u32, session: &str, started: Option<&str>, kind: &str) {
        let dir = sessions::config_dir(home).join(sessions::LIVE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let mut record = serde_json::json!({
            "pid": pid, "sessionId": session, "cwd": "/w", "kind": kind, "status": "idle",
        });
        if let Some(started) = started {
            record["procStart"] = started.into();
        }
        std::fs::write(dir.join(format!("{pid}.json")), record.to_string()).unwrap();
    }

    /// A session is held while the process that wrote its record lives, and by nothing once it
    /// ended; a record whose pid another process took since (its start differs) holds nothing,
    /// one whose start matches holds, and the worker's own sessions are left out.
    #[test]
    fn a_session_is_held_by_the_live_process_that_wrote_its_record() {
        let home = tempfile::tempdir().unwrap();
        let mut terminal = std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let pid = terminal.id();
        let started = start_of(pid).expect("ps says when it started");
        record(home.path(), pid, "held", Some(&started), "interactive");
        let held = holders(home.path(), &HashSet::new());
        assert_eq!(held.get("held").map(|h| (h.pid, h.kind.as_str())), Some((pid, "interactive")));
        assert!(holder(home.path(), "held").is_some_and(|h| h.lives()));
        let ours = HashSet::from(["held".to_owned()]);
        assert!(holders(home.path(), &ours).is_empty(), "this worker's own is left out");

        record(home.path(), pid, "reused", Some("Thu Jan  1 00:00:00 1970"), "interactive");
        assert!(holder(home.path(), "reused").is_none(), "another process took the pid");
        record(home.path(), pid, "bare", None, "bg");
        let bare = holder(home.path(), "bare").expect("no start to check: its pid lives");
        assert!(bare.words("mac").contains("in the background (bg) on mac"), "{bare:?}");

        terminal.kill().unwrap();
        terminal.wait().unwrap();
        assert!(!bare.lives(), "its process ended");
        assert!(holders(home.path(), &HashSet::new()).is_empty(), "nothing holds it now");
    }

    /// The words name the terminal and the machine, and how to go on.
    #[test]
    fn the_words_say_where_it_runs_and_how_to_go_on() {
        let holder = Holder { pid: 1, kind: "interactive".to_owned(), started: None };
        assert_eq!(
            holder.words("studio"),
            "This session is running in another terminal on studio. Close it there to continue \
             here."
        );
    }
}
