//! The Slopty macOS app.
//!
//! Connects to the paired host, then shows its canvas: every terminal the host knows about, on
//! one plane shared by all clients. Networking runs on a tokio runtime thread; GPUI owns the
//! main thread. The two talk through channels only.

mod net;

use anyhow::Result;
use gpui::{
    AppContext as _, Bounds, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit::component::Root;
use slopty_client::LinkEvent;
use slopty_proto::HostMsg;
use slopty_theme::Theme;
use slopty_ui::canvas::{CanvasEvent, CanvasView, NewTerminal};
use slopty_ui::colors::hsla;

/// Height of the top bar (the titlebar area; traffic lights sit at its left).
const TOP_BAR: f32 = 38.0;
/// Space reserved for the traffic lights.
const TRAFFIC_LIGHTS: f32 = 78.0;

/// The window's root view.
struct Workspace {
    canvas: Option<Entity<CanvasView>>,
    status: String,
    host_name: String,
    zoom: f32,
    rtt: Option<std::time::Duration>,
    relayed: Option<bool>,
    theme: Theme,
    subscriptions: Vec<gpui::Subscription>,
}

impl Workspace {
    fn top_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let s = &self.theme.surfaces;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0–400")]
        let zoom_pct = (self.zoom * 100.0).round().max(0.0) as u32;
        let label = |text: String| {
            div().text_size(px(12.0)).text_color(hsla(s.text_muted)).child(SharedString::from(text))
        };
        let new_button = div()
            .id("new-terminal")
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(5.0))
            .text_size(px(12.0))
            .text_color(hsla(s.text))
            .hover(|el| el.bg(hsla(s.panel)))
            .cursor_pointer()
            .on_click(cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = &this.canvas {
                    canvas.update(cx, |c, cx| c.new_terminal(&NewTerminal, window, cx));
                }
            }))
            .child("+ shell  ⌘T");
        div()
            .h(px(TOP_BAR))
            .w_full()
            .flex()
            .items_center()
            .gap(px(12.0))
            .pl(px(TRAFFIC_LIGHTS))
            .pr(px(12.0))
            .bg(hsla(s.canvas))
            .border_b_1()
            .border_color(hsla(s.border))
            .font_family(self.theme.typography.ui_family.clone())
            .child(div().text_size(px(12.5)).text_color(hsla(s.text)).child(SharedString::from(
                if self.host_name.is_empty() {
                    "Slopty".to_owned()
                } else {
                    self.host_name.clone()
                },
            )))
            .child(label(self.status.clone()))
            .child(div().flex_1())
            .child(label(match (self.rtt, self.relayed) {
                (Some(d), Some(true)) => format!("{:.1} ms via relay", d.as_secs_f64() * 1e3),
                (Some(d), _) => format!("{:.1} ms", d.as_secs_f64() * 1e3),
                (None, _) => String::new(),
            }))
            .child(label(format!("{zoom_pct}%")))
            .child(new_button)
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surfaces = &self.theme.surfaces;
        let body = match &self.canvas {
            Some(canvas) => div().flex_1().w_full().child(canvas.clone()),
            None => div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.0))
                .text_color(hsla(surfaces.text_muted))
                .child(SharedString::from(self.status.clone())),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(surfaces.canvas))
            .child(self.top_bar(cx))
            .child(body)
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = runtime.handle().clone();
    let theme = Theme::default();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        if let Err(e) = slopty_ui::fonts::install(cx) {
            tracing::error!(error = %e, "bundled fonts");
        }
        cx.bind_keys(slopty_ui::canvas::key_bindings());
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Slopty".into()),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
            }),
            ..Default::default()
        };
        let workspace = cx.new(|_cx| Workspace {
            canvas: None,
            status: "connecting…".to_owned(),
            host_name: String::new(),
            zoom: 1.0,
            rtt: None,
            relayed: None,
            theme: theme.clone(),
            subscriptions: Vec::new(),
        });
        let root_view = workspace.clone();
        let window = match cx
            .open_window(options, move |window, cx| cx.new(|cx| Root::new(root_view, window, cx)))
        {
            Ok(window) => window,
            Err(e) => {
                tracing::error!(error = %e, "open window");
                cx.quit();
                return;
            }
        };

        // Connect on the tokio side; hand the link's channels to GPUI. Runs forever: a lost
        // connection (host restart, network change) is retried with a capped backoff.
        cx.spawn(async move |cx| {
            let mut failures: u32 = 0;
            loop {
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
                    ws.subscriptions.push(cx.subscribe(&canvas, |ws, _canvas, event, cx| {
                        if let CanvasEvent::Zoom(z) = event {
                            ws.zoom = *z;
                            cx.notify();
                        }
                    }));
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
                // RTT readout once a second: the top bar shows it and predictors gate on it.
                let rtt_link = std::sync::Arc::downgrade(&link);
                let rtt_canvas = canvas.clone();
                let rtt_workspace = workspace.clone();
                cx.spawn(async move |cx| {
                    loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                        let Some(link) = rtt_link.upgrade() else { break };
                        let rtt = link.rtt();
                        let relayed = link.relayed();
                        tracing::debug!(paths = %link.paths(), "link paths");
                        rtt_canvas.update(cx, |c, cx| c.set_rtt(rtt, cx));
                        rtt_workspace.update(cx, |ws, cx| {
                            ws.rtt = rtt;
                            ws.relayed = relayed;
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
                                // to type into.
                                let _opened = window.update(cx, |_root, window, cx| {
                                    canvas.update(cx, |c, cx| {
                                        if c.is_empty() {
                                            c.new_terminal(&NewTerminal, window, cx);
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
        cx.activate(true);
    });
    Ok(())
}

/// Backoff between connection attempts: 1 s after a drop, doubling per failure, capped.
fn retry_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((1_u64 << failures.min(4)).min(10))
}
