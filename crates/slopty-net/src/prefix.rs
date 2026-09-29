//! The wire prefix both ends open the control stream with, and the check of the peer's
//! (`slopty_proto::wire`, `docs/decisions/transport.md`, "Each end says its wire first").
//!
//! Each end writes this build's prefix before its first message and reads the peer's before
//! the peer's first message. The end that finds another fingerprint closes the connection with
//! [`close_code::WRONG_BUILD`] and its own build as the reason. The other end reads that close
//! as [`NetError::WrongBuild`] too, whichever it sees first: the prefix or the close.

use std::time::Duration;

use noq::Connection;
use serde::Serialize;
use serde::de::DeserializeOwned;
use slopty_proto::wire::{BUILD, Prefix};

use crate::framed::{FramedRecv, FramedSend};
use crate::worker::close_code;
use crate::{NetError, WrongBuild};

/// Write this build's prefix: the first bytes on `tx`.
pub(crate) async fn say<T: Serialize>(tx: &mut FramedSend<T>) -> Result<(), NetError> {
    tx.send_raw(Prefix::this().encode()).await
}

/// Read the peer's prefix off `rx` within `wait`; one that is not this build's closes `conn`
/// with [`close_code::WRONG_BUILD`]. A stream that opens with no prefix at all is a build
/// older than the prefix, and closes the same way.
pub(crate) async fn check<T: DeserializeOwned>(
    conn: &Connection,
    rx: &mut FramedRecv<T>,
    wait: Duration,
) -> Result<Prefix, NetError> {
    let read = tokio::time::timeout(wait, rx.prefix())
        .await
        .map_err(|_elapsed| NetError::Protocol("no wire prefix"))??;
    let peer = match read {
        Ok(prefix) if prefix.is_this_wire() => return Ok(prefix),
        Ok(prefix) => prefix.build,
        Err(_not_slopty) => String::new(),
    };
    conn.close(close_code::WRONG_BUILD.into(), BUILD.as_bytes());
    Err(WrongBuild { peer }.into())
}
