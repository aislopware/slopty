//! A stream's [`Injector`] on a thread of its own, so the stream's task only queues.
//!
//! Everything an injection can wait on happens here: the window server's answer for the target's
//! bounds (p95 2–6 ms, 93 ms at worst), the `NSRunningApplication` lookup behind activation
//! (1–2 ms each), the post itself. A second thread re-reads the bounds while pointer input
//! flows, so a move mid-drag is mapped with bounds already read and waits on nothing
//! (MEASUREMENTS.md, "input injection off the runtime").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use slopty_capture::Rect;
use slopty_proto::screen::{CaptureTarget, ScreenInput};

use crate::backend::{Backend, System};
use crate::injector::{BOUNDS_TTL, Injector};
use crate::{InputError, InputSink};

/// How often the bounds are re-read while pointer input flows: inside [`BOUNDS_TTL`], so the
/// injector never finds them stale mid-gesture.
const REFRESH_EVERY: Duration = BOUNDS_TTL.saturating_sub(Duration::from_millis(20));

/// Refresh periods without pointer input before the re-reads stop: about a second, so a hand
/// resting on the mouse does not cost a read on the next move.
const REFRESH_QUIET: u32 = 12;

/// The worker's [`InputSink`]: an [`InputThread`] posting real `CGEvent`s.
pub type CgEvents = InputThread;

/// One stream's input, injected in order on a thread of its own.
///
/// Dropping it lets go of everything held down on the worker and ends the threads.
#[derive(Debug)]
pub struct InputThread {
    jobs: Sender<Job>,
}

#[derive(Debug)]
enum Job {
    Input(ScreenInput),
    Focus,
    Scale(f64),
    Release,
}

/// Whether pointer input came since the bounds were last read, and whether the reader is
/// waiting to be woken for more.
#[derive(Debug, Default)]
struct Pointer {
    used: AtomicBool,
    idle: AtomicBool,
}

impl InputThread {
    /// An injector for `target` whose stream has `scale` pixels per display point, posting
    /// through `backend` on a new thread. A thread that cannot be started is logged, and every
    /// injection answers [`InputError::Stopped`].
    pub fn spawn<B>(target: CaptureTarget, scale: f64, backend: B) -> Self
    where
        B: Backend + Clone + Send + 'static,
    {
        let (jobs, queue) = mpsc::channel();
        let started = std::thread::Builder::new()
            .name("slopty-input".to_owned())
            .spawn(move || serve(target, scale, backend, &queue));
        if let Err(e) = started {
            tracing::warn!(?target, error = %e, "input thread");
        }
        Self { jobs }
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

    fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
        self.send(Job::Input(input.clone()))
    }

    fn focus(&mut self) -> Result<(), InputError> {
        self.send(Job::Focus)
    }

    fn release_all(&mut self) {
        let _stopped = self.send(Job::Release);
    }
}

/// The input thread: jobs in order until the handle is dropped, then the injector's drop lets
/// go of what is held.
fn serve<B>(target: CaptureTarget, scale: f64, backend: B, queue: &Receiver<Job>)
where
    B: Backend + Clone + Send + 'static,
{
    let (published, fresh) = mpsc::channel();
    let (wake, woken) = mpsc::sync_channel(1);
    let pointer = Arc::new(Pointer::default());
    let reader = backend.clone();
    let shared = Arc::clone(&pointer);
    let started = std::thread::Builder::new()
        .name("slopty-input-bounds".to_owned())
        .spawn(move || read_bounds(reader, target, &published, &woken, &shared));
    if let Err(e) = started {
        // The injector reads the bounds itself when they go stale.
        tracing::warn!(?target, error = %e, "input bounds thread");
    }
    let mut injector = Injector::with_backend(target, scale, backend);
    while let Ok(job) = queue.recv() {
        while let Ok((bounds, at)) = fresh.try_recv() {
            injector.set_bounds(bounds, at);
        }
        let done = match job {
            Job::Input(input) => {
                if is_pointer(&input) {
                    wake_reader(&pointer, &wake);
                }
                injector.inject(&input)
            }
            Job::Focus => injector.focus(),
            Job::Scale(scale) => {
                injector.set_scale(scale);
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

const fn is_pointer(input: &ScreenInput) -> bool {
    matches!(
        input,
        ScreenInput::Move { .. } | ScreenInput::Button { .. } | ScreenInput::Scroll { .. }
    )
}

fn wake_reader(pointer: &Pointer, wake: &SyncSender<()>) {
    pointer.used.store(true, Ordering::Relaxed);
    if pointer.idle.load(Ordering::Relaxed) {
        // Full means a wake is already on its way.
        let _pending = wake.try_send(());
    }
}

/// The bounds thread: a read every [`REFRESH_EVERY`] while pointer input flows, none once it
/// has been quiet for [`REFRESH_QUIET`] periods until the input thread wakes it. A wake that
/// races the reader going idle costs the next event one read of its own, never a wrong point.
fn read_bounds<B: Backend>(
    mut backend: B,
    target: CaptureTarget,
    published: &Sender<(Option<Rect>, Instant)>,
    woken: &Receiver<()>,
    pointer: &Pointer,
) {
    let mut quiet = 0_u32;
    loop {
        let at = Instant::now();
        let bounds = backend.bounds(target);
        if published.send((bounds, at)).is_err() {
            return;
        }
        if matches!(woken.recv_timeout(REFRESH_EVERY), Err(RecvTimeoutError::Disconnected)) {
            return;
        }
        if pointer.used.swap(false, Ordering::Relaxed) {
            quiet = 0;
            continue;
        }
        quiet = quiet.saturating_add(1);
        if quiet < REFRESH_QUIET {
            continue;
        }
        pointer.idle.store(true, Ordering::Relaxed);
        let woke = woken.recv();
        pointer.idle.store(false, Ordering::Relaxed);
        if woke.is_err() {
            return;
        }
        quiet = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{CGEventFlags, CGEventType};
    use slopty_core::WindowId;
    use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};

    use super::*;
    use crate::backend::{Event, Post, Recorder};
    use crate::keymap;

    /// A [`Recorder`] whose posts leave through a channel, so a test can read them while the
    /// injector lives on its thread, and whose bounds reads are counted by the thread that made
    /// them.
    #[derive(Clone, Debug)]
    struct Tap {
        recorder: Recorder,
        posts: Sender<Post>,
        /// Reads on the input thread, in front of an event.
        inline: Arc<AtomicUsize>,
        /// Reads on the bounds thread.
        beside: Arc<AtomicUsize>,
    }

    impl Tap {
        fn new(recorder: Recorder) -> (Self, Receiver<Post>) {
            let (posts, rx) = mpsc::channel();
            let tap = Self { recorder, posts, inline: Arc::default(), beside: Arc::default() };
            (tap, rx)
        }
    }

    impl Backend for Tap {
        fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
            self.recorder.owner_pid(target)
        }

        fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
            let beside = std::thread::current().name() == Some("slopty-input-bounds");
            let count = if beside { &self.beside } else { &self.inline };
            count.fetch_add(1, Ordering::Relaxed);
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

    /// Everything posted until the injector and the bounds reader have both let go of the tap.
    fn drain(posts: &Receiver<Post>) -> Vec<Post> {
        let mut all = Vec::new();
        loop {
            match posts.recv_timeout(Duration::from_secs(5)) {
                Ok(post) => all.push(post),
                Err(RecvTimeoutError::Disconnected) => return all,
                Err(RecvTimeoutError::Timeout) => panic!("the input threads did not end: {all:?}"),
            }
        }
    }

    /// A stream dropped mid-⌘-drag (the connection went, or the stream closed) leaves nothing
    /// down on the worker: the input thread posts what was queued, then lets go of the button,
    /// the key and ⌘, in that order, the last with no flags left.
    #[test]
    fn dropping_the_sink_lets_go_of_what_it_held() {
        let bounds = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };
        let (tap, posts) = Tap::new(Recorder::window(7, bounds));
        let mut sink = InputThread::spawn(CaptureTarget::Window(WindowId(3)), 1.0, tap);
        let key = |code, action| ScreenInput::Key { code, action, mods: Mods::SUPER, text: None };
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

    /// While pointer input flows the bounds come from the reader thread: over 400 ms of moves
    /// at 200 Hz the input thread reads them at most once, for the first move if it beat the
    /// reader's first read, and every move is mapped through them.
    #[test]
    fn the_bounds_are_read_beside_the_pointer_not_in_front_of_it() {
        let bounds = Rect { x: 100.0, y: 0.0, w: 800.0, h: 600.0 };
        let (tap, posts) = Tap::new(Recorder::display(bounds));
        let (inline, beside) = (Arc::clone(&tap.inline), Arc::clone(&tap.beside));
        let mut sink = InputThread::spawn(CaptureTarget::Display(1), 1.0, tap);
        let (_never, pause) = mpsc::channel::<()>();
        let started = Instant::now();
        let mut moves = 0_usize;
        while started.elapsed() < BOUNDS_TTL.saturating_mul(4) {
            #[expect(clippy::cast_precision_loss, reason = "a small count")]
            let x = (moves % 100) as f32;
            sink.inject(&ScreenInput::Move { x, y: 0.0 }).unwrap();
            moves = moves.saturating_add(1);
            let _paced = pause.recv_timeout(Duration::from_millis(5));
        }
        drop(sink);
        let posted = drain(&posts);
        assert_eq!(posted.len(), moves);
        assert!(
            posted.iter().all(|p| matches!(p.event, Event::Mouse { at, .. } if at.x >= 100.0)),
            "every move mapped through the bounds"
        );
        let (inline, beside) = (inline.load(Ordering::Relaxed), beside.load(Ordering::Relaxed));
        assert!(inline <= 1, "{inline} reads in front of a move");
        assert!(beside >= 4, "{beside} reads beside");
    }
}
