//! Keeping the server's machine out of idle sleep while the fleet needs it.
//!
//! Every notice that reaches the person on another device, every project step and every wake
//! goes through the server, so a server's Mac that idles to sleep silences them all, the agents
//! on other machines included. The server holds its machine awake while a person's client is
//! linked or an agent works on any worker (a thread working, or waiting on its own background
//! work, in a table the workers publish). The person narrows it with the machine's own
//! `keep_awake` (`[worker] keep_awake`, the same choice for the worker and the server of one
//! machine), followed as the file changes: to linked clients only, or to nothing. A hold does
//! not keep a laptop with its lid closed awake; the setup's Server line says so on a Mac with a
//! battery.

/// The operating-system assertion the server takes and lets go.
pub trait Hold: Send + std::fmt::Debug {
    /// Hold (or release) the machine out of idle sleep.
    fn system(&mut self, hold: bool);
}

/// What the person lets keep the server's machine awake.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Policy {
    /// A person's client linked, or an agent at work on any worker.
    #[default]
    Working,
    /// A person's client linked only.
    Attached,
    /// Nothing: the machine sleeps as its owner set it to.
    Never,
}

/// The hold, as what is linked and at work implies it under the policy.
#[derive(Debug, Default)]
pub(super) struct Awake {
    policy: Policy,
    held: bool,
    hold: Option<Box<dyn Hold>>,
}

impl Awake {
    /// Hold through `hold` from now on, under `policy`, given whether a person is linked and
    /// an agent is at work now.
    pub(super) fn keep(
        &mut self,
        hold: Box<dyn Hold>,
        policy: Policy,
        linked: bool,
        working: bool,
    ) {
        self.hold = Some(hold);
        self.held = false;
        self.policy = policy;
        self.settle(linked, working);
    }

    /// The person changed the policy while the server runs.
    pub(super) fn set_policy(&mut self, policy: Policy, linked: bool, working: bool) {
        self.policy = policy;
        self.settle(linked, working);
    }

    /// Hold exactly while the policy lets what is linked and at work hold; only a change
    /// reaches the assertion.
    pub(super) fn settle(&mut self, linked: bool, working: bool) {
        let hold = match self.policy {
            Policy::Working => linked || working,
            Policy::Attached => linked,
            Policy::Never => false,
        };
        let Some(holds) = self.hold.as_mut() else { return };
        if hold != self.held {
            self.held = hold;
            holds.system(hold);
        }
    }
}

impl super::Hub {
    /// Hold the server's machine through `hold` from now on, under `policy`.
    pub fn keep_awake(&self, hold: Box<dyn Hold>, policy: Policy) {
        self.inner.state.lock().board.keep_awake(hold, policy);
    }

    /// The person changed what keeps the machine awake while the server runs.
    pub fn set_keep_awake(&self, policy: Policy) {
        self.inner.state.lock().board.set_keep_awake(policy);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use slopty_core::{SessionId, WorkerId};
    use slopty_proto::thread::Phase;
    use tokio::sync::mpsc;

    use super::super::Hub;
    use super::super::ladder::tests::{Client, row, snapshot};
    use super::super::tests::{registration, summary};
    use super::*;

    /// Every change of the hold, in order.
    #[derive(Debug)]
    struct Log(Arc<Mutex<Vec<bool>>>);

    impl Hold for Log {
        fn system(&mut self, hold: bool) {
            self.0.lock().push(hold);
        }
    }

    /// Each of what is linked and at work holds as far as the policy lets it, a change of the
    /// policy applies at once, and only a change of the hold reaches the assertion.
    #[test]
    fn the_policy_decides_what_holds() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut awake = Awake::default();
        awake.keep(Box::new(Log(Arc::clone(&log))), Policy::Working, false, true);
        awake.settle(true, true);
        awake.settle(true, false);
        assert_eq!(*log.lock(), [true], "held from the first agent on, once");
        awake.settle(false, false);
        awake.settle(false, true);
        awake.set_policy(Policy::Attached, false, true);
        assert_eq!(*log.lock(), [true, false, true, false], "an agent alone holds nothing");
        awake.settle(true, true);
        awake.set_policy(Policy::Never, true, true);
        assert_eq!(*log.lock(), [true, false, true, false, true, false]);
    }

    /// The server holds its Mac while an agent works on any worker, or waits on its own
    /// background work, and while a person's client is linked; an agent at rest, or whose
    /// worker went, holds nothing.
    #[tokio::test]
    async fn the_server_holds_the_mac_awake_while_an_agent_works_or_a_client_links() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        hub.keep_awake(Box::new(Log(Arc::clone(&log))), Policy::Working);
        assert_eq!(*log.lock(), Vec::<bool>::new(), "nothing to hold for yet");

        let (worker, session) = (WorkerId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub
            .register(registration(worker, vec![summary(session)]), [100, 64, 0, 9].into(), tx)
            .unwrap();
        lease.handle(snapshot(vec![row(Phase::Working, 10, Some(session))]));
        hub.rank_ladder();
        assert_eq!(*log.lock(), [true], "a Linux worker's agent at work");
        lease.handle(snapshot(vec![row(Phase::Waiting, 20, Some(session))]));
        hub.rank_ladder();
        assert_eq!(*log.lock(), [true], "its background work still out");
        lease.handle(snapshot(vec![row(Phase::Idle, 30, Some(session))]));
        hub.rank_ladder();
        assert_eq!(*log.lock(), [true, false], "at rest");

        let phone = Client::sit(&hub, "phone");
        assert_eq!(*log.lock(), [true, false, true], "a person linked");
        hub.set_keep_awake(Policy::Never);
        assert_eq!(*log.lock(), [true, false, true, false], "the person's choice applies at once");
        hub.set_keep_awake(Policy::Attached);
        drop(phone);
        assert_eq!(*log.lock(), [true, false, true, false, true, false], "the link ended");
        lease.handle(snapshot(vec![row(Phase::Working, 40, Some(session))]));
        hub.rank_ladder();
        assert_eq!(log.lock().len(), 6, "an agent alone holds nothing while attached only");
    }
}
