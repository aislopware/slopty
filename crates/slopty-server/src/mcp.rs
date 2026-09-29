//! The MCP front end: Streamable HTTP, protocol revision 2026-07-28, stateless.
//!
//! It serves the tools of [`slopty_tools::tools`] over the same [`Hub::dispatch`] as the QUIC
//! links, so a model sees the tools and answers `slopty mcp` gives.
//!
//! The listener admits TCP peers by address like the QUIC one. It does not check `Host`,
//! because a tailnet name or an IP literal is as legitimate as `localhost`; instead it refuses
//! every request carrying an `Origin`, which only a browser sends, and which is what a
//! DNS-rebinding page cannot leave out.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::model::{Implementation, ServerCapabilities};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use slopty_net::admission::{Admission, Verdict};
use slopty_proto::codec::MAX_FRAME_BYTES;
use slopty_tools::mcp::Handler;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::hub::Hub;

/// Largest request body: a `write_file` of a whole message's worth of bytes in base64.
///
/// That is [`MAX_FRAME_BYTES`] in base64 and a megabyte for the JSON around it. rmcp's 4 MiB
/// default refused over HTTP files that `slopty mcp` takes over stdio; a file too large for the
/// worker's link is refused the same way over both.
pub const MAX_BODY_BYTES: usize =
    MAX_FRAME_BYTES.div_ceil(3).saturating_mul(4).saturating_add(1_048_576);

/// The MCP tool server over the hub.
pub type Mcp = Handler<Hub>;

/// The tools over `hub`. A stateless HTTP call has no stream to send progress on.
#[must_use]
pub fn handler(hub: Hub) -> Mcp {
    let info = Implementation::new("slopty-server", env!("CARGO_PKG_VERSION"));
    Handler::new(hub, info, ServerCapabilities::builder().enable_tools().build())
}

/// Serve MCP on `listener` to the peers `admission` admits. Connections run in a set this
/// future owns, so dropping it ends them all.
pub async fn serve(listener: TcpListener, admission: Admission, hub: Hub) {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .disable_allowed_hosts()
        .enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || Ok(handler(hub.clone())),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let mut connections = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            Some(_done) = connections.join_next() => continue,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(e) => {
                tracing::warn!(error = %e, "mcp accept failed");
                continue;
            }
        };
        let peer = slopty_net::endpoint::canonical(peer);
        let (service, admission) = (service.clone(), admission.clone());
        connections.spawn(async move {
            // An agent's surface: the tailnet must grant the node the agent role.
            match admission.check(peer).await {
                Verdict::Admit(grant) if grant.allows(slopty_tailnet::Role::Agent) => {}
                Verdict::Admit(_) => {
                    tracing::info!(%peer, "mcp refused: the tailnet grants no agent role");
                    return;
                }
                Verdict::Refuse(why) => {
                    tracing::info!(%peer, why, "mcp refused");
                    return;
                }
            }
            let serve = service_fn(move |request| {
                let service = service.clone();
                async move { Ok::<_, Infallible>(service.handle(request).await) }
            });
            if let Err(e) =
                http1::Builder::new().serve_connection(TokioIo::new(stream), serve).await
            {
                tracing::debug!(%peer, error = %e, "mcp connection ended");
            }
        });
    }
}

/// Bind the MCP listener on `local`.
///
/// An unspecified IPv6 address is bound dual-stack, so one socket answers loopback, the tailnet
/// and the LAN in both families, including interfaces that come up after the server does.
///
/// XNU lets `[::]:p` bind beside an IPv4 socket on `0.0.0.0:p`, which then takes every IPv4
/// connection, so a fixed port is first bound on `0.0.0.0` alone: held there, it is in use, as
/// on Linux. A free port (0) needs no check, since TCP draws it clear of IPv4 sockets.
pub fn bind(local: SocketAddr) -> std::io::Result<TcpListener> {
    if local.ip().is_unspecified() && local.is_ipv6() && local.port() != 0 {
        drop(tcp_socket(SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, local.port())))?);
    }
    let socket = tcp_socket(local)?;
    socket.listen(128)?;
    TcpListener::from_std(socket.into())
}

/// A non-blocking TCP socket bound on `local`, dual-stack when it is IPv6, reusing an address
/// that closed connections still hold in `TIME_WAIT`.
fn tcp_socket(local: SocketAddr) -> std::io::Result<socket2::Socket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(local),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if local.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&local.into())?;
    Ok(socket)
}
