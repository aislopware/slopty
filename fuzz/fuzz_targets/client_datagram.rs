//! The `client_datagram` target: [`slopty_fuzz::datagram::client_datagram`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::datagram::client_datagram(data));
