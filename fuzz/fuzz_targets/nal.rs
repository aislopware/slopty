//! The `nal` target: [`slopty_fuzz::nal::run`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::nal::run(data));
