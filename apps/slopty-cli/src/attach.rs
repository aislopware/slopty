//! `slopty open|attach`: a raw-mode terminal client that renders frames into the local terminal.
//!
//! Keys go to the host as raw bytes (the local terminal already encoded them), so this is the
//! exact bytes-in/rows-out path the GPUI apps use minus the prediction layer. Detach with `^]`.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use rustix::termios::{self, OptionalActions, Termios};
use slopty_core::SessionId;
use slopty_grid::{Color, Line, Style, StyleFlags, Underline};
use slopty_net::{ClientMsg, HostMsg};
use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::{Frame, OpenSession, TermEvent, TermRequest, TermSize};

use crate::client::{Session, connect_to};

/// `^]` detaches.
const DETACH: u8 = 0x1d;

pub async fn open(
    data_dir: &Path,
    host: Option<&str>,
    cwd: Option<String>,
    command: Vec<String>,
) -> Result<()> {
    let mut session = connect_to(data_dir, host).await?;
    let size = local_size()?;
    session
        .conn
        .tx
        .send(&ClientMsg::OpenSession(OpenSession {
            size,
            cwd,
            command,
            env: Vec::new(),
            title: None,
            attach: true,
        }))
        .await?;
    let id = loop {
        match session.conn.rx.recv().await? {
            HostMsg::SessionOpened(s) => break s.id,
            HostMsg::Term { event: TermEvent::Error(e), .. } => bail!("host: {e}"),
            _other => {}
        }
    };
    run(session, id).await
}

pub async fn attach(data_dir: &Path, host: Option<&str>, needle: &str) -> Result<()> {
    let mut session = connect_to(data_dir, host).await?;
    let needle = needle.to_lowercase();
    let mut hits =
        session.conn.ack.sessions.iter().filter(|s| s.id.to_string().starts_with(&needle));
    let id = match (hits.next(), hits.next()) {
        (Some(s), None) => s.id,
        (None, _) => bail!("no session matches {needle}"),
        (Some(_), Some(_)) => bail!("several sessions match {needle}"),
    };
    let size = local_size()?;
    session
        .conn
        .tx
        .send(&ClientMsg::Term { session: id, req: TermRequest::Attach { size } })
        .await?;
    run(session, id).await
}

fn local_size() -> Result<TermSize> {
    let ws = termios::tcgetwinsize(std::io::stdout()).context("stdout is not a terminal")?;
    let cols = ws.ws_col.max(1);
    let rows = ws.ws_row.max(1);
    let default = TermSize::default().metrics;
    let metrics = CellMetrics {
        cell_width: ws.ws_xpixel.checked_div(cols).filter(|w| *w > 0).unwrap_or(default.cell_width),
        cell_height: ws
            .ws_ypixel
            .checked_div(rows)
            .filter(|h| *h > 0)
            .unwrap_or(default.cell_height),
    };
    Ok(TermSize { cols, rows, metrics })
}

/// Restores the terminal on drop.
struct RawGuard(Termios);

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _restored = termios::tcsetattr(std::io::stdout(), OptionalActions::Now, &self.0);
        let mut out = std::io::stdout();
        let _written = out.write_all(b"\x1b[?25h\x1b[0m\x1b[?1049l");
        let _flushed = out.flush();
    }
}

fn enter_raw() -> Result<RawGuard> {
    let stdout = std::io::stdout();
    let saved = termios::tcgetattr(&stdout).context("tcgetattr")?;
    let mut raw = saved.clone();
    raw.make_raw();
    termios::tcsetattr(&stdout, OptionalActions::Now, &raw).context("tcsetattr")?;
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1049h\x1b[2J\x1b[H")?;
    out.flush()?;
    Ok(RawGuard(saved))
}

async fn run(mut session: Session, id: SessionId) -> Result<()> {
    let (header, mut events) = session.conn.accept_session_stream().await?;
    if header.session != id {
        bail!("host opened a stream for another session");
    }
    let raw = enter_raw()?;
    let mut stdin = tokio::io::stdin();
    let mut input = vec![0_u8; 4096];
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut frames: u64 = 0;
    let outcome: Result<&'static str> = loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(TermEvent::Frame(frame)) => {
                    frames = frames.saturating_add(1);
                    paint(&frame)?;
                }
                Ok(TermEvent::Bell) => {
                    let mut out = std::io::stdout();
                    out.write_all(b"\x07")?;
                    out.flush()?;
                }
                Ok(TermEvent::Exited { status }) => {
                    break Ok(if status == 0 { "exited" } else { "exited with error" });
                }
                Ok(TermEvent::Error(e)) => break Err(anyhow::anyhow!("host: {e}")),
                Ok(_other) => {}
                Err(slopty_net::NetError::Closed) => break Ok("session stream closed"),
                Err(e) => break Err(e.into()),
            },
            read = tokio::io::AsyncReadExt::read(&mut stdin, &mut input) => {
                let n = read?;
                if n == 0 {
                    break Ok("stdin closed");
                }
                let bytes = input.get(..n).unwrap_or_default();
                if bytes.contains(&DETACH) {
                    break Ok("detached");
                }
                let req = TermRequest::Raw(bytes.to_vec());
                session.conn.tx.send(&ClientMsg::Term { session: id, req }).await?;
            }
            _sig = winch.recv() => {
                let size = local_size()?;
                let req = TermRequest::Resize(size);
                session.conn.tx.send(&ClientMsg::Term { session: id, req }).await?;
            }
            msg = session.conn.rx.recv() => match msg {
                Ok(HostMsg::SessionClosed { session: s, reason }) if s == id => {
                    break Ok(match reason {
                        slopty_proto::terminal::CloseReason::Requested => "closed",
                        slopty_proto::terminal::CloseReason::Exited => "exited",
                        slopty_proto::terminal::CloseReason::HostShutdown => "host shut down",
                    });
                }
                Ok(_other) => {}
                Err(e) => break Err(e.into()),
            },
        }
    };
    drop(raw);
    let rtt = session.conn.rtt().map_or_else(|| "?".to_owned(), |d| format!("{d:.1?}"));
    let why = outcome?;
    println!("[{why}; {frames} frames; rtt {rtt}]");
    session.conn.tx.send(&ClientMsg::Term { session: id, req: TermRequest::Detach }).await?;
    session.close().await;
    Ok(())
}

/// Draw one frame's changed rows and place the cursor.
fn paint(frame: &Frame) -> Result<()> {
    let mut buf = Vec::with_capacity(8192);
    buf.extend_from_slice(b"\x1b[?25l");
    for update in &frame.updates {
        let row = u32::from(update.row).saturating_add(1);
        buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        paint_line(&mut buf, &update.line);
        buf.extend_from_slice(b"\x1b[0m\x1b[K");
    }
    let (r, c) = (
        u32::from(frame.cursor.row).saturating_add(1),
        u32::from(frame.cursor.col).saturating_add(1),
    );
    buf.extend_from_slice(format!("\x1b[{r};{c}H").as_bytes());
    if frame.cursor.visible {
        buf.extend_from_slice(b"\x1b[?25h");
    }
    let mut out = std::io::stdout();
    out.write_all(&buf)?;
    out.flush()?;
    Ok(())
}

fn paint_line(buf: &mut Vec<u8>, line: &Line) {
    let mut current = Style::DEFAULT;
    buf.extend_from_slice(b"\x1b[0m");
    for cell in &line.cells {
        if !cell.width.draws_text() {
            continue;
        }
        if cell.style != current {
            sgr(buf, &cell.style);
            current = cell.style;
        }
        if cell.text.is_empty() {
            buf.push(b' ');
        } else {
            buf.extend_from_slice(cell.text.as_str().as_bytes());
        }
    }
}

fn sgr(buf: &mut Vec<u8>, style: &Style) {
    let mut params: Vec<String> = vec!["0".to_owned()];
    let flags = [
        (StyleFlags::BOLD, "1"),
        (StyleFlags::FAINT, "2"),
        (StyleFlags::ITALIC, "3"),
        (StyleFlags::BLINK, "5"),
        (StyleFlags::INVERSE, "7"),
        (StyleFlags::INVISIBLE, "8"),
        (StyleFlags::STRIKETHROUGH, "9"),
    ];
    for (flag, code) in flags {
        if style.flags.contains(flag) {
            params.push(code.to_owned());
        }
    }
    match style.underline {
        Underline::None => {}
        Underline::Single => params.push("4".to_owned()),
        Underline::Double => params.push("4:2".to_owned()),
        Underline::Curly => params.push("4:3".to_owned()),
        Underline::Dotted => params.push("4:4".to_owned()),
        Underline::Dashed => params.push("4:5".to_owned()),
    }
    color(&mut params, 38, style.fg);
    color(&mut params, 48, style.bg);
    color(&mut params, 58, style.underline_color);
    buf.extend_from_slice(format!("\x1b[{}m", params.join(";")).as_bytes());
}

fn color(params: &mut Vec<String>, base: u8, c: Color) {
    match c {
        Color::Default => {}
        Color::Palette(n) => params.push(format!("{base};5;{n}")),
        Color::Rgb(r, g, b) => params.push(format!("{base};2;{r};{g};{b}")),
    }
}
