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

    /// Spin on the calling thread until its own CPU clock has moved `cpu`: an amount of work
    /// measured in what it costs, so a loaded machine only stretches how long it takes.
    fn work(cpu: Duration) {
        let start = this_thread().unwrap();
        while this_thread().unwrap().saturating_sub(start) < cpu {
            for _ in 0..1_000 {
                std::hint::black_box(0_u64);
            }
        }
    }

    /// Work moves a thread's CPU time, never faster than the wall clock; waiting on another
    /// thread's work does not move it. Both sides are amounts of CPU, not of wall time: a loaded
    /// host gives a wall-timed spin a fraction of its time on a core, too little to tell apart
    /// from the wait.
    #[test]
    fn a_threads_cpu_time_counts_its_own_work_only() {
        let wall = std::time::Instant::now();
        let before = this_thread().unwrap();
        work(Duration::from_millis(20));
        let worked = this_thread().unwrap().saturating_sub(before);
        assert!(worked >= Duration::from_millis(20), "20 ms of work: {worked:?}");
        assert!(worked <= wall.elapsed(), "{worked:?} of CPU in {:?}", wall.elapsed());

        let before = this_thread().unwrap();
        let theirs = std::thread::spawn(|| {
            let start = this_thread().unwrap();
            work(Duration::from_millis(50));
            this_thread().unwrap().saturating_sub(start)
        })
        .join()
        .unwrap();
        let idle = this_thread().unwrap().saturating_sub(before);
        assert!(theirs >= Duration::from_millis(50), "their 50 ms of work: {theirs:?}");
        assert!(
            idle < Duration::from_millis(20),
            "waiting on {theirs:?} of another's work: {idle:?}"
        );
    }
}
