//! Load for measurements, not a check: every core spun at `QOS_CLASS_USER_INITIATED`, the class
//! a build's threads run at, for `SLOPTY_LOAD_SECS` seconds (30 by default). Run it beside the
//! thing being measured (MEASUREMENTS.md, "the keystroke path under an all-core spin"):
//!
//! ```sh
//! cargo nextest run -p slopty-worker --release --test load --run-ignored only
//! ```

#[cfg(test)]
mod spin {
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "load for a measurement, run on demand"]
    fn every_core_at_user_initiated() {
        let secs =
            std::env::var("SLOPTY_LOAD_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        let until = Instant::now() + Duration::from_secs(secs);
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let spinners: Vec<_> = std::iter::repeat_with(|| {
            std::thread::spawn(move || {
                // SAFETY: `pthread_set_qos_class_self_np` (pthread/qos.h) changes only the
                // calling thread's own class; a relative priority of 0 is always in range.
                let set = unsafe {
                    libc::pthread_set_qos_class_self_np(
                        libc::qos_class_t::QOS_CLASS_USER_INITIATED,
                        0,
                    )
                };
                assert_eq!(set, 0, "QoS class refused");
                let mut turns = 0_u64;
                while Instant::now() < until {
                    turns = std::hint::black_box(turns.wrapping_add(1));
                }
                turns
            })
        })
        .take(cores)
        .collect();
        for spinner in spinners {
            let _turns = spinner.join().expect("a spinner");
        }
    }
}
