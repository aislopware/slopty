//! The `terminal` target: [`slopty_fuzz::terminal::run`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::terminal::run(data));
