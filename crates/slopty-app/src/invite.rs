//! A phone or iPad finds the server from a Mac that knows it: the Mac shows a code, the
//! device's own Camera reads it, and Slopty opens on the device with the address filled in.
//!
//! The code is a link, `slopty://connect?server=<host:port>`, which iOS hands to Slopty through
//! its URL scheme, so Slopty asks for no camera. A link opens "Connect to a server" with the
//! address in its field and connects nothing: the person presses Connect, since any page or app
//! may open such a link.
//!
//! The address is the one the device reaches: the server's own, or, for a server on this Mac
//! (which this app reaches over loopback), this Mac's name on the tailnet.

use std::cell::RefCell;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, FocusHandle, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_net::HostAddr;
use slopty_theme::{Rgb, Theme, Variant};
use slopty_ui::colors::hsla;
use slopty_ui::kit::{self, ButtonKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub(crate) mod actions {
    //! The palette's way to the code.
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        workers,
        [
            /// Show the code a phone or iPad scans to connect to this app's server.
            ConnectDevice,
        ]
    );
}

/// The link's scheme and what it asks.
const PREFIX: &str = "slopty://connect?";
/// The query key that holds the server's address.
const SERVER_KEY: &str = "server";

/// The palette's line, and the dialog's heading.
pub(crate) const TITLE: &str = "Connect a phone or iPad";
/// What the dialog says under the code.
pub(crate) const HOW: &str =
    "Point the Camera of your iPhone or iPad at the code, then press Connect in Slopty.";
/// Why there is no code: the server is on this Mac, which no tailnet names.
pub(crate) const NO_NAME: &str =
    "This Mac is on no tailnet, so your devices need its address on your VPN: type it there.";
/// Why there is no code: this app has no server.
pub(crate) const NO_SERVER: &str = "Connect to a server first: the code carries its address.";
/// Why there is no code: the link is past what a code holds, which no address comes near.
const TOO_LONG: &str = "The server's address is too long for a code: type it on your device.";
/// What the dialog says while it asks the tailnet for this Mac's name.
const LOOKING: &str = "Finding this Mac on your tailnet\u{2026}";
/// What "Copy link" says it did.
const LINK_COPIED: &str = "Link copied";
/// The code's side at most, in points: whole points per module under it, so every module
/// edge falls on a pixel.
const CODE_SIDE: f32 = 208.0;
/// Light modules round the code on every side, as the QR standard (ISO/IEC 18004) asks: a
/// scanner finds the code's edge by them.
const QUIET: usize = 4;

/// The link that opens Slopty on a device at "Connect to a server" with `server` filled in.
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

/// The server a link names: only [`link`]'s exact form, `slopty://connect?server=<host:port>`
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

/// The address a device reaches the server at `server` by, this app reaching it there; `name`
/// is this Mac's name on the tailnet, when one names it. A server on this Mac is reached over
/// loopback here and by that name elsewhere.
pub(crate) fn reached(server: &HostAddr, name: Option<&str>) -> Result<HostAddr, &'static str> {
    if !loopback(server.host()) {
        return Ok(server.clone());
    }
    name.filter(|n| !n.is_empty()).map(|name| HostAddr::new(name, server.port())).ok_or(NO_NAME)
}

/// Whether `host` is this machine's own loopback.
fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The colours the code is drawn in, the light theme's page and its text whatever this app's
/// appearance: a scanner needs dark modules on a light field, and an inverted code on a dark
/// dialog is one many scanners do not read.
fn inks() -> (Rgb, Rgb) {
    let paper = Theme::new(Variant::Light);
    (paper.content(), paper.surfaces.text)
}

/// A code's modules with its quiet zone, as the runs of dark modules each row holds.
#[derive(Debug, PartialEq, Eq)]
struct Matrix {
    /// Modules a side, the quiet zone's included.
    side: usize,
    /// Each run of dark modules: its row, its first column and its length.
    runs: Vec<(usize, usize, usize)>,
}

impl Matrix {
    /// The code for `link`, at the medium error correction (15 % of it may be lost); `None`
    /// past what a code holds.
    fn of(link: &str) -> Option<Self> {
        let code = qrcode::QrCode::with_error_correction_level(link, qrcode::EcLevel::M).ok()?;
        let width = code.width();
        let side = QUIET.checked_mul(2)?.checked_add(width)?;
        let colours = code.to_colors();
        let mut runs = Vec::new();
        for (row, modules) in colours.chunks(width.max(1)).enumerate() {
            let mut from = None;
            let dark = modules.iter().map(|&c| c == qrcode::Color::Dark).chain([false]);
            for (col, dark) in dark.enumerate() {
                match (dark, from) {
                    (true, None) => from = Some(col),
                    (false, Some(first)) => {
                        let len = col.checked_sub(first)?;
                        runs.push((row.checked_add(QUIET)?, first.checked_add(QUIET)?, len));
                        from = None;
                    }
                    _ => {}
                }
            }
        }
        Some(Self { side, runs })
    }

    /// Whole points a module, as large as [`CODE_SIDE`] allows.
    #[expect(clippy::cast_precision_loss, reason = "a code is at most 185 modules a side")]
    fn module(&self) -> f32 {
        (CODE_SIDE / self.side.max(1) as f32).floor().max(1.0)
    }

    /// The code drawn on its field, [`inks`]'s colours, `corner` round the field.
    #[expect(clippy::cast_precision_loss, reason = "a code is at most 185 modules a side")]
    fn element(self: std::rc::Rc<Self>, corner: f32) -> gpui::Canvas<()> {
        let module = self.module();
        let side = module * self.side as f32;
        let (paper, ink) = inks();
        gpui::canvas(
            |_bounds, _window, _cx| {},
            move |bounds, (), window, _cx| {
                window.paint_quad(gpui::fill(bounds, hsla(paper)).corner_radii(px(corner)));
                for &(row, col, len) in &self.runs {
                    let at = gpui::point(px(col as f32 * module), px(row as f32 * module));
                    let run = gpui::size(px(len as f32 * module), px(module));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(bounds.origin + at, run),
                        hsla(ink),
                    ));
                }
            },
        )
        .size(px(side))
    }
}

/// What the dialog shows.
#[derive(Debug)]
enum Shown {
    /// This Mac's name is being asked of the tailnet.
    Looking,
    /// The code for `address`, whose link is `link`.
    Code { address: HostAddr, link: String, matrix: std::rc::Rc<Matrix> },
    /// No code, and why.
    Missing(&'static str),
}

impl Shown {
    /// The code for the address a device reaches the server by, or why there is none.
    fn of(reached: Result<HostAddr, &'static str>) -> Self {
        match reached {
            Ok(address) => {
                let link = link(&address);
                match Matrix::of(&link) {
                    Some(matrix) => Self::Code { address, link, matrix: std::rc::Rc::new(matrix) },
                    None => Self::Missing(TOO_LONG),
                }
            }
            Err(why) => Self::Missing(why),
        }
    }
}

/// The dialog with the code, while it is up.
#[derive(Debug)]
pub(crate) struct Invite {
    shown: Shown,
    /// It holds the keyboard, so Esc reaches it.
    focus: FocusHandle,
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

impl crate::Workspace {
    /// A link opened Slopty: one of [`link`]'s opens "Connect to a server" with its address in
    /// the field, and connecting stays the person's press. Any other is ignored.
    pub(crate) fn open_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = server_in(url) else { return };
        tracing::debug!(%server, "a link names a server");
        self.close_invite(window, cx);
        self.show_add_worker(crate::Panel::Server, window, cx);
        let Some(adding) = &mut self.adding else { return };
        if adding.busy {
            return;
        }
        adding.error = None;
        adding.address.update(cx, |input, cx| input.set_value(server.to_string(), window, cx));
        cx.notify();
    }

    /// Show the code for this app's server; for a server on this Mac, once the tailnet names
    /// the Mac.
    pub(crate) fn show_invite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = match self.server_address().cloned() {
            None => Shown::Missing(NO_SERVER),
            Some(server) if loopback(server.host()) => {
                self.name_this_mac(server, cx);
                Shown::Looking
            }
            Some(server) => Shown::of(Ok(server)),
        };
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.inviting = Some(Invite { shown, focus });
        cx.notify();
    }

    /// Ask the tailnet for this Mac's name, for the code of `server`, which is on this Mac.
    fn name_this_mac(&self, server: HostAddr, cx: &Context<Self>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _unheard = tx.send(crate::net::tailnet_name().await);
        });
        cx.spawn(async move |this, cx| {
            let name = rx.await.ok().flatten();
            let _gone = this.update(cx, |ws, cx| {
                if let Some(invite) = &mut ws.inviting
                    && matches!(invite.shown, Shown::Looking)
                {
                    invite.shown = Shown::of(reached(&server, name.as_deref()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Close the dialog, the keyboard going back where it was.
    pub(crate) fn close_invite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inviting.take().is_some() {
            self.view.update(cx, |view, cx| view.return_keyboard(window, cx));
            cx.notify();
        }
    }

    /// Put the code's link on the clipboard, for a device with no camera to hand.
    fn copy_invite_link(&self, cx: &mut Context<Self>) {
        let Some(Shown::Code { link, .. }) = self.inviting.as_ref().map(|i| &i.shown) else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
        self.show_notice(LINK_COPIED.to_owned(), cx);
    }

    /// The dialog over the workspace: the code, the address it carries under it, and how to
    /// use it; closed by Done, Esc or a click outside it.
    pub(crate) fn invite_dialog(
        &self,
        invite: &Invite,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let (s, sp) = (theme.surfaces, theme.spacing);
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let (body, blurb, copy) = match &invite.shown {
            Shown::Looking => {
                (kit::meta(div(), theme).child(LOOKING).into_any_element(), None, false)
            }
            Shown::Missing(why) => (
                div()
                    .id("invite-missing")
                    .role(Role::Status)
                    .aria_label(*why)
                    .text_color(hsla(s.text_secondary))
                    .child(*why)
                    .into_any_element(),
                None,
                false,
            ),
            Shown::Code { address, matrix, .. } => {
                let address = address.to_string();
                let code = div()
                    .id("invite-code")
                    .debug_selector(|| "invite-code".to_owned())
                    .role(Role::Image)
                    .aria_label(format!("Code for {address}"))
                    .child(std::rc::Rc::clone(matrix).element(theme.radii.md));
                let named = div()
                    .id("invite-address")
                    .role(Role::Label)
                    .aria_label(address.clone())
                    .font_family(mono)
                    .text_color(hsla(s.text_secondary))
                    .child(address);
                let body = div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(sp.sm))
                    .child(code)
                    .child(named)
                    .into_any_element();
                (body, Some(HOW), true)
            }
        };
        let heading =
            kit::title(theme, TITLE).id("invite-title").role(Role::Heading).aria_label(TITLE);
        let intro =
            div().flex().flex_col().gap(px(sp.xs)).child(heading).when_some(blurb, |el, blurb| {
                el.child(div().text_color(hsla(s.text_secondary)).child(blurb))
            });
        let copy = copy.then(|| {
            kit::button(theme, "invite-copy", "Copy link", ButtonKind::Secondary)
                .on_click(cx.listener(|this, _ev, _window, cx| this.copy_invite_link(cx)))
        });
        let done = kit::button(theme, "invite-done", "Done", ButtonKind::Primary)
            .on_click(cx.listener(|this, _ev, window, cx| this.close_invite(window, cx)));
        let dialog = kit::dialog(theme, kit::Overlay::List)
            .id("invite")
            .debug_selector(|| "invite".to_owned())
            .track_focus(&invite.focus)
            .role(Role::Dialog)
            .aria_label(TITLE)
            .max_w(px(crate::ADD_PANEL_W))
            .p(px(sp.xl))
            .gap(px(sp.lg))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_invite(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(intro)
            .child(body)
            .child(div().flex().justify_end().gap(px(sp.xs)).children(copy).child(done));
        // As the add-worker dialog: it fades in where it stands, the scrim dimming with it.
        let dialog = kit::fade_in(dialog, "invite-in", cx);
        let backdrop = kit::backdrop(theme, window)
            .id("invite-backdrop")
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.close_invite(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(dialog);
        if !kit::motion(cx) {
            return backdrop.into_any_element();
        }
        let dim = kit::scrim(theme);
        backdrop
            .with_animation("invite-scrim", kit::Pace::Fade.animation(), move |el, t| {
                el.bg(gpui::Hsla { a: dim.a * t, ..dim })
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_net::endpoint::SERVER_PORT;

    use super::*;

    /// A link names the server, an IPv6 one in brackets, and reads back as it.
    #[test]
    fn a_link_carries_the_server_and_reads_back() {
        let named = HostAddr::new("studio.tail1234.ts.net", SERVER_PORT);
        assert_eq!(link(&named), "slopty://connect?server=studio.tail1234.ts.net:45560");
        let v6 = HostAddr::new("fd7a:115c:a1e0::1", 45_600);
        assert_eq!(link(&v6), "slopty://connect?server=%5Bfd7a:115c:a1e0::1%5D:45600");
        for server in [named, v6, HostAddr::new("100.64.0.3", 1), HostAddr::new("::1", 65_535)] {
            assert_eq!(server_in(&link(&server)), Some(server.clone()), "{server}");
        }
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

    /// A server elsewhere is reached as this app reaches it; one on this Mac by this Mac's
    /// tailnet name, and with no name, not at all.
    #[test]
    fn a_device_reaches_the_server_as_it_can() {
        let studio = HostAddr::new("studio.local", SERVER_PORT);
        assert_eq!(reached(&studio, None), Ok(studio.clone()));
        let here = HostAddr::new("127.0.0.1", SERVER_PORT);
        let named = reached(&here, Some("mac-studio.tail1234.ts.net"));
        assert_eq!(named, Ok(HostAddr::new("mac-studio.tail1234.ts.net", SERVER_PORT)));
        assert_eq!(reached(&HostAddr::new("::1", 1), Some("m")), Ok(HostAddr::new("m", 1)));
        assert_eq!(reached(&HostAddr::new("localhost", 1), None), Err(NO_NAME));
        assert_eq!(reached(&here, Some("")), Err(NO_NAME));
    }

    /// The code keeps four light modules on every side, opens on a finder pattern's dark top
    /// edge, and fits the side it is drawn at in whole points a module.
    #[test]
    fn the_code_keeps_its_quiet_zone() {
        let link = link(&HostAddr::new("mac-studio.tail1234.ts.net", SERVER_PORT));
        let code = Matrix::of(&link).expect("a link fits a code");
        let far = code.side - QUIET;
        for &(row, col, len) in &code.runs {
            assert!((QUIET..far).contains(&row), "row {row} in the quiet zone");
            assert!(col >= QUIET && col + len <= far, "run {col}+{len} in the quiet zone");
        }
        assert!(code.runs.contains(&(QUIET, QUIET, 7)), "the top-left finder's edge");
        let module = code.module();
        assert!(module >= 4.0 && module.fract() == 0.0, "{module}");
        let side = f32::from(u16::try_from(code.side).unwrap());
        assert!(module * side <= CODE_SIDE, "{module} × {side}");
    }

    /// Dark modules on a light field whichever the appearance, at text contrast and more.
    #[test]
    fn the_code_is_dark_on_light() {
        let (paper, ink) = inks();
        assert!(paper.luminance() > ink.luminance(), "light field, dark modules");
        assert!(paper.contrast(ink) >= 7.0, "{}", paper.contrast(ink));
    }

    /// A link opens "Connect to a server" with its address in the field, from any panel, and
    /// connects nothing; any other link changes nothing.
    #[gpui::test]
    fn a_link_fills_the_server_s_field_and_waits(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = crate::tests::shell(cx, &runtime, &dir, true);
        let typed = |ws: &gpui::Entity<crate::Workspace>, cx: &mut gpui::VisualTestContext| {
            ws.read_with(cx, |ws, cx| {
                ws.adding.as_ref().map(|a| (a.mode, a.busy, a.address.read(cx).value().to_string()))
            })
        };
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.open_link("slopty://connect?server=a:1&x=y", window, cx));
        });
        assert_eq!(typed(&ws, cx), Some((crate::Panel::Worker, false, String::new())));
        cx.update(|window, cx| {
            let url = "slopty://connect?server=studio.tail1234.ts.net:45560";
            ws.update(cx, |ws, cx| ws.open_link(url, window, cx));
        });
        cx.run_until_parked();
        let filled = "studio.tail1234.ts.net:45560".to_owned();
        assert_eq!(typed(&ws, cx), Some((crate::Panel::Server, false, filled)));
    }

    /// The palette's line shows the code for a server elsewhere at once, with its address under
    /// it; Esc closes it.
    #[gpui::test]
    fn the_code_shows_and_esc_closes_it(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = crate::tests::shell(cx, &runtime, &dir, true);
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                ws.adding = None;
                ws.show_invite(window, cx);
            });
        });
        cx.run_until_parked();
        let code = cx.debug_bounds("invite-code").expect("the code");
        assert!((code.size.width - code.size.height).abs() < px(0.5), "square: {code:?}");
        let shown = ws.read_with(cx, |ws, _| match ws.inviting.as_ref().map(|i| &i.shown) {
            Some(Shown::Code { address, .. }) => Some(address.to_string()),
            _ => None,
        });
        assert_eq!(shown.as_deref(), Some("hub:45560"), "the server's own address");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.inviting.is_none()), "Esc closes it");
    }
}
