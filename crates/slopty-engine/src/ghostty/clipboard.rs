//! A program's clipboard reads (OSC 52 `?`, OSC 5522 `type=read`), and pastes as Kitty paste
//! events.
//!
//! A read is answered from the text the session is given to share: whatever the worker mirrors
//! while clipboard sharing is on, and nothing otherwise. A program that turned on paste events
//! (mode 5522) is told of a paste as the list of its MIME types instead of its text, and reads
//! the ones it wants with the paste's one-time password; that read is answered from the paste.
//! Every answer is decided in the callback, on the spot: a read the program waits on never
//! waits on anything itself.

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::terminal::{
    ClipboardLocation, ClipboardMime, ClipboardRead, ClipboardReadError, ClipboardReplyContent,
    Mode,
};
use libghostty_vt::{Terminal, paste};
use slopty_proto::terminal::MAX_OSC52_BYTES;

use super::GhosttyEngine;
use crate::EngineError;

/// What a program's clipboard reads are answered from. Both are asked on the engine's thread
/// inside [`GhosttyEngine::write`], so they must return at once.
pub trait ClipboardSource {
    /// The shared text now, or `None` to deny the read. A denied OSC 52 read gets an empty
    /// answer.
    fn text(&self) -> Option<String>;

    /// Whether a viewer shares its clipboard now: the primary device attributes then list
    /// clipboard access (52), so a program knows its OSC 52 reads are answered.
    fn shared(&self) -> bool;
}

/// A closure is a source that is always shared.
impl<F: Fn() -> Option<String>> ClipboardSource for F {
    fn text(&self) -> Option<String> {
        self()
    }

    fn shared(&self) -> bool {
        true
    }
}

/// One representation of a paste: its MIME type and its bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PasteRep {
    /// The MIME type the program asks for it by, such as `text/html`.
    pub mime: &'static str,
    /// Its bytes.
    pub data: Vec<u8>,
}

/// What the read callback answers from, shared with it.
#[derive(Default)]
pub(super) struct Reads {
    source: Option<Box<dyn ClipboardSource>>,
    /// The last paste event's representations, until the program reads them with its password.
    pasted: Vec<PasteRep>,
}

pub(super) type Shared = Rc<RefCell<Reads>>;

impl Reads {
    /// A viewer shares its clipboard with the program now.
    pub(super) fn shared(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.shared())
    }
}

/// The MIME type of text, as Kitty names it.
pub const TEXT_MIME: &str = "text/plain";

impl GhosttyEngine {
    /// Answer the program's clipboard reads from `source`; `None` denies them all, as a new
    /// engine does.
    pub fn share_clipboard(&self, source: Option<Box<dyn ClipboardSource>>) {
        self.clipboard.borrow_mut().source = source;
    }

    /// Whether the program asked to be told of pastes as Kitty paste events (mode 5522), so a
    /// paste goes to [`Self::paste_event`] rather than [`Self::encode_paste`].
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn paste_events(&self) -> Result<bool, EngineError> {
        Ok(self.term.mode(Mode::PASTE_EVENTS)?)
    }

    /// Tell the program of a paste of `text`, with `more` of the same copy beside it, as a
    /// Kitty paste event: the MIME types, text first, and a one-time password. Nothing of the
    /// paste reaches the program until it reads what it wants with that password, which is
    /// answered from this paste. A paste with no representation tells nothing (`false`).
    ///
    /// # Errors
    ///
    /// libghostty-vt failing, or no entropy for the password.
    pub fn paste_event(&mut self, text: &str, more: Vec<PasteRep>) -> Result<bool, EngineError> {
        let mut reps = Vec::with_capacity(more.len().saturating_add(1));
        if !text.is_empty() {
            reps.push(PasteRep { mime: TEXT_MIME, data: text.as_bytes().to_vec() });
        }
        reps.extend(more.into_iter().filter(|rep| rep.mime != TEXT_MIME));
        if reps.is_empty() {
            return Ok(false);
        }
        let mimes: Vec<ClipboardMime<'_>> =
            reps.iter().map(|rep| ClipboardMime::new(rep.mime)).collect();
        let told = self.term.paste(paste::Options::new(), &mimes, |_, _| Ok(()))?;
        drop(mimes);
        if told {
            self.clipboard.borrow_mut().pasted = reps;
        }
        Ok(told)
    }
}

/// Answer a read with a paste event's password from that paste, and any other read of the
/// standard clipboard's text from the shared source, up to the [`MAX_OSC52_BYTES`] a program's
/// own copy may be; every other read is refused. Installing the callback is what lets a
/// program turn on paste events (mode 5522).
pub(super) fn install(
    term: &mut Terminal<'static, 'static>,
    reads: &Shared,
) -> Result<(), EngineError> {
    let reads = Rc::clone(reads);
    term.on_clipboard_read(move |_, read| {
        if read.granted() {
            // The paste event's password: the paste is this read's, once.
            let pasted = std::mem::take(&mut reads.borrow_mut().pasted);
            return reply_paste(read, &pasted);
        }
        let text = answerable(&read)
            .then(|| reads.borrow().source.as_ref().and_then(|s| s.text()))
            .flatten()
            .filter(|text| text.len() <= MAX_OSC52_BYTES);
        // The reply writes to the pty through the pty-write callback: no borrow is held over it.
        reply(read, text.as_deref());
    })?;
    Ok(())
}

/// Whether `read` asks for anything the shared text can answer: the standard clipboard, as
/// text or as the list of what it holds.
fn answerable(read: &ClipboardRead<'_>) -> bool {
    read.location() == ClipboardLocation::Standard
        && (read.list() || read.mimes().any(|m| m == TEXT_MIME.as_bytes()))
}

fn reply(read: ClipboardRead<'_>, text: Option<&str>) {
    match text {
        Some(text) => {
            let wanted = read.mimes().any(|m| m == TEXT_MIME.as_bytes());
            let content = [ClipboardReplyContent::new(TEXT_MIME, text.as_bytes())];
            let contents: &[ClipboardReplyContent<'_>] = if wanted { &content } else { &[] };
            read.reply(Ok(contents), &[ClipboardMime::new(TEXT_MIME)], false);
        }
        None => read.reply(Err(ClipboardReadError::Denied), &[], false),
    }
}

/// Answer a read with a paste event's password from the paste: each type it asks for that the
/// paste has.
fn reply_paste(read: ClipboardRead<'_>, pasted: &[PasteRep]) {
    if pasted.is_empty() {
        return read.reply(Err(ClipboardReadError::Denied), &[], false);
    }
    let contents: Vec<ClipboardReplyContent<'_>> = pasted
        .iter()
        .filter(|rep| read.mimes().any(|m| m == rep.mime.as_bytes()))
        .map(|rep| ClipboardReplyContent::new(rep.mime, &rep.data))
        .collect();
    let available: Vec<ClipboardMime<'_>> =
        pasted.iter().map(|rep| ClipboardMime::new(rep.mime)).collect();
    read.reply(Ok(&contents), &available, false);
}

#[cfg(test)]
mod tests;
