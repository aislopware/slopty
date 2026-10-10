//! An agent played into a terminal as its worker would report it: the hook-level status a test
//! names goes through the worker's own codec (`slopty_agent::observed`) into the row of the
//! thread that names the terminal, and reaches the workspace as a frame of the thread table.
//! The workspace reads agents only from those rows.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::Context;
use slopty_agent::observed::{ASK_GRACE, Observed, Out, terminal_thread};
use slopty_agent::status::{AgentEvent, AgentStatus};
use slopty_client::layout::WorkerKey;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::attention::{Ladder, Ranked, Rung, ThreadAt};
use slopty_proto::thread::wire::{TableFrame, TurnEnded};
use slopty_proto::thread::{Cursor, ThreadState, TurnId};

use crate::workspace::WorkspaceView;

/// Each played terminal's codec and the thread it keeps, and each server-reported one's place
/// on the ladder, per test thread.
#[derive(Default)]
struct Played {
    codecs: HashMap<SessionId, (Observed, Option<ThreadState>)>,
    /// Each terminal's turns as its transcript would give them to the worker (hooks alone carry
    /// none): how many ended, when the one under way began, and the last that ended.
    turns: HashMap<SessionId, (u32, Option<WallMs>, Option<TurnEnded>)>,
    ladder: Vec<Ranked>,
    seq: u64,
}

thread_local! {
    static PLAYED: RefCell<Played> = RefCell::new(Played::default());
}

/// A test's way to say what a terminal's agent does.
pub(crate) trait Agents {
    /// `event` as the worker's codec maps it, into the row of the thread that names its
    /// terminal on the terminal's worker (or a provisional thread of the terminal's own).
    fn agent_event(&mut self, event: AgentEvent, cx: &mut Context<WorkspaceView>) {
        if let Some(key) = self.worker_of(event.session) {
            self.agent_event_on(key, event, cx);
        }
    }

    /// The worker whose table a terminal's agent goes in: the terminal's, else the first.
    fn worker_of(&self, session: SessionId) -> Option<WorkerKey>;

    /// [`Self::agent_event`] on `key`'s table, whether or not a tile here shows the terminal.
    fn agent_event_on(
        &mut self,
        key: WorkerKey,
        event: AgentEvent,
        cx: &mut Context<WorkspaceView>,
    );

    /// `event` as the server's ladder would rank it, for `worker`, which this client may not
    /// reach.
    fn server_agent_event(
        &mut self,
        worker: WorkerKey,
        event: AgentEvent,
        cx: &mut Context<WorkspaceView>,
    );
}

impl Agents for WorkspaceView {
    fn worker_of(&self, session: SessionId) -> Option<WorkerKey> {
        self.worker_of_session(session).or_else(|| self.workers().next().map(|(k, ..)| k))
    }

    fn agent_event_on(
        &mut self,
        key: WorkerKey,
        event: AgentEvent,
        cx: &mut Context<WorkspaceView>,
    ) {
        let session = event.session;
        let (played, seq) = PLAYED.with(|p| {
            let mut p = p.borrow_mut();
            p.seq = p.seq.saturating_add(1);
            let seq = p.seq;
            let (codec, state) = p.codecs.entry(session).or_insert_with(|| {
                (Observed::provisional("2.1.0", session, "/w", WallMs::ZERO), None)
            });
            let grace = u64::try_from(ASK_GRACE.as_millis()).unwrap_or(0).saturating_add(1);
            let later = WallMs::from_millis(event.since_ms.as_millis().saturating_add(grace));
            let mut outs = codec.status(&event);
            outs.extend(codec.waited(later));
            for out in outs {
                match out {
                    Out::Begin(meta) => *state = Some(ThreadState::new(*meta)),
                    Out::Actions(_, actions) => {
                        if let Some(state) = state.as_mut() {
                            for action in &actions {
                                state.apply(action);
                            }
                        }
                    }
                }
            }
            let row = state.as_ref().map(|s| s.row(event.since_ms));
            let (ended, began, last) = p.turns.entry(session).or_default();
            match event.status {
                AgentStatus::Working | AgentStatus::Tool { .. } => {
                    began.get_or_insert(event.since_ms);
                    *last = None;
                }
                AgentStatus::Done => {
                    if let Some(from) = began.take() {
                        *ended = ended.saturating_add(1);
                        let at_ms = event.since_ms;
                        let ran_ms = at_ms.as_millis().saturating_sub(from.as_millis());
                        *last =
                            Some(TurnEnded { turn: TurnId(*ended), at_ms, ran_ms, answered: true });
                    }
                }
                AgentStatus::Blocked(_) | AgentStatus::Waiting { .. } => {}
                _ => *began = None,
            }
            (row.map(|r| slopty_proto::thread::wire::ThreadRow { ended: *last, ..r }), seq)
        });
        let Some(mut played) = played else { return };
        // What the agent said of a turn it ended is its transcript's last line, read by the
        // worker beside the hooks.
        if event.status == AgentStatus::Done
            && let Some(said) = event.detail
        {
            played.last_line = Some(said);
        }
        let row = match self.row_of_terminal(key, session, cx) {
            Some(mut row) => {
                row.status = played.status;
                row.requests = played.requests;
                row.doing = played.doing;
                row.ended = played.ended;
                if played.last_line.is_some() {
                    row.last_line = played.last_line;
                }
                row.meters.mode.clone_from(&played.meters.mode);
                row
            }
            None => played,
        };
        let cursor = Cursor { epoch: 1, seq: 1_000_u64.saturating_add(seq) };
        let frame = TableFrame::Delta { cursor, rows: vec![row], removed: Vec::new() };
        self.thread_table(key, &frame, cx);
        // As the worker's frames come one by one: the workspace hears each, not only the last
        // of a test's update.
        self.threads_of_sessions(key, cx);
    }

    fn server_agent_event(
        &mut self,
        worker: WorkerKey,
        event: AgentEvent,
        cx: &mut Context<WorkspaceView>,
    ) {
        use slopty_agent::status::BlockReason;
        let session = event.session;
        let rung = match &event.status {
            AgentStatus::Blocked(why) if *why != BlockReason::IdlePrompt => Some(Rung::NeedsYou),
            AgentStatus::Failed { .. } => Some(Rung::Failed),
            AgentStatus::Working | AgentStatus::Tool { .. } => Some(Rung::Working),
            AgentStatus::Waiting { .. } => Some(Rung::Waiting),
            AgentStatus::None => None,
            _ => Some(Rung::Idle),
        };
        let ladder = PLAYED.with(|p| {
            let mut p = p.borrow_mut();
            p.ladder.retain(|r| r.terminal != Some(session));
            if let Some(rung) = rung {
                // The server's id for the worker the workspace keys as `worker`.
                let id = format!("{:032x}", worker.value()).parse::<slopty_core::WorkerId>();
                let at =
                    ThreadAt { worker: id.unwrap_or_default(), thread: terminal_thread(session) };
                p.ladder.push(Ranked {
                    at,
                    rung,
                    since_ms: event.since_ms,
                    terminal: Some(session),
                });
            }
            Ladder { threads: p.ladder.clone(), ..Ladder::default() }
        });
        self.server_ladder(&ladder, cx);
    }
}
