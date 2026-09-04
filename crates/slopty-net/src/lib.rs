//! Transport between Slopty clients and hosts, on iroh (QUIC via `noq`).
//!
//! * [`endpoint`] — build a tuned iroh endpoint for either role.
//! * [`framed`] — typed, length-prefixed messages over QUIC streams.
//! * [`pairing`] — pairing tickets and the host's trust store.
//! * [`host`] — accept loop yielding authenticated client connections.
//! * [`client`] — connect to a host by ticket or by id.
//! * [`identity`] — the client's own key and its paired hosts.
//!
//! One QUIC connection carries: the control stream (bidirectional, opened by the client), one
//! unidirectional session stream per attached terminal (opened by the host), and unreliable
//! datagrams for media.

pub mod client;
pub mod endpoint;
pub mod framed;
pub mod host;
pub mod identity;
pub mod pairing;

pub use endpoint::Reach;
pub use iroh::endpoint::Connection;
pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
pub use slopty_proto::{ClientMsg, HostMsg};

/// ALPN for the Slopty protocol. The trailing number tracks [`slopty_proto::PROTOCOL_VERSION`]
/// so incompatible peers fail at the QUIC handshake instead of after it.
pub const ALPN: &[u8] = b"slopty/1";

/// Transport errors.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Binding the endpoint failed.
    #[error("bind: {0}")]
    Bind(String),
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
    /// Not paired.
    #[error("not paired")]
    NotPaired,
    /// Bad ticket.
    #[error("ticket: {0}")]
    Ticket(String),
    /// Trust store I/O.
    #[error("trust store: {0}")]
    Store(String),
}
