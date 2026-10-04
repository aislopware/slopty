//! The worker's drags: the one crossing it ([`Drags`]) and the drag helper that carries it as a
//! real drag session (`docs/decisions/audio.md`, "Drag and drop lands at the point, both
//! ways").
//!
//! The helper is this executable run as `slopty-worker dnd` (`slopty_dnd::helper`), an
//! accessory AppKit app, so it needs no grant of its own, and a hang in AppKit's drag code
//! stalls no stream. It starts with the first drag and runs on; one that dies is started again
//! by the next drag, and the drag it carried hears that it went. It speaks
//! [`slopty_proto::dnd`] over its stdin and stdout, framed by [`slopty_proto::codec`].

use std::collections::VecDeque;
use std::sync::Arc;

use bytes::BytesMut;
use parking_lot::Mutex;
use slopty_core::{ClientId, SessionId};
use slopty_input::{DragStep, InputError};
use slopty_proto::codec;
use slopty_proto::dnd::{FromHelper, ToHelper};
use slopty_proto::drag::{DragEvent, DragId, DragInput, DragItem, DragOp};
use slopty_proto::input::MouseButton;
use slopty_proto::screen::ScreenInput;
#[cfg(target_os = "macos")]
use slopty_proto::transfer::{INLINE_CLIP_BYTES, MAX_CLIP_ITEMS, Rep};
use slopty_worker::screen::drag::out::{DragOut, OutAct, OutHeard, Presses, Watching};
use slopty_worker::screen::drag::{Act, BUSY, Claim, Drags, DropIn, Heard};
use slopty_worker::xfer::Transfers;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

/// The argument the daemon runs itself with to be the drag helper.
pub const HELPER_ARG: &str = "dnd";

/// The variable that makes a worker record its drops rather than carry them: `<file>:<op>`.
///
/// `op` (`copy` or `none`) is what every target answers. No helper starts; one scripted
/// here answers as a real one would, and each release writes a line to `<file>` naming the
/// drag and each file in its landing with its BLAKE3 digest, as the release found them. An
/// end-to-end test's seam (`docs/TESTING.md`), with the stream's own input recorded by the
/// synthetic screen.
pub const RECORD: &str = "SLOPTY_DND_RECORD";

/// A worker recording its drops ([`RECORD`]).
#[derive(Debug)]
struct Record {
    file: std::path::PathBuf,
    op: DragOp,
}

impl Record {
    fn from_env() -> Option<Self> {
        let value = std::env::var(RECORD).ok()?;
        let (file, op) = value.rsplit_once(':')?;
        let op = if op == "none" { DragOp::None } else { DragOp::Copy };
        Some(Self { file: file.into(), op })
    }

    /// Answer `msg` as the helper would: a source is up and pressed at once, and over a target
    /// that answers the recorded operation.
    fn answer(&self, drags: &Drags, msg: &ToHelper) {
        if let ToHelper::SourceAt { drag, .. } = *msg {
            for said in [
                FromHelper::Ready { drag },
                FromHelper::Began { drag },
                FromHelper::Operation { drag, op: self.op },
            ] {
                drags.tell(drag, Heard::Helper(said));
            }
        }
    }

    /// The drop of `drag` was let go with its landing at `dir`: what is there goes in the
    /// record, and the target answers as recorded.
    fn released(&self, drags: &Drags, drag: DragId, dir: &std::path::Path) {
        let mut line = format!("release drag={drag}");
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();
        for path in entries {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            let digest = std::fs::read(&path).map(|b| blake3::hash(&b).to_hex().to_string());
            if let (Some(name), Ok(digest)) = (name, digest) {
                use std::fmt::Write as _;
                let _infallible = write!(line, " {name}={digest}");
            }
        }
        line.push('\n');
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(e) = written {
            tracing::warn!(error = %e, file = %self.file.display(), "the drop record");
        }
        drags.tell(drag, Heard::Helper(FromHelper::Ended { drag, op: self.op }));
    }
}

/// The worker's drags, and the helper while it runs.
#[derive(Debug)]
pub struct Dnd {
    drags: Arc<Drags>,
    /// Drops recorded rather than carried ([`RECORD`]).
    record: Option<Record>,
    /// The pasteboard a drag out is watched on: the system's drag pasteboard, or a test's own.
    drag_board: Option<String>,
    /// Where a drag's files land (`Transfers::drag_dir`).
    transfers: Arc<Transfers>,
    helper: Link,
    /// The terminal drags lately entered, newest last, the session each is over and whose it
    /// is: their data goes to that session, never near the drag crossing the worker.
    terms: Mutex<VecDeque<TermDrag>>,
}

/// Most terminal drags [`Dnd`] remembers: one per viewer at a time, and a few just dropped
/// whose data still comes.
const TERM_DRAGS: usize = 16;

/// A client's drag over a terminal.
#[derive(Clone, Copy, Debug)]
struct TermDrag {
    drag: DragId,
    session: SessionId,
    client: ClientId,
}

impl Dnd {
    /// `client`'s drag `drag` is over terminal `session`: its data goes there.
    pub fn term_drag(&self, drag: DragId, session: SessionId, client: ClientId) {
        let mut terms = self.terms.lock();
        terms.retain(|t| t.drag != drag);
        if terms.len() >= TERM_DRAGS {
            terms.pop_front();
        }
        terms.push_back(TermDrag { drag, session, client });
    }

    /// The terminal session `drag` is over or was dropped on, if it is a terminal's.
    #[must_use]
    pub fn term_session(&self, drag: DragId) -> Option<SessionId> {
        self.terms.lock().iter().find(|t| t.drag == drag).map(|t| t.session)
    }

    /// `client`'s drag left terminal `session` without a drop: it is forgotten, and whatever
    /// of its files reached its landing goes, since no program will read them.
    pub fn term_left(&self, session: SessionId, client: ClientId) {
        let mut terms = self.terms.lock();
        let at = terms.iter().rposition(|t| t.session == session && t.client == client);
        let left = at.and_then(|at| terms.remove(at));
        drop(terms);
        if let Some(left) = left {
            self.discard_landing(left.drag);
        }
    }

    /// The drag crossing the worker, and what is heard for it.
    #[must_use]
    pub const fn drags(&self) -> &Arc<Drags> {
        &self.drags
    }

    /// Nothing landed from `drag`: its landing goes, off the stream's task
    /// ([`Transfers::discard_drag`]).
    fn discard_landing(&self, drag: DragId) {
        let transfers = Arc::clone(&self.transfers);
        drop(tokio::task::spawn_blocking(move || transfers.discard_drag(drag)));
    }

    /// No drag yet, its files landing as `transfers` puts them.
    #[must_use]
    pub fn new(transfers: Arc<Transfers>) -> Self {
        let record = Record::from_env();
        if record.is_some() {
            tracing::info!("drops are recorded, not carried");
        }
        Self {
            drags: Arc::default(),
            record,
            drag_board: None,
            transfers,
            helper: Arc::default(),
            terms: Mutex::default(),
        }
    }

    /// No drag yet, and `helper` in the helper's place: a test's.
    #[cfg(test)]
    #[must_use]
    pub fn with_helper(transfers: Arc<Transfers>, helper: mpsc::UnboundedSender<ToHelper>) -> Self {
        let helper = Arc::new(Mutex::new(Some(helper)));
        Self {
            drags: Arc::default(),
            record: None,
            drag_board: None,
            transfers,
            helper,
            terms: Mutex::default(),
        }
    }

    /// Watch drags out on the pasteboard called `name` in place of the system's drag
    /// pasteboard: a test's.
    #[cfg(test)]
    #[must_use]
    pub fn watching(mut self, name: &str) -> Self {
        self.drag_board = Some(name.to_owned());
        self
    }

    /// Tell the helper `msg`, starting it first when it is not running. A helper that cannot
    /// start is as one that went: the drag being carried hears it.
    pub fn tell(&self, msg: ToHelper) {
        if let Some(record) = &self.record {
            record.answer(&self.drags, &msg);
            return;
        }
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
    /// The target took the drop: its landing is what it read, and stays.
    landed: bool,
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
        let Some(claim) = dnd.drags().claim(client, drag) else {
            dnd.discard_landing(drag);
            return Err(refuse(BUSY));
        };
        let dir = dnd.transfers.drag_dir(drag);
        let (drop_in, acts) = DropIn::enter(drag, (x, y), allowed, &items, &dir);
        tracing::info!(%client, %drag, items = items.len(), "a drag enters");
        Ok((Self { drop_in, claim, mapping: None, deadline: None, landed: false }, acts))
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
                Act::Tell(DragEvent::Ended { op, .. }) if op.takes() => self.landed = true,
                _ => {}
            }
            match act {
                Act::Enter { x, y } => {
                    let (answer, mapping) = oneshot::channel();
                    self.mapping = Some(mapping);
                    stream.drag(DragStep::Enter { x, y, answer });
                }
                Act::Press { x, y } => stream.drag(DragStep::Press { x, y }),
                Act::Move { x, y } => stream.drag(DragStep::Move { x, y }),
                Act::Release => {
                    stream.drag(DragStep::Release);
                    if let Some((dnd, record)) = dnd.and_then(|d| Some((d, d.record.as_ref()?))) {
                        let dir = dnd.transfers.drag_dir(self.drag());
                        record.released(dnd.drags(), self.drag(), &dir);
                    }
                }
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
            tracing::info!(drag = %self.drag(), landed = self.landed, "the drag is over");
            if let Some(dnd) = dnd {
                dnd.drags().forget(self.drag());
                if !self.landed {
                    dnd.discard_landing(self.drag());
                }
            }
        }
        over
    }
}

/// How often the drag pasteboard's count is read while the client's left button is held and
/// moved: an app's drag reaches the client within this of beginning.
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "only a Mac has a drag pasteboard to watch")
)]
pub const WATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(8);

/// The drag pasteboard's watch for one stream, on a thread of its own from its first press.
///
/// AppKit's pasteboard is no runtime's to wait on. It marks the change count at each
/// press, reads it every [`WATCH_EVERY`] once the button moves held until the release, and says
/// what a drag that began carries, once ([`Watching`]).
#[derive(Debug, Default)]
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "only a Mac has a drag pasteboard to watch")
)]
pub struct PressWatch {
    /// The pasteboard watched, when not the system's drag pasteboard (a test's, given to
    /// [`Dnd`]'s `watching`).
    board: Option<String>,
    /// What the thread is asked, with the count a press saw.
    to: Option<std::sync::mpsc::Sender<(Watching, isize)>>,
    began: Option<mpsc::UnboundedReceiver<Vec<DragItem>>>,
}

impl PressWatch {
    /// Ask the watch `watching`, starting its thread at the first press.
    #[cfg(target_os = "macos")]
    pub fn ask(&mut self, watching: Watching) {
        let count = if watching == Watching::Mark { mark_count(self.board.as_deref()) } else { 0 };
        if let Some(to) = &self.to
            && to.send((watching, count)).is_ok()
        {
            return;
        }
        if watching == Watching::Mark {
            self.start(count);
        }
    }

    /// Start the watch's thread, marked at `count`.
    #[cfg(target_os = "macos")]
    fn start(&mut self, count: isize) {
        let (to, asked) = std::sync::mpsc::channel();
        let (said, began) = mpsc::unbounded_channel();
        let board = self.board.clone();
        let spawned = std::thread::Builder::new()
            .name("slopty-drag-watch".to_owned())
            .spawn(move || watch_presses(board.as_deref(), &asked, &said));
        match spawned {
            Ok(_detached) => {
                let _sent = to.send((Watching::Mark, count));
                self.to = Some(to);
                self.began = Some(began);
            }
            Err(e) => tracing::warn!(error = %e, "no thread to watch drags out on"),
        }
    }

    /// What the next drag that began under the client's press carries.
    pub async fn began(&mut self) -> Vec<DragItem> {
        match self.began.as_mut() {
            Some(began) => match began.recv().await {
                Some(items) => items,
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }
}

/// The drag pasteboard's count as a press is seen, on the stream's task: the watch's thread
/// may start after an app's drag has.
#[cfg(target_os = "macos")]
fn mark_count(board: Option<&str>) -> isize {
    slopty_dnd::watch::change_count(board)
}

/// The watch's thread: as [`PressWatch`] asks, until it goes.
#[cfg(target_os = "macos")]
fn watch_presses(
    board: Option<&str>,
    asked: &std::sync::mpsc::Receiver<(Watching, isize)>,
    said: &mpsc::UnboundedSender<Vec<DragItem>>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut watch =
        board.map_or_else(slopty_dnd::watch::DragWatch::drag, slopty_dnd::watch::DragWatch::named);
    let mut armed = false;
    loop {
        let next = if armed {
            asked.recv_timeout(WATCH_EVERY)
        } else {
            asked.recv().map_err(|_gone| RecvTimeoutError::Disconnected)
        };
        match next {
            Ok((Watching::Mark, count)) => {
                watch.mark_at(count);
                armed = false;
            }
            Ok((Watching::Arm, _)) => armed = true,
            Ok((Watching::Disarm, _)) => armed = false,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(found) = watch.began() {
                    armed = false;
                    if said.send(found_items(found)).is_err() {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// What a drag's items hold as the client hears it begin: each named file by its path here,
/// each promise by its content type, and each item's data types, its text inline while
/// [`INLINE_CLIP_BYTES`] lasts and the rest only named, for the catch to bring.
#[cfg(target_os = "macos")]
fn found_items(found: Vec<slopty_dnd::watch::Found>) -> Vec<DragItem> {
    use slopty_input::pasteboard::clip_type;
    const TEXT: &str = "public.utf8-plain-text";
    let mut inline_left = INLINE_CLIP_BYTES;
    found
        .into_iter()
        .take(MAX_CLIP_ITEMS)
        .map(|item| {
            let file = item.file.and_then(|f| slopty_worker::screen::drag::out::file_meta(&f.path));
            let text = item.text.map(String::into_bytes).filter(|t| t.len() <= inline_left);
            let mut reps = Vec::new();
            if let Some(text) = text {
                inline_left = inline_left.saturating_sub(text.len());
                let size = Some(u64::try_from(text.len()).unwrap_or(u64::MAX));
                reps.push(Rep { kind: clip_type(TEXT), size, hash: None, inline: Some(text) });
            }
            let lazy = item.carried.iter().filter(|t| reps.is_empty() || *t != TEXT);
            let lazy: Vec<Rep> = lazy
                .map(|t| Rep { kind: clip_type(t), size: None, hash: None, inline: None })
                .collect();
            reps.extend(lazy);
            DragItem { file, promised: item.promised, reps }
        })
        .collect()
}

/// A drag out of an app on the worker under the client's press, beside the stream it began
/// in.
#[derive(Debug)]
pub struct Going {
    out: DragOut,
    claim: Claim,
    /// The input thread's answer to where the drag's point is, while it is awaited.
    locating: Option<oneshot::Receiver<Result<(f64, f64), InputError>>>,
    deadline: Option<tokio::time::Instant>,
}

impl Going {
    /// An app began a drag carrying `items` under `client`'s press, the pointer at `at`: carried
    /// out with its first acts, unless another drag crosses the worker.
    pub fn begin(
        dnd: &Dnd,
        client: ClientId,
        at: (f32, f32),
        items: Vec<DragItem>,
    ) -> Option<(Self, Vec<OutAct>)> {
        let drag = DragId::new();
        let claim = dnd.drags().claim(client, drag)?;
        tracing::info!(%client, %drag, items = items.len(), "a drag out begins");
        let (out, acts) = DragOut::began(drag, at, items, dnd.transfers.drag_dir(drag));
        Some((Self { out, claim, locating: None, deadline: None }, acts))
    }

    /// The drag.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        self.out.drag()
    }

    /// Hear `heard`: what to do comes back.
    pub fn hear(&mut self, heard: OutHeard) -> Vec<OutAct> {
        self.out.hear(heard)
    }

    /// What is heard next: the point, the helper, or the deadline passing.
    pub async fn next(&mut self) -> OutHeard {
        loop {
            let locating = async {
                match self.locating.as_mut() {
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
            return tokio::select! {
                answer = locating => {
                    self.locating = None;
                    OutHeard::Located(answer.unwrap_or(Err(InputError::Unsupported)))
                }
                news = self.claim.news().recv() => match news {
                    Some(Heard::Helper(said)) => OutHeard::Helper(said),
                    Some(Heard::HelperGone) | None => OutHeard::HelperGone,
                    // A transfer's or a representation's news is a drop's, never a drag out's.
                    Some(_) => continue,
                },
                () = deadline => {
                    self.deadline = None;
                    OutHeard::Late
                }
            };
        }
    }

    /// Do `acts` on `stream`, the helper and `tell` (what goes to the client): whether the drag
    /// is over, and the input to post as the client's own, in order.
    pub fn act<P: slopty_worker::platform::Platform>(
        &mut self,
        acts: Vec<OutAct>,
        stream: &mut slopty_worker::screen::Pipeline<P>,
        dnd: Option<&Dnd>,
        mut tell: impl FnMut(DragEvent),
    ) -> (bool, Vec<ScreenInput>) {
        let (mut over, mut inputs) = (false, Vec::new());
        for act in acts {
            match act {
                OutAct::Locate { x, y } => {
                    let (answer, locating) = oneshot::channel();
                    self.locating = Some(locating);
                    stream.drag(DragStep::Locate { x, y, answer });
                }
                OutAct::Input(input) => inputs.push(input),
                OutAct::Cancel => stream.drag(DragStep::Cancel),
                OutAct::Helper(msg) => {
                    if let Some(dnd) = dnd {
                        dnd.tell(msg);
                    }
                }
                OutAct::Tell(event) => tell(event),
                OutAct::Keep(data) => {
                    if let Some(dnd) = dnd {
                        dnd.drags().keep(self.drag(), data);
                    }
                }
                OutAct::Deadline(wait) => {
                    let now = tokio::time::Instant::now();
                    self.deadline = wait.map(|w| now.checked_add(w).unwrap_or(now));
                }
                OutAct::Over => over = true,
            }
        }
        if over {
            tracing::info!(drag = %self.drag(), "the drag out is over");
            if let Some(dnd) = dnd {
                dnd.drags().forget(self.drag());
            }
        }
        (over, inputs)
    }
}

/// What a stream's drag out is waiting on next.
#[derive(Debug)]
pub enum OutNext {
    /// An app began a drag under the client's press, carrying these.
    Began(Vec<DragItem>),
    /// The drag out under way heard this.
    Heard(OutHeard),
}

/// A stream's drags out of the apps it shows: its client's left button as the drag
/// pasteboard's watch follows it, and the drag out under way (`docs/decisions/audio.md`, "Drag
/// out").
#[derive(Debug)]
pub struct Outward {
    presses: Presses,
    watch: PressWatch,
    /// Where the client's held pointer last was, in stream pixels.
    at: (f32, f32),
    going: Option<Going>,
}

impl Outward {
    /// No drag out yet; drags out are watched as `dnd` says.
    #[must_use]
    pub fn new(dnd: Option<&Dnd>) -> Self {
        let board = dnd.and_then(|dnd| dnd.drag_board.clone());
        Self {
            presses: Presses::default(),
            watch: PressWatch { board, ..PressWatch::default() },
            at: (0.0, 0.0),
            going: None,
        }
    }

    /// See `input` before it goes to the app: the watch follows the left button, and a drag out
    /// on the tile follows the pointer and its release.
    pub fn see(&mut self, input: &ScreenInput) -> Vec<OutAct> {
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_variables, reason = "only a Mac has a drag pasteboard to watch")
        )]
        let watching = self.presses.input(input);
        #[cfg(target_os = "macos")]
        if let Some(watching) = watching {
            self.watch.ask(watching);
        }
        let held = self.presses.held();
        let heard = match *input {
            ScreenInput::Move { x, y } if held => {
                self.at = (x, y);
                OutHeard::Moved { x, y }
            }
            ScreenInput::Button { button: MouseButton::Left, down: true, x, y, .. } => {
                self.at = (x, y);
                return Vec::new();
            }
            ScreenInput::Button { button: MouseButton::Left, down: false, .. } => {
                OutHeard::Released
            }
            _ => return Vec::new(),
        };
        self.going.as_mut().map_or_else(Vec::new, |going| going.hear(heard))
    }

    /// The client asks for its drag `drag` to be caught.
    pub fn catch(&mut self, drag: DragId) -> Vec<OutAct> {
        match self.going.as_mut() {
            Some(going) if going.drag() == drag => going.hear(OutHeard::Catch),
            _ => Vec::new(),
        }
    }

    /// What comes next: a drag beginning under the held button, or news for the one under way.
    pub async fn next(&mut self) -> OutNext {
        match self.going.as_mut() {
            Some(going) => OutNext::Heard(going.next().await),
            None if self.presses.held() => OutNext::Began(self.watch.began().await),
            None => std::future::pending().await,
        }
    }

    /// An app began a drag carrying `items` under `client`'s press: carried out unless the
    /// button is up by now or another drag crosses the worker.
    pub fn begin(
        &mut self,
        dnd: Option<&Dnd>,
        client: ClientId,
        items: Vec<DragItem>,
    ) -> Vec<OutAct> {
        let Some(dnd) = dnd.filter(|_| self.presses.held() && self.going.is_none()) else {
            return Vec::new();
        };
        let Some((going, acts)) = Going::begin(dnd, client, self.at, items) else {
            tracing::debug!(%client, "a drag out while another drag crosses the worker");
            return Vec::new();
        };
        self.going = Some(going);
        acts
    }

    /// Hear `heard` for the drag out under way.
    pub fn hear(&mut self, heard: OutHeard) -> Vec<OutAct> {
        self.going.as_mut().map_or_else(Vec::new, |going| going.hear(heard))
    }

    /// Do `acts` for the drag out under way: the input to post as the client's own comes back.
    pub fn act<P: slopty_worker::platform::Platform>(
        &mut self,
        acts: Vec<OutAct>,
        stream: &mut slopty_worker::screen::Pipeline<P>,
        dnd: Option<&Dnd>,
        tell: impl FnMut(DragEvent),
    ) -> Vec<ScreenInput> {
        let Some(going) = self.going.as_mut() else { return Vec::new() };
        let (over, inputs) = going.act(acts, stream, dnd, tell);
        if over {
            self.going = None;
        }
        inputs
    }

    /// The stream ends: a catch under way is stopped on the helper, and the injector lets go of
    /// the button as the stream's input ends.
    pub fn end(self, dnd: Option<&Dnd>) {
        if let (Some(going), Some(dnd)) = (self.going, dnd) {
            dnd.tell(ToHelper::Stop { drag: going.drag() });
            dnd.drags().forget(going.drag());
        }
    }
}
