//! The worker's drags: the one crossing it ([`Drags`]) and the drag helper that carries it as a
//! real drag session (`docs/decisions/audio.md`, "Drag and drop lands at the point, both
//! ways").
//!
//! The helper is this executable run as `slopty-worker dnd` (`slopty_dnd::helper`), an
//! accessory AppKit app, so it needs no grant of its own, and a hang in AppKit's drag code
//! stalls no stream. It starts with the first drag and runs on; one that dies is started again
//! by the next drag, and the drag it carried hears that it went. It speaks
//! [`slopty_proto::dnd`] over its stdin and stdout, framed by [`slopty_proto::codec`].

use std::sync::Arc;

use bytes::BytesMut;
use parking_lot::Mutex;
use slopty_core::ClientId;
use slopty_input::{DragStep, InputError};
use slopty_proto::codec;
use slopty_proto::dnd::{FromHelper, ToHelper};
use slopty_proto::drag::{DragEvent, DragId, DragInput, DragOp};
use slopty_worker::screen::drag::{Act, BUSY, Claim, Drags, DropIn, Heard};
use slopty_worker::xfer::Transfers;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

/// The argument the daemon runs itself with to be the drag helper.
pub const HELPER_ARG: &str = "dnd";

/// The worker's drags, and the helper while it runs.
#[derive(Debug)]
pub struct Dnd {
    drags: Arc<Drags>,
    /// Where a drag's files land (`Transfers::drag_dir`).
    transfers: Arc<Transfers>,
    helper: Link,
}

impl Dnd {
    /// The drag crossing the worker, and what is heard for it.
    #[must_use]
    pub const fn drags(&self) -> &Arc<Drags> {
        &self.drags
    }

    /// No drag yet, its files landing as `transfers` puts them.
    #[must_use]
    pub fn new(transfers: Arc<Transfers>) -> Self {
        Self { drags: Arc::default(), transfers, helper: Arc::default() }
    }

    /// No drag yet, and `helper` in the helper's place: a test's.
    #[cfg(test)]
    #[must_use]
    pub fn with_helper(transfers: Arc<Transfers>, helper: mpsc::UnboundedSender<ToHelper>) -> Self {
        Self { drags: Arc::default(), transfers, helper: Arc::new(Mutex::new(Some(helper))) }
    }

    /// Tell the helper `msg`, starting it first when it is not running. A helper that cannot
    /// start is as one that went: the drag being carried hears it.
    pub fn tell(&self, msg: ToHelper) {
        let mut helper = self.helper.lock();
        if let Some(to) = helper.as_ref()
            && to.send(msg.clone()).is_ok()
        {
            return;
        }
        if !cfg!(target_os = "macos") {
            drop(helper);
            self.drags.helper_gone();
            return;
        }
        match spawn(Arc::clone(&self.drags), Arc::clone(&self.helper)) {
            Ok(to) => {
                let _sent = to.send(msg);
                *helper = Some(to);
            }
            Err(e) => {
                tracing::warn!(error = %e, "the drag helper did not start");
                *helper = None;
                drop(helper);
                self.drags.helper_gone();
            }
        }
    }
}

/// The way to the helper while it runs.
type Link = Arc<Mutex<Option<mpsc::UnboundedSender<ToHelper>>>>;

/// Start the helper: what goes to it, and the tasks that write to it and read what it says
/// into `drags` until it ends, when `link` lets go of it.
fn spawn(drags: Arc<Drags>, link: Link) -> std::io::Result<mpsc::UnboundedSender<ToHelper>> {
    let exe = std::env::current_exe()?;
    let mut child = tokio::process::Command::new(exe)
        .arg(HELPER_ARG)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(std::io::Error::other("the helper's pipes"));
    };
    tracing::info!(pid = child.id(), "drag helper started");
    let (to, mut queued) = mpsc::unbounded_channel::<ToHelper>();
    let ours = to.clone();
    drop(tokio::spawn(async move {
        while let Some(msg) = queued.recv().await {
            let Ok(frame) = codec::encode(&msg) else { continue };
            if stdin.write_all(&frame).await.is_err() {
                break;
            }
        }
    }));
    drop(tokio::spawn(async move {
        let mut buf = BytesMut::with_capacity(4096);
        loop {
            match codec::try_decode::<FromHelper>(&mut buf) {
                Ok(Some(said)) => {
                    let drag = said.drag();
                    tracing::debug!(%drag, ?said, "the drag helper says");
                    drags.tell(drag, Heard::Helper(said));
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "the drag helper's message");
                    break;
                }
            }
            match stdout.read_buf(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let status = child.wait().await;
        tracing::info!(?status, "drag helper ended");
        let mut link = link.lock();
        if link.as_ref().is_some_and(|to| to.same_channel(&ours)) {
            *link = None;
        }
        drop(link);
        drags.helper_gone();
    }));
    Ok(to)
}

/// Run as the drag helper when the daemon was started as one: the process's whole life.
#[cfg(target_os = "macos")]
pub fn helper_main() -> Option<std::process::ExitCode> {
    (std::env::args().nth(1).as_deref() == Some(HELPER_ARG)).then(slopty_dnd::helper::run)
}

/// Why a drop is refused where no drag can be carried.
const NOT_HERE: &str = "this worker does not take drops";

/// A drag from the client being carried on the worker, beside the stream it entered.
#[derive(Debug)]
pub struct Carrying {
    drop_in: DropIn,
    claim: Claim,
    /// The input thread's answer to where the drag's point is, while it is awaited.
    mapping: Option<oneshot::Receiver<Result<(f64, f64), InputError>>>,
    deadline: Option<tokio::time::Instant>,
}

impl Carrying {
    /// The client's drag entering with `enter`: carried, with what to do first, or refused
    /// with the end to tell the client when another drag crosses the worker or none can be
    /// carried here.
    pub fn enter(
        dnd: Option<&Dnd>,
        client: ClientId,
        enter: DragInput,
    ) -> Result<(Self, Vec<Act>), DragEvent> {
        let DragInput::Enter { drag, x, y, allowed, items } = enter else {
            return Err(DragEvent::Ended { drag: enter.drag(), op: DragOp::None, error: None });
        };
        let refuse =
            |why: &str| DragEvent::Ended { drag, op: DragOp::None, error: Some(why.to_owned()) };
        let Some(dnd) = dnd else { return Err(refuse(NOT_HERE)) };
        let Some(claim) = dnd.drags().claim(client, drag) else { return Err(refuse(BUSY)) };
        let dir = dnd.transfers.drag_dir(drag);
        let (drop_in, acts) = DropIn::enter(drag, (x, y), allowed, &items, &dir);
        tracing::info!(%client, %drag, items = items.len(), "a drag enters");
        Ok((Self { drop_in, claim, mapping: None, deadline: None }, acts))
    }

    /// The drag.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        self.drop_in.drag()
    }

    /// Hear `heard`: what to do comes back.
    pub fn hear(&mut self, heard: Heard) -> Vec<Act> {
        self.drop_in.hear(heard)
    }

    /// What is heard next: the point, news for the drag, or its deadline passing.
    pub async fn next(&mut self) -> Heard {
        let mapping = async {
            match self.mapping.as_mut() {
                Some(answer) => answer.await,
                None => std::future::pending().await,
            }
        };
        let deadline = async {
            match self.deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            answer = mapping => {
                self.mapping = None;
                Heard::Mapped(answer.unwrap_or(Err(InputError::Unsupported)))
            }
            news = self.claim.news().recv() => news.unwrap_or(Heard::HelperGone),
            () = deadline => {
                self.deadline = None;
                Heard::Late
            }
        }
    }

    /// Do `acts` on `stream`, the helper and `tell` (what goes to the client); `true` once the
    /// drag is over.
    pub fn act<P: slopty_worker::platform::Platform>(
        &mut self,
        acts: Vec<Act>,
        stream: &mut slopty_worker::screen::Pipeline<P>,
        dnd: Option<&Dnd>,
        mut tell: impl FnMut(DragEvent),
    ) -> bool {
        let mut over = false;
        for act in acts {
            match act {
                Act::Enter { x, y } => {
                    let (answer, mapping) = oneshot::channel();
                    self.mapping = Some(mapping);
                    stream.drag(DragStep::Enter { x, y, answer });
                }
                Act::Press { x, y } => stream.drag(DragStep::Press { x, y }),
                Act::Move { x, y } => stream.drag(DragStep::Move { x, y }),
                Act::Release => stream.drag(DragStep::Release),
                Act::Cancel => stream.drag(DragStep::Cancel),
                Act::Helper(msg) => {
                    if let Some(dnd) = dnd {
                        dnd.tell(msg);
                    }
                }
                Act::Tell(event) => tell(event),
                Act::Deadline(wait) => {
                    let now = tokio::time::Instant::now();
                    self.deadline = wait.map(|w| now.checked_add(w).unwrap_or(now));
                }
                Act::Over => over = true,
            }
        }
        if over {
            tracing::info!(drag = %self.drag(), "the drag is over");
            if let Some(dnd) = dnd {
                dnd.drags().forget(self.drag());
            }
        }
        over
    }
}
