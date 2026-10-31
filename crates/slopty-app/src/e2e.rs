//! The self-test socket: drive and observe the running app from inside GPUI.
//!
//! Listens on `$SLOPTY_TEST_SOCKET` (only when set; a bundle launched from Finder never
//! has it) and answers `slopty_e2e::Command`s. Keys go through `Window::dispatch_keystroke`
//! and pointer events through `Window::dispatch_event`, each after `gpui::prepare_input` in an
//! update of its own, exactly the path a real keyboard and mouse take once AppKit has
//! translated them; state comes back as a `Dump` read straight
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
    ScrollEvent, TouchPhase, Window, point, px, size,
};
use slopty_core::{ItemId, WorkerId};
use slopty_e2e::{
    Button, Command, Dump, FaceInfo, FrameInfo, ItemInfo, LatencyInfo, Reply, ScreenInfo,
    TerminalInfo, WindowInfo, WorkerInfo,
};
use slopty_proto::items::ItemKind;
use slopty_ui::screen::ScreenView;
use slopty_ui::workspace::WorkerStatus;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::Workspace;

/// A command with the channel its reply goes back on.
type Request = (Command, oneshot::Sender<Reply>);

/// Start serving `socket` for `workspace` in `window`. Call once, after the window is open.
pub(crate) fn serve(
    socket: PathBuf,
    workspace: Entity<Workspace>,
    window: AnyWindowHandle,
    runtime: &tokio::runtime::Handle,
    cx: &App,
) {
    let (tx, mut rx) = mpsc::channel::<Request>(16);
    runtime.spawn(listen(socket, tx));
    cx.spawn(async move |cx| {
        // GPUI's fades run on the wall clock, so a frame and the same state drawn from scratch a
        // few milliseconds later differ while one runs: under Reduce Motion they land at once,
        // and every dump's frame can be judged (`Dump::stale`). The frame-time measurements
        // keep the motion they measure.
        if std::env::var_os("SLOPTY_E2E_MOTION").is_none() {
            cx.update(|cx| slopty_shell::motion::hold(true, cx));
        }
        // The readouts show a local link's round trip, not the one a busy machine measured, so
        // a golden never carries live timing. The dump and the predictors keep the link's own.
        workspace.update(cx, |ws, cx| {
            ws.view.update(cx, |view, cx| view.pin_rtt_readout(Some(slopty_e2e::SHOWN_RTT), cx));
        });
        #[cfg(target_os = "macos")]
        let _pointer = cx.update_window(window, |_root, window, cx| {
            workspace.update(cx, |_ws, cx| pointer::keep(window, cx));
        });
        // A Mac with an assistive app running (a launcher, a window manager, a screen reader)
        // asks for the tree from the first frame and keeps it: every frame is drawn so.
        #[cfg(feature = "e2e")]
        if tree_kept() {
            let _kept = cx.update_window(window, |_root, window, _cx| window.set_a11y_active(true));
        }
        while let Some((command, reply)) = rx.recv().await {
            let answer = match command {
                Command::Render { path, scale } => {
                    // The frame the app draws with everything dispatched so far, as it draws
                    // it: only what was notified is built again. It must be the frame the same
                    // state drawn from scratch gives, or a view is showing an old state.
                    // A stream's picture is on a layer of its own, which the render cannot see:
                    // for this frame each stream draws it with GPUI too, over its layer.
                    cx.update(|cx| slopty_ui::screen::capture_pictures(true, cx));
                    let rendered = on_settled_frame(window, cx, move |window, cx| {
                        render(window, cx, &path, scale)
                    })
                    .await;
                    cx.update(|cx| slopty_ui::screen::capture_pictures(false, cx));
                    rendered
                }
                Command::Dump => {
                    // Every dump also holds the frame the app draws against the same state
                    // drawn from scratch (`Dump::stale`), so any test's dump catches a view
                    // left showing an old state.
                    let workspace = workspace.clone();
                    on_settled_frame(window, cx, move |window, cx| {
                        Reply::Dump(Box::new(dump(&workspace, window, cx)))
                    })
                    .await
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
                | Command::UiPointerClick { .. }
                | Command::UiPinch { .. }
                | Command::UiInsertText { .. }
                | Command::UiDeleteBackward => match uikit::inject(command) {
                    Ok(()) => after_frame(window, Reply::Ok, cx).await,
                    Err(message) => Reply::Error { message },
                },
                // A finger UIKit itself delivers, its moves paced on the app's clock; then a
                // frame, like any input.
                Command::UiFinger { from, to, steps, held_ms } => {
                    match finger(from, to, steps, held_ms, cx).await {
                        Ok(view) => after_frame(window, Reply::Touched { view }, cx).await,
                        Err(message) => Reply::Error { message },
                    }
                }
                // The system's own answers, from the async task: nothing on screen changes.
                Command::PushRegister { token } => push_register(&token).await,
                Command::OpenPush { payload } => open_push(&payload),
                Command::Delivered => {
                    let notes = slopty_platform::notify::delivered().await;
                    Reply::Delivered {
                        notes: notes
                            .into_iter()
                            .map(|n| slopty_e2e::DeliveredNote {
                                id: n.id,
                                title: n.title,
                                body: n.body,
                            })
                            .collect(),
                    }
                }
                // Input settles on the next frame: focus moved by a click is only in the
                // dispatch tree once it has been drawn, so a keystroke sent before that would
                // go nowhere. Reply after the frame, and the driver never races the app.
                other if let Some(inputs) = inputs(&other) => match inputs {
                    Ok(inputs) => {
                        dispatch(window, &other, inputs, cx);
                        after_frame(window, Reply::Ok, cx).await
                    }
                    Err(message) => Reply::Error { message },
                },
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

/// Run `f` in the next frame, once every next-frame callback of that frame has run and before
/// the frame is drawn. The callbacks run in the order they were asked for, and one of them may
/// notify a view (an animation asks for its frame that way): run among them, `f` would draw
/// that frame before the notify that belongs in it.
fn before_next_draw(window: &Window, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    let handle = window.window_handle();
    window.on_next_frame(move |_window, cx| {
        cx.defer(move |cx| {
            let _closed = handle.update(cx, |_root, window, cx| f(window, cx));
        });
    });
}

/// `frame`, read before a frame is drawn, once the window has settled and still shows what it
/// settled on. Something may arrive between [`settle`]'s last frame and this one (a toast whose
/// first frame guesses its height and measures it in paint, ARCHITECTURE.md "Drawing under
/// retention"): the app is still drawing toward that state, so settle again rather than judge
/// the frame between, at most [`RESETTLES`] times. What arrived since the last frame and is
/// still to be drawn (an agent's toast pushed after [`settle`] read its last frame) is drawn
/// first, as the frame about to be drawn would draw it, views built again only where they were
/// told: a frame read before it would still show what settled and be judged against a state it
/// never drew.
async fn on_settled_frame(
    window: AnyWindowHandle,
    cx: &mut gpui::AsyncApp,
    frame: impl Fn(&mut Window, &mut App) -> Reply + Clone + 'static,
) -> Reply {
    let mut again = 0..RESETTLES;
    loop {
        let settled = settle(window, cx).await;
        let last_try = again.next().is_none();
        let (done_tx, done_rx) = oneshot::channel();
        let frame = frame.clone();
        // `update_window` (not `WindowHandle::update`): the root view must not be leased while
        // a dispatched action updates it.
        let scheduled = cx.update_window(window, |_root, window, _cx| {
            before_next_draw(window, move |window, cx| {
                window.draw(cx).clear(cx);
                let still = last_try || still_shows(window, settled.as_ref());
                let _sent = done_tx.send(still.then(|| frame(window, cx)));
            });
        });
        match scheduled {
            Ok(()) => match done_rx.await {
                Ok(Some(reply)) => return reply,
                Ok(None) => {}
                Err(_) => {
                    return Reply::Error { message: "window closed before the frame".into() };
                }
            },
            Err(e) => return error(&e),
        }
    }
}

/// How many times [`on_settled_frame`] settles again when a frame arrived after it settled.
const RESETTLES: usize = 4;

/// What a frame painted, as [`settle`] compares frames.
#[cfg(feature = "e2e")]
type Painted = Vec<String>;
#[cfg(not(feature = "e2e"))]
type Painted = ();

/// Whether the window last painted what [`settle`] settled on (or it never settled: something in
/// motion, judged as it is).
#[cfg(feature = "e2e")]
fn still_shows(window: &Window, settled: Option<&Painted>) -> bool {
    settled.is_none_or(|settled| *settled == slopty_kit::retained::painted(window))
}

#[cfg(not(feature = "e2e"))]
const fn still_shows(_window: &Window, _settled: Option<&Painted>) -> bool {
    true
}

/// Once the window has drawn the frames the app asked for: a value measured while drawing (the
/// composer stack the transcript clears, a fitted grid) reaches the views that read it in the
/// frame after (ARCHITECTURE.md, "Drawing under retention"), so a frame is judged only after a
/// frame that painted nothing new. The platform draws them, as a person's would be; at most
/// [`SETTLE_FRAMES`], since something in motion never stops painting anew. Gives what the
/// settled frame painted, or `None` when the window kept moving.
#[cfg(feature = "e2e")]
async fn settle(window: AnyWindowHandle, cx: &mut gpui::AsyncApp) -> Option<Painted> {
    let mut last = None;
    for _ in 0..SETTLE_FRAMES {
        let (done_tx, done_rx) = oneshot::channel();
        let scheduled = cx.update_window(window, |_root, window, _cx| {
            // Read before this frame's draw: what the last frame painted.
            before_next_draw(window, move |window, _cx| {
                let _sent = done_tx.send(slopty_kit::retained::painted(window));
            });
        });
        let (Ok(()), Ok(painted)) = (scheduled, done_rx.await) else { return None };
        if last.as_ref() == Some(&painted) {
            return last;
        }
        last = Some(painted);
    }
    None
}

#[cfg(not(feature = "e2e"))]
#[expect(clippy::unused_async, reason = "one signature for both builds")]
async fn settle(_window: AnyWindowHandle, _cx: &mut gpui::AsyncApp) -> Option<Painted> {
    None
}

/// How many frames [`settle`] waits at most.
#[cfg(feature = "e2e")]
const SETTLE_FRAMES: usize = 8;

/// `answer`, once the window has had a frame with everything dispatched so far. The frame draws
/// only what the command notified, as a person's input would: a view the command changed
/// without telling shows its old state in it, and the next `Render` or `Dump` says so.
async fn after_frame(window: AnyWindowHandle, answer: Reply, cx: &mut gpui::AsyncApp) -> Reply {
    let (done_tx, done_rx) = oneshot::channel();
    let scheduled = cx.update_window(window, |_root, window, _cx| {
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

/// A finger UIKit itself delivers (`slopty_platform::finger`): down at `from`, `steps` moves
/// 16 ms apart to `to`, lifted 16 ms after the last; with no steps a tap, lifted 60 ms after it
/// lands. Each event is sent when it is due and says that time, as a digitizer's does: one the
/// main thread was too busy to send then goes as soon as it can, still saying its time. The
/// main thread is held busy `held_ms` after the last move, so the lift goes that late. Gives
/// the view it landed on.
#[cfg(all(target_os = "ios", feature = "e2e"))]
async fn finger(
    from: [f32; 2],
    to: [f32; 2],
    steps: u32,
    held_ms: u32,
    cx: &mut gpui::AsyncApp,
) -> Result<String, String> {
    use std::time::Duration;

    use slopty_platform::finger::Finger;
    const EVERY: Duration = Duration::from_millis(16);
    const TAP: Duration = Duration::from_millis(60);
    let point = |t: f32| {
        let [x0, y0] = from;
        let [x1, y1] = to;
        (f64::from((x1 - x0).mul_add(t, x0)), f64::from((y1 - y0).mul_add(t, y0)))
    };
    let (x, y) = point(0.0);
    let finger = Finger::down(x, y)?;
    let landed = std::time::Instant::now();
    let until =
        |due: Duration| cx.background_executor().timer(due.saturating_sub(landed.elapsed()));
    for step in 1..=steps {
        let due = EVERY * step;
        until(due).await;
        #[expect(clippy::cast_precision_loss, reason = "a drag's few steps")]
        let (x, y) = point(step as f32 / steps as f32);
        finger.moved(x, y, due);
    }
    // The stall itself: this is the main thread, and nothing else runs on it meanwhile.
    std::thread::sleep(Duration::from_millis(u64::from(held_ms)));
    let lifted = if steps == 0 { TAP } else { EVERY * (steps + 1) };
    until(lifted).await;
    let (x, y) = point(1.0);
    let view = finger.landed().to_owned();
    finger.lift(x, y, lifted);
    Ok(view)
}

#[cfg(not(all(target_os = "ios", feature = "e2e")))]
#[expect(clippy::unused_async, reason = "one signature for both builds")]
async fn finger(
    _from: [f32; 2],
    _to: [f32; 2],
    _steps: u32,
    _held_ms: u32,
    _cx: &mut gpui::AsyncApp,
) -> Result<String, String> {
    Err("a finger UIKit delivers is iOS only (with the e2e feature)".into())
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
    pub(super) fn modifier_flags(modifiers: &str) -> Result<u32, String> {
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
    pub(crate) fn inject(command: Command) -> Result<(), String> {
        use gpui_ios::described::{
            DescribedInput, DescribedPinch, DescribedPress, DescribedTouch, GestureState,
            PressPhase, UiTouchPhase,
        };
        use slopty_e2e::{UiGesturePhase, UiPressPhase, UiTouchPhase as Phase};

        let touch_phase = |phase: Phase| match phase {
            Phase::Began => UiTouchPhase::Began,
            Phase::Moved => UiTouchPhase::Moved,
            Phase::Stationary => UiTouchPhase::Stationary,
            Phase::Ended => UiTouchPhase::Ended,
            Phase::Cancelled => UiTouchPhase::Cancelled,
        };

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
                let phase = touch_phase(phase);
                DescribedInput::Touches(
                    touches
                        .into_iter()
                        .map(|t| DescribedTouch { id: t.id, phase, x: t.x, y: t.y })
                        .collect(),
                )
            }
            Command::UiPointerClick { at, secondary, phase } => DescribedInput::PointerClick {
                touch: DescribedTouch { id: at.id, phase: touch_phase(phase), x: at.x, y: at.y },
                secondary,
            },
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
    pub(super) fn inject(command: Command) -> Result<(), String> {
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
            modifier_flags("fn").unwrap_err();
        }
    }
}

/// [`Command::PushRegister`]: `token` arrives as the app delegate hands it over, notes are
/// allowed with no prompt, and the phone's push key is made (or read back) as for a server.
#[cfg(target_os = "ios")]
async fn push_register(token: &[u8]) -> Reply {
    use slopty_platform::notify::{Alerts, ask_quietly, pushed};
    pushed::token_arrived(token);
    let alerts = ask_quietly().await;
    if alerts != Alerts::Allowed {
        return Reply::Error { message: format!("notes are {alerts:?}") };
    }
    match pushed::device_key() {
        Ok(key) => Reply::PushKey { key: key.public().to_vec() },
        Err(e) => error(&e),
    }
}

/// [`Command::OpenPush`]: the payload's top-level keys are the `userInfo` the extension reads.
#[cfg(target_os = "ios")]
fn open_push(payload: &str) -> Reply {
    let info: serde_json::Value = match serde_json::from_str(payload) {
        Ok(info) => info,
        Err(e) => return error(&e),
    };
    let text = |key: &str| info.get(key)?.as_str().map(str::to_owned);
    match slopty_platform::notify::pushed::note_from(text) {
        Ok(note) => Reply::Opened {
            note: slopty_e2e::DeliveredNote { id: note.id, title: note.title, body: note.body },
        },
        Err(e) => error(&e),
    }
}

/// [`Command::OpenPush`] where nothing is pushed to.
#[cfg(not(target_os = "ios"))]
fn open_push(_payload: &str) -> Reply {
    Reply::Error { message: "only an iPhone or an iPad is pushed to".into() }
}

/// [`Command::PushRegister`] where nothing is pushed to.
#[cfg(not(target_os = "ios"))]
fn push_register(_token: &[u8]) -> std::future::Ready<Reply> {
    std::future::ready(Reply::Error { message: "only an iPhone or an iPad is pushed to".into() })
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

/// One input of a driver's command: a keystroke, which goes through the window's keystroke
/// path (key down, then the text it types), or any other platform event.
enum Input {
    Key(Keystroke),
    Event(PlatformInput),
}

/// The events `command` sends, in order, or why it cannot send them; `None` when it is not
/// input the person's keyboard, pointer or a drop would send.
fn inputs(command: &Command) -> Option<Result<Vec<Input>, String>> {
    let left = MouseButton::Left;
    let none = Modifiers::default();
    let mouse_move = |position, pressed_button, modifiers| {
        Input::Event(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button,
            modifiers,
        }))
    };
    let mouse_down = |button, position, modifiers, click_count| {
        Input::Event(PlatformInput::MouseDown(MouseDownEvent {
            button,
            position,
            modifiers,
            click_count,
            first_mouse: false,
        }))
    };
    let mouse_up = |button, position, modifiers, click_count| {
        Input::Event(PlatformInput::MouseUp(MouseUpEvent {
            button,
            position,
            modifiers,
            click_count,
        }))
    };
    Some(Ok(match command {
        Command::Keys { keys } => {
            return Some(
                keys.split_whitespace()
                    .map(|text| {
                        Keystroke::parse_with_os(text, slopty_shell::keys::OS)
                            .map(Input::Key)
                            .map_err(|e| format!("{text:?}: {e}"))
                    })
                    .collect(),
            );
        }
        Command::Type { text } => text
            .chars()
            .map(|ch| {
                Input::Key(Keystroke {
                    modifiers: Modifiers { shift: ch.is_uppercase(), ..Modifiers::default() },
                    key: ch.to_lowercase().to_string(),
                    key_char: Some(ch.to_string()),
                })
            })
            .collect(),
        Command::DropFiles { paths, x, y } => {
            let position = point(px(*x), px(*y));
            let paths = gpui::ExternalPaths(paths.iter().map(PathBuf::from).collect());
            [
                gpui::FileDropEvent::Entered { position, paths },
                gpui::FileDropEvent::Pending { position },
                gpui::FileDropEvent::Submit { position },
            ]
            .into_iter()
            .map(|event| Input::Event(PlatformInput::FileDrop(event)))
            .collect()
        }
        Command::Click { x, y, button, count } => {
            let position = point(px(*x), px(*y));
            let button = mouse_button(*button);
            let clicks = usize::try_from(*count).unwrap_or(1).max(1);
            vec![
                mouse_move(position, None, none),
                mouse_down(button, position, none, clicks),
                mouse_up(button, position, none, clicks),
            ]
        }
        Command::Drag { x, y, to_x, to_y, command } => {
            let (from, to) = (point(px(*x), px(*y)), point(px(*to_x), px(*to_y)));
            let modifiers = Modifiers { platform: *command, ..Modifiers::default() };
            vec![
                mouse_down(left, from, modifiers, 1),
                mouse_move(to, Some(left), modifiers),
                mouse_up(left, to, modifiers, 1),
            ]
        }
        Command::Move { x, y } => vec![mouse_move(point(px(*x), px(*y)), None, none)],
        Command::DragTo { x, y } => vec![mouse_move(point(px(*x), px(*y)), Some(left), none)],
        Command::Press { x, y } => vec![mouse_down(left, point(px(*x), px(*y)), none, 1)],
        Command::Release { x, y } => vec![mouse_up(left, point(px(*x), px(*y)), none, 1)],
        Command::Scroll { x, y, dx, dy } => {
            vec![Input::Event(PlatformInput::Scroll(ScrollEvent {
                position: point(px(*x), px(*y)),
                delta: ScrollDelta::Lines(point(*dx, *dy)),
                modifiers: none,
                touch_phase: TouchPhase::Moved,
                momentum_phase: None,
                source: gpui::ScrollSource::Pointer,
            }))]
        }
        _ => return None,
    }))
}

/// Send `command`'s inputs as the platform sends a person's: one by one, each first through
/// `gpui::prepare_input`, which draws the frame the input needs in an update of its own so the
/// work that frame defers runs before the input's handlers, and then dispatched in an update
/// of its own. Sent together in one update, a frame drawn for the second input would leave its
/// deferred work to run after that input's handlers, where a person's never does.
fn dispatch(
    window: AnyWindowHandle,
    #[cfg_attr(
        not(target_os = "macos"),
        expect(unused_variables, reason = "only macOS tracks the pointer a command drives")
    )]
    command: &Command,
    inputs: Vec<Input>,
    cx: &mut gpui::AsyncApp,
) {
    #[cfg(target_os = "macos")]
    cx.update(|cx| pointer::driven(command, cx));
    for input in inputs {
        match input {
            Input::Key(keystroke) => {
                gpui::prepare_keystroke(window, &keystroke, cx);
                let _sent = cx.update_window(window, |_root, window, cx| {
                    let handled = window.dispatch_keystroke(keystroke.clone(), cx);
                    let keystroke = keystroke.unparse(slopty_shell::keys::OS);
                    tracing::debug!(%keystroke, handled, "keys");
                });
            }
            Input::Event(event) => {
                gpui::prepare_input(window, &event, cx);
                let _sent = cx.update_window(window, |_root, window, cx| {
                    let _handled = window.dispatch_event(event, cx);
                });
            }
        }
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
        Command::OpenLink { url } => {
            crate::invite::open_link(url);
            Reply::Ok
        }
        Command::Resume { what } => match slopty_platform::resume::Resume::named(&what) {
            Some(resume) => {
                workspace.read(cx).resume(resume);
                Reply::Ok
            }
            None => Reply::Error { message: format!("no resume is called {what}") },
        },
        #[cfg(target_os = "macos")]
        Command::Clipboard { .. } => Reply::Error {
            message: "clipboard: the Mac app shares a named pasteboard; write that".into(),
        },
        #[cfg(not(target_os = "macos"))]
        Command::Clipboard { media_type, data } => {
            let Ok(bytes) = data_encoding::BASE64.decode(data.as_bytes()) else {
                return Reply::Error { message: "clipboard: data is not base64".into() };
            };
            let format = match media_type.as_str() {
                "image/png" => gpui::ImageFormat::Png,
                "image/jpeg" => gpui::ImageFormat::Jpeg,
                "image/gif" => gpui::ImageFormat::Gif,
                "image/webp" => gpui::ImageFormat::Webp,
                other => {
                    return Reply::Error { message: format!("clipboard: not a picture: {other}") };
                }
            };
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image {
                format,
                bytes,
                id: 0,
            }));
            Reply::Ok
        }
        // Dispatched event by event, each in an update of its own (`dispatch`).
        Command::Keys { .. }
        | Command::Type { .. }
        | Command::DropFiles { .. }
        | Command::Click { .. }
        | Command::Drag { .. }
        | Command::Move { .. }
        | Command::DragTo { .. }
        | Command::Press { .. }
        | Command::Release { .. }
        | Command::Scroll { .. } => Reply::Error { message: "input goes through dispatch".into() },
        Command::PinClock { at_ms } => {
            // The terminal's readouts read the kit's clock, the shell's ages and dates its own.
            slopty_kit::clock::pin(at_ms.map(slopty_core::WallMs::from_millis), cx);
            slopty_shell::clock::pin(at_ms, cx);
            // Nothing that showed a time was told: every view is built again.
            window.refresh();
            Reply::Ok
        }
        Command::FramesReset => {
            slopty_kit::frames::reset(cx);
            cx.set_global(CpuAtReset(slopty_platform::cpu::this_thread().unwrap_or_default()));
            Reply::Ok
        }
        Command::ForgetWorker { id } => {
            let Ok(id) = id.trim().parse::<WorkerId>() else {
                return Reply::Error { message: format!("not a worker id: {id}") };
            };
            workspace.update(cx, |ws, cx| ws.forget_worker(id, cx));
            Reply::Ok
        }
        Command::HoldMain { ms } => {
            // Busy, as a hang is: a sleep would give the thread back to the system.
            let (started, held) = (std::time::Instant::now(), std::time::Duration::from_millis(ms));
            while started.elapsed() < held {
                std::hint::spin_loop();
            }
            Reply::Ok
        }
        Command::Resize { width, height } => {
            if cfg!(target_os = "ios") {
                return Reply::Error { message: "UIKit sizes the window on iOS".into() };
            }
            window.resize(size(px(width), px(height)));
            Reply::Ok
        }
        Command::Appearance { dark } => {
            use slopty_shell::appearance::{AppearanceMode, set_mode};
            set_mode(if dark { AppearanceMode::Dark } else { AppearanceMode::Light }, cx);
            window.refresh();
            Reply::Ok
        }
        Command::Surface { frosted } => {
            use slopty_shell_theme::SurfacePreference;
            let surface =
                if frosted { SurfacePreference::Frosted } else { SurfacePreference::Opaque };
            slopty_shell::appearance::set_surface(surface, cx);
            window.refresh();
            Reply::Ok
        }
        // A flip redraws every window, so each view reads it again.
        Command::Motion { reduced } => {
            slopty_shell::motion::hold(reduced, cx);
            Reply::Ok
        }
        Command::Dump
        | Command::PushRegister { .. }
        | Command::OpenPush { .. }
        | Command::Delivered
        | Command::Render { .. }
        | Command::Quit
        | Command::UiKeyPress { .. }
        | Command::UiTouch { .. }
        | Command::UiPointerClick { .. }
        | Command::UiFinger { .. }
        | Command::UiPinch { .. }
        | Command::UiInsertText { .. }
        | Command::UiDeleteBackward => Reply::Error { message: "handled elsewhere".into() },
    }
}

/// How the iPad's pointer looks now, in [`Dump::pointer`]'s words.
#[cfg(all(target_os = "ios", feature = "e2e"))]
fn pointer_look() -> String {
    use gpui_ios::pointer::{Outline, PointerLook};
    match gpui_ios::pointer_look() {
        PointerLook::System => "system",
        PointerLook::Beam { upright: true } => "beam",
        PointerLook::Beam { upright: false } => "beam-lying",
        PointerLook::Hidden => "hidden",
        PointerLook::Outline(Outline::LeftRight) => "outline-left-right",
        PointerLook::Outline(Outline::UpDown) => "outline-up-down",
        PointerLook::Outline(Outline::UpLeftDownRight) => "outline-up-left-down-right",
        PointerLook::Outline(Outline::UpRightDownLeft) => "outline-up-right-down-left",
        PointerLook::Outline(Outline::Cross) => "outline-cross",
    }
    .to_owned()
}

/// Off iOS, or without the fork's test support, there is no pointer to read; never called
/// ([`Dump::pointer`] is `None`).
#[cfg(not(all(target_os = "ios", feature = "e2e")))]
const fn pointer_look() -> String {
    String::new()
}

const fn mouse_button(button: Button) -> MouseButton {
    match button {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
    }
}

/// Where the frame shown differs from the same state drawn from scratch
/// ([`slopty_kit::retained::stale`]), with where the pointer was and whether the keyboard was
/// the last input: what a hover reads, and so what a stale hover is judged by.
#[cfg(feature = "e2e")]
fn stale(window: &mut Window, cx: &mut App) -> Option<String> {
    let pointer = window.mouse_position();
    let keyboard = window.last_input_was_keyboard();
    slopty_kit::retained::stale(window, cx, STALE_LINES).map(|stale| {
        let (x, y) = (f32::from(pointer.x), f32::from(pointer.y));
        format!("{stale}\n(pointer at {x},{y}, last input from the keyboard: {keyboard})")
    })
}

/// How many lines of a stale frame's difference a reply quotes, each way.
#[cfg(feature = "e2e")]
const STALE_LINES: usize = 12;

/// Whether the accessibility tree stays on for the whole run (`SLOPTY_E2E_A11Y=always`), as on
/// a Mac where an assistive app asked for it once: every frame then builds the tree with the
/// views it draws again, and a dump reads it without drawing anything from scratch. A dump that
/// drew the window from scratch would build every view again before the next input, which a
/// person's window never does, and hide a view that acts on what it was last built with.
#[cfg(feature = "e2e")]
fn tree_kept() -> bool {
    std::env::var_os("SLOPTY_E2E_A11Y").is_some_and(|mode| mode == "always")
}

/// What GPUI's check of every frame (`GPUI_CHECK_RETAINED=record`) found stale since it was
/// last asked, without drawing a frame from scratch into the window as [`stale`] does.
#[cfg(feature = "e2e")]
fn kept_stale(window: &mut Window) -> Option<String> {
    let found = window.take_retained_check_reports();
    (!found.is_empty()).then(|| found.join("\n"))
}

/// The frame the app draws now, saved to `path`, and checked against the same state drawn from
/// scratch: a difference is an error, and the frame from scratch is saved beside it
/// (`<path>.scratch.png`) for the review.
#[cfg(feature = "e2e")]
fn render(window: &mut Window, cx: &mut App, path: &str, scale: Option<f32>) -> Reply {
    // Next-frame callbacks run before the frame is drawn: draw it as the app would.
    window.draw(cx).clear(cx);
    let image = match scale {
        // A scale the window is not on is drawn afresh offscreen, and the window's own put back.
        Some(scale) => window.render_to_image_at(scale, cx),
        None => window.render_to_image(),
    };
    let mut image = match image {
        Ok(image) => image,
        Err(e) => return Reply::Error { message: format!("render: {e:#}") },
    };
    over_backdrop(&mut image);
    let stale = if tree_kept() { kept_stale(window) } else { stale(window, cx) };
    let (width, height) = image.dimensions();
    if let Err(e) = image.save(path) {
        return Reply::Error { message: format!("write {path}: {e}") };
    }
    match stale {
        // The tree is on and the frame was drawn with it: read it as it is.
        None if tree_kept() => {
            let scale = scale.unwrap_or_else(|| window.scale_factor());
            Reply::Rendered { width, height, a11y: a11y_nodes(window), scale }
        }
        None => {
            // What the frame says, from the same state drawn once more with the tree asked for:
            // the tree is built only while it is, and the frame drawn for the picture is a
            // person's.
            let started = std::time::Instant::now();
            window.set_a11y_active(true);
            window.draw(cx).clear(cx);
            let a11y = a11y_nodes(window);
            window.set_a11y_active(false);
            tracing::info!(us = started.elapsed().as_micros(), nodes = a11y.len(), "frame text");
            // The tree is in points, read at the window's own scale; the picture's scale maps it.
            let scale = scale.unwrap_or_else(|| window.scale_factor());
            Reply::Rendered { width, height, a11y, scale }
        }
        Some(stale) => {
            let scratch = format!("{path}.scratch.png");
            if let Ok(image) = window.render_to_image() {
                let _kept = image.save(&scratch);
            }
            Reply::Error { message: format!("the frame drawn is stale ({scratch}): {stale}") }
        }
    }
}

/// What a render shows behind a window that lets its backdrop through (frosted surfaces): a fixed
/// slate, where on a Mac the desktop would be, so a frosted frame is the same on every machine.
#[cfg(feature = "e2e")]
const BACKDROP: [u8; 3] = [72, 96, 128];

/// Composite `image`, whose colours are premultiplied by their alpha as GPUI draws them, over
/// [`BACKDROP`]: every pixel comes out opaque, and an opaque one unchanged.
#[cfg(feature = "e2e")]
fn over_backdrop(image: &mut image::RgbaImage) {
    for pixel in image.pixels_mut() {
        let clear = 255_u16.saturating_sub(u16::from(pixel[3]));
        for (channel, backdrop) in pixel.0.iter_mut().zip(BACKDROP) {
            let under = clear.saturating_mul(u16::from(backdrop)).saturating_add(127) / 255;
            *channel = u8::try_from(u16::from(*channel).saturating_add(under)).unwrap_or(u8::MAX);
        }
        pixel[3] = u8::MAX;
    }
}

#[cfg(not(feature = "e2e"))]
fn render(_window: &mut Window, _cx: &mut App, _path: &str, _scale: Option<f32>) -> Reply {
    Reply::Error { message: "built without the `e2e` feature; no renderer access".into() }
}

/// The app's state as the frame about to be drawn shows it, with that frame checked against
/// the same state drawn from scratch and the accessibility tree read from a frame of its own.
///
/// Runs before the next frame is drawn, once its next-frame callbacks have run
/// ([`before_next_draw`]). The frame times are read first,
/// so the frames the check draws are not in them. The tree is built only while a screen reader
/// asks for it, and GPUI draws every view from scratch while it does: it is asked for in one
/// frame here, so every other frame of a run is drawn as a person's would be.
#[cfg(feature = "e2e")]
fn dump(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) -> Dump {
    let frames = frame_info(cx);
    window.draw(cx).clear(cx);
    if tree_kept() {
        // Nothing is drawn from scratch: the next input lands on the frame a person's would.
        let mut dump = workspace.read(cx).dump(window, cx);
        dump.frames = frames;
        dump.stale = kept_stale(window).map(|stale| format!("{stale} (focus {})", dump.focused));
        return dump;
    }
    let stale = stale(window, cx);
    window.set_a11y_active(true);
    window.draw(cx).clear(cx);
    let mut dump = workspace.read(cx).dump(window, cx);
    dump.frames = frames;
    dump.stale = stale.map(|stale| format!("{stale} (focus {})", dump.focused));
    window.set_a11y_active(false);
    dump
}

#[cfg(not(feature = "e2e"))]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "one signature for both builds: the e2e build's draws frames through them"
)]
fn dump(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) -> Dump {
    workspace.read(cx).dump(window, cx)
}

/// The accessibility tree of the last frame, trimmed for the dump.
#[cfg(feature = "e2e")]
fn a11y_nodes(window: &Window) -> Vec<slopty_e2e::A11yNode> {
    slopty_kit::a11y::tree(window)
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

/// The surfaces the last frame painted, in window points.
#[cfg(feature = "e2e")]
fn surfaces(window: &Window) -> Vec<slopty_e2e::SurfaceInfo> {
    let scale = window.scale_factor();
    let points = |v: gpui::ScaledPixels| v.as_f32() / scale;
    window
        .painted_surfaces()
        .iter()
        .map(|painted| {
            let gpui::Bounds { origin, size: extent } = painted.bounds;
            let bounds =
                [points(origin.x), points(origin.y), points(extent.width), points(extent.height)];
            slopty_e2e::SurfaceInfo::of(&painted.image_buffer, bounds)
        })
        .collect()
}

#[cfg(not(feature = "e2e"))]
const fn surfaces(_window: &Window) -> Vec<slopty_e2e::SurfaceInfo> {
    Vec::new()
}

/// Everything a test may want to know about one remote window, timings in microseconds.
fn screen_info(item: ItemId, view: &ScreenView, window: &Window) -> ScreenInfo {
    let us = |d: std::time::Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    let pacing = view.pacing();
    let size = view.size();
    let s = view.stats();
    let source = match view.source_state() {
        slopty_proto::screen::SourceState::Idle => "idle",
        slopty_proto::screen::SourceState::Live => "live",
        slopty_proto::screen::SourceState::Locked => "locked",
        slopty_proto::screen::SourceState::Away => "away",
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
        silences_worker: s.silences.worker_quiet.saturating_add(s.silences.worker_covered),
        silences_dozed: s.silences.receiver_dozed,
        stalls_in_flight: s.silences.in_flight,
        stalls_unread: s
            .silences
            .stamp_wrapped
            .saturating_add(s.silences.stamp_backwards)
            .saturating_add(s.silences.stamp_absent),
        gap_ms_max: s.silences.gap_ms_max,
        dozed_ms_max: s.silences.dozed_ms_max,
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
        layer: view.layer_host().and_then(|id| {
            let shows = window.presented_natives()?.placement(id)?.visible;
            #[expect(clippy::cast_possible_truncation, reason = "window points")]
            let points = |p: gpui::Pixels| f32::from(p).round() as i32;
            Some([
                points(shows.origin.x),
                points(shows.origin.y),
                points(shows.size.width),
                points(shows.size.height),
            ])
        }),
    }
}

/// The main thread's CPU time when the frame probe was last reset ([`Command::FramesReset`]).
struct CpuAtReset(std::time::Duration);

impl gpui::Global for CpuAtReset {}

/// The frame probe's numbers, and the main thread's CPU time since its reset, microseconds. The
/// commands run on the main thread, so its clock is the one read here.
fn frame_info(cx: &App) -> FrameInfo {
    let us = |d: std::time::Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    let since = cx.try_global::<CpuAtReset>().map_or(std::time::Duration::ZERO, |at| at.0);
    let main_cpu_us =
        slopty_platform::cpu::this_thread().map_or(0, |now| us(now.saturating_sub(since)));
    let stats = slopty_kit::frames::stats(cx);
    stats.map_or_else(FrameInfo::default, |s| FrameInfo {
        main_cpu_us,
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

/// A terminal's keystroke → glass numbers, microseconds.
fn latency_info(s: &slopty_terminal::latency::LatencyStats) -> LatencyInfo {
    let us = |d: std::time::Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    LatencyInfo {
        echoed: s.echoed,
        echo_p50_us: us(s.echo_p50),
        echo_p95_us: us(s.echo_p95),
        echo_p99_us: us(s.echo_p99),
        echo_max_us: us(s.echo_max),
        echo_hops_us: s.echo_hops.map(|h| [us(h.p50), us(h.p95)]),
        predicted: s.predicted,
        echo_first: s.echo_first,
        guess_late: s.guess_late,
        predicted_p50_us: us(s.predicted_p50),
        predicted_p95_us: us(s.predicted_p95),
        predicted_p99_us: us(s.predicted_p99),
        predicted_max_us: us(s.predicted_max),
        predicted_hops_us: s.predicted_hops.map(|h| [us(h.p50), us(h.p95)]),
    }
}

impl Workspace {
    /// Everything a test may want to know, read from the entities.
    fn dump(&self, window: &Window, cx: &App) -> Dump {
        let viewport = window.viewport_size();
        let view = self.view.read(cx);
        let watching = view.watching();
        let workers: Vec<WorkerInfo> = view
            .workers()
            .map(|(key, name, status)| WorkerInfo {
                name: name.to_owned(),
                status: status.text(),
                needs_you: view.needs_you_on(key),
                rtt_us: view.rtt(key).map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX)),
                clipboard_watched: watching.contains(&key),
                links: view.links(key),
                doubts: view.doubts(key),
            })
            .collect();
        let status = if workers.is_empty() {
            "no workers".to_owned()
        } else {
            view.workers()
                .find(|(_, _, s)| **s != WorkerStatus::Connected)
                .map_or_else(|| "connected".to_owned(), |(_, _, s)| s.text())
        };
        let mut dump = Dump {
            a11y: a11y_nodes(window),
            surfaces: surfaces(window),
            window: WindowInfo {
                width: f32::from(viewport.width),
                height: f32::from(viewport.height),
                scale: window.scale_factor(),
                active: window.is_window_active(),
                safe_bottom: f32::from(window.insets().safe_area.bottom),
                keyboard_bottom: f32::from(window.insets().ime.bottom),
            },
            workers,
            status,
            dark: self.theme.variant() == slopty_theme::Variant::Dark,
            frames: frame_info(cx),
            pointer: cfg!(all(target_os = "ios", feature = "e2e")).then(pointer_look),
            natives: window.presented_natives().map_or(0, |frame| frame.placements.len()),
            native_hits: window
                .native_hit_map()
                .map(|map| {
                    let rect = |b: gpui::Bounds<gpui::Pixels>| {
                        [
                            f32::from(b.origin.x),
                            f32::from(b.origin.y),
                            f32::from(b.size.width),
                            f32::from(b.size.height),
                        ]
                    };
                    map.entries
                        .iter()
                        .map(|entry| slopty_e2e::NativeHit {
                            visible: rect(entry.visible),
                            occluders: entry.occluders.iter().copied().map(rect).collect(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            focus_path: window
                .context_stack()
                .iter()
                .map(|context| format!("{context:?}"))
                .collect::<Vec<_>>()
                .join(" > "),
            ..Dump::default()
        };
        let mut focused = if window.focused(cx).is_some() { "other" } else { "none" }.to_owned();
        dump.client = view
            .workers()
            .next()
            .and_then(|(key, ..)| view.me(key))
            .map(|me| me.to_string())
            .unwrap_or_default();
        let names: std::collections::HashMap<_, _> =
            view.workers().map(|(key, name, _)| (key, name.to_owned())).collect();
        let mut items: Vec<_> = view.items().collect();
        items.sort_by_key(|(_, item)| item.id);
        for (worker, item) in items {
            let (kind, session) = match &item.kind {
                ItemKind::Terminal { session } => ("terminal", Some(session.to_string())),
                ItemKind::Window { .. } => ("window", None),
                ItemKind::Display { .. } => ("display", None),
            };
            dump.items.push(ItemInfo {
                id: item.id.to_string(),
                kind: kind.to_owned(),
                worker: names.get(&worker).cloned().unwrap_or_default(),
                session,
            });
            if matches!(item.kind, ItemKind::Window { .. } | ItemKind::Display { .. })
                && let Some(screen) = view.screen(item.id)
            {
                let screen = screen.read(cx);
                if screen.focus_handle(cx).is_focused(window) {
                    focused = format!("screen:{}", screen.stream().0);
                }
                dump.screens.push(screen_info(item.id, screen, window));
            }
            if let ItemKind::Terminal { session } = item.kind
                && let Some(terminal) = view.terminal(session)
            {
                let terminal = terminal.read(cx);
                if terminal.focus_handle(cx).is_focused(window) {
                    focused = format!("terminal:{session}");
                }
                let size = terminal.size();
                let cursor = terminal.cursor();
                dump.terminals.push(TerminalInfo {
                    session: session.to_string(),
                    kind: "terminal".to_owned(),
                    size: [size.cols, size.rows],
                    cursor: [cursor.col, cursor.row],
                    cursor_shape: format!("{:?}", cursor.shape),
                    at_prompt: !terminal.state().command_running()
                        && terminal
                            .state()
                            .screen()
                            .lines()
                            .get(usize::from(cursor.row))
                            .is_some_and(|line| line.mark.is_prompt()),
                    rows: terminal.rows(),
                    lines: terminal.lines(),
                    epoch: terminal.state().epoch(),
                    latency: latency_info(&terminal.latency()),
                    face: terminal.metrics().map(|m| face_info(&m)),
                    driving: terminal.driving(),
                    images: terminal.state().placements().len(),
                    ports: view
                        .forwards(session)
                        .iter()
                        .map(|f| [f.port.number, f.local.unwrap_or(0)])
                        .collect(),
                    resizes: terminal.state().resizes(),
                    grid: terminal.metrics().map(|m| {
                        let (x, y) = (f32::from(m.origin.x), f32::from(m.origin.y));
                        [x, y, f32::from(m.cell_width), f32::from(m.line_height)]
                    }),
                });
            }
        }
        dump.focused = focused;
        dump
    }
}

/// The measured face for the dump.
#[expect(clippy::cast_possible_truncation, reason = "device pixels; f32 carries them")]
fn face_info(m: &slopty_terminal::CellMetrics) -> FaceInfo {
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

/// The driver's pointer, which the driver alone moves.
///
/// The driver's moves are synthetic events: they set GPUI's `mouse_position` and nothing else,
/// and the real cursor stays wherever it is, outside the window. GPUI takes the real cursor's
/// position back on every resize, move and activation change (`Window::bounds_changed`), so
/// another app taking the front (another test's, in the e2e guest) moved the pointer off the
/// control the driver rests on, and its hover and tooltip went (`docs/decisions/tooling.md`,
/// "The e2e driver owns the pointer"). The pointer is put back where the driver left it, before
/// the frame the change asks for.
#[cfg(target_os = "macos")]
mod pointer {
    use gpui::{
        App, Context, Modifiers, MouseButton, MouseMoveEvent, Pixels, PlatformInput, Point, Window,
    };
    use slopty_e2e::Command;

    use crate::Workspace;

    /// Where the driver last put the pointer, and whether a button is held there.
    #[derive(Clone, Copy, Debug)]
    struct Kept {
        at: Point<Pixels>,
        pressed: bool,
    }

    struct Pointer(Option<Kept>);

    impl gpui::Global for Pointer {}

    /// Put the pointer back where the driver left it whenever the window's bounds change, for as
    /// long as the app runs.
    pub(super) fn keep(window: &mut Window, cx: &mut Context<Workspace>) {
        cx.set_global(Pointer(None));
        cx.observe_window_bounds(window, |_ws, window, cx| {
            let Some(kept) = cx.try_global::<Pointer>().and_then(|p| p.0) else { return };
            if window.mouse_position() == kept.at {
                return;
            }
            tracing::debug!(from = ?window.mouse_position(), to = ?kept.at, "the driver's pointer put back");
            // The workspace is leased here: the move goes once the change's effects have run,
            // before the frame it asked for.
            window.defer(cx, move |window, cx| {
                let _moved = window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: kept.at,
                        pressed_button: kept.pressed.then_some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Keep where `command` puts the pointer.
    pub(super) fn driven(command: &Command, cx: &mut App) {
        let kept = cx.try_global::<Pointer>().and_then(|p| p.0);
        let at = |x: f32, y: f32| gpui::point(gpui::px(x), gpui::px(y));
        let next = match *command {
            Command::Move { x, y } | Command::Release { x, y } | Command::Click { x, y, .. } => {
                Some(Kept { at: at(x, y), pressed: false })
            }
            Command::Drag { to_x, to_y, .. } => Some(Kept { at: at(to_x, to_y), pressed: false }),
            Command::DragTo { x, y } | Command::Press { x, y } => {
                Some(Kept { at: at(x, y), pressed: true })
            }
            Command::Scroll { x, y, .. } => {
                Some(Kept { at: at(x, y), pressed: kept.is_some_and(|k| k.pressed) })
            }
            _ => kept,
        };
        if cx.has_global::<Pointer>() {
            cx.set_global(Pointer(next));
        }
    }
}
