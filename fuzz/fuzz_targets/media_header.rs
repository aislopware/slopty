//! The `media_header` target: [`slopty_fuzz::datagram::media_header`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::datagram::media_header(data));
