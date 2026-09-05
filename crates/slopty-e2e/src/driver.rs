//! The client side of the test socket.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::{Button, Command, Dump, Reply};

/// How often [`Driver::wait_for`] polls.
const POLL: Duration = Duration::from_millis(100);

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
        let n = self.reader.read_line(&mut answer).await.context("receive")?;
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

    /// The app's state.
    ///
    /// # Errors
    ///
    /// When the socket breaks.
    pub async fn dump(&mut self) -> Result<Dump> {
        match self.call(&Command::Dump).await? {
            Reply::Dump(dump) => Ok(dump),
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
