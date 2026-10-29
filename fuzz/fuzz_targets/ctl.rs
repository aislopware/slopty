//! The `ctl` target: [`slopty_fuzz::stream::ctl`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::stream::ctl(data));
