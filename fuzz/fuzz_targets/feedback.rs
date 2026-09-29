//! The `feedback` target: [`slopty_fuzz::feedback::run`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::feedback::run(data));
