//! The self-test socket: drive and observe the running app from inside GPUI.
//!
//! Listens on `$SLOPTY_TEST_SOCKET` (only when set; a bundle launched from Finder never
//! has it) and answers `slopty_e2e::Command`s. Keys go through `Window::dispatch_keystroke`
//! and pointer events through `Window::dispatch_event`, exactly the path a real keyboard and
//! mouse take once AppKit has translated them; state comes back as a `Dump` read straight
//! from the entities; a `Render` asks GPUI's own Metal renderer for the frame (built with
//! the `e2e` feature). No system event is posted and no screen is captured, so the whole
//! client is testable on a machine that has granted nothing.
//!
//! The listener runs on the tokio runtime; each command crosses to the GPUI thread through a
//! channel and is applied inside `WindowHandle::update`, so the app sees it like any other
//! foreground work.
//!
//! The `Ui*` commands are the exception: on iOS they are handed to the fork's `gpui_ios::inject`
//! with no window leased, which runs the metal view's own `pressesBegan:` / `touchesBegan:` /
//! pinch / `insertText:` delivery from a *description* of the UIKit object (the values the
//! callback would have read out of it), so the socket proves the phone's input path and not
//! only GPUI's. macOS answers them with an error.

use std::path::PathBuf;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Entity, Focusable as _, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollDelta,
    ScrollWheelEvent, TouchPhase, Window, point, px, size,
};
use slopty_core::{ItemId, SessionId};
use slopty_e2e::{
    Button, Command, ConversationInfo, Dump, FaceInfo, FrameInfo, HostInfo, ItemInfo, LatencyInfo,
    Reply, ScreenInfo, TerminalInfo, WindowInfo,
};
use slopty_proto::agent::{
    AgentSource, AgentStatus, BlockReason, DiffKind, TodoStatus, ToolDetail, TranscriptBody,
    TranscriptEntry,
};
use slopty_proto::canvas::ItemKind;
use slopty_ui::canvas::KeyTarget;
use slopty_ui::screen::ScreenView;
use slopty_ui::terminal::conversation::Attention;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::{Workspace, net};

/// A command with the channel its reply goes back on.
type Request = (Command, oneshot::Sender<Reply>);

/// Start serving `socket` for `workspace` in `window`. Call once, after the window is open.
pub fn serve(
    socket: PathBuf,
    workspace: Entity<Workspace>,
    window: AnyWindowHandle,
    runtime: &tokio::runtime::Handle,
    cx: &App,
) {
    let (tx, mut rx) = mpsc::channel::<Request>(16);
    runtime.spawn(listen(socket, tx));
    let handle = runtime.clone();
    cx.spawn(async move |cx| {
        while let Some((command, reply)) = rx.recv().await {
            let answer = match command {
                Command::Pair { ticket } => {
                    let (done_tx, done_rx) = oneshot::channel();
                    handle.spawn(async move {
                        let _sent = done_tx.send(net::pair_host(&ticket).await);
                    });
                    match done_rx.await {
                        Ok(Ok(net::Paired { id, name })) => {
                            workspace.update(cx, |ws, cx| {
                                ws.pairing = None;
                                ws.add_host(id, name, cx);
                                ws.active = Some(id);
                                cx.notify();
                            });
                            Reply::Ok
                        }
                        Ok(Err(e)) => error(&e),
                        Err(_dropped) => Reply::Error { message: "pairing task died".into() },
                    }
                }
                Command::Render { path } => {
                    // The frame must be laid out with everything dispatched so far; render
                    // on the next frame so `Dump` and `Render` agree.
                    let (done_tx, done_rx) = oneshot::channel();
                    // `update_window` (not `WindowHandle::update`): the root view must not be
                    // leased while a dispatched action updates it.
                    let scheduled = cx.update_window(window, |_root, window, _cx| {
                        window.refresh();
                        window.on_next_frame(move |window, _cx| {
                            let _sent = done_tx.send(render(window, &path));
                        });
                    });
                    match scheduled {
                        Ok(()) => done_rx.await.unwrap_or_else(|_| Reply::Error {
                            message: "window closed before the frame".into(),
                        }),
                        Err(e) => error(&e),
                    }
                }
                Command::Quit => {
                    let _sent = reply.send(Reply::Ok);
                    cx.update(|cx| cx.quit());
                    continue;
                }
                // At the UIKit boundary, from the async task (no window leased): the fork's
                // delivery re-enters GPUI's dispatch exactly as a UIKit callback does. Then
                // settle on a frame like any input.
                Command::UiKeyPress { .. }
                | Command::UiTouch { .. }
                | Command::UiPinch { .. }
                | Command::UiInsertText { .. }
                | Command::UiDeleteBackward => match uikit::inject(command) {
                    Ok(()) => after_frame(window, Reply::Ok, cx).await,
                    Err(message) => Reply::Error { message },
                },
                // Input settles on the next frame: focus moved by a click is only in the
                // dispatch tree once it has been drawn, so a keystroke sent before that would
                // go nowhere. Reply after the frame, and the driver never races the app.
                other => {
                    let mut answer = None;
                    let applied = cx.update_window(window, |_root, window, cx| {
                        answer = Some(apply(&workspace, other, window, cx));
                    });
                    match (applied, answer) {
                        (Ok(()), Some(answer)) => after_frame(window, answer, cx).await,
                        (Ok(()), None) => Reply::Error { message: "not applied".into() },
                        (Err(e), _) => error(&e),
                    }
                }
            };
            let _sent = reply.send(answer);
        }
    })
    .detach();
}

/// `answer`, once the window has drawn a frame with everything dispatched so far.
async fn after_frame(window: AnyWindowHandle, answer: Reply, cx: &mut gpui::AsyncApp) -> Reply {
    let (done_tx, done_rx) = oneshot::channel();
    let scheduled = cx.update_window(window, |_root, window, _cx| {
        window.refresh();
        window.on_next_frame(move |_window, _cx| {
            let _sent = done_tx.send(answer);
        });
    });
    match scheduled {
        Ok(()) => done_rx
            .await
            .unwrap_or_else(|_| Reply::Error { message: "window closed before the frame".into() }),
        Err(e) => error(&e),
    }
}

/// The `Ui*` commands: UIKit-boundary delivery on iOS, an error elsewhere.
mod uikit {
    use slopty_e2e::Command;

    /// `UIKeyModifierFlags`, from GPUI's modifier names.
    const ALPHA_SHIFT: u32 = 1 << 16;
    const SHIFT: u32 = 1 << 17;
    const CONTROL: u32 = 1 << 18;
    const ALTERNATE: u32 = 1 << 19;
    const COMMAND: u32 = 1 << 20;

    /// The flag word for `cmd-shift`-style modifiers (empty for none).
    ///
    /// # Errors
    ///
    /// Names a modifier GPUI does not have.
    pub fn modifier_flags(modifiers: &str) -> Result<u32, String> {
        let mut flags = 0;
        for name in modifiers.split('-').filter(|m| !m.is_empty()) {
            flags |= match name {
                "cmd" | "command" | "platform" | "super" => COMMAND,
                "ctrl" | "control" => CONTROL,
                "alt" | "option" => ALTERNATE,
                "shift" => SHIFT,
                "capslock" => ALPHA_SHIFT,
                other => return Err(format!("unknown modifier {other:?}")),
            };
        }
        Ok(flags)
    }

    /// Deliver `command` at the UIKit boundary.
    ///
    /// # Errors
    ///
    /// Off iOS, with a bad modifier, or when the fork has no window.
    #[cfg(all(target_os = "ios", feature = "e2e"))]
    pub fn inject(command: Command) -> Result<(), String> {
        use gpui_ios::described::{
            DescribedInput, DescribedPinch, DescribedPress, DescribedTouch, GestureState,
            PressPhase, UiTouchPhase,
        };
        use slopty_e2e::{UiGesturePhase, UiPressPhase, UiTouchPhase as Phase};

        let input = match command {
            Command::UiKeyPress { usage, modifiers, phase } => {
                DescribedInput::Press(DescribedPress {
                    usage,
                    flags: modifier_flags(&modifiers)?,
                    phase: match phase {
                        UiPressPhase::Began => PressPhase::Began,
                        UiPressPhase::Ended => PressPhase::Ended,
                        UiPressPhase::Cancelled => PressPhase::Cancelled,
                    },
                })
            }
            Command::UiTouch { touches, phase } => {
                let phase = match phase {
                    Phase::Began => UiTouchPhase::Began,
                    Phase::Moved => UiTouchPhase::Moved,
                    Phase::Stationary => UiTouchPhase::Stationary,
                    Phase::Ended => UiTouchPhase::Ended,
                    Phase::Cancelled => UiTouchPhase::Cancelled,
                };
                DescribedInput::Touches(
                    touches
                        .into_iter()
                        .map(|t| DescribedTouch { id: t.id, phase, x: t.x, y: t.y })
                        .collect(),
                )
            }
            Command::UiPinch { scale, x, y, phase } => DescribedInput::Pinch(DescribedPinch {
                state: match phase {
                    UiGesturePhase::Began => GestureState::Began,
                    UiGesturePhase::Changed => GestureState::Changed,
                    UiGesturePhase::Ended => GestureState::Ended,
                    UiGesturePhase::Cancelled => GestureState::Cancelled,
                },
                scale,
                x,
                y,
            }),
            Command::UiInsertText { text } => DescribedInput::InsertText(text),
            Command::UiDeleteBackward => DescribedInput::DeleteBackward,
            other => return Err(format!("not a UIKit command: {other:?}")),
        };
        gpui_ios::inject(input).map_err(|e| e.to_string())
    }

    /// Off iOS (or without the `e2e` feature) nothing sits at a UIKit boundary.
    #[cfg(not(all(target_os = "ios", feature = "e2e")))]
    #[expect(clippy::needless_pass_by_value, reason = "the iOS twin consumes it")]
    pub fn inject(command: Command) -> Result<(), String> {
        let _ = modifier_flags;
        Err(format!("{command:?}: UIKit injection is iOS only (with the e2e feature)"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn modifier_names_become_uikey_flags() {
            assert_eq!(modifier_flags(""), Ok(0));
            assert_eq!(modifier_flags("cmd-shift"), Ok(COMMAND | SHIFT));
            assert_eq!(modifier_flags("ctrl"), Ok(CONTROL));
            assert_eq!(modifier_flags("alt-capslock"), Ok(ALTERNATE | ALPHA_SHIFT));
            assert!(modifier_flags("fn").is_err());
        }
    }
}

fn error(e: &dyn std::fmt::Display) -> Reply {
    Reply::Error { message: format!("{e:#}") }
}

/// Accept connections and relay each line as a command; one reply line per command.
async fn listen(socket: PathBuf, tx: mpsc::Sender<Request>) {
    if socket.exists()
        && let Err(e) = std::fs::remove_file(&socket)
    {
        tracing::warn!(path = %socket.display(), error = %e, "stale test socket");
    }
    let listener = match UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(path = %socket.display(), error = %e, "bind test socket");
            return;
        }
    };
    tracing::info!(path = %socket.display(), "test socket listening");
    loop {
        let Ok((stream, _addr)) = listener.accept().await else { break };
        let tx = tx.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let answer = match serde_json::from_str::<Command>(&line) {
                    Ok(command) => {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if tx.send((command, reply_tx)).await.is_err() {
                            break;
                        }
                        reply_rx.await.unwrap_or_else(|_| Reply::Error {
                            message: "app dropped the command".into(),
                        })
                    }
                    Err(e) => Reply::Error { message: format!("bad command: {e}") },
                };
                let Ok(mut out) = serde_json::to_string(&answer) else { break };
                out.push('\n');
                if write.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

/// Apply a synchronous command inside the window.
fn apply(
    workspace: &Entity<Workspace>,
    command: Command,
    window: &mut Window,
    cx: &mut App,
) -> Reply {
    match command {
        Command::Ping => Reply::Ok,
        Command::Keys { keys } => {
            for text in keys.split_whitespace() {
                match Keystroke::parse(text) {
                    Ok(keystroke) => {
                        let handled = window.dispatch_keystroke(keystroke, cx);
                        tracing::debug!(keystroke = text, handled, "keys");
                    }
                    Err(e) => return Reply::Error { message: format!("{text:?}: {e}") },
                }
            }
            Reply::Ok
        }
        Command::Attach { media_type, data } => {
            let Ok(data) = data_encoding::BASE64.decode(data.as_bytes()) else {
                return Reply::Error { message: "attach: data is not base64".into() };
            };
            let Some(view) =
                workspace.read(cx).active_canvas().and_then(|c| c.read(cx).active_terminal())
            else {
                return Reply::Error { message: "no active terminal".into() };
            };
            view.update(cx, |view, cx| {
                view.attach_image(slopty_proto::agent::Image { media_type, data }, cx);
            });
            Reply::Ok
        }
        Command::Type { text } => {
            for ch in text.chars() {
                let keystroke = Keystroke {
                    modifiers: Modifiers { shift: ch.is_uppercase(), ..Modifiers::default() },
                    key: ch.to_lowercase().to_string(),
                    key_char: Some(ch.to_string()),
                };
                let _handled = window.dispatch_keystroke(keystroke, cx);
            }
            Reply::Ok
        }
        Command::Click { x, y, button, count } => {
            let position = point(px(x), px(y));
            let button = mouse_button(button);
            let click_count = usize::try_from(count).unwrap_or(1).max(1);
            let _moved = window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            let _down = window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position,
                    modifiers: Modifiers::default(),
                    click_count,
                    first_mouse: false,
                }),
                cx,
            );
            let _up = window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button,
                    position,
                    modifiers: Modifiers::default(),
                    click_count,
                }),
                cx,
            );
            Reply::Ok
        }
        Command::Drag { x, y, to_x, to_y } => {
            let (from, to) = (point(px(x), px(y)), point(px(to_x), px(to_y)));
            let button = MouseButton::Left;
            let _down = window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position: from,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            let _moved = window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: to,
                    pressed_button: Some(button),
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            let _up = window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button,
                    position: to,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
            Reply::Ok
        }
        Command::Move { x, y } => {
            let _moved = window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(px(x), px(y)),
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            Reply::Ok
        }
        Command::Scroll { x, y, dx, dy, zoom } => {
            let _scrolled = window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: point(px(x), px(y)),
                    delta: ScrollDelta::Lines(point(dx, dy)),
                    modifiers: Modifiers { platform: zoom, ..Modifiers::default() },
                    touch_phase: TouchPhase::Moved,
                }),
                cx,
            );
            Reply::Ok
        }
        Command::Open { command, count } => {
            let Some(canvas) = workspace.read(cx).active_canvas() else {
                return Reply::Error { message: "no active canvas".into() };
            };
            canvas.update(cx, |canvas, cx| {
                for _ in 0..count {
                    canvas.open_command(command.clone(), cx);
                }
            });
            Reply::Ok
        }
        Command::OpenAgent { cwd, resume } => {
            let Some(canvas) = workspace.read(cx).active_canvas() else {
                return Reply::Error { message: "no active canvas".into() };
            };
            canvas.update(cx, |canvas, cx| match resume {
                None => canvas.open_agent(cwd, cx),
                Some(id) => canvas.open_agent_with(
                    slopty_proto::agent::OpenAgent {
                        cwd,
                        resume: Some(id),
                        model: None,
                        title: Some("resumed".to_owned()),
                    },
                    cx,
                ),
            });
            Reply::Ok
        }
        Command::FramesReset => {
            slopty_ui::frames::reset(cx);
            Reply::Ok
        }
        Command::Reveal { session } => {
            let Ok(session) = session.parse::<SessionId>() else {
                return Reply::Error { message: format!("not a session id: {session}") };
            };
            let Some(canvas) = workspace.read(cx).active_canvas() else {
                return Reply::Error { message: "no active canvas".into() };
            };
            canvas.update(cx, |canvas, cx| canvas.reveal_session(session, cx));
            Reply::Ok
        }
        Command::AddDisplay => {
            let Some(canvas) = workspace.read(cx).active_canvas() else {
                return Reply::Error { message: "no active canvas".into() };
            };
            canvas.update(cx, slopty_ui::canvas::CanvasView::add_first_display);
            Reply::Ok
        }
        Command::NotificationResponse { tag, action } => {
            let Ok(session) = tag.parse::<SessionId>() else {
                return Reply::Error { message: format!("not a session id: {tag}") };
            };
            workspace.update(cx, |ws, cx| {
                ws.notification_response(session, action.as_deref(), window, cx);
            });
            Reply::Ok
        }
        Command::Resize { width, height } => {
            window.resize(size(px(width), px(height)));
            Reply::Ok
        }
        Command::Dump => Reply::Dump(Box::new(workspace.read(cx).dump(window, cx))),
        Command::Pair { .. }
        | Command::Render { .. }
        | Command::Quit
        | Command::UiKeyPress { .. }
        | Command::UiTouch { .. }
        | Command::UiPinch { .. }
        | Command::UiInsertText { .. }
        | Command::UiDeleteBackward => Reply::Error { message: "handled elsewhere".into() },
    }
}

const fn mouse_button(button: Button) -> MouseButton {
    match button {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
    }
}

#[cfg(feature = "e2e")]
fn render(window: &Window, path: &str) -> Reply {
    match window.render_to_image() {
        Ok(image) => {
            let (width, height) = image.dimensions();
            match image.save(path) {
                Ok(()) => Reply::Rendered { width, height },
                Err(e) => Reply::Error { message: format!("write {path}: {e}") },
            }
        }
        Err(e) => Reply::Error { message: format!("render: {e:#}") },
    }
}

#[cfg(not(feature = "e2e"))]
fn render(_window: &Window, _path: &str) -> Reply {
    Reply::Error { message: "built without the `e2e` feature; no renderer access".into() }
}

/// The accessibility tree of the last frame, trimmed for the dump.
#[cfg(feature = "e2e")]
fn a11y_nodes(window: &Window) -> Vec<slopty_e2e::A11yNode> {
    slopty_ui::a11y::tree(window)
        .into_iter()
        .map(|n| slopty_e2e::A11yNode {
            role: n.role,
            label: n.label,
            value: n.value,
            focused: n.focused,
            bounds: n.bounds,
        })
        .collect()
}

#[cfg(not(feature = "e2e"))]
const fn a11y_nodes(_window: &Window) -> Vec<slopty_e2e::A11yNode> {
    Vec::new()
}

/// The agent's state as one word, as the dump lists it.
fn agent_line(status: &AgentStatus) -> String {
    match status {
        AgentStatus::None => "none".to_owned(),
        AgentStatus::Idle => "idle".to_owned(),
        AgentStatus::Working => "working".to_owned(),
        AgentStatus::Tool { tool } => format!("tool:{tool}"),
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            format!("blocked:permission:{tool}")
        }
        AgentStatus::Blocked(BlockReason::Question) => "blocked:question".to_owned(),
        AgentStatus::Blocked(BlockReason::Elicitation) => "blocked:elicitation".to_owned(),
        AgentStatus::Blocked(BlockReason::IdlePrompt) => "blocked:idle".to_owned(),
        AgentStatus::Done => "done".to_owned(),
    }
}

/// Everything a test may want to know about one remote window, timings in microseconds.
fn screen_info(item: ItemId, view: &ScreenView) -> ScreenInfo {
    let us = |d: std::time::Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    let pacing = view.pacing();
    let size = view.size();
    let s = view.stats();
    let source = match view.source_state() {
        slopty_proto::screen::SourceState::Idle => "idle",
        slopty_proto::screen::SourceState::Live => "live",
    };
    let recovery = slopty_e2e::RecoveryInfo {
        frames: s.frames,
        frames_fec: s.frames_fec,
        frames_retransmit: s.frames_retransmit,
        frames_lost: s.frames_lost,
        datagrams_lost: s.datagrams_lost,
        data_shards: s.data_shards,
        parity_shards: s.parity_shards,
        parity_permille: s.parity_permille,
        nacks: s.nacks,
        refreshes: s.refreshes,
        datagrams: s.datagrams,
        bytes: s.bytes,
        stalls: s.stalls,
        stalled_ms: s.stalled_ms,
        audio_packets: s.audio_packets,
        audio_lost: s.audio_lost,
        audio_concealed: s.audio_concealed,
    };
    ScreenInfo {
        recovery,
        source: source.to_owned(),
        item: item.to_string(),
        stream: view.stream().0,
        size: size.into(),
        frames: view.frames(),
        presented: pacing.presented,
        skipped: pacing.skipped,
        repeats: pacing.repeats,
        late: pacing.late,
        latency_p50_us: us(pacing.latency_p50),
        latency_p95_us: us(pacing.latency_p95),
        latency_max_us: us(pacing.latency_max),
        decode_p50_us: us(pacing.decode_p50),
        interval_p50_us: us(pacing.interval_p50),
        interval_jitter_us: us(pacing.interval_jitter),
        window: pacing.window,
    }
}

/// The frame probe's numbers, microseconds.
fn frame_info(stats: Option<slopty_ui::frames::FrameStats>) -> FrameInfo {
    let us = |d: std::time::Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    stats.map_or_else(FrameInfo::default, |s| FrameInfo {
        frames: s.frames,
        over_budget: s.over_budget,
        dropped: s.dropped,
        draw_p50_us: us(s.draw_p50),
        draw_p95_us: us(s.draw_p95),
        draw_p99_us: us(s.draw_p99),
        draw_max_us: us(s.draw_max),
        interval_p50_us: us(s.interval_p50),
        interval_p95_us: us(s.interval_p95),
        interval_p99_us: us(s.interval_p99),
        nominal_us: us(s.nominal),
    })
}

/// A terminal's keystroke → paint numbers, microseconds.
fn latency_info(s: slopty_ui::terminal::latency::LatencyStats) -> LatencyInfo {
    let us = |d: std::time::Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    LatencyInfo {
        echoed: s.echoed,
        echo_p50_us: us(s.echo_p50),
        echo_p95_us: us(s.echo_p95),
        echo_max_us: us(s.echo_max),
        predicted: s.predicted,
        predicted_p50_us: us(s.predicted_p50),
        predicted_p95_us: us(s.predicted_p95),
        predicted_max_us: us(s.predicted_max),
    }
}

/// One line for a conversation entry, as the dump lists them.
fn entry_line(entry: &TranscriptEntry) -> String {
    let first = |text: &str| text.lines().next().unwrap_or_default().to_owned();
    match &entry.body {
        TranscriptBody::User { text, images } if *images > 0 => {
            format!("user: {} [+{images}]", first(text))
        }
        TranscriptBody::User { text, .. } => format!("user: {}", first(text)),
        TranscriptBody::Assistant { markdown } => format!("assistant: {}", first(markdown)),
        TranscriptBody::Thinking { .. } => "thinking".to_owned(),
        TranscriptBody::ToolUse { name, summary, detail, .. } => match detail {
            ToolDetail::Diff { lines, .. } => {
                let added = lines.iter().filter(|l| l.kind == DiffKind::Added).count();
                let removed = lines.iter().filter(|l| l.kind == DiffKind::Removed).count();
                format!("tool {name}: {summary} (+{added} -{removed})")
            }
            ToolDetail::Todos { items } => {
                let done = items.iter().filter(|t| t.status == TodoStatus::Completed).count();
                format!("tool {name}: {done}/{} done", items.len())
            }
            _ => format!("tool {name}: {summary}"),
        },
        TranscriptBody::ToolResult { tool, output, is_error } => format!(
            "result {}{}: {}",
            tool.as_deref().unwrap_or("?"),
            if *is_error { " failed" } else { "" },
            first(&output.text)
        ),
    }
}

impl Workspace {
    /// Everything a test may want to know, read from the entities.
    fn dump(&self, window: &Window, cx: &App) -> Dump {
        let viewport = window.viewport_size();
        let hosts = self
            .hosts
            .iter()
            .map(|h| HostInfo {
                name: h.name.clone(),
                status: h.status.text(),
                active: self.active == Some(h.id),
                needs_you: h.needs_you,
                rtt_us: h.rtt.map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX)),
                relayed: h.relayed,
            })
            .collect();
        let mut dump = Dump {
            a11y: a11y_nodes(window),
            window: WindowInfo {
                width: f32::from(viewport.width),
                height: f32::from(viewport.height),
                scale: window.scale_factor(),
                active: window.is_window_active(),
            },
            hosts,
            pairing: self.pairing.is_some(),
            status: self.status_text(),
            notice: self.notice.as_ref().map(|(_seq, text)| text.clone()),
            frames: frame_info(slopty_ui::frames::stats(cx)),
            ..Dump::default()
        };
        let mut focused = if window.focused(cx).is_some() { "other" } else { "none" }.to_owned();
        let Some(canvas) = self.active_canvas() else {
            dump.focused = focused;
            return dump;
        };
        let canvas = canvas.read(cx);
        dump.zoom = canvas.zoom();
        dump.client = canvas.me().to_string();
        dump.hooks_offered = canvas.hooks_offered();
        if canvas.focus_handle(cx).is_focused(window) {
            focused = String::from("canvas");
        }
        dump.focus = canvas.active_key_target().map(|t| match t {
            KeyTarget::Terminal(_) => "terminal".to_owned(),
            KeyTarget::Screen(_) => "screen".to_owned(),
        });
        for item in canvas.items() {
            let (kind, session) = match &item.kind {
                ItemKind::Terminal { session } => ("terminal", Some(session.to_string())),
                ItemKind::Window { .. } => ("window", None),
                ItemKind::Display { .. } => ("display", None),
                ItemKind::Note { .. } => ("note", None),
            };
            let note = match &item.kind {
                ItemKind::Note { text } => Some(text.clone()),
                _ => None,
            };
            let b = canvas.window_bounds(item.rect);
            dump.items.push(ItemInfo {
                id: item.id.to_string(),
                kind: kind.to_owned(),
                session,
                rect: [item.rect.x, item.rect.y, item.rect.w, item.rect.h],
                bounds: [
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                ],
                active: canvas.active_item() == Some(item.id),
                sleeping: item.sleeping,
                note,
            });
            if matches!(item.kind, ItemKind::Window { .. } | ItemKind::Display { .. })
                && let Some(view) = canvas.screen(item.id)
            {
                let view = view.read(cx);
                if view.focus_handle(cx).is_focused(window) {
                    focused = format!("screen:{}", view.stream().0);
                }
                dump.screens.push(screen_info(item.id, view));
            }
            if let ItemKind::Terminal { session } = item.kind
                && let Some(view) = canvas.terminal(session)
            {
                let view = view.read(cx);
                if view.focus_handle(cx).is_focused(window) {
                    focused = format!("terminal:{session}");
                }
                let size = view.size();
                let cursor = view.cursor();
                let conversation = view.conversation().map(|c| ConversationInfo {
                    entries: c.entries().iter().map(entry_line).collect(),
                    composer: c.composer_text(cx),
                    composer_focused: c.composer_focus(cx).is_focused(window),
                    pinned: c.pinned(),
                    partial: view.partial().to_owned(),
                    permission: view.permission().map(|p| format!("{}:{}", p.tool, p.summary)),
                    always: view.permission().and_then(|p| p.always.clone()),
                    usage: view
                        .info()
                        .usage
                        .as_ref()
                        .map(slopty_ui::terminal::conversation::usage_label),
                    attachments: view
                        .attachments()
                        .iter()
                        .map(slopty_ui::terminal::conversation::attachment_label)
                        .collect(),
                    tasks: c
                        .tasks()
                        .iter()
                        .map(|t| {
                            format!(
                                "{}:{}:{}:{}",
                                t.call,
                                t.description,
                                t.tool_uses,
                                if t.done { "done" } else { "running" }
                            )
                        })
                        .collect(),
                    model: view.info().model.clone(),
                    permission_mode: view.info().permission_mode.clone(),
                    agent_session: view.info().agent_session.clone(),
                    slash_commands: view.info().slash_commands.clone(),
                    turns: view.info().turns,
                    completions: view.completions(cx),
                    model_menu: c.model_menu_open(),
                    attention: view.attention().map(|a| match a {
                        Attention::Permission { tool, answered: None, .. } => {
                            format!("permission:{tool}")
                        }
                        Attention::Permission { answered: Some(true), .. } => "allowed".to_owned(),
                        Attention::Permission { answered: Some(false), .. } => "denied".to_owned(),
                        Attention::Question { answered: false, .. } => "question".to_owned(),
                        Attention::Question { answered: true, .. } => "answered".to_owned(),
                        Attention::Prompt => "prompt".to_owned(),
                    }),
                    question_options: match view.attention() {
                        Some(Attention::Question { questions, answered: false, .. }) => questions
                            .iter()
                            .flat_map(|q| q.options.iter().map(|o| o.label.clone()))
                            .collect(),
                        _ => Vec::new(),
                    },
                });
                dump.terminals.push(TerminalInfo {
                    session: session.to_string(),
                    kind: if view.is_driven() { "agent" } else { "terminal" }.to_owned(),
                    title: Some(canvas.terminal_title(session, cx)),
                    size: [size.cols, size.rows],
                    cursor: [cursor.col, cursor.row],
                    rows: view.rows(),
                    agent: view.agent_status().map(agent_line),
                    agent_source: canvas
                        .agent(session)
                        .map(|a| match a.source {
                            AgentSource::Process => "process",
                            AgentSource::Title => "title",
                            AgentSource::Transcript => "transcript",
                            AgentSource::Hook => "hook",
                            AgentSource::Driven => "driven",
                        })
                        .map(str::to_owned),
                    conversation,
                    latency: latency_info(view.latency()),
                    face: view.metrics().map(|m| face_info(&m)),
                    driving: view.driving(),
                });
            }
        }
        dump.focused = focused;
        dump
    }
}

/// The measured face for the dump.
#[expect(clippy::cast_possible_truncation, reason = "device pixels; f32 carries them")]
fn face_info(m: &slopty_ui::terminal::CellMetrics) -> FaceInfo {
    let f = m.face;
    FaceInfo {
        size: m.face_size,
        cell_width: f.cell_width as f32,
        ascent: f.ascent as f32,
        descent: f.descent as f32,
        line_gap: f.line_gap as f32,
        underline_position: f.underline_position.map(|v| v as f32),
        underline_thickness: f.underline_thickness.map(|v| v as f32),
    }
}
