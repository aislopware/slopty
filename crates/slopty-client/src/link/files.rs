//! File tile texts too big for the control stream, both ways.
//!
//! Down: the worker announces a large read on the control stream (`FileRead::Streamed`) and
//! sends its text on a bulk stream of the same transfer. The two arrive in either order, so
//! [`Join`] holds whichever comes first and hands on a plain `FileRead::Text` when both are
//! here, in the place the announcement took among the control stream's messages: a later read
//! of the same path, announced or inline, supersedes one still arriving, and its text is
//! dropped when it lands.
//!
//! Up: a save too big to inline goes as a bulk stream ([`send_save`]), and the worker answers
//! it on the control stream as it answers an inline one.

use std::collections::HashMap;

use slopty_core::{WallMs, XferId};
use slopty_net::streams::{self, RawRecv};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::file::{FILE_BYTES, FileRead, WriteResult};
use slopty_proto::transfer::{BulkHeader, Purpose};

/// Bytes read from a bulk stream at a time.
const CHUNK: usize = 256 << 10;

/// What a streamed read's announcement says of the file, beside its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Meta {
    size: u64,
    modified_ms: WallMs,
    final_newline: bool,
}

/// The streamed reads of one link, between their two halves.
#[derive(Debug, Default)]
pub(super) struct Join {
    /// The newest read of each path the control stream announced a stream for.
    latest: HashMap<String, XferId>,
    /// Announced, the text still on its way.
    announced: HashMap<XferId, (String, Meta)>,
    /// Texts (or why none came) whose announcement is still on its way.
    early: HashMap<XferId, Result<String, String>>,
}

impl Join {
    /// A read on the control stream: what to hand on now, if anything. A streamed one waits for
    /// its text unless that came first.
    pub(super) fn on_read(&mut self, path: String, read: FileRead) -> Option<WorkerMsg> {
        let FileRead::Streamed { xfer, size, modified_ms, final_newline } = read else {
            self.latest.remove(&path);
            return Some(WorkerMsg::File { path, read });
        };
        let meta = Meta { size, modified_ms, final_newline };
        if let Some(text) = self.early.remove(&xfer) {
            self.latest.remove(&path);
            return Some(joined(path, meta, text));
        }
        self.latest.insert(path.clone(), xfer);
        self.announced.insert(xfer, (path, meta));
        None
    }

    /// The text of transfer `xfer` came off its bulk stream, or the stream broke: the read to
    /// hand on, when it was announced and nothing newer was since.
    pub(super) fn on_text(
        &mut self,
        xfer: XferId,
        text: Result<String, String>,
    ) -> Option<WorkerMsg> {
        let Some((path, meta)) = self.announced.remove(&xfer) else {
            self.early.insert(xfer, text);
            return None;
        };
        if self.latest.get(&path) != Some(&xfer) {
            return None;
        }
        self.latest.remove(&path);
        Some(joined(path, meta, text))
    }
}

/// The read a streamed one makes once its text is here: the text, or a missing file with why
/// the stream broke.
fn joined(path: String, meta: Meta, text: Result<String, String>) -> WorkerMsg {
    let read = match text {
        Ok(text) => FileRead::Text {
            text,
            size: meta.size,
            modified_ms: meta.modified_ms,
            final_newline: meta.final_newline,
        },
        Err(error) => FileRead::Missing { error },
    };
    WorkerMsg::File { path, read }
}

/// A streamed read's text off its bulk stream: exactly the bytes its header announces, at most
/// [`FILE_BYTES`], as UTF-8.
///
/// # Errors
///
/// For a person: the stream broke, carried more or less than it said, or was not text.
pub(super) async fn read_text(header: &BulkHeader, rx: &mut RawRecv) -> Result<String, String> {
    if header.size > FILE_BYTES {
        rx.stop();
        return Err(format!("The worker sent {} bytes, past the cap", header.size));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(header.size).unwrap_or(0));
    loop {
        match rx.chunk(CHUNK).await {
            Ok(Some(chunk)) => {
                bytes.extend_from_slice(&chunk);
                if bytes.len() as u64 > header.size {
                    rx.stop();
                    return Err("The read ran past its size".to_owned());
                }
            }
            Ok(None) => break,
            Err(e) => return Err(format!("The read was cut off: {e}")),
        }
    }
    if bytes.len() as u64 != header.size {
        return Err("The read was cut off".to_owned());
    }
    String::from_utf8(bytes).map_err(|_not_text| "The read was not text".to_owned())
}

/// A save too big for the control stream, as a bulk stream the worker writes from. The answer
/// comes as an inline save's does; a stream that could not be sent is answered here, with
/// the reason, as a failed save.
pub(super) async fn send_save(
    conn: &Connection,
    path: String,
    text: String,
    base_modified_ms: Option<WallMs>,
) -> Option<WorkerMsg> {
    let size = text.len() as u64;
    let header = BulkHeader {
        xfer: XferId::new(),
        purpose: Purpose::Save { path: path.clone(), base_modified_ms },
        name: String::new(),
        size,
        mtime_ms: WallMs::ZERO,
        mode: 0,
        offset: 0,
    };
    let sent = async {
        let mut send = streams::open_bulk(conn, header).await?;
        send.write_all(text.as_bytes()).await.map_err(|e| NetError::stream(&e))?;
        send.finish().map_err(|e| NetError::stream(&e))
    };
    match sent.await {
        Ok(()) => {
            tracing::debug!(%path, size, "save streamed");
            None
        }
        Err(e) => {
            tracing::info!(%path, size, error = %e, "save stream failed");
            let error = format!("the save did not reach the worker: {e}");
            Some(WorkerMsg::Written { path, result: WriteResult::Failed { error } })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streamed(xfer: XferId, modified_ms: WallMs) -> FileRead {
        FileRead::Streamed { xfer, size: 10, modified_ms, final_newline: true }
    }

    fn text_of(msg: Option<WorkerMsg>) -> Option<(String, String)> {
        match msg? {
            WorkerMsg::File { path, read: FileRead::Text { text, .. } } => Some((path, text)),
            _ => None,
        }
    }

    /// The announcement and the text meet whichever comes first, and make a plain text read
    /// with the announcement's size, time and newline.
    #[test]
    fn a_streamed_read_is_handed_on_once_both_halves_are_here() {
        let mut join = Join::default();
        let (a, b) = (XferId::new(), XferId::new());
        assert!(
            join.on_read("/a".to_owned(), streamed(a, WallMs::from_millis(1))).is_none(),
            "waits for the text"
        );
        let msg = join.on_text(a, Ok("alpha".to_owned()));
        assert_eq!(
            msg,
            Some(WorkerMsg::File {
                path: "/a".to_owned(),
                read: FileRead::Text {
                    text: "alpha".to_owned(),
                    size: 10,
                    modified_ms: WallMs::from_millis(1),
                    final_newline: true
                },
            })
        );
        assert!(join.on_text(b, Ok("beta".to_owned())).is_none(), "the text came first");
        assert_eq!(
            text_of(join.on_read("/b".to_owned(), streamed(b, WallMs::from_millis(2)))),
            Some(("/b".to_owned(), "beta".to_owned()))
        );
        assert!(join.latest.is_empty() && join.announced.is_empty() && join.early.is_empty());
    }

    /// A newer read of the same path supersedes one still streaming, inline or streamed: the
    /// old text is dropped when it lands, so a slow stream never puts back an older file.
    #[test]
    fn a_newer_read_of_the_path_drops_the_text_still_on_its_way() {
        let mut join = Join::default();
        let (old, new) = (XferId::new(), XferId::new());
        assert!(join.on_read("/f".to_owned(), streamed(old, WallMs::from_millis(1))).is_none());
        let gone = FileRead::Missing { error: "No such file or directory".to_owned() };
        assert!(join.on_read("/f".to_owned(), gone).is_some(), "an inline read goes on at once");
        assert!(join.on_text(old, Ok("stale".to_owned())).is_none(), "and the old text is dropped");

        assert!(join.on_read("/f".to_owned(), streamed(old, WallMs::from_millis(1))).is_none());
        assert!(join.on_read("/f".to_owned(), streamed(new, WallMs::from_millis(2))).is_none());
        assert_eq!(
            text_of(join.on_text(new, Ok("fresh".to_owned()))),
            Some(("/f".to_owned(), "fresh".to_owned()))
        );
        assert!(join.on_text(old, Ok("stale".to_owned())).is_none(), "overtaken");
        assert!(join.announced.is_empty() && join.early.is_empty());
    }

    /// A stream that broke makes the read a missing file with the reason, so a tile waiting on
    /// its first read says why instead of waiting for good.
    #[test]
    fn a_broken_stream_is_a_missing_file_with_the_reason() {
        let mut join = Join::default();
        let xfer = XferId::new();
        assert!(join.on_read("/f".to_owned(), streamed(xfer, WallMs::from_millis(1))).is_none());
        assert_eq!(
            join.on_text(xfer, Err("The read was cut off".to_owned())),
            Some(WorkerMsg::File {
                path: "/f".to_owned(),
                read: FileRead::Missing { error: "The read was cut off".to_owned() },
            })
        );
    }
}
