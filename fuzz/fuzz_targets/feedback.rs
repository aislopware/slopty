//! The `feedback` target: [`slopty_fuzz::feedback::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::feedback::run(data));
