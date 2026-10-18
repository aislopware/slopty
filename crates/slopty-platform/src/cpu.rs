//! The CPU time a thread has spent, for a probe that judges work by what it costs rather than
//! by the wall clock, which a loaded machine stretches with time spent waiting to run.

use std::time::Duration;

/// The CPU time the calling thread has spent running, user and system together; `None` when the
/// system will not say.
#[must_use]
pub fn this_thread() -> Option<Duration> {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `now` is a live `timespec` the call fills; the clock id is one it knows.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    if status != 0 {
        return None;
    }
    let secs = u64::try_from(now.tv_sec).ok()?;
    let nanos = u32::try_from(now.tv_nsec).ok()?;
    Some(Duration::new(secs, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Work moves a thread's CPU time; waiting on another thread's work does not.
    #[test]
    fn a_threads_cpu_time_counts_its_own_work_only() {
        let before = this_thread().unwrap();
        let mut x = 0_u64;
        let busy = std::time::Instant::now();
        while busy.elapsed() < Duration::from_millis(50) {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let worked = this_thread().unwrap().saturating_sub(before);
        assert!(worked >= Duration::from_millis(20), "50 ms of work: {worked:?}");

        let before = this_thread().unwrap();
        std::thread::spawn(|| {
            let busy = std::time::Instant::now();
            while busy.elapsed() < Duration::from_millis(50) {
                std::hint::black_box(0_u64);
            }
        })
        .join()
        .unwrap();
        let idle = this_thread().unwrap().saturating_sub(before);
        assert!(idle < Duration::from_millis(20), "another's work: {idle:?}");
    }
}
