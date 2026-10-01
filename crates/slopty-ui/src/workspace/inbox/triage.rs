//! The inbox by keyboard, as Linear's and Superhuman's are worked: ↑/↓ and J/K walk the rows,
//! ↵ goes to one, E marks it done and goes on to the next, H snoozes it, U marks a read one
//! unread again, and ⌘↵ / ⌘⌫ allow or deny a held prompt. Each of E, H and U says what it did
//! in a notice with "Undo" for [`UNDO_FOR`].
//!
//! A snoozed finish leaves *Unread* and the bell until the session finishes again or
//! [`SNOOZE_FOR`] has passed. Only a finish can be snoozed: an agent that needs the person stays
//! where they see it until they answer it.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

use std::time::{Duration, Instant};

use gpui::{Context, Window, actions};
use slopty_core::SessionId;
use slopty_proto::conversation::Verdict;

use super::super::toast::ToastKind;
use super::super::{Finished, WorkspaceView};
use super::{Go, Row};

actions!(
    inbox,
    [
        /// Select the next row.
        SelectNext,
        /// Select the row above.
        SelectPrevious,
        /// Go to the selected row's tile, and close the inbox.
        Open,
        /// Mark the selected finish read, and select the next row.
        MarkDone,
        /// Snooze the selected finish until its session finishes again, or for an hour.
        Snooze,
        /// Mark the selected read finish unread again.
        MarkUnread,
        /// Allow the selected row's held prompt.
        Allow,
        /// Deny the selected row's held prompt.
        Deny,
        /// Close the inbox.
        Close,
    ]
);

/// The key context the inbox's list holds while it is up.
pub(crate) const CTX: &str = "Inbox";

/// How long a triage notice offers its undo.
pub(in crate::workspace) const UNDO_FOR: Duration = Duration::from_secs(10);

/// How long a snooze lasts when the session does not finish again first.
pub(in crate::workspace) const SNOOZE_FOR: Duration = Duration::from_hours(1);

/// What a triage verb did, kept for its notice's "Undo".
#[derive(Clone, Debug)]
pub(in crate::workspace) enum Undo {
    /// A finish marked read: it comes back as it was.
    Done(SessionId, Finished),
    /// A finish snoozed: it is unread again at once.
    Snoozed(SessionId),
    /// A read finish marked unread: it is read again.
    Unread(SessionId),
}

impl Undo {
    /// What the notice says it did.
    pub(in crate::workspace) const fn line(&self) -> &'static str {
        match self {
            Self::Done(..) => "Marked done",
            Self::Snoozed(_) => "Snoozed for an hour",
            Self::Unread(_) => "Marked unread",
        }
    }
}

impl WorkspaceView {
    /// The inbox's focus handle, made the first time it opens.
    pub(in crate::workspace) fn inbox_focus(&mut self, cx: &Context<Self>) -> gpui::FocusHandle {
        self.inbox.focus.get_or_insert_with(|| cx.focus_handle()).clone()
    }

    /// Whether `session`'s finish is snoozed now.
    pub(in crate::workspace) fn snoozed(&self, session: SessionId) -> bool {
        self.inbox.snoozed.get(&session).is_some_and(|until| Instant::now() < *until)
    }

    /// `session` finished again: a snooze on it is over.
    pub(in crate::workspace) fn wake_snoozed(&mut self, session: SessionId) {
        self.inbox.snoozed.remove(&session);
    }

    /// The rows in the order the list shows them: what needs you, then the finishes.
    fn triage_rows(&self) -> Vec<Row> {
        let (waiting, finished) = self.inbox_rows(self.inbox.all);
        waiting.into_iter().chain(finished).collect()
    }

    /// The selected row, or the first while none is.
    fn selected_row(&self) -> Option<Row> {
        let rows = self.triage_rows();
        let at = self.inbox_selected_at(&rows).unwrap_or(0);
        rows.into_iter().nth(at)
    }

    /// Where the selection stands among `rows`.
    pub(in crate::workspace) fn inbox_selected_at(&self, rows: &[Row]) -> Option<usize> {
        let selected = self.inbox.selected.as_deref()?;
        rows.iter().position(|row| row.id == selected)
    }

    /// Select the row `by` steps from the selection, stopping at either end.
    fn step_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows = self.triage_rows();
        let last = rows.len().saturating_sub(1);
        let at = match self.inbox_selected_at(&rows) {
            Some(at) => at.saturating_add_signed(by).min(last),
            None if by < 0 => last,
            None => 0,
        };
        self.inbox.selected = rows.get(at).map(|row| row.id.clone());
        cx.notify();
    }

    pub(in crate::workspace) fn inbox_next(
        &mut self,
        _: &SelectNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_selection(1, cx);
    }

    pub(in crate::workspace) fn inbox_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_selection(-1, cx);
    }

    pub(in crate::workspace) fn inbox_open(
        &mut self,
        _: &Open,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row() else { return };
        self.close_menu(window, cx);
        self.go_row(row.go, cx);
    }

    /// Go where a row leads.
    pub(in crate::workspace) fn go_row(&mut self, go: Go, cx: &mut Context<Self>) {
        match go {
            Go::Waiting(waiting) => self.go_to_waiting(waiting, cx),
            Go::Session(session) => self.reveal_session(session, cx),
        }
    }

    /// The selection moves past `row` to the next one, or the one before at the end.
    fn select_after(&mut self, row: &Row) {
        let rows = self.triage_rows();
        let at = rows.iter().position(|r| r.id == row.id);
        let next = at
            .and_then(|at| rows.get(at.saturating_add(1)).or_else(|| rows.get(at.checked_sub(1)?)));
        self.inbox.selected = next.map(|r| r.id.clone());
    }

    pub(in crate::workspace) fn inbox_done(
        &mut self,
        _: &MarkDone,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row().filter(|row| row.unread) else { return };
        let Go::Session(session) = row.go else { return };
        self.select_after(&row);
        if let Some(done) = self.finished.remove(&session) {
            self.triaged(Undo::Done(session, done), cx);
        }
    }

    pub(in crate::workspace) fn inbox_snooze(
        &mut self,
        _: &Snooze,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row().filter(|row| row.unread) else { return };
        let Go::Session(session) = row.go else { return };
        self.select_after(&row);
        let Some(until) = Instant::now().checked_add(SNOOZE_FOR) else { return };
        self.inbox.snoozed.insert(session, until);
        self.triaged(Undo::Snoozed(session), cx);
    }

    pub(in crate::workspace) fn inbox_unread(
        &mut self,
        _: &MarkUnread,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row().filter(|row| !row.unread) else { return };
        let Some(done) = row.logged.and_then(|seq| self.logged_finish(seq)) else { return };
        let Go::Session(session) = row.go else { return };
        self.finished.insert(session, done);
        self.wake_snoozed(session);
        self.triaged(Undo::Unread(session), cx);
    }

    pub(in crate::workspace) fn inbox_allow(
        &mut self,
        _: &Allow,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((session, ask)) = self.selected_row().and_then(|row| row.approval) {
            self.answer_approval(session, ask, Verdict::Allow, cx);
        }
    }

    pub(in crate::workspace) fn inbox_deny(
        &mut self,
        _: &Deny,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((session, ask)) = self.selected_row().and_then(|row| row.approval) {
            let verdict = Verdict::Deny { message: String::new(), interrupt: false };
            self.answer_approval(session, ask, verdict, cx);
        }
    }

    pub(in crate::workspace) fn inbox_close(
        &mut self,
        _: &Close,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(window, cx);
    }

    /// Say what a verb did, with its undo.
    fn triaged(&mut self, undo: Undo, cx: &mut Context<Self>) {
        self.show_toast_for(ToastKind::Triaged(undo), UNDO_FOR, cx);
        cx.notify();
    }

    /// Put back what `undo` names.
    pub(in crate::workspace) fn undo_triage(&mut self, undo: Undo, cx: &mut Context<Self>) {
        match undo {
            Undo::Done(session, done) => {
                self.finished.insert(session, done);
            }
            Undo::Snoozed(session) => self.wake_snoozed(session),
            Undo::Unread(session) => {
                self.finished.remove(&session);
            }
        }
        cx.notify();
    }
}
