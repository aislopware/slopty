//! A drag out of an app on the worker, the client's half (`docs/decisions/audio.md`, "Drag
//! out").
//!
//! The worker says a drag began under this client's press ([`DragEvent::OutBegan`]) with what
//! its items hold, which the tile draws at the pointer while it stays on the tile. When the
//! pointer leaves the tile with the button held, the client drags the same items on from there
//! and the worker catches its own drag ([`DragEvent::OutCaught`]). [`Outgoing`] keeps one such
//! drag and answers what the local drag offers ([`Offer`]) and, as a target reads, where each
//! piece is: a named file at its path on the worker from the start, a promised file once the
//! catch has called it in, data inline or fetched under the drag ([`Source::Drag`]).
//!
//! A target reads a drag's data on this device's main thread, which is where the tile hears the
//! worker, so a read before the catch is in could never see it there. The link's control reader
//! hands the catch to every drag out it is told of ([`DragOuts`]) as it arrives, and a read
//! waits for it there ([`Shared::data`]).

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use slopty_platform::pasteboard::{clip_type, uti_of_type};
use slopty_proto::drag::{DragEvent, DragId, DragItem, FileMeta};
use slopty_proto::transfer::{Rep, RepRef, Source};

/// What the local drag offers for one item of a drag out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Offer {
    /// A file, as a promise kept by bringing it down: by its path on the worker when the item
    /// names one, else the `promise`-th promised file once the catch called it in.
    File {
        /// The name it is first offered under.
        name: String,
        /// Where it is on the worker, when known from the start.
        path: Option<String>,
        /// Which promised file it is, counted from 0, when it is one.
        promise: Option<usize>,
    },
    /// Data in these types, richest first: the `item`-th data item.
    Data {
        /// Which data item, counted from 0.
        item: usize,
        /// Uniform type identifiers.
        types: Vec<String>,
    },
}

/// Where the bytes of one type of a data item are, as a target reads it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DataAt {
    /// Here.
    Bytes(Vec<u8>),
    /// On the worker, kept by the catch: fetch this.
    Fetch(RepRef),
    /// Not caught yet.
    Waiting,
    /// Not coming.
    Gone,
}

/// Where a promised file is, as a target's drop keeps the promise.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileAt {
    /// On the worker at this path.
    At(String),
    /// Not caught yet.
    Waiting,
    /// The promise was not kept there.
    Gone,
}

/// One drag out of a worker's app. See the module docs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Outgoing {
    drag: DragId,
    began: Vec<DragItem>,
    caught: Option<Vec<DragItem>>,
}

/// Whether an item is data: it names and promises no file.
const fn is_data(item: &DragItem) -> bool {
    item.file.is_none() && item.promised.is_none()
}

impl Outgoing {
    /// The worker's drag `drag` began carrying `items`.
    #[must_use]
    pub const fn began(drag: DragId, items: Vec<DragItem>) -> Self {
        Self { drag, began: items, caught: None }
    }

    /// The drag.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        self.drag
    }

    /// What it carries, as it began.
    #[must_use]
    pub fn items(&self) -> &[DragItem] {
        &self.began
    }

    /// Whether the catch is in.
    #[must_use]
    pub const fn is_caught(&self) -> bool {
        self.caught.is_some()
    }

    /// The worker said `event`: the catch's items are kept, and a failure's reason comes back.
    pub fn heard(&mut self, event: &DragEvent) -> Option<String> {
        match event {
            DragEvent::OutCaught { drag, items } if *drag == self.drag => {
                self.caught = Some(items.clone());
                None
            }
            DragEvent::OutFailed { drag, error } if *drag == self.drag => {
                self.caught = Some(Vec::new());
                Some(error.clone())
            }
            _ => None,
        }
    }

    /// What the local drag offers, item by item. A promise is named by its content type until
    /// the catch names its file.
    #[must_use]
    pub fn offers(&self) -> Vec<Offer> {
        let (mut promise, mut data) = (0_usize, 0_usize);
        self.began
            .iter()
            .filter_map(|item| {
                if let Some(file) = &item.file {
                    let (name, path) = (file.name.clone(), file.path.clone());
                    return Some(Offer::File { name, path, promise: None });
                }
                if let Some(kind) = &item.promised {
                    let n = promise;
                    promise = promise.saturating_add(1);
                    let name = promised_name(kind, n);
                    return Some(Offer::File { name, path: None, promise: Some(n) });
                }
                let types: Vec<String> =
                    item.reps.iter().map(|r| uti_of_type(&r.kind).to_owned()).collect();
                let n = data;
                data = data.saturating_add(1);
                (!types.is_empty()).then_some(Offer::Data { item: n, types })
            })
            .collect()
    }

    /// Where the `promise`-th promised file is.
    #[must_use]
    pub fn promised(&self, promise: usize) -> FileAt {
        if self.caught.is_none() {
            return FileAt::Waiting;
        }
        let path = self.caught_promise(promise).and_then(|f| f.path.clone());
        path.map_or(FileAt::Gone, FileAt::At)
    }

    /// The `promise`-th promised file as the catch named it: the catch lists the named files
    /// first, then the promised ones it kept.
    fn caught_promise(&self, promise: usize) -> Option<&FileMeta> {
        let named = self.began.iter().filter(|i| i.file.is_some()).count();
        let caught = self.caught.as_ref()?;
        caught.iter().filter_map(|i| i.file.as_ref()).nth(named.saturating_add(promise))
    }

    /// What the drag carries back onto a tile of its own worker, where nothing needs to come
    /// up: each file by its path there (a promised one once the catch named it, else it is
    /// left out) and each data item's representations that ride inline.
    #[must_use]
    pub fn back(&self) -> Vec<DragItem> {
        let (mut promise, mut data) = (0_usize, 0_usize);
        self.began
            .iter()
            .filter_map(|item| {
                if item.file.is_some() {
                    return Some(item.clone());
                }
                if item.promised.is_some() {
                    let n = promise;
                    promise = promise.saturating_add(1);
                    let file = self.caught_promise(n).filter(|f| f.path.is_some()).cloned()?;
                    return Some(DragItem { file: Some(file), promised: None, reps: Vec::new() });
                }
                let n = data;
                data = data.saturating_add(1);
                let reps: Vec<Rep> = item
                    .reps
                    .iter()
                    .filter_map(|r| match self.data(n, uti_of_type(&r.kind)) {
                        DataAt::Bytes(bytes) => Some(Rep {
                            kind: r.kind.clone(),
                            size: u64::try_from(bytes.len()).ok(),
                            hash: None,
                            inline: Some(bytes),
                        }),
                        DataAt::Fetch(_) | DataAt::Waiting | DataAt::Gone => None,
                    })
                    .collect();
                (!reps.is_empty()).then_some(DragItem { file: None, promised: None, reps })
            })
            .collect()
    }

    /// Where the `item`-th data item's bytes are as `uti`: inline as it began, else as the catch
    /// has them.
    #[must_use]
    pub fn data(&self, item: usize, uti: &str) -> DataAt {
        let kind = clip_type(uti);
        let began = self.began.iter().filter(|i| is_data(i)).nth(item);
        if let Some(bytes) =
            began.and_then(|i| i.reps.iter().find(|r| r.kind == kind)?.inline.clone())
        {
            return DataAt::Bytes(bytes);
        }
        let Some(caught) = &self.caught else { return DataAt::Waiting };
        let Some((at, item)) = caught.iter().enumerate().filter(|(_, i)| is_data(i)).nth(item)
        else {
            return DataAt::Gone;
        };
        match item.reps.iter().find(|r| r.kind == kind) {
            Some(rep) => match &rep.inline {
                Some(bytes) => DataAt::Bytes(bytes.clone()),
                None => DataAt::Fetch(RepRef {
                    source: Source::Drag(self.drag),
                    item: u16::try_from(at).unwrap_or(u16::MAX),
                    kind,
                }),
            },
            None => DataAt::Gone,
        }
    }
}

/// An [`Outgoing`] shared by the tile, which hears the worker, and the local drag's promises,
/// which wait on another thread for the catch to name their files.
#[derive(Debug)]
pub struct Shared {
    out: parking_lot::Mutex<Outgoing>,
    changed: parking_lot::Condvar,
}

impl Shared {
    /// Share `out`.
    #[must_use]
    pub const fn new(out: Outgoing) -> Self {
        Self { out: parking_lot::Mutex::new(out), changed: parking_lot::Condvar::new() }
    }

    /// The drag.
    #[must_use]
    pub fn drag(&self) -> DragId {
        self.out.lock().drag()
    }

    /// The worker said `event`, as [`Outgoing::heard`] takes it; whoever waits on the catch
    /// looks again.
    pub fn heard(&self, event: &DragEvent) -> Option<String> {
        let failed = self.out.lock().heard(event);
        self.changed.notify_all();
        failed
    }

    /// What it carries back onto its own worker ([`Outgoing::back`]).
    #[must_use]
    pub fn back(&self) -> Vec<DragItem> {
        self.out.lock().back()
    }

    /// What the local drag offers ([`Outgoing::offers`]).
    #[must_use]
    pub fn offers(&self) -> Vec<Offer> {
        self.out.lock().offers()
    }

    /// Where the `item`-th data item's bytes are as `uti` ([`Outgoing::data`]), waiting up to
    /// `within` for the catch to say. Only a drag out the link was told of
    /// ([`crate::remote::Remote::watch_drag_out`]) hears the catch while the main thread waits
    /// here.
    #[must_use]
    pub fn data(&self, item: usize, uti: &str, within: Duration) -> DataAt {
        self.wait(within, |out| match out.data(item, uti) {
            DataAt::Waiting => None,
            at => Some(at),
        })
        .unwrap_or(DataAt::Waiting)
    }

    /// Where the `promise`-th promised file is, waiting up to `within` for the catch to say.
    #[must_use]
    pub fn promised(&self, promise: usize, within: Duration) -> FileAt {
        self.wait(within, |out| match out.promised(promise) {
            FileAt::Waiting => None,
            at => Some(at),
        })
        .unwrap_or(FileAt::Waiting)
    }

    /// What `look` finds, looking again each time the worker is heard, for up to `within`.
    fn wait<T>(&self, within: Duration, look: impl Fn(&Outgoing) -> Option<T>) -> Option<T> {
        let until = Instant::now().checked_add(within);
        let mut out = self.out.lock();
        loop {
            if let Some(found) = look(&out) {
                return Some(found);
            }
            let until = until?;
            if self.changed.wait_until(&mut out, until).timed_out() {
                return look(&out);
            }
        }
    }
}

/// The drags out of one worker's apps that its link tells, off the main thread.
///
/// The link's control reader hands the worker's word on each to it as it arrives
/// ([`crate::remote::Remote::watch_drag_out`]). Each is held weakly, so one the person's drag
/// is done with goes with it.
#[derive(Debug, Default)]
pub struct DragOuts {
    watched: parking_lot::Mutex<Vec<Weak<Shared>>>,
}

impl DragOuts {
    /// Hand the worker's word on `shared`'s drag to it from now on.
    pub fn watch(&self, shared: &Arc<Shared>) {
        let held = Arc::downgrade(shared);
        let mut watched = self.watched.lock();
        watched.retain(|w| w.strong_count() > 0 && !w.ptr_eq(&held));
        watched.push(held);
    }

    /// The worker said `event` of a drag: the drag out it is about hears it.
    pub fn heard(&self, event: &DragEvent) {
        let (DragEvent::OutCaught { drag, .. } | DragEvent::OutFailed { drag, .. }) = event else {
            return;
        };
        let found = self
            .watched
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|shared| shared.drag() == *drag);
        if let Some(shared) = found {
            let _said = shared.heard(event);
        }
    }
}

/// The name a promised file is first offered under: its content type's own extension, as the
/// receiver will try it before the file is there.
fn promised_name(kind: &str, n: usize) -> String {
    let extension = match kind {
        "com.adobe.pdf" => "pdf",
        "public.jpeg" => "jpg",
        "public.png" => "png",
        "public.heic" => "heic",
        "public.plain-text" | "public.utf8-plain-text" => "txt",
        _ => "",
    };
    let base = if n == 0 {
        "Dragged file".to_owned()
    } else {
        format!("Dragged file {}", n.saturating_add(1))
    };
    if extension.is_empty() { base } else { format!("{base}.{extension}") }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::transfer::{ClipFormat, ClipType};

    use super::*;

    fn file(name: &str, path: &str) -> DragItem {
        let meta = FileMeta {
            name: name.to_owned(),
            size: 1,
            folder: false,
            mode: 0o644,
            mtime_ms: WallMs::ZERO,
            path: Some(path.to_owned()),
        };
        DragItem { file: Some(meta), promised: None, reps: Vec::new() }
    }

    fn rep(format: ClipFormat, inline: Option<&[u8]>) -> Rep {
        Rep {
            kind: ClipType::Format(format),
            size: None,
            hash: None,
            inline: inline.map(<[u8]>::to_vec),
        }
    }

    /// A named file is offered by its path from the start, a promise by its content type until
    /// the catch names it, and data in its types; the catch says where the promised file is
    /// and where each type's bytes are, inline or fetched under the drag, and a failed catch
    /// leaves the promises broken. A promise kept on another thread waits for the catch.
    #[test]
    fn a_drag_out_offers_its_items_and_finds_them_once_caught() {
        let drag = DragId::new();
        let text = rep(ClipFormat::Text, Some(b"fox"));
        let png = rep(ClipFormat::Png, None);
        let data = DragItem { file: None, promised: None, reps: vec![text, png.clone()] };
        let promised =
            DragItem { file: None, promised: Some("com.adobe.pdf".to_owned()), reps: Vec::new() };
        let mut out = Outgoing::began(drag, vec![file("a.txt", "/w/a.txt"), promised, data]);
        let text_uti = uti_of_type(&ClipType::Format(ClipFormat::Text)).to_owned();
        let png_uti = uti_of_type(&ClipType::Format(ClipFormat::Png)).to_owned();
        assert_eq!(
            out.offers(),
            [
                Offer::File {
                    name: "a.txt".to_owned(),
                    path: Some("/w/a.txt".to_owned()),
                    promise: None
                },
                Offer::File { name: "Dragged file.pdf".to_owned(), path: None, promise: Some(0) },
                Offer::Data { item: 0, types: vec![text_uti.clone(), png_uti.clone()] },
            ]
        );
        assert_eq!(out.promised(0), FileAt::Waiting);
        assert_eq!(out.data(0, &text_uti), DataAt::Bytes(b"fox".to_vec()), "inline from the start");
        assert_eq!(out.data(0, &png_uti), DataAt::Waiting);

        let caught = vec![
            file("a.txt", "/w/a.txt"),
            file("Invoice.pdf", "/w/.slopty/drop/d/Invoice.pdf"),
            DragItem { file: None, promised: None, reps: vec![png] },
        ];
        assert_eq!(out.heard(&DragEvent::OutCaught { drag, items: caught }), None);
        assert!(out.is_caught());
        assert_eq!(out.promised(0), FileAt::At("/w/.slopty/drop/d/Invoice.pdf".to_owned()));
        assert_eq!(out.promised(1), FileAt::Gone);
        let fetch =
            RepRef { source: Source::Drag(drag), item: 2, kind: ClipType::Format(ClipFormat::Png) };
        assert_eq!(out.data(0, &png_uti), DataAt::Fetch(fetch));
        assert_eq!(out.data(0, "public.tiff"), DataAt::Gone);

        let shared = Arc::new(Shared::new(Outgoing::began(drag, out.items().to_vec())));
        let waiter = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || shared.promised(0, Duration::from_secs(5)))
        };
        let caught = vec![file("a.txt", "/w/a.txt"), file("b.pdf", "/w/b.pdf")];
        assert_eq!(shared.heard(&DragEvent::OutCaught { drag, items: caught }), None);
        assert_eq!(waiter.join().unwrap(), FileAt::At("/w/b.pdf".to_owned()), "woken by the catch");

        let mut failed = Outgoing::began(
            drag,
            vec![DragItem { file: None, promised: Some(String::new()), reps: Vec::new() }],
        );
        let error = DragEvent::OutFailed { drag, error: "no catch".to_owned() };
        assert_eq!(failed.heard(&error).as_deref(), Some("no catch"));
        assert_eq!(failed.promised(0), FileAt::Gone);
    }

    /// Back onto its own worker a drag carries its files by their paths there, the promised one
    /// once the catch named it and not before, and only the data that rides inline: the rest
    /// would have to come down to go up again.
    #[test]
    fn a_drag_out_carries_back_its_paths_and_inline_data() {
        let drag = DragId::new();
        let text = rep(ClipFormat::Text, Some(b"fox"));
        let png = rep(ClipFormat::Png, None);
        let data = DragItem { file: None, promised: None, reps: vec![text.clone(), png.clone()] };
        let promised =
            DragItem { file: None, promised: Some("com.adobe.pdf".to_owned()), reps: Vec::new() };
        let named = file("a.txt", "/w/a.txt");
        let mut out = Outgoing::began(drag, vec![named.clone(), promised, data]);
        let text_only =
            DragItem { file: None, promised: None, reps: vec![Rep { size: Some(3), ..text }] };
        assert_eq!(out.back(), [named.clone(), text_only.clone()], "before the catch");

        let invoice = file("Invoice.pdf", "/w/.slopty/drag/d/Invoice.pdf");
        let caught = vec![
            named.clone(),
            invoice.clone(),
            DragItem { file: None, promised: None, reps: vec![png] },
        ];
        out.heard(&DragEvent::OutCaught { drag, items: caught });
        assert_eq!(
            out.back(),
            [named, invoice, text_only],
            "the promise named, the picture not inline"
        );
    }

    /// Data a target reads before the catch is in waits for it, and the link's reader hands the
    /// catch to the drag it names, on its own thread: a read on the main thread gets the bytes
    /// though the main thread hears nothing. A drag nobody holds any more is let go,
    /// and a read with nothing coming stops at its wait.
    #[test]
    fn data_read_before_the_catch_waits_for_it_off_the_main_thread() {
        let drag = DragId::new();
        let png = rep(ClipFormat::Png, None);
        let data = DragItem { file: None, promised: None, reps: vec![png] };
        let shared = Arc::new(Shared::new(Outgoing::began(drag, vec![data])));
        let outs = Arc::new(DragOuts::default());
        outs.watch(&shared);
        let other = Arc::new(Shared::new(Outgoing::began(DragId::new(), Vec::new())));
        outs.watch(&other);
        drop(other);
        let png_uti = uti_of_type(&ClipType::Format(ClipFormat::Png)).to_owned();
        assert_eq!(shared.data(0, &png_uti, Duration::from_millis(20)), DataAt::Waiting);

        let reader = {
            let outs = Arc::clone(&outs);
            std::thread::spawn(move || {
                let inline = rep(ClipFormat::Png, Some(b"png"));
                let caught = vec![DragItem { file: None, promised: None, reps: vec![inline] }];
                outs.heard(&DragEvent::OutCaught { drag: DragId::new(), items: Vec::new() });
                outs.heard(&DragEvent::OutCaught { drag, items: caught });
            })
        };
        let read = shared.data(0, &png_uti, Duration::from_secs(5));
        assert_eq!(read, DataAt::Bytes(b"png".to_vec()), "the catch, from the reader's thread");
        reader.join().unwrap();
        assert_eq!(outs.watched.lock().len(), 2, "the gone one is still listed");
        outs.watch(&shared);
        assert_eq!(outs.watched.lock().len(), 1, "let go at the next watch, and none twice");
    }
}
