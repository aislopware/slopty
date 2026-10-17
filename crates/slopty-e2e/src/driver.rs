//! The client side of the test socket.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::snapshot::Frame;
use crate::{
    Button, Command, Dump, Reply, UiGesturePhase, UiPressPhase, UiTouchPhase, UiTouchPoint, hid,
};

/// How often [`Driver::wait_for`] polls.
const POLL: Duration = Duration::from_millis(100);

/// How long one command may take to be answered (a frame, or a connect round trip).
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// One connection to a running app.
#[derive(Debug)]
pub struct Driver {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Driver {
    /// Connect to the app listening on `socket`.
    ///
    /// # Errors
    ///
    /// When nothing listens there.
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect {}", socket.display()))?;
        let (read, writer) = stream.into_split();
        Ok(Self { reader: BufReader::new(read), writer })
    }

    /// Send one command and wait for its reply.
    ///
    /// # Errors
    ///
    /// When the socket breaks or the app answers with something that is not a reply.
    pub async fn call(&mut self, command: &Command) -> Result<Reply> {
        let mut line = serde_json::to_string(command)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.context("send")?;
        let mut answer = String::new();
        // Every reply waits for the app's next frame; one that never comes is the app hung,
        // and the failure should say on which command.
        let n = tokio::time::timeout(REPLY_TIMEOUT, self.reader.read_line(&mut answer))
            .await
            .with_context(|| format!("no reply to {command:?} within {REPLY_TIMEOUT:?}"))?
            .context("receive")?;
        if n == 0 {
            bail!("app closed the test socket");
        }
        serde_json::from_str(answer.trim()).with_context(|| format!("parse reply {answer:?}"))
    }

    /// Send one command and require [`Reply::Ok`].
    ///
    /// # Errors
    ///
    /// When the app reports an error or answers with the wrong reply kind.
    pub async fn ok(&mut self, command: &Command) -> Result<()> {
        match self.call(command).await? {
            Reply::Ok => Ok(()),
            Reply::Error { message } => bail!("{command:?}: {message}"),
            other => bail!("{command:?}: unexpected {other:?}"),
        }
    }

    /// Keystrokes in binding syntax, space separated.
    ///
    /// # Errors
    ///
    /// When a keystroke does not parse or the socket breaks.
    pub async fn keys(&mut self, keys: &str) -> Result<()> {
        self.ok(&Command::Keys { keys: keys.to_owned() }).await
    }

    /// Type text.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn type_text(&mut self, text: &str) -> Result<()> {
        self.ok(&Command::Type { text: text.to_owned() }).await
    }

    /// Drop files on a window point, through GPUI's own file-drop events.
    ///
    /// # Errors
    ///
    /// When the app answers an error or the socket fails.
    pub async fn drop_files(&mut self, paths: &[&Path], x: f32, y: f32) -> Result<()> {
        let paths = paths.iter().map(|p| p.display().to_string()).collect();
        self.ok(&Command::DropFiles { paths, x, y }).await
    }

    /// Left click at a window point.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn click(&mut self, x: f32, y: f32) -> Result<()> {
        self.ok(&Command::Click { x, y, button: Button::Left, count: 1 }).await
    }

    /// Press at one window point and release at another.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn drag(&mut self, x: f32, y: f32, to_x: f32, to_y: f32) -> Result<()> {
        self.ok(&Command::Drag { x, y, to_x, to_y, command: false }).await
    }

    /// Press the primary button at a window point and hold it: a drag's start.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn press(&mut self, x: f32, y: f32) -> Result<()> {
        self.ok(&Command::Press { x, y }).await
    }

    /// Move the pointer with the button held since [`Self::press`]: a step of a drag.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn drag_to(&mut self, x: f32, y: f32) -> Result<()> {
        self.ok(&Command::DragTo { x, y }).await
    }

    /// Let the button held since [`Self::press`] go at a window point.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn release(&mut self, x: f32, y: f32) -> Result<()> {
        self.ok(&Command::Release { x, y }).await
    }

    /// [`Self::drag`] with ⌘ held: on a terminal path, the file is dragged out of the app.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn cmd_drag(&mut self, x: f32, y: f32, to_x: f32, to_y: f32) -> Result<()> {
        self.ok(&Command::Drag { x, y, to_x, to_y, command: true }).await
    }

    /// Scroll `dx`, `dy` lines at a window point.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn scroll(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> Result<()> {
        self.ok(&Command::Scroll { x, y, dx, dy }).await
    }

    /// A hardware key press and release at the UIKit boundary (iOS): `keystroke` in GPUI's
    /// binding syntax (`cmd-shift-l`, `up`, `a`), the key's HID usage and the chord's
    /// modifier flags going through `pressesBegan:` / `pressesEnded:`.
    ///
    /// # Errors
    ///
    /// When the key has no usage, the app is not on iOS, or the socket breaks.
    pub async fn ui_key(&mut self, keystroke: &str) -> Result<()> {
        let (usage, modifiers) =
            hid::chord(keystroke).with_context(|| format!("no HID usage for {keystroke:?}"))?;
        for phase in [UiPressPhase::Began, UiPressPhase::Ended] {
            self.ok(&Command::UiKeyPress { usage, modifiers: modifiers.clone(), phase }).await?;
        }
        Ok(())
    }

    /// One `touches…:withEvent:` set at the UIKit boundary (iOS).
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_touch(&mut self, touches: &[UiTouchPoint], phase: UiTouchPhase) -> Result<()> {
        self.ok(&Command::UiTouch { touches: touches.to_vec(), phase }).await
    }

    /// One finger down and up at a window point (iOS): a tap once GPUI recognizes it.
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_tap(&mut self, x: f32, y: f32) -> Result<()> {
        let finger = [UiTouchPoint { id: 1, x, y }];
        self.ui_touch(&finger, UiTouchPhase::Began).await?;
        self.ui_touch(&finger, UiTouchPhase::Ended).await
    }

    /// A finger UIKit itself delivers, dragged from one window point to another in `steps`
    /// moves 16 ms apart and lifted (iOS): what a page's web view sees as a person's drag.
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_finger_drag(&mut self, from: [f32; 2], to: [f32; 2], steps: u32) -> Result<()> {
        self.ok(&Command::UiFinger { from, to, steps }).await
    }

    /// A finger UIKit itself delivers, down and up at a window point (iOS): a tap whatever view
    /// is under it, a page's included.
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_finger_tap(&mut self, x: f32, y: f32) -> Result<()> {
        self.ok(&Command::UiFinger { from: [x, y], to: [x, y], steps: 0 }).await
    }

    /// A trackpad's click at a window point (iOS): pressed and let go, the secondary button (a
    /// two-finger click) or the primary, delivered as UIKit's pointer touch.
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_pointer_click(&mut self, x: f32, y: f32, secondary: bool) -> Result<()> {
        let at = UiTouchPoint { id: 2, x, y };
        for phase in [UiTouchPhase::Began, UiTouchPhase::Ended] {
            self.ok(&Command::UiPointerClick { at, secondary, phase }).await?;
        }
        Ok(())
    }

    /// A pinch about a window point growing (or shrinking) by `factor` in `steps` reports of
    /// the recognizer (iOS): each report carries the step's own scale, as the view reads it.
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_pinch(&mut self, x: f32, y: f32, factor: f32, steps: u32) -> Result<()> {
        #[expect(clippy::cast_precision_loss, reason = "a handful of steps")]
        let step = factor.powf(1.0 / steps.max(1) as f32);
        self.ok(&Command::UiPinch { scale: 1.0, x, y, phase: UiGesturePhase::Began }).await?;
        for _ in 0..steps.max(1) {
            self.ok(&Command::UiPinch { scale: step, x, y, phase: UiGesturePhase::Changed })
                .await?;
        }
        self.ok(&Command::UiPinch { scale: 1.0, x, y, phase: UiGesturePhase::Ended }).await
    }

    /// The text system's `insertText:` (iOS).
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_insert_text(&mut self, text: &str) -> Result<()> {
        self.ok(&Command::UiInsertText { text: text.to_owned() }).await
    }

    /// Put the picture `bytes` of `media_type` (`image/png`, …) on the simulator app's own
    /// clipboard through GPUI, as a screenshot would be (iOS; the Mac app's clipboard is a named
    /// pasteboard the test writes itself).
    ///
    /// # Errors
    ///
    /// When the app is the Mac's, the type is no picture's, or the socket breaks.
    pub async fn clipboard_image(&mut self, media_type: &str, bytes: &[u8]) -> Result<()> {
        let data = base64(bytes);
        self.ok(&Command::Clipboard { media_type: media_type.to_owned(), data }).await
    }

    /// The text system's `deleteBackward` (iOS).
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_delete_backward(&mut self) -> Result<()> {
        self.ok(&Command::UiDeleteBackward).await
    }

    /// Start a fresh frame-time window.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn frames_reset(&mut self) -> Result<()> {
        self.ok(&Command::FramesReset).await
    }

    /// Activate an agent banner: reveal `tag`'s session, on whichever worker runs it, as the
    /// response handler does.
    ///
    /// # Errors
    ///
    /// When the socket breaks or the session is on no worker.
    pub async fn notification_response(&mut self, tag: &str) -> Result<()> {
        self.ok(&Command::NotificationResponse { tag: tag.to_owned() }).await
    }

    /// Register `token` as the phone's device token, with notes allowed quietly; the public
    /// half of the phone's push key.
    ///
    /// # Errors
    ///
    /// When the socket breaks or the app has no push key (it is not an iPhone or an iPad).
    pub async fn push_register(&mut self, token: &[u8]) -> Result<Vec<u8>> {
        let command = Command::PushRegister { token: token.to_vec() };
        match self.call(&command).await? {
            Reply::PushKey { key } => Ok(key),
            Reply::Error { message } => bail!("{command:?}: {message}"),
            other => bail!("{command:?}: unexpected {other:?}"),
        }
    }

    /// The note the push `payload` opens to, opened as the notification extension opens it.
    ///
    /// # Errors
    ///
    /// When the socket breaks or the push does not open: no sealed body, no key or token kept,
    /// the Keychain not shared, or a body sealed to another phone.
    pub async fn open_push(&mut self, payload: &str) -> Result<crate::DeliveredNote> {
        match self.call(&Command::OpenPush { payload: payload.to_owned() }).await? {
            Reply::Opened { note } => Ok(note),
            Reply::Error { message } => bail!("OpenPush: {message}"),
            other => bail!("OpenPush: unexpected {other:?}"),
        }
    }

    /// Press `action` ("allow" or "deny") on an approval note carrying `info`, as a press with
    /// no window is answered: through the server, over a link of the press's own.
    ///
    /// # Errors
    ///
    /// When the socket breaks, or the server did not take the answer (in its words).
    pub async fn press_note(
        &mut self,
        action: &str,
        info: std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        self.ok(&Command::PressNote { action: action.to_owned(), info }).await
    }

    /// The notes the Notification Centre shows for the app.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn delivered(&mut self) -> Result<Vec<crate::DeliveredNote>> {
        match self.call(&Command::Delivered).await? {
            Reply::Delivered { notes } => Ok(notes),
            Reply::Error { message } => bail!("Delivered: {message}"),
            other => bail!("Delivered: unexpected {other:?}"),
        }
    }

    /// The app's state.
    ///
    /// # Errors
    ///
    /// When the socket breaks, or when the frame the app drew for the dump differs from the
    /// same state drawn from scratch ([`Dump::stale`]).
    pub async fn dump(&mut self) -> Result<Dump> {
        match self.call(&Command::Dump).await? {
            Reply::Dump(dump) => match &dump.stale {
                Some(stale) => bail!("the app drew a stale frame: {stale}"),
                None => Ok(*dump),
            },
            Reply::Error { message } => bail!("dump: {message}"),
            other => bail!("dump: unexpected {other:?}"),
        }
    }

    /// The app's state while something on screen moves on its own (a working mark's steps,
    /// with `SLOPTY_E2E_MOTION`): the frame the dump drew may be a step behind the same state
    /// drawn from scratch a moment later, which [`Dump::stale`] then says, and the tree is still
    /// the state's.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn dump_moving(&mut self) -> Result<Dump> {
        match self.call(&Command::Dump).await? {
            Reply::Dump(dump) => Ok(*dump),
            Reply::Error { message } => bail!("dump: {message}"),
            other => bail!("dump: unexpected {other:?}"),
        }
    }

    /// Poll [`Driver::dump`] until `pred` holds or `timeout` passes; the last dump's words are in
    /// the error so a failure says what the app was showing.
    ///
    /// # Errors
    ///
    /// On timeout, with the last dump.
    pub async fn wait_for(
        &mut self,
        what: &str,
        timeout: Duration,
        mut pred: impl FnMut(&Dump) -> bool,
    ) -> Result<Dump> {
        // The deadline is checked between dumps, never inside one: a dump cut off mid-call would
        // leave its answer on the socket, read as the answer to the next command.
        let since = tokio::time::Instant::now();
        loop {
            let dump = self.dump().await?;
            if pred(&dump) {
                return Ok(dump);
            }
            if since.elapsed() >= timeout {
                let terminals: Vec<String> =
                    dump.terminals.iter().map(|t| t.lines.join("\n")).collect();
                bail!(
                    "timed out waiting for {what}; the app ({}, focus {}) said:\n{}{}",
                    dump.status,
                    dump.focused,
                    crate::frame_text(&dump.a11y),
                    terminals.join("\n--\n")
                );
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Render the current frame to `path` and load it back, with what the frame says in words.
    ///
    /// In a design review (`--review`) the same frame is also drawn at 2x beside the 1x renders
    /// in the artifacts, as `<name>@2x.png`: what a Retina Mac shows, which is what the person
    /// judges by. A window that already draws at 2x or more (a Retina Mac, the iOS simulator) is
    /// copied there as it is, since its render is that picture. A golden stays at the window's
    /// scale and passes or fails on its numbers.
    ///
    /// # Errors
    ///
    /// When the app was built without the `e2e` feature, or the file cannot be read.
    pub async fn render(&mut self, path: &Path) -> Result<Frame> {
        let frame = self.rendered(path, None).await?;
        if crate::snapshot::Accept::from_env() == crate::snapshot::Accept::Review {
            self.review_at_2x(path, frame.scale).await?;
        }
        Ok(frame)
    }

    /// The render at `path`, drawn at `scale`, into the artifacts as `<name>@2x.png`: drawn again
    /// at 2x when the window draws at less, else the render itself.
    async fn review_at_2x(&mut self, path: &Path, scale: f32) -> Result<()> {
        let dir = crate::harness::artifacts_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let name = path.file_stem().and_then(|n| n.to_str()).context("a render names a file")?;
        let at = dir.join(format!("{name}@2x.png"));
        if scale >= 2.0 {
            std::fs::copy(path, &at).with_context(|| format!("copy to {}", at.display()))?;
        } else {
            let _frame = self.rendered(&at, Some(2.0)).await?;
        }
        Ok(())
    }

    /// [`Self::render`] at `scale` device pixels to the point rather than the window's own:
    /// the Retina picture of a window on a 1x display, drawn afresh at that scale.
    ///
    /// # Errors
    ///
    /// As [`Self::render`].
    pub async fn render_at(&mut self, path: &Path, scale: f32) -> Result<Frame> {
        self.rendered(path, Some(scale)).await
    }

    async fn rendered(&mut self, path: &Path, scale: Option<f32>) -> Result<Frame> {
        let path_str = path.to_str().context("render path is not UTF-8")?.to_owned();
        match self.call(&Command::Render { path: path_str, scale }).await? {
            Reply::Rendered { width, height, a11y, scale } => {
                let img = image::open(path)
                    .with_context(|| format!("read {}", path.display()))?
                    .into_rgba8();
                anyhow::ensure!(
                    img.dimensions() == (width, height),
                    "rendered {width}×{height}, file is {:?}",
                    img.dimensions()
                );
                Ok(Frame { image: img, a11y, scale })
            }
            Reply::Error { message } => bail!("render: {message}"),
            other => bail!("render: unexpected {other:?}"),
        }
    }
}

/// `bytes` in standard base64 with padding, as the test socket's [`Command::Clipboard`] takes
/// them.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .chain(std::iter::repeat(&0))
            .take(3)
            .fold(0_u32, |n, byte| n.wrapping_shl(8) | u32::from(*byte));
        for (at, shift) in [18_u32, 12, 6, 0].into_iter().enumerate() {
            let sextet = usize::try_from(n.wrapping_shr(shift) & 63).unwrap_or_default();
            match ALPHABET.get(sextet) {
                Some(&letter) if at <= chunk.len() => out.push(char::from(letter)),
                _ => out.push('='),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_as_the_standard_does() {
        for (bytes, said) in
            [("", ""), ("M", "TQ=="), ("Ma", "TWE="), ("Man", "TWFu"), ("Many", "TWFueQ==")]
        {
            assert_eq!(super::base64(bytes.as_bytes()), said, "{bytes:?}");
        }
    }
}
