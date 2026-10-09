//! The curtain over the worker's Mac while a client holds it.
//!
//! Its own screens show a shield, and its own keyboard and pointer are held off, while the
//! clients still see and drive the session (`docs/decisions/video.md`, "The curtain").
//!
//! The holders are counted here, off the main thread, and the curtain itself lives on the main
//! thread (AppKit's windows), where each change hands a job ([`Main`]) that brings it in line
//! with the holders: raised for the first, lowered with the last. A client that goes lets go;
//! when the last one goes that way the Mac locks as the curtain falls, so the session is never
//! left open at the desk, while letting go on the person's word leaves it unlocked.
//!
//! Each display capture leaves the shield's windows out of its picture
//! (`slopty_capture::leave_out`), but a capture already running keeps the filter it was made
//! with: [`Curtain::shield_moved`] says when the shield's windows changed, and each display
//! stream takes its filter again then.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::ClientId;
use slopty_proto::screen::CurtainState;
use tokio::sync::{oneshot, watch};

use super::sized::Main;

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
    /// The displays changed: cover each screen there is now. Whether the shield's windows
    /// changed.
    fn follow(&mut self, raised: &mut Self::Raised) -> bool;
    /// Lower it, locking the Mac when `lock`.
    fn lower(&mut self, raised: Self::Raised, lock: bool);
}

/// The curtain on the main thread: what draws it, and it while it is up.
pub struct Stage<D: Drapes> {
    drapes: D,
    raised: Option<D::Raised>,
}

impl<D: Drapes> std::fmt::Debug for Stage<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stage").field("up", &self.raised.is_some()).finish_non_exhaustive()
    }
}

impl<D: Drapes> Stage<D> {
    /// Down, drawn by `drapes` when it is raised.
    pub const fn new(drapes: D) -> Self {
        Self { drapes, raised: None }
    }

    /// Bring the curtain in line with `holders`, saying on `moved` when its windows came or
    /// went.
    fn settle(&mut self, holders: &Mutex<Holders>, moved: &watch::Sender<u64>) -> Settled {
        let (count, lock) = {
            let holders = holders.lock();
            (holders.clients.len(), holders.lock)
        };
        let told = u32::try_from(count).unwrap_or(u32::MAX);
        match (count, self.raised.take()) {
            (0, Some(raised)) => {
                self.drapes.lower(raised, lock);
                bump(moved);
                Settled { state: CurtainState::Down, changed: true }
            }
            (0, None) => Settled { state: CurtainState::Down, changed: false },
            (_, Some(raised)) => {
                let input_held = self.drapes.input_held(&raised);
                self.raised = Some(raised);
                Settled { state: CurtainState::Up { holders: told, input_held }, changed: false }
            }
            (_, None) => match self.drapes.raise() {
                Ok(raised) => {
                    let input_held = self.drapes.input_held(&raised);
                    self.raised = Some(raised);
                    bump(moved);
                    let state = CurtainState::Up { holders: told, input_held };
                    Settled { state, changed: true }
                }
                Err(why) => {
                    tracing::warn!(%why, "the curtain not raised");
                    holders.lock().clients.clear();
                    Settled { state: CurtainState::Refused { why }, changed: true }
                }
            },
        }
    }
}

/// Say on `moved` that the shield's windows changed.
fn bump(moved: &watch::Sender<u64>) {
    moved.send_modify(|n| *n = n.wrapping_add(1));
}

/// Who holds the curtain, and whether it locks the Mac as it falls.
#[derive(Debug, Default)]
struct Holders {
    clients: HashSet<ClientId>,
    /// Holders whose link went, each with the mark it went under: one that holds again
    /// before its grace ends takes its mark away, so the let-go it was due does not come.
    leaving: HashMap<ClientId, u64>,
    /// The last mark given.
    marks: u64,
    /// The last holder went rather than let go.
    lock: bool,
}

/// How a change left the curtain: where it stands, and whether that moved, so every client is
/// told.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settled {
    /// Where it stands.
    pub state: CurtainState,
    /// It went up or down, or was refused.
    pub changed: bool,
}

/// The worker's curtain: its holders, and it on the main thread.
pub struct Curtain<D: Drapes> {
    main: Arc<dyn Main<Stage<D>>>,
    holders: Arc<Mutex<Holders>>,
    /// Moved each time the windows the captures leave out change.
    moved: Arc<watch::Sender<u64>>,
}

impl<D: Drapes> Clone for Curtain<D> {
    fn clone(&self) -> Self {
        Self {
            main: Arc::clone(&self.main),
            holders: Arc::clone(&self.holders),
            moved: Arc::clone(&self.moved),
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
        Self { main, holders: Arc::default(), moved: Arc::new(watch::Sender::new(0)) }
    }

    /// `client` holds the curtain: up once the Mac has it.
    pub async fn hold(&self, client: ClientId) -> Settled {
        {
            let mut holders = self.holders.lock();
            holders.clients.insert(client);
            holders.leaving.remove(&client);
            holders.lock = false;
        }
        self.settle().await
    }

    /// `client` lets go of the curtain: on the person's word, or by going (`gone`). The last
    /// holder letting go lowers it, and the Mac locks when it went. `None` when it held none.
    pub async fn let_go(&self, client: ClientId, gone: bool) -> Option<Settled> {
        {
            let mut holders = self.holders.lock();
            holders.leaving.remove(&client);
            if !holders.clients.remove(&client) {
                return None;
            }
            if holders.clients.is_empty() {
                holders.lock = gone;
            }
        }
        Some(self.settle().await)
    }

    /// `client`'s link went: it lets go, as gone, once `grace` has passed without it holding
    /// the curtain again on a new link, and `told` hears how that left it. The curtain stays up
    /// meanwhile, so a link that blips (a phone between networks) neither shows the session at
    /// the desk nor locks the Mac.
    pub fn went(
        &self,
        client: ClientId,
        grace: Duration,
        told: impl FnOnce(Settled) + Send + 'static,
    ) {
        let mark = {
            let mut holders = self.holders.lock();
            if !holders.clients.contains(&client) {
                return;
            }
            holders.marks = holders.marks.wrapping_add(1);
            let mark = holders.marks;
            holders.leaving.insert(client, mark);
            mark
        };
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let still = {
                let mut holders = this.holders.lock();
                let still = holders.leaving.get(&client) == Some(&mark);
                if still {
                    holders.leaving.remove(&client);
                }
                still
            };
            if still && let Some(settled) = this.let_go(client, true).await {
                told(settled);
            }
        });
    }

    /// Where the curtain stands now.
    pub async fn state(&self) -> CurtainState {
        self.settle().await.state
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

    /// Bring the curtain on the main thread in line with its holders.
    async fn settle(&self) -> Settled {
        let (tx, rx) = oneshot::channel();
        let (holders, moved) = (Arc::clone(&self.holders), Arc::clone(&self.moved));
        self.main.run(Box::new(move |stage: &mut Stage<D>| {
            let _asker_gone = tx.send(stage.settle(&holders, &moved));
        }));
        rx.await.unwrap_or_else(|_main_gone| Settled {
            state: CurtainState::Refused { why: "the worker's main thread is gone".to_owned() },
            changed: false,
        })
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
        let mtm =
            objc2::MainThreadMarker::new().ok_or("the curtain is drawn on the main thread")?;
        let mut shield =
            slopty_platform::curtain::Shield::new(mtm, &slopty_vdisplay::made_for_a_client);
        // Out of every capture before it is on a screen.
        slopty_capture::leave_out(shield.window_ids());
        shield.show();
        let hold = slopty_platform::curtain::InputHold::start(slopty_input::backend::SLOPTY_EVENT)
            .inspect_err(|e| tracing::warn!(error = %e, "the Mac's own input not held off"))
            .ok();
        Ok(MacRaised { shield, hold })
    }

    fn input_held(&self, raised: &MacRaised) -> bool {
        raised.hold.is_some()
    }

    fn follow(&mut self, raised: &mut MacRaised) -> bool {
        let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
        let changed = raised.shield.cover(mtm, &slopty_vdisplay::made_for_a_client);
        if changed {
            slopty_capture::leave_out(raised.shield.window_ids());
        }
        changed
    }

    fn lower(&mut self, raised: MacRaised, lock: bool) {
        let MacRaised { shield, hold } = raised;
        drop(hold);
        drop(shield);
        slopty_capture::leave_out(Vec::new());
        if lock && let Err(e) = slopty_platform::curtain::lock_screen() {
            tracing::warn!(error = %e, "the Mac not locked as the curtain fell");
        }
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

    fn follow(&mut self, (): &mut Self::Raised) -> bool {
        false
    }

    fn lower(&mut self, (): Self::Raised, _lock: bool) {}
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
