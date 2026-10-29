//! The `uni_stream` target: [`slopty_fuzz::stream::uni_stream`].

#![cfg_attr(feature = "libfuzzer", no_main)]

/// Counts the heap a decode takes, which the target bounds.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

slopty_fuzz::target!(|data| slopty_fuzz::stream::uni_stream(data));
