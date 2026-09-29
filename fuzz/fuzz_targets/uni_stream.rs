//! The `uni_stream` target: [`slopty_fuzz::stream::uni_stream`].

#![no_main]

/// Counts the heap a decode takes, which the target bounds.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::stream::uni_stream(data));
