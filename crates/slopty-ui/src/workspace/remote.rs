//! What makes a worker feel local: the clipboard shared with it, files dropped on its tiles,
//! and the ports its shells listen on, served here.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, WeakEntity, Window};
use slopty_client::clip::{Answer, Fetched, relay};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_client::remote::Remote;
use slopty_client::tunnel::Forward;
use slopty_client::xfer::paste_paths;
use slopty_core::{SessionId, XferId};
use slopty_platform::pasteboard::Pasteboard;
use slopty_proto::ClientMsg;
use slopty_proto::drag::DragId;
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::TermRequest;
use slopty_proto::transfer::{ClipMsg, Dest, RepRef, XferMsg};

#[cfg(target_os = "macos")]
mod drag_out;
#[cfg(target_os = "macos")]
pub(super) use drag_out::DragsOut;
mod drop_in;

#[cfg(target_os = "macos")]
pub use drop_in::Carried;
pub use drop_in::DropIn;

use super::actions::{ListPorts, SaveCopy};
use super::{KeyTarget, WorkspaceView};
use crate::clipboard::{ClipFiles, ClipSync, provider, shell_paste, worker_file_paths};
use crate::conversation::Attach;
use crate::conversation::composer::Target;
use crate::palette::{CommandPalette, PaletteItem};
use crate::screen::{PasteAhead, ScreenView};
use crate::terminal::{ClipHook, ClipPaste};

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
    /// The directory it goes into, for a drop on a folder tile, which lists it again when done.
    pub dir: Option<String>,
    /// Bytes in it.
    pub total: u64,
    /// Bytes the worker holds.
    pub done: u64,
    /// The window whose paste waits for these files, for a paste rather than a drop.
    pub paste: Option<WeakEntity<ScreenView>>,
    /// Where files brought from another worker wait to go up, deleted once the upload ends.
    pub scratch: Option<PathBuf>,
    /// The composer the files are attached to, and the attachment's chip there.
    pub attach: Option<(Target, u64)>,
    /// The drag from this device whose drop the files are, into its landing on the worker.
    pub drag: Option<DragId>,
}

impl Upload {
    /// Nothing sent yet, to `session`'s directory, its paths typed there once done.
    #[must_use]
    pub const fn to_shell(tile: TileRef, session: SessionId) -> Self {
        Self {
            tile,
            session: Some(session),
            dir: None,
            total: 0,
            done: 0,
            paste: None,
            scratch: None,
            attach: None,
            drag: None,
        }
    }

    /// Nothing sent yet, to the worker's staging and its pasteboard.
    #[must_use]
    pub const fn to_staging(tile: TileRef) -> Self {
        Self {
            tile,
            session: None,
            dir: None,
            total: 0,
            done: 0,
            paste: None,
            scratch: None,
            attach: None,
            drag: None,
        }
    }

    /// Nothing sent yet, to a directory of its own on the worker, its paths typed into
    /// `composer` once done, where chip `id` shows it meanwhile.
    #[must_use]
    pub fn to_face(tile: TileRef, composer: Target, id: u64) -> Self {
        Self { attach: Some((composer, id)), ..Self::to_staging(tile) }
    }

    /// Nothing sent yet, into the landing on the worker of `drag`, a drag over `tile`, which
    /// drops them at its point there.
    #[must_use]
    pub fn to_drag(tile: TileRef, drag: DragId) -> Self {
        Self { drag: Some(drag), ..Self::to_staging(tile) }
    }

    /// Nothing sent yet, into `dir`, a folder tile's directory.
    #[must_use]
    pub fn to_folder(tile: TileRef, dir: String) -> Self {
        Self { dir: Some(dir), ..Self::to_staging(tile) }
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

    /// The person pasted with the system's paste button (iOS), which handed over `pasted`, the
    /// clipboard as it is, with no prompt. It goes where the key bar's Paste goes, the focused
    /// shell or remote window, and stands for the read of the clipboard that paste would make.
    pub fn paste_made(&self, pasted: &dyn Pasteboard, cx: &mut Context<Self>) {
        let Some(tile) = self.focused() else { return };
        if let (Some(me), Some(clip)) = (self.me(tile.worker), self.clip.as_ref()) {
            clip.borrow_mut().pasted(pasted, me);
        }
        match self.active_key_target() {
            Some(KeyTarget::Terminal(terminal)) => {
                let text = pasted
                    .data(slopty_platform::pasteboard::TEXT_UTI)
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
                terminal.update(cx, |t, cx| t.paste_made(text, cx));
            }
            Some(KeyTarget::Screen(screen)) => screen.update(cx, ScreenView::paste_key),
            None => {}
        }
    }

    /// The app is frontmost or not: a worker's clipboard is watched only while it is.
    pub fn set_app_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.app_active != active {
            self.app_active = active;
            cx.notify();
        }
    }

    /// Share the clipboard with the workers as `sharing` says: a worker it is not shared with
    /// is told its clipboard is no longer watched at once, hears none of this one's, and is
    /// given none of it on a paste.
    pub fn set_clipboard_sharing(
        &mut self,
        sharing: slopty_settings::ClipboardSettings,
        cx: &mut Context<Self>,
    ) {
        if self.clip_sharing != sharing {
            self.clip_sharing = sharing;
            self.clip_sharing_changed();
            cx.notify();
        }
    }

    /// The workers' names or the settings moved: which keys the clipboard is not shared with.
    pub(super) fn clip_sharing_changed(&self) {
        let sharing = &self.clip_sharing;
        let off = self.workers.iter().filter(|(_, w)| !sharing.shared_with(&w.name));
        *self.clip_unshared.borrow_mut() = off.map(|(key, _)| *key).collect();
    }

    /// Whether the clipboard is shared with `key`.
    fn clip_shared(&self, key: WorkerKey) -> bool {
        !self.clip_unshared.borrow().contains(&key)
    }

    /// The workers whose clipboard this client watches now.
    #[must_use]
    pub fn watching(&self) -> Vec<WorkerKey> {
        self.watching.iter().copied().collect()
    }

    /// The worker whose clipboard matters now: the focused tile's, while it is a terminal or a
    /// remote window and the app is frontmost, or the tile whose own window has the keyboard.
    fn clipboard_worker(&self) -> Option<WorkerKey> {
        let tile = match self.popouts.active() {
            Some(item) => self.tile_of(item)?,
            None if self.app_active => self.focused()?,
            None => return None,
        };
        let item = self.item(tile)?;
        let remote = matches!(
            item.kind,
            ItemKind::Terminal { .. } | ItemKind::Window { .. } | ItemKind::Display { .. }
        );
        let linked = self.workers.get(&tile.worker).is_some_and(|w| w.link.is_some());
        (remote && linked && self.clip_shared(tile.worker)).then_some(tile.worker)
    }

    /// Tell each worker whether its clipboard is wanted, and give the one that just became
    /// wanted this client's clipboard, and again whenever it changes while wanted, so the
    /// worker mirrors it. Runs every frame; sends only changes.
    pub(super) fn sync_clipboard_watch(&mut self) {
        let wanted = self.clipboard_worker();
        let stale: Vec<WorkerKey> =
            self.watching.iter().copied().filter(|k| Some(*k) != wanted).collect();
        for key in stale {
            self.watching.remove(&key);
            self.send(key, ClientMsg::Clip(ClipMsg::Watch(false)));
        }
        let moved = self
            .clip
            .as_ref()
            .is_some_and(|clip| clip.borrow_mut().tick(std::time::Instant::now()));
        let Some(key) = wanted else { return };
        let fresh = self.watching.insert(key);
        if fresh {
            self.send(key, ClientMsg::Clip(ClipMsg::Watch(true)));
        }
        if !fresh && !moved {
            return;
        }
        // Where reading asks the person first, their clipboard waits for their paste.
        if let Some(offer) = self.focus_offer(key) {
            self.send(key, ClientMsg::Clip(ClipMsg::Offer(offer)));
        }
    }

    fn focus_offer(&self, key: WorkerKey) -> Option<slopty_proto::transfer::Offer> {
        let me = self.me(key)?;
        self.clip.as_ref()?.borrow_mut().focus_offer(key, me)
    }

    /// What a remote window of `key` needs ahead of a paste chord: this client's offer, and the
    /// files on the clipboard.
    pub(super) fn paste_hook(&self, key: WorkerKey) -> Option<crate::screen::PasteHook> {
        let me = self.me(key)?;
        let clip = Rc::clone(self.clip.as_ref()?);
        let unshared = Rc::clone(&self.clip_unshared);
        Some(Rc::new(move || {
            // Not shared: the chord pastes what the worker's own clipboard holds.
            if unshared.borrow().contains(&key) {
                return PasteAhead { offer: None, files: None };
            }
            let mut clip = clip.borrow_mut();
            let offer = clip.paste_offer(key, me).map(|o| ClientMsg::Clip(ClipMsg::Offer(o)));
            PasteAhead { offer, files: clip.files() }
        }))
    }

    /// What a shell of `key` asks on ⌘V and ⌃V: files on the clipboard, which it pastes as a
    /// drop ([`crate::terminal::TerminalViewEvent::PasteFiles`]); else a picture and no text,
    /// which goes to the worker's pasteboard ahead of the chord, with this client's offer when
    /// the worker has not heard it.
    pub(super) fn clip_hook(&self, key: WorkerKey) -> Option<ClipHook> {
        let me = self.me(key)?;
        let clip = Rc::clone(self.clip.as_ref()?);
        let unshared = Rc::clone(&self.clip_unshared);
        Some(Rc::new(move || {
            // Not shared: a shell pastes this Mac's text and nothing goes to the worker's
            // pasteboard.
            if unshared.borrow().contains(&key) {
                return ClipPaste::Text;
            }
            shell_paste(&mut clip.borrow_mut(), key, me)
        }))
    }

    pub(super) fn remote(&self, key: WorkerKey) -> Option<Arc<dyn Remote>> {
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
            ClipFiles::Worker { worker, urls } if worker == tile.worker => {
                let Some(remote) = self.remote(worker) else { return };
                let task = cx
                    .background_executor()
                    .spawn(async move { worker_file_paths(&remote, &urls, CLIP_WAIT) });
                cx.spawn(async move |this, cx| {
                    let paths = task.await;
                    let _gone = this.update(cx, |this, cx| match paths {
                        Some(paths) if !paths.is_empty() => {
                            let paths: Vec<String> =
                                paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                            let text = paste_paths(&paths);
                            let paste = TermRequest::Paste { text, confirmed: false };
                            this.send_session(session, ClientMsg::Term { session, req: paste });
                        }
                        _ => this.show_notice("The copied files are gone".to_owned(), cx),
                    });
                })
                .detach();
            }
            ClipFiles::Worker { worker, urls } => {
                self.bring_over(worker, urls, Upload::to_shell(tile, session), cx);
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
            ClipFiles::Worker { worker, urls } => {
                self.bring_over(worker, urls, upload, cx);
            }
        }
    }

    /// Bring worker `from`'s copied files (their `urls` in its offer) down into a directory of
    /// their own here, then send them up as `upload` says. A failure is a notice, and a paste
    /// waiting on them goes on.
    fn bring_over(
        &self,
        from: WorkerKey,
        urls: Vec<RepRef>,
        upload: Upload,
        cx: &mut Context<Self>,
    ) {
        let Some(remote) = self.remote(from) else {
            Self::upload_ended(upload, cx);
            return;
        };
        let scratch = std::env::temp_dir().join(format!("slopty-paste-{}", XferId::new()));
        let into = scratch.clone();
        let task = cx.background_executor().spawn(async move {
            let paths = worker_file_paths(&remote, &urls, CLIP_WAIT)
                .ok_or_else(|| "the copied files are gone".to_owned())?;
            let mut landed = Vec::new();
            for (n, path) in paths.into_iter().enumerate() {
                let name = path.file_name().ok_or_else(|| "a file with no name".to_owned())?;
                let dir = into.join(n.to_string());
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                remote
                    .download(path.to_string_lossy().into_owned(), dir.clone(), None)
                    .map_err(|e| e.to_string())?;
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

    /// An upload is over, however it ended: a paste waiting on it goes on, an attachment's chip
    /// goes unless it landed, and files brought over for it go.
    fn upload_ended(upload: Upload, cx: &mut Context<Self>) {
        if let Some(view) = upload.paste {
            let _gone = view.update(cx, |v, _cx| v.release_paste());
        }
        if let Some((composer, id)) = upload.attach {
            composer.ended(id, cx);
        }
        if let Some(scratch) = upload.scratch {
            cx.background_executor()
                .spawn(async move {
                    let _removed = std::fs::remove_dir_all(scratch);
                })
                .detach();
        }
    }

    /// A clipboard message from `key`. Its fetch of another worker's offer, which this client
    /// relayed, is answered by fetching from that worker, off the main thread.
    pub fn clip_message(&self, key: WorkerKey, msg: ClipMsg, cx: &Context<Self>) {
        let Some(clip) = self.clip.clone() else { return };
        // Not shared: the worker's copies stay its own, and none of this Mac's is handed over.
        if !self.clip_shared(key) {
            if let ClipMsg::Fetch { rep, .. } = msg {
                let source = rep.source;
                self.send(key, ClientMsg::Clip(ClipMsg::Unavailable { source }));
            }
            return;
        }
        match msg {
            ClipMsg::Offer(offer) => {
                let mut clip = clip.borrow_mut();
                let provide = provider(clip.link(key), &offer, CLIP_WAIT);
                clip.receive(key, &offer, provide);
            }
            ClipMsg::Fetch { rep, max, urgent } => {
                let answer = clip.borrow().answer(&rep, max);
                let (Some(to), answer) = (self.remote(key), answer) else {
                    let source = rep.source;
                    self.send(key, ClientMsg::Clip(ClipMsg::Unavailable { source }));
                    return;
                };
                match answer {
                    Answer::Here(fetched) => to.send_clip(rep, fetched, urgent),
                    Answer::From(from) => {
                        let Some(from) = self.remote(from) else {
                            to.send_clip(rep, Fetched::Gone, urgent);
                            return;
                        };
                        cx.background_executor()
                            .spawn(async move {
                                relay(&*from, &*to, rep, max, urgent, CLIP_WAIT);
                            })
                            .detach();
                    }
                }
            }
            ClipMsg::Watch(_)
            | ClipMsg::Data { .. }
            | ClipMsg::TooBig { .. }
            | ClipMsg::Unavailable { .. } => {}
        }
    }

    /// Attachment `id` of `session`'s face goes up to a directory of its own on the worker, its
    /// chip showing meanwhile: files as they are, a pasted picture written to a scratch
    /// directory here first, which goes once the upload ends.
    pub(super) fn attach_to_face(
        &mut self,
        session: SessionId,
        composer: Target,
        id: u64,
        what: Attach,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.tile_of_session(session) else {
            composer.ended(id, cx);
            return;
        };
        let upload = Upload::to_face(tile, composer, id);
        match what {
            Attach::Files(paths) => {
                let _started = self.upload(tile, &paths, upload, cx);
            }
            Attach::Picture { name, bytes } => {
                let scratch = std::env::temp_dir().join(format!("slopty-attach-{}", XferId::new()));
                let file = scratch.join(name);
                let (dir, at) = (scratch.clone(), file.clone());
                let written = cx.background_executor().spawn(async move {
                    std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&at, bytes))
                });
                cx.spawn(async move |this, cx| {
                    let written = written.await;
                    let upload = Upload { scratch: Some(scratch), ..upload };
                    let _gone = this.update(cx, |this, cx| match written {
                        Ok(()) => {
                            let _started = this.upload(tile, &[file], upload, cx);
                        }
                        Err(e) => {
                            this.show_notice(format!("Cannot attach the picture: {e}"), cx);
                            Self::upload_ended(upload, cx);
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// The human took attachment `id` off `session`'s draft: its upload stops. A picture still
    /// being written here has no upload yet; it goes up, and lands on no chip.
    pub(super) fn detach_from_face(&mut self, composer: &Target, id: u64, cx: &mut Context<Self>) {
        let xfer = self.uploads.iter().find_map(|(xfer, upload)| {
            upload.attach.as_ref().filter(|(c, at)| *at == id && c == composer).map(|_| *xfer)
        });
        if let Some(xfer) = xfer {
            self.cancel_upload(xfer, cx);
        }
    }

    /// The upload on `tile` that its header shows: not an attachment, whose chip in the face's
    /// composer already says how far it got and stops it.
    #[must_use]
    pub fn header_upload(&self, tile: TileRef) -> Option<(XferId, &Upload)> {
        self.uploads
            .iter()
            .find(|(_, u)| u.tile == tile && u.attach.is_none())
            .map(|(x, u)| (*x, u))
    }

    /// Files dropped on `tile`: to the shell's directory for a terminal, whose paths are typed
    /// into it once they are there; to its face's composer, as attachments, while the face shows;
    /// to the worker's staging for a remote window, where they wait on its clipboard; into the
    /// folder a folder tile is at. Nothing happens on a note, a file or a page.
    ///
    /// Files the platform received for the drop wait in its landing: the upload deletes it
    /// when it ends, and a tile that takes nothing deletes it at once.
    pub fn drop_files(&mut self, tile: TileRef, paths: &[PathBuf], cx: &mut Context<Self>) {
        let landing = self.drop_landing.take();
        let upload = match self.item(tile).map(|i| &i.kind) {
            Some(ItemKind::Terminal { session })
                if let Some(composer) = self.shown_composer(*session)
                    && let Some(id) = composer.start(&Attach::Files(paths.to_vec()), cx) =>
            {
                Upload::to_face(tile, composer, id)
            }
            Some(ItemKind::Terminal { session }) => Upload::to_shell(tile, *session),
            Some(ItemKind::Window { .. } | ItemKind::Display { .. }) => Upload::to_staging(tile),
            Some(ItemKind::Folder { path }) => Upload::to_folder(tile, path.clone()),
            Some(
                ItemKind::Note { .. }
                | ItemKind::File { .. }
                | ItemKind::Browser { .. }
                | ItemKind::Review { .. },
            )
            | None => {
                Self::discard_landing(landing, cx);
                return;
            }
        };
        let _started = self.upload(tile, paths, Upload { scratch: landing, ..upload }, cx);
    }

    /// An upload for a shell will not land: a program waiting for the files of its drop is
    /// told they will not come.
    fn terminal_upload_failed(&self, upload: &Upload, cx: &mut Context<Self>) {
        if let Some(view) = upload.session.and_then(|s| self.terminals.get(&s)).cloned() {
            view.update(cx, |view, cx| view.files_failed(cx));
        }
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
    pub(super) fn upload(
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
        let dest = match (&upload.session, &upload.dir) {
            _ if let Some(drag) = upload.drag => Dest::Drag(drag),
            (Some(session), _) => Dest::SessionCwd(*session),
            (None, Some(dir)) => Dest::Path(dir.clone()),
            (None, None) if upload.attach.is_some() => Dest::Attachment,
            (None, None) => Dest::Staging,
        };
        let xfer = XferId::new();
        tracing::info!(%xfer, files = paths.len(), total = upload.total, "upload");
        self.uploads.insert(xfer, upload);
        remote.upload(xfer, paths.to_vec(), dest);
        cx.notify();
        true
    }

    /// Take every drag over the window (`drop_in`): over a remote window or display, the
    /// worker drops it at the point there; elsewhere GPUI's own handling takes it. Files apps
    /// promise rather than name (Mail, Photos) and, on iPad, every drop land in a temporary
    /// directory first, and go to the tile under the drop as a file drop of their paths, which
    /// uploads them. A file that did not arrive is named in a notice. Once, with the app's
    /// window.
    pub fn accept_dropped_files(window: &Window, cx: &Context<Self>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let host = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
            Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view,
            Ok(RawWindowHandle::UiKit(handle)) => handle.ui_view,
            _ => return,
        };
        let sink =
            drop_in::Sink::new(window.window_handle(), cx.entity().downgrade(), cx.to_async());
        slopty_platform::file_drop::install(host, Rc::new(sink));
        // Drops in and drags out are the same window's two directions; iPad takes both.
        #[cfg(target_os = "ios")]
        Self::offer_drags(window, cx);
    }

    /// The upload in flight on `tile`, if any.
    #[must_use]
    pub fn upload_on(&self, tile: TileRef) -> Option<(XferId, &Upload)> {
        self.uploads.iter().find(|(_, u)| u.tile == tile).map(|(x, u)| (*x, u))
    }

    /// Stop an upload; what the worker holds of it stays there.
    pub fn cancel_upload(&mut self, xfer: XferId, cx: &mut Context<Self>) {
        let Some(upload) = self.uploads.remove(&xfer) else { return };
        self.terminal_upload_failed(&upload, cx);
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
                    if let Some((composer, id)) = upload.attach.clone() {
                        composer.progress(id, upload.fraction(), cx);
                    }
                    cx.notify();
                }
            }
            XferMsg::Finished { xfer, paths } => {
                let Some(upload) = self.uploads.remove(&xfer) else { return };
                match upload.session {
                    // The shell types their paths, or a program that took the drop reads them.
                    Some(session) if let Some(view) = self.terminals.get(&session).cloned() => {
                        view.update(cx, |view, cx| view.files_landed(&paths, cx));
                    }
                    Some(_) => {}
                    None if let Some((composer, id)) = upload.attach.clone() => {
                        composer.landed(id, &paths, cx);
                    }
                    None if upload.dir.is_some() => self.refresh_folder(upload.tile.item, cx),
                    None if upload.paste.is_some() || upload.drag.is_some() => {}
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
                    self.terminal_upload_failed(&upload, cx);
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
            self.terminal_upload_failed(&upload, cx);
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
                let shell = self.terminal_title(*session);
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

    /// "Save a copy…": the focused file tile's file comes down whole onto this device, to where
    /// the Mac's save panel says, or through the Files export sheet on iPhone and iPad. It
    /// comes as a download, not the editor's text, so a file of any size or kind is saved as
    /// its bytes are on the worker.
    pub fn save_copy(&mut self, _: &SaveCopy, _window: &mut Window, cx: &mut Context<Self>) {
        let file = self.focused().and_then(|tile| match &self.item(tile)?.kind {
            ItemKind::File { path } => Some((tile.worker, path.clone())),
            _ => None,
        });
        let Some((worker, source)) = file else {
            self.show_notice("Focus a file to save a copy of".to_owned(), cx);
            return;
        };
        tracing::info!(%source, "save a copy");
        #[cfg(target_os = "ios")]
        self.ask_files(
            &super::folders::FilesAsk::Export { worker, path: source, folder: false },
            cx,
        );
        #[cfg(not(target_os = "ios"))]
        self.save_copy_as(worker, source, cx);
    }

    /// The save panel, opened in `~/Downloads` on the file's name; the file comes down to the
    /// path it gives, off the main thread.
    #[cfg(not(target_os = "ios"))]
    fn save_copy_as(&mut self, worker: WorkerKey, source: String, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(worker) else {
            self.show_notice("The worker is away; nothing was saved".to_owned(), cx);
            return;
        };
        let name = worker_name(&source).to_owned();
        let downloads = slopty_platform::web::downloads_dir(&slopty_platform::dirs::home());
        let chosen = cx.prompt_for_new_path(&downloads, Some(&name));
        cx.spawn(async move |this, cx| {
            let dest = match chosen.await {
                Ok(Ok(Some(dest))) => dest,
                // Cancelled, or the panel went with the window.
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    let text = format!("Cannot show the save panel: {e}");
                    let _gone = this.update(cx, |this, cx| this.show_notice(text, cx));
                    return;
                }
            };
            let saved = cx
                .background_spawn(async move { bring_down_to(remote.as_ref(), &source, &dest) })
                .await;
            let text = match saved {
                Ok(()) => format!("Saved a copy of {name}"),
                Err(e) => format!("{name} was not saved: {e}"),
            };
            let _gone = this.update(cx, |this, cx| this.show_notice(text, cx));
        })
        .detach();
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

/// The last component of a worker path, a trailing `/` aside.
#[cfg(not(target_os = "ios"))]
fn worker_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// The promise of one worker file, kept by a download into the drop's directory.
#[cfg(target_os = "macos")]
fn promise(remote: Arc<dyn Remote>, path: &str) -> Option<slopty_platform::drag::Promise> {
    let name = worker_name(path).to_owned();
    if name.is_empty() {
        return None;
    }
    let source = path.to_owned();
    let keep: slopty_platform::drag::Keep =
        Arc::new(move |dest: &std::path::Path| bring_down_to(remote.as_ref(), &source, dest));
    Some(slopty_platform::drag::Promise { name, keep })
}

/// Bring the worker's `source` down to exactly `dest`: into a hidden directory beside it
/// first, then renamed into place, so a half-arrived file never sits under the chosen name.
#[cfg(not(target_os = "ios"))]
fn bring_down_to(remote: &dyn Remote, source: &str, dest: &std::path::Path) -> Result<(), String> {
    let parent = dest.parent().ok_or_else(|| "no directory to put it in".to_owned())?;
    let staging = parent.join(format!(".slopty-{}", XferId::new()));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let moved = remote
        .download(source.to_owned(), staging.clone(), Some(dest.to_path_buf()))
        .map_err(|e| e.to_string())
        .and_then(|landed| slopty_platform::file_drop::out::landed_top(&landed, &staging))
        .and_then(|top| std::fs::rename(top, dest).map_err(|e| e.to_string()));
    let _cleaned = std::fs::remove_dir_all(&staging);
    moved
}
