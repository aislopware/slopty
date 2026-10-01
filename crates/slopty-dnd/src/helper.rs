//! The drag helper process: what `slopty-worker dnd` runs.
//!
//! An accessory `NSApplication` on the main thread, apart from the daemon, so AppKit's windows
//! and drag sessions never stall a stream or a virtual display. The worker talks to it over its
//! stdin and stdout, [`slopty_proto::dnd`] messages framed by [`slopty_proto::codec`]: nothing to
//! bind or name, and the helper ends when the worker does, since its stdin closes.
//!
//! - [`ToHelper::SourceAt`] puts the [`Source`] under a point with the drag's items, every one a
//!   promise ([`Item::Later`]) answered from a table the worker fills as the files land and the
//!   data arrives ([`ToHelper::Data`]). A target that reads something not yet there waits for it, a
//!   while, on the main thread, as AppKit's providers do; the worker lets go of a drop only once
//!   everything is here, so only a target that reads while it hovers ever waits.
//! - While a session is on, a thread of its own reads the system cursor every [`CURSOR_EVERY`] and
//!   classes it ([`crate::operation`]), and each change of what the target would do goes out as
//!   [`FromHelper::Operation`]. Not the main thread: AppKit tracks the drag there, and a timer on
//!   it fired 10–39 ms after the cursor changed (MEASUREMENTS.md, "the badge, timed").
//! - [`ToHelper::CatcherAt`] puts the [`Catcher`] under a point for a drag out of an app here, and
//!   says [`FromHelper::Ready`] once it shows there, for the drag to be carried onto it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use parking_lot::{Condvar, Mutex};
use slopty_proto::codec;
use slopty_proto::dnd::{CaughtData, FromHelper, SourceItem, ToHelper};
use slopty_proto::drag::{DragId, DragOp};

use crate::catcher::{Catcher, CatcherEvents, Caught, Promised};
use crate::items::{Item, Provide, file_url_bytes};
use crate::operation::{Badge, Cursors};
use crate::source::{Source, SourceEvents};
use crate::window;

/// The type a file's URL is on the pasteboard.
const FILE_URL: &str = "public.file-url";

/// How often the system cursor's seed is read while a session is on: what the target would do
/// reaches the worker within this of the cursor changing. A seed read is tens of nanoseconds.
pub const CURSOR_EVERY: Duration = Duration::from_millis(1);

/// The scales AppKit's cursors are read at, once, when the helper starts: a 1080p display's and
/// a Retina one's.
const SCALES: [u8; 2] = [1, 2];

/// The longest a target reading something not yet here waits for it.
pub const PROVIDE_WAIT: Duration = Duration::from_secs(5);

/// The most a caught representation is kept whole: past this it is listed with its size.
pub const CAUGHT_MAX: u64 = 64 << 20;

/// What each item of the current drag gives, as the worker has said.
#[derive(Debug, Default)]
struct Given {
    drag: Option<DragId>,
    items: Vec<Slot>,
}

/// One item: its file's path once known, and its data by type; `None` for what will not come.
#[derive(Debug, Default)]
struct Slot {
    is_file: bool,
    file: File,
    data: HashMap<String, Option<Vec<u8>>>,
}

/// Where an item's file is.
#[derive(Debug, Default)]
enum File {
    /// Not known yet.
    #[default]
    Coming,
    /// Here, whole or about to be.
    At(String),
    /// Not coming.
    Gone,
}

/// What a lookup found.
enum Found {
    Here(Option<Vec<u8>>),
    NotYet,
}

impl Given {
    fn find(&self, drag: DragId, item: usize, uti: &str) -> Found {
        if self.drag != Some(drag) {
            return Found::Here(None);
        }
        let Some(slot) = self.items.get(item) else { return Found::Here(None) };
        if uti == FILE_URL && slot.is_file {
            return match &slot.file {
                File::At(path) => Found::Here(Some(file_url_bytes(std::path::Path::new(path)))),
                File::Gone => Found::Here(None),
                File::Coming => Found::NotYet,
            };
        }
        match slot.data.get(uti) {
            Some(bytes) => Found::Here(bytes.clone()),
            None => Found::NotYet,
        }
    }
}

/// [`Given`] shared by the main thread, where targets read, and the reader thread, which fills
/// it.
#[derive(Debug, Default)]
struct Table {
    given: Mutex<Given>,
    changed: Condvar,
}

impl Table {
    fn start(&self, drag: DragId, items: &[SourceItem]) {
        let slots = items
            .iter()
            .map(|item| Slot {
                is_file: item.is_file,
                file: item.file.clone().map_or(File::Coming, File::At),
                data: item.given.iter().map(|g| (g.uti.clone(), Some(g.bytes.clone()))).collect(),
            })
            .collect();
        *self.given.lock() = Given { drag: Some(drag), items: slots };
        self.changed.notify_all();
    }

    fn give(&self, drag: DragId, item: u16, uti: &str, bytes: Option<Vec<u8>>) {
        let mut given = self.given.lock();
        if given.drag != Some(drag) {
            return;
        }
        let Some(slot) = given.items.get_mut(usize::from(item)) else { return };
        if uti == FILE_URL && slot.is_file {
            slot.file = bytes.and_then(|b| String::from_utf8(b).ok()).map_or(File::Gone, File::At);
        } else {
            slot.data.insert(uti.to_owned(), bytes);
        }
        drop(given);
        self.changed.notify_all();
    }

    fn stop(&self, drag: DragId) {
        let mut given = self.given.lock();
        if given.drag == Some(drag) {
            *given = Given::default();
        }
        drop(given);
        self.changed.notify_all();
    }

    /// What `item` gives as `uti`, waiting up to [`PROVIDE_WAIT`] for the worker to send it,
    /// and asking for it once.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the guard is what the condition variable waits with"
    )]
    fn wait(
        &self,
        drag: DragId,
        item: usize,
        uti: &str,
        out: &Sender<FromHelper>,
    ) -> Option<Vec<u8>> {
        let until = Instant::now().checked_add(PROVIDE_WAIT);
        let mut given = self.given.lock();
        let mut asked = false;
        loop {
            if let Found::Here(bytes) = given.find(drag, item, uti) {
                return bytes;
            }
            if !asked {
                asked = true;
                let item = u16::try_from(item).unwrap_or(u16::MAX);
                let _gone = out.send(FromHelper::Asked { drag, item, uti: uti.to_owned() });
            }
            let until = until?;
            if self.changed.wait_until(&mut given, until).timed_out() {
                tracing::debug!(%drag, item, uti, "a target read what never came");
                return None;
            }
        }
    }
}

/// What the main thread keeps: the roles, the drag on, and the cursor's watch while it is.
struct Main {
    source: Source,
    catcher: Catcher,
    table: Arc<Table>,
    out: Sender<FromHelper>,
    /// The drag the source or the catcher is for.
    drag: Rc<RefCell<Option<DragId>>>,
    /// Reads the cursor while a session is on.
    watch: Rc<RefCell<Option<Watch>>>,
    /// Waits for the source's window to show before saying it is ready.
    showing: RefCell<Option<Retained<NSTimer>>>,
}

thread_local! {
    static MAIN: RefCell<Option<Main>> = const { RefCell::new(None) };
}

/// The source's session, told to the worker.
struct Told {
    out: Sender<FromHelper>,
    drag: Rc<RefCell<Option<DragId>>>,
    watch: Rc<RefCell<Option<Watch>>>,
    cursors: Arc<[Cursors]>,
    /// The drag manager's steps of the session on, which a none waits for ([`Badge`]).
    steps: Arc<AtomicU64>,
}

impl SourceEvents for Told {
    fn began(&self, _at: (f64, f64)) {
        let Some(drag) = *self.drag.borrow() else { return };
        let _gone = self.out.send(FromHelper::Began { drag });
        self.steps.store(0, Ordering::Relaxed);
        let (out, cursors, steps) =
            (self.out.clone(), Arc::clone(&self.cursors), Arc::clone(&self.steps));
        self.watch.replace(Watch::start(drag, out, cursors, steps));
    }

    fn moved(&self, _at: (f64, f64)) {
        self.steps.fetch_add(1, Ordering::Relaxed);
    }

    fn ended(&self, operation: u64, _at: (f64, f64)) {
        self.watch.take();
        let Some(drag) = *self.drag.borrow() else { return };
        // The source allows Copy alone, so any operation is a copy.
        let op = if operation == 0 { DragOp::None } else { DragOp::Copy };
        let _gone = self.out.send(FromHelper::Ended { drag, op });
    }
}

/// The catcher's drop, told to the worker. Its promises are told from an operation queue.
struct Catching {
    out: Sender<FromHelper>,
    drag: Mutex<Option<DragId>>,
}

impl CatcherEvents for Catching {
    /// The drag is over the catcher, which takes a copy: the worker lets go now.
    fn entered(&self) {
        let Some(drag) = *self.drag.lock() else { return };
        let _gone = self.out.send(FromHelper::Operation { drag, op: DragOp::Copy });
    }

    fn caught(&self, caught: Caught) {
        let Some(drag) = *self.drag.lock() else { return };
        let files = caught.files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
        let index = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        let mut data: Vec<CaughtData> = caught
            .data
            .into_iter()
            .map(|(n, uti, bytes)| {
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                CaughtData { item: index(n), uti, bytes: Some(bytes), size }
            })
            .collect();
        data.extend(caught.too_big.into_iter().map(|(n, uti, size)| CaughtData {
            item: index(n),
            uti,
            bytes: None,
            size,
        }));
        let promises = u16::try_from(caught.promises).unwrap_or(u16::MAX);
        let _gone = self.out.send(FromHelper::Caught { drag, files, data, promises });
    }

    fn promised(&self, promised: Promised) {
        let Some(drag) = *self.drag.lock() else { return };
        let (path, error) = match promised {
            Ok(path) => (Some(path.to_string_lossy().into_owned()), None),
            Err(error) => (None, Some(error)),
        };
        let _gone = self.out.send(FromHelper::Promised { drag, path, error });
    }
}

/// What a drop does where the system cursor is `shape`, by AppKit's cursors at its scale; a
/// copy at a scale none was read at, as for a cursor that is none of AppKit's.
fn operation_of(cursors: &[Cursors], shape: &slopty_proto::screen::CursorShape) -> DragOp {
    cursors
        .iter()
        .find(|c| c.scale() == shape.scale)
        .map_or(DragOp::Copy, |c| c.class(shape).0.op())
}

/// The thread that reads the system cursor while a session is on; it ends when this goes.
struct Watch {
    _stop: Sender<()>,
}

impl Watch {
    /// Read the cursor every [`CURSOR_EVERY`] and say what a drop would do as [`Badge`] says
    /// it, by the session's `steps`, until the watch goes; `None` when no thread would start.
    fn start(
        drag: DragId,
        out: Sender<FromHelper>,
        cursors: Arc<[Cursors]>,
        steps: Arc<AtomicU64>,
    ) -> Option<Self> {
        let (stop, stopped) = mpsc::channel::<()>();
        let watching = move || {
            slopty_platform::user_interactive_thread();
            let mut watch = slopty_capture::CursorWatch::new();
            let (mut badge, mut seen) = (Badge::default(), None);
            while stopped.recv_timeout(CURSOR_EVERY) == Err(mpsc::RecvTimeoutError::Timeout) {
                if let Some(shape) = watch.poll() {
                    seen = Some(operation_of(&cursors, &shape));
                }
                let Some(op) = seen else { continue };
                if let Some(op) = badge.see(op, steps.load(Ordering::Relaxed), Instant::now()) {
                    let _gone = out.send(FromHelper::Operation { drag, op });
                }
            }
        };
        let thread = std::thread::Builder::new().name("slopty-dnd-cursor".to_owned());
        match thread.spawn(watching) {
            Ok(_detached) => Some(Self { _stop: stop }),
            Err(e) => {
                tracing::warn!(%drag, error = %e, "no thread to read the cursor on");
                None
            }
        }
    }
}

/// How long the source's window may take to show at its point before the helper says it is
/// ready anyway, for the press to try.
const SHOWN_WAIT: Duration = Duration::from_millis(250);

/// How often the window list is read meanwhile.
const SHOWN_EVERY: Duration = Duration::from_millis(1);

/// Say [`FromHelper::Ready`] once the window server shows the source's window `window` over
/// `at`, which the press there needs: AppKit orders it front at once, and the window server
/// takes it a moment later. The timer that waits, if it must.
fn ready_when_shown(
    window: isize,
    at: (f64, f64),
    drag: DragId,
    out: Sender<FromHelper>,
) -> Option<Retained<NSTimer>> {
    let id = slopty_core::WindowId(u32::try_from(window).unwrap_or(0));
    let shown = move || {
        slopty_capture::window_state(id)
            .is_some_and(|s| s.on_screen && s.bounds.contains(at.0, at.1))
    };
    let since = Instant::now();
    if shown() {
        let _gone = out.send(FromHelper::Ready { drag });
        return None;
    }
    let tick = RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| {
        let waited = since.elapsed();
        if !shown() && waited < SHOWN_WAIT {
            return;
        }
        tracing::debug!(%drag, waited_us = waited.as_micros(), "the source shows");
        let _gone = out.send(FromHelper::Ready { drag });
        // SAFETY: AppKit rule: the timer handed to its block is valid for the call, and a
        // repeating timer is stopped by invalidating it on the thread it fires on, this one.
        unsafe { timer.as_ref() }.invalidate();
    });
    // SAFETY: AppKit rule: a repeating timer, its block kept by the timer, made on this, the
    // main thread, and added below to this thread's run loop.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_repeats_block(SHOWN_EVERY.as_secs_f64(), true, &tick)
    };
    // SAFETY: AppKit rule: a timer is added to the run loop of the thread it fires on, this
    // one, in the common modes; the mode is a framework-provided constant string.
    unsafe {
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    Some(timer)
}

/// Act on a message the reader handed to the main thread.
fn on_main(msg: ToHelper) {
    MAIN.with(|main| {
        let main = main.borrow();
        let Some(main) = main.as_ref() else { return };
        match msg {
            ToHelper::SourceAt { drag, x, y, items } => {
                main.catcher.stop();
                main.table.start(drag, &items);
                main.drag.replace(Some(drag));
                let later: Vec<Item> = items
                    .iter()
                    .map(|item| {
                        let file = item.is_file.then(|| FILE_URL.to_owned());
                        Item::Later(file.into_iter().chain(item.types.iter().cloned()).collect())
                    })
                    .collect();
                let (table, out) = (Arc::clone(&main.table), main.out.clone());
                let provide: Provide = Arc::new(move |item, uti| table.wait(drag, item, uti, &out));
                main.source.at((x, y), &later, &provide);
                let window = main.source.window_number();
                let showing = ready_when_shown(window, (x, y), drag, main.out.clone());
                if let Some(earlier) = main.showing.replace(showing) {
                    earlier.invalidate();
                }
            }
            ToHelper::Stop { drag } => {
                if *main.drag.borrow() == Some(drag) {
                    main.source.stop();
                    main.catcher.stop();
                    main.table.stop(drag);
                    main.watch.take();
                    if let Some(timer) = main.showing.take() {
                        timer.invalidate();
                    }
                    main.drag.replace(None);
                }
            }
            ToHelper::CatcherAt { drag, x, y, dir } => {
                main.source.stop();
                main.drag.replace(Some(drag));
                main.catcher.at((x, y), PathBuf::from(dir));
                let window = main.catcher.window_number();
                let showing = ready_when_shown(window, (x, y), drag, main.out.clone());
                if let Some(earlier) = main.showing.replace(showing) {
                    earlier.invalidate();
                }
            }
            // The reader thread fills the table itself: a target may be waiting on the main
            // thread for it.
            ToHelper::Data { .. } => {}
        }
    });
}

/// Read framed messages off stdin: data into the table, everything else to the main thread.
/// The process ends when stdin does.
fn read(table: &Table, catching: &Catching) {
    let mut stdin = std::io::stdin();
    let mut prefix = [0_u8; codec::PREFIX_BYTES];
    loop {
        if stdin.read_exact(&mut prefix).is_err() {
            break;
        }
        let len = usize::try_from(u32::from_le_bytes(prefix)).unwrap_or(usize::MAX);
        if len > codec::MAX_FRAME_BYTES {
            tracing::warn!(len, "a frame past the limit from the worker");
            break;
        }
        let mut body = vec![0_u8; len];
        if stdin.read_exact(&mut body).is_err() {
            break;
        }
        let msg: ToHelper = match codec::decode_body(&body) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(error = %e, "a message the helper does not read");
                continue;
            }
        };
        match msg {
            ToHelper::Data { drag, item, uti, bytes } => table.give(drag, item, &uti, bytes),
            other => {
                match &other {
                    ToHelper::CatcherAt { drag, .. } => *catching.drag.lock() = Some(*drag),
                    ToHelper::Stop { drag } => {
                        table.stop(*drag);
                        let mut catching = catching.drag.lock();
                        if *catching == Some(*drag) {
                            *catching = None;
                        }
                    }
                    ToHelper::SourceAt { .. } | ToHelper::Data { .. } => {}
                }
                dispatch2::DispatchQueue::main().exec_async(move || on_main(other));
            }
        }
    }
}

/// Write each message to stdout as it comes.
fn write(said: &mpsc::Receiver<FromHelper>) {
    let mut stdout = std::io::stdout().lock();
    for msg in said {
        let Ok(frame) = codec::encode(&msg) else { continue };
        if stdout.write_all(&frame).and_then(|()| stdout.flush()).is_err() {
            break;
        }
    }
}

/// Run the helper until the worker goes. Call it on the main thread, first thing.
#[must_use]
pub fn run() -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
    let app = window::application(mtm);
    let (out, said) = mpsc::channel();
    let table = Arc::new(Table::default());
    let drag = Rc::new(RefCell::new(None));
    let watch = Rc::new(RefCell::new(None));
    // The first cursor read in a process pays for the window server's connection, and reading
    // AppKit's cursors for their images, so both are paid before any drag.
    let warmed = slopty_capture::warm_cursor();
    let cursors: Arc<[Cursors]> = SCALES.into_iter().map(Cursors::at).collect();
    let told = Told {
        out: out.clone(),
        drag: Rc::clone(&drag),
        watch: Rc::clone(&watch),
        cursors,
        steps: Arc::default(),
    };
    let source = Source::new(mtm, Box::new(told));
    let catching = Arc::new(Catching { out: out.clone(), drag: Mutex::new(None) });
    let events: Arc<dyn CatcherEvents> = Arc::<Catching>::clone(&catching);
    let catcher = Catcher::new(mtm, CAUGHT_MAX, events);
    MAIN.with(|main| {
        let showing = RefCell::new(None);
        let table = Arc::clone(&table);
        main.replace(Some(Main { source, catcher, table, out, drag, watch, showing }));
    });
    tracing::debug!(warm_ms = warmed.as_millis(), "drag helper up");
    let writer = std::thread::Builder::new().name("slopty-dnd-out".to_owned()).spawn(move || {
        write(&said);
    });
    let reader = std::thread::Builder::new().name("slopty-dnd-in".to_owned()).spawn(move || {
        read(&table, &catching);
        #[expect(clippy::exit, reason = "the worker closed the helper's stdin: it is gone")]
        std::process::exit(0);
    });
    if writer.is_err() || reader.is_err() {
        return ExitCode::FAILURE;
    }
    app.run();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use slopty_proto::dnd::Given as GivenRep;

    use super::*;

    /// What the table gives a target: data given at the source at once, a file's URL once the
    /// worker names where it landed, nothing for another drag or for what the worker said will
    /// not come. A read of something not yet here asks for it once and waits until it is given.
    #[test]
    fn a_target_gets_what_is_here_and_waits_for_what_is_coming() {
        let drag = DragId::new();
        let table = Arc::new(Table::default());
        let items = [
            SourceItem { file: None, is_file: true, types: vec![], given: vec![] },
            SourceItem {
                file: None,
                is_file: false,
                types: vec!["public.utf8-plain-text".to_owned(), "public.png".to_owned()],
                given: vec![GivenRep {
                    uti: "public.utf8-plain-text".to_owned(),
                    bytes: b"fox".to_vec(),
                }],
            },
        ];
        table.start(drag, &items);
        let (out, asked) = mpsc::channel();
        let text = "public.utf8-plain-text";
        assert_eq!(table.wait(drag, 1, text, &out), Some(b"fox".to_vec()));
        assert!(asked.try_recv().is_err(), "nothing asked for what was here");
        assert_eq!(table.wait(DragId::new(), 1, text, &out), None, "another drag's");
        let file = std::env::temp_dir().join("slopty-dnd-landed.txt");
        // The worker's side: it hears the ask and names where the file landed.
        let giver = {
            let table = Arc::clone(&table);
            let path = file.to_string_lossy().into_owned();
            std::thread::spawn(move || {
                let heard = asked.recv().unwrap();
                table.give(drag, 0, FILE_URL, Some(path.into_bytes()));
                table.give(drag, 1, "public.png", None);
                heard
            })
        };
        let url = table.wait(drag, 0, FILE_URL, &out).expect("the file's URL, once landed");
        assert!(String::from_utf8(url).unwrap().starts_with("file:///"));
        let heard = giver.join().unwrap();
        assert_eq!(heard, FromHelper::Asked { drag, item: 0, uti: FILE_URL.to_owned() });
        assert_eq!(table.wait(drag, 1, "public.png", &out), None, "said it will not come");
        table.stop(drag);
        assert_eq!(table.wait(drag, 1, text, &out), None, "stopped");
    }
}
