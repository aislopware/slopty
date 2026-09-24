//! Port tunnels: each bidirectional stream a client opens after the control stream is a TCP
//! connection it accepted, joined here to the same port on this worker's loopback.

use slopty_core::ClientId;
use slopty_net::Connection;
use slopty_net::streams::{self, RawRecv};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// Bytes moved at a time each way.
const CHUNK: usize = 64 << 10;

/// Accept the client's tunnels until the connection ends.
pub async fn accept(conn: Connection, client: ClientId) {
    loop {
        match streams::accept_tunnel(&conn).await {
            Ok((open, send, rx)) => {
                tracing::debug!(%client, port = open.port, "tunnel");
                drop(tokio::spawn(splice(open.port, send, rx)));
            }
            Err(e) => {
                if conn.close_reason().is_some() {
                    break;
                }
                tracing::debug!(%client, error = %e, "tunnel refused");
            }
        }
    }
}

/// A local server listening on `port`: IPv4 loopback first, then IPv6 (a dev server bound to
/// `localhost` on macOS is often on `::1` only).
async fn connect(port: u16) -> std::io::Result<TcpStream> {
    match TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await {
        Ok(tcp) => Ok(tcp),
        Err(_v4) => TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).await,
    }
}

/// Join the stream to `127.0.0.1:port` both ways until both directions finish. A finished
/// stream shuts the socket's write half, and the socket's EOF finishes the stream.
async fn splice(port: u16, mut send: noq::SendStream, mut rx: RawRecv) {
    let tcp = match connect(port).await {
        Ok(tcp) => tcp,
        Err(e) => {
            tracing::debug!(port, error = %e, "tunnel target refused");
            let _reset = send.reset(0_u32.into());
            rx.stop();
            return;
        }
    };
    let _nodelay = tcp.set_nodelay(true);
    let (mut from_tcp, mut to_tcp) = tcp.into_split();
    let up = async {
        while let Ok(Some(chunk)) = rx.chunk(CHUNK).await {
            if to_tcp.write_all(&chunk).await.is_err() {
                rx.stop();
                return;
            }
        }
        let _shut = to_tcp.shutdown().await;
    };
    let down = async {
        let mut buf = vec![0_u8; CHUNK];
        loop {
            match from_tcp.read(&mut buf).await {
                Ok(0) => {
                    let _finished = send.finish();
                    return;
                }
                Ok(n) => {
                    if send.write_all(buf.get(..n).unwrap_or_default()).await.is_err() {
                        return;
                    }
                }
                Err(_e) => {
                    let _reset = send.reset(0_u32.into());
                    return;
                }
            }
        }
    };
    tokio::join!(up, down);
}
