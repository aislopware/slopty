//! Connection establishment.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, WorkerId};

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
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HelloAck {
    /// Worker identity: a UUID the worker keeps in its data directory, so a client keys
    /// everything by it and not by the address it happened to dial.
    pub worker: WorkerId,
    /// Worker name ("mac-studio").
    pub name: String,
    /// Sessions currently alive on the worker, so the client can reattach immediately.
    pub sessions: Vec<SessionSummary>,
}
