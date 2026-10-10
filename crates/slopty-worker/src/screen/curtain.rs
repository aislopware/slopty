//! The curtain over the worker's Mac while a client holds it.
//!
//! Its own screens show a shield, and its own keyboard and pointer are held off, while the
//! clients still see and drive the session (`docs/decisions/video.md`, "The curtain").
//!
//! The holders are counted here, off the main thread, and the curtain itself lives on the main
//! thread (AppKit's windows), where each change hands a job ([`Main`]) that brings it in line
//! with the holders: raised for the first, lowered with the last. A client holds it on each of
//! its links ([`Link`]) and lets go when its last link goes; when the last holder goes that way
//! the Mac is locked first and the curtain falls once it reads as locked, so the session is
//! never left open at the desk, while letting go on the person's word leaves it unlocked. The
//! Mac's own input is held on a lease the runtime renews ([`HOLD_LEASE`]), so a worker alive
//! but stuck never shuts the desk out.
//!
//! Each display capture leaves the shield's windows out of its picture
//! (`slopty_capture::leave_out`), but a capture already running keeps the filter it was made
//! with: [`Curtain::shield_moved`] says when the shield's windows changed, and each display
//! stream takes its filter again then.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::ClientId;
use slopty_proto::screen::CurtainState;
use tokio::sync::{oneshot, watch};

use super::sized::Main;

/// How long the Mac's own input stays held without a renewal: a worker alive but stuck renews
/// nothing, and the desk has its input back this long after.
pub const HOLD_LEASE: Duration = Duration::from_secs(5);
/// How often the runtime renews the hold's lease while the curtain is up.
const RENEW_EVERY: Duration = Duration::from_secs(1);
/// How long the curtain waits, once it asked for the lock, for the Mac to read as locked before
/// it falls; past it the shield stays.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// How often it looks meanwhile.
const LOCK_POLL: Duration = Duration::from_millis(100);
/// The file beside the worker's data that says the curtain is up ([`Curtain::keep_marker_at`]).
pub const MARKER: &str = "curtain-up";

/// What renews the input hold's lease, from the runtime.
pub type Renew = Arc<dyn Fn() + Send + Sync>;

/// What draws the curtain: the shield, the input hold and the lock (`slopty_platform::curtain`)
/// on a Mac ([`Mac`]), a fake in tests. Called on the main thread only.
pub trait Drapes: 'static {
    /// The curtain while it is up.
    type Raised;
    /// Raise it: the shield over every screen of the Mac's own, out of every capture, and the
    /// Mac's own input held off.
    ///
    /// # Errors
    ///
    /// Why it could not be raised at all.
    fn raise(&mut self) -> Result<Self::Raised, String>;
    /// Whether the Mac's own input is held off: `false` when the hold could not start, while
    /// the screens are still covered.
    fn input_held(&self, raised: &Self::Raised) -> bool;
    /// What renews the input hold's lease ([`HOLD_LEASE`]); `None` when no input is held.
    fn lease(&self, raised: &Self::Raised) -> Option<Renew>;
    /// The displays changed: cover each screen there is now. Whether the shield's windows
    /// changed.
    fn follow(&mut self, raised: &mut Self::Raised) -> bool;
    /// Ask for the Mac to be locked. The lock lands a moment later ([`Self::locked`]).
    ///
    /// # Errors
    ///
    /// Why it could not be asked.
    fn lock(&mut self) -> Result<(), String>;
    /// Whether the Mac's session reads as locked, or is off its screens.
    fn locked(&self) -> bool;
    /// Lower it.
    fn lower(&mut self, raised: Self::Raised);
}

/// Where a curtain whose last holder went stands with the lock it falls behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Locking {
    /// No lock is due: it falls when the last holder lets go.
    Not,
    /// The lock is asked for: it falls once the Mac reads as locked.
    Asked,
    /// The lock could not be asked for: the shield stays until a holder lifts it.
    Failed,
}

/// The curtain on the main thread: what draws it, and it while it is up.
pub struct Stage<D: Drapes> {
    drapes: D,
    raised: Option<D::Raised>,
    locking: Locking,
}

impl<D: Drapes> std::fmt::Debug for Stage<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stage")
            .field("up", &self.raised.is_some())
            .field("locking", &self.locking)
            .finish_non_exhaustive()
    }
}

impl<D: Drapes> Stage<D> {
    /// Down, drawn by `drapes` when it is raised.
    pub const fn new(drapes: D) -> Self {
        Self { drapes, raised: None, locking: Locking::Not }
    }

    /// Bring the curtain in line with `holders`, saying on `moved` when its windows came or
    /// went, and on `state` where it stands.
    fn settle(
        &mut self,
        holders: &Mutex<Holders>,
        moved: &watch::Sender<u64>,
        state: &watch::Sender<CurtainState>,
    ) -> Settled {
        let (count, lock) = {
            let mut holders = holders.lock();
            (holders.clients.len(), std::mem::take(&mut holders.lock))
        };
        let told = u32::try_from(count).unwrap_or(u32::MAX);
        let settled = match (count, self.raised.take()) {
            (0, Some(raised)) => self.emptied(raised, lock, holders, moved),
            (0, None) => Settled::new(CurtainState::Down, false),
            (_, Some(raised)) => {
                self.locking = Locking::Not;
                let input_held = self.drapes.input_held(&raised);
                self.raised = Some(raised);
                Settled::new(CurtainState::Up { holders: told, input_held }, false)
            }
            (_, None) => match self.drapes.raise() {
                Ok(raised) => {
                    let input_held = self.drapes.input_held(&raised);
                    holders.lock().lease = self.drapes.lease(&raised);
                    self.raised = Some(raised);
                    self.locking = Locking::Not;
                    bump(moved);
                    Settled::new(CurtainState::Up { holders: told, input_held }, true)
                }
                Err(why) => {
                    tracing::warn!(%why, "the curtain not raised");
                    holders.lock().clients.clear();
                    Settled::new(CurtainState::Refused { why }, true)
                }
            },
        };
        state.send_replace(settled.state.clone());
        settled
    }

    /// Nobody holds the raised curtain: it falls, once the Mac reads as locked when its last
    /// holder went (`lock`), and it stays up when the lock could not be asked for, so the
    /// session is never left open at the desk.
    fn emptied(
        &mut self,
        raised: D::Raised,
        lock: bool,
        holders: &Mutex<Holders>,
        moved: &watch::Sender<u64>,
    ) -> Settled {
        if lock {
            self.locking = match self.drapes.lock() {
                Ok(()) => Locking::Asked,
                Err(why) => {
                    tracing::warn!(%why, "the Mac not locked; the shield stays");
                    Locking::Failed
                }
            };
        }
        let stays = match self.locking {
            Locking::Not => false,
            Locking::Asked => !self.drapes.locked(),
            Locking::Failed => true,
        };
        if stays {
            let input_held = self.drapes.input_held(&raised);
            self.raised = Some(raised);
            let mut settled = Settled::new(CurtainState::Up { holders: 0, input_held }, lock);
            settled.locking = self.locking == Locking::Asked;
            return settled;
        }
        self.locking = Locking::Not;
        self.drapes.lower(raised);
        holders.lock().lease = None;
        bump(moved);
        Settled::new(CurtainState::Down, true)
    }
}

/// How long a worker about to end waits for the Mac to read as locked: the lock's own wait, and
/// a moment for the main thread to take the job.
pub const EXIT_WAIT: Duration = LOCK_WAIT.saturating_add(Duration::from_secs(1));

/// Answer on `tx` once `stage`'s Mac reads as locked, taking the marker away then, or `false`
/// once `polls` more looks, each [`LOCK_POLL`] apart on the main thread, found it not.
fn confirm_locked<D: Drapes>(
    stage: &Stage<D>,
    main: &Arc<dyn Main<Stage<D>>>,
    holders: Arc<Mutex<Holders>>,
    tx: SyncSender<bool>,
    polls: u32,
) {
    if stage.drapes.locked() {
        let marker = holders.lock().marker.clone();
        if let Some(path) = marker
            && let Err(e) = unmark(&path)
        {
            tracing::warn!(path = %path.display(), error = %e, "the curtain's marker stays");
        }
        let _asker_gone = tx.send(true);
        return;
    }
    let Some(left) = polls.checked_sub(1) else {
        tracing::warn!("the Mac did not read as locked before the worker ends");
        let _asker_gone = tx.send(false);
        return;
    };
    let again = Arc::clone(main);
    main.run_after(
        LOCK_POLL,
        Box::new(move |stage: &mut Stage<D>| confirm_locked(stage, &again, holders, tx, left)),
    );
}

/// Take the marker at `path` away; one already gone is no error.
fn unmark(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Say on `moved` that the shield's windows changed.
fn bump(moved: &watch::Sender<u64>) {
    moved.send_modify(|n| *n = n.wrapping_add(1));
}

/// Who holds the curtain, and whether it locks the Mac as it falls.
#[derive(Default)]
struct Holders {
    /// Each holder, with the links it holds the curtain on: a client on a new link and an old
    /// one not yet timed out holds it on both. An empty set is a holder in its grace.
    clients: HashMap<ClientId, HashSet<u64>>,
    /// Holders whose last link went, each with the mark it went under: one that holds again
    /// before its grace ends takes its mark away, so the let-go it was due does not come.
    leaving: HashMap<ClientId, u64>,
    /// The last mark given.
    marks: u64,
    /// The last link number given.
    links: u64,
    /// The last holder went rather than let go.
    lock: bool,
    /// What renews the input hold's lease while the curtain is up.
    lease: Option<Renew>,
    /// A task renews it.
    renewing: bool,
    /// The file that says the curtain is up, while one is kept ([`Curtain::keep_marker_at`]).
    marker: Option<PathBuf>,
}

/// How a change left the curtain: where it stands, and whether that moved, so every client is
/// told.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settled {
    /// Where it stands.
    pub state: CurtainState,
    /// It went up or down, or was refused.
    pub changed: bool,
    /// It waits for the Mac to read as locked before it falls.
    pub locking: bool,
}

impl Settled {
    const fn new(state: CurtainState, changed: bool) -> Self {
        Self { state, changed, locking: false }
    }
}

/// The worker's curtain: its holders, and it on the main thread.
pub struct Curtain<D: Drapes> {
    main: Arc<dyn Main<Stage<D>>>,
    holders: Arc<Mutex<Holders>>,
    /// Moved each time the windows the captures leave out change.
    moved: Arc<watch::Sender<u64>>,
    /// Where it stands, as the main thread last settled it.
    state: Arc<watch::Sender<CurtainState>>,
}

impl<D: Drapes> Clone for Curtain<D> {
    fn clone(&self) -> Self {
        Self {
            main: Arc::clone(&self.main),
            holders: Arc::clone(&self.holders),
            moved: Arc::clone(&self.moved),
            state: Arc::clone(&self.state),
        }
    }
}

impl<D: Drapes> std::fmt::Debug for Curtain<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Curtain")
            .field("holders", &self.holders.lock().clients.len())
            .finish_non_exhaustive()
    }
}

impl<D: Drapes> Curtain<D> {
    /// Down, its stage on `main`.
    pub fn new(main: Arc<dyn Main<Stage<D>>>) -> Self {
        Self {
            main,
            holders: Arc::default(),
            moved: Arc::new(watch::Sender::new(0)),
            state: Arc::new(watch::Sender::new(CurtainState::Down)),
        }
    }

    /// `client`'s hold on the curtain through one link of its own.
    #[must_use]
    pub fn link(&self, client: ClientId) -> Link<D> {
        let id = {
            let mut holders = self.holders.lock();
            holders.links = holders.links.wrapping_add(1);
            holders.links
        };
        Link { curtain: self.clone(), client, id, alive: Arc::new(AtomicBool::new(true)) }
    }

    /// Where the curtain stands, as last settled: read without a trip to the main thread, so a
    /// stalled main thread never holds up a link's greeting.
    #[must_use]
    pub fn current(&self) -> CurtainState {
        self.state.borrow().clone()
    }

    /// Where the curtain stands now, settled on the main thread.
    pub async fn state(&self) -> CurtainState {
        self.settle().await.state
    }

    /// `client` lets go of the curtain on every link: on the person's word, or by going
    /// (`gone`). The last holder letting go lowers it, and when it went the Mac locks first and
    /// the curtain falls once it reads as locked. `None` when it held none.
    async fn let_go(&self, client: ClientId, gone: bool) -> Option<Settled> {
        {
            let mut holders = self.holders.lock();
            holders.leaving.remove(&client);
            holders.clients.remove(&client)?;
            if holders.clients.is_empty() {
                holders.lock = gone;
            }
        }
        let mut settled = self.settle().await;
        let mut changed = settled.changed;
        let mut waited = Duration::ZERO;
        while settled.locking && waited < LOCK_WAIT {
            tokio::time::sleep(LOCK_POLL).await;
            waited = waited.saturating_add(LOCK_POLL);
            settled = self.settle().await;
            changed |= settled.changed;
        }
        if settled.locking {
            tracing::warn!("the Mac did not read as locked; the shield stays");
        }
        settled.changed = changed;
        Some(settled)
    }

    /// Said each time the shield's windows came, went or moved: a display stream then takes
    /// its filter again, so its picture shows the session rather than the shield.
    #[must_use]
    pub fn shield_moved(&self) -> watch::Receiver<u64> {
        self.moved.subscribe()
    }

    /// The displays were reconfigured: the shield covers each screen there is now.
    pub fn reconfigured(&self) {
        let moved = Arc::clone(&self.moved);
        self.main.run(Box::new(move |stage: &mut Stage<D>| {
            if let Some(raised) = stage.raised.as_mut()
                && stage.drapes.follow(raised)
            {
                bump(&moved);
            }
        }));
    }

    /// Bring the curtain on the main thread in line with its holders, and keep the input
    /// hold's lease renewed while it is up.
    async fn settle(&self) -> Settled {
        let (tx, rx) = oneshot::channel();
        let (holders, moved, state) =
            (Arc::clone(&self.holders), Arc::clone(&self.moved), Arc::clone(&self.state));
        self.main.run(Box::new(move |stage: &mut Stage<D>| {
            let _asker_gone = tx.send(stage.settle(&holders, &moved, &state));
        }));
        let settled = rx.await.unwrap_or_else(|_main_gone| {
            let why = "the worker's main thread is gone".to_owned();
            Settled::new(CurtainState::Refused { why }, false)
        });
        if settled.changed {
            self.mark(matches!(settled.state, CurtainState::Up { .. }));
        }
        self.keep_renewing();
        settled
    }

    /// Keep the file at `path` while the curtain is up, so a worker that ends with it up, by a
    /// kill or a crash that locks nothing, finds the file as it starts again. Found now, the
    /// Mac is locked first, before any client is served, and the file goes once it reads as
    /// locked: the shield went with the process that drew it, so the desk would be open.
    pub async fn keep_marker_at(&self, path: PathBuf) {
        let found = path.exists();
        self.holders.lock().marker = Some(path);
        if !found {
            return;
        }
        tracing::warn!("the last worker ended with the curtain up; locking the Mac first");
        let answer = self.lock_and_confirm(false);
        let locked = tokio::task::spawn_blocking(move || answer.recv_timeout(EXIT_WAIT))
            .await
            .is_ok_and(|answer| answer == Ok(true));
        if !locked {
            tracing::warn!("the Mac did not read as locked; the marker stays for the next start");
        }
    }

    /// Lock the Mac now when the curtain is up, for a worker about to end: the shield and the
    /// input hold end with the process, so the desk must be locked before they go. Answered
    /// `true` once the Mac reads as locked or when the curtain was down, `false` when the lock
    /// could not be asked for or did not land within `LOCK_WAIT`. Blocking on the answer is
    /// for a thread off the main one with no runtime left, as a panicked daemon's is
    /// ([`EXIT_WAIT`] bounds it).
    #[must_use]
    pub fn lock_for_exit(&self) -> Receiver<bool> {
        self.lock_and_confirm(true)
    }

    /// Ask the lock on the main thread, only while the curtain is up when `only_up`, and answer
    /// once it reads as locked (the marker gone then), it fails, or [`LOCK_WAIT`] passed.
    fn lock_and_confirm(&self, only_up: bool) -> Receiver<bool> {
        let (tx, rx) = sync_channel(1);
        let main = Arc::clone(&self.main);
        let holders = Arc::clone(&self.holders);
        self.main.run(Box::new(move |stage: &mut Stage<D>| {
            if only_up && stage.raised.is_none() {
                let _asker_gone = tx.send(true);
                return;
            }
            if let Err(why) = stage.drapes.lock() {
                tracing::warn!(%why, "the Mac not locked before the worker ends");
                let _asker_gone = tx.send(false);
                return;
            }
            let polls = LOCK_WAIT.as_millis().checked_div(LOCK_POLL.as_millis()).unwrap_or(0);
            let polls = u32::try_from(polls).unwrap_or(u32::MAX);
            confirm_locked(stage, &main, holders, tx, polls);
        }));
        rx
    }

    /// Keep the marker while the curtain is `up`, and take it away once it is down.
    fn mark(&self, up: bool) {
        let Some(path) = self.holders.lock().marker.clone() else { return };
        let kept = if up { std::fs::write(&path, b"") } else { unmark(&path) };
        if let Err(e) = kept {
            tracing::warn!(path = %path.display(), error = %e, up, "the curtain's marker");
        }
    }

    /// Renew the input hold's lease from the runtime every [`RENEW_EVERY`] while there is one:
    /// a runtime that stalls lets it lapse, and the desk has its input back.
    fn keep_renewing(&self) {
        {
            let mut holders = self.holders.lock();
            if holders.lease.is_none() || holders.renewing {
                return;
            }
            holders.renewing = true;
        }
        let holders = Arc::clone(&self.holders);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(RENEW_EVERY).await;
                let lease = {
                    let mut holders = holders.lock();
                    let lease = holders.lease.clone();
                    holders.renewing = lease.is_some();
                    lease
                };
                let Some(lease) = lease else { return };
                lease();
            }
        });
    }
}

/// One link's hold on the curtain for its client. A client holds it while any of its links
/// does, so a new link that holds it before the old one times out keeps it up through the old
/// one's end.
pub struct Link<D: Drapes> {
    curtain: Curtain<D>,
    client: ClientId,
    id: u64,
    /// Turned off, under the holders' lock, as the link goes: a hold asked on it that comes
    /// after holds nothing, so it never outlives the link.
    alive: Arc<AtomicBool>,
}

impl<D: Drapes> Clone for Link<D> {
    fn clone(&self) -> Self {
        Self {
            curtain: self.curtain.clone(),
            client: self.client,
            id: self.id,
            alive: Arc::clone(&self.alive),
        }
    }
}

impl<D: Drapes> std::fmt::Debug for Link<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("client", &self.client)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<D: Drapes> Link<D> {
    /// The client holds the curtain on this link: up once the Mac has it. On a link gone,
    /// nothing is held, and where the curtain stands is said.
    pub async fn hold(&self) -> Settled {
        {
            let mut holders = self.curtain.holders.lock();
            if !self.alive.load(Ordering::Acquire) {
                drop(holders);
                return Settled::new(self.curtain.current(), false);
            }
            holders.clients.entry(self.client).or_default().insert(self.id);
            holders.leaving.remove(&self.client);
            holders.lock = false;
        }
        self.curtain.settle().await
    }

    /// The client lets go of the curtain on the person's word, on every link: the last holder
    /// letting go lowers it, the Mac left unlocked. `None` when it held none.
    pub async fn let_go(&self) -> Option<Settled> {
        self.curtain.let_go(self.client, false).await
    }

    /// The link went. The client still holds the curtain while another of its links does;
    /// else it lets go, as gone, once `grace` has passed without it holding the curtain again,
    /// and `told` hears how that left it. The curtain stays up meanwhile, so a link that blips
    /// (a phone between networks) neither shows the session at the desk nor locks the Mac.
    pub fn went(&self, grace: Duration, told: impl FnOnce(Settled) + Send + 'static) {
        let mark = {
            let mut holders = self.curtain.holders.lock();
            self.alive.store(false, Ordering::Release);
            let Some(links) = holders.clients.get_mut(&self.client) else { return };
            links.remove(&self.id);
            if !links.is_empty() {
                return;
            }
            holders.marks = holders.marks.wrapping_add(1);
            let mark = holders.marks;
            holders.leaving.insert(self.client, mark);
            mark
        };
        let (curtain, client) = (self.curtain.clone(), self.client);
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let still = {
                let mut holders = curtain.holders.lock();
                let still = holders.leaving.get(&client) == Some(&mark);
                if still {
                    holders.leaving.remove(&client);
                }
                still
            };
            if still && let Some(settled) = curtain.let_go(client, true).await {
                told(settled);
            }
        });
    }
}

/// The curtain on a Mac: [`slopty_platform::curtain`]'s shield over every display but those made
/// for clients, its input hold letting the worker's own events through, and its lock.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Mac;

/// A Mac's curtain while it is up.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct MacRaised {
    shield: slopty_platform::curtain::Shield,
    hold: Option<slopty_platform::curtain::InputHold>,
}

#[cfg(target_os = "macos")]
impl Drapes for Mac {
    type Raised = MacRaised;

    fn raise(&mut self) -> Result<MacRaised, String> {
        use slopty_input::backend::SLOPTY_EVENT;
        use slopty_platform::curtain::{InputHold, Shield, release_held};

        let mtm =
            objc2::MainThreadMarker::new().ok_or("the curtain is drawn on the main thread")?;
        let mut shield = Shield::new(mtm, &slopty_vdisplay::made_for_a_client);
        // Out of every capture before it is on a screen.
        slopty_capture::leave_out(shield.window_ids());
        shield.show();
        let hold = InputHold::start(SLOPTY_EVENT, HOLD_LEASE)
            .inspect_err(|e| tracing::warn!(error = %e, "the Mac's own input not held off"))
            .ok();
        if hold.is_some() {
            // What the desk held down as the hold began would stay down under the session.
            release_held(SLOPTY_EVENT);
        }
        Ok(MacRaised { shield, hold })
    }

    fn input_held(&self, raised: &MacRaised) -> bool {
        raised.hold.is_some()
    }

    fn lease(&self, raised: &MacRaised) -> Option<Renew> {
        let lease = raised.hold.as_ref()?.lease();
        Some(Arc::new(move || lease.renew()))
    }

    fn follow(&mut self, raised: &mut MacRaised) -> bool {
        let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
        let changed = raised.shield.cover(mtm, &slopty_vdisplay::made_for_a_client);
        if changed {
            slopty_capture::leave_out(raised.shield.window_ids());
        }
        changed
    }

    fn lock(&mut self) -> Result<(), String> {
        slopty_platform::curtain::lock_screen().map_err(|e| e.to_string())
    }

    fn locked(&self) -> bool {
        use slopty_capture::Console;
        matches!(slopty_capture::console(), Some(Console::Locked | Console::Away))
    }

    fn lower(&mut self, raised: MacRaised) {
        let MacRaised { shield, hold } = raised;
        drop(hold);
        drop(shield);
        slopty_capture::leave_out(Vec::new());
    }
}

/// The curtain this platform draws: a Mac's.
#[cfg(target_os = "macos")]
pub type Native = Mac;

/// Elsewhere no curtain is drawn: a worker there says it has none (`WorkerCaps::curtain`), and
/// never makes one.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

#[cfg(not(target_os = "macos"))]
impl Drapes for Native {
    /// Never made: [`Self::raise`] always refuses, so the rest answer for a curtain never up.
    type Raised = ();

    fn raise(&mut self) -> Result<Self::Raised, String> {
        Err("only a Mac draws the curtain".to_owned())
    }

    fn input_held(&self, (): &Self::Raised) -> bool {
        false
    }

    fn lease(&self, (): &Self::Raised) -> Option<Renew> {
        None
    }

    fn follow(&mut self, (): &mut Self::Raised) -> bool {
        false
    }

    fn lock(&mut self) -> Result<(), String> {
        Err("only a Mac draws the curtain".to_owned())
    }

    fn locked(&self) -> bool {
        false
    }

    fn lower(&mut self, (): Self::Raised) {}
}

/// The worker's curtain, on the main queue, following every display reconfiguration. `None` off
/// the main thread.
#[cfg(target_os = "macos")]
#[must_use]
pub fn on_main_queue() -> Option<Curtain<Mac>> {
    let main = super::sized::MainQueue::new(Stage::new(Mac))?;
    let curtain = Curtain::new(Arc::new(main));
    let notified = curtain.clone();
    if let Err(e) = slopty_vdisplay::on_reconfiguration(move || notified.reconfigured()) {
        tracing::warn!(error = %e, "no display reconfiguration notices; the shield stays as raised");
    }
    Some(curtain)
}

#[cfg(test)]
mod tests;
