//! A phone or iPad finds the server from a Mac that knows it: the Mac's Settings → Devices shows
//! a code on the server's row ("Connect a phone"), the device's own Camera reads it, and Slopty
//! opens on the device with the address filled in.
//!
//! The code is a link, `slopty://connect?server=<host:port>`, which iOS hands to Slopty through
//! its URL scheme, so Slopty asks for no camera. It opens Settings → Devices' Add device on "A
//! server" with the address in its field and connects nothing: the person presses Connect, since
//! any page or app may open such a link.
//!
//! The address is the one the device reaches: the server's own, or, for a server on this Mac
//! (which this app reaches over loopback), this Mac's name on the tailnet.

use std::cell::RefCell;

use gpui::{AnyWindowHandle, App};
use slopty_net::HostAddr;
use slopty_view_proto::ServerInvite;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// The link's scheme and what it asks.
const PREFIX: &str = "slopty://connect?";
/// The query key that holds the server's address.
const SERVER_KEY: &str = "server";

/// What the dialog says under the code.
const HOW: &str =
    "Point the Camera of your iPhone or iPad at the code, then press Connect in Slopty.";
/// Why there is no code: the server is on this Mac, which no tailnet names.
const NO_NAME: &str =
    "This Mac is on no tailnet, so your devices need its address on your VPN: type it there.";

/// The link that opens Slopty on a device at Add device with `server` filled in.
pub(crate) fn link(server: &HostAddr) -> String {
    let mut out = String::from(PREFIX);
    out.push_str(SERVER_KEY);
    out.push('=');
    for c in server.to_string().chars() {
        match c {
            '[' => out.push_str("%5B"),
            ']' => out.push_str("%5D"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

/// The address a device reaches `server` by: its own, or for a server on this Mac, this Mac's
/// tailnet `name`, when one names it.
fn reached(server: &HostAddr, name: Option<&str>) -> Result<HostAddr, &'static str> {
    if !loopback(server.host()) {
        return Ok(server.clone());
    }
    name.filter(|n| !n.is_empty()).map(|name| HostAddr::new(name, server.port())).ok_or(NO_NAME)
}

/// Whether `host` is this machine's own loopback.
pub(crate) fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The code for `server` a device scans, given this Mac's tailnet `name`: its link and address,
/// or why there is none.
pub(crate) fn invite(server: &HostAddr, name: Option<&str>) -> ServerInvite {
    match reached(server, name) {
        Ok(address) => ServerInvite {
            link: Some(link(&address)),
            address: Some(address.to_string()),
            note: HOW.to_owned(),
        },
        Err(why) => ServerInvite { link: None, address: None, note: why.to_owned() },
    }
}

/// The server a link names: only the exact form `slopty://connect?server=<host:port>`
/// with a host name or IP address and a port. Anything else names none, logged at debug, since
/// a link is anybody's to open.
pub(crate) fn server_in(url: &str) -> Option<HostAddr> {
    let server = strict(url);
    if server.is_none() {
        tracing::debug!(url, "not a link to a server");
    }
    server
}

fn strict(url: &str) -> Option<HostAddr> {
    let value = url.strip_prefix(PREFIX)?.strip_prefix(SERVER_KEY)?.strip_prefix('=')?;
    let text = decode(value)?;
    if text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    // No default port, so one the link leaves out reads as 0 and is refused.
    let server = HostAddr::parse_with_port(&text, 0).ok()?;
    (server.port() != 0 && host_name(server.host())).then_some(server)
}

/// Whether `host` is an IP address or a DNS name: dot-separated labels of letters, digits and
/// inner hyphens, each at most 63 bytes, at most 253 in all.
fn host_name(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
        || (host.len() <= 253
            && host.split('.').all(|label| {
                (1..=63).contains(&label.len())
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
}

/// `text` with its `%XX` escapes decoded; `None` when one is broken.
fn decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.bytes();
    while let Some(b) = rest.next() {
        if b == b'%' {
            let hex = [rest.next()?, rest.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).ok()
}

/// Links the system opened Slopty with, held until the workspace listens.
enum Inbox {
    Held(Vec<String>),
    Listening(UnboundedSender<String>),
}

thread_local! {
    /// The main thread's: UIKit calls the scene's delegate there, and GPUI runs there.
    static INBOX: RefCell<Inbox> = const { RefCell::new(Inbox::Held(Vec::new())) };
}

/// Hand the workspace a link the system opened Slopty with. On the main thread.
///
/// It comes from `scene:openURLContexts:`, or from the connection options of a launch it
/// caused. The workspace keeps only `slopty://connect?server=<host:port>`.
pub fn open_link(url: String) {
    tracing::debug!(url, "a link opened Slopty");
    INBOX.with_borrow_mut(|inbox| match inbox {
        Inbox::Held(held) => held.push(url),
        Inbox::Listening(listener) => {
            if listener.send(url).is_err() {
                tracing::debug!("a link with no workspace to open it");
            }
        }
    });
}

/// Every link from now on, those held until now first.
pub(crate) fn links() -> UnboundedReceiver<String> {
    let (listener, links) = tokio::sync::mpsc::unbounded_channel();
    INBOX.with_borrow_mut(|inbox| {
        if let Inbox::Held(held) = inbox {
            for url in held.drain(..) {
                // Cannot fail: the receiver is in hand.
                let _sent = listener.send(url);
            }
        }
        *inbox = Inbox::Listening(listener);
    });
    links
}

/// A link opened Slopty: one naming a server opens Settings → Devices' Add device on "A server"
/// with its address in the field, in `shell`'s window, and connecting stays the person's press.
/// One to a conversation (the shell's Copy link) opens that chat. Any other is ignored.
pub(crate) fn open_in(url: &str, shell: AnyWindowHandle, cx: &mut App) {
    if slopty_shell::open_conversation_link(url, cx) {
        return;
    }
    let Some(server) = server_in(url) else { return };
    tracing::debug!(%server, "a link names a server");
    let Some(shell) = shell.downcast::<slopty_shell::shell::Shell>() else { return };
    let address = server.to_string();
    if shell.update(cx, |shell, _, cx| shell.open_add_device_with_server(address, cx)).is_err() {
        tracing::debug!("the shell's window is gone");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link names the server, an IPv6 one in brackets, and reads back as it.
    #[test]
    fn a_link_carries_the_server_and_reads_back() {
        let named = HostAddr::new("studio.tail1234.ts.net", 45_560);
        assert_eq!(link(&named), "slopty://connect?server=studio.tail1234.ts.net:45560");
        let v6 = HostAddr::new("fd7a:115c:a1e0::1", 45_600);
        assert_eq!(link(&v6), "slopty://connect?server=%5Bfd7a:115c:a1e0::1%5D:45600");
        for server in [named, v6, HostAddr::new("100.64.0.3", 1), HostAddr::new("::1", 65_535)] {
            assert_eq!(server_in(&link(&server)), Some(server.clone()), "{server}");
        }
    }

    /// A server elsewhere is reached as this app reaches it; one on this Mac by this Mac's
    /// tailnet name, and with no name, not at all, which the invite says.
    #[test]
    fn a_device_reaches_the_server_as_it_can() {
        let studio = HostAddr::new("studio.local", 45_560);
        assert_eq!(reached(&studio, None), Ok(studio.clone()));
        let here = HostAddr::new("127.0.0.1", 45_560);
        let named = reached(&here, Some("mac-studio.tail1234.ts.net"));
        assert_eq!(named, Ok(HostAddr::new("mac-studio.tail1234.ts.net", 45_560)));
        assert_eq!(reached(&HostAddr::new("::1", 1), Some("m")), Ok(HostAddr::new("m", 1)));
        assert_eq!(reached(&HostAddr::new("localhost", 1), None), Err(NO_NAME));
        assert_eq!(reached(&here, Some("")), Err(NO_NAME));
        let shown = invite(&here, Some("m"));
        assert_eq!(shown.link.as_deref(), Some("slopty://connect?server=m:45560"));
        assert_eq!(shown.address.as_deref(), Some("m:45560"));
        let none = invite(&here, None);
        assert_eq!((none.link, none.note.as_str()), (None, NO_NAME));
    }

    /// Only the exact form names a server: no other scheme, path, key, extra pair or fragment,
    /// no missing or bad port, and no host that is not a name or an address.
    #[test]
    fn any_other_link_names_no_server() {
        for other in [
            "",
            "https://example.com/?server=a:1",
            "SLOPTY://connect?server=a:1",
            "slopty://open?server=a:1",
            "slopty://connect/?server=a:1",
            "slopty://connect?from=mac&server=a:1",
            "slopty://connect?server=a:1&from=mac",
            "slopty://connect?server=a:1#x",
            "slopty://connect?servers=a:1",
            "slopty://connect?server",
            "slopty://connect?server=",
            "slopty://connect?server=home-server",
            "slopty://connect?server=%5B::1%5D",
            "slopty://connect?server=a:0",
            "slopty://connect?server=a:65536",
            "slopty://connect?server=a:x",
            "slopty://connect?server=%zz:1",
            "slopty://connect?server=%5",
            "slopty://connect?server=a%20b:1",
            "slopty://connect?server=%20a:1",
            "slopty://connect?server=a%0A:1",
            "slopty://connect?server=-a:1",
            "slopty://connect?server=a..b:1",
            "slopty://connect?server=a_b:1",
            "slopty://connect?server=user@a:1",
            "slopty://connect?server=a/b:1",
            "slopty://connect?server=%5Bnot:v6%5D:1",
        ] {
            assert_eq!(server_in(other), None, "{other:?}");
        }
    }
}
