//! The `ctl` target: [`slopty_fuzz::stream::ctl`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::stream::ctl(data));
