//! A stream's [`Injector`] on a thread of its own, so the stream's task only queues.
//!
//! Everything an injection can wait on happens here: the `NSRunningApplication` lookup behind
//! activation (1–2 ms each), the post itself, and, when nobody handed fresher ones over, the
//! window server's answer for the target's bounds (p95 2–6 ms, 93 ms at worst). The stream's
//! geometry probe reads the bounds off the runtime every 100 ms whether or not input flows and
//! hands each read over ([`InputSink::set_bounds`]), in order with the input, so a move is mapped
//! with bounds already read and waits on nothing (MEASUREMENTS.md, "input injection off the
//! runtime").

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use slopty_capture::Rect;
use slopty_proto::screen::{CaptureTarget, ScreenInput};

use crate::backend::{Backend, System};
use crate::injector::Injector;
use crate::{InputError, InputSink, PointerWatch};

/// The worker's [`InputSink`]: an [`InputThread`] posting real `CGEvent`s.
pub type CgEvents = InputThread;

/// One stream's input, injected in order on a thread of its own.
///
/// Dropping it lets go of everything held down on the worker and ends the thread.
#[derive(Debug)]
pub struct InputThread {
    jobs: Sender<Job>,
    pointer: PointerWatch,
}

#[derive(Debug)]
enum Job {
    Input(ScreenInput),
    Focus,
    Scale(f64),
    Bounds(Option<Rect>, Instant),
    Release,
}

impl InputThread {
    /// An injector for `target` whose stream has `scale` pixels per display point, posting
    /// through `backend` on a new thread. A thread that cannot be started is logged, and every
    /// injection answers [`InputError::Stopped`].
    pub fn spawn<B>(target: CaptureTarget, scale: f64, backend: B) -> Self
    where
        B: Backend + Send + 'static,
    {
        let (jobs, queue) = mpsc::channel();
        let pointer = PointerWatch::default();
        let watch = pointer.clone();
        let started = std::thread::Builder::new()
            .name("slopty-input".to_owned())
            .spawn(move || serve(target, scale, backend, watch, &queue));
        if let Err(e) = started {
            tracing::warn!(?target, error = %e, "input thread");
        }
        Self { jobs, pointer }
    }

    fn send(&self, job: Job) -> Result<(), InputError> {
        self.jobs.send(job).map_err(|_gone| InputError::Stopped)
    }
}

impl InputSink for InputThread {
    fn new(target: CaptureTarget, scale: f64) -> Self {
        Self::spawn(target, scale, System)
    }

    fn set_scale(&mut self, scale: f64) {
        let _stopped = self.send(Job::Scale(scale));
    }

    fn set_bounds(&mut self, bounds: Option<Rect>, at: Instant) {
        let _stopped = self.send(Job::Bounds(bounds, at));
    }

    fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
        self.send(Job::Input(input.clone()))
    }

    fn focus(&mut self) -> Result<(), InputError> {
        self.send(Job::Focus)
    }

    fn release_all(&mut self) {
        let _stopped = self.send(Job::Release);
    }

    fn pointer(&self) -> PointerWatch {
        self.pointer.clone()
    }
}

/// The input thread: jobs in order until the handle is dropped, then the injector's drop lets
/// go of what is held.
fn serve<B: Backend>(
    target: CaptureTarget,
    scale: f64,
    backend: B,
    pointer: PointerWatch,
    queue: &Receiver<Job>,
) {
    let mut injector = Injector::with_backend(target, scale, backend);
    injector.report_pointer(pointer);
    while let Ok(job) = queue.recv() {
        let done = match job {
            Job::Input(input) => injector.inject(&input),
            Job::Focus => injector.focus(),
            Job::Scale(scale) => {
                injector.set_scale(scale);
                Ok(())
            }
            Job::Bounds(bounds, at) => {
                injector.set_bounds(bounds, at);
                Ok(())
            }
            Job::Release => {
                injector.release_all();
                Ok(())
            }
        };
        if let Err(e) = done {
            tracing::debug!(?target, error = %e, "input");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{CGEventFlags, CGEventType};
    use slopty_core::WindowId;
    use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};

    use super::*;
    use crate::backend::{Event, Post, Recorder};
    use crate::injector::BOUNDS_TTL;
    use crate::{Pointer, keymap};

    /// A [`Recorder`] whose posts leave through a channel, so a test can read them while the
    /// injector lives on its thread, and whose bounds reads are counted.
    #[derive(Debug)]
    struct Tap {
        recorder: Recorder,
        posts: Sender<Post>,
        reads: Arc<AtomicUsize>,
    }

    impl Tap {
        fn new(recorder: Recorder) -> (Self, Receiver<Post>) {
            let (posts, rx) = mpsc::channel();
            (Self { recorder, posts, reads: Arc::default() }, rx)
        }
    }

    impl Backend for Tap {
        fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
            self.recorder.owner_pid(target)
        }

        fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.recorder.bounds(target)
        }

        fn is_active(&mut self, pid: i32) -> bool {
            self.recorder.is_active(pid)
        }

        fn activate(&mut self, pid: i32) -> Result<(), InputError> {
            self.recorder.activate(pid)
        }

        fn post(&mut self, post: Post) -> Result<(), InputError> {
            let _gone = self.posts.send(post);
            Ok(())
        }
    }

    /// The next post, waiting for the input thread.
    fn next(posts: &Receiver<Post>) -> Post {
        posts.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    /// Everything posted until the injector has let go of the tap.
    fn drain(posts: &Receiver<Post>) -> Vec<Post> {
        let mut all = Vec::new();
        loop {
            match posts.recv_timeout(Duration::from_secs(5)) {
                Ok(post) => all.push(post),
                Err(RecvTimeoutError::Disconnected) => return all,
                Err(RecvTimeoutError::Timeout) => panic!("the input thread did not end: {all:?}"),
            }
        }
    }

    fn key(code: KeyCode, action: KeyAction) -> ScreenInput {
        ScreenInput::Key { code, action, mods: Mods::SUPER, text: None }
    }

    /// A stream dropped mid-⌘-drag (the connection went, or the stream closed) leaves nothing
    /// down on the worker: the input thread posts what was queued, then lets go of the button,
    /// the key and ⌘, in that order, the last with no flags left.
    #[test]
    fn dropping_the_sink_lets_go_of_what_it_held() {
        let bounds = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };
        let (tap, posts) = Tap::new(Recorder::window(7, bounds));
        let mut sink = InputThread::spawn(CaptureTarget::Window(WindowId(3)), 1.0, tap);
        sink.inject(&key(KeyCode::MetaLeft, KeyAction::Press)).unwrap();
        sink.inject(&key(KeyCode::C, KeyAction::Press)).unwrap();
        let press = ScreenInput::Button {
            button: MouseButton::Left,
            down: true,
            clicks: 1,
            x: 10.0,
            y: 20.0,
            mods: Mods::SUPER,
        };
        sink.inject(&press).unwrap();
        sink.inject(&ScreenInput::Move { x: 30.0, y: 40.0 }).unwrap();
        drop(sink);

        let posts = drain(&posts);
        let (vk_meta, vk_c) =
            (keymap::virtual_key(KeyCode::MetaLeft), keymap::virtual_key(KeyCode::C));
        let ups: Vec<(&Event, CGEventFlags)> =
            posts.iter().skip(4).map(|p| (&p.event, p.flags)).collect();
        assert_eq!(posts.len(), 7, "{posts:?}");
        assert!(
            matches!(
                ups[0],
                (Event::Mouse { kind: CGEventType::LeftMouseUp, at, .. }, _)
                    if *at == CGPoint::new(30.0, 40.0)
            ),
            "the button goes up where the pointer last was: {ups:?}"
        );
        assert!(
            matches!(ups[1], (Event::Key { vk, down: false, .. }, flags)
                if Some(*vk) == vk_c && flags.contains(CGEventFlags::MaskCommand)),
            "{ups:?}"
        );
        assert!(
            matches!(ups[2], (Event::Key { vk, down: false, modifier: true, .. }, flags)
                if Some(*vk) == vk_meta && flags.is_empty()),
            "{ups:?}"
        );
    }

    /// A stream that is ending lets go before its handle is dropped: `release_all` posts the
    /// releases while the sink still lives (the stream's close then waits on ScreenCaptureKit
    /// with nothing held), and the drop after it has nothing left to let go of.
    #[test]
    fn release_all_lets_go_while_the_sink_lives() {
        let bounds = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };
        let (tap, posts) = Tap::new(Recorder::window(7, bounds));
        let mut sink = InputThread::spawn(CaptureTarget::Window(WindowId(3)), 1.0, tap);
        sink.inject(&key(KeyCode::MetaLeft, KeyAction::Press)).unwrap();
        sink.inject(&key(KeyCode::C, KeyAction::Press)).unwrap();
        sink.release_all();

        let got: Vec<Post> = std::iter::repeat_with(|| next(&posts)).take(4).collect();
        let downs: Vec<bool> =
            got.iter().map(|p| matches!(p.event, Event::Key { down, .. } if down)).collect();
        assert_eq!(downs, [true, true, false, false], "{got:?}");
        drop(sink);
        assert_eq!(drain(&posts), [], "nothing held is left for the drop");
    }

    /// The bounds the stream's probe hands over are the only ones used: moves at 200 Hz for
    /// longer than the bounds live, with a hand-over every 100 ms as the probe does, read the
    /// window server not once, and each move maps through the latest bounds handed over before
    /// it, including after the target moved.
    #[test]
    fn the_probes_bounds_spare_every_read_in_front_of_the_pointer() {
        let unread = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };
        let (tap, posts) = Tap::new(Recorder::display(unread));
        let reads = Arc::clone(&tap.reads);
        let mut sink = InputThread::spawn(CaptureTarget::Display(1), 1.0, tap);
        let (_never, pause) = mpsc::channel::<()>();
        let probed = |x: f64| Some(Rect { x, y: 0.0, w: 800.0, h: 600.0 });
        let run = BOUNDS_TTL.saturating_mul(3);
        let started = Instant::now();
        let mut probe_at = started;
        let mut origin = 100.0;
        let mut moved_at = None;
        let mut origins = Vec::new();
        while started.elapsed() < run {
            if Instant::now() >= probe_at {
                if moved_at.is_none() && started.elapsed() >= run / 2 {
                    origin = 300.0;
                    moved_at = Some(origins.len());
                }
                sink.set_bounds(probed(origin), Instant::now());
                probe_at = probe_at.checked_add(Duration::from_millis(100)).unwrap();
            }
            sink.inject(&ScreenInput::Move { x: 10.0, y: 0.0 }).unwrap();
            origins.push(origin);
            let _paced = pause.recv_timeout(Duration::from_millis(5));
        }
        drop(sink);
        let posted = drain(&posts);
        assert!(moved_at.is_some(), "the target moved mid-run");
        assert_eq!(posted.len(), origins.len());
        for (post, origin) in posted.iter().zip(&origins) {
            assert!(
                matches!(post.event, Event::Mouse { at, .. } if at == CGPoint::new(origin + 10.0, 0.0)),
                "mapped through the bounds handed over: {post:?}, origin {origin}"
            );
        }
        assert_eq!(reads.load(Ordering::Relaxed), 0, "reads in front of a move");
    }

    /// A window stream's events go to its owner and leave the worker's pointer alone, so the
    /// pointer the stream shows is where the last event was put; a display stream's move the
    /// real one.
    #[test]
    fn a_window_streams_pointer_is_where_its_input_put_it() {
        let bounds = Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 };
        let (tap, posts) = Tap::new(Recorder::window(7, bounds));
        let mut window = InputThread::spawn(CaptureTarget::Window(WindowId(3)), 2.0, tap);
        assert_eq!(window.pointer().get(), Pointer::Placed(None), "nowhere before the first");
        window.inject(&ScreenInput::Move { x: 20.0, y: 40.0 }).unwrap();
        let _moved = next(&posts);
        assert_eq!(window.pointer().get(), Pointer::Placed(Some((110.0, 70.0))));

        let (tap, posts) = Tap::new(Recorder::display(bounds));
        let mut display = InputThread::spawn(CaptureTarget::Display(1), 2.0, tap);
        display.inject(&ScreenInput::Move { x: 20.0, y: 40.0 }).unwrap();
        let _moved = next(&posts);
        assert_eq!(display.pointer().get(), Pointer::Real);
    }
}
