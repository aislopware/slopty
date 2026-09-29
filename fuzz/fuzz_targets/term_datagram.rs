//! The `term_datagram` target: [`slopty_fuzz::datagram::term_datagram`].

#![no_main]

/// Counts the heap a decode takes, which the target bounds.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::datagram::term_datagram(data));
