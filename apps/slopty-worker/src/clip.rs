//! The worker's pasteboard, looked at only while a client wants it, and the clipboard requests
//! of the programs in its sessions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_input::pasteboard::{Board, Item, board_type, format_of};
use slopty_proto::WorkerMsg;
use slopty_proto::ctl::{ClipAsk, CtlReply, Selection};
use slopty_proto::transfer::{ClipFormat, ClipMsg};
use slopty_worker::clip::{Interest, MAX_REP_BYTES};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::Daemon;

/// How often the pasteboard is looked at while a client watches it: one Mach call to the
/// pasteboard server for the change count, and a read only when it moved.
const WATCHED: Duration = Duration::from_millis(50);

/// How often the change count alone is read while clients are linked but none watches, so a
/// copy made on the worker meanwhile is known to be newer than theirs.
const LINKED: Duration = Duration::from_millis(250);

/// Announce every change of the worker pasteboard while some client watches it, note changes
/// while clients are only linked, and sleep, reading nothing, while none is. Each look also
/// clears a secret a client pasted once its time is up. `conn` forwards an offer only to the
/// clients that watch.
pub async fn watch(daemon: Daemon) {
    let mut interest = daemon.clip.interest();
    loop {
        let now = *interest.borrow_and_update();
        let period = match now {
            Interest::Idle => {
                if interest.changed().await.is_err() {
                    return;
                }
                continue;
            }
            Interest::Linked => LINKED,
            Interest::Watched => WATCHED,
        };
        tokio::select! {
            () = tokio::time::sleep(period) => {}
            changed = interest.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
        }
        let clip = Arc::clone(&daemon.clip);
        // Off the runtime: the pasteboard server answers in its own time.
        let looked = tokio::task::spawn_blocking(move || {
            let at = Instant::now();
            clip.expire(at);
            if now == Interest::Watched {
                clip.poll(at)
            } else {
                clip.observe(at);
                None
            }
        });
        if let Ok(Some(offer)) = looked.await {
            tracing::debug!(
                generation = offer.generation,
                items = offer.items.len(),
                concealed = offer.concealed,
                "worker clipboard changed"
            );
            let _sent = daemon.events.send(WorkerMsg::Clip(ClipMsg::Offer(offer)));
        }
    }
}

/// The X11 names of text, which `xclip` and `xsel` ask for and list beside the MIME type.
const X11_TEXT: [&str; 4] = ["text/plain", "UTF8_STRING", "STRING", "TEXT"];

/// The format a type named the X11 or Wayland way is, when it is one.
fn format_named(kind: &str) -> Option<ClipFormat> {
    if X11_TEXT.contains(&kind) {
        return Some(ClipFormat::Text);
    }
    ClipFormat::ALL.into_iter().find(|format| format.mime().eq_ignore_ascii_case(kind))
}

/// The type on the board a program's `kind` reads or writes.
fn on_board(kind: &str) -> String {
    format_named(kind).map_or_else(|| kind.to_owned(), board_type)
}

/// What a program's `TARGETS` or `--list-types` shows of `items`: the first item's types named
/// as X11 and Wayland name them, text with its X11 names, and nothing that is not a MIME type
/// (the markers clipboard sync stamps, or an Apple type on a Mac).
fn listed(items: &[Vec<String>]) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    for kind in items.first().into_iter().flatten() {
        let names: Vec<&str> = match format_of(kind) {
            Some(ClipFormat::Text) => {
                std::iter::once(ClipFormat::Text.mime()).chain(X11_TEXT).collect()
            }
            Some(format) => vec![format.mime()],
            None if kind.contains('/') => vec![kind.as_str()],
            None => Vec::new(),
        };
        for name in names {
            if !types.iter().any(|t| t == name) {
                types.push(name.to_owned());
            }
        }
    }
    types
}

/// The bytes a read of `kind` gets: every item's, a line each, for a `text/uri-list` (one file
/// a item, as a Mac copies them), else the first item's.
fn read(board: &dyn Board, kind: &str) -> Option<Vec<u8>> {
    let format = format_named(kind);
    let wanted = on_board(kind);
    if format != Some(ClipFormat::FileUrls) {
        return board.data(0, &wanted);
    }
    let lines: Vec<Vec<u8>> = board
        .items()
        .iter()
        .enumerate()
        .filter(|(_, types)| types.contains(&wanted))
        .filter_map(|(n, _)| board.data(n, &wanted))
        .collect();
    (!lines.is_empty()).then(|| lines.join(&b"\r\n"[..]))
}

/// What a program's copy of `bytes` as `kind` puts on the board: a `text/uri-list` as one item
/// a line, as a Mac's copy of files is, else one item.
fn items_of(kind: &str, bytes: Vec<u8>) -> Vec<Item> {
    let wanted = on_board(kind);
    if format_named(kind) != Some(ClipFormat::FileUrls) {
        return vec![Item::data(vec![(wanted, bytes)])];
    }
    bytes
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .map(|line| Item::data(vec![(wanted.clone(), line.to_vec())]))
        .collect()
}

/// A program in a session reading or writing a clipboard ([`CtlRequest::Clip`]): the synced
/// one, or the primary selection, which stays here. A write's bytes follow the request line on
/// `rd`; a read's follow the reply line on `wr`. Reading the synced board waits on the client
/// in front while it fetches what that client copied, so it runs off the runtime.
///
/// [`CtlRequest::Clip`]: slopty_proto::ctl::CtlRequest::Clip
pub async fn answer(
    daemon: &Daemon,
    ask: ClipAsk,
    rd: &mut BufReader<OwnedReadHalf>,
    wr: &mut OwnedWriteHalf,
) -> anyhow::Result<()> {
    let answer = match ask {
        ClipAsk::Types { selection } => {
            on(daemon, selection, |board| {
                Answer::Reply(CtlReply::ClipTypes { types: listed(&board.items()) })
            })
            .await?
        }
        ClipAsk::Read { selection: Selection::Clipboard, .. }
            if !daemon.clip.access().reads_freely() =>
        {
            Answer::Reply(refused(
                "reading this Mac's pasteboard would ask the person; use pbpaste",
            ))
        }
        ClipAsk::Read { selection, kind } => {
            on(daemon, selection, move |board| match read(board, &kind) {
                Some(bytes) => Answer::Data(bytes),
                None => Answer::Reply(refused(&format!("the clipboard holds no {kind}"))),
            })
            .await?
        }
        ClipAsk::Write { len, .. } if len > MAX_REP_BYTES => {
            Answer::Reply(refused(&format!("{len} bytes is past the clipboard's {MAX_REP_BYTES}")))
        }
        ClipAsk::Write { selection, kind, len } => {
            let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            let got = (&mut *rd).take(len).read_to_end(&mut bytes).await?;
            if u64::try_from(got).ok() != Some(len) {
                anyhow::bail!("the copy ended after {got} of {len} bytes");
            }
            on(daemon, selection, move |board| written(board.write(&items_of(&kind, bytes), None)))
                .await?
        }
        ClipAsk::Clear { selection } => {
            on(daemon, selection, |board| written(board.write(&[], None))).await?
        }
    };
    let (reply, body) = match answer {
        Answer::Reply(reply) => (reply, Vec::new()),
        Answer::Data(bytes) => {
            (CtlReply::ClipData { len: u64::try_from(bytes.len()).unwrap_or(u64::MAX) }, bytes)
        }
    };
    let mut line = serde_json::to_vec(&reply)?;
    line.push(b'\n');
    wr.write_all(&line).await?;
    wr.write_all(&body).await?;
    wr.shutdown().await?;
    Ok(())
}

/// `f` on the board `selection` names, off the runtime: a read may wait on a client.
async fn on<R: Send + 'static>(
    daemon: &Daemon,
    selection: Selection,
    f: impl FnOnce(&dyn Board) -> R + Send + 'static,
) -> anyhow::Result<R> {
    let clip = Arc::clone(&daemon.clip);
    let primary = Arc::clone(&daemon.primary);
    Ok(tokio::task::spawn_blocking(move || match selection {
        Selection::Clipboard => f(clip.board()),
        Selection::Primary => f(&*primary),
    })
    .await?)
}

/// What a clipboard request answers: a reply alone, or a read's bytes after their line.
enum Answer {
    Reply(CtlReply),
    Data(Vec<u8>),
}

fn refused(message: &str) -> CtlReply {
    CtlReply::Error { message: message.to_owned() }
}

/// How a write went, as its reply.
fn written(count: Option<isize>) -> Answer {
    Answer::Reply(count.map_or_else(
        || refused("the clipboard refused the write"),
        |_count| CtlReply::Ok { changed: true },
    ))
}

#[cfg(test)]
mod tests {
    use slopty_input::pasteboard::{Board as _, Held, Item, ORIGIN_TYPE, board_type};
    use slopty_proto::transfer::ClipFormat;

    use super::{X11_TEXT, items_of, listed, on_board, read};

    /// A client's copy as clipboard sync mirrors it: text and a picture, stamped with its
    /// origin. The commands see MIME types, text under its X11 names too, and no marker.
    #[test]
    fn the_commands_list_mime_types_and_texts_x11_names() {
        let (text, png) = (board_type(ClipFormat::Text), board_type(ClipFormat::Png));
        let item = Item::data(vec![
            (png, b"png".to_vec()),
            (text, b"caption".to_vec()),
            (ORIGIN_TYPE.to_owned(), vec![1]),
            ("application/x-thing".to_owned(), vec![2]),
        ]);
        let board = Held::default();
        board.write(&[item], None).unwrap();
        let mut want = vec!["image/png", ClipFormat::Text.mime()];
        want.extend(X11_TEXT);
        want.push("application/x-thing");
        assert_eq!(listed(&board.items()), want);
        assert_eq!(listed(&[]), Vec::<String>::new());
    }

    /// Text read by any of its names is the board's text; a file list is every item's file, a
    /// line each; a copied file list becomes one item a file, comments and blank lines left out.
    #[test]
    fn texts_names_meet_and_file_lists_are_a_line_a_file() {
        let board = Held::default();
        board.write(&items_of("UTF8_STRING", b"hi".to_vec()), None).unwrap();
        for name in ["text/plain;charset=utf-8", "text/plain", "STRING", "TEXT", "UTF8_STRING"] {
            assert_eq!(read(&board, name).as_deref(), Some(&b"hi"[..]), "{name}");
        }
        assert_eq!(read(&board, "image/png"), None);

        let list = b"# copied\r\nfile:///home/a.txt\r\n\r\nfile:///home/b%20c.png\n".to_vec();
        let items = items_of("text/uri-list", list);
        let urls = board_type(ClipFormat::FileUrls);
        assert_eq!(
            items,
            vec![
                Item::data(vec![(urls.clone(), b"file:///home/a.txt".to_vec())]),
                Item::data(vec![(urls, b"file:///home/b%20c.png".to_vec())]),
            ]
        );
        board.write(&items, None).unwrap();
        assert_eq!(
            read(&board, "text/uri-list").as_deref(),
            Some(&b"file:///home/a.txt\r\nfile:///home/b%20c.png"[..])
        );
        assert_eq!(on_board("image/PNG"), board_type(ClipFormat::Png), "MIME types ignore case");
        assert_eq!(on_board("application/json"), "application/json");
    }
}
