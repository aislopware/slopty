//! What a program in a worker's shell hands to the client in front of the person: a web page to
//! open, a file to edit.
//!
//! A shell on the worker gets `BROWSER` and `EDITOR` naming the `slopty` CLI, and an `open` that
//! forwards web addresses (`slopty_pty::shell_integration`). The CLI asks the worker over its
//! control socket (`ctl::CtlRequest::Open`, `ctl::CtlRequest::Edit`). The worker asks the clients
//! that said they take handoffs ([`HandoffCaps`]), the one in front of the shell first, with a
//! [`HandoffEvent`], and each answers with a [`HandoffReply`] (`docs/decisions/terminal.md`, "A
//! shell's browser and editor are the client's").

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs};
use url::{Host, Url};

/// The worker's number for one handoff. It starts from a random point each run, so a client
/// never takes a new handoff for one of a worker run before.
pub type HandoffId = u64;

/// The longest address handed over; a longer one is refused.
pub const URL_MAX: usize = 8 * 1024;

/// What a client takes, said once after its hello (`ClientMsg::HandoffCaps`) and again when it
/// changes. A client that never says takes nothing, and the worker asks only those that do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct HandoffCaps {
    /// It opens web pages, or offers them.
    pub open: bool,
    /// It shows a file in a tile and says when the person is done with it.
    pub edit: bool,
}

/// A web page a client may open for a program on the worker.
///
/// Its address is read as a browser reads it (the WHATWG URL standard, the `url` crate), with
/// the host that address really goes to, and why the person should see it before it opens, if
/// anything.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Page {
    /// The address, serialised as parsed. This is what is opened, so the host shown is the
    /// host visited.
    pub url: String,
    /// The host, ASCII (an internationalised name in its `xn--` form).
    pub host: String,
    /// Why it is only ever offered, never opened unasked.
    pub wary: Option<Wary>,
}

/// Why an address is offered to the person rather than opened, whoever asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Wary {
    /// A name or password before the host (`https://github.com@evil.test/`): the host is not
    /// the first name the eye reads.
    UserInfo,
    /// An internationalised name: shown in its `xn--` form, since its letters can pass for
    /// another name's.
    Idn,
    /// This machine: `localhost`, `127.0.0.0/8`, `::1`, `0.0.0.0`. On the client that is the
    /// client's own machine, not the worker.
    Loopback,
    /// A private network: `10/8`, `172.16/12`, `192.168/16`, `100.64/10` (Tailscale),
    /// `fc00::/7`, or a `.ts.net` name. A page on the person's own network, reached from a
    /// browser that is signed in to it.
    Private,
    /// A link-local address: `169.254/16`, `fe80::/10`.
    LinkLocal,
    /// A name only a local network resolves: one label (`intranet`), `.local`, `.lan`,
    /// `.home.arpa`, `.internal`.
    LocalName,
}

impl std::fmt::Display for Wary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UserInfo => "the address carries a name before its host",
            Self::Idn => "the host is an internationalised name",
            Self::Loopback => "the address is this machine's own",
            Self::Private => "the address is on a private network",
            Self::LinkLocal => "the address is link-local",
            Self::LocalName => "the host is a local network name",
        })
    }
}

/// `url` as a page a client may open, or `None`.
///
/// It must be `http` or `https` with a host, at most [`URL_MAX`] bytes, with no whitespace or
/// control character anywhere (a browser would drop some of them and read another address).
///
/// Both ends check it. Any other scheme is refused: `file:` names the wrong machine, and a
/// custom scheme (`vscode:`, `zoommtg:`, `x-apple.systempreferences:`) or `mailto:` would let a
/// program on the worker start an application on the person's own machine.
#[must_use]
pub fn page(url: &str) -> Option<Page> {
    if url.len() > URL_MAX || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host()?;
    let wary = if !parsed.username().is_empty() || parsed.password().is_some() {
        Some(Wary::UserInfo)
    } else {
        wary_host(&host)
    };
    let host = host.to_string();
    if host.is_empty() {
        return None;
    }
    Some(Page { url: parsed.into(), host, wary })
}

/// Whether `url` is a page a client may open ([`page`]).
#[must_use]
pub fn is_openable(url: &str) -> bool {
    page(url).is_some()
}

fn wary_host(host: &Host<&str>) -> Option<Wary> {
    match host {
        Host::Ipv4(ip) => wary_ip(IpAddr::V4(*ip)),
        Host::Ipv6(ip) => wary_ip(IpAddr::V6(*ip)),
        Host::Domain(name) => {
            let name = name.trim_end_matches('.');
            let under = |zone: &str| name == zone || name.ends_with(&format!(".{zone}"));
            if name.split('.').any(|label| label.starts_with("xn--")) {
                Some(Wary::Idn)
            } else if under("localhost") {
                Some(Wary::Loopback)
            } else if under("ts.net") {
                Some(Wary::Private)
            } else if !name.contains('.')
                || ["local", "lan", "home.arpa", "internal"].into_iter().any(under)
            {
                Some(Wary::LocalName)
            } else {
                None
            }
        }
    }
}

fn wary_ip(ip: IpAddr) -> Option<Wary> {
    let v4 = |ip: Ipv4Addr| {
        let [a, b, ..] = ip.octets();
        if ip.is_loopback() || ip.is_unspecified() {
            Some(Wary::Loopback)
        } else if ip.is_link_local() {
            Some(Wary::LinkLocal)
        } else if ip.is_private() || ip.is_broadcast() || (a == 100 && (64..128).contains(&b)) {
            Some(Wary::Private)
        } else {
            None
        }
    };
    let v6 = |ip: Ipv6Addr| {
        if let Some(mapped) = ip.to_ipv4_mapped() {
            return v4(mapped);
        }
        let first = ip.segments()[0];
        if ip.is_loopback() || ip.is_unspecified() {
            Some(Wary::Loopback)
        } else if first & 0xffc0 == 0xfe80 {
            Some(Wary::LinkLocal)
        } else if first & 0xfe00 == 0xfc00 {
            Some(Wary::Private)
        } else {
            None
        }
    };
    match ip {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => v6(ip),
    }
}

/// Why a page is offered to the person (a notice with an "Open" action) rather than opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum OfferReason {
    /// Nobody typed into the session just before it asked: a program opening pages on its
    /// own, not a login the person started.
    NotTyped,
    /// The address itself ([`Wary`]).
    Wary(Wary),
    /// More pages opened lately than a person asks for: the rest are offered.
    Busy,
    /// It reached the client long after it was asked (a slow or returning client).
    Late,
}

impl std::fmt::Display for OfferReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTyped => f.write_str("nobody typed into the session just before"),
            Self::Wary(wary) => wary.fmt(f),
            Self::Busy => f.write_str("several pages opened in the last few seconds"),
            Self::Late => f.write_str("it arrived late"),
        }
    }
}

/// Worker → client.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum HandoffEvent {
    /// Open a web page in the person's browser, or offer it. Answered with
    /// [`HandoffReply::Taken`] once opened, [`HandoffReply::Offered`] once offered.
    Open(OpenUrl),
    /// Show a file in a file tile beside the session's. Answered with [`HandoffReply::Taken`]
    /// once the tile shows it, [`HandoffReply::Refused`] by a client that has no file tiles;
    /// for a waiting edit, later with [`HandoffReply::Edited`].
    Edit(EditFile),
    /// Forget the handoff: the program waiting on an edit went away (the tile stops waiting),
    /// the edit's client stayed away too long, or the ask timed out and went to another
    /// client (a page not opened yet is not opened).
    Withdrawn {
        /// The edit's number.
        id: HandoffId,
    },
}

/// A web page a program asked to open.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OpenUrl {
    /// The handoff's number.
    pub id: HandoffId,
    /// The session the program runs in; `None` for one outside any session.
    pub session: Option<SessionId>,
    /// The address as parsed ([`Page::url`]).
    pub url: String,
    /// When the program asked, by the worker's clock.
    pub asked_ms: WallMs,
    /// Offer it rather than open it, and why. The client offers on its own reasons too: an
    /// address it finds [`Wary`], or one that arrived [`OfferReason::Late`].
    pub offer: Option<OfferReason>,
}

/// A file a program asked to have edited.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EditFile {
    /// The handoff's number; an ask sent again after a reconnect keeps it, so a client that
    /// already shows the file takes it without a second tile.
    pub id: HandoffId,
    /// The session the program runs in; `None` for one outside any session.
    pub session: Option<SessionId>,
    /// The file, an absolute path on the worker. It may not exist yet.
    pub path: String,
    /// The line to put the cursor on, from 1, when the program named one (`+12`).
    pub line: Option<u32>,
    /// The program waits until the person is done with it (`git commit`, `git rebase -i`,
    /// Claude Code's `Ctrl+G`): the client answers [`HandoffReply::Edited`] then.
    pub wait: bool,
}

/// Client → worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum HandoffReply {
    /// The page is open, or the file shows in a tile.
    Taken {
        /// The handoff's number.
        id: HandoffId,
    },
    /// The page is offered in a notice; the person may open it from there.
    Offered {
        /// The handoff's number.
        id: HandoffId,
        /// Why it was offered rather than opened.
        why: OfferReason,
    },
    /// This client cannot take it (a terminal client with no file tiles); the worker asks
    /// another.
    Refused {
        /// The handoff's number.
        id: HandoffId,
    },
    /// The person is done with a waiting edit. Sent after the answer to the tile's last save
    /// (`WorkerMsg::Written`) arrived: the worker also writes the tile's saves before it lets
    /// the program go, so the program reads what was saved.
    Edited {
        /// The handoff's number.
        id: HandoffId,
        /// How it ended.
        outcome: EditOutcome,
    },
}

impl HandoffReply {
    /// The handoff it answers.
    #[must_use]
    pub const fn id(&self) -> HandoffId {
        match self {
            Self::Taken { id }
            | Self::Offered { id, .. }
            | Self::Refused { id }
            | Self::Edited { id, .. } => *id,
        }
    }
}

/// How a waiting edit ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditOutcome {
    /// Closed or marked done: the program goes on with the file as saved (exit status 0).
    Done,
    /// The person gave the edit up: the program is told the editor failed (exit status 1), which
    /// makes `git commit` and `git rebase -i` stop without doing anything.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Web addresses with a host pass; every other scheme, a hostless address, and one carrying
    /// whitespace or a control character do not.
    #[test]
    fn only_web_addresses_with_a_host_are_openable() {
        for url in [
            "https://github.com/login/device",
            "http://localhost:5173/",
            "HTTPS://claude.ai/oauth/authorize?code=true&state=x#frag",
            "http://user@127.0.0.1:8080",
        ] {
            assert!(is_openable(url), "{url}");
        }
        for url in [
            "file:///etc/passwd",
            "mailto:someone@example.com",
            "vscode://file/tmp/x",
            "x-apple.systempreferences:com.apple.preference.security",
            "javascript:alert(1)",
            "https://",
            "https://:443/",
            "https://a b",
            "https://a\nb",
            "https://git\thub.com/",
            " https://github.com/",
            "github.com",
            "",
        ] {
            assert!(!is_openable(url), "{url:?}");
        }
        assert!(!is_openable(&format!("https://a/{}", "x".repeat(URL_MAX))));
    }

    /// The host is the one a browser goes to, not the one the text suggests; and an address a
    /// person would misread, or one on a network of their own, is marked to be offered.
    #[test]
    fn the_host_is_the_one_a_browser_visits_and_tricky_ones_are_marked() {
        let read = |url: &str| page(url).map(|p| (p.host, p.wary));
        let host = |h: &str, wary: Option<Wary>| Some((h.to_owned(), wary));
        assert_eq!(read("https://github.com/login/device"), host("github.com", None));
        assert_eq!(read("https://evil.com\\@github.com/"), host("evil.com", None));
        assert_eq!(
            page("https://evil.com\\@github.com/").map(|p| p.url).as_deref(),
            Some("https://evil.com/@github.com/"),
            "what opens is what was read"
        );
        assert_eq!(read("https://github.com@evil.com/"), host("evil.com", Some(Wary::UserInfo)));
        assert_eq!(read("https://u:p@example.com/"), host("example.com", Some(Wary::UserInfo)));
        assert_eq!(read("https://gіthub.com/"), host("xn--gthub-n2e.com", Some(Wary::Idn)));
        assert_eq!(
            read("https://xn--80ak6aa92e.com/"),
            host("xn--80ak6aa92e.com", Some(Wary::Idn))
        );
        assert_eq!(read("http://127.0.0.1:8080/"), host("127.0.0.1", Some(Wary::Loopback)));
        assert_eq!(read("http://0x7f.1/"), host("127.0.0.1", Some(Wary::Loopback)));
        assert_eq!(read("http://2130706433/"), host("127.0.0.1", Some(Wary::Loopback)));
        assert_eq!(read("http://0.0.0.0:3000/"), host("0.0.0.0", Some(Wary::Loopback)));
        assert_eq!(read("http://[::1]:3000/"), host("[::1]", Some(Wary::Loopback)));
        assert_eq!(read("http://[::ffff:192.168.1.1]/").map(|h| h.1), Some(Some(Wary::Private)));
        assert_eq!(read("http://LOCALHOST./"), host("localhost.", Some(Wary::Loopback)));
        assert_eq!(read("http://app.localhost/"), host("app.localhost", Some(Wary::Loopback)));
        assert_eq!(read("http://192.168.1.10/"), host("192.168.1.10", Some(Wary::Private)));
        assert_eq!(read("http://10.0.0.1/"), host("10.0.0.1", Some(Wary::Private)));
        assert_eq!(read("http://172.20.0.1/"), host("172.20.0.1", Some(Wary::Private)));
        assert_eq!(read("http://100.101.102.103/"), host("100.101.102.103", Some(Wary::Private)));
        assert_eq!(read("http://[fd7a:115c:a1e0::1]/").map(|h| h.1), Some(Some(Wary::Private)));
        assert_eq!(
            read("http://mac-studio.tail1234.ts.net/").map(|h| h.1),
            Some(Some(Wary::Private))
        );
        assert_eq!(read("http://169.254.169.254/").map(|h| h.1), Some(Some(Wary::LinkLocal)));
        assert_eq!(read("http://[fe80::1]/").map(|h| h.1), Some(Some(Wary::LinkLocal)));
        assert_eq!(read("http://printer.local/").map(|h| h.1), Some(Some(Wary::LocalName)));
        assert_eq!(read("http://router.lan/").map(|h| h.1), Some(Some(Wary::LocalName)));
        assert_eq!(read("http://intranet/").map(|h| h.1), Some(Some(Wary::LocalName)));
        assert_eq!(read("https:///path"), host("path", Some(Wary::LocalName)), "slashes skipped");
        assert_eq!(read("https://8.8.8.8/"), host("8.8.8.8", None));
        assert_eq!(read("https://100.128.0.1/"), host("100.128.0.1", None), "past 100.64/10");
    }
}
