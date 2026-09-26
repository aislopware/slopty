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
//! * [`known`] — the client's id and the workers it has added.
//! * [`redial`] — when a dropped link is dialled again.
//!
//! The endpoint and the crypto provider know nothing of the roles: the same plaintext QUIC
//! serves client ↔ worker and worker, client or agent ↔ server links.
//!
//! One QUIC connection carries: the control stream (bidirectional, opened by the client), one
//! unidirectional session stream per attached terminal (opened by the worker), and unreliable
//! datagrams for media, loss feedback and the copies of keystrokes and their echoes.

#![forbid(unsafe_code)]

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
pub mod redial;
pub mod server;
pub mod streams;
pub mod worker;

pub use addr::HostAddr;
pub use noq::{Connection, Endpoint};
pub use slopty_proto::{ClientMsg, WorkerMsg};

/// Transport errors.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Binding the endpoint failed.
    #[error("bind: {0}")]
    Bind(String),
    /// An address that does not parse.
    #[error("address: {0}")]
    Address(String),
    /// A name that does not resolve.
    #[error("resolve: {0}")]
    Resolve(String),
    /// Connecting failed.
    #[error("connect: {0}")]
    Connect(String),
    /// A QUIC stream failed.
    #[error("stream: {0}")]
    Stream(String),
    /// Framing.
    #[error(transparent)]
    Codec(#[from] slopty_proto::codec::CodecError),
    /// The peer closed the stream or connection.
    #[error("closed")]
    Closed,
    /// Something waited on the peer longer than it may.
    #[error("{0} timed out")]
    TimedOut(&'static str),
    /// The peer sent something we did not expect at this point.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// Known-workers store I/O.
    #[error("store: {0}")]
    Store(String),
    /// The worker closed the connection because the tailnet grants this device no role there
    /// ([`worker::close_code::NOT_GRANTED`]).
    #[error("not granted by the tailnet policy")]
    NotGranted,
}

impl NetError {
    /// A stream error with its source chain: noq's `ReadError::ConnectionLost` displays as
    /// just "connection lost", and the reason (timed out, reset, closed by peer) is the part
    /// worth logging.
    /// A close by the peer with [`worker::close_code::NOT_GRANTED`] anywhere in the chain is
    /// [`NetError::NotGranted`].
    pub(crate) fn stream(e: &(dyn std::error::Error + 'static)) -> Self {
        if closed_with(e) == Some(u64::from(worker::close_code::NOT_GRANTED)) {
            return Self::NotGranted;
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

/// The application close code the peer closed the connection with, found anywhere in `e`'s
/// source chain.
fn closed_with(e: &(dyn std::error::Error + 'static)) -> Option<u64> {
    let mut at = Some(e);
    while let Some(err) = at {
        if let Some(noq::ConnectionError::ApplicationClosed(close)) = err.downcast_ref() {
            return Some(close.error_code.into_inner());
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
        let close = noq::ApplicationClose {
            error_code: noq::VarInt::from_u32(code),
            reason: bytes::Bytes::from_static(b"no role"),
        };
        LostTo(noq::ConnectionError::ApplicationClosed(close))
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

    #[test]
    fn stream_error_carries_its_source_chain() {
        let e = NetError::stream(&Lost(TimedOut));
        assert_eq!(e.to_string(), "stream: connection lost: timed out");
    }
}
