//! `slopty attach`: a raw-mode terminal client that renders frames into the local terminal.
//!
//! Keys go to the worker as raw bytes (the local terminal already encoded them), so this is the
//! exact bytes-in/rows-out path the GPUI apps use minus the prediction layer. Detach with `^]`.
//!
//! As with `ssh`, a program that exits ends the command with its status, so a script can run
//! `slopty attach -- make test` and branch on it. The session stays, exited, until it is closed.

use std::io::{Read, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use rustix::termios::{self, OptionalActions, Termios};
use slopty_client::{Effect, LinkEvent, TermState, WorkerLink};
use slopty_core::SessionId;
use slopty_grid::{Color, Line, Style, StyleFlags, Underline};
use slopty_net::{ClientMsg, WorkerMsg};
use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::{CloseReason, OpenSession, TermEvent, TermRequest, TermSize};

use crate::client::{Session, connect_to};

/// `^]` detaches.
const DETACH: u8 = 0x1d;

pub async fn open(
    data_dir: &Path,
    worker: Option<&str>,
    cwd: Option<String>,
    command: Vec<String>,
) -> Result<ExitCode> {
    let mut session = connect_to(data_dir, worker).await?;
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
            WorkerMsg::SessionOpened(s) => break s.id,
            WorkerMsg::Term { event: TermEvent::Error(e), .. } => bail!("worker: {e}"),
            _other => {}
        }
    };
    run(session, id).await
}

pub async fn attach(data_dir: &Path, worker: Option<&str>, needle: &str) -> Result<ExitCode> {
    let mut session = connect_to(data_dir, worker).await?;
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

async fn run(session: Session, id: SessionId) -> Result<ExitCode> {
    let Session { conn, endpoint, .. } = session;
    let size = local_size()?;
    let mut link = WorkerLink::start(conn);
    let mut events = link.events().context("events taken")?;
    let mut state = TermState::new(size);
    let raw = enter_raw()?;
    let mut stdin = read_on_a_thread(std::io::stdin())?;
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let outcome: Result<(String, u8)> = loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(LinkEvent::Term { session, event }) if session == id => {
                    let mut done = None;
                    for effect in state.apply(event) {
                        match effect {
                            Effect::Request(req) => {
                                link.send(ClientMsg::Term { session: id, req }).await?;
                            }
                            Effect::Bell => {
                                let mut out = std::io::stdout();
                                out.write_all(b"\x07")?;
                                out.flush()?;
                            }
                            Effect::Notification { title, body } => {
                                let mut out = std::io::stdout();
                                out.write_all(relay_notification(&title, &body).as_bytes())?;
                                out.flush()?;
                            }
                            Effect::Exited(status) => done = Some(exited(status)),
                            Effect::Error(e) => tracing::warn!(error = %e, "worker error"),
                            Effect::Title(_)
                            | Effect::Cwd { .. }
                            | Effect::ClipboardWrite(_)
                            | Effect::Matches { .. }
                            | Effect::SearchInvalid { .. }
                            | Effect::CommandStarted(_)
                            | Effect::CommandFinished { .. } => {}
                        }
                    }
                    paint(&state)?;
                    if let Some(why) = done {
                        break Ok(why);
                    }
                }
                Some(LinkEvent::Control(WorkerMsg::SessionClosed { session, reason })) if session == id => {
                    break Ok((
                        match reason {
                            CloseReason::Requested => "closed",
                            CloseReason::Exited => "exited",
                            CloseReason::WorkerShutdown => "worker shut down",
                        }
                        .to_owned(),
                        0,
                    ));
                }
                Some(
                    LinkEvent::Control(_)
                    | LinkEvent::Term { .. }
                    | LinkEvent::Ports { .. }
                    | LinkEvent::XferFailed { .. },
                ) => {}
                Some(LinkEvent::Disconnected(why)) => break Err(anyhow::anyhow!("disconnected: {why}")),
                None => break Ok(("link closed".to_owned(), 0)),
            },
            read = stdin.recv() => {
                let Some(bytes) = read.transpose()? else {
                    break Ok(("stdin closed".to_owned(), 0));
                };
                if bytes.contains(&DETACH) {
                    break Ok(("detached".to_owned(), 0));
                }
                link.send(ClientMsg::Term { session: id, req: TermRequest::Raw(bytes) }).await?;
            }
            _sig = winch.recv() => {
                for effect in state.resize(local_size()?) {
                    if let Effect::Request(req) = effect {
                        link.send(ClientMsg::Term { session: id, req }).await?;
                    }
                }
            }
        }
    };
    drop(raw);
    let rtt = link.rtt().map_or_else(|| "?".to_owned(), |d| format!("{d:.1?}"));
    let (why, code) = outcome?;
    println!("[{why}; {} frames; rtt {rtt}]", state.frames());
    let _detached = link.send(ClientMsg::Term { session: id, req: TermRequest::Detach }).await;
    link.close();
    crate::client::close_endpoint(&endpoint).await;
    Ok(ExitCode::from(code))
}

/// Read `input` on a thread of its own, a chunk per read, until it ends (the channel closes)
/// or fails (the error, then it closes).
///
/// Not `tokio::io::stdin`: its read blocks a runtime thread that nothing can interrupt, and the
/// runtime's shutdown waits for it, so `slopty attach -- make test` would hang after the
/// program exited until a key was pressed. A plain thread is left behind when `main` returns.
fn read_on_a_thread(
    mut input: impl Read + Send + 'static,
) -> std::io::Result<tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    std::thread::Builder::new().name("slopty-stdin".to_owned()).spawn(move || {
        let mut buf = vec![0_u8; 4096];
        loop {
            let chunk = match input.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => Ok(buf.get(..n).unwrap_or_default().to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(e),
            };
            let failed = chunk.is_err();
            if tx.blocking_send(chunk).is_err() || failed {
                return;
            }
        }
    })?;
    Ok(rx)
}

/// What ends an attach when the program exits with `status`, and the code this command exits
/// with: the status itself, as a shell reports it, or 1 when it does not fit one.
fn exited(status: i32) -> (String, u8) {
    match status {
        0 => ("exited".to_owned(), 0),
        _ => {
            (format!("exited {status}"), u8::try_from(status).ok().filter(|c| *c != 0).unwrap_or(1))
        }
    }
}

/// A program's notification handed on to the terminal we sit in as OSC 777 `notify`, so it
/// posts the banner; control characters are dropped so the payload cannot end or fake
/// the sequence.
fn relay_notification(title: &str, body: &str) -> String {
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
    format!("\x1b]777;notify;{};{}\x07", clean(title), clean(body))
}

/// Redraw every row and place the cursor.
fn paint(state: &TermState) -> Result<()> {
    let mut buf = Vec::with_capacity(8192);
    buf.extend_from_slice(b"\x1b[?25l");
    for (i, row) in state.view().iter().enumerate() {
        let line_no = i.saturating_add(1);
        buf.extend_from_slice(format!("\x1b[{line_no};1H").as_bytes());
        match row.line {
            Some(line) => paint_line(&mut buf, line),
            None => buf.extend_from_slice(b"\x1b[0m\x1b[2m~"),
        }
        buf.extend_from_slice(b"\x1b[0m\x1b[K");
    }
    let cursor = state.cursor();
    let (r, c) = (u32::from(cursor.row).saturating_add(1), u32::from(cursor.col).saturating_add(1));
    buf.extend_from_slice(format!("\x1b[{r};{c}H").as_bytes());
    if cursor.visible && state.view_offset() == 0 {
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    use super::{exited, read_on_a_thread, relay_notification};

    /// Input arrives as it is typed and its end closes the channel; a read still blocked when
    /// the session ends does not hold the runtime's shutdown, so the command exits at once.
    #[test]
    fn stdin_is_read_on_a_thread_the_runtime_does_not_wait_for() {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut input = read_on_a_thread(reader).unwrap();
        writer.write_all(b"ls\r").unwrap();
        let first = runtime.block_on(input.recv()).unwrap().unwrap();
        assert_eq!(first, b"ls\r");
        // Nothing more is typed: the thread sits in `read`, as it does on a terminal.
        let started = Instant::now();
        drop(runtime);
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());

        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        let mut input = read_on_a_thread(reader).unwrap();
        drop(writer);
        assert!(runtime.block_on(input.recv()).is_none(), "an ended input closes the channel");
    }

    /// The command exits as the program did, as `ssh` does; a status no exit code can carry
    /// (a signal's negative, or past 255) is still a failure.
    #[test]
    fn an_exited_program_hands_its_status_on() {
        assert_eq!(exited(0), ("exited".to_owned(), 0));
        assert_eq!(exited(3), ("exited 3".to_owned(), 3));
        assert_eq!(exited(256), ("exited 256".to_owned(), 1));
        assert_eq!(exited(-9), ("exited -9".to_owned(), 1));
    }

    #[test]
    fn a_relayed_notification_is_one_clean_osc_777() {
        assert_eq!(relay_notification("Tests", "all green"), "\x1b]777;notify;Tests;all green\x07");
        assert_eq!(relay_notification("", "a\x1b]9;x\x07b\n"), "\x1b]777;notify;;a]9;xb\x07");
    }
}
