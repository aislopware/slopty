//! The `client_datagram` target: [`slopty_fuzz::datagram::client_datagram`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| slopty_fuzz::datagram::client_datagram(data));
