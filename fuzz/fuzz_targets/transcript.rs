//! The `transcript` target: [`slopty_fuzz::transcript::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

/// Counts the heap a decode takes, which the target bounds.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

slopty_fuzz::target!(|data| slopty_fuzz::transcript::run(data));
