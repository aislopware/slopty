//! A program's clipboard reads (OSC 52 `?`, OSC 5522 `type=read`), answered from the text the
//! session is given to share: whatever the worker mirrors while clipboard sharing is on, and
//! nothing otherwise. The answer is decided in the callback, on the spot: a read the program
//! waits on never waits on anything itself.

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::Terminal;
use libghostty_vt::terminal::{
    ClipboardLocation, ClipboardMime, ClipboardRead, ClipboardReadError, ClipboardReplyContent,
};
use slopty_proto::terminal::MAX_OSC52_BYTES;

use super::GhosttyEngine;
use crate::EngineError;

/// What a program's clipboard read is answered with: the shared text now, or `None` to deny it.
///
/// A denied OSC 52 read gets an empty answer. It is called on the engine's thread inside
/// [`GhosttyEngine::write`], so it must return at once.
pub type ClipboardSource = Box<dyn Fn() -> Option<String>>;

/// The source shared with the read callback.
pub(super) type Shared = Rc<RefCell<Option<ClipboardSource>>>;

const TEXT: &str = "text/plain";

impl GhosttyEngine {
    /// Answer the program's clipboard reads from `source`; `None` denies them all, as a new
    /// engine does.
    pub fn share_clipboard(&self, source: Option<ClipboardSource>) {
        *self.clipboard.borrow_mut() = source;
    }
}

/// Answer reads of the standard clipboard's text from `source`, up to the [`MAX_OSC52_BYTES`] a
/// program's own copy may be; every other read is refused.
///
/// Installing the callback also turns on Kitty paste events (mode 5522) in libghostty, which
/// only [`Terminal::paste`] sends; the engine's pastes go through [`libghostty_vt::paste::encode`]
/// and stay plain (bracketed) pastes.
pub(super) fn install(
    term: &mut Terminal<'static, 'static>,
    source: &Shared,
) -> Result<(), EngineError> {
    let source = Rc::clone(source);
    term.on_clipboard_read(move |_, read| {
        let text = answerable(&read)
            .then(|| source.borrow().as_ref().and_then(|f| f()))
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
        && (read.list() || read.mimes().any(|m| m == TEXT.as_bytes()))
}

fn reply(read: ClipboardRead<'_>, text: Option<&str>) {
    match text {
        Some(text) => {
            let wanted = read.mimes().any(|m| m == TEXT.as_bytes());
            let content = [ClipboardReplyContent::new(TEXT, text.as_bytes())];
            let contents: &[ClipboardReplyContent<'_>] = if wanted { &content } else { &[] };
            read.reply(Ok(contents), &[ClipboardMime::new(TEXT)], false);
        }
        None => read.reply(Err(ClipboardReadError::Denied), &[], false),
    }
}

#[cfg(test)]
mod tests;
