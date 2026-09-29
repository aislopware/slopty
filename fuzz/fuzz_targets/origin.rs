//! The `origin` target: [`slopty_fuzz::datagram::origin`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::datagram::origin(data));
