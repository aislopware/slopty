//! The keystroke path's threads ask for `QOS_CLASS_USER_INTERACTIVE`.
//!
//! A thread nobody classed runs below a build's user-initiated threads, so on a busy Mac a
//! keystroke's echo waited behind the build: 5–6 ms p90 inside the worker against 0.2–0.5 ms
//! classed (MEASUREMENTS.md, "the keystroke path under an all-core spin"). The process's
//! `LatencyCritical` activity (`slopty_platform::Activity`) keeps its timers sharp but classes
//! no thread.

/// Put the calling thread in `QOS_CLASS_USER_INTERACTIVE`, the class of the work a person is
/// waiting on (the video lanes' dispatch queues have it already).
pub fn user_interactive() {
    // SAFETY: `pthread_set_qos_class_self_np` (pthread/qos.h) changes only the calling thread's
    // own class, and a relative priority of 0 is always within the class's range.
    let refused = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    if refused != 0 {
        tracing::warn!(error = refused, "user-interactive QoS refused");
    }
}
