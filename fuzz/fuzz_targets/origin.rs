//! The `origin` target: [`slopty_fuzz::datagram::origin`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::datagram::origin(data));
