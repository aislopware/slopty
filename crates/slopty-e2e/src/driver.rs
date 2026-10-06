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

    /// Carry a drag of `paths` and `texts` to a window point: what it is over (`local`, or the
    /// worker's `none`, `copy`, `link`, `move`) and the drag the window carries to the worker.
    ///
    /// # Errors
    ///
    /// When the app answers an error or the socket fails.
    pub async fn drag_over(
        &mut self,
        paths: &[&Path],
        texts: &[&str],
        x: f32,
        y: f32,
    ) -> Result<(String, Option<String>)> {
        let paths = paths.iter().map(|p| p.display().to_string()).collect();
        let texts = texts.iter().map(|&t| t.to_owned()).collect();
        let command = Command::DragOver { paths, texts, x, y };
        match self.call(&command).await? {
            Reply::Over { op, drag } => Ok((op, drag)),
            Reply::Error { message } => bail!("{command:?}: {message}"),
            other => bail!("{command:?}: unexpected {other:?}"),
        }
    }

    /// Let go of the carried drag at a window point: whether the drop was taken.
    ///
    /// # Errors
    ///
    /// When the app answers an error or the socket fails.
    pub async fn drag_drop(&mut self, x: f32, y: f32) -> Result<bool> {
        let command = Command::DragDrop { x, y };
        match self.call(&command).await? {
            Reply::Taken { taken } => Ok(taken),
            Reply::Error { message } => bail!("{command:?}: {message}"),
            other => bail!("{command:?}: unexpected {other:?}"),
        }
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

    /// [`Self::drag`] with ⌘ held: on a terminal path, the file is dragged out of the app.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn cmd_drag(&mut self, x: f32, y: f32, to_x: f32, to_y: f32) -> Result<()> {
        self.ok(&Command::Drag { x, y, to_x, to_y, command: true }).await
    }

    /// Keep the promises of the last drag out of the app in the directory `into`
    /// ([`Command::KeepDragged`]); returns once every file is written.
    ///
    /// # Errors
    ///
    /// When the socket breaks, or a promise could not be kept.
    pub async fn keep_dragged(&mut self, into: &Path) -> Result<()> {
        self.ok(&Command::KeepDragged { into: into.display().to_string() }).await
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

    /// Open `count` sessions running `command` in the active workspace.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn open(&mut self, command: &[&str], count: u32) -> Result<()> {
        let command = command.iter().map(|s| (*s).to_owned()).collect();
        self.ok(&Command::Open { command, count }).await
    }

    /// Open a file tile for `path` on the worker.
    pub async fn open_file(&mut self, path: &str, line: Option<u32>) -> Result<()> {
        self.ok(&Command::OpenFile { path: path.to_owned(), line }).await
    }

    /// Add the worker's first display to the workspace.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
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

    /// Bring `session`'s terminal into view, active and holding the keyboard.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn reveal(&mut self, session: &str) -> Result<()> {
        self.ok(&Command::Reveal { session: session.to_owned() }).await
    }

    /// Show `item`'s tile, focused and holding the keyboard.
    ///
    /// # Errors
    ///
    /// When the socket breaks, or the app holds no tile of `item`.
    pub async fn focus_item(&mut self, item: &str) -> Result<()> {
        self.ok(&Command::FocusItem { item: item.to_owned() }).await
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

    /// Render the current frame to `path` and load it back, with what the frame says in words.
    ///
    /// In a design review (`--review`) the same frame is also drawn at 2x beside the 1x renders
    /// in the artifacts, as `<name>@2x.png`: what a Retina Mac shows, which is what the person
    /// judges by. A golden stays 1x and passes or fails on its numbers.
    ///
    /// # Errors
    ///
    /// When the app was built without the `e2e` feature, or the file cannot be read.
    pub async fn render(&mut self, path: &Path) -> Result<Frame> {
        let frame = self.rendered(path, None).await?;
        if crate::snapshot::Accept::from_env() == crate::snapshot::Accept::Review {
            self.review_at_2x(path).await?;
        }
        Ok(frame)
    }

    /// The render at `path` drawn again at 2x into the artifacts as `<name>@2x.png`.
    async fn review_at_2x(&mut self, path: &Path) -> Result<()> {
        let dir = crate::harness::artifacts_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let name = path.file_stem().and_then(|n| n.to_str()).context("a render names a file")?;
        let _frame = self.rendered(&dir.join(format!("{name}@2x.png")), Some(2.0)).await?;
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
