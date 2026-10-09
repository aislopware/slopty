//! What a program says of itself with the program status protocol (`OSC 7501`), shown as its
//! tile's state: a record waiting on the person is the tile's "needs you", and a result (done,
//! or failed) is the tile's until the person has looked at it.
//!
//! The records come with the session's summary, so a tile that is not viewed shows them too
//! (`docs/decisions/terminal.md`). An agent's own adapter is the richer source and leads
//! ([`WorkspaceView::tile_status`]); a record outranks what is only inferred, a command's exit or
//! a progress report. A result is looked at when its tile takes the focus, or arrives while it
//! has it.

use slopty_core::SessionId;
use slopty_proto::terminal::{ProgramState, ProgramStatus};

use super::WorkspaceView;
use crate::icons::Status;

/// Whether `record` is a result, which stays for the person to see.
const fn result(record: &ProgramStatus) -> bool {
    matches!(record.state, ProgramState::Done | ProgramState::Error)
}

impl WorkspaceView {
    /// `session`'s status records, as its summary last said them.
    fn program(&self, session: SessionId) -> &[ProgramStatus] {
        self.summary(session).map_or(&[], |s| s.program.as_slice())
    }

    /// The tile state `session`'s records give it: a record waiting on the person is "needs
    /// you"; else a result not yet looked at, failed when one failed.
    pub(super) fn program_mark(&self, session: SessionId) -> Option<Status> {
        let program = self.program(session);
        if program.iter().any(|r| r.state == ProgramState::Blocked) {
            return Some(Status::NeedsYou);
        }
        let seen = self.program_seen.get(&session);
        let mut unseen =
            program.iter().filter(|r| result(r) && seen.is_none_or(|s| !s.contains(r))).peekable();
        unseen.peek()?;
        let failed = unseen.any(|r| r.state == ProgramState::Error);
        Some(if failed { Status::Failed } else { Status::Done })
    }

    /// What `session`'s record waiting on the person needs, in the words a waiting agent's are
    /// said in; `None` when none waits.
    pub(super) fn program_need_word(&self, session: SessionId) -> Option<String> {
        let blocked = self.program(session).iter().find(|r| r.state == ProgramState::Blocked)?;
        let word = match blocked.need.as_deref() {
            Some(ProgramStatus::PERMISSION) => "Needs approval",
            Some(ProgramStatus::QUESTION) => "Has a question",
            Some(ProgramStatus::AUTH) => "Needs a sign-in",
            _ => "Needs you",
        };
        Some(word.to_owned())
    }

    /// What `session`'s program says, for its row's second line: the words of the record that
    /// waits on the person, else of one at work, else of a result not yet looked at. A record's
    /// message, else its title; `None` when the record says neither.
    pub(super) fn program_words(&self, session: SessionId) -> Option<String> {
        let program = self.program(session);
        let seen = self.program_seen.get(&session);
        let first = |state: ProgramState| program.iter().find(|r| r.state == state);
        let unseen = || program.iter().find(|r| result(r) && seen.is_none_or(|s| !s.contains(r)));
        let record = first(ProgramState::Blocked)
            .or_else(|| first(ProgramState::Working))
            .or_else(unseen)?;
        [&record.message, &record.title].into_iter().find(|w| !w.is_empty()).cloned()
    }

    /// The person looked at `session`'s tile: its results are seen.
    pub(super) fn see_program(&mut self, session: SessionId) {
        let results: Vec<ProgramStatus> =
            self.program(session).iter().filter(|r| result(r)).cloned().collect();
        if results.is_empty() {
            self.program_seen.remove(&session);
        } else {
            self.program_seen.insert(session, results);
        }
    }

    /// `session`'s summary changed: a result that arrives while its tile has the focus is seen
    /// as it comes.
    pub(super) fn program_moved(&mut self, session: SessionId) {
        let focused = self.focused().and_then(|tile| self.item(tile)).is_some_and(|item| {
            matches!(item.kind, slopty_proto::items::ItemKind::Terminal { session: s } if s == session)
        });
        if focused {
            self.see_program(session);
        }
    }
}
