//! Who wrote a line of the review: under the pointer, at the line's end, the turn that brought
//! it in, or the thread when another did; a press opens that thread at that turn.
//!
//! Each file's authors are asked of the worker for the file as the diff ends
//! ([`Stamp::Blob`](slopty_client::threads::Stamp::Blob)): a file that has moved on since says
//! nothing, since its lines would be numbered otherwise. The first files are asked as the review
//! comes; the rest as the pointer first crosses one of their lines.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_client::threads::Stamp;
use slopty_core::WallMs;
use slopty_proto::thread::wire::Authors;

use super::{ReviewEvent, ReviewView};
use crate::authorship::{self, Authored, Opens, Writer};

/// The files asked as a review comes, in the list's order; the rest wait for the pointer.
const ASKED_FIRST: usize = 16;

/// The group a line's row and its tag share: the tag shows while the pointer is on the row.
pub(super) fn line_group(id: &str) -> SharedString {
    SharedString::from(format!("{id}-hover"))
}

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
        let thread = self.thread;
        let stamp = Stamp::Blob(blob);
        let held = self.hub.update(cx, |hub, cx| hub.authors(Some(thread), &path, &stamp, cx));
        if let Some(authors) = held {
            let authored = self.with_writers(authors, cx);
            self.authored.insert(at, authored);
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

    /// The tag of the line numbered `line` on the new side of the file at `at`, shown while
    /// the pointer is on row `group`; nothing for a line no thread is known to have written.
    pub(super) fn author_tag(
        &self,
        at: usize,
        line: Option<u32>,
        group: &str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let authored = self.authored.get(&at)?;
        let run = authored.at(line?)?;
        let writer = authored.writers.get(&run.thread);
        let own = Some(self.thread);
        let opens = Opens { thread: run.thread, turn: run.turn };
        let id = SharedString::from(format!("{group}-author"));
        let tag = authorship::tag(&self.theme, id, run, writer, own, WallMs::now())
            .bg(crate::colors::hsla(self.theme.content()))
            // A press on the tag is the tag's: it starts no comment on the line under it.
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .when(writer.is_some() || run.thread == self.thread, |el| {
                el.on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(ReviewEvent::OpenThread(opens));
                }))
            });
        let group = line_group(group);
        Some(
            div()
                .absolute()
                .top_0()
                .right(self.z(self.theme.spacing.sm))
                .invisible()
                .group_hover(group, gpui::StyleRefinement::visible)
                .child(tag)
                .into_any_element(),
        )
    }
}
