//! Datagram copies of a keystroke and its echo (`slopty_proto::datagram`): when one goes, and
//! whether it goes at all.
//!
//! A copy written beside its stream copy leaves in the same packet and is lost with it, so it
//! waits [`COPY_DELAY`] and goes in a packet of its own. It goes only onto an empty datagram
//! queue: behind queued video it would arrive after the stream's own retransmission, and noq
//! makes room in a full queue by dropping the oldest datagram, which would be video's.

use std::time::Duration;

use bytes::Bytes;
use noq::Connection;

use crate::endpoint::DATAGRAM_BUFFER;

/// Environment override for the copies.
///
/// `off`, or the delay after the stream copy in milliseconds (`0` sends at once, into the
/// stream copy's packet as often as not), or two delays for two copies (`2,10`).
pub const ECHO_COPY_ENV: &str = "SLOPTY_ECHO_COPY";

/// How long a copy waits after its stream copy: long enough that noq has written the stream's
/// packet, so the two are never lost together.
pub const COPY_DELAY: Duration = Duration::from_millis(2);

/// Datagram bytes that may be queued ahead of a copy: two packets' worth, a cursor update or
/// an audio packet, never a video frame.
const QUEUED_AHEAD: usize = 2 * slopty_proto::media::MAX_DATAGRAM;

/// How copies go on a connection: after a delay, perhaps a second one later, or not at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copies {
    delay: Duration,
    again: Option<Duration>,
}

impl Copies {
    /// As [`ECHO_COPY_ENV`] says; on, after [`COPY_DELAY`], unless it says `off`.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::parse(std::env::var(ECHO_COPY_ENV).ok().as_deref())
    }

    /// [`Self::from_env`] without the environment. Anything unreadable is the default.
    fn parse(raw: Option<&str>) -> Option<Self> {
        let raw = raw.map(str::trim).unwrap_or_default();
        if raw.eq_ignore_ascii_case("off") {
            return None;
        }
        let ms = |s: &str| s.trim().parse::<u64>().ok().map(Duration::from_millis);
        let parsed = match raw.split_once(',') {
            Some((first, second)) => ms(first).zip(ms(second).map(Some)),
            None => ms(raw).map(|delay| (delay, None)),
        };
        let (delay, again) = parsed.unwrap_or((COPY_DELAY, None));
        Some(Self { delay, again: again.filter(|again| *again > delay) })
    }

    /// Send `datagram` on `conn` once the delay has passed, and again at the second delay when
    /// there is one, each time if the path carries a datagram that large and nothing but a
    /// packet or two of datagrams is queued. Must be called on a tokio runtime.
    pub fn send(self, conn: &Connection, datagram: Bytes) {
        let Self { delay, again } = self;
        if delay.is_zero() && again.is_none() {
            send_now(conn, datagram);
            return;
        }
        let conn = conn.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            send_now(&conn, datagram.clone());
            if let Some(again) = again {
                tokio::time::sleep(again.saturating_sub(delay)).await;
                send_now(&conn, datagram);
            }
        });
    }
}

fn send_now(conn: &Connection, datagram: Bytes) {
    let queued = DATAGRAM_BUFFER.saturating_sub(conn.datagram_send_buffer_space());
    if !fits(datagram.len(), conn.max_datagram_size(), queued) {
        tracing::trace!(len = datagram.len(), queued, "echo copy not sent");
        return;
    }
    if let Err(e) = conn.send_datagram(datagram) {
        tracing::trace!(error = %e, "echo copy not sent");
    }
}

/// Whether a copy of `len` bytes goes: the path takes it, and at most [`QUEUED_AHEAD`] bytes of
/// datagrams wait ahead of it.
fn fits(len: usize, max: Option<usize>, queued: usize) -> bool {
    max.is_some_and(|max| len <= max) && queued <= QUEUED_AHEAD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_are_on_after_the_delay_unless_turned_off() {
        let ms = Duration::from_millis;
        let copies = |delay, again| Some(Copies { delay, again });
        assert_eq!(Copies::parse(None), copies(COPY_DELAY, None));
        assert_eq!(Copies::parse(Some("5")), copies(ms(5), None));
        assert_eq!(Copies::parse(Some("0")), copies(Duration::ZERO, None));
        assert_eq!(Copies::parse(Some("2,10")), copies(ms(2), Some(ms(10))));
        assert_eq!(Copies::parse(Some(" 2 , 10 ")), copies(ms(2), Some(ms(10))));
        assert_eq!(Copies::parse(Some("5,5")), copies(ms(5), None), "the same packet twice");
        assert_eq!(Copies::parse(Some(" OFF ")), None);
        for default in ["", "soon", "-1", "1.5", "2,x", ",3"] {
            assert_eq!(Copies::parse(Some(default)), copies(COPY_DELAY, None), "{default:?}");
        }
    }

    /// A copy never waits behind video, and never makes noq drop a queued datagram for it.
    #[test]
    fn a_copy_goes_only_onto_an_empty_queue_and_within_the_path() {
        assert!(fits(300, Some(1200), 0));
        assert!(fits(300, Some(1200), QUEUED_AHEAD), "a cursor update or two ahead");
        assert!(!fits(300, Some(1200), QUEUED_AHEAD + 1), "a video frame ahead");
        assert!(!fits(1201, Some(1200), 0), "larger than the path takes");
        assert!(!fits(10, None, 0), "the peer takes no datagrams");
    }
}
