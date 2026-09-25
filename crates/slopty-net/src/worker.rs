//! Worker side: admit, accept, read the `Hello`, hand over the control stream.

use std::net::SocketAddr;
use std::sync::Arc;

use noq::{Connection, Endpoint};
use slopty_proto::handshake::Hello;
use slopty_proto::{ClientMsg, WorkerMsg};
use tokio::sync::{Mutex, mpsc};

use crate::NetError;
use crate::admission::Admission;
use crate::framed::{FramedRecv, FramedSend};
use crate::listen::Greeted;

/// QUIC close codes.
pub mod close_code {
    /// Normal shutdown.
    pub const NORMAL: u32 = 0;
    /// Protocol error.
    pub const PROTOCOL: u32 = 2;
}

/// The worker's listening endpoint and who it lets in.
#[derive(Debug, Clone)]
pub struct WorkerListener {
    endpoint: Endpoint,
    admission: Admission,
    greeted: Arc<Mutex<mpsc::Receiver<Greeted<WorkerMsg, ClientMsg, Hello>>>>,
}

/// A client that said `Hello`. The caller answers with a `HelloAck`.
#[derive(Debug)]
pub struct AcceptedClient {
    /// The QUIC connection (for session streams and datagrams).
    pub conn: Connection,
    /// Where it connected from.
    pub remote: SocketAddr,
    /// Its hello.
    pub hello: Hello,
    /// Control stream, worker → client.
    pub tx: FramedSend<WorkerMsg>,
    /// Control stream, client → worker.
    pub rx: FramedRecv<ClientMsg>,
}

impl WorkerListener {
    /// Listen on `local` (see [`crate::endpoint::bind`]), letting in whom `admission` admits.
    /// Must be called on a tokio runtime: the accept loop runs as a task of its own.
    pub fn bind(local: SocketAddr, admission: Admission) -> Result<Self, NetError> {
        let endpoint = crate::endpoint::bind(local, true)?;
        let hello = |first| match first {
            ClientMsg::Hello(hello) => Some(hello),
            _ => None,
        };
        let rx = crate::listen::spawn(endpoint.clone(), admission.clone(), hello, "client");
        Ok(Self { endpoint, admission, greeted: Arc::new(Mutex::new(rx)) })
    }

    /// The endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Where it listens.
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.endpoint.local_addr().map_err(|e| NetError::Bind(e.to_string()))
    }

    /// Who it lets in.
    #[must_use]
    pub const fn admission(&self) -> &Admission {
        &self.admission
    }

    /// The next client that said `Hello`; `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<AcceptedClient> {
        let Greeted { conn, remote, hello, tx, rx } = self.greeted.lock().await.recv().await?;
        Some(AcceptedClient { conn, remote, hello, tx, rx })
    }
}
