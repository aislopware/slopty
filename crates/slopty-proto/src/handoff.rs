//! What a program in a worker's shell hands to the client in front of the person: a web page to
//! open, a file to edit.
//!
//! A shell on the worker gets `BROWSER` and `EDITOR` naming the `slopty` CLI, and an `open` that
//! forwards web addresses (`slopty_pty::shell_integration`). The CLI asks the worker over its
//! control socket (`ctl::CtlRequest::Open`, `ctl::CtlRequest::Edit`). The worker asks the clients
//! that said they take handoffs ([`HandoffCaps`]), the one in front of the shell first, with a
//! [`HandoffEvent`], and each answers with a [`HandoffReply`] (`docs/decisions/terminal.md`, "A
//! shell's browser and editor are the client's").
//!
//! An agent drives the person's window the same way (`slopty ui`, the `ui_*` tools): the CLI
//! asks with `ctl::CtlRequest::Ui`, the worker hands a [`HandoffEvent::Ui`] to the clients that
//! take pages, the one in front of the agent first, and the first with a window answers with
//! [`HandoffReply::Ui`] (`docs/decisions/agents.md`, "Agents drive the person's window through
//! open verbs").

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
    /// Do something in the person's window for an agent. Answered with [`HandoffReply::Ui`],
    /// or [`HandoffReply::Refused`] by a client with no window.
    Ui(UiAsk),
}

/// What an agent asks of the person's window, and who asks.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UiAsk {
    /// The handoff's number.
    pub id: HandoffId,
    /// The session the agent runs in (its seat, or a shell's), which names its chat; `None`
    /// for a program outside any.
    pub session: Option<SessionId>,
    /// What to do.
    pub act: UiAct,
}

/// What an agent asks of the person's window: the verbs of `slopty ui`.
///
/// What they name stays open: a region, a place or a chat is a word the window reads, and a
/// word it does not know is answered with the ones it does ([`UiError::Unknown`]). Paths are
/// absolute on the asking agent's machine.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAct {
    /// Say what the window shows.
    State,
    /// Open a shell in a pane beside the chat on screen.
    Terminal {
        /// The folder it starts in; the chat's own when `None`.
        folder: Option<String>,
        /// A command line to run in it once its shell is up. One that acts as root or
        /// destroys, or that spans lines, is left at the prompt for the person to read.
        command: Option<String>,
    },
    /// Show something in a pane beside the session.
    Open(UiTarget),
    /// Show, hide, expand or collapse one region of the window.
    Region {
        /// `sidebar`, `files`, `changes`, `inspector` or `terminal`, as [`UiWindow::regions`]
        /// names them.
        region: String,
        /// What becomes of it.
        change: UiChange,
    },
    /// Put a chat on screen.
    Chat {
        /// Its id, a prefix of its id, or its title; the agent's own chat when `None`.
        chat: Option<String>,
    },
    /// Lay the session's panes at even shares, showing `target` beside it first when given.
    Split(Option<UiTarget>),
}

/// What a pane beside the session shows for an agent.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTarget {
    /// A file, by its absolute path, at a line from 1 when given.
    File {
        /// The file.
        path: String,
        /// The line.
        line: Option<u32>,
    },
    /// A web page ([`page`]).
    Page {
        /// Its address.
        url: String,
    },
    /// The chat's uncommitted changes.
    Diff,
}

/// What becomes of a region ([`UiAct::Region`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiChange {
    /// On screen.
    Show,
    /// Off screen.
    Hide,
    /// The other of the two.
    Toggle,
    /// As large as it goes: shown.
    Expand,
    /// Back out of the way: hidden.
    Collapse,
}

/// What an agent's ask did, and the window after it.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct UiDone {
    /// What changed, in words, as the window marked it (`opened a terminal in ~/app`); `None`
    /// when nothing changed.
    pub did: Option<String>,
    /// The pane it opened.
    pub made: Option<UiTab>,
    /// For a shell opened with a command: whether it ran, or waits at the prompt for the
    /// person to read and run.
    pub ran: Option<bool>,
    /// The window after it.
    pub window: UiWindow,
}

/// What the person's window shows.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct UiWindow {
    /// The page on screen: `chat`, or `settings` with its section after a slash.
    pub page: String,
    /// The chat on screen; `None` on the new-chat page.
    pub chat: Option<UiChat>,
    /// Its regions and whether each shows.
    pub regions: Vec<UiRegion>,
    /// The panes beside the session, in reading order.
    pub tabs: Vec<UiTab>,
    /// The person is typing a draft: what would move the keyboard or the chat away from it is
    /// refused ([`UiError::Busy`]).
    pub typing: bool,
}

/// A chat, as the window shows it.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct UiChat {
    /// Its id.
    pub id: String,
    /// Its title.
    pub title: Option<String>,
    /// The machine it runs on, by name.
    pub machine: Option<String>,
}

/// One region of the window.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct UiRegion {
    /// Its name: `sidebar`, `files`, `changes`, `inspector`, `terminal`.
    pub name: String,
    /// It is on screen.
    pub shown: bool,
    /// Its pane fills the session's view (zoomed).
    pub expanded: bool,
}

/// One pane beside the session.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct UiTab {
    /// The window's name for it, stable while it is open (`pane/file/3`, `terminal/2`).
    pub id: String,
    /// What it shows: `file`, `page`, `diff`, `terminal`, `chat`, `screen`…
    pub kind: String,
    /// Its title as its header shows it.
    pub title: String,
    /// The region it belongs to, or `pane` for a view of none (a file, a page, a screen).
    pub region: String,
    /// It has the keyboard.
    pub selected: bool,
    /// A shell's session id, once it is up.
    pub terminal: Option<String>,
    /// The file it shows, or the page's address.
    pub target: Option<String>,
}

/// Why an agent's ask was not done.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UiFailure {
    /// What kind of failure.
    pub code: UiError,
    /// For the agent to read: what went wrong and what would do it.
    pub message: String,
}

impl UiFailure {
    /// A failure of `code`, said as `message`.
    pub fn new(code: UiError, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for UiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// What kind of failure an agent's ask met.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiError {
    /// No Slopty window is open on this machine set, or none answered.
    NoWindow,
    /// It names a region, chat, file or place the window does not know.
    Unknown,
    /// The person is typing, or in Settings: the window stays as they have it.
    Busy,
    /// The chat on screen is on another machine than the agent, or there is none.
    Elsewhere,
    /// The window tried and it did not work.
    Failed,
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
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    /// An agent's ask of the window was done, or why not.
    Ui {
        /// The handoff's number.
        id: HandoffId,
        /// What it did and the window after it, or why it did nothing.
        outcome: Box<Result<UiDone, UiFailure>>,
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
            | Self::Edited { id, .. }
            | Self::Ui { id, .. } => *id,
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

    /// An agent's ask of the window and its answer cross the link whole, each verb and each
    /// failure; the answer names the ask it answers.
    #[test]
    fn an_agents_ask_and_its_answer_cross_the_link() {
        let file = UiTarget::File { path: "/w/app/src/main.rs".to_owned(), line: Some(12) };
        let acts = [
            UiAct::State,
            UiAct::Terminal {
                folder: Some("/w/app".to_owned()),
                command: Some("cargo test".to_owned()),
            },
            UiAct::Open(file.clone()),
            UiAct::Open(UiTarget::Page { url: "https://example.com/".to_owned() }),
            UiAct::Open(UiTarget::Diff),
            UiAct::Region { region: "sidebar".to_owned(), change: UiChange::Expand },
            UiAct::Chat { chat: Some("Tidy the greeting".to_owned()) },
            UiAct::Split(Some(file)),
            UiAct::Split(None),
        ];
        for act in acts {
            let event = HandoffEvent::Ui(UiAsk { id: 7, session: None, act });
            let bytes = crate::codec::encode_body(&event).unwrap();
            assert_eq!(crate::codec::decode_body::<HandoffEvent>(&bytes).unwrap(), event);
        }
        let tab = UiTab {
            id: "terminal/2".to_owned(),
            kind: "terminal".to_owned(),
            title: "zsh".to_owned(),
            region: "terminal".to_owned(),
            selected: true,
            terminal: Some("0192".to_owned()),
            target: None,
        };
        let window = UiWindow {
            page: "chat".to_owned(),
            chat: Some(UiChat { id: "c1".to_owned(), title: None, machine: Some("studio".into()) }),
            regions: vec![UiRegion { name: "inspector".to_owned(), shown: true, expanded: false }],
            tabs: vec![tab.clone()],
            typing: true,
        };
        let done = UiDone { did: Some("opened".into()), made: Some(tab), ran: Some(false), window };
        for outcome in [Ok(done), Err(UiFailure::new(UiError::Busy, "the person is typing"))] {
            let reply = HandoffReply::Ui { id: 9, outcome: Box::new(outcome) };
            let bytes = crate::codec::encode_body(&reply).unwrap();
            assert_eq!(crate::codec::decode_body::<HandoffReply>(&bytes).unwrap(), reply);
            assert_eq!(reply.id(), 9);
        }
    }

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
