//! The `reassemble` target: [`slopty_fuzz::reassemble::run`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::reassemble::run(data));
