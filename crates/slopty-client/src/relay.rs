//! Whether a worker's link has stayed on a Tailscale DERP relay long enough to say so.
//!
//! The worker tells each client its path as it changes (`WorkerMsg::Path`). A path starts on
//! DERP while a direct one is found, and a direct path whose pongs stopped reads as DERP for a
//! few seconds, so a DERP path is said only once it has held for
//! [`DERP_NOTICE_AFTER`]
//! (`docs/decisions/transport.md`, "A link that stays on DERP says so"). Pure: the caller feeds
//! it each path and asks with its own clock.

use std::time::Instant;

use slopty_proto::tailnet::{DERP_NOTICE_AFTER, LinkPath};

/// One link's path, and since when it has been on DERP.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RelayWatch {
    path: Option<LinkPath>,
    derp_since: Option<Instant>,
}

/// What to show for a link that stays on DERP.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RelayNotice {
    /// "Relayed via fra — adds latency".
    pub note: String,
    /// The one-line fix ([`LinkPath::relay_fix`]).
    pub fix: &'static str,
    /// Since when the link has been on DERP.
    pub since: Instant,
}

impl RelayWatch {
    /// The worker said the link's path is `path`, heard `now`.
    pub fn observe(&mut self, path: LinkPath, now: Instant) {
        if !path.relayed() {
            self.derp_since = None;
        } else if self.derp_since.is_none() {
            self.derp_since = Some(now);
        }
        self.path = Some(path);
    }

    /// The link is gone: whatever comes next starts afresh.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The path last heard.
    #[must_use]
    pub const fn path(&self) -> Option<&LinkPath> {
        self.path.as_ref()
    }

    /// What to show `now`: something once the link has been on DERP for
    /// [`DERP_NOTICE_AFTER`], nothing before or on any other path.
    #[must_use]
    pub fn notice(&self, now: Instant) -> Option<RelayNotice> {
        let since = self.derp_since?;
        if now.saturating_duration_since(since) < DERP_NOTICE_AFTER {
            return None;
        }
        let note = self.path.as_ref()?.relay_note()?;
        Some(RelayNotice { note, fix: LinkPath::relay_fix(), since })
    }

    /// When [`Self::notice`] next changes on its own, for a UI that redraws on a timer: the
    /// moment a DERP path has held long enough, `None` when nothing is pending.
    #[must_use]
    pub fn due(&self, now: Instant) -> Option<Instant> {
        let at = self.derp_since?.checked_add(DERP_NOTICE_AFTER)?;
        (at > now).then_some(at)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn derp() -> LinkPath {
        LinkPath::Derp { region: "fra".to_owned() }
    }

    /// DERP is said after ten seconds on it, and not for a DERP start that turns direct.
    #[test]
    fn derp_is_said_once_it_has_held() {
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let mut watch = RelayWatch::default();
        watch.observe(derp(), at(0));
        assert_eq!(watch.notice(at(9)), None, "a path is still settling");
        assert_eq!(watch.due(at(9)), Some(at(10)));
        watch.observe(derp(), at(5));
        let notice = watch.notice(at(10)).unwrap();
        assert_eq!(notice.note, "Relayed via fra — adds latency");
        assert_eq!(notice.since, at(0), "a repeat of DERP does not restart the clock");
        assert_eq!(watch.due(at(10)), None);

        watch.observe(LinkPath::Direct, at(12));
        assert_eq!(watch.notice(at(30)), None);
        assert_eq!(watch.path(), Some(&LinkPath::Direct));
        watch.observe(derp(), at(31));
        assert_eq!(watch.notice(at(40)), None, "back on DERP starts the clock again");
        assert!(watch.notice(at(41)).is_some());
        watch.reset();
        assert_eq!(watch.notice(at(60)), None);
    }

    /// A peer relay is no detour worth a word.
    #[test]
    fn a_peer_relay_says_nothing() {
        let t0 = Instant::now();
        let mut watch = RelayWatch::default();
        watch.observe(LinkPath::PeerRelay, t0);
        assert_eq!(watch.notice(t0 + Duration::from_mins(1)), None);
        assert_eq!(watch.due(t0), None);
    }
}
