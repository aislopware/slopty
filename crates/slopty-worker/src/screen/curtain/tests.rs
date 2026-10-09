use std::time::Duration;

use tokio::sync::mpsc;

use super::*;
use crate::screen::sized::Job;

/// What the fake drew, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Drawn {
    Raised,
    Followed,
    Lowered { lock: bool },
}

/// Drapes that note what they drew, refuse to raise while `refuse` says why, and hold the input
/// unless `deaf`.
#[derive(Clone, Default)]
struct Fake {
    drawn: Arc<Mutex<Vec<Drawn>>>,
    refuse: Arc<Mutex<Option<String>>>,
    deaf: bool,
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

    fn follow(&mut self, (): &mut ()) -> bool {
        self.drawn.lock().push(Drawn::Followed);
        true
    }

    fn lower(&mut self, (): (), lock: bool) {
        self.drawn.lock().push(Drawn::Lowered { lock });
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

/// The first holder raises the curtain and the last lowers it, once each; a second holder and
/// a let-go of one that held none move nothing. Letting go on the person's word leaves the Mac
/// unlocked, and the last holder going locks it as the curtain falls.
#[tokio::test]
async fn the_curtain_is_up_while_any_client_holds_it_and_locks_when_the_last_goes() {
    let fake = Fake::default();
    let curtain = curtain(&fake);
    let (a, b) = (ClientId::new(), ClientId::new());

    assert_eq!(curtain.hold(a).await, Settled { state: up(1), changed: true });
    assert_eq!(curtain.hold(b).await, Settled { state: up(2), changed: false });
    assert_eq!(curtain.let_go(ClientId::new(), true).await, None, "it held none");
    assert_eq!(curtain.let_go(a, false).await, Some(Settled { state: up(1), changed: false }));
    let fell = curtain.let_go(b, false).await;
    assert_eq!(fell, Some(Settled { state: CurtainState::Down, changed: true }));
    assert_eq!(*fake.drawn.lock(), [Drawn::Raised, Drawn::Lowered { lock: false }]);

    curtain.hold(a).await;
    curtain.hold(b).await;
    curtain.let_go(a, true).await;
    assert_eq!(fake.drawn.lock().len(), 3, "still up for b");
    curtain.let_go(b, true).await;
    assert_eq!(fake.drawn.lock().last(), Some(&Drawn::Lowered { lock: true }), "b went");

    curtain.hold(a).await;
    curtain.let_go(a, true).await;
    curtain.hold(b).await;
    let asked = curtain.let_go(b, false).await;
    assert_eq!(asked.map(|s| s.state), Some(CurtainState::Down));
    assert_eq!(fake.drawn.lock().last(), Some(&Drawn::Lowered { lock: false }), "a hold since");
}

/// A curtain that cannot be raised is refused in its words and holds nobody, so the next ask
/// tries again; one raised without the input held says so; the displays changing while it is
/// up re-cover them, and while it is down do nothing.
#[tokio::test]
async fn a_curtain_refused_holds_nobody_and_a_raised_one_follows_the_displays() {
    let fake = Fake { deaf: true, ..Fake::default() };
    let curtain = curtain(&fake);
    let a = ClientId::new();
    *fake.refuse.lock() = Some("no screen".to_owned());
    let refused = curtain.hold(a).await;
    let why = CurtainState::Refused { why: "no screen".to_owned() };
    assert_eq!(refused, Settled { state: why, changed: true });
    assert_eq!(curtain.let_go(a, true).await, None, "nobody holds it");

    curtain.reconfigured();
    *fake.refuse.lock() = None;
    let raised = curtain.hold(a).await;
    let deaf = CurtainState::Up { holders: 1, input_held: false };
    assert_eq!(raised, Settled { state: deaf, changed: true });
    curtain.reconfigured();
    assert_eq!(curtain.state().await, CurtainState::Up { holders: 1, input_held: false });
    assert_eq!(*fake.drawn.lock(), [Drawn::Raised, Drawn::Followed]);
}

/// The shield moving is said each time the curtain goes up or down, so a display stream takes
/// its filter again; a second holder moves nothing.
#[tokio::test]
async fn a_display_stream_hears_the_shield_move_as_the_curtain_goes_up_and_down() {
    let fake = Fake::default();
    let curtain = curtain(&fake);
    let mut moved = curtain.shield_moved();
    moved.mark_unchanged();
    let (a, b) = (ClientId::new(), ClientId::new());
    curtain.hold(a).await;
    assert!(moved.has_changed().unwrap(), "up");
    moved.mark_unchanged();
    curtain.hold(b).await;
    assert!(!moved.has_changed().unwrap(), "already up");
    curtain.let_go(a, false).await;
    curtain.let_go(b, false).await;
    assert!(moved.has_changed().unwrap(), "down");
}

/// A holder whose link went keeps the curtain up through its grace: holding again on its next
/// link within it moves nothing and locks nothing, and one that does not come back lets go once
/// the grace has passed, the Mac locked as the curtain falls, and every client told.
#[tokio::test(start_paused = true)]
async fn a_link_that_blips_keeps_the_curtain_and_one_gone_past_its_grace_locks_the_mac() {
    let fake = Fake::default();
    let curtain = curtain(&fake);
    let a = ClientId::new();
    let grace = Duration::from_secs(20);
    let told = Arc::new(Mutex::new(Vec::new()));
    let tell = |told: &Arc<Mutex<Vec<Settled>>>| {
        let told = Arc::clone(told);
        move |settled| told.lock().push(settled)
    };

    curtain.hold(a).await;
    curtain.went(a, grace, tell(&told));
    tokio::time::sleep(grace / 2).await;
    assert_eq!(curtain.hold(a).await.state, up(1), "back on its next link");
    tokio::time::sleep(grace).await;
    assert_eq!(curtain.state().await, up(1), "the blip's let-go never came");
    assert_eq!(*fake.drawn.lock(), [Drawn::Raised]);
    assert!(told.lock().is_empty());

    curtain.went(a, grace, tell(&told));
    curtain.went(ClientId::new(), grace, tell(&told));
    tokio::time::sleep(grace / 2).await;
    assert_eq!(curtain.state().await, up(1), "up through the grace");
    tokio::time::sleep(grace).await;
    assert_eq!(curtain.state().await, CurtainState::Down);
    assert_eq!(fake.drawn.lock().last(), Some(&Drawn::Lowered { lock: true }));
    assert_eq!(*told.lock(), [Settled { state: CurtainState::Down, changed: true }]);
}
