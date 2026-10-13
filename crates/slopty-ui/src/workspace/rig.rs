//! A headless workspace's one worker on a fake link (tests): what the workspace sends it lands
//! in a channel, and what the workspace says to the app is kept.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Entity, VisualTestContext};
use slopty_client::WorkerKey;
use slopty_core::{ClientId, WorkerId};
use slopty_proto::ClientMsg;
use slopty_proto::handoff::HandoffId;
use slopty_proto::handshake::HelloAck;
use slopty_proto::server::{Os, WorkerCaps};
use slopty_proto::terminal::SessionSummary;
use tokio::sync::mpsc;

use super::{WorkerLink, WorkspaceEvent, WorkspaceView};

/// The one worker's key.
pub const KEY: WorkerKey = WorkerKey::new(1);

/// What the workspace said to the app.
#[derive(Default)]
pub struct Heard {
    /// Its notices' words.
    pub notices: Vec<String>,
    /// The pages it held back: the notice's words and what Open opens.
    pub offers: Vec<(String, String)>,
    /// The handoffs whose notices it took down.
    pub withdrawn: Vec<HandoffId>,
    /// The files it asked the shell to open: the chat and the link.
    pub files: Vec<(String, String)>,
}

/// What the worker was sent, and what the workspace said.
pub struct Fake {
    /// The link's control queue.
    pub rx: mpsc::Receiver<ClientMsg>,
    /// What the workspace said to the app.
    pub heard: Rc<RefCell<Heard>>,
}

impl Fake {
    /// Everything sent so far, paced typing given its frames.
    pub fn sent(&mut self, cx: &VisualTestContext) -> Vec<ClientMsg> {
        let mut all = Vec::new();
        let mut quiet = 0_u8;
        while quiet < 3 {
            cx.run_until_parked();
            let more: Vec<ClientMsg> = std::iter::from_fn(|| self.rx.try_recv().ok()).collect();
            quiet = if more.is_empty() { quiet.saturating_add(1) } else { 0 };
            all.extend(more);
            cx.executor().advance_clock(Duration::from_millis(16));
        }
        all
    }

    /// The last notice said.
    pub fn said(&self) -> Option<String> {
        self.heard.borrow().notices.last().cloned()
    }
}

/// A worker that is a Mac and can do what `caps` adds.
pub fn mac(caps: impl FnOnce(&mut WorkerCaps)) -> WorkerCaps {
    let mut out = WorkerCaps { can_capture: true, can_inject: true, ..WorkerCaps::bare(Os::MacOs) };
    caps(&mut out);
    out
}

/// Keep what `workspace` says to the app.
pub fn hear(workspace: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Rc<RefCell<Heard>> {
    let heard = Rc::new(RefCell::new(Heard::default()));
    let kept = Rc::clone(&heard);
    cx.update(|_, cx| {
        cx.subscribe(workspace, move |_, event, _| {
            let mut kept = kept.borrow_mut();
            match event {
                WorkspaceEvent::Notice { text, .. } => kept.notices.push(text.clone()),
                WorkspaceEvent::Offer { text, url, .. } => {
                    kept.offers.push((text.clone(), url.clone()));
                }
                WorkspaceEvent::Withdrawn { id, .. } => kept.withdrawn.push(*id),
                WorkspaceEvent::OpenFile { chat, target } => {
                    kept.files.push((chat.clone(), target.clone()));
                }
                _ => {}
            }
        })
        .detach();
    });
    heard
}

/// The link to the worker comes up with `sessions` alive: what it is sent goes to the returned
/// channel.
pub fn link(
    workspace: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    caps: WorkerCaps,
    sessions: Vec<SessionSummary>,
) -> mpsc::Receiver<ClientMsg> {
    let (out, rx) = mpsc::channel(256);
    let link = WorkerLink {
        me: ClientId::new(),
        out,
        open_screen: Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream)),
        remote: None,
    };
    let ack = HelloAck {
        worker: WorkerId::new(),
        name: "studio".to_owned(),
        home: String::new(),
        settings: String::new(),
        caps,
        load: 0.0,
        sessions,
    };
    workspace.update(cx, |v, cx| v.connect_worker(KEY, link, ack, cx));
    cx.run_until_parked();
    rx
}
