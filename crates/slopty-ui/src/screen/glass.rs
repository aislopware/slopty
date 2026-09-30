//! A stream's pictures, from the decoder's thread to a layer of their own.
//!
//! The decoder hands every picture to [`Glass::offer`] on its own thread
//! (`slopty_client::screen::ScreenHandle::set_present`). The glass gives it straight to a
//! [`VideoLayer`], which draws it on the layer's thread and never waits for a GPUI frame; the
//! view only places the layer where the picture goes, in its own frame, like any native. A
//! picture costs the window no frame. The view is told only when what it draws around the
//! picture changes: the first picture, and a new size or chroma ([`Shape`]).
//!
//! The [`Pacer`] is kept here, where all three threads reach it. A picture is offered to it on
//! the decoder's thread and put up on the layer when the pacer takes it. The layer's report
//! (`VideoLayer::on_presented`, on a Metal thread) stops the picture's clock when the window
//! server shows it, or counts it skipped when a newer picture replaced it in the layer's mailbox
//! first (reported from inside the present). A picture drawn that the display did not time is
//! left untimed. Until a layer is attached, the newest picture waits here and goes to the first
//! layer.
//!
//! A stream coded as two stripes decodes each on a session of its own
//! (`slopty_client::screen::Stitched`), and each goes to a layer of its own with no copy
//! between them: the view stacks the two layers, each clipped to the rows it shows. The glass
//! hands a capture's two pictures to their layers back to back, under one lock. The two layers
//! present on threads of their own, so the glass counts the captures whose stripes reached the
//! glass apart ([`Glass::seams`]): one picture drawn into one drawable would be the only
//! commit that cannot split, and that is `VideoLayer`'s to offer.
//!
//! Locks: `slot` is held across a present, so pictures reach the layer in the order their
//! sequences were booked, and so a layer swapped for a window's own never gets one late. `book`
//! is taken inside it for a moment, and alone by the layer's report, which `VideoLayer::present`
//! can make on the presenting thread itself. Nothing takes `slot` while it holds `book`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::PresentedFrame;
use gpui::composition::NativeHost;
use gpui_apple::fast::video_layer::{VideoLayer, VideoLayerOptions};
use parking_lot::Mutex;
use slopty_client::Presentable;
use slopty_client::pacing::{FrameStamp, GlassStats, Pace, Pacer, PacingStats};
use tokio::sync::watch;

use super::{Chroma, chroma_of};

/// Pictures put up and not yet reported that are remembered: a report can go missing when the
/// layer leaves the screen, and a forgotten one is a picture never timed, not a leak.
const FLYING: usize = 8;

/// Two stripes' pictures seen apart at the glass by more than this showed a tear: a refresh
/// is 8.3 ms at 120 Hz, and two layers shown in one refresh are timed within a millisecond.
const SPLIT_AFTER: Duration = Duration::from_millis(4);

/// Captures whose stripes are remembered until both layers report: as [`FLYING`].
const PAIRS: usize = 8;

/// What the view draws around the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Shape {
    /// The picture in pixels: both stripes together for a striped one.
    pub size: (u32, u32),
    /// How much colour it carries.
    pub chroma: Chroma,
    /// Where a striped picture's two stripes meet; `None` for one picture.
    pub seam: Option<Seam>,
}

/// How a striped picture's two pictures stack: each is its stripe's coded rows, which run past
/// the seam into the other's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Seam {
    /// The top stripe's picture's height.
    pub top: u32,
    /// Its rows shown, from its first: the rows above the seam.
    pub top_rows: u32,
    /// The lower stripe's picture's height.
    pub lower: u32,
    /// The first of its rows shown, the one at the seam.
    pub lower_from: u32,
}

impl Shape {
    /// The shape of `picture`.
    pub(super) fn of(picture: &Picture) -> Self {
        let (width, top) = side(&picture.top);
        let seam = picture.lower.as_ref().map(|lower| Seam {
            top,
            top_rows: lower.top_rows,
            lower: side(&lower.buffer).1,
            lower_from: lower.shown_from,
        });
        let height = seam.map_or(top, |seam| {
            seam.top_rows.saturating_add(seam.lower.saturating_sub(seam.lower_from))
        });
        Self { size: (width, height), chroma: chroma_of(picture.top.get_pixel_format()), seam }
    }
}

/// A buffer's width and height.
fn side(buffer: &CVPixelBuffer) -> (u32, u32) {
    let side = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    (side(buffer.get_width()), side(buffer.get_height()))
}

/// A decoded picture the decoder's thread, the layer's and the main thread all hold: one
/// picture, or a striped one's top stripe and its lower one.
#[derive(Clone)]
pub(super) struct Picture {
    top: CVPixelBuffer,
    lower: Option<Lower>,
}

/// A striped picture's lower stripe.
#[derive(Clone)]
struct Lower {
    buffer: CVPixelBuffer,
    /// Rows of the top stripe's picture shown.
    top_rows: u32,
    /// The first row of this one's shown.
    shown_from: u32,
}

// SAFETY: CVBuffer.h: "CVBuffers may be used from any thread". Retain and release are atomic,
// and a picture is never written after the decoder hands it out: everything here reads it.
#[expect(clippy::non_send_fields_in_send_ty, reason = "CVBuffer is documented thread-safe")]
unsafe impl Send for Picture {}
// SAFETY: as above; every use through `&Picture` reads.
unsafe impl Sync for Picture {}

impl Picture {
    /// A picture a test made.
    #[cfg(test)]
    pub(super) const fn new(buffer: CVPixelBuffer) -> Self {
        Self { top: buffer, lower: None }
    }

    /// A striped picture a test made: `top` shows its first `top_rows`, `lower` its rows from
    /// `lower_from`.
    #[cfg(test)]
    pub(super) const fn striped(
        top: CVPixelBuffer,
        top_rows: u32,
        lower: CVPixelBuffer,
        lower_from: u32,
    ) -> Self {
        Self { top, lower: Some(Lower { buffer: lower, top_rows, shown_from: lower_from }) }
    }

    /// The decoder's picture, in the wrapper GPUI and the layer take.
    pub(super) fn of(frame: &Presentable) -> Self {
        Self {
            top: wrap(frame.frame.image.as_cv()),
            lower: frame.stripes.as_ref().map(|stitched| Lower {
                buffer: wrap(stitched.lower.image.as_cv()),
                top_rows: stitched.top_rows,
                shown_from: stitched.lower_from,
            }),
        }
    }

    /// The whole picture's buffer, or the top stripe's.
    pub(super) fn buffer(&self) -> CVPixelBuffer {
        self.top.clone()
    }

    /// The lower stripe's buffer; `None` for one picture.
    pub(super) fn lower(&self) -> Option<CVPixelBuffer> {
        self.lower.as_ref().map(|lower| lower.buffer.clone())
    }
}

/// The decoder's buffer at `image`, a `CVPixelBufferRef`, in the wrapper GPUI and the layer
/// take.
fn wrap<T>(image: &T) -> CVPixelBuffer {
    let raw = std::ptr::from_ref(image).cast_mut().cast::<core_video::buffer::__CVBuffer>();
    // SAFETY: `raw` is a live `CVPixelBufferRef` the frame owns; `wrap_under_get_rule` takes a
    // retain of its own, so the wrapper outlives the frame.
    unsafe { CVPixelBuffer::wrap_under_get_rule(raw) }
}

/// A layer the pictures go to.
struct Layer {
    video: VideoLayer,
    /// Tells this layer's reports from a replaced one's.
    generation: u64,
    /// The sequence the layer gives the next picture: it counts every picture it took.
    next: u64,
}

#[derive(Default)]
struct Slot {
    layer: Option<Layer>,
    /// The lower stripe's layer, once a striped picture came.
    lower: Option<Layer>,
    /// The newest picture the pacer took: a render that captures the window draws it, and a
    /// layer attached after it came shows it first.
    last: Option<Picture>,
    /// `last` has not been put up on a layer yet.
    waiting: bool,
    generations: u64,
    /// Where the stripes meet in the layout the view last placed the layers for; `None` before
    /// it has placed them. A picture whose stripes meet elsewhere, or that has none where the
    /// layout has, waits for the view to place the layers for it ([`Glass::place`]): shown
    /// before, the top stripe would fill the whole picture's place for a frame, or the whole
    /// picture be clipped at a seam it does not have.
    placed: Option<Placed>,
}

/// How the view placed the layers: for pictures whose stripes meet at `seam`, or for pictures
/// of one piece.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Placed {
    pub seam: Option<Seam>,
}

impl Slot {
    /// Whether `last` can go up on the layers as the view placed them.
    fn fits(&self) -> bool {
        let Some(picture) = &self.last else { return false };
        self.placed.is_none_or(|placed| placed.seam == Shape::of(picture).seam)
    }
}

/// A picture on its way to the glass.
#[derive(Clone, Copy)]
struct Flying {
    generation: u64,
    sequence: u64,
    stamp: FrameStamp,
}

struct Book {
    pacer: Pacer,
    flying: VecDeque<Flying>,
    /// Captures whose stripes went up on the two layers, until both report.
    pairs: VecDeque<Pair>,
    seams: Seams,
    /// The thread inside `VideoLayer::present` now, which reports there a picture its mailbox
    /// replaced.
    presenting: Option<std::thread::ThreadId>,
}

/// A capture's two stripes on their way to the glass: each one's layer generation and sequence,
/// and what each layer reported of it, once it has.
#[derive(Clone, Copy)]
struct Pair {
    stripes: [(u64, u64); 2],
    reports: [Option<Report>; 2],
}

/// What a layer reported of a stripe's picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Report {
    /// On the glass then.
    Shown(Instant),
    /// Replaced in the layer's mailbox by a newer picture, never shown.
    Replaced,
}

/// How a striped stream's captures reached the glass.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Seams {
    /// Captures whose two stripes were shown in the same refresh.
    pub together: u64,
    /// Captures whose stripes were shown apart, or only one of them: the seam tore for a
    /// refresh.
    pub split: u64,
}

/// One stream's way to the glass.
pub(super) struct Glass {
    slot: Mutex<Slot>,
    book: Mutex<Book>,
    /// Pictures put up on a layer.
    put_up: AtomicU64,
    shape: watch::Sender<Option<Shape>>,
}

impl Glass {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(Slot::default()),
            book: Mutex::new(Book {
                pacer: Pacer::default(),
                flying: VecDeque::new(),
                pairs: VecDeque::new(),
                seams: Seams::default(),
                presenting: None,
            }),
            put_up: AtomicU64::new(0),
            shape: watch::Sender::new(None),
        })
    }

    /// What the view draws around the picture, as it changes.
    pub(super) fn shapes(&self) -> watch::Receiver<Option<Shape>> {
        self.shape.subscribe()
    }

    /// A decoded picture: put up on the layer at once unless the pacer drops it (older than the
    /// one up). Any thread.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "held from the pacer's verdict through the present: pictures reach the layer in \
                  the order the pacer took them"
    )]
    pub(super) fn offer(&self, picture: Picture, stamp: FrameStamp) {
        let mut slot = self.slot.lock();
        if self.book.lock().pacer.offer(stamp) == Pace::Drop {
            return;
        }
        let shape = Shape::of(&picture);
        self.shape.send_if_modified(|was| {
            let changed = *was != Some(shape);
            *was = Some(shape);
            changed
        });
        slot.last = Some(picture);
        slot.waiting = true;
        self.present(&mut slot);
    }

    /// Put `slot`'s newest picture up on its layer, if it has one: a striped picture's top
    /// stripe there and its lower one on the lower layer, back to back.
    fn present(&self, slot: &mut Slot) {
        if !slot.fits() {
            return;
        }
        let Slot { layer: Some(layer), lower, last: Some(picture), waiting, .. } = slot else {
            return;
        };
        // A picture shown on a replaced layer goes on the new one untimed: it was timed there.
        let stamp = if *waiting { self.book.lock().pacer.painted() } else { None };
        *waiting = false;
        let booked = stamp.map(|stamp| {
            let flying = Flying { generation: layer.generation, sequence: layer.next, stamp };
            let mut book = self.book.lock();
            if book.flying.len() >= FLYING {
                book.flying.pop_front();
            }
            book.flying.push_back(flying);
            flying
        });
        self.book.lock().presenting = Some(std::thread::current().id());
        let presented = layer.video.present(&picture.top);
        let lower = picture.lower.as_ref().zip(lower.as_mut()).map(|(picture, lower)| {
            let presented = lower.video.present(&picture.buffer);
            (lower, presented)
        });
        self.book.lock().presenting = None;
        if let (Ok(top), Some((lower, Ok(sequence)))) = (&presented, lower) {
            debug_assert_eq!(sequence, lower.next, "the layer counts the pictures it took");
            lower.next = sequence.saturating_add(1);
            let mut book = self.book.lock();
            if book.pairs.len() >= PAIRS {
                book.pairs.pop_front();
            }
            let stripes = [(layer.generation, *top), (lower.generation, sequence)];
            book.pairs.push_back(Pair { stripes, reports: [None, None] });
        }
        match presented {
            Ok(sequence) => {
                debug_assert_eq!(sequence, layer.next, "the layer counts the pictures it took");
                layer.next = sequence.saturating_add(1);
                self.put_up.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                if let Some(booked) = booked {
                    self.book.lock().flying.retain(|f| {
                        (f.generation, f.sequence) != (booked.generation, booked.sequence)
                    });
                }
                tracing::warn!(%error, "the video layer refused a picture");
            }
        }
    }

    /// Put the pictures on a layer in `host`, replacing the one they went to (a view moved to
    /// another window). The newest picture goes up on it at once. Main thread.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "held through the present: no picture reaches the replaced layer after it"
    )]
    pub(super) fn attach(self: &Arc<Self>, host: &NativeHost) -> anyhow::Result<()> {
        let video = VideoLayer::attach(host, VideoLayerOptions::default())?;
        let mut slot = self.slot.lock();
        slot.generations = slot.generations.saturating_add(1);
        let generation = slot.generations;
        let glass = Arc::downgrade(self);
        video.on_presented(Some(Arc::new(move |sequence, frame| {
            reported(&glass, generation, sequence, frame);
        })));
        slot.layer = Some(Layer { video, generation, next: 0 });
        self.present(&mut slot);
        Ok(())
    }

    /// Put a striped picture's lower stripe on a layer in `host`, replacing the one it went
    /// to. Main thread.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "held through the present: no picture reaches the replaced layer after it"
    )]
    pub(super) fn attach_lower(self: &Arc<Self>, host: &NativeHost) -> anyhow::Result<()> {
        let video = VideoLayer::attach(host, VideoLayerOptions::default())?;
        let mut slot = self.slot.lock();
        slot.generations = slot.generations.saturating_add(1);
        let generation = slot.generations;
        let glass = Arc::downgrade(self);
        video.on_presented(Some(Arc::new(move |sequence, frame| {
            if let Some(glass) = glass.upgrade() {
                glass.book.lock().stripe_shown(1, (generation, sequence), frame.presented_at);
            }
        })));
        slot.lower = Some(Layer { video, generation, next: 0 });
        self.present(&mut slot);
        Ok(())
    }

    /// The view placed the layers as `placed` says: a picture that waited for it goes up now.
    /// Main thread, when it changes.
    pub(super) fn place(&self, placed: Placed) {
        let mut slot = self.slot.lock();
        let was = slot.placed.replace(placed);
        if was.is_some_and(|was| was != placed) && slot.waiting {
            self.present(&mut slot);
        }
    }

    /// Let go of the layers: pictures wait here until another is attached.
    pub(super) fn detach(&self) {
        let (layer, lower) = {
            let mut slot = self.slot.lock();
            (slot.layer.take(), slot.lower.take())
        };
        drop((layer, lower));
    }

    /// How a striped stream's captures reached the glass so far.
    pub(super) fn seams(&self) -> Seams {
        self.book.lock().seams
    }

    /// The newest picture, for a render that draws it itself.
    pub(super) fn last(&self) -> Option<Picture> {
        self.slot.lock().last.clone()
    }

    /// Pictures put up on a layer so far.
    pub(super) fn put_up(&self) -> u64 {
        self.put_up.load(Ordering::Relaxed)
    }

    /// Arrival → glass over the last pictures shown, and the counters.
    pub(super) fn pacing(&self) -> PacingStats {
        self.book.lock().pacer.stats()
    }

    /// Capture → glass and input → glass over the last pictures shown.
    pub(super) fn glass(&self) -> GlassStats {
        self.book.lock().pacer.glass()
    }

    /// How long ago the picture on the glass got there.
    pub(super) fn age(&self) -> Option<Duration> {
        self.book.lock().pacer.age()
    }
}

impl Book {
    /// Stripe `index`'s layer reported the picture it numbered `at` (its generation and
    /// sequence): shown then, or never. Once both of a capture's stripes have reported, the
    /// capture counts as shown together or apart.
    fn stripe_shown(&mut self, index: usize, at: (u64, u64), shown: Option<Instant>) {
        let found = self.pairs.iter().position(|pair| pair.stripes.get(index) == Some(&at));
        let Some((found, pair)) = found.and_then(|i| Some((i, self.pairs.get_mut(i)?))) else {
            return;
        };
        let Some(report) = pair.reports.get_mut(index) else { return };
        *report = Some(shown.map_or(Report::Replaced, Report::Shown));
        let [Some(top), Some(lower)] = pair.reports else { return };
        self.pairs.remove(found);
        match (top, lower) {
            (Report::Shown(top), Report::Shown(lower))
                if top.max(lower).saturating_duration_since(top.min(lower)) <= SPLIT_AFTER =>
            {
                self.seams.together = self.seams.together.saturating_add(1);
            }
            // Neither shown: a newer capture replaced both, and nothing tore.
            (Report::Replaced, Report::Replaced) => {}
            _apart => self.seams.split = self.seams.split.saturating_add(1),
        }
    }
}

/// What became of the picture the layer of `generation` numbered `sequence`.
fn reported(glass: &Weak<Glass>, generation: u64, sequence: u64, frame: PresentedFrame) {
    let Some(glass) = glass.upgrade() else { return };
    let mut book = glass.book.lock();
    book.stripe_shown(0, (generation, sequence), frame.presented_at);
    let Some(at) =
        book.flying.iter().position(|f| f.generation == generation && f.sequence == sequence)
    else {
        return;
    };
    let Some(flying) = book.flying.remove(at) else { return };
    match frame.presented_at {
        Some(shown) => book.pacer.shown(flying.stamp, shown),
        // The layer reports a picture its mailbox replaced from inside the present.
        None if book.presenting == Some(std::thread::current().id()) => book.pacer.unshown(),
        // Drawn, but the display did not say when it showed it (one that reports no scan-out,
        // as a remote desktop's virtual display): untimed, as GPUI's own frames are there.
        None => {}
    }
}

/// A stamp for a picture a test puts up: decoded and arrived now, after every one before.
#[cfg(test)]
pub(super) fn test_stamp(index: u64) -> FrameStamp {
    let now = Instant::now();
    FrameStamp {
        pts_us: index.saturating_add(1),
        decode_seq: index,
        arrived: now,
        decoded: now,
        captured: None,
    }
}

#[cfg(test)]
mod tests {
    use core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;

    use super::*;

    fn picture(w: usize, h: usize) -> Picture {
        let buffer = CVPixelBuffer::new(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, w, h, None)
            .expect("pixel buffer");
        Picture::new(buffer)
    }

    fn frame(presented_at: Option<Instant>) -> PresentedFrame {
        PresentedFrame { submitted_at: Instant::now(), presented_at }
    }

    /// Pictures put up on the layer of generation 1 as `sequence`s 0, 1 and 2.
    fn flown(glass: &Glass) {
        let mut book = glass.book.lock();
        for sequence in 0..3 {
            assert_eq!(book.pacer.offer(test_stamp(sequence)), Pace::Present);
            let stamp = book.pacer.painted().expect("put up");
            book.flying.push_back(Flying { generation: 1, sequence, stamp });
        }
    }

    /// The layer's report stops its picture's clock at the glass, and one its mailbox replaced
    /// counts it skipped. A report from a replaced layer, a second report and one for a picture
    /// never put up change nothing.
    #[test]
    fn a_report_times_its_picture_or_counts_it_skipped() {
        let glass = Glass::new();
        flown(&glass);
        let weak = Arc::downgrade(&glass);
        reported(&weak, 2, 0, frame(Some(Instant::now())));
        assert_eq!((glass.pacing().presented, glass.pacing().skipped), (0, 0), "another layer's");
        glass.book.lock().presenting = Some(std::thread::current().id());
        reported(&weak, 1, 0, frame(None));
        glass.book.lock().presenting = None;
        reported(&weak, 1, 1, frame(Some(Instant::now())));
        reported(&weak, 1, 1, frame(Some(Instant::now())));
        reported(&weak, 1, 7, frame(Some(Instant::now())));
        let pacing = glass.pacing();
        assert_eq!((pacing.presented, pacing.skipped, pacing.window), (1, 1, 1));
        assert_eq!(glass.book.lock().flying.len(), 1, "the third, still on its way");
        drop(glass);
        reported(&weak, 1, 2, frame(None));
    }

    /// A report with no time from inside a present, on the presenting thread, is a picture the
    /// layer's mailbox replaced: skipped. One from anywhere else is a picture drawn that the
    /// display did not time (one that reports no scan-out, as Parsec's): left untimed, not
    /// counted against the stream.
    #[test]
    fn an_untimed_report_is_skipped_only_when_the_mailbox_replaced_it() {
        let glass = Glass::new();
        flown(&glass);
        let weak = Arc::downgrade(&glass);
        reported(&weak, 1, 0, frame(None));
        assert_eq!(glass.pacing().skipped, 0, "drawn, untimed by the display");
        assert_eq!(glass.book.lock().flying.len(), 2, "and forgotten");
        glass.book.lock().presenting = Some(std::thread::current().id());
        reported(&weak, 1, 1, frame(None));
        assert_eq!(glass.pacing().skipped, 1, "replaced inside the present");
    }

    /// With no layer the newest picture waits, and the view hears its shape; one replaced
    /// before any layer took it is skipped, and one older than the picture waiting is dropped.
    #[test]
    fn pictures_wait_for_a_layer() {
        let glass = Glass::new();
        let shapes = glass.shapes();
        glass.offer(picture(64, 48), test_stamp(0));
        glass.offer(picture(32, 16), test_stamp(1));
        glass.offer(picture(64, 48), test_stamp(0));
        assert_eq!(glass.put_up(), 0);
        let last = glass.last().expect("the newest waits");
        assert_eq!((last.top.get_width(), last.top.get_height()), (32, 16));
        assert_eq!(shapes.borrow().map(|shape| shape.size), Some((32, 16)), "not the late one's");
        let pacing = glass.pacing();
        assert_eq!((pacing.skipped, pacing.late), (1, 1));
    }

    /// A striped picture's shape is both stripes' shown rows together, with where they meet; a
    /// picture of one piece has no seam.
    #[test]
    fn a_striped_pictures_shape_is_its_shown_rows() {
        // 2160 rows: the seam at 1088, each stripe coding 64 rows past it.
        let (top, lower) = (picture(3840, 1152).top, picture(3840, 1136).top);
        let striped = Picture::striped(top, 1088, lower, 64);
        let shape = Shape::of(&striped);
        assert_eq!(shape.size, (3840, 2160));
        assert_eq!(
            shape.seam,
            Some(Seam { top: 1152, top_rows: 1088, lower: 1136, lower_from: 64 })
        );
        assert_eq!(Shape::of(&picture(800, 600)).seam, None);
    }

    /// A picture goes up only on layers placed for where its stripes meet: before the view has
    /// placed any, anything goes; after, a whole picture waits for a layout without a seam, a
    /// striped one for its own seam, and a resize that moves the seam waits too.
    #[test]
    fn a_picture_waits_for_layers_placed_for_its_seam() {
        let striped = |h_top, top_rows, h_lower| {
            Picture::striped(picture(64, h_top).top, top_rows, picture(64, h_lower).top, 64)
        };
        let mut slot = Slot { last: Some(striped(576, 512, 560)), ..Slot::default() };
        assert!(slot.fits(), "nothing placed yet");
        slot.placed = Some(Placed { seam: None });
        assert!(!slot.fits(), "laid out for one picture");
        slot.placed = Some(Placed { seam: Shape::of(&striped(576, 512, 560)).seam });
        assert!(slot.fits());
        slot.last = Some(striped(640, 576, 560));
        assert!(!slot.fits(), "the seam moved");
        slot.last = Some(picture(64, 1024));
        assert!(!slot.fits(), "a whole picture on striped layers");
        slot.placed = Some(Placed { seam: None });
        assert!(slot.fits());

        let glass = Glass::new();
        glass.place(Placed { seam: Shape::of(&striped(576, 512, 560)).seam });
        glass.offer(picture(64, 1024), test_stamp(0));
        assert!(glass.slot.lock().waiting, "held for the layout");
        glass.place(Placed { seam: None });
        assert!(glass.slot.lock().waiting, "and still for a layer");
    }

    /// A capture whose two stripes the layers showed within [`SPLIT_AFTER`] of each other went
    /// up together; one shown a refresh later, or only one of them shown, split. One both
    /// layers dropped for a newer capture counts neither way, and a report of a picture no pair
    /// names changes nothing.
    #[test]
    fn a_capture_counts_its_stripes_together_or_split() {
        let at = Instant::now();
        let later = |ms: u64| at.checked_add(Duration::from_millis(ms)).expect("a later time");
        let mut book = Book {
            pacer: Pacer::default(),
            flying: VecDeque::new(),
            pairs: VecDeque::new(),
            seams: Seams::default(),
            presenting: None,
        };
        for sequence in 0..4 {
            book.pairs
                .push_back(Pair { stripes: [(1, sequence), (2, sequence)], reports: [None, None] });
        }
        book.stripe_shown(0, (1, 0), Some(at));
        assert_eq!(book.seams, Seams::default(), "the lower stripe still to report");
        book.stripe_shown(1, (2, 0), Some(later(1)));
        book.stripe_shown(1, (2, 1), Some(later(9)));
        book.stripe_shown(0, (1, 1), Some(at));
        book.stripe_shown(0, (1, 2), Some(at));
        book.stripe_shown(1, (2, 2), None);
        book.stripe_shown(0, (1, 3), None);
        book.stripe_shown(1, (2, 3), None);
        book.stripe_shown(1, (2, 9), Some(at));
        assert_eq!(book.seams, Seams { together: 1, split: 2 });
        assert!(book.pairs.is_empty(), "every pair reported");
    }
}
