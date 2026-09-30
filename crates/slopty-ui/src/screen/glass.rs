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
//! Locks: `slot` is held across a present, so pictures reach the layer in the order their
//! sequences were booked, and so a layer swapped for a window's own never gets one late. `book`
//! is taken inside it for a moment, and alone by the layer's report, which `VideoLayer::present`
//! can make on the presenting thread itself. Nothing takes `slot` while it holds `book`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::PresentedFrame;
use gpui::composition::NativeHost;
use gpui_apple::fast::video_layer::{VideoLayer, VideoLayerOptions};
use parking_lot::Mutex;
use slopty_client::Presentable;
use slopty_client::pacing::{FrameStamp, Pace, Pacer, PacingStats};
use tokio::sync::watch;

use super::{Chroma, chroma_of};

/// Pictures put up and not yet reported that are remembered: a report can go missing when the
/// layer leaves the screen, and a forgotten one is a picture never timed, not a leak.
const FLYING: usize = 8;

/// What the view draws around the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Shape {
    /// The picture in pixels.
    pub size: (u32, u32),
    /// How much colour it carries.
    pub chroma: Chroma,
}

impl Shape {
    /// The shape of `picture`.
    pub(super) fn of(picture: &Picture) -> Self {
        #[expect(clippy::cast_possible_truncation, reason = "pixel counts")]
        let size = (picture.0.get_width() as u32, picture.0.get_height() as u32);
        Self { size, chroma: chroma_of(picture.0.get_pixel_format()) }
    }
}

/// A decoded picture the decoder's thread, the layer's and the main thread all hold.
#[derive(Clone)]
pub(super) struct Picture(CVPixelBuffer);

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
        Self(buffer)
    }

    /// The decoder's picture, in the wrapper GPUI and the layer take.
    pub(super) fn of(frame: &Presentable) -> Self {
        let raw = std::ptr::from_ref(frame.frame.image.as_cv())
            .cast_mut()
            .cast::<core_video::buffer::__CVBuffer>();
        // SAFETY: `raw` is a live `CVPixelBufferRef` the frame owns; `wrap_under_get_rule`
        // takes a retain of its own, so the wrapper outlives the frame.
        Self(unsafe { CVPixelBuffer::wrap_under_get_rule(raw) })
    }

    /// The buffer.
    pub(super) fn buffer(&self) -> CVPixelBuffer {
        self.0.clone()
    }
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
    /// The newest picture the pacer took: a render that captures the window draws it, and a
    /// layer attached after it came shows it first.
    last: Option<Picture>,
    /// `last` has not been put up on a layer yet.
    waiting: bool,
    generations: u64,
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
    /// The thread inside `VideoLayer::present` now, which reports there a picture its mailbox
    /// replaced.
    presenting: Option<std::thread::ThreadId>,
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

    /// Put `slot`'s newest picture up on its layer, if it has one.
    fn present(&self, slot: &mut Slot) {
        let Slot { layer: Some(layer), last: Some(picture), waiting, .. } = slot else {
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
        let presented = layer.video.present(&picture.0);
        self.book.lock().presenting = None;
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

    /// Let go of the layer: pictures wait here until another is attached.
    pub(super) fn detach(&self) {
        let layer = self.slot.lock().layer.take();
        drop(layer);
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

    /// How long ago the picture on the glass got there.
    pub(super) fn age(&self) -> Option<Duration> {
        self.book.lock().pacer.age()
    }
}

/// What became of the picture the layer of `generation` numbered `sequence`.
fn reported(glass: &Weak<Glass>, generation: u64, sequence: u64, frame: PresentedFrame) {
    let Some(glass) = glass.upgrade() else { return };
    let mut book = glass.book.lock();
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
    FrameStamp { pts_us: index.saturating_add(1), decode_seq: index, arrived: now, decoded: now }
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
        assert_eq!((last.0.get_width(), last.0.get_height()), (32, 16));
        assert_eq!(shapes.borrow().map(|shape| shape.size), Some((32, 16)), "not the late one's");
        let pacing = glass.pacing();
        assert_eq!((pacing.skipped, pacing.late), (1, 1));
    }
}
