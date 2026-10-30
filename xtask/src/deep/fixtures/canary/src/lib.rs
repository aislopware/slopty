//! Planted defects, one for each deep check that must catch it (`cargo xtask deep canary`,
//! `xtask/src/deep/canary.rs`). A check whose canary passes cannot fail, so its green says
//! nothing; each Deep job runs its canary and fails when the planted defect gets through.
//!
//! Every planted test does nothing unless `SLOPTY_CANARY` names it, so a plain run of the crate
//! passes: the mutants canary runs them all against every mutant of [`doubled`]. One test is a
//! guard rather than a plant, and passes everywhere it is right: `sigchld_is_heard_at_once`,
//! which fails under `ThreadSanitizer` without the `SIGCHLD` pump (`../sigchld.rs`).

/// Twice `x`. Its one test reads it and checks nothing, so every mutant of it lives: the
/// mutants canary.
#[must_use]
pub fn doubled(x: u32) -> u32 {
    x * 2
}

/// One, which a test checks: the line the coverage canary must see covered.
#[must_use]
pub fn covered() -> u32 {
    1
}

/// Two, which nothing calls: the line the coverage canary must see uncovered.
#[must_use]
pub fn uncovered() -> u32 {
    2
}

/// The features canary: it calls [`helper`], which only `helpers` builds, so `quick` alone does
/// not compile, while the default (both) and none do.
#[cfg(feature = "quick")]
#[must_use]
pub fn quick() -> u32 {
    helper()
}

#[cfg(feature = "helpers")]
#[must_use]
fn helper() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use std::cell::UnsafeCell;
    use std::ffi::c_int;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    /// Whether `SLOPTY_CANARY` plants `name`.
    fn planted(name: &str) -> bool {
        std::env::var_os("SLOPTY_CANARY").is_some_and(|v| v == name)
    }

    /// A counter two threads write without a lock.
    struct Unguarded(UnsafeCell<u64>);

    // SAFETY: it is not: the data race is the point, and `ThreadSanitizer` must see it.
    unsafe impl Sync for Unguarded {}

    static COUNT: Unguarded = Unguarded(UnsafeCell::new(0));

    /// `race`: two threads write one counter with nothing ordering the writes.
    #[test]
    fn a_data_race() {
        if !planted("race") {
            return;
        }
        // SAFETY: none; the race is planted.
        let other = std::thread::spawn(|| unsafe { *COUNT.0.get() += 1 });
        // SAFETY: none; the race is planted.
        unsafe { *COUNT.0.get() += 1 };
        other.join().unwrap();
    }

    /// `overflow`: a read one byte past a heap allocation.
    #[test]
    fn an_out_of_bounds_read() {
        if !planted("overflow") {
            return;
        }
        let bytes = vec![0_u8; 4].into_boxed_slice();
        // SAFETY: none; the read past the end is planted.
        let past = unsafe { bytes.as_ptr().add(bytes.len()).read_volatile() };
        std::hint::black_box(past);
    }

    /// `leak`: an allocation no pointer reaches once the test returns.
    #[test]
    fn a_leak() {
        if !planted("leak") {
            return;
        }
        let leaked: &'static mut [u8] = Box::leak(vec![7_u8; 1 << 16].into_boxed_slice());
        leaked[0] = 1;
        std::hint::black_box(leaked.len());
    }

    /// `failure`: a test that fails.
    #[test]
    fn a_failure() {
        assert!(!planted("failure"), "the planted failure");
    }

    #[cfg(target_os = "macos")]
    const SIGCHLD: c_int = 20;
    #[cfg(target_os = "linux")]
    const SIGCHLD: c_int = 17;

    unsafe extern "C" {
        fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
    }

    static HEARD: AtomicBool = AtomicBool::new(false);

    extern "C" fn heard(_: c_int) {
        HEARD.store(true, Ordering::Relaxed);
    }

    /// A child's `SIGCHLD` reaches its handler at once. Under `ThreadSanitizer` it waits for the
    /// thread the kernel gave it to call a function the runtime intercepts, which the harness's
    /// main thread does only at its one-minute notice, unless the pump takes it.
    #[test]
    #[expect(clippy::disallowed_methods, reason = "a test polling for its signal")]
    fn sigchld_is_heard_at_once() {
        // SAFETY: `heard` only stores to an atomic, which a signal handler may.
        unsafe { signal(SIGCHLD, heard) };
        let at = Instant::now();
        let mut child =
            std::process::Command::new("/bin/sh").args(["-c", "exit 0"]).spawn().unwrap();
        while !HEARD.load(Ordering::Relaxed) && at.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let took = at.elapsed();
        child.wait().unwrap();
        assert!(HEARD.load(Ordering::Relaxed), "no SIGCHLD in {took:?}");
        println!("SIGCHLD heard in {took:?}");
    }

    /// Reads [`super::doubled`] and checks nothing (the mutants canary).
    #[test]
    fn doubled_is_read() {
        std::hint::black_box(super::doubled(std::hint::black_box(2)));
    }

    /// Checks [`super::covered`] (the coverage canary).
    #[test]
    fn covered_is_one() {
        assert_eq!(super::covered(), 1);
    }
}
