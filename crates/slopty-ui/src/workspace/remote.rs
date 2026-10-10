//! What makes a worker feel local: the clipboard shared with it, files dropped on its tiles,
//! and the ports its shells listen on, served here.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, WeakEntity, Window};
use slopty_client::clip::{Answer, Fetched, Place, relay};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_client::remote::Remote;
use slopty_client::tunnel::Forward;
use slopty_client::xfer::ledger::{Kept, Way};
use slopty_client::xfer::{Download, RELINK_WAIT, XferError, paste_paths};
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
pub mod transfers;

#[cfg(target_os = "macos")]
pub use drop_in::Carried;
pub use drop_in::DropIn;

use super::actions::{ListPorts, SaveCopy};
use super::{KeyTarget, WorkspaceView};
use crate::clipboard::{ClipFiles, ClipSync, provider, shell_paste, worker_file_paths};
use crate::conversation::Attach;
use crate::conversation::attach::Target;
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
    /// The worker's remote it was sent through, which reaches it while the worker is away.
    pub via: Option<Arc<dyn Remote>>,
    /// The names of the top-level files and folders sent, as they left here, for what the
    /// person is told of them.
    pub names: Vec<String>,
    /// Since when its worker is away, while it is; the upload waits for the next link
    /// ([`RELINK_WAIT`]).
    pub away_since: Option<std::time::Instant>,
    /// How fast it goes.
    pub pace: transfers::Pace,
    /// An earlier run of the app began it, and this one took it up again: its end is said,
    /// whatever it was dropped on.
    pub taken_up: bool,
    /// A drag's files, dropped: the drop waits for them on the worker, so they are a transfer
    /// the person sees and can stop, no longer a hover's guess.
    pub dropped: bool,
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
            via: None,
            names: Vec::new(),
            away_since: None,
            pace: transfers::Pace::new(),
            taken_up: false,
            dropped: false,
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
            via: None,
            names: Vec::new(),
            away_since: None,
            pace: transfers::Pace::new(),
            taken_up: false,
            dropped: false,
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

    /// An earlier run's upload to `dest`, dropped on `tile`, taken up again: a folder's lists
    /// it again once done; a shell's types nothing, its prompt having moved on since.
    #[must_use]
    pub fn taken_up(tile: TileRef, dest: &Dest) -> Self {
        let dir = match dest {
            Dest::Path(dir) => Some(dir.clone()),
            Dest::SessionCwd(_) | Dest::Staging | Dest::Attachment | Dest::Drag(_) => None,
        };
        Self { dir, taken_up: true, ..Self::to_staging(tile) }
    }

    /// Whether it is listed with the transfers and holds a quit: anything but a drag's files
    /// still hovering, which go when the drag leaves.
    #[must_use]
    pub const fn listed(&self) -> bool {
        self.drag.is_none() || self.dropped
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
            if active {
                self.release_held_toasts(cx);
                // The tile in front is looked at again: its thread's turn is read.
                self.see_focused(cx);
            } else {
                // The app may be ended while it is away (iOS ends a suspended one): what the
                // person wrote is on the disk before it goes.
                self.keep_drafts_now(cx);
            }
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

    /// Where `key`'s home shows in Finder: the root of its File Provider domain, once the
    /// extension has said where the system put it and while it is there (switched on). A
    /// worker's copied files paste into Finder from it. `None` for a worker the server does not
    /// list (no domain), and in a build the team did not sign (no shared container).
    #[cfg(target_os = "macos")]
    fn finder_place(&self, key: WorkerKey) -> Option<Place> {
        let home = self.workers.get(&key)?.home.clone()?;
        let id: slopty_core::WorkerId = format!("{:032x}", key.value()).parse().ok()?;
        let root = slopty_platform::files::root(&slopty_platform::files::container()?, id)?;
        root.is_dir().then_some(Place { home, root })
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
    /// their own here, then send them up as `upload` says. Each comes down listed with the
    /// transfers, with its stop. A failure is a notice, and a paste waiting on them goes on;
    /// one the person stopped, here or on the window's line, says nothing and sends nothing.
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
        #[cfg(not(target_os = "ios"))]
        let tell = Some(Self::tell_downloads(from, Arc::clone(&remote), Bringing::Paste, cx));
        #[cfg(target_os = "ios")]
        let tell: Option<Tell> = None;
        let scratch = std::env::temp_dir().join(format!("slopty-paste-{}", XferId::new()));
        let into = scratch.clone();
        let task = cx.background_executor().spawn(async move {
            let paths = worker_file_paths(&remote, &urls, CLIP_WAIT)
                .ok_or_else(|| Some("the copied files are gone".to_owned()))?;
            let mut landed = Vec::new();
            for (n, path) in paths.into_iter().enumerate() {
                let name =
                    path.file_name().ok_or_else(|| Some("a file with no name".to_owned()))?;
                let dir = into.join(n.to_string());
                std::fs::create_dir_all(&dir).map_err(|e| Some(e.to_string()))?;
                let source = path.to_string_lossy().into_owned();
                let came =
                    fetch_told(remote.as_ref(), source, &dir, &dir.join(name), tell.as_ref());
                came.map_err(|e| (!matches!(e, XferError::Cancelled)).then(|| e.to_string()))?;
                landed.push(dir.join(name));
            }
            Ok::<_, Option<String>>(landed)
        });
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            let _gone = this.update(cx, |this, cx| {
                let tile = upload.tile;
                // Stopped on the window's line while they came over: nothing more to send.
                let stopped = upload
                    .paste
                    .as_ref()
                    .is_some_and(|v| v.upgrade().is_none_or(|v| !v.read(cx).paste_held()));
                match landed {
                    Ok(paths) if !paths.is_empty() && !stopped => {
                        let upload = Upload { scratch: Some(scratch), ..upload };
                        let _started = this.upload(tile, &paths, upload, cx);
                    }
                    Ok(_) => Self::upload_ended(Upload { scratch: Some(scratch), ..upload }, cx),
                    Err(None) => {
                        let mut upload = Upload { scratch: Some(scratch), ..upload };
                        if let Some(view) = upload.paste.take() {
                            let _gone = view.update(cx, ScreenView::cancel_paste);
                        }
                        Self::upload_ended(upload, cx);
                    }
                    Err(Some(e)) => {
                        let said = format!("Cannot bring the files over: {e}");
                        this.show_failure_at(tile, said, cx);
                        Self::upload_ended(Upload { scratch: Some(scratch), ..upload }, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// The person stopped, from `view`'s line, the paste of files its input waits for: what
    /// goes up for it stops. Files still coming over from another machine for it are sent no
    /// further once here ([`Self::bring_over`]).
    pub(super) fn cancel_paste_of(&mut self, view: &Entity<ScreenView>, cx: &mut Context<Self>) {
        let id = view.entity_id();
        let xfers: Vec<XferId> = self
            .uploads
            .iter()
            .filter(|(_, u)| u.paste.as_ref().is_some_and(|p| p.entity_id() == id))
            .map(|(x, _)| *x)
            .collect();
        tracing::info!(uploads = xfers.len(), "a paste of files stopped");
        if xfers.is_empty() {
            view.update(cx, ScreenView::cancel_paste);
        }
        for xfer in xfers {
            self.cancel_upload(xfer, cx);
        }
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
        // The program a drag was dropped on reads what it carries: the person's own drop,
        // whatever the clipboard shares.
        #[cfg(target_os = "macos")]
        if let ClipMsg::Fetch { rep, max, urgent } = &msg
            && let Some(fetched) = self.drag_fetch(rep, *max, cx)
        {
            if let Some(to) = self.remote(key) {
                to.send_clip(rep.clone(), fetched, *urgent);
            }
            return;
        }
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
                #[cfg(target_os = "macos")]
                let place = self.finder_place(key);
                // No other system shows a worker's home as a place of its own.
                #[cfg(not(target_os = "macos"))]
                let place: Option<Place> = None;
                let mut clip = clip.borrow_mut();
                let provide = provider(clip.link(key), &offer, CLIP_WAIT);
                #[cfg_attr(
                    not(target_os = "macos"),
                    expect(unused_variables, reason = "only a Mac has a Finder to miss")
                )]
                let placeless = clip.receive(key, &offer, provide, place.as_ref());
                drop(clip);
                #[cfg(target_os = "macos")]
                if placeless {
                    let text = no_finder_place(&self.worker_name(key));
                    cx.spawn(async move |this, cx| {
                        let _gone = this.update(cx, |this, cx| this.show_notice(text, cx));
                    })
                    .detach();
                }
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

    /// The answer to a fetch of a drag a terminal tile carries; `None` when `rep` is no
    /// terminal's drag.
    #[cfg(target_os = "macos")]
    fn drag_fetch(&self, rep: &RepRef, max: Option<u64>, cx: &Context<Self>) -> Option<Fetched> {
        let slopty_proto::transfer::Source::Drag(drag) = rep.source else { return None };
        self.terminals.values().find_map(|v| v.read(cx).drag_fetch(drag, rep.item, &rep.kind, max))
    }

    /// Attachment `id` of the thread composer on `tile` goes up to a directory of its own on
    /// the tile's worker, its chip showing meanwhile: files as they are, a pasted picture
    /// written to a scratch directory here first, which goes once the upload ends. With no
    /// tile, the chip ends.
    pub(super) fn attach_to_composer(
        &mut self,
        tile: Option<TileRef>,
        composer: Target,
        id: u64,
        what: Attach,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = tile else {
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
                            let said = format!("Cannot attach the picture: {e}");
                            this.show_failure_at(tile, said, cx);
                            Self::upload_ended(upload, cx);
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// The person took attachment `id` off `composer`'s draft: its upload stops. A picture still
    /// being written here has no upload yet; it goes up, and lands on no chip.
    pub(super) fn detach_from_composer(
        &mut self,
        composer: &Target,
        id: u64,
        cx: &mut Context<Self>,
    ) {
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
    /// into it once they are there; to the thread's composer, as attachments, on a thread tile
    /// and on a terminal while its thread face shows; to the worker's staging for a remote
    /// window, where they wait on its clipboard; into the folder a folder tile is at. Nothing
    /// happens on a file, a page or a review.
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
            Some(ItemKind::Thread { .. })
                if let Some(composer) =
                    self.thread_item(tile.item).map(|v| Target(v.downgrade()))
                    && let Some(id) = composer.start(&Attach::Files(paths.to_vec()), cx) =>
            {
                Upload::to_face(tile, composer, id)
            }
            // A thread on its way: to the composer writing its first message.
            None if let Some(composer) = self.starting.composer(tile.item)
                && let Some(id) = composer.start(&Attach::Files(paths.to_vec()), cx) =>
            {
                Upload::to_face(tile, composer, id)
            }
            Some(ItemKind::Window { .. } | ItemKind::Display { .. }) => Upload::to_staging(tile),
            Some(ItemKind::Folder { path }) => Upload::to_folder(tile, path.clone()),
            Some(
                ItemKind::File { .. }
                | ItemKind::Browser { .. }
                | ItemKind::Review { .. }
                | ItemKind::Changes { .. }
                | ItemKind::Thread { .. },
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
            let drag = upload.drag;
            view.update(cx, |view, cx| view.files_failed(drag, cx));
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
            self.show_notice_at(tile, "The machine is away; nothing was sent".to_owned(), cx);
            Self::upload_ended(upload, cx);
            return false;
        };
        upload.total = match slopty_client::xfer::entries(paths) {
            Ok(entries) => entries.iter().fold(0_u64, |sum, e| sum.saturating_add(e.size)),
            Err(e) => {
                self.show_failure_at(tile, format!("Cannot send that: {e}"), cx);
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
        upload.via = Some(Arc::clone(&remote));
        upload.names = paths
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        upload.pace.note(cx.background_executor().now(), 0);
        // A drop on a shell or a folder still means something to the next run of the app; a
        // paste, an attachment or a drag ends with this one.
        let kept = upload.paste.is_none()
            && matches!(dest, Dest::SessionCwd(_) | Dest::Path(_) if upload.attach.is_none());
        if kept {
            let way = Way::Up {
                tile,
                files: paths.to_vec(),
                dest: dest.clone(),
                total: upload.total,
                scratch: upload.scratch.clone(),
            };
            self.keep_transfer(Kept { xfer, worker: tile.worker, way });
        }
        self.uploads.insert(xfer, upload);
        remote.upload(xfer, paths.to_vec(), dest, false);
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

    /// Upload `xfer` ends, however it does: it leaves the list and the ledger.
    fn take_upload(&mut self, xfer: XferId) -> Option<Upload> {
        let upload = self.uploads.remove(&xfer)?;
        self.transfer_over(xfer);
        Some(upload)
    }

    /// Stop an upload, while its worker is away too; what the worker holds of it stays there.
    /// A paste waiting on it lets go of nothing it held but what lets go of a key or a button.
    pub fn cancel_upload(&mut self, xfer: XferId, cx: &mut Context<Self>) {
        let Some(mut upload) = self.take_upload(xfer) else { return };
        if let Some(view) = upload.paste.take() {
            let _gone = view.update(cx, ScreenView::cancel_paste);
        }
        self.terminal_upload_failed(&upload, cx);
        if let Some(remote) = self.remote(upload.tile.worker).or_else(|| upload.via.clone()) {
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
                    upload.pace.note(cx.background_executor().now(), done);
                    if let Some((composer, id)) = upload.attach.clone() {
                        composer.progress(id, upload.fraction(), cx);
                    }
                    cx.notify();
                }
            }
            XferMsg::Finished { xfer, paths } => {
                let Some(upload) = self.take_upload(xfer) else { return };
                if upload.taken_up {
                    let machine = self.worker_name(upload.tile.worker);
                    self.show_notice(format!("{} reached {machine}", sent(&upload.names)), cx);
                    if upload.dir.is_some() {
                        self.refresh_folder(upload.tile.item, cx);
                    }
                    Self::upload_ended(upload, cx);
                    cx.notify();
                    return;
                }
                match upload.session {
                    // The shell types their paths, or a program that took the drop reads them.
                    Some(session) if let Some(view) = self.terminals.get(&session).cloned() => {
                        let drag = upload.drag;
                        view.update(cx, |view, cx| view.files_landed(drag, &paths, cx));
                    }
                    // The shell closed while they went up (across a relink, say): said, so the
                    // files are not lost track of.
                    Some(_) => {
                        let what = if paths.len() == 1 { "file" } else { "files" };
                        let gone = "landed, but the shell they were for has closed";
                        self.show_notice(format!("{} {what} {gone}", paths.len()), cx);
                    }
                    None if let Some((composer, id)) = upload.attach.clone() => {
                        composer.landed(id, &paths, cx);
                    }
                    None if upload.dir.is_some() => {
                        if let Some(text) = kept_both(&upload.names, &paths) {
                            self.show_notice(text, cx);
                        }
                        self.refresh_folder(upload.tile.item, cx);
                    }
                    None if upload.paste.is_some() || upload.drag.is_some() => {}
                    None => {
                        let what = if paths.len() == 1 { "file" } else { "files" };
                        self.show_notice(
                            format!("{} {what} on the machine's clipboard", paths.len()),
                            cx,
                        );
                    }
                }
                Self::upload_ended(upload, cx);
                cx.notify();
            }
            // A file the worker could not write cuts its stream, which the upload sends again
            // from what the worker holds; it says so here itself (`LinkEvent::XferFailed`) once
            // it gives up. Only the whole transfer's failure ends it from the worker's word.
            XferMsg::Failed { xfer, name: None, error } => self.xfer_failed(xfer, &error, cx),
            XferMsg::Cancel { xfer } => {
                if let Some(upload) = self.take_upload(xfer) {
                    self.terminal_upload_failed(&upload, cx);
                    Self::upload_ended(upload, cx);
                    cx.notify();
                }
            }
            XferMsg::Failed { name: Some(_), .. }
            | XferMsg::Begin { .. }
            | XferMsg::Resume { .. }
            | XferMsg::Offset { .. }
            | XferMsg::Done { .. }
            | XferMsg::Fetch { .. } => {}
        }
    }

    /// An upload failed, here or on the worker.
    pub fn xfer_failed(&mut self, xfer: XferId, error: &str, cx: &mut Context<Self>) {
        if let Some(upload) = self.take_upload(xfer) {
            self.terminal_upload_failed(&upload, cx);
            let machine = self
                .workers
                .get(&upload.tile.worker)
                .map_or_else(|| "the machine".to_owned(), |w| w.name.clone());
            let said = format!("{} did not reach {machine}: {error}", sent(&upload.names));
            self.show_failure_at(upload.tile, said, cx);
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
                    self.show_failure(format!("Port {port} could not be forwarded"), cx);
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
    /// whether a drag began. A worker that is away, or a file that could not be brought down
    /// once dropped, is said in a notice: the drop's app shows only a bare error.
    pub fn drag_out(&mut self, worker: WorkerKey, path: &str, cx: &mut Context<Self>) -> bool {
        let name = worker_name(path).to_owned();
        let Some(remote) = self.remote(worker) else {
            let machine = self.workers.get(&worker).map_or("The machine", |w| w.name.as_str());
            let text = format!("{machine} is away; {name} was not dragged out");
            self.show_notice(text, cx);
            return false;
        };
        #[cfg(target_os = "macos")]
        {
            let tell = Self::tell_downloads(worker, Arc::clone(&remote), Bringing::Drag, cx);
            let Some(promise) = promise(remote, path, tell) else { return false };
            match &self.drag_sink {
                Some(sink) => sink(vec![promise]),
                None => slopty_platform::drag::drag_out(vec![promise]),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _unsupported = (remote, cx);
            false
        }
    }

    /// "Save a copy…": the focused file tile's file comes down whole onto this device, to where
    /// the Mac's save panel says, or into a folder chosen in Files on iPhone and iPad. It
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
        self.ask_files(&super::folders::FilesAsk::Export { worker, path: source }, cx);
        #[cfg(not(target_os = "ios"))]
        self.bring_down_as(worker, source, Bringing::Copy, cx);
    }

    /// Where downloads from `worker` through `via` that the view did not start itself tell it
    /// of themselves: each is listed while it goes, with its stop, and the person is told as
    /// `bringing` says when it ends. Listens until every sender is gone.
    #[cfg(not(target_os = "ios"))]
    pub(super) fn tell_downloads(
        worker: WorkerKey,
        via: Arc<dyn Remote>,
        bringing: Bringing,
        cx: &Context<Self>,
    ) -> Tell {
        let (tell, mut heard) = tokio::sync::mpsc::unbounded_channel::<Told>();
        cx.spawn(async move |this, cx| {
            while let Some(told) = heard.recv().await {
                let via = Arc::clone(&via);
                let _gone = this.update(cx, |this, cx| match told {
                    Told::Began { xfer, source, dest, seen } => {
                        let versions = slopty_client::xfer::Versions::new();
                        let down = transfers::Down { worker, xfer, source, dest, versions };
                        this.download_began(&down, bringing, via, seen, cx);
                    }
                    Told::Ended { xfer, result } => this.download_over(xfer, result, cx),
                });
            }
        })
        .detach();
        tell
    }

    /// The save panel, opened in `~/Downloads` on the file's or folder's name; it comes down to
    /// the path it gives, off the main thread, and the person is told as `bringing` says.
    #[cfg(not(target_os = "ios"))]
    pub(super) fn bring_down_as(
        &mut self,
        worker: WorkerKey,
        source: String,
        bringing: Bringing,
        cx: &mut Context<Self>,
    ) {
        if self.remote(worker).is_none() {
            self.show_notice(format!("The machine is away; nothing was {}", bringing.done()), cx);
            return;
        }
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
            let _gone = this.update(cx, |this, cx| {
                let versions = slopty_client::xfer::Versions::new();
                let down = transfers::Down { worker, xfer: XferId::new(), source, dest, versions };
                this.bring_down(down, bringing, cx);
            });
        })
        .detach();
    }

    /// Send the file promises of drags out of the app to `sink` instead of a system drag: the
    /// self-test keeps them itself.
    #[cfg(target_os = "macos")]
    pub fn set_drag_sink(&mut self, sink: DragSink) {
        self.drag_sink = Some(sink);
    }

    /// A worker's link came up or went, as `workers` holds it now: its watch and its ports
    /// start over, and its clipboard promises fetch over the new link.
    ///
    /// Its uploads outlive the link: each goes on over the next one from what the worker holds
    /// (`slopty_client::xfer::upload`), its progress and its cancel kept on its tile meanwhile.
    /// A paste waiting on one lets its keys go now rather than minutes later (the files still
    /// reach the worker's pasteboard), and the drop of a drag ends, as the drag on the worker
    /// did. An upload whose worker stays away past [`RELINK_WAIT`] has ended on its link too.
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
        let linked = self.remote(key).is_some();
        let now = cx.background_executor().now();
        let mut dragged = Vec::new();
        let mut waiting = false;
        for (xfer, upload) in self.uploads.iter_mut().filter(|(_, u)| u.tile.worker == key) {
            if upload.drag.is_some() {
                dragged.push(*xfer);
                continue;
            }
            if let Some(view) = upload.paste.take() {
                let _gone = view.update(cx, |v, _cx| v.release_paste());
            }
            upload.away_since = (!linked).then(|| upload.away_since.unwrap_or(now));
            waiting |= !linked;
        }
        for xfer in dragged {
            if let Some(upload) = self.take_upload(xfer) {
                Self::upload_ended(upload, cx);
            }
        }
        if linked {
            self.take_up_kept(key, cx);
        }
        if waiting {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RELINK_WAIT).await;
                let _gone = this.update(cx, Self::end_outlived_uploads);
            })
            .detach();
        }
        for session in sessions {
            self.ports.remove(session);
        }
    }

    /// The uploads whose worker has been away for [`RELINK_WAIT`] end: no next link came for
    /// them to go on over, and their tasks have given up.
    fn end_outlived_uploads(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let outlived: Vec<XferId> = self
            .uploads
            .iter()
            .filter(|(_, u)| {
                u.away_since
                    .is_some_and(|since| now.saturating_duration_since(since) >= RELINK_WAIT)
            })
            .map(|(xfer, _)| *xfer)
            .collect();
        for xfer in outlived {
            self.xfer_failed(xfer, "the machine went away", cx);
        }
    }
}

/// What a Mac says the first time a worker's copied files cannot paste into Finder: the worker
/// has no File Provider location here (the server does not list it, the location is switched
/// off, or the build is not signed for the shared container). A paste into a shell still
/// brings them.
#[cfg(target_os = "macos")]
fn no_finder_place(machine: &str) -> String {
    format!(
        "Files copied on {machine} paste into shells, not Finder: {machine} has no \
         location in Finder here"
    )
}

/// The last component of a worker path, a trailing `/` aside.
pub(in crate::workspace) fn worker_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// What a download the view did not start itself tells it, from the thread it runs on: a drag
/// out's promise kept for the drop's app, or a file brought over for a paste.
#[derive(Debug)]
#[cfg_attr(
    target_os = "ios",
    expect(dead_code, reason = "iOS lists no downloads here: nothing reads what is told")
)]
pub(in crate::workspace) enum Told {
    /// It began, as `xfer`, the worker's `source` to land at `dest`; `seen` says how far it got.
    Began {
        xfer: XferId,
        source: String,
        dest: PathBuf,
        seen: tokio::sync::watch::Receiver<slopty_client::xfer::Brought>,
    },
    /// It ended.
    Ended { xfer: XferId, result: Result<(), String> },
}

/// Where such a download is told ([`WorkspaceView::tell_downloads`]).
pub(in crate::workspace) type Tell = tokio::sync::mpsc::UnboundedSender<Told>;

/// Bring the worker's `source` into the directory `into` here, where it lands as `dest`, told
/// to `tell` as it begins and ends so it is listed with its stop. Blocks: off the main thread.
fn fetch_told(
    remote: &dyn Remote,
    source: String,
    into: &std::path::Path,
    dest: &std::path::Path,
    tell: Option<&Tell>,
) -> Result<Vec<PathBuf>, XferError> {
    let xfer = XferId::new();
    let Some(tell) = tell else {
        return remote.download(Download::new(xfer, source, into.to_path_buf()));
    };
    let (seen, heard) = tokio::sync::watch::channel(slopty_client::xfer::Brought::default());
    let began = Told::Began { xfer, source: source.clone(), dest: dest.to_path_buf(), seen: heard };
    let _told = tell.send(began);
    let ask = Download { seen: Some(seen), ..Download::new(xfer, source, into.to_path_buf()) };
    let came = remote.download(ask);
    let result = came.as_ref().map(|_| ()).map_err(ToString::to_string);
    let _told = tell.send(Told::Ended { xfer, result });
    came
}

/// Bring the worker's `source` down to exactly `dest` for a drop, told to `tell` as it begins
/// and ends ([`bring_down_seen`]).
#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
fn promise(
    remote: Arc<dyn Remote>,
    path: &str,
    tell: Tell,
) -> Option<slopty_platform::drag::Promise> {
    let name = worker_name(path).to_owned();
    if name.is_empty() {
        return None;
    }
    let source = path.to_owned();
    let keep: slopty_platform::drag::Keep =
        Arc::new(move |dest: &std::path::Path| keep_told(remote.as_ref(), &source, dest, &tell));
    Some(slopty_platform::drag::Promise { name, keep })
}

/// Why a worker's file comes down, for what the person is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum Bringing {
    /// "Save a copy…" of a file tile's file.
    #[cfg_attr(target_os = "ios", expect(dead_code, reason = "iOS saves a copy through Files"))]
    Copy,
    /// "Download…" of a folder tile's selected entry, or a download an earlier run left.
    Download,
    /// Dragged out of a folder tile, a shell or a remote window and dropped here.
    #[cfg_attr(target_os = "ios", expect(dead_code, reason = "a drag out is the Mac's"))]
    Drag,
    /// Copied on one machine and pasted on another: it comes down on its way there.
    #[cfg_attr(
        target_os = "ios",
        expect(dead_code, reason = "iOS brings a paste over without listing it")
    )]
    Paste,
    /// "Save to Files" on iPhone and iPad: into a folder chosen in Files, under the folder's
    /// security scope, which a new run of the app does not hold.
    Files,
}

impl Bringing {
    /// What it was, as in "nothing was saved".
    const fn done(self) -> &'static str {
        match self {
            Self::Copy | Self::Files => "saved",
            Self::Download => "downloaded",
            Self::Drag => "dragged out",
            Self::Paste => "brought over",
        }
    }

    /// Whether it still means something to the next run of the app: a drop's place may be
    /// another app's scratch, and a paste is over with this run.
    const fn kept(self) -> bool {
        matches!(self, Self::Copy | Self::Download)
    }
}

/// `path` with the home directory as `~`, as a person reads it.
fn tildes(path: &std::path::Path) -> String {
    let home = slopty_platform::dirs::home();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// What an upload of the top-level `names` is called: its one name, or how many there were.
fn sent(names: &[String]) -> String {
    match names {
        [one] => one.clone(),
        _ => format!("{} files", names.len()),
    }
}

/// What to say of the top-level entries `sent` that `landed` (the worker's paths) under other
/// names, because those names were taken where they went: `report 2.pdf (report.pdf was there
/// already)`. Each renamed one is paired with the unmatched sent name it shares the longest
/// start with, as the worker's numbering keeps the start (`report` of `report 2.pdf`). None when
/// every entry kept its name.
fn kept_both(sent: &[String], landed: &[String]) -> Option<String> {
    let landed: Vec<&str> =
        landed.iter().map(|p| p.rsplit('/').find(|n| !n.is_empty()).unwrap_or(p)).collect();
    let mut unmatched: Vec<&str> =
        sent.iter().map(String::as_str).filter(|s| !landed.contains(s)).collect();
    let said: Vec<String> = landed
        .iter()
        .filter(|l| !sent.iter().any(|s| s == *l))
        .map(|l| {
            let shared = |s: &&str| s.chars().zip(l.chars()).take_while(|(a, b)| a == b).count();
            let best = unmatched.iter().enumerate().max_by_key(|(_, s)| shared(s)).map(|(i, _)| i);
            match best.map(|i| unmatched.remove(i)) {
                Some(was) => format!("{l} ({was} was there already)"),
                None => (*l).to_owned(),
            }
        })
        .collect();
    (!said.is_empty()).then(|| said.join("; "))
}

/// Bring the worker's `source` down to exactly `dest` as transfer `xfer`, telling `seen` how far
/// it got: into a hidden directory beside it first, named for the transfer, then renamed into
/// place, so a half-arrived file never sits under the chosen name. A transfer an earlier run
/// left takes up its directory again.
fn bring_down_seen(
    remote: &dyn Remote,
    source: &str,
    dest: &std::path::Path,
    xfer: XferId,
    seen: Option<slopty_client::xfer::Seen>,
    resumed: slopty_client::xfer::Versions,
) -> Result<(), String> {
    let parent = dest.parent().ok_or_else(|| "no directory to put it in".to_owned())?;
    let staging = parent.join(format!(".slopty-{xfer}"));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let ask = Download {
        shown_at: Some(dest.to_path_buf()),
        seen,
        resumed,
        ..Download::new(xfer, source.to_owned(), staging.clone())
    };
    let moved = remote
        .download(ask)
        .map_err(|e| e.to_string())
        .and_then(|landed| slopty_platform::file_drop::out::landed_top(&landed, &staging))
        .and_then(|top| std::fs::rename(top, dest).map_err(|e| e.to_string()));
    let _cleaned = std::fs::remove_dir_all(&staging);
    moved
}
