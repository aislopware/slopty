//! The client side of the test socket.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::{
    Button, Command, Dump, Reply, UiGesturePhase, UiPressPhase, UiTouchPhase, UiTouchPoint, hid,
};

/// How often [`Driver::wait_for`] polls.
const POLL: Duration = Duration::from_millis(100);

/// How long one command may take to be answered (a frame, or a pairing round trip).
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a described finger rests before it lifts: longer than the 40 ms of silence after
/// which gpui's touch recognizer takes a finger for stopped.
const REST: Duration = Duration::from_millis(100);

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
        self.ok(&Command::Drag { x, y, to_x, to_y }).await
    }

    /// Scroll `dx`, `dy` lines at a window point; with `zoom`, ⌘ is held and the canvas zooms.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn scroll(&mut self, x: f32, y: f32, dx: f32, dy: f32, zoom: bool) -> Result<()> {
        self.ok(&Command::Scroll { x, y, dx, dy, zoom }).await
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

    /// One finger down at `from`, moved to `to` in `steps`, resting [`REST`] before it lifts
    /// so the release carries no velocity (no fling), and lifted (iOS): a pan once GPUI
    /// recognizes it.
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_pan(&mut self, from: (f32, f32), to: (f32, f32), steps: u32) -> Result<()> {
        let at = |x: f32, y: f32| [UiTouchPoint { id: 1, x, y }];
        self.ui_touch(&at(from.0, from.1), UiTouchPhase::Began).await?;
        #[expect(clippy::cast_precision_loss, reason = "a handful of steps")]
        let n = steps.max(1) as f32;
        for i in 1..=steps.max(1) {
            #[expect(clippy::cast_precision_loss, reason = "a handful of steps")]
            let t = i as f32 / n;
            let (x, y) = ((to.0 - from.0).mul_add(t, from.0), (to.1 - from.1).mul_add(t, from.1));
            self.ui_touch(&at(x, y), UiTouchPhase::Moved).await?;
        }
        // A finger that stops reports nothing until it lifts (UIKit sends no `touchesMoved:`
        // for a still touch), and that silence is what gpui's recognizer reads as stopped:
        // stationary reports would keep its velocity fit alive and fling the content.
        tokio::time::sleep(REST).await;
        self.ui_touch(&at(to.0, to.1), UiTouchPhase::Ended).await
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

    /// The text system's `deleteBackward` (iOS).
    ///
    /// # Errors
    ///
    /// When the app is not on iOS or the socket breaks.
    pub async fn ui_delete_backward(&mut self) -> Result<()> {
        self.ok(&Command::UiDeleteBackward).await
    }

    /// Open `count` sessions running `command` on the active canvas.
    ///
    /// # Errors
    ///
    /// When the socket breaks or there is no canvas.
    pub async fn open(&mut self, command: &[&str], count: u32) -> Result<()> {
        let command = command.iter().map(|s| (*s).to_owned()).collect();
        self.ok(&Command::Open { command, count }).await
    }

    /// Add the host's first display to the canvas.
    ///
    /// # Errors
    ///
    /// When the socket breaks or there is no canvas.
    pub async fn add_display(&mut self) -> Result<()> {
        self.ok(&Command::AddDisplay).await
    }

    /// Start a fresh frame-time window.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn frames_reset(&mut self) -> Result<()> {
        self.ok(&Command::FramesReset).await
    }

    /// Bring `session`'s terminal into view, active and holding the keyboard (a phone zooms the
    /// card up to a live grid, which a click cannot).
    ///
    /// # Errors
    ///
    /// When the socket breaks or there is no canvas.
    pub async fn reveal(&mut self, session: &str) -> Result<()> {
        self.ok(&Command::Reveal { session: session.to_owned() }).await
    }

    /// Activate an agent banner: reveal `tag`'s session (no action) or answer its prompt
    /// (`allow` / `deny`), switching hosts as the response handler does.
    ///
    /// # Errors
    ///
    /// When the socket breaks or the session is on no host.
    pub async fn notification_response(&mut self, tag: &str, action: Option<&str>) -> Result<()> {
        self.ok(&Command::NotificationResponse {
            tag: tag.to_owned(),
            action: action.map(str::to_owned),
        })
        .await
    }

    /// The app's state.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn dump(&mut self) -> Result<Dump> {
        match self.call(&Command::Dump).await? {
            Reply::Dump(dump) => Ok(*dump),
            Reply::Error { message } => bail!("dump: {message}"),
            other => bail!("dump: unexpected {other:?}"),
        }
    }

    /// Poll [`Driver::dump`] until `pred` holds or `timeout` passes; the last dump is in the
    /// error so a failure says what the app was showing.
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
        let mut last: Option<Dump> = None;
        let polled = tokio::time::timeout(timeout, async {
            loop {
                let dump = self.dump().await?;
                if pred(&dump) {
                    return Ok(dump);
                }
                last = Some(dump);
                tokio::time::sleep(POLL).await;
            }
        })
        .await;
        match polled {
            Ok(result) => result,
            Err(_elapsed) => bail!("timed out waiting for {what}; last state:\n{last:#?}"),
        }
    }

    /// Render the current frame to `path` and load it back.
    ///
    /// # Errors
    ///
    /// When the app was built without the `e2e` feature, or the file cannot be read.
    pub async fn render(&mut self, path: &Path) -> Result<image::RgbaImage> {
        let path_str = path.to_str().context("render path is not UTF-8")?.to_owned();
        match self.call(&Command::Render { path: path_str }).await? {
            Reply::Rendered { width, height } => {
                let img = image::open(path)
                    .with_context(|| format!("read {}", path.display()))?
                    .into_rgba8();
                anyhow::ensure!(
                    img.dimensions() == (width, height),
                    "rendered {width}×{height}, file is {:?}",
                    img.dimensions()
                );
                Ok(img)
            }
            Reply::Error { message } => bail!("render: {message}"),
            other => bail!("render: unexpected {other:?}"),
        }
    }
}
