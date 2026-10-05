//! Who wrote a line of the review: at the end of the line under the pointer, the turn that
//! brought it in, or the thread when another did; a press opens that thread at that turn.
//!
//! Each file's authors are asked of the worker for the file as the diff ends
//! ([`Stamp::Blob`]): a file that has moved on since says
//! nothing, since its lines would be numbered otherwise. The first files are asked as the review
//! comes; the rest as the pointer first crosses one of their lines.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_client::threads::Stamp;
use slopty_proto::thread::ThreadId;
use slopty_proto::thread::wire::Authors;

use super::{ReviewEvent, ReviewView};
use crate::authorship::{self, Authored, Opens, Writer};

/// The files asked as a review comes, in the list's order; the rest wait for the pointer.
const ASKED_FIRST: usize = 16;

impl ReviewView {
    /// Ask who wrote the lines of the first files listed, and take what is held already.
    pub(super) fn author_review(&mut self, cx: &mut Context<Self>) {
        self.authored.clear();
        self.authors_asked.clear();
        let first: Vec<usize> =
            self.model.listed().iter().map(|l| l.at).take(ASKED_FIRST).collect();
        for at in first {
            self.author_file(at, cx);
        }
    }

    /// The worker said who wrote some file's lines: take any that are this review's.
    pub(super) fn authors_came(&mut self, cx: &mut Context<Self>) {
        let asked: Vec<usize> = self.authors_asked.iter().copied().collect();
        for at in asked {
            self.author_file(at, cx);
        }
    }

    /// Who wrote the lines of the file at `at`, as its diff ends: held, or asked once.
    pub(super) fn author_file(&mut self, at: usize, cx: &mut Context<Self>) {
        if self.authored.contains_key(&at) {
            return;
        }
        let Some(file) = self.model.file(at).filter(|f| !f.binary) else { return };
        let Some(blob) = file.to.clone() else { return };
        let path = file.path.clone();
        self.authors_asked.insert(at);
        let thread = self.own();
        let stamp = Stamp::Blob(blob);
        let held = self.hub.update(cx, |hub, cx| hub.authors(thread, &path, &stamp, cx));
        if let Some(authors) = held {
            let authored = self.with_writers(authors, cx);
            self.authored.insert(at, authored);
            cx.notify();
        }
    }

    /// The threads the lines' authors name, for the host to name ([`Self::set_writers`]).
    #[must_use]
    pub fn authoring_threads(&self) -> Vec<ThreadId> {
        let mut threads: Vec<ThreadId> =
            self.authored.values().flat_map(|a| a.authors.runs.iter().map(|r| r.thread)).collect();
        threads.sort_unstable();
        threads.dedup();
        threads
    }

    /// Name the threads the lines' authors name as every other surface does; a thread left
    /// out goes by its row in this worker's table.
    pub fn set_writers(&mut self, writers: HashMap<ThreadId, Writer>, cx: &mut Context<Self>) {
        if self.writers != writers {
            self.writers = writers;
            cx.notify();
        }
    }

    /// `authors` with the names of its threads, as this worker's table knows them.
    fn with_writers(&self, authors: Arc<Authors>, cx: &App) -> Authored {
        let rows = &self.hub.read(cx).threads().rows().rows;
        let writers = authors
            .runs
            .iter()
            .filter_map(|run| {
                let row = rows.get(&run.thread)?;
                Some((run.thread, Writer { agent: row.agent.clone(), title: row.title.clone() }))
            })
            .collect();
        Authored { authors, writers }
    }

    /// The pointer came onto row `row` (a file, a hunk, a place), or left it: its author shows
    /// while it is there, and its file's authors are asked the first time.
    pub(super) fn hover_line(
        &mut self,
        row: (usize, usize, usize),
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            if !self.authors_asked.contains(&row.0) {
                self.author_file(row.0, cx);
            }
            if self.hovered != Some(row) {
                self.hovered = Some(row);
                cx.notify();
            }
        } else if self.hovered == Some(row) {
            self.hovered = None;
            cx.notify();
        }
    }

    /// The tag of row `row`, whose line on the new side is numbered `line`, while the pointer
    /// is on it; nothing for a line no thread is known to have written.
    pub(super) fn author_tag(
        &self,
        row: (usize, usize, usize),
        line: Option<u32>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if self.hovered != Some(row) {
            return None;
        }
        let (at, hunk, ix) = row;
        let authored = self.authored.get(&at)?;
        let run = authored.at(line?)?;
        let writer = self.writers.get(&run.thread).or_else(|| authored.writers.get(&run.thread));
        let own = self.own();
        let opens = Opens { thread: run.thread, turn: run.turn };
        let id = SharedString::from(format!("review-author-{at}-{hunk}-{ix}"));
        let tag = authorship::tag(&self.theme, id, run, writer, own, crate::clock::now(cx))
            .bg(crate::colors::hsla(self.theme.content()))
            // A press on the tag is the tag's: it starts no comment on the line under it.
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .when(writer.is_some() || own == Some(run.thread), |el| {
                el.on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(ReviewEvent::OpenThread(opens));
                }))
            });
        Some(
            div()
                .absolute()
                .top_0()
                .right(self.z(self.theme.spacing.sm))
                .child(tag)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
impl ReviewView {
    /// Hold `authors` as the answer for the file at `at`.
    pub(crate) fn take_authors(&mut self, at: usize, authors: Authors, cx: &mut Context<Self>) {
        let authored = self.with_writers(Arc::new(authors), cx);
        self.authored.insert(at, authored);
        cx.notify();
    }

    /// The name the host gave `thread`.
    pub(crate) fn writer(&self, thread: ThreadId) -> Option<&Writer> {
        self.writers.get(&thread)
    }
}
