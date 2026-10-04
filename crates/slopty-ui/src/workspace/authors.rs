//! Who wrote a line, across the tiles: each file tile is told who wrote its lines as it read
//! them, and a press on a line's author opens that thread at the turn that wrote it.
//!
//! The answer is asked of the file's worker through its thread hub, once per file as read
//! ([`FileView::stamp`]), and kept there; the names of the threads it gives come from every
//! worker's table here, since a commit may name a thread another machine ran
//! ([`crate::authorship`]).

use gpui::{Context, Entity};
use slopty_client::threads::Stamp;
use slopty_proto::thread::wire::Authors;

use super::WorkspaceView;
use crate::authorship::{Authored, Opens};
use crate::file::FileView;

impl WorkspaceView {
    /// Tell the file tile `view` who wrote its lines as it read them, asking its worker when
    /// that is not known yet.
    pub(super) fn author_file(&mut self, view: &Entity<FileView>, cx: &mut Context<Self>) {
        let (key, path, stamp) = {
            let file = view.read(cx);
            (file.worker(), file.path().to_owned(), file.stamp())
        };
        let authored = match stamp {
            Some(stamp) => {
                let hub = self.thread_hub(key, cx);
                let stamp = Stamp::Modified(stamp);
                hub.update(cx, |hub, cx| hub.authors(None, &path, &stamp, cx))
                    .map(|authors| self.authored(authors))
            }
            None => None,
        };
        if view.read(cx).authored() != authored.as_ref() {
            view.update(cx, |v, cx| v.set_authored(authored, cx));
        }
    }

    /// Every file tile on `key`, told again: its worker said who wrote a file's lines.
    pub(super) fn author_files(
        &mut self,
        key: slopty_client::layout::WorkerKey,
        cx: &mut Context<Self>,
    ) {
        let views: Vec<Entity<FileView>> =
            self.files.values().filter(|v| v.read(cx).worker() == key).cloned().collect();
        for view in views {
            self.author_file(&view, cx);
        }
    }

    /// `authors` with the names of the threads it gives, as the workers here know them.
    pub(super) fn authored(&self, authors: std::sync::Arc<Authors>) -> Authored {
        let writers = authors
            .runs
            .iter()
            .filter_map(|run| Some((run.thread, self.writer_of(run.thread)?)))
            .collect();
        Authored { authors, writers }
    }

    /// Open the thread a line's author names, at the turn that wrote it: in the agent's tile
    /// showing it, or its own tile on the worker whose table holds it.
    pub(super) fn open_thread_at(&mut self, opens: Opens, cx: &mut Context<Self>) {
        let Some(key) = self.worker_of_thread(opens.thread, cx) else {
            self.show_notice("That thread is on a machine not connected here".to_owned(), cx);
            return;
        };
        if let Some(turn) = opens.turn {
            self.go_to_turn_when_shown(opens.thread, turn);
        }
        // Where the agent's own tile shows the thread, there; else the thread's tile.
        match self.thread_session(opens.thread, cx) {
            Some(session) => self.reveal_session(session, cx),
            None => self.open_thread(key, opens.thread, cx),
        }
        cx.notify();
    }

    /// Each open review names its lines' authors as every surface names a thread.
    pub(super) fn settle_review_writers(&self, cx: &mut Context<Self>) {
        let reviews: Vec<_> = self.open_reviews().cloned().collect();
        for view in reviews {
            let writers = view
                .read(cx)
                .authoring_threads()
                .into_iter()
                .filter_map(|t| Some((t, self.writer_of(t)?)))
                .collect();
            view.update(cx, |v, cx| v.set_writers(writers, cx));
        }
    }
}
