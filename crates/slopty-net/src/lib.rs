//! Transport between Slopty clients and hosts: QUIC over plain UDP (noq), in the clear.
//!
//! Hosts are reached over Tailscale, `WireGuard` or a VPN, which already encrypt and
//! authenticate; Slopty adds neither, and admits peers by source address instead.
//!
//! * [`crypto`] — the null QUIC crypto provider: parameters exchanged, nothing encrypted.
//! * [`endpoint`] — bind a tuned endpoint; read a connection's path.
//! * [`addr`] — `host[:port]`, as a person types it.
//! * [`admission`] — which source addresses a host lets in.
//! * [`framed`] — typed, length-prefixed messages over QUIC streams.
//! * [`host`] — accept loop yielding clients that said `Hello`.
//! * [`client`] — connect to a host by address.
//! * [`server`] — links to the server: its accept loop, and the dial to it.
//! * [`known`] — the client's id and the workers it has added.
//!
//! The endpoint and the crypto provider know nothing of the roles: the same plaintext QUIC
//! serves client ↔ worker and worker, client or agent ↔ server links.
//!
//! One QUIC connection carries: the control stream (bidirectional, opened by the client), one
//! unidirectional session stream per attached terminal (opened by the host), and unreliable
//! datagrams for media.

#![forbid(unsafe_code)]

pub mod addr;
pub mod admission;
pub mod client;
pub mod crypto;
pub mod endpoint;
pub mod framed;
pub mod host;
pub mod known;
pub mod server;
pub mod streams;

pub use addr::HostAddr;
pub use noq::{Connection, Endpoint};
pub use slopty_proto::{ClientMsg, HostMsg};

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
    /// The peer sent something we did not expect at this point.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// Known-workers store I/O.
    #[error("store: {0}")]
    Store(String),
}

impl NetError {
    /// A stream error with its source chain: noq's `ReadError::ConnectionLost` displays as
    /// just "connection lost", and the reason (timed out, reset, closed by peer) is the part
    /// worth logging.
    pub(crate) fn stream(e: &(dyn std::error::Error + 'static)) -> Self {
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

#[cfg(test)]
mod tests {
    use super::NetError;

    #[derive(Debug, thiserror::Error)]
    #[error("connection lost")]
    struct Lost(#[source] TimedOut);

    #[derive(Debug, thiserror::Error)]
    #[error("timed out")]
    struct TimedOut;

    #[test]
    fn stream_error_carries_its_source_chain() {
        let e = NetError::stream(&Lost(TimedOut));
        assert_eq!(e.to_string(), "stream: connection lost: timed out");
    }
}
