//! The `nal` target: [`slopty_fuzz::nal::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::nal::run(data));
