//! The `media_header` target: [`slopty_fuzz::datagram::media_header`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::datagram::media_header(data));
