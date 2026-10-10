//! Transport between Slopty clients and workers: QUIC over plain UDP (noq), in the clear.
//!
//! Workers are reached over Tailscale, `WireGuard` or a VPN, which already encrypt and
//! authenticate; Slopty adds neither, and admits peers by source address instead.
//!
//! * [`crypto`] — the null QUIC crypto provider: parameters exchanged, nothing encrypted.
//! * [`endpoint`] — bind a tuned endpoint; read a connection's path.
//! * [`echo`] — when a keystroke's and an echo's datagram copies go.
//! * [`addr`] — `host[:port]`, as a person types it.
//! * [`admission`] — which source addresses a worker lets in.
//! * [`framed`] — typed, length-prefixed messages over QUIC streams.
//! * [`worker`] — accept loop yielding clients that said `Hello`.
//! * [`client`] — connect to a worker by address.
//! * [`server`] — links to the server: its accept loop, and the dial to it.
//! * [`known`] — the client's installation id.
//! * [`redial`] — when a dropped link is dialled again.
//! * `udp` — the UDP socket under every endpoint, which finishes a send that went out in part.
//! * `prefix` — the wire prefix each end opens the control stream with, and the check of the peer's
//!   ([`slopty_proto::wire`]).
//!
//! The endpoint and the crypto provider know nothing of the roles: the same plaintext QUIC
//! serves client ↔ worker and worker, client or agent ↔ server links.
//!
//! One QUIC connection carries: the control stream (bidirectional, opened by the client), one
//! unidirectional session stream per attached terminal (opened by the worker), and unreliable
//! datagrams for media, loss feedback and the copies of keystrokes and their echoes.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod addr;
pub mod admission;
pub mod client;
pub mod congestion;
pub mod crypto;
pub mod discover;
pub mod echo;
pub mod endpoint;
pub mod framed;
pub mod known;
mod listen;
mod prefix;
pub mod redial;
pub mod server;
pub mod streams;
mod udp;
pub mod worker;

pub use addr::HostAddr;
pub use noq::{Connection, Endpoint, SendStream};
pub use slopty_proto::{ClientMsg, WorkerMsg};

/// Transport errors.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Binding the endpoint failed.
    #[error("bind {addr}: {source}")]
    Bind {
        /// The address it was bound to.
        addr: String,
        /// The OS's error.
        #[source]
        source: std::io::Error,
    },
    /// Reading or writing a file failed: a transfer's, a save's or the client's id.
    #[error("{context}: {source}")]
    Io {
        /// What was read or written.
        context: String,
        /// The OS's error.
        #[source]
        source: std::io::Error,
    },
    /// An address that does not parse.
    #[error("address: {0}")]
    Address(String),
    /// A name that does not resolve.
    #[error("resolve: {0}")]
    Resolve(String),
    /// Connecting failed, for a reason none of the variants below names.
    #[error("connect: {0}")]
    Connect(String),
    /// The address did not answer the handshake in time: the machine is asleep, off the
    /// network, or nothing listens there.
    #[error("{0}: no answer")]
    NoAnswer(String),
    /// The address turned this device away before the handshake (QUIC's `CONNECTION_REFUSED`):
    /// its admitted ranges, a worker's `[network] allow`, do not hold this device's address.
    #[error("{0}: turned this device away")]
    Refused(String),
    /// A QUIC stream failed.
    #[error("stream: {0}")]
    Stream(String),
    /// Framing.
    #[error(transparent)]
    Codec(#[from] slopty_proto::codec::CodecError),
    /// The peer closed the stream or connection.
    #[error("closed")]
    Closed,
    /// The peer reset the stream with this code: what a tunnel's worker says when it cannot
    /// reach the target (`slopty_proto::transfer::TunnelRefusal`).
    #[error("stream reset by the peer: code {0}")]
    Reset(u64),
    /// Something waited on the peer longer than it may.
    #[error("{0} timed out")]
    TimedOut(&'static str),
    /// The peer sent something we did not expect at this point.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// The client's id file does not parse, or would not serialize.
    #[error("store: {0}")]
    Store(#[source] serde_json::Error),
    /// The worker closed the connection because the tailnet grants this device no role there
    /// ([`worker::close_code::NOT_GRANTED`]).
    #[error("not granted by the tailnet policy")]
    NotGranted,
    /// The peer speaks another wire: it runs a different build
    /// ([`worker::close_code::WRONG_BUILD`]).
    #[error(transparent)]
    WrongBuild(#[from] WrongBuild),
}

/// A peer that runs a different build, whose messages this one cannot read. Found in the
/// wire prefix before any message (`slopty_proto::wire`), so the link is never redialled
/// into a decode error.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WrongBuild {
    /// The peer's build as it said it; empty for a build older than the prefix, which says
    /// none.
    pub peer: String,
}

impl WrongBuild {
    /// The peer's build as a person reads it.
    #[must_use]
    pub fn peer_build(&self) -> &str {
        if self.peer.is_empty() { "an older one" } else { &self.peer }
    }
}

impl std::fmt::Display for WrongBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (peer, this) = (self.peer_build(), slopty_proto::wire::this_build());
        write!(f, "the peer runs a different build ({peer}); this is {this}")
    }
}

impl std::error::Error for WrongBuild {}

pub use slopty_proto::wire::Newer;

impl WrongBuild {
    /// Which build is the newer ([`slopty_proto::wire::newer`]): by version, then by commit,
    /// else by when each one's wire last changed. A build that says none is older than any.
    #[must_use]
    pub fn newer(&self) -> Option<Newer> {
        slopty_proto::wire::newer(&slopty_proto::wire::this_build(), &self.peer)
    }
}

/// Why a peer was not reached, in the terms a person is told it: what is said, and what can be
/// done about it, differ for each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unreached {
    /// Its name does not resolve, or its address does not parse.
    NoSuchHost,
    /// Nothing answered ([`NetError::NoAnswer`]).
    NoAnswer,
    /// It answered and turned this device away by its address ([`NetError::Refused`]).
    Refused,
    /// The tailnet grants this device no role there ([`NetError::NotGranted`]).
    NotGranted,
    /// It runs another build ([`NetError::WrongBuild`]).
    WrongBuild,
    /// The link was made and then ended: closed, reset or lost.
    Dropped,
}

impl NetError {
    /// Why this failure left the peer unreached, when it is about the peer at all: a local
    /// failure (binding, a file, the codec, a protocol slip) is `None`.
    #[must_use]
    pub const fn unreached(&self) -> Option<Unreached> {
        Some(match self {
            Self::Address(_) | Self::Resolve(_) => Unreached::NoSuchHost,
            Self::NoAnswer(_) | Self::TimedOut(_) => Unreached::NoAnswer,
            Self::Refused(_) => Unreached::Refused,
            Self::NotGranted => Unreached::NotGranted,
            Self::WrongBuild(_) => Unreached::WrongBuild,
            Self::Connect(_) | Self::Stream(_) | Self::Closed | Self::Reset(_) => {
                Unreached::Dropped
            }
            Self::Bind { .. }
            | Self::Io { .. }
            | Self::Codec(_)
            | Self::Protocol(_)
            | Self::Store(_) => return None,
        })
    }

    /// An I/O error on `context` (a path, or what was being done).
    #[must_use]
    pub fn io(context: impl std::fmt::Display, source: std::io::Error) -> Self {
        Self::Io { context: context.to_string(), source }
    }

    /// A stream error with its source chain: noq's `ReadError::ConnectionLost` displays as
    /// just "connection lost", and the reason (timed out, reset, closed by peer) is the part
    /// worth logging.
    /// A close by the peer with [`worker::close_code::NOT_GRANTED`] anywhere in the chain is
    /// [`NetError::NotGranted`], and one with [`worker::close_code::WRONG_BUILD`] is
    /// [`NetError::WrongBuild`], with the build its reason names. A stream the peer reset is
    /// [`NetError::Reset`], with the code it gave.
    #[must_use]
    pub fn stream(e: &(dyn std::error::Error + 'static)) -> Self {
        if let Some(noq::ReadError::Reset(code)) = e.downcast_ref() {
            return Self::Reset(code.into_inner());
        }
        if let Some(close) = closed_with(e) {
            let code = close.error_code.into_inner();
            if code == u64::from(worker::close_code::NOT_GRANTED) {
                return Self::NotGranted;
            }
            if code == u64::from(worker::close_code::WRONG_BUILD) {
                let peer = String::from_utf8_lossy(&close.reason).into_owned();
                return Self::WrongBuild(WrongBuild { peer });
            }
        }
        let mut text = e.to_string();
        let mut source = e.source();
        while let Some(s) = source {
            let part = s.to_string();
            if !text.contains(&part) {
                text.push_str(": ");
                text.push_str(&part);
            }
            source = s.source();
        }
        Self::Stream(text)
    }
}

/// How the peer closed the connection, found anywhere in `e`'s source chain.
fn closed_with<'e>(e: &'e (dyn std::error::Error + 'static)) -> Option<&'e noq::ApplicationClose> {
    let mut at = Some(e);
    while let Some(err) = at {
        if let Some(noq::ConnectionError::ApplicationClosed(close)) = err.downcast_ref() {
            return Some(close);
        }
        at = err.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::NetError;

    #[derive(Debug, thiserror::Error)]
    #[error("connection lost")]
    struct Lost(#[source] TimedOut);

    #[derive(Debug, thiserror::Error)]
    #[error("timed out")]
    struct TimedOut;

    #[derive(Debug, thiserror::Error)]
    #[error("connection lost")]
    struct LostTo(#[source] noq::ConnectionError);

    fn closed(code: u32) -> LostTo {
        closed_for(code, b"no role")
    }

    fn closed_for(code: u32, reason: &'static [u8]) -> LostTo {
        let close = noq::ApplicationClose {
            error_code: noq::VarInt::from_u32(code),
            reason: bytes::Bytes::from_static(reason),
        };
        LostTo(noq::ConnectionError::ApplicationClosed(close))
    }

    /// A `WRONG_BUILD` close is its own error, naming the build its reason carries.
    #[test]
    fn a_wrong_build_close_names_the_peers_build() {
        let e = NetError::stream(&closed_for(4, b"0.1.0+wire.0badf00d"));
        let NetError::WrongBuild(wrong) = &e else { panic!("{e:?}") };
        assert_eq!(wrong.peer, "0.1.0+wire.0badf00d");
        assert!(e.to_string().contains("different build (0.1.0+wire.0badf00d)"), "{e}");
        let older = super::WrongBuild { peer: String::new() };
        assert!(older.to_string().contains("different build (an older one)"), "{older}");
    }

    /// A worker's `NOT_GRANTED` close reads as its own error wherever it sits in the chain;
    /// any other close is a stream error with its text.
    #[test]
    fn a_not_granted_close_is_its_own_error() {
        assert!(matches!(NetError::stream(&closed(3)), NetError::NotGranted));
        assert!(matches!(NetError::stream(&closed(3).0), NetError::NotGranted));
        let normal = NetError::stream(&closed(0));
        assert!(
            matches!(&normal, NetError::Stream(text) if text.contains("closed by peer")),
            "{normal}"
        );
    }

    /// A reset stream keeps its code, which is how a tunnel says why it went nowhere.
    #[test]
    fn a_reset_keeps_its_code() {
        let reset = noq::ReadError::Reset(noq::VarInt::from_u32(2));
        assert!(matches!(NetError::stream(&reset), NetError::Reset(2)));
    }

    /// Clippy reads only the nearest `clippy.toml`, so this crate's copies the workspace's and
    /// adds the clock: the two must not drift apart.
    #[test]
    fn clippy_config_is_the_workspaces_plus_the_clock() {
        let root = include_str!("../../../clippy.toml");
        let ours = include_str!("../clippy.toml");
        let clock = ["std::time::Instant::now", "std::time::Instant::elapsed"];
        for path in clock {
            assert!(ours.contains(&format!("path = \"{path}\"")), "{path} is not disallowed");
        }
        let rest = ours
            .lines()
            .skip_while(|line| line.starts_with('#') || line.is_empty())
            .filter(|line| !clock.iter().any(|path| line.contains(path)))
            .fold(String::new(), |mut rest, line| {
                rest.push_str(line);
                rest.push('\n');
                rest
            });
        assert_eq!(rest, root, "this crate's clippy.toml is the root's plus the clock entries");
    }

    #[test]
    fn stream_error_carries_its_source_chain() {
        let e = NetError::stream(&Lost(TimedOut));
        assert_eq!(e.to_string(), "stream: connection lost: timed out");
    }
}
