//! Drags out of the window onto this Mac: a path a shell printed, ⌘-dragged
//! ([`slopty_terminal::TerminalViewEvent::DragOut`]), and a drag an app on a worker began in its
//! remote window ([`crate::screen::ScreenViewEvent::DragOut`]) (`docs/decisions/audio.md`, "Drag
//! out").
//!
//! The workspace drags them on from the very mouse event that handed them over, as this Mac's
//! own drag: each file a promise kept by bringing it down from the worker when the drop asks,
//! a promised one once the worker's catch has called it in, each data item given from what the
//! catch kept as a target reads it. Each file coming down is listed with the transfers.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_client::clip::Fetched;
use slopty_client::dnd::out::{DataAt, FileAt, Offer, Shared};
use slopty_client::remote::Remote;
use slopty_core::{SessionId, XferId};
use slopty_platform::drag::{Data, Give, Keep, Promise};

use super::{Bringing, Tell, Told, WorkspaceView, bring_down_seen, worker_name};

/// How long a promised file's keeping waits for the worker's catch to name it: an app writes a
/// promised file inside the catch's drop, for as long as it takes.
const CATCH_WAIT: Duration = Duration::from_secs(30);

/// How long a target reading data the catch kept waits for it to come down.
const FETCH_WAIT: Duration = Duration::from_secs(5);

/// How long a target reading data before the worker's catch is in waits for it, on the main
/// thread: the catcher's showing and four carries over it take about a second at most.
const CAUGHT_WAIT: Duration = Duration::from_secs(3);

/// Where a drag's promises go instead of the system's drag: a test's hand.
pub type DragSink = Rc<dyn Fn(Vec<Promise>) -> bool>;

impl WorkspaceView {
    /// Drag the files of a test's drags to `sink` instead of the system.
    pub fn set_drag_sink(&mut self, sink: DragSink) {
        self.drag_sink = Some(sink);
    }

    /// A path shell `session` printed, ⌘-dragged out: the worker's file, the path made absolute
    /// against the shell's directory.
    pub(in crate::workspace) fn drag_shell_path(
        &self,
        session: SessionId,
        path: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.worker_of_session(session) else { return };
        let cwd = self.summary(session).and_then(|s| s.cwd.as_deref());
        let home = self.workers.get(&key).and_then(|w| w.home.as_deref());
        let Some(absolute) = super::super::handoffs::absolute(path, cwd, home) else {
            Self::show_notice(format!("Where {path} is, the shell has not said"), cx);
            return;
        };
        self.drag_out(key, &absolute, cx);
    }

    /// Drag worker `worker`'s file `path` out of the window, brought down when the drop asks for
    /// it. Whether a drag began; a machine that is away is said at once.
    pub fn drag_out(&self, worker: WorkerKey, path: &str, cx: &mut Context<Self>) -> bool {
        let name = worker_name(path).to_owned();
        let Some(remote) = self.remote(worker) else {
            let machine = self.workers.get(&worker).map_or("The machine", |w| w.name.as_str());
            Self::show_notice(format!("{machine} is away; {name} was not dragged out"), cx);
            return false;
        };
        let tell = Self::tell_downloads(worker, Arc::clone(&remote), Bringing::Drag, cx);
        let Some(promise) = promise(remote, path, tell) else { return false };
        self.drag(vec![promise], Vec::new())
    }

    /// Drag what worker `worker`'s drag out carries on from the mouse event being handled.
    /// Whether a drag began.
    pub fn drag_out_of(&self, worker: WorkerKey, shared: &Arc<Shared>, cx: &Context<Self>) -> bool {
        let Some(remote) = self.remote(worker) else { return false };
        remote.watch_drag_out(shared);
        let tell = Self::tell_downloads(worker, Arc::clone(&remote), Bringing::Drag, cx);
        let (mut files, mut data) = (Vec::new(), Vec::new());
        for offer in shared.offers() {
            match offer {
                Offer::File { path: Some(path), .. } => {
                    files.extend(promise(Arc::clone(&remote), &path, tell.clone()));
                }
                Offer::File { name, path: None, promise: Some(n) } => {
                    let (remote, shared) = (Arc::clone(&remote), Arc::clone(shared));
                    let tell = tell.clone();
                    let keep: Keep = Arc::new(move |dest| match shared.promised(n, CATCH_WAIT) {
                        FileAt::At(path) => keep_told(remote.as_ref(), &path, dest, &tell),
                        FileAt::Waiting => {
                            Err("the machine did not catch the drag in time".to_owned())
                        }
                        FileAt::Gone => Err("the app on the machine did not write it".to_owned()),
                    });
                    files.push(Promise { name, keep });
                }
                Offer::File { path: None, promise: None, .. } => {}
                Offer::Data { item, types } => {
                    let (remote, shared) = (Arc::clone(&remote), Arc::clone(shared));
                    let give: Give =
                        Arc::new(move |uti| match shared.data(item, uti, CAUGHT_WAIT) {
                            DataAt::Bytes(bytes) => Some(bytes),
                            DataAt::Fetch(rep) => match remote.clip_fetch(&rep, None, FETCH_WAIT) {
                                Fetched::Data(bytes) => Some(bytes),
                                Fetched::TooBig(_) | Fetched::Gone => None,
                            },
                            DataAt::Waiting | DataAt::Gone => None,
                        });
                    data.push(Data { types, give });
                }
            }
        }
        tracing::info!(drag = %shared.drag(), files = files.len(), data = data.len(), "a drag out goes on here");
        self.drag(files, data)
    }

    /// Hand `files` and `data` to the system's drag, or to a test's sink.
    fn drag(&self, files: Vec<Promise>, data: Vec<Data>) -> bool {
        match &self.drag_sink {
            Some(sink) => sink(files),
            None => slopty_platform::drag::drag_out_items(files, data, None),
        }
    }
}

/// Bring the worker's `source` down to exactly `dest` for a drop, told to `tell` as it begins
/// and ends.
fn keep_told(
    remote: &dyn Remote,
    source: &str,
    dest: &std::path::Path,
    tell: &Tell,
) -> Result<(), String> {
    let xfer = XferId::new();
    let (seen, heard) = tokio::sync::watch::channel(slopty_client::xfer::Brought::default());
    let began =
        Told::Began { xfer, source: source.to_owned(), dest: dest.to_path_buf(), seen: heard };
    let _told = tell.send(began);
    let none = slopty_client::xfer::Versions::new();
    let kept = bring_down_seen(remote, source, dest, xfer, Some(seen), none);
    let _told = tell.send(Told::Ended { xfer, result: kept.clone() });
    kept
}

/// The promise of one worker file, kept by a download into the drop's directory, told to
/// `tell` as it begins and ends.
fn promise(remote: Arc<dyn Remote>, path: &str, tell: Tell) -> Option<Promise> {
    let name = worker_name(path).to_owned();
    if name.is_empty() {
        return None;
    }
    let source = path.to_owned();
    let keep: Keep =
        Arc::new(move |dest: &std::path::Path| keep_told(remote.as_ref(), &source, dest, &tell));
    Some(Promise { name, keep })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use gpui::{AppContext as _, TestAppContext};
    use slopty_client::xfer::{Download, XferError};
    use slopty_proto::transfer::{Dest, RepRef};
    use slopty_theme::Theme;

    use super::*;
    use crate::workspace::rig::{KEY, hear, link_with, mac, shell};

    /// A worker whose every download is a file named as the path's last part, holding the path.
    #[derive(Debug)]
    struct Echoes;

    impl Remote for Echoes {
        fn upload(&self, _xfer: XferId, _files: Vec<PathBuf>, _dest: Dest, _again: bool) {}

        fn cancel(&self, _xfer: XferId) {}

        fn download(&self, ask: Download) -> Result<Vec<PathBuf>, XferError> {
            let (path, into) = (ask.path, ask.into);
            let name =
                path.rsplit('/').next().ok_or_else(|| XferError::Worker("no name".into()))?;
            let file = into.join(name);
            std::fs::write(&file, &path).map_err(|e| XferError::Worker(e.to_string()))?;
            Ok(vec![file])
        }

        fn clip_fetch(&self, _rep: &RepRef, _max: Option<u64>, _wait: Duration) -> Fetched {
            Fetched::Gone
        }

        fn send_clip(&self, _rep: RepRef, _answer: Fetched, _urgent: bool) {}

        fn forward(&self, _port: u16) -> Option<u16> {
            None
        }
    }

    /// A path a shell printed, ⌘-dragged out, goes on as a promise of the worker's file made
    /// absolute against the shell's directory, brought down when the drop asks; one that cannot
    /// land says why, and a machine that is away is said at once with nothing dragged.
    #[gpui::test]
    fn a_path_dragged_out_of_a_shell_comes_down_when_dropped(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let heard = hear(&workspace, cx);
        let session = SessionId::new();
        let shell = shell(session);
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let echoes: Arc<dyn Remote> = Arc::new(Echoes);
        let _rx = link_with(&workspace, cx, mac(|_| {}), vec![shell], Some(echoes));
        let parked = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&parked);
        workspace.update(cx, |v, _| {
            v.set_drag_sink(Rc::new(move |promises| {
                sink.borrow_mut().extend(promises);
                true
            }));
        });

        workspace.update(cx, |v, cx| v.drag_shell_path(session, "src/main.rs", cx));
        let promise = parked.borrow_mut().pop().expect("the file goes on as a promise");
        assert_eq!(promise.name, "main.rs");
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("main.rs");
        (promise.keep)(&dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "/w/app/src/main.rs", "absolute");
        cx.run_until_parked();
        assert!(heard.borrow().notices.is_empty(), "a drop that lands says nothing");

        let plain = dir.path().join("plain");
        std::fs::write(&plain, b"x").unwrap();
        (promise.keep)(&plain.join("main.rs")).unwrap_err();
        cx.run_until_parked();
        let said = heard.borrow().notices.last().cloned().unwrap_or_default();
        assert!(said.starts_with("main.rs was not dragged out: "), "{said}");

        workspace.update(cx, |v, cx| {
            v.disconnect_worker(
                KEY,
                crate::workspace::WorkerStatus::Reconnecting("lost".into()),
                cx,
            );
        });
        assert!(!workspace.update(cx, |v, cx| v.drag_out(KEY, "/w/app/a.txt", cx)));
        let said = heard.borrow().notices.last().cloned();
        assert_eq!(said.as_deref(), Some("studio is away; a.txt was not dragged out"));
        assert!(parked.borrow().is_empty(), "nothing is dragged");
    }
}
