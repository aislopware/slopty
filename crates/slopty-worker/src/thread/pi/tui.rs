//! pi's own TUI holding a session: it runs in one of the worker's terminals, and the thread
//! follows the entries it appends to the session's file until it exits or the session is taken
//! back once it rests.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slopty_agent::pi::driven::{self, Driven};
use slopty_agent::pi::rpc::{Entries, Entry};
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::{ThreadId, ThreadMeta, ThreadState};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::sync::mpsc;

use super::{Ended, Next, ThreadAsk};
use crate::thread::Host;

/// How often the session's file is looked at while the TUI holds it: a stat, and a read only
/// of what it grew by.
pub const FOLLOW: Duration = Duration::from_millis(250);

pub use crate::thread::terminals::{Pending, Terminals};

/// Where pi keeps the session of `meta`: the file pi named, else the one of its id in pi's
/// session directory for the thread's folder (`<pi dir>/sessions/--<folder>--/<time>_<id>.jsonl`).
#[must_use]
pub fn session_file(meta: &ThreadMeta) -> Option<PathBuf> {
    if let Some(file) = meta.facts.get(driven::SESSION_FILE_FACT) {
        return Some(PathBuf::from(file));
    }
    let agent = std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi").join("agent"))
    })?;
    let folder = format!("--{}--", meta.cwd.trim_start_matches('/').replace(['/', '\\', ':'], "-"));
    let ending = format!("_{}.jsonl", meta.native);
    std::fs::read_dir(agent.join("sessions").join(folder))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(&ending))
}

/// The session's file as it is read: how far, the line begun and not ended, and every entry.
#[derive(Debug, Default)]
struct Reading {
    offset: u64,
    partial: Vec<u8>,
    entries: Vec<Entry>,
}

impl Reading {
    /// What `file` has gained since the last read: its new entries, or `None` when the thread
    /// is to be read again from the start: the file no longer holds what was read (it shrank,
    /// or is gone), or, while `following`, an entry is not on from the last (the person went to
    /// another branch).
    async fn more(&mut self, file: &Path, following: bool) -> Option<Vec<Entry>> {
        let len = tokio::fs::metadata(file).await.map_or(0, |m| m.len());
        if len < self.offset {
            *self = Self::default();
            return None;
        }
        if len == self.offset {
            return Some(Vec::new());
        }
        let mut read = Vec::new();
        let opened = tokio::fs::File::open(file).await;
        if let Ok(mut opened) = opened
            && opened.seek(SeekFrom::Start(self.offset)).await.is_ok()
        {
            let _read = opened.read_to_end(&mut read).await;
        }
        self.offset = self.offset.saturating_add(u64::try_from(read.len()).unwrap_or(0));
        self.partial.extend_from_slice(&read);
        let mut fresh = Vec::new();
        while let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=at).collect();
            // The header and anything else that is no entry of the tree are passed over.
            if let Ok(entry) = serde_json::from_slice::<Entry>(&line)
                && entry.kind != "session"
            {
                let on = entry.parent_id.as_deref() == self.entries.last().map(|e| e.id.as_str());
                self.entries.push(entry.clone());
                fresh.push(entry);
                if following && !on {
                    return None;
                }
            }
        }
        Some(fresh)
    }
}

/// The TUI in `terminal` holding `thread`'s session.
pub(super) struct Watch {
    host: Host,
    thread: ThreadId,
    terminal: SessionId,
}

impl Watch {
    /// The TUI in `terminal` holding `thread`'s session in `host`.
    pub(super) const fn new(host: Host, thread: ThreadId, terminal: SessionId) -> Self {
        Self { host, thread, terminal }
    }

    /// Follow the session until the TUI exits (the session rests with Slopty, its pi exited) or
    /// it is taken back once it rests (its TUI ended, then driven again).
    pub(super) async fn follow(
        self,
        terminals: std::sync::Arc<dyn Terminals>,
        mut asks: mpsc::UnboundedReceiver<ThreadAsk>,
        ended: Ended,
    ) {
        let Self { host, thread, terminal } = self;
        let Some((state, _)) = host.state(thread) else { return };
        let file = session_file(&state.meta);
        let mut reading = Reading::default();
        let mut driven = read_whole(&host, &state, terminal, file.as_deref(), &mut reading).await;
        let exited = terminals.exited(terminal);
        tokio::pin!(exited);
        let mut tick = tokio::time::interval(FOLLOW);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut take_back = false;
        let next = loop {
            if take_back && driven.rests() {
                terminals.close(terminal).await;
                exited.as_mut().await;
                break Next::Driven;
            }
            tokio::select! {
                () = exited.as_mut() => break Next::Rest,
                ask = asks.recv() => match ask {
                    Some(ThreadAsk::TakeBack) => take_back = true,
                    Some(ask) => tracing::debug!(%thread, ?ask, "an ask while pi's TUI holds it"),
                    // The worker goes; the TUI is the person's and stays.
                    None => return,
                },
                _ = tick.tick() => {
                    let Some(file) = &file else { continue };
                    if let Some(fresh) = reading.more(file, true).await {
                        for entry in &fresh {
                            host.apply(thread, driven.appended(entry, WallMs::now()));
                        }
                    } else {
                        let Some((state, _)) = host.state(thread) else { return };
                        driven = read_whole(&host, &state, terminal, Some(file), &mut reading).await;
                    }
                }
            }
        };
        // Whatever it wrote last is the thread's before Slopty holds the session again.
        if let Some(file) = &file
            && let Some(fresh) = reading.more(file, true).await
        {
            for entry in &fresh {
                host.apply(thread, driven.appended(entry, WallMs::now()));
            }
        }
        let mut actions = driven.held_by_slopty();
        if next == Next::Rest {
            actions.extend(driven.exited(None, WallMs::now()));
        }
        host.apply(thread, actions);
        asks.close();
        let mut left = Vec::new();
        while let Ok(ask) = asks.try_recv() {
            left.push(ask);
        }
        let _gone = ended.send((thread, left, next));
    }
}

/// `state`'s thread read whole from the session's `file` as held by the TUI in `terminal`,
/// and the codec that goes on from there.
async fn read_whole(
    host: &Host,
    state: &ThreadState,
    terminal: SessionId,
    file: Option<&Path>,
    reading: &mut Reading,
) -> Driven {
    let now = WallMs::now();
    let (mut driven, mut actions) = Driven::of(&state.meta, now);
    actions.extend(driven.held_by_tui(terminal, now));
    *reading = Reading::default();
    if let Some(file) = file {
        let _fresh = reading.more(file, false).await;
    }
    let entries = Entries { entries: reading.entries.clone(), leaf_id: None };
    actions.extend(driven.observed(&entries, now));
    if let Err(e) = host.reset(state.meta.id, ThreadState::new(driven.meta().clone())) {
        tracing::warn!(thread = %state.meta.id, "a pi thread could not be read again: {e}");
    }
    host.apply(state.meta.id, actions);
    driven
}
