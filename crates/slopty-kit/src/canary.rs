//! The GPUI seeds' canary (`cargo xtask deep canary gpui-seeds`): a test that fails on the second
//! scheduler seed of its process when `SLOPTY_CANARY=gpui-seeds` is set, as a test does whose
//! state outlives a seed (a thread-local, a static) and so passes alone and fails under the
//! nightly's `ITERATIONS`. The seeds run must fail on it and name the seed.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::TestAppContext;

/// The runs of the test in this process so far, across seeds.
static RUNS: AtomicUsize = AtomicUsize::new(0);

#[gpui::test]
fn a_planted_failure_on_a_later_seed(cx: &TestAppContext) {
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    cx.run_until_parked();
    let planted = std::env::var_os("SLOPTY_CANARY").is_some_and(|v| v == "gpui-seeds");
    assert!(!planted || run == 0, "the planted later seed: run {run}");
}
