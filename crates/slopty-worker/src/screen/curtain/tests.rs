use std::time::Duration;

use tokio::sync::mpsc;

use super::*;
use crate::screen::sized::Job;

/// What the fake drew, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Drawn {
    Raised,
    Followed,
    Locked,
    Lowered,
}

/// Drapes that note what they drew, refuse to raise while `refuse` says why, hold the input
/// unless `deaf`, count the lease's renewals, refuse the lock while `unlockable`, and read as
/// locked once `locked` says so.
#[derive(Clone, Default)]
struct Fake {
    drawn: Arc<Mutex<Vec<Drawn>>>,
    refuse: Arc<Mutex<Option<String>>>,
    deaf: bool,
    renewed: Arc<std::sync::atomic::AtomicU32>,
    unlockable: bool,
    locked: Arc<AtomicBool>,
}

impl Fake {
    /// A fake whose lock lands at once.
    fn locking() -> Self {
        let fake = Self::default();
        fake.locked.store(true, Ordering::Relaxed);
        fake
    }

    fn drawn(&self) -> Vec<Drawn> {
        self.drawn.lock().clone()
    }
}

impl Drapes for Fake {
    type Raised = ();

    fn raise(&mut self) -> Result<(), String> {
        let refused = self.refuse.lock().clone();
        if let Some(why) = refused {
            return Err(why);
        }
        self.drawn.lock().push(Drawn::Raised);
        Ok(())
    }

    fn input_held(&self, (): &()) -> bool {
        !self.deaf
    }

    fn lease(&self, (): &()) -> Option<Renew> {
        if self.deaf {
            return None;
        }
        let renewed = Arc::clone(&self.renewed);
        Some(Arc::new(move || {
            renewed.fetch_add(1, Ordering::Relaxed);
        }))
    }

    fn follow(&mut self, (): &mut ()) -> bool {
        self.drawn.lock().push(Drawn::Followed);
        true
    }

    fn lock(&mut self) -> Result<(), String> {
        if self.unlockable {
            return Err("no login framework".to_owned());
        }
        self.drawn.lock().push(Drawn::Locked);
        Ok(())
    }

    fn locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    fn lower(&mut self, (): ()) {
        self.drawn.lock().push(Drawn::Lowered);
    }
}

/// A task that owns the stage and runs jobs one at a time, as the main queue does.
struct TaskMain<S>(mpsc::UnboundedSender<Job<S>>);

impl<S: Send + 'static> Main<S> for TaskMain<S> {
    fn run(&self, job: Job<S>) {
        let _gone = self.0.send(job);
    }

    fn run_after(&self, delay: Duration, job: Job<S>) {
        let tx = self.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _gone = tx.send(job);
        });
    }
}

fn curtain(fake: &Fake) -> Curtain<Fake> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job<Stage<Fake>>>();
    let mut stage = Stage::new(fake.clone());
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            job(&mut stage);
        }
    });
    Curtain::new(Arc::new(TaskMain(tx)))
}

const fn up(holders: u32) -> CurtainState {
    CurtainState::Up { holders, input_held: true }
}

/// What a test tells about a let-go past its grace.
type Told = Arc<Mutex<Vec<Settled>>>;

fn tell(told: &Told) -> impl FnOnce(Settled) + Send + 'static {
    let told = Arc::clone(told);
    move |settled| told.lock().push(settled)
}

/// The first holder raises the curtain and the last lowers it, once each; a second holder and
/// a let-go of one that held none move nothing. Letting go on the person's word leaves the Mac
/// unlocked, and the last holder going locks it before the curtain falls.
#[tokio::test(start_paused = true)]
async fn the_curtain_is_up_while_any_client_holds_it_and_locks_when_the_last_goes() {
    let fake = Fake::locking();
    let curtain = curtain(&fake);
    let (a, b) = (curtain.link(ClientId::new()), curtain.link(ClientId::new()));
    let grace = Duration::from_secs(20);

    let first = a.hold().await;
    assert_eq!((first.state, first.changed), (up(1), true));
    let second = b.hold().await;
    assert_eq!((second.state, second.changed), (up(2), false));
    assert_eq!(curtain.link(ClientId::new()).let_go().await, None, "it held none");
    assert_eq!(a.let_go().await.map(|s| (s.state, s.changed)), Some((up(1), false)));
    let fell = b.let_go().await.map(|s| (s.state, s.changed));
    assert_eq!(fell, Some((CurtainState::Down, true)));
    assert_eq!(fake.drawn(), [Drawn::Raised, Drawn::Lowered], "on the word: no lock");
    assert_eq!(curtain.current(), CurtainState::Down, "as last settled");

    let told = Told::default();
    a.hold().await;
    b.hold().await;
    a.went(grace, tell(&told));
    tokio::time::sleep(grace * 2).await;
    assert_eq!(fake.drawn().len(), 3, "still up for b");
    assert_eq!(curtain.current(), up(1));
    b.went(grace, tell(&told));
    tokio::time::sleep(grace * 2).await;
    assert_eq!(fake.drawn()[3..], [Drawn::Locked, Drawn::Lowered], "b went: locked, then down");
    assert_eq!(told.lock().last().map(|s| s.state.clone()), Some(CurtainState::Down));
}

/// A curtain that cannot be raised is refused in its words and holds nobody, so the next ask
/// tries again; one raised without the input held says so; the displays changing while it is
/// up re-cover them, and while it is down do nothing.
#[tokio::test]
async fn a_curtain_refused_holds_nobody_and_a_raised_one_follows_the_displays() {
    let fake = Fake { deaf: true, ..Fake::default() };
    let curtain = curtain(&fake);
    let a = curtain.link(ClientId::new());
    *fake.refuse.lock() = Some("no screen".to_owned());
    let refused = a.hold().await;
    let why = CurtainState::Refused { why: "no screen".to_owned() };
    assert_eq!((refused.state, refused.changed), (why, true));
    assert_eq!(a.let_go().await, None, "nobody holds it");

    curtain.reconfigured();
    *fake.refuse.lock() = None;
    let raised = a.hold().await;
    let deaf = CurtainState::Up { holders: 1, input_held: false };
    assert_eq!((raised.state, raised.changed), (deaf.clone(), true));
    curtain.reconfigured();
    assert_eq!(curtain.state().await, deaf);
    assert_eq!(fake.drawn(), [Drawn::Raised, Drawn::Followed]);
}

/// The shield moving is said each time the curtain goes up or down, so a display stream takes
/// its filter again; a second holder moves nothing.
#[tokio::test]
async fn a_display_stream_hears_the_shield_move_as_the_curtain_goes_up_and_down() {
    let fake = Fake::default();
    let curtain = curtain(&fake);
    let mut moved = curtain.shield_moved();
    moved.mark_unchanged();
    let (a, b) = (curtain.link(ClientId::new()), curtain.link(ClientId::new()));
    a.hold().await;
    assert!(moved.has_changed().unwrap(), "up");
    moved.mark_unchanged();
    b.hold().await;
    assert!(!moved.has_changed().unwrap(), "already up");
    a.let_go().await;
    b.let_go().await;
    assert!(moved.has_changed().unwrap(), "down");
}

/// A holder whose link went keeps the curtain up through its grace: holding again on its next
/// link within it moves nothing and locks nothing, and one that does not come back lets go once
/// the grace has passed, the Mac locked before the curtain falls, and every client told.
#[tokio::test(start_paused = true)]
async fn a_link_that_blips_keeps_the_curtain_and_one_gone_past_its_grace_locks_the_mac() {
    let fake = Fake::locking();
    let curtain = curtain(&fake);
    let client = ClientId::new();
    let grace = Duration::from_secs(20);
    let told = Told::default();

    let old = curtain.link(client);
    old.hold().await;
    old.went(grace, tell(&told));
    tokio::time::sleep(grace / 2).await;
    let new = curtain.link(client);
    assert_eq!(new.hold().await.state, up(1), "back on its next link");
    tokio::time::sleep(grace).await;
    assert_eq!(curtain.state().await, up(1), "the blip's let-go never came");
    assert_eq!(fake.drawn(), [Drawn::Raised]);
    assert!(told.lock().is_empty());

    new.went(grace, tell(&told));
    curtain.link(ClientId::new()).went(grace, tell(&told));
    tokio::time::sleep(grace / 2).await;
    assert_eq!(curtain.state().await, up(1), "up through the grace");
    tokio::time::sleep(grace).await;
    assert_eq!(curtain.state().await, CurtainState::Down);
    assert_eq!(fake.drawn(), [Drawn::Raised, Drawn::Locked, Drawn::Lowered]);
    let told: Vec<_> = told.lock().iter().map(|s| (s.state.clone(), s.changed)).collect();
    assert_eq!(told, [(CurtainState::Down, true)]);
}

/// A client that relinks holds on its new link before its old link times out: the old link's
/// end starts no grace while the new one holds, so the Mac neither locks nor shows the session
/// while the client drives. A hold asked on a link that has gone holds nothing.
#[tokio::test(start_paused = true)]
async fn a_new_link_holding_keeps_the_curtain_through_the_old_link_timing_out() {
    let fake = Fake::locking();
    let curtain = curtain(&fake);
    let client = ClientId::new();
    let grace = Duration::from_secs(20);
    let told = Told::default();

    let old = curtain.link(client);
    old.hold().await;
    let new = curtain.link(client);
    new.hold().await;
    // The old link idles out well after the new one holds.
    old.went(grace, tell(&told));
    tokio::time::sleep(grace * 3).await;
    assert_eq!(curtain.state().await, up(1), "still held on the new link");
    assert_eq!(fake.drawn(), [Drawn::Raised], "never locked, never lowered");
    assert!(told.lock().is_empty());

    // A hold that comes after its link went holds nothing.
    let other = curtain.link(ClientId::new());
    other.went(grace, tell(&told));
    let late = other.hold().await;
    assert_eq!((late.state, late.changed), (up(1), false), "nothing held on a gone link");
    new.let_go().await;
    assert_eq!(curtain.state().await, CurtainState::Down, "no holder left behind");
}

/// The curtain stays up until the Mac reads as locked, and one whose lock could not be asked
/// for keeps its shield until a holder lifts it on the person's word.
#[tokio::test(start_paused = true)]
async fn the_curtain_falls_only_once_the_mac_reads_as_locked() {
    let grace = Duration::from_secs(20);
    let fake = Fake::default();
    let locks = curtain(&fake);
    let a = locks.link(ClientId::new());
    a.hold().await;
    a.went(grace, |_| {});
    tokio::time::sleep(grace + LOCK_WAIT / 2).await;
    assert_eq!(fake.drawn(), [Drawn::Raised, Drawn::Locked], "asked, not yet locked");
    assert_eq!(locks.current(), up(0), "the shield stays while it locks");
    fake.locked.store(true, Ordering::Relaxed);
    tokio::time::sleep(LOCK_WAIT).await;
    assert_eq!(fake.drawn(), [Drawn::Raised, Drawn::Locked, Drawn::Lowered]);
    assert_eq!(locks.current(), CurtainState::Down);

    let fake = Fake { unlockable: true, ..Fake::default() };
    let stuck = curtain(&fake);
    let a = stuck.link(ClientId::new());
    a.hold().await;
    a.went(grace, |_| {});
    tokio::time::sleep(grace + LOCK_WAIT * 2).await;
    assert_eq!(fake.drawn(), [Drawn::Raised], "no lock: never lowered");
    assert_eq!(stuck.state().await, up(0));
    let b = stuck.link(ClientId::new());
    b.hold().await;
    b.let_go().await;
    assert_eq!(fake.drawn(), [Drawn::Raised, Drawn::Lowered], "lifted on the word");
}

/// The input hold's lease is renewed from the runtime while the curtain is up, and no longer
/// once it is down.
#[tokio::test(start_paused = true)]
async fn the_hold_s_lease_is_renewed_while_the_curtain_is_up() {
    let fake = Fake::default();
    let curtain = curtain(&fake);
    let a = curtain.link(ClientId::new());
    a.hold().await;
    tokio::time::sleep(RENEW_EVERY * 4 + RENEW_EVERY / 2).await;
    let renewed = fake.renewed.load(Ordering::Relaxed);
    assert_eq!(renewed, 4, "once a period");
    a.let_go().await;
    tokio::time::sleep(RENEW_EVERY * 4).await;
    let after = fake.renewed.load(Ordering::Relaxed);
    assert!(after <= renewed + 1, "down: no more renewals, {renewed} then {after}");
    assert!(HOLD_LEASE > RENEW_EVERY * 2, "a renewal or two may be late without a lapse");
}
