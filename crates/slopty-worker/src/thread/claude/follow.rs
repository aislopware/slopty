//! Every observed session's transcripts, followed by the kernel's own events: one follower
//! ([`crate::fswatch`]) for them all, whose changes ring the sessions that read the files moved.
//!
//! One for all, not one per session, since on Linux each follower is an inotify instance and a
//! user gets 128 by default, shared with every other program it runs; one kqueue on macOS
//! likewise holds each directory's watch once for the sessions of a project.
//!
//! A path is followed from the moment the follower takes the list naming it, so a write before
//! that is no event. A session whose new paths are not all followed yet is rung once the
//! follower says it took them ([`fswatch::Status::followed`]): its read then sees whatever was
//! written before, and every write after is an event.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Weak};

use tokio::sync::{mpsc, watch};

use crate::fswatch::{self, Changes, Limits, Status};

/// What the transcripts' following may cost: watches for a few hundred transcripts (on macOS
/// each file is a descriptor, under the limit `fswatch::raise_descriptor_limit` raised), and a
/// file the kernel cannot report on (a network volume) looked at as often as a session ticks
/// while it is busy.
const LIMITS: Limits = Limits { watches: 512, poll: super::TICK };

/// The follower shared by the observed sessions. Dropping the last handle ends it.
#[derive(Clone)]
pub struct Follows(Arc<Shared>);

struct Shared {
    /// Every session's paths together, as the follower takes them.
    lists: watch::Sender<Vec<String>>,
    /// What the follower last took.
    taken: watch::Receiver<Status>,
    /// Each joined session, by the number it joined under: a terminal's session observed
    /// again joins anew, while the one before may not have let go yet.
    sessions: parking_lot::Mutex<(u64, HashMap<u64, Session>)>,
}

struct Session {
    paths: Vec<String>,
    /// Some of `paths` are not followed yet: rung once the follower takes them.
    unwatched: bool,
    /// Rung when one of `paths` moved: a ring not yet heard holds the next.
    bell: mpsc::Sender<()>,
}

impl Session {
    /// Whether the follower follows each of its paths, as `taken` says.
    fn followed_in(&self, taken: &Status) -> bool {
        self.paths.iter().all(|p| taken.followed.binary_search(p).is_ok())
    }

    fn ring(&self) {
        // Full: a ring not yet heard reads this change too.
        let _rung = self.bell.try_send(());
    }
}

impl Follows {
    /// Start following, for the sessions that join.
    #[must_use]
    pub fn start() -> Self {
        let (lists, taken) = watch::channel(Vec::new());
        let changes = fswatch::follow(taken, LIMITS);
        let sessions = parking_lot::Mutex::new((0, HashMap::new()));
        let shared = Arc::new(Shared { lists, taken: changes.status(), sessions });
        tokio::spawn(route(Arc::downgrade(&shared), changes));
        Self(shared)
    }

    /// Follow a session's transcripts as [`Followed::set`] names them; the receiver hears when
    /// one moved. It is let go when the [`Followed`] is dropped.
    #[must_use]
    pub fn join(&self) -> (Followed, mpsc::Receiver<()>) {
        let (bell, rung) = mpsc::channel(1);
        let mut sessions = self.0.sessions.lock();
        let id = sessions.0;
        sessions.0 = id.wrapping_add(1);
        sessions.1.insert(id, Session { paths: Vec::new(), unwatched: false, bell });
        drop(sessions);
        (Followed { follows: self.clone(), id }, rung)
    }

    /// Hand the follower every session's paths, if they changed.
    fn publish(&self, sessions: &HashMap<u64, Session>) {
        let all: BTreeSet<&String> = sessions.values().flat_map(|s| &s.paths).collect();
        let all: Vec<String> = all.into_iter().cloned().collect();
        self.0.lists.send_if_modified(|list| {
            let changed = *list != all;
            if changed {
                *list = all;
            }
            changed
        });
    }
}

/// One session's place in the [`Follows`].
pub struct Followed {
    follows: Follows,
    id: u64,
}

impl Followed {
    /// Follow `paths` from now on, before the session reads them. Those not followed yet ring
    /// the session once the follower takes them. A path that is not UTF-8 cannot be named to the
    /// follower and is left out.
    pub fn set(&self, paths: &[std::path::PathBuf]) {
        let paths: Vec<String> =
            paths.iter().filter_map(|p| p.to_str()).map(str::to_owned).collect();
        // Published under the lock, so two sessions' changes reach the follower in order, and
        // judged against what it took under it, so the router's look at a take comes after.
        let mut sessions = self.follows.0.sessions.lock();
        let Some(session) = sessions.1.get_mut(&self.id) else { return };
        if session.paths == paths {
            return;
        }
        session.paths = paths;
        session.unwatched = !session.followed_in(&self.follows.0.taken.borrow());
        self.follows.publish(&sessions.1);
        drop(sessions);
    }
}

impl Drop for Followed {
    fn drop(&mut self) {
        let mut sessions = self.follows.0.sessions.lock();
        if sessions.1.remove(&self.id).is_some() {
            self.follows.publish(&sessions.1);
        }
        drop(sessions);
    }
}

/// Ring each session whose paths moved, and each whose new paths the follower took, until the
/// follower ends: once every [`Follows`] handle is gone, its lists' sender with them.
async fn route(shared: Weak<Shared>, mut changes: Changes) {
    let mut taken = changes.status();
    loop {
        tokio::select! {
            moved = changes.next() => {
                let Some(moved) = moved else { return };
                let Some(shared) = shared.upgrade() else { return };
                let moved: BTreeSet<String> = moved.into_iter().collect();
                for session in shared.sessions.lock().1.values() {
                    if session.paths.iter().any(|p| moved.contains(p)) {
                        session.ring();
                    }
                }
            }
            took = taken.changed() => {
                if took.is_err() {
                    return;
                }
                let Some(shared) = shared.upgrade() else { return };
                let status = taken.borrow_and_update().clone();
                for session in shared.sessions.lock().1.values_mut() {
                    if session.unwatched && session.followed_in(&status) {
                        session.unwatched = false;
                        session.ring();
                    }
                }
            }
        }
    }
}
