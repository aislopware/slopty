//! What makes a worker feel local: the clipboard shared with it, files dropped on its tiles,
//! and the ports its shells listen on, served here.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_client::tunnel::Forward;
use slopty_client::xfer::paste_paths;
use slopty_core::{SessionId, XferId};
use slopty_platform::pasteboard::{Pasteboard, Provide};
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::TermRequest;
use slopty_proto::transfer::{ClipMsg, Dest, XferMsg};

use super::WorkspaceView;
use super::actions::ListPorts;
use crate::clipboard::ClipSync;
use crate::palette::{CommandPalette, PaletteItem};

/// How long a paste waits for a worker's clipboard bytes before it gives up. The pasting app's
/// main thread waits with it, so this is as long as a paste may hang.
const CLIP_WAIT: Duration = Duration::from_secs(5);

/// An upload in flight: where it was dropped and how far it got.
#[derive(Clone, Copy, Debug)]
pub struct Upload {
    /// The tile it was dropped on.
    pub tile: TileRef,
    /// The shell its paths are typed into when it is done, for a drop on a terminal.
    pub session: Option<SessionId>,
    /// Bytes in it.
    pub total: u64,
    /// Bytes the worker holds.
    pub done: u64,
}

impl Upload {
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
        if let Some(offer) = self.offer_for(key) {
            self.send(key, ClientMsg::Clip(ClipMsg::Offer(offer)));
        }
    }

    fn offer_for(&self, key: WorkerKey) -> Option<slopty_proto::transfer::Offer> {
        let me = self.me(key)?;
        self.clip.as_ref()?.borrow_mut().offer_for(key, me)
    }

    /// What a remote window of `key` sends ahead of a paste chord.
    pub(super) fn paste_hook(&self, key: WorkerKey) -> Option<crate::screen::PasteHook> {
        let me = self.me(key)?;
        let clip = Rc::clone(self.clip.as_ref()?);
        Some(Rc::new(move || {
            let offer = clip.borrow_mut().offer_for(key, me)?;
            Some(ClientMsg::Clip(ClipMsg::Offer(offer)))
        }))
    }

    /// A clipboard message from `key`.
    pub fn clip_message(&self, key: WorkerKey, msg: ClipMsg) {
        let Some(clip) = self.clip.clone() else { return };
        match msg {
            ClipMsg::Offer(offer) => {
                let Some(remote) =
                    self.workers.get(&key).and_then(|w| w.link.as_ref()?.remote.clone())
                else {
                    return;
                };
                let generation = offer.generation;
                let provide: Provide =
                    Arc::new(move |uti: &str| remote.clip_data(generation, uti, CLIP_WAIT));
                clip.borrow_mut().receive(&offer, provide);
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
    pub fn drop_files(&mut self, tile: TileRef, paths: &[PathBuf], cx: &mut Context<Self>) {
        let Some(item) = self.item(tile) else { return };
        let (dest, session) = match item.kind {
            ItemKind::Terminal { session } => (Dest::SessionCwd(session), Some(session)),
            ItemKind::Window { .. } | ItemKind::Display { .. } => (Dest::Staging, None),
            ItemKind::Note { .. } | ItemKind::File { .. } | ItemKind::Browser { .. } => return,
        };
        let Some(remote) =
            self.workers.get(&tile.worker).and_then(|w| w.link.as_ref()?.remote.clone())
        else {
            self.show_notice("The worker is away; nothing was sent".to_owned(), cx);
            return;
        };
        let total = match slopty_client::xfer::entries(paths) {
            Ok(entries) => entries.iter().fold(0_u64, |sum, e| sum.saturating_add(e.size)),
            Err(e) => {
                self.show_notice(format!("Cannot send that: {e}"), cx);
                return;
            }
        };
        let xfer = XferId::new();
        tracing::info!(%xfer, files = paths.len(), total, "upload");
        self.uploads.insert(xfer, Upload { tile, session, total, done: 0 });
        remote.upload(xfer, paths.to_vec(), dest);
        cx.notify();
    }

    /// The upload in flight on `tile`, if any.
    #[must_use]
    pub fn upload_on(&self, tile: TileRef) -> Option<(XferId, &Upload)> {
        self.uploads.iter().find(|(_, u)| u.tile == tile).map(|(x, u)| (*x, u))
    }

    /// Stop an upload; what the worker holds of it stays there.
    pub fn cancel_upload(&mut self, xfer: XferId, cx: &mut Context<Self>) {
        let Some(upload) = self.uploads.remove(&xfer) else { return };
        if let Some(remote) =
            self.workers.get(&upload.tile.worker).and_then(|w| w.link.as_ref()?.remote.clone())
        {
            remote.cancel(xfer);
        }
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
                    None => {
                        let what = if paths.len() == 1 { "file" } else { "files" };
                        self.show_notice(
                            format!("{} {what} on the worker's clipboard", paths.len()),
                            cx,
                        );
                    }
                }
                cx.notify();
            }
            XferMsg::Failed { xfer, error, .. } => self.xfer_failed(xfer, &error, cx),
            XferMsg::Cancel { xfer } => {
                if self.uploads.remove(&xfer).is_some() {
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
        if self.uploads.remove(&xfer).is_some() {
            self.show_notice(format!("Upload failed: {error}"), cx);
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
        let Some(remote) = self.workers.get(&worker).and_then(|w| w.link.as_ref()?.remote.clone())
        else {
            return false;
        };
        promise_drag(remote, path)
    }

    /// A worker's link came up or went: its watch, its uploads and its ports start over.
    pub(super) fn reset_remote(&mut self, key: WorkerKey, sessions: &[SessionId]) {
        self.watching.remove(&key);
        if let Some(clip) = &self.clip {
            clip.borrow_mut().forget(key);
        }
        self.uploads.retain(|_, u| u.tile.worker != key);
        for session in sessions {
            self.ports.remove(session);
        }
    }
}

/// Start a drag of one worker file, kept by a download into the drop's directory.
#[cfg(target_os = "macos")]
fn promise_drag(remote: Arc<dyn slopty_client::remote::Remote>, path: &str) -> bool {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_owned();
    if name.is_empty() {
        return false;
    }
    let source = path.to_owned();
    let keep: slopty_platform::drag::Keep =
        Arc::new(move |dest: &std::path::Path| keep_promise(remote.as_ref(), &source, dest));
    slopty_platform::drag::drag_out(vec![slopty_platform::drag::Promise { name, keep }])
}

/// No file promises on iOS yet.
#[cfg(not(target_os = "macos"))]
fn promise_drag(_remote: Arc<dyn slopty_client::remote::Remote>, _path: &str) -> bool {
    false
}

/// Bring the worker's `source` down to exactly `dest`: into a hidden directory beside it
/// first, then renamed into place, so a half-arrived file never sits under the promised name.
#[cfg(target_os = "macos")]
fn keep_promise(
    remote: &dyn slopty_client::remote::Remote,
    source: &str,
    dest: &std::path::Path,
) -> Result<(), String> {
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
