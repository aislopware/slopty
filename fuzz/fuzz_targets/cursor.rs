//! The `cursor` target: [`slopty_fuzz::datagram::cursor`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::datagram::cursor(data));
