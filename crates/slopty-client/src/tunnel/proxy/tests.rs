//! The proxy against a real worker connection's far end, with a raw SOCKS5 client as the page.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use slopty_net::streams::accept_tunnel;
use slopty_proto::transfer::{TunnelHost, TunnelOpen, TunnelRefusal};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::Proxy;
use crate::tunnel::tests::{PATIENCE, linked};

/// A page's connection through `proxy` to `target` (address type, address, port), past the
/// greeting; the reply's code.
async fn connect(proxy: &Proxy, target: &[u8]) -> (TcpStream, u8) {
    let at = SocketAddr::from(([127, 0, 0, 1], proxy.local()));
    let mut page = TcpStream::connect(at).await.unwrap();
    page.write_all(&[5, 1, 0]).await.unwrap();
    let mut method = [0_u8; 2];
    page.read_exact(&mut method).await.unwrap();
    assert_eq!(method, [5, 0], "no authentication");
    page.write_all(&[&[5, 1, 0][..], target].concat()).await.unwrap();
    let mut reply = [0_u8; 10];
    tokio::time::timeout(PATIENCE, page.read_exact(&mut reply)).await.unwrap().unwrap();
    (page, reply[1])
}

fn name(name: &str, port: u16) -> Vec<u8> {
    let len = u8::try_from(name.len()).unwrap();
    [&[3, len][..], name.as_bytes(), &port.to_be_bytes()].concat()
}

/// A CONNECT by name is answered at once, and is a tunnel to the name for the worker's
/// resolver: the request rides it up, the answer comes back down.
#[tokio::test]
async fn a_name_is_a_tunnel_to_it_on_the_worker() {
    let (conn, worker) = linked().await;
    let proxy = Proxy::start(conn).unwrap();
    let (mut page, code) = connect(&proxy, &name("Postgres-Admin", 8080)).await;
    assert_eq!(code, 0, "answered before the worker dials");
    page.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    let (open, mut send, mut rx) = accept_tunnel(&worker).await.unwrap();
    let host = TunnelHost::Name("Postgres-Admin".to_owned());
    assert_eq!(open, TunnelOpen { host, port: 8080 });
    let request = rx.chunk(1024).await.unwrap().unwrap();
    assert_eq!(&*request, b"GET / HTTP/1.1\r\n\r\n");
    send.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    send.finish().unwrap();
    let mut answer = Vec::new();
    page.read_to_end(&mut answer).await.unwrap();
    assert_eq!(answer, b"HTTP/1.1 200 OK\r\n\r\n");
}

/// Each address type names the host the worker dials: `localhost` is its loopback, an address
/// written as a name is that address.
#[tokio::test]
async fn every_address_type_names_its_host() {
    let (conn, worker) = linked().await;
    let proxy = Proxy::start(conn).unwrap();
    let v6 = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 7);
    let port = 443_u16.to_be_bytes();
    let cases = [
        (
            [&[1][..], &[10, 0, 0, 7], &port].concat(),
            TunnelHost::Ip(Ipv4Addr::new(10, 0, 0, 7).into()),
        ),
        ([&[4][..], &v6.octets(), &port].concat(), TunnelHost::Ip(v6.into())),
        (name("LocalHost", 443), TunnelHost::Loopback),
        (name("192.168.1.9", 443), TunnelHost::Ip(Ipv4Addr::new(192, 168, 1, 9).into())),
    ];
    for (target, host) in cases {
        let (_page, code) = connect(&proxy, &target).await;
        assert_eq!(code, 0);
        let (open, _send, _rx) = accept_tunnel(&worker).await.unwrap();
        assert_eq!(open, TunnelOpen { host, port: 443 });
    }
}

/// The worker cannot resolve the name: the page's connection is reset, the proxy has kept why
/// by then for the page to say, and forgets it once the name answers.
#[tokio::test]
async fn a_refusal_resets_the_page_and_is_kept_until_it_answers() {
    let (conn, worker) = linked().await;
    let proxy = Proxy::start(conn).unwrap();
    let (mut page, _) = connect(&proxy, &name("db.internal", 5432)).await;
    page.write_all(b"hello").await.unwrap();
    let (_open, mut send, mut rx) = accept_tunnel(&worker).await.unwrap();
    send.reset(TunnelRefusal::Unresolved.code().into()).unwrap();
    rx.stop();
    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(PATIENCE, page.read(&mut byte)).await.unwrap();
    assert_eq!(read.unwrap_err().kind(), std::io::ErrorKind::ConnectionReset);
    let kept = proxy.refusal("DB.internal", 5432);
    assert_eq!(kept, Some(TunnelRefusal::Unresolved), "kept before the page heard the reset");
    assert_eq!(proxy.refusal("db.internal", 5433), None, "per port");

    let (mut page, _) = connect(&proxy, &name("db.internal", 5432)).await;
    let (_open, mut send, _rx) = accept_tunnel(&worker).await.unwrap();
    send.write_all(b"ok").await.unwrap();
    let mut two = [0_u8; 2];
    page.read_exact(&mut two).await.unwrap();
    assert_eq!(proxy.refusal("db.internal", 5432), None, "it answered");
}

/// What the proxy does not serve is answered with why: a page that asks for a password, a
/// BIND, an unknown address type.
#[tokio::test]
async fn what_it_does_not_serve_it_says_so() {
    let (conn, _worker) = linked().await;
    let proxy = Proxy::start(conn).unwrap();
    let at = SocketAddr::from(([127, 0, 0, 1], proxy.local()));

    let mut page = TcpStream::connect(at).await.unwrap();
    page.write_all(&[5, 1, 2]).await.unwrap();
    let mut method = [0_u8; 2];
    page.read_exact(&mut method).await.unwrap();
    assert_eq!(method, [5, 0xff], "no method it offers");

    let bind = [5, 2, 0, 1, 0, 0, 0, 0, 0, 80];
    let unknown_type = [5, 1, 0, 9, 0, 0, 0, 0, 0, 80];
    for (request, code) in [(bind, 7), (unknown_type, 8)] {
        let mut page = TcpStream::connect(at).await.unwrap();
        page.write_all(&[5, 1, 0]).await.unwrap();
        page.read_exact(&mut method).await.unwrap();
        page.write_all(&request).await.unwrap();
        let mut reply = [0_u8; 10];
        page.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], code, "{request:?}");
    }
}
