//! The `worker_msg` target: [`slopty_fuzz::stream::worker_msg`].

#![no_main]

/// Counts the heap a decode takes, which the target bounds.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::stream::worker_msg(data));
