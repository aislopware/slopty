//! The `cursor` target: [`slopty_fuzz::datagram::cursor`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::datagram::cursor(data));
