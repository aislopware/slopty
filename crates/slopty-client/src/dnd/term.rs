//! A drag from this client over a terminal whose program asks for drops (Kitty drag and drop,
//! OSC 72), the client's half (`docs/decisions/terminal.md`, "Drops are read lazily").
//!
//! What the drag carries is read off its pasteboard once, as it enters ([`super::read`]), and
//! kept here for the drag's life: the drag's pasteboard is not to be read once the drag is
//! over, and the program may ask for a type well after the drop. The worker is told what the
//! items are, with none of their bytes ([`TermDrag::items`]). What the program accepts during
//! the hover goes up at once ([`TermDrag::accepted`]): its representations, and its files as
//! the drag's own upload, so they are there by the drop. Files an item only promises are
//! written at the drop, so a drag with any goes up whole after it. Anything else the program
//! asks for on the drop the worker fetches ([`TermDrag::fetch`]).

use std::collections::HashSet;
use std::path::PathBuf;

use slopty_proto::drag::{DragId, DragItem};
use slopty_proto::terminal::{DropFrom, drop_offer};
use slopty_proto::transfer::{ClipType, RepRef, Source};

use super::Read;
use crate::clip::Fetched;

/// One drag over a terminal tile, as read when it entered.
#[derive(Debug)]
pub struct TermDrag {
    drag: DragId,
    read: Read,
    /// The types the program is offered, as the worker numbers them.
    offer: Vec<(String, DropFrom)>,
    /// The representations sent up already.
    pushed: HashSet<(u16, ClipType)>,
    /// The program accepted the files.
    files_wanted: bool,
    /// The files are going up.
    uploading: bool,
}

/// What goes up for what the program accepted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Accepted {
    /// Representations to send, under the drag.
    pub pushes: Vec<(RepRef, Vec<u8>)>,
    /// The drag's files are to start going up.
    pub upload: bool,
}

impl TermDrag {
    /// A fresh drag carrying `read`.
    #[must_use]
    pub fn new(read: Read) -> Self {
        let offer = drop_offer(&read.items);
        Self {
            drag: DragId::new(),
            read,
            offer,
            pushed: HashSet::new(),
            files_wanted: false,
            uploading: false,
        }
    }

    /// The drag.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        self.drag
    }

    /// The files the drag names here, its upload.
    #[must_use]
    pub fn files(&self) -> &[PathBuf] {
        &self.read.files
    }

    /// Whether it carries files, named or promised: the program is offered their list.
    #[must_use]
    pub fn has_files(&self) -> bool {
        !self.read.files.is_empty() || self.read.promises()
    }

    /// Whether an item promises a file, which is written only once the drop calls it in.
    #[must_use]
    pub fn promises(&self) -> bool {
        self.read.promises()
    }

    /// The items as the worker is told of them: what they are, none of their bytes.
    #[must_use]
    pub fn items(&self) -> Vec<DragItem> {
        let mut items = self.read.items.clone();
        for rep in items.iter_mut().flat_map(|i| i.reps.iter_mut()) {
            rep.inline = None;
        }
        items
    }

    /// The program accepted `mimes` for the drag: what to send up now that has not gone yet.
    /// An acceptance that names no type sends nothing; what the program reads is fetched.
    pub fn accepted(&mut self, mimes: &[String]) -> Accepted {
        let mut accepted = Accepted::default();
        for mime in mimes {
            let Some((_, from)) = self.offer.iter().find(|(m, _)| m == mime) else { continue };
            match from {
                DropFrom::Files => {
                    self.files_wanted = true;
                    if !self.uploading && !self.read.files.is_empty() && !self.read.promises() {
                        self.uploading = true;
                        accepted.upload = true;
                    }
                }
                DropFrom::Rep { item, kind } => {
                    if !self.pushed.insert((*item, kind.clone())) {
                        continue;
                    }
                    if let Some(bytes) = self.bytes(*item, kind) {
                        let rep = RepRef {
                            source: Source::Drag(self.drag),
                            item: *item,
                            kind: kind.clone(),
                        };
                        accepted.pushes.push((rep, bytes.to_vec()));
                    }
                }
            }
        }
        accepted
    }

    /// Whether the program accepted the files: a drop whose program did not is told at once
    /// that they will not come, and nothing goes up for it.
    #[must_use]
    pub const fn files_wanted(&self) -> bool {
        self.files_wanted
    }

    /// Whether the files went up during the hover.
    #[must_use]
    pub const fn uploading(&self) -> bool {
        self.uploading
    }

    /// The worker's fetch of representation `kind` of item `item`, capped at `max`.
    #[must_use]
    pub fn fetch(&self, item: u16, kind: &ClipType, max: Option<u64>) -> Fetched {
        match self.bytes(item, kind) {
            Some(bytes) => {
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                if max.is_some_and(|max| size > max) {
                    Fetched::TooBig(size)
                } else {
                    Fetched::Data(bytes.to_vec())
                }
            }
            None => Fetched::Gone,
        }
    }

    /// The bytes of representation `kind` of item `item`, inline or kept aside.
    fn bytes(&self, item: u16, kind: &ClipType) -> Option<&[u8]> {
        let inline = self
            .read
            .items
            .get(usize::from(item))?
            .reps
            .iter()
            .find(|r| &r.kind == kind)
            .and_then(|r| r.inline.as_deref());
        inline.or_else(|| {
            let push = self.read.pushes.iter().find(|p| p.item == item && &p.kind == kind)?;
            Some(push.bytes.as_slice())
        })
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::drag::FileMeta;
    use slopty_proto::terminal::URI_LIST;
    use slopty_proto::transfer::{ClipFormat, Rep};

    use super::super::Push;
    use super::*;

    fn text(bytes: &[u8], inline: bool) -> Rep {
        Rep {
            kind: ClipType::Format(ClipFormat::Text),
            size: Some(bytes.len() as u64),
            hash: None,
            inline: inline.then(|| bytes.to_vec()),
        }
    }

    /// A drag of a file and a text, and a picture too big to ride inline.
    fn drag() -> TermDrag {
        let file = FileMeta {
            name: "a.txt".to_owned(),
            size: 1,
            folder: false,
            mode: 0o644,
            mtime_ms: slopty_core::WallMs::ZERO,
            path: None,
        };
        let png = ClipType::Format(ClipFormat::Png);
        let picture = Rep { kind: png.clone(), size: Some(3), hash: None, inline: None };
        let read = Read {
            items: vec![
                DragItem { file: Some(file), promised: None, reps: Vec::new() },
                DragItem { file: None, promised: None, reps: vec![text(b"hi", true), picture] },
            ],
            files: vec![PathBuf::from("/here/a.txt")],
            pushes: vec![Push { item: 1, kind: png, bytes: b"png".to_vec() }],
        };
        TermDrag::new(read)
    }

    /// The worker hears what the items are and none of their bytes.
    #[test]
    fn the_worker_is_told_the_items_without_their_bytes() {
        let drag = drag();
        let items = drag.items();
        assert_eq!(items.len(), 2);
        assert!(items.iter().flat_map(|i| &i.reps).all(|r| r.inline.is_none()));
        assert!(items[1].reps.iter().all(|r| r.size.is_some()), "sizes stay");
    }

    /// What the program accepts goes up once: its texts and pictures, inline or kept aside,
    /// and its files as the upload. A type it did not accept stays here for a fetch.
    #[test]
    fn what_the_program_accepts_goes_up_once() {
        let mut drag = drag();
        let id = drag.drag();
        let accepted = drag.accepted(&["text/plain".to_owned(), URI_LIST.to_owned()]);
        let text =
            RepRef { source: Source::Drag(id), item: 1, kind: ClipType::Format(ClipFormat::Text) };
        assert_eq!(accepted, Accepted { pushes: vec![(text, b"hi".to_vec())], upload: true });
        let again = drag.accepted(&[ClipFormat::Text.mime().to_owned(), URI_LIST.to_owned()]);
        assert_eq!(again, Accepted::default(), "the same text under its other name, and the files");
        assert!(drag.files_wanted() && drag.uploading());

        let png = ClipType::Format(ClipFormat::Png);
        assert_eq!(drag.fetch(1, &png, None), Fetched::Data(b"png".to_vec()));
        assert_eq!(drag.fetch(1, &png, Some(2)), Fetched::TooBig(3));
        assert_eq!(drag.fetch(0, &png, None), Fetched::Gone);
        assert_eq!(drag.accepted(&["image/x-unknown".to_owned()]), Accepted::default());
    }

    /// Files the program never accepted never go up.
    #[test]
    fn unaccepted_files_stay_here() {
        let mut drag = drag();
        let _accepted = drag.accepted(&["text/plain".to_owned()]);
        assert!(!drag.files_wanted() && !drag.uploading());
    }

    /// A drag with a promised file goes up whole after the drop, when the promise is kept: the
    /// program gets one list.
    #[test]
    fn a_drag_with_a_promise_waits_for_the_drop() {
        let mut read = drag().read;
        read.items.push(DragItem {
            file: None,
            promised: Some("public.jpeg".to_owned()),
            reps: Vec::new(),
        });
        let mut drag = TermDrag::new(read);
        assert!(!drag.accepted(&[URI_LIST.to_owned()]).upload);
        assert!(drag.files_wanted() && !drag.uploading());
    }

    /// M2 of the lazy drop: the bytes that go up for a drag of a text, its HTML and a picture
    /// onto a program that accepts only the text, against sending every type, as an eager drop
    /// would.
    #[test]
    fn only_what_the_program_accepts_goes_up() {
        let png = ClipType::Format(ClipFormat::Png);
        let html = ClipType::Format(ClipFormat::Html);
        let (text_len, html_len, png_len) = (1 << 10, 10 << 10, 1 << 20);
        let rep = |kind: ClipType, len: usize, inline: bool| Rep {
            kind,
            size: Some(len as u64),
            hash: None,
            inline: inline.then(|| vec![b'x'; len]),
        };
        let read = Read {
            items: vec![DragItem {
                file: None,
                promised: None,
                reps: vec![
                    text(&vec![b'x'; text_len], true),
                    rep(html, html_len, true),
                    rep(png.clone(), png_len, false),
                ],
            }],
            files: Vec::new(),
            pushes: vec![Push { item: 0, kind: png, bytes: vec![b'x'; png_len] }],
        };
        let mut drag = TermDrag::new(read);
        let pushed: usize =
            drag.accepted(&["text/plain".to_owned()]).pushes.iter().map(|(_, b)| b.len()).sum();
        let every = text_len + html_len + png_len;
        assert_eq!(pushed, text_len);
        println!(
            "MEASURE drop of text + HTML + PNG, text accepted: {pushed} bytes up, against {every} eager"
        );
    }
}
