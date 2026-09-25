//! What makes a worker feel local: the clipboard shared with it, files dropped on its tiles,
//! and the ports its shells listen on, served here.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, WeakEntity, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_client::remote::Remote;
use slopty_client::tunnel::Forward;
use slopty_client::xfer::paste_paths;
use slopty_core::{SessionId, XferId};
use slopty_platform::pasteboard::{FILE_URL_UTI, Pasteboard};
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::TermRequest;
use slopty_proto::transfer::{ClipMsg, Dest, XferMsg};

use super::WorkspaceView;
use super::actions::ListPorts;
use crate::clipboard::{ClipFiles, ClipSync, file_url_paths, provider};
use crate::palette::{CommandPalette, PaletteItem};
use crate::screen::{PasteAhead, ScreenView};

/// How long a paste waits for a worker's clipboard bytes before it gives up. The pasting app's
/// main thread waits with it, so this is as long as a paste may hang.
const CLIP_WAIT: Duration = Duration::from_secs(5);

/// Takes the file promises of a drag out of the app, and says whether a drag began.
#[cfg(target_os = "macos")]
pub type DragSink = Rc<dyn Fn(Vec<slopty_platform::drag::Promise>) -> bool>;

/// An upload in flight: where it was dropped and how far it got.
#[derive(Clone, Debug)]
pub struct Upload {
    /// The tile it was dropped on.
    pub tile: TileRef,
    /// The shell its paths are typed into when it is done, for a drop on a terminal.
    pub session: Option<SessionId>,
    /// Bytes in it.
    pub total: u64,
    /// Bytes the worker holds.
    pub done: u64,
    /// The window whose paste waits for these files, for a paste rather than a drop.
    pub paste: Option<WeakEntity<ScreenView>>,
    /// Where files brought from another worker wait to go up, deleted once the upload ends.
    pub scratch: Option<PathBuf>,
}

impl Upload {
    /// Nothing sent yet, to `session`'s directory, its paths typed there once done.
    #[must_use]
    pub const fn to_shell(tile: TileRef, session: SessionId) -> Self {
        Self { tile, session: Some(session), total: 0, done: 0, paste: None, scratch: None }
    }

    /// Nothing sent yet, to the worker's staging and its pasteboard.
    #[must_use]
    pub const fn to_staging(tile: TileRef) -> Self {
        Self { tile, session: None, total: 0, done: 0, paste: None, scratch: None }
    }

    /// How far along, 0 to 1.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "a fraction for a progress bar")]
        let fraction = self.done as f32 / self.total as f32;
        fraction.clamp(0.0, 1.0)
    }

    /// What the tile says: `↑ 42%`.
    #[must_use]
    pub fn label(&self) -> String {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 100")]
        let percent = (self.fraction() * 100.0).round() as u32;
        format!("\u{2191} {percent}%")
    }
}

impl WorkspaceView {
    /// Keep the clipboard in step with the workers through `board` (the general pasteboard in
    /// the app, a named one under the self-test).
    pub fn set_pasteboard(&mut self, board: Rc<dyn Pasteboard>) {
        self.clip = Some(Rc::new(RefCell::new(ClipSync::new(board))));
    }

    /// The app is frontmost or not: a worker's clipboard is watched only while it is.
    pub fn set_app_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.app_active != active {
            self.app_active = active;
            cx.notify();
        }
    }

    /// The workers whose clipboard this client watches now.
    #[must_use]
    pub fn watching(&self) -> Vec<WorkerKey> {
        self.watching.iter().copied().collect()
    }

    /// The worker whose clipboard matters now: the focused tile's, while it is a terminal or a
    /// remote window and the app is frontmost.
    fn clipboard_worker(&self) -> Option<WorkerKey> {
        if !self.app_active {
            return None;
        }
        let tile = self.focused()?;
        let item = self.item(tile)?;
        let remote = matches!(
            item.kind,
            ItemKind::Terminal { .. } | ItemKind::Window { .. } | ItemKind::Display { .. }
        );
        let linked = self.workers.get(&tile.worker).is_some_and(|w| w.link.is_some());
        (remote && linked).then_some(tile.worker)
    }

    /// Tell each worker whether its clipboard is wanted, and give the one that just became
    /// wanted this client's clipboard. Runs every frame; sends only changes.
    pub(super) fn sync_clipboard_watch(&mut self) {
        let wanted = self.clipboard_worker();
        let stale: Vec<WorkerKey> =
            self.watching.iter().copied().filter(|k| Some(*k) != wanted).collect();
        for key in stale {
            self.watching.remove(&key);
            self.send(key, ClientMsg::Clip(ClipMsg::Watch(false)));
        }
        let Some(key) = wanted else { return };
        if !self.watching.insert(key) {
            return;
        }
        self.send(key, ClientMsg::Clip(ClipMsg::Watch(true)));
        // Where reading asks the person first (iOS), their clipboard waits for their paste.
        if self.clip.as_ref().is_some_and(|clip| clip.borrow().board().reads_ask()) {
            return;
        }
        if let Some(offer) = self.offer_for(key) {
            self.send(key, ClientMsg::Clip(ClipMsg::Offer(offer)));
        }
    }

    fn offer_for(&self, key: WorkerKey) -> Option<slopty_proto::transfer::Offer> {
        let me = self.me(key)?;
        self.clip.as_ref()?.borrow_mut().offer_for(key, me)
    }

    /// What a remote window of `key` needs ahead of a paste chord: this client's offer, and the
    /// files on the clipboard.
    pub(super) fn paste_hook(&self, key: WorkerKey) -> Option<crate::screen::PasteHook> {
        let me = self.me(key)?;
        let clip = Rc::clone(self.clip.as_ref()?);
        Some(Rc::new(move || {
            let mut clip = clip.borrow_mut();
            let offer = clip.offer_for(key, me).map(|o| ClientMsg::Clip(ClipMsg::Offer(o)));
            PasteAhead { offer, files: clip.files() }
        }))
    }

    /// What a shell's ⌘V asks before it pastes text: the files on the clipboard, which it
    /// pastes instead ([`crate::terminal::TerminalViewEvent::PasteFiles`]).
    pub(super) fn files_hook(&self) -> Option<crate::terminal::FilesHook> {
        let clip = Rc::clone(self.clip.as_ref()?);
        Some(Rc::new(move || clip.borrow().files()))
    }

    fn remote(&self, key: WorkerKey) -> Option<Arc<dyn Remote>> {
        self.workers.get(&key).and_then(|w| w.link.as_ref()?.remote.clone())
    }

    /// ⌘V of files into `session`'s shell, as a drop there: files here go up to its directory
    /// and their paths are typed. Files on its own worker are typed where they are; files on
    /// another worker come down here first and then go up.
    pub fn paste_files_in_shell(
        &mut self,
        session: SessionId,
        files: ClipFiles,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.tile_of_session(session) else { return };
        match files {
            ClipFiles::Here(paths) => {
                let _started = self.upload(tile, &paths, Upload::to_shell(tile, session), cx);
            }
            ClipFiles::Worker { worker, generation } if worker == tile.worker => {
                let Some(remote) = self.remote(worker) else { return };
                let task = cx.background_executor().spawn(async move {
                    remote
                        .clip_data(generation, FILE_URL_UTI, CLIP_WAIT)
                        .map(|b| file_url_paths(&b))
                });
                cx.spawn(async move |this, cx| {
                    let paths = task.await;
                    let _gone = this.update(cx, |this, cx| match paths {
                        Some(paths) if !paths.is_empty() => {
                            let paths: Vec<String> =
                                paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                            let text = paste_paths(&paths);
                            let paste = TermRequest::Paste(text);
                            this.send_session(session, ClientMsg::Term { session, req: paste });
                        }
                        _ => this.show_notice("The copied files are gone".to_owned(), cx),
                    });
                })
                .detach();
            }
            ClipFiles::Worker { worker, generation } => {
                self.bring_over(worker, generation, Upload::to_shell(tile, session), cx);
            }
        }
    }

    /// ⌘V of files into a remote window: they go to its worker's staging and onto its
    /// pasteboard, and `view` holds the chord until they are there. Files on the window's own
    /// worker are there already.
    pub(super) fn paste_files_in_window(
        &mut self,
        tile: TileRef,
        view: &WeakEntity<ScreenView>,
        files: ClipFiles,
        cx: &mut Context<Self>,
    ) {
        let upload = Upload { paste: Some(view.clone()), ..Upload::to_staging(tile) };
        match files {
            ClipFiles::Here(paths) => {
                let _started = self.upload(tile, &paths, upload, cx);
            }
            ClipFiles::Worker { worker, .. } if worker == tile.worker => {
                Self::upload_ended(upload, cx);
            }
            ClipFiles::Worker { worker, generation } => {
                self.bring_over(worker, generation, upload, cx);
            }
        }
    }

    /// Bring worker `from`'s copied files (its offer `generation`) down into a directory of
    /// their own here, then send them up as `upload` says. A failure is a notice, and a paste
    /// waiting on them goes on.
    fn bring_over(&self, from: WorkerKey, generation: u64, upload: Upload, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(from) else {
            Self::upload_ended(upload, cx);
            return;
        };
        let scratch = std::env::temp_dir().join(format!("slopty-paste-{}", XferId::new()));
        let into = scratch.clone();
        let task = cx.background_executor().spawn(async move {
            let bytes = remote
                .clip_data(generation, FILE_URL_UTI, CLIP_WAIT)
                .ok_or_else(|| "the copied files are gone".to_owned())?;
            let mut landed = Vec::new();
            for (n, path) in file_url_paths(&bytes).into_iter().enumerate() {
                let name = path.file_name().ok_or_else(|| "a file with no name".to_owned())?;
                let dir = into.join(n.to_string());
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                remote.download(path.to_string_lossy().into_owned(), dir.clone())?;
                landed.push(dir.join(name));
            }
            Ok::<_, String>(landed)
        });
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            let _gone = this.update(cx, |this, cx| {
                let tile = upload.tile;
                match landed {
                    Ok(paths) if !paths.is_empty() => {
                        let upload = Upload { scratch: Some(scratch), ..upload };
                        let _started = this.upload(tile, &paths, upload, cx);
                    }
                    Ok(_) => Self::upload_ended(Upload { scratch: Some(scratch), ..upload }, cx),
                    Err(e) => {
                        this.show_notice(format!("Cannot bring the files over: {e}"), cx);
                        Self::upload_ended(Upload { scratch: Some(scratch), ..upload }, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// An upload is over, however it ended: a paste waiting on it goes on and files brought
    /// over for it go.
    fn upload_ended(upload: Upload, cx: &mut Context<Self>) {
        if let Some(view) = upload.paste {
            let _gone = view.update(cx, |v, _cx| v.release_paste());
        }
        if let Some(scratch) = upload.scratch {
            cx.background_executor()
                .spawn(async move {
                    let _removed = std::fs::remove_dir_all(scratch);
                })
                .detach();
        }
    }

    /// A clipboard message from `key`.
    pub fn clip_message(&self, key: WorkerKey, msg: ClipMsg) {
        let Some(clip) = self.clip.clone() else { return };
        match msg {
            ClipMsg::Offer(offer) => {
                let mut clip = clip.borrow_mut();
                let provide = provider(clip.link(key), &offer, CLIP_WAIT);
                clip.receive(key, &offer, provide);
            }
            ClipMsg::Fetch { generation, uti } => {
                let bytes = clip.borrow().answer(generation, &uti);
                let remote = self.workers.get(&key).and_then(|w| w.link.as_ref()?.remote.clone());
                match (bytes, remote) {
                    (Some(bytes), Some(remote)) => remote.send_clip(generation, uti, bytes),
                    _ => self.send(key, ClientMsg::Clip(ClipMsg::Unavailable { generation })),
                }
            }
            ClipMsg::Watch(_) | ClipMsg::Data { .. } | ClipMsg::Unavailable { .. } => {}
        }
    }

    /// Files dropped on `tile`: to the shell's directory for a terminal, whose paths are typed
    /// into it once they are there; to the worker's staging for a remote window, where they
    /// wait on its clipboard. Nothing happens on a note, a file or a page.
    ///
    /// Files the platform received for the drop wait in its landing: the upload deletes it
    /// when it ends, and a tile that takes nothing deletes it at once.
    pub fn drop_files(&mut self, tile: TileRef, paths: &[PathBuf], cx: &mut Context<Self>) {
        let landing = self.drop_landing.take();
        let upload = match self.item(tile).map(|i| &i.kind) {
            Some(ItemKind::Terminal { session }) => Upload::to_shell(tile, *session),
            Some(ItemKind::Window { .. } | ItemKind::Display { .. }) => Upload::to_staging(tile),
            Some(ItemKind::Note { .. } | ItemKind::File { .. } | ItemKind::Browser { .. })
            | None => {
                Self::discard_landing(landing, cx);
                return;
            }
        };
        let _started = self.upload(tile, paths, Upload { scratch: landing, ..upload }, cx);
    }

    /// Delete a drop's landing that nothing will upload from, off the main thread.
    fn discard_landing(landing: Option<PathBuf>, cx: &Context<Self>) {
        if let Some(landing) = landing {
            cx.background_executor()
                .spawn(async move { slopty_platform::file_drop::discard(&landing) })
                .detach();
        }
    }

    /// Send `paths` to `tile`'s worker as `upload` says: to its shell's directory when it names
    /// a shell, else to staging. Whether it started; when it did not, it has ended.
    fn upload(
        &mut self,
        tile: TileRef,
        paths: &[PathBuf],
        mut upload: Upload,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(remote) = self.remote(tile.worker) else {
            self.show_notice("The worker is away; nothing was sent".to_owned(), cx);
            Self::upload_ended(upload, cx);
            return false;
        };
        upload.total = match slopty_client::xfer::entries(paths) {
            Ok(entries) => entries.iter().fold(0_u64, |sum, e| sum.saturating_add(e.size)),
            Err(e) => {
                self.show_notice(format!("Cannot send that: {e}"), cx);
                Self::upload_ended(upload, cx);
                return false;
            }
        };
        let dest = upload.session.map_or(Dest::Staging, Dest::SessionCwd);
        let xfer = XferId::new();
        tracing::info!(%xfer, files = paths.len(), total = upload.total, "upload");
        self.uploads.insert(xfer, upload);
        remote.upload(xfer, paths.to_vec(), dest);
        cx.notify();
        true
    }

    /// Take drops of files that apps promise rather than name (Mail, Photos) and, on iPad,
    /// every drop: the platform lands them in a temporary directory, and they go to the tile
    /// under the drop as a file drop of their paths, which uploads them. A file that did not
    /// arrive is named in a notice. Once, with the app's window.
    pub fn accept_dropped_files(window: &Window, cx: &Context<Self>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let host = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
            Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view,
            Ok(RawWindowHandle::UiKit(handle)) => handle.ui_view,
            _ => return,
        };
        let (handle, view, app) = (window.window_handle(), cx.entity().downgrade(), cx.to_async());
        let sink = Rc::new(move |dropped: slopty_platform::file_drop::Dropped| {
            let mut app = app.clone();
            let landing = dropped.landing.clone();
            let delivered = handle.update(&mut app, |_root, window, cx| {
                #[expect(clippy::cast_possible_truncation, reason = "points in a window")]
                let position = gpui::point(gpui::px(dropped.x as f32), gpui::px(dropped.y as f32));
                // The tile the drop lands on takes the landing with the paths.
                let _gone = view.update(cx, |v, _cx| v.drop_landing.clone_from(&dropped.landing));
                if !dropped.paths.is_empty() {
                    let paths = gpui::ExternalPaths(dropped.paths.iter().cloned().collect());
                    for event in [
                        gpui::FileDropEvent::Entered { position, paths },
                        gpui::FileDropEvent::Pending { position },
                        gpui::FileDropEvent::Submit { position },
                    ] {
                        let _handled =
                            window.dispatch_event(gpui::PlatformInput::FileDrop(event), cx);
                    }
                }
                // No tile took it (a drop on the bars, or between tiles): nothing uploads it.
                let _gone =
                    view.update(cx, |v, cx| Self::discard_landing(v.drop_landing.take(), cx));
                if !dropped.failed.is_empty() {
                    let text = format!("Not sent: {}", dropped.failed.join("; "));
                    let _gone = view.update(cx, |v, cx| v.show_notice(text, cx));
                }
            });
            if let Err(e) = delivered {
                tracing::warn!(error = %e, "a drop for a window that is gone");
                if let Some(landing) = &landing {
                    slopty_platform::file_drop::discard(landing);
                }
            }
        });
        slopty_platform::file_drop::install(host, sink);
    }

    /// The upload in flight on `tile`, if any.
    #[must_use]
    pub fn upload_on(&self, tile: TileRef) -> Option<(XferId, &Upload)> {
        self.uploads.iter().find(|(_, u)| u.tile == tile).map(|(x, u)| (*x, u))
    }

    /// Stop an upload; what the worker holds of it stays there.
    pub fn cancel_upload(&mut self, xfer: XferId, cx: &mut Context<Self>) {
        let Some(upload) = self.uploads.remove(&xfer) else { return };
        if let Some(remote) = self.remote(upload.tile.worker) {
            remote.cancel(xfer);
        }
        Self::upload_ended(upload, cx);
        cx.notify();
    }

    /// A transfer message from a worker.
    pub fn xfer_message(&mut self, msg: XferMsg, cx: &mut Context<Self>) {
        match msg {
            XferMsg::Progress { xfer, done } => {
                if let Some(upload) = self.uploads.get_mut(&xfer)
                    && upload.done != done
                {
                    upload.done = done;
                    cx.notify();
                }
            }
            XferMsg::Finished { xfer, paths } => {
                let Some(upload) = self.uploads.remove(&xfer) else { return };
                match upload.session {
                    Some(session) if self.terminals.contains_key(&session) => {
                        let text = paste_paths(&paths);
                        self.send_session(
                            session,
                            ClientMsg::Term { session, req: TermRequest::Paste(text) },
                        );
                    }
                    Some(_) => {}
                    None if upload.paste.is_some() => {}
                    None => {
                        let what = if paths.len() == 1 { "file" } else { "files" };
                        self.show_notice(
                            format!("{} {what} on the worker's clipboard", paths.len()),
                            cx,
                        );
                    }
                }
                Self::upload_ended(upload, cx);
                cx.notify();
            }
            XferMsg::Failed { xfer, error, .. } => self.xfer_failed(xfer, &error, cx),
            XferMsg::Cancel { xfer } => {
                if let Some(upload) = self.uploads.remove(&xfer) {
                    Self::upload_ended(upload, cx);
                    cx.notify();
                }
            }
            XferMsg::Begin { .. }
            | XferMsg::Resume { .. }
            | XferMsg::Offset { .. }
            | XferMsg::Done { .. }
            | XferMsg::Fetch { .. } => {}
        }
    }

    /// An upload failed, here or on the worker.
    pub fn xfer_failed(&mut self, xfer: XferId, error: &str, cx: &mut Context<Self>) {
        if let Some(upload) = self.uploads.remove(&xfer) {
            self.show_notice(format!("Upload failed: {error}"), cx);
            Self::upload_ended(upload, cx);
            cx.notify();
        }
    }

    /// The listening ports of `session` changed; each is served here.
    pub fn ports_changed(
        &mut self,
        session: SessionId,
        forwards: Vec<Forward>,
        cx: &mut Context<Self>,
    ) {
        let before: Vec<u16> = self
            .ports
            .get(&session)
            .map(|f| f.iter().map(|f| f.port.number).collect())
            .unwrap_or_default();
        for moved in forwards.iter().filter(|f| !before.contains(&f.port.number)) {
            match moved.local {
                Some(local) if local != moved.port.number => {
                    let port = moved.port.number;
                    self.show_notice(
                        format!("Port {port} is taken here; forwarded on {local}"),
                        cx,
                    );
                }
                None => {
                    let port = moved.port.number;
                    self.show_notice(format!("Port {port} could not be forwarded"), cx);
                }
                Some(_) => {}
            }
        }
        if forwards.is_empty() {
            self.ports.remove(&session);
        } else {
            self.ports.insert(session, forwards);
        }
        cx.notify();
    }

    /// The forwarded ports of a session, as the worker listed them.
    #[must_use]
    pub fn forwards(&self, session: SessionId) -> &[Forward] {
        self.ports.get(&session).map_or(&[], Vec::as_slice)
    }

    /// Open a forwarded port in the default browser.
    pub fn open_forward(forward: &Forward) {
        if let Some(url) = forward.url() {
            tracing::info!(%url, "opening a forwarded port");
            slopty_platform::open_url(&url);
        }
    }

    /// The palette as the list of forwarded ports; ↩ opens one in a tile or in the browser.
    pub fn list_ports(&mut self, _: &ListPorts, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let mut lines: Vec<PaletteItem> = self
            .ports
            .iter()
            .flat_map(|(session, forwards)| {
                let shell = self.terminal_title(*session, cx);
                forwards
                    .iter()
                    .filter_map(move |f| {
                        let url = f.url()?;
                        let local = f.local.unwrap_or(f.port.number);
                        let detail = format!("{} · {shell}", f.port.process);
                        let tile = format!("Open localhost:{} in a tile", f.port.number);
                        let browser = format!("Open localhost:{local} in the browser");
                        Some([
                            PaletteItem::in_tile(&tile, &detail, &f.worker_url()),
                            PaletteItem::url(&browser, &detail, &url),
                        ])
                    })
                    .flatten()
            })
            .collect();
        lines.sort_by(|a, b| a.label.cmp(&b.label));
        if lines.is_empty() {
            self.show_notice("No ports are forwarded".to_owned(), cx);
            return;
        }
        let theme = self.theme.clone();
        let palette = cx.new(|cx| CommandPalette::new(lines, theme, window, cx));
        self.show_palette(palette, window, cx);
    }

    /// Drag the worker's file at `path` out of the app, from the mouse event being handled: a
    /// file promise, kept by bringing the file down when something takes the drop. Returns
    /// whether a drag began.
    pub fn drag_out(&self, worker: WorkerKey, path: &str) -> bool {
        let Some(remote) = self.remote(worker) else { return false };
        #[cfg(target_os = "macos")]
        {
            let Some(promise) = promise(remote, path) else { return false };
            match &self.drag_sink {
                Some(sink) => sink(vec![promise]),
                None => slopty_platform::drag::drag_out(vec![promise]),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _unsupported = (remote, path);
            false
        }
    }

    /// Send the file promises of drags out of the app to `sink` instead of a system drag: the
    /// self-test keeps them itself.
    #[cfg(target_os = "macos")]
    pub fn set_drag_sink(&mut self, sink: DragSink) {
        self.drag_sink = Some(sink);
    }

    /// A worker's link came up or went, as `workers` holds it now: its watch, its uploads and
    /// its ports start over, and its clipboard promises fetch over the new link.
    pub(super) fn reset_remote(
        &mut self,
        key: WorkerKey,
        sessions: &[SessionId],
        cx: &mut Context<Self>,
    ) {
        self.watching.remove(&key);
        if let Some(clip) = &self.clip {
            clip.borrow_mut().relink(key, self.remote(key));
        }
        let gone: Vec<XferId> =
            self.uploads.iter().filter(|(_, u)| u.tile.worker == key).map(|(x, _)| *x).collect();
        for xfer in gone {
            if let Some(upload) = self.uploads.remove(&xfer) {
                Self::upload_ended(upload, cx);
            }
        }
        for session in sessions {
            self.ports.remove(session);
        }
    }
}

/// The promise of one worker file, kept by a download into the drop's directory.
#[cfg(target_os = "macos")]
fn promise(remote: Arc<dyn Remote>, path: &str) -> Option<slopty_platform::drag::Promise> {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_owned();
    if name.is_empty() {
        return None;
    }
    let source = path.to_owned();
    let keep: slopty_platform::drag::Keep =
        Arc::new(move |dest: &std::path::Path| keep_promise(remote.as_ref(), &source, dest));
    Some(slopty_platform::drag::Promise { name, keep })
}

/// Bring the worker's `source` down to exactly `dest`: into a hidden directory beside it
/// first, then renamed into place, so a half-arrived file never sits under the promised name.
#[cfg(target_os = "macos")]
fn keep_promise(remote: &dyn Remote, source: &str, dest: &std::path::Path) -> Result<(), String> {
    let parent = dest.parent().ok_or_else(|| "no directory to drop in".to_owned())?;
    let staging = parent.join(format!(".slopty-{}", XferId::new()));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let landed = remote.download(source.to_owned(), staging.clone());
    let moved = landed.and_then(|landed| {
        let first = landed.first().ok_or_else(|| "nothing arrived".to_owned())?;
        let top = first
            .strip_prefix(&staging)
            .ok()
            .and_then(|rel| rel.components().next())
            .ok_or_else(|| "arrived outside the drop".to_owned())?;
        std::fs::rename(staging.join(top), dest).map_err(|e| e.to_string())
    });
    let _cleaned = std::fs::remove_dir_all(&staging);
    moved
}
