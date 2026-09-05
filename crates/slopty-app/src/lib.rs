//! The Slopty app shell, shared by the macOS and iOS apps.
//!
//! Connects to the paired host, then shows its canvas: every terminal the host knows about, on
//! one plane shared by all clients. Networking runs on a tokio runtime thread; GPUI owns the
//! main thread. The two talk through channels only. The platform binaries set up logging, the
//! runtime and the GPUI application, then call [`open_workspace`].

pub mod net;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    WindowOptions, div, px,
};
use gpui_kit::component::Root;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::LinkEvent;
use slopty_proto::HostMsg;
use slopty_theme::Theme;
use slopty_ui::canvas::{CanvasEvent, CanvasView, FitAll, NewNote, NewTerminal};
use slopty_ui::colors::hsla;
use slopty_ui::terminal::TerminalView;

/// Height of the top bar (the titlebar area; traffic lights sit at its left on macOS).
const TOP_BAR: f32 = 38.0;
/// Space reserved for the traffic lights.
#[cfg(target_os = "macos")]
const LEADING_INSET: f32 = 78.0;
/// No traffic lights on iOS; the safe area is added from the window's insets.
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: f32 = 12.0;
/// Keyboard shortcut hints make sense where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");
/// A row of keys the soft keyboard lacks (Esc, Tab, Control, arrows, shell symbols).
const KEY_BAR: bool = cfg!(target_os = "ios");
/// Key bar height in points.
const KEY_BAR_H: f32 = 40.0;

/// The key bar's keys: label, GPUI key name, and the character it types (`None` for
/// non-printing keys).
const BAR_KEYS: [(&str, &str, Option<&str>); 11] = [
    ("esc", "escape", None),
    ("tab", "tab", None),
    ("⌃", "", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("-", "-", Some("-")),
    ("/", "/", Some("/")),
    ("|", "|", Some("|")),
    ("~", "~", Some("~")),
];
/// Nothing heard from the host for this long is shown as a warning (keep-alives run every 5 s).
const SILENCE_WARN: std::time::Duration = std::time::Duration::from_secs(8);
/// Nothing heard for this long and the connection is given up so the reconnect loop takes
/// over: a restarted host is back in ~1 s instead of after the transport's 45 s idle timeout.
/// Three missed keep-alives, the same bar noq uses to abandon a path.
const SILENCE_DROP: std::time::Duration = std::time::Duration::from_secs(15);

/// The pairing panel: shown until this installation knows a host.
#[derive(Debug)]
struct Pairing {
    /// Where the ticket is typed or pasted.
    ticket: Entity<InputState>,
    /// A pairing attempt is in flight.
    busy: bool,
    /// Why the last attempt failed.
    error: Option<String>,
}

/// The window's root view.
#[derive(Debug)]
pub struct Workspace {
    canvas: Option<Entity<CanvasView>>,
    status: String,
    host_name: String,
    zoom: f32,
    rtt: Option<std::time::Duration>,
    relayed: Option<bool>,
    /// How long the host has sent nothing at all, once past [`SILENCE_WARN`].
    silent: Option<std::time::Duration>,
    theme: Theme,
    subscriptions: Vec<gpui::Subscription>,
    pairing: Option<Pairing>,
    /// Networking runtime; pairing runs there.
    runtime: tokio::runtime::Handle,
    /// Fires once a pairing succeeds; the connect loop waits on it while unpaired.
    paired: tokio::sync::mpsc::UnboundedSender<()>,
}

impl Workspace {
    /// Show the pairing panel (idempotent).
    fn show_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing.is_some() {
            return;
        }
        let ticket = cx.new(|cx| {
            InputState::new(window, cx).placeholder("sloptypair… (from `slopty host ticket`)")
        });
        self.subscriptions.push(cx.subscribe(&ticket, |this, _input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.pair(cx);
            }
        }));
        ticket.update(cx, |input, cx| input.focus(window, cx));
        self.pairing = Some(Pairing { ticket, busy: false, error: None });
        "not paired".clone_into(&mut self.status);
        cx.notify();
    }

    /// Put the clipboard's text into the ticket field.
    fn paste_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pairing) = &self.pairing else { return };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        pairing.ticket.update(cx, |input, cx| input.set_value(text.trim().to_owned(), window, cx));
        self.pair(cx);
    }

    /// Redeem the ticket in the field on the runtime; report back to the panel.
    fn pair(&mut self, cx: &mut Context<Self>) {
        let Some(pairing) = &mut self.pairing else { return };
        if pairing.busy {
            return;
        }
        let ticket = pairing.ticket.read(cx).value().trim().to_owned();
        if ticket.is_empty() {
            return;
        }
        pairing.busy = true;
        pairing.error = None;
        cx.notify();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::pair_host(&ticket).await);
        });
        cx.spawn(async move |this, cx| {
            let outcome = rx.await;
            let _updated = this.update(cx, |ws, cx| {
                match outcome {
                    Ok(Ok(name)) => {
                        ws.pairing = None;
                        ws.status = format!("paired with {name}; connecting…");
                        let _woken = ws.paired.send(());
                    }
                    Ok(Err(e)) => {
                        if let Some(p) = &mut ws.pairing {
                            p.busy = false;
                            p.error = Some(format!("{e:#}"));
                        }
                    }
                    Err(_dropped) => {
                        if let Some(p) = &mut ws.pairing {
                            p.busy = false;
                            p.error = Some("pairing task died".to_owned());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pairing_panel(&self, pairing: &Pairing, cx: &Context<Self>) -> impl IntoElement {
        let s = &self.theme.surfaces;
        let button = |id: &'static str, text: &'static str, accent: bool| {
            div()
                .id(id)
                .px(px(12.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .text_size(px(13.0))
                .text_color(hsla(if accent { s.canvas } else { s.text }))
                .bg(hsla(if accent { s.accent } else { s.canvas }))
                .hover(|el| el.opacity(0.85))
                .cursor_pointer()
                .child(text)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(420.0))
            .max_w_full()
            .p(px(20.0))
            .rounded(px(10.0))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .child(div().text_size(px(15.0)).text_color(hsla(s.text)).child("Pair with a host"))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(hsla(s.text_muted))
                    .child("On the host Mac run `slopty host ticket`, then paste the ticket here."),
            )
            .child(Input::new(&pairing.ticket))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .items_center()
                    .child(
                        button("pair", "Pair", true)
                            .on_click(cx.listener(|this, _ev, _window, cx| this.pair(cx))),
                    )
                    .child(button("paste-ticket", "Paste & pair", false).on_click(
                        cx.listener(|this, _ev, window, cx| this.paste_ticket(window, cx)),
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(hsla(if pairing.error.is_some() {
                                self.theme.terminal.palette(1)
                            } else {
                                s.text_muted
                            }))
                            .child(SharedString::from(match (&pairing.error, pairing.busy) {
                                (Some(e), _) => e.clone(),
                                (None, true) => "pairing…".to_owned(),
                                (None, false) => String::new(),
                            })),
                    ),
            )
    }

    /// Esc, Tab, sticky Control, arrows and the shell symbols a phone keyboard hides; shown
    /// above the keyboard inset while a terminal is active.
    fn key_bar(&self, terminal: &Entity<TerminalView>, cx: &Context<Self>) -> gpui::AnyElement {
        let s = &self.theme.surfaces;
        let armed = terminal.read(cx).sticky_control();
        let has_selection = terminal.read(cx).selection().is_some();
        let mut bar = div()
            .h(px(KEY_BAR_H))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(6.0))
            .gap(px(4.0))
            .bg(hsla(s.panel))
            .border_t_1()
            .border_color(hsla(s.border))
            .font_family(self.theme.typography.ui_family.clone());
        for (label, key, typed) in BAR_KEYS {
            let is_control = key.is_empty();
            let lit = is_control && armed;
            let target = terminal.clone();
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("key-{label}")))
                    .flex_1()
                    .h(px(KEY_BAR_H - 10.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_size(px(14.0))
                    .text_color(hsla(if lit { s.canvas } else { s.text }))
                    .bg(hsla(if lit { s.accent } else { s.canvas }))
                    .active(|el| el.opacity(0.7))
                    .child(SharedString::from(label))
                    .on_click(move |_ev, _window, cx| {
                        target.update(cx, |t, cx| {
                            if is_control {
                                let on = !t.sticky_control();
                                t.set_sticky_control(on, cx);
                            } else {
                                t.press(
                                    gpui::Keystroke {
                                        modifiers: gpui::Modifiers::default(),
                                        key: key.to_owned(),
                                        key_char: typed.map(str::to_owned),
                                    },
                                    cx,
                                );
                            }
                        });
                    }),
            );
        }
        // The phone has no ⌘C/⌘V: while text is selected the bar offers copy, otherwise paste.
        let target = terminal.clone();
        bar = bar.child(
            div()
                .id("key-clipboard")
                .flex_1()
                .h(px(KEY_BAR_H - 10.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .text_size(px(12.0))
                .text_color(hsla(if has_selection { s.canvas } else { s.text }))
                .bg(hsla(if has_selection { s.accent } else { s.canvas }))
                .active(|el| el.opacity(0.7))
                .child(if has_selection { "copy" } else { "paste" })
                .on_click(move |_ev, window, cx| {
                    target.update(cx, |t, cx| {
                        if has_selection {
                            // Copy, then the key reads "paste" again.
                            t.copy(&slopty_ui::terminal::Copy, window, cx);
                            t.clear_selection(cx);
                        } else {
                            t.paste_clipboard(&slopty_ui::terminal::Paste, window, cx);
                        }
                    });
                }),
        );
        bar.into_any_element()
    }

    fn top_bar(&self, safe_top: gpui::Pixels, cx: &Context<Self>) -> impl IntoElement {
        let s = &self.theme.surfaces;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0–400")]
        let zoom_pct = (self.zoom * 100.0).round().max(0.0) as u32;
        let label = |text: String| {
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(text))
        };
        let button = |id: &'static str, text: &'static str, hint: &'static str| {
            div()
                .id(id)
                .flex_none()
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(5.0))
                .text_size(px(12.0))
                .text_color(hsla(s.text))
                .hover(|el| el.bg(hsla(s.panel)))
                .cursor_pointer()
                .child(SharedString::from(if SHORTCUT_HINTS {
                    format!("{text}  {hint}")
                } else {
                    text.to_owned()
                }))
        };
        let new_button = button("new-terminal", "+ shell", "⌘T").on_click(cx.listener(
            |this, _ev, window, cx| {
                if let Some(canvas) = &this.canvas {
                    canvas.update(cx, |c, cx| c.new_terminal(&NewTerminal, window, cx));
                }
            },
        ));
        let note_button =
            button("new-note", "+ note", "⌘⇧N").on_click(cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = &this.canvas {
                    canvas.update(cx, |c, cx| c.new_note(&NewNote, window, cx));
                }
            }));
        let fit_button =
            button("fit-all", "fit", "⌘1").on_click(cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = &this.canvas {
                    canvas.update(cx, |c, cx| c.fit_all(&FitAll, window, cx));
                }
            }));
        div()
            .h(px(TOP_BAR) + safe_top)
            .pt(safe_top)
            .w_full()
            .flex()
            .items_center()
            .gap(px(12.0))
            .pl(px(LEADING_INSET))
            .pr(px(12.0))
            .bg(hsla(s.canvas))
            .border_b_1()
            .border_color(hsla(s.border))
            .font_family(self.theme.typography.ui_family.clone())
            // The host name gives way first: a phone's bar must keep every button.
            .child(
                div()
                    .flex_shrink(1.0)
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(12.5))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(if self.host_name.is_empty() {
                        "Slopty".to_owned()
                    } else {
                        self.host_name.clone()
                    })),
            )
            .child(label(self.status.clone()))
            .child(div().flex_1())
            .child(match (self.silent, self.rtt, self.relayed) {
                (Some(gap), _, _) => label(format!("host silent {}s", gap.as_secs()))
                    .text_color(hsla(self.theme.terminal.palette(3))),
                (None, Some(d), Some(true)) => {
                    label(format!("{:.1} ms via relay", d.as_secs_f64() * 1e3))
                }
                (None, Some(d), _) => label(format!("{:.1} ms", d.as_secs_f64() * 1e3)),
                (None, None, _) => label(String::new()),
            })
            .child(label(format!("{zoom_pct}%")))
            .child(fit_button)
            .child(new_button)
            .child(note_button)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surfaces = &self.theme.surfaces;
        // Notch / Dynamic Island, home indicator and the soft keyboard on iOS; zero on macOS.
        let insets = window.insets().effective();
        let body = match (&self.canvas, &self.pairing) {
            (Some(canvas), _) => div().flex_1().w_full().child(canvas.clone()),
            (None, Some(pairing)) => div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.0))
                .child(self.pairing_panel(pairing, cx)),
            (None, None) => div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.0))
                .text_color(hsla(surfaces.text_muted))
                .child(SharedString::from(self.status.clone())),
        };
        let key_bar = KEY_BAR
            .then(|| self.canvas.as_ref()?.read(cx).active_terminal())
            .flatten()
            .map(|terminal| self.key_bar(&terminal, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(surfaces.canvas))
            .child(self.top_bar(insets.top, cx))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_col()
                    .pl(insets.left)
                    .pr(insets.right)
                    .pb(insets.bottom)
                    .child(body)
                    .when_some(key_bar, gpui::ParentElement::child),
            )
    }
}

/// Open the workspace window and start the host link loop on `handle`'s runtime. Call once
/// from inside the GPUI application callback, after `gpui_kit::init`.
///
/// # Errors
///
/// When the window cannot be opened.
pub fn open_workspace(
    cx: &mut App,
    handle: tokio::runtime::Handle,
    options: WindowOptions,
) -> anyhow::Result<()> {
    if let Err(e) = slopty_ui::fonts::install(cx) {
        tracing::error!(error = %e, "bundled fonts");
    }
    cx.bind_keys(slopty_ui::canvas::key_bindings());
    cx.bind_keys(slopty_ui::terminal::key_bindings());
    // gpui-kit widgets (the pairing input) follow their own theme; Slopty is dark only.
    gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
    let theme = Theme::default();
    let (paired_tx, mut paired_rx) = tokio::sync::mpsc::unbounded_channel();
    let workspace = cx.new(|_cx| Workspace {
        canvas: None,
        status: "connecting…".to_owned(),
        host_name: String::new(),
        zoom: 1.0,
        rtt: None,
        relayed: None,
        silent: None,
        theme: theme.clone(),
        subscriptions: Vec::new(),
        pairing: None,
        runtime: handle.clone(),
        paired: paired_tx,
    });
    let root_view = workspace.clone();
    let window =
        cx.open_window(options, move |window, cx| cx.new(|cx| Root::new(root_view, window, cx)))?;

    // Connect on the tokio side; hand the link's channels to GPUI. Runs forever: a lost
    // connection (host restart, network change) is retried with a capped backoff.
    cx.spawn(async move |cx| {
        let mut failures: u32 = 0;
        loop {
            // Unpaired: show the pairing panel and wait for it to succeed.
            if !net::is_paired().unwrap_or(false) {
                let _shown = window.update(cx, |_root, window, cx| {
                    workspace.update(cx, |ws, cx| ws.show_pairing(window, cx));
                });
                if paired_rx.recv().await.is_none() {
                    break;
                }
                failures = 0;
            }
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            handle.spawn(async move {
                let _sent = ready_tx.send(net::connect_host().await);
            });
            let outcome = ready_rx.await;
            let Ok(Ok(connected)) = outcome else {
                let why = match outcome {
                    Ok(Err(e)) => format!("{e:#}"),
                    _ => "connection task died".to_owned(),
                };
                failures = failures.saturating_add(1);
                let delay = retry_delay(failures);
                workspace.update(cx, |ws, cx| {
                    ws.status = format!("{why}; retrying in {}s", delay.as_secs());
                    cx.notify();
                });
                cx.background_executor().timer(delay).await;
                continue;
            };
            failures = 0;
            let net::Connected { me, ack, sender, mut events, link, endpoint } = connected;
            let link = std::sync::Arc::new(link);
            let screen_link = std::sync::Arc::clone(&link);
            let open_screen: slopty_ui::screen::ScreenFactory =
                std::sync::Arc::new(move |stream, codec| screen_link.screen(stream, codec));
            let canvas = workspace.update(cx, |ws, cx| {
                let theme = ws.theme.clone();
                let sessions = ack.sessions.clone();
                let canvas =
                    cx.new(|cx| CanvasView::new(me, sender, sessions, open_screen, theme, cx));
                ws.subscriptions.push(cx.subscribe(
                    &canvas,
                    |ws, _canvas, event, cx| match event {
                        CanvasEvent::Zoom(z) => {
                            ws.zoom = *z;
                            cx.notify();
                        }
                        CanvasEvent::Attention(_session) => slopty_platform::attention(),
                        CanvasEvent::Bell(_session) => {}
                    },
                ));
                ws.canvas = Some(canvas.clone());
                ws.host_name.clone_from(&ack.name);
                "connected".clone_into(&mut ws.status);
                cx.notify();
                canvas
            });
            let focus_canvas = canvas.clone();
            let _focused = window.update(cx, move |_root, window, cx| {
                let handle = focus_canvas.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            });
            // Once a second: RTT for the top bar and the predictors, and a liveness check. QUIC
            // keep-alives make the host send something every few seconds; when the received
            // datagram count stops moving the host is gone or unreachable, and the bar says so
            // long before the transport's idle timeout drops the connection.
            let rtt_link = std::sync::Arc::downgrade(&link);
            let rtt_canvas = canvas.clone();
            let rtt_workspace = workspace.clone();
            cx.spawn(async move |cx| {
                let mut heard = (0_u64, std::time::Instant::now());
                loop {
                    cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                    let Some(link) = rtt_link.upgrade() else { break };
                    let rtt = link.rtt();
                    let relayed = link.relayed();
                    let received = link.received_datagrams();
                    if received != heard.0 {
                        heard = (received, std::time::Instant::now());
                    }
                    let gap = heard.1.elapsed();
                    let silent = (gap >= SILENCE_WARN).then_some(gap);
                    tracing::debug!(paths = %link.paths(), received, "link paths");
                    if gap >= SILENCE_DROP {
                        tracing::warn!(?gap, paths = %link.paths(), "host silent; reconnecting");
                        link.abandon("host silent");
                        break;
                    }
                    rtt_canvas.update(cx, |c, cx| c.set_rtt(rtt, cx));
                    rtt_workspace.update(cx, |ws, cx| {
                        ws.rtt = rtt;
                        ws.relayed = relayed;
                        ws.silent = silent;
                        cx.notify();
                    });
                }
            })
            .detach();
            let mut first_snapshot = true;
            tracing::debug!(sessions = ack.sessions.len(), "link up; pumping events");
            while let Some(event) = events.recv().await {
                tracing::trace!(?event, "link event");
                match event {
                    LinkEvent::Term { session, event } => {
                        canvas.update(cx, |c, cx| c.term_event(session, event, cx));
                    }
                    LinkEvent::Control(HostMsg::Canvas(sync)) => {
                        let is_snapshot =
                            matches!(sync, slopty_proto::canvas::CanvasSync::Snapshot { .. });
                        canvas.update(cx, |c, cx| c.apply_sync(sync, cx));
                        if is_snapshot && first_snapshot {
                            first_snapshot = false;
                            // First run: an empty canvas gets one shell so there is something
                            // to type into. Otherwise bring the existing layout into view.
                            let _opened = window.update(cx, |_root, window, cx| {
                                canvas.update(cx, |c, cx| {
                                    if c.is_empty() {
                                        c.new_terminal(&NewTerminal, window, cx);
                                    } else {
                                        c.fit_when_painted();
                                    }
                                });
                            });
                        }
                    }
                    LinkEvent::Control(HostMsg::SessionOpened(summary)) => {
                        canvas.update(cx, |c, cx| c.session_opened(summary, cx));
                    }
                    LinkEvent::Control(HostMsg::SessionClosed { session, .. }) => {
                        canvas.update(cx, |c, cx| c.session_closed(session, cx));
                    }
                    LinkEvent::Control(HostMsg::Term { session, event }) => {
                        canvas.update(cx, |c, cx| c.term_event(session, event, cx));
                    }
                    LinkEvent::Control(HostMsg::Screen(event)) => {
                        canvas.update(cx, |c, cx| c.screen_event(event, cx));
                    }
                    LinkEvent::Control(HostMsg::Agent(event)) => {
                        canvas.update(cx, |c, cx| c.agent_event(event, cx));
                    }
                    LinkEvent::Control(_) => {}
                    LinkEvent::Disconnected(why) => {
                        workspace.update(cx, |ws, cx| {
                            ws.canvas = None;
                            ws.status = format!("disconnected: {why}; reconnecting…");
                            cx.notify();
                        });
                        break;
                    }
                }
            }
            drop(link);
            // iroh's close uses tokio timers, which need a runtime context this GPUI task
            // does not have; run it on the runtime and wait for the join.
            let _closed = handle.spawn(async move { endpoint.close().await }).await;
            cx.background_executor().timer(retry_delay(0)).await;
        }
    })
    .detach();
    Ok(())
}

/// Backoff between connection attempts: 1 s after a drop, doubling per failure, capped.
fn retry_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((1_u64 << failures.min(4)).min(10))
}
