//! A hang of the main thread is filed like a crash: GPUI's hang monitor watches the foreground
//! journal from a thread of its own, and each hang past [`THRESHOLD`] becomes a report next to
//! the crashes (`slopty_crash::record_hang`), which `slopty crashes` lists. The process lives on.
//!
//! The monitor starts once the workspace window has drawn its first frame (`open_window` draws
//! it before it returns), and sees only what comes after: the launch's own work (fonts, the
//! first layout) is not a hang. It does not wait for the first frame on screen, which a window
//! opened behind others never shows until it is brought forward.
//!
//! The journal times every piece of main-thread work (a task's poll, an action, an input, a
//! draw, a present); the monitor reads it every [`POLL`], so a report lands at most that long
//! after the frame that ended the hang. What that timing costs a frame is measured in
//! `docs/MEASUREMENTS.md`, "hang reports".

use std::time::Duration;

use gpui::App;
use gpui::profiler::hang::{HangMonitorConfig, HangMonitorPoll, HangTrigger};
use gpui::profiler::journal::ForegroundEvent;

/// One piece of main-thread work this long is a hang: a quarter second without a frame or an
/// answer to a key is when the app reads as stuck, not slow.
const THRESHOLD: Duration = Duration::from_millis(250);

/// As much main-thread work before one frame, in pieces each shorter than [`THRESHOLD`], is a
/// hang too: the user waited as long.
const FRAME_BUDGET: Duration = THRESHOLD;

/// How often the monitor's thread reads the journal. What happened meanwhile waits in the
/// journal, so this sets only how late a report lands, and how often an idle app wakes.
const POLL: Duration = Duration::from_secs(2);

/// The pieces of work a report names, longest first; the rest say nothing new.
const NAMED: usize = 8;

/// The app's hang monitor is running.
struct Watching;

impl gpui::Global for Watching {}

/// Start filing this process's hangs. Once per app; later calls do nothing.
pub(crate) fn watch(cx: &mut App) {
    if cx.has_global::<Watching>() {
        return;
    }
    cx.set_global(Watching);
    let config =
        HangMonitorConfig { threshold: THRESHOLD, frame_budget: FRAME_BUDGET, interval: POLL };
    if let Err(error) = cx.start_hang_monitor(config, |poll| file(&poll)) {
        tracing::warn!(%error, "hangs will not be reported");
    }
}

/// On the monitor's thread: each hang since the last poll, filed.
fn file(poll: &HangMonitorPoll) {
    for incident in &poll.incidents {
        let (start, end) = incident.active_window();
        let hang =
            hang(incident.trigger, end.saturating_duration_since(start), &incident.contributors);
        let stall_ms = hang.stall.as_millis();
        match slopty_crash::record_hang(hang) {
            Ok(path) => tracing::warn!(stall_ms, path = %path.display(), "the main thread hung"),
            Err(error) => tracing::warn!(%error, stall_ms, "a hang went unreported"),
        }
    }
}

/// The report of a hang: `contributors` longest first, as the monitor gives them.
fn hang(
    trigger: HangTrigger,
    active: Duration,
    contributors: &[ForegroundEvent],
) -> slopty_crash::Hang {
    let longest = contributors.iter().max_by_key(|event| event.duration());
    slopty_crash::Hang {
        stall: longest.map_or(Duration::ZERO, ForegroundEvent::duration),
        active,
        cause: longest.map_or_else(|| "main-thread work".to_owned(), cause),
        piled_up: trigger == HangTrigger::Budget,
        frames: contributors.iter().take(NAMED).filter_map(frame).collect(),
    }
}

/// A piece of main-thread work in words.
fn cause(event: &ForegroundEvent) -> String {
    match event {
        ForegroundEvent::TaskPoll(task) => {
            format!("a task spawned at {}:{}", task.location.file(), task.location.line())
        }
        ForegroundEvent::Action(action) => format!("the action {}", action.name),
        ForegroundEvent::Input(input) => format!("a {} input", input.kind),
        ForegroundEvent::Draw(_) => "drawing a window".to_owned(),
        ForegroundEvent::Present(_) => "presenting a frame".to_owned(),
        ForegroundEvent::SmallPolls(_) => "many short tasks".to_owned(),
    }
}

/// Where a piece of work came from, when the journal knows: a task's spawn site.
fn frame(event: &ForegroundEvent) -> Option<slopty_crash::Frame> {
    let ForegroundEvent::TaskPoll(task) = event else { return None };
    Some(slopty_crash::Frame {
        function: Some(cause(event)),
        file: Some(task.location.file().to_owned()),
        line: Some(task.location.line()),
        ..slopty_crash::Frame::default()
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui::profiler::ActionTiming;
    use gpui::profiler::hang::HangTrigger;
    use gpui::profiler::journal::ForegroundEvent;

    use super::hang;

    /// The instant `after` past `start`.
    fn at(start: Instant, after: Duration) -> Instant {
        start.checked_add(after).unwrap_or(start)
    }

    fn action(name: &'static str, start: Instant, ms: u64) -> ForegroundEvent {
        ForegroundEvent::Action(ActionTiming {
            name,
            start,
            end: at(start, Duration::from_millis(ms)),
        })
    }

    /// A hang names the longest piece of work and how long it held the thread; one made of
    /// many short pieces says so.
    #[test]
    fn a_hang_names_its_longest_work() {
        let start = Instant::now();
        let events =
            [action("file::SaveFile", start, 40), action("workspace::ZoomPane", start, 310)];
        let one = hang(HangTrigger::Threshold, Duration::from_millis(360), &events);
        assert_eq!(one.stall, Duration::from_millis(310));
        assert_eq!(one.cause, "the action workspace::ZoomPane");
        assert!(!one.piled_up);
        assert_eq!(one.active, Duration::from_millis(360));

        let many = hang(HangTrigger::Budget, Duration::from_millis(300), &events[..1]);
        assert!(many.piled_up, "no single piece past the threshold");
        assert_eq!(many.cause, "the action file::SaveFile");
    }
}
