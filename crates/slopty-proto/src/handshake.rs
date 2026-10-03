//! Connection establishment.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, WorkerId};

use crate::server::WorkerCaps;
use crate::terminal::SessionSummary;

/// First message from a client.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// Stable identity of this client installation.
    pub client: ClientId,
    /// Human-readable name shown on the worker ("Cong's iPad").
    pub name: String,
}

/// Worker's acceptance.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct HelloAck {
    /// Worker identity: a UUID the worker keeps in its data directory, so a client keys
    /// everything by it and not by the address it happened to dial.
    pub worker: WorkerId,
    /// Worker name ("mac-studio").
    pub name: String,
    /// The worker's home directory (`$HOME` of its daemon), so a client writes a path under it
    /// as `~/…` knowing, not guessing from the path's shape. Empty when the daemon has none.
    pub home: String,
    /// Where its settings file is, so a client edits that machine's settings in a file tile of
    /// its own. Empty when the daemon has none.
    pub settings: String,
    /// What the worker can do and how it is doing, as the server's directory lists it; later
    /// changes come as [`WorkerMsg::Caps`](crate::WorkerMsg::Caps).
    pub caps: WorkerCaps,
    /// Its one-minute load average; later moves come as
    /// [`WorkerMsg::Load`](crate::WorkerMsg::Load).
    pub load: f32,
    /// Sessions currently alive on the worker, so the client can reattach immediately.
    pub sessions: Vec<SessionSummary>,
}
