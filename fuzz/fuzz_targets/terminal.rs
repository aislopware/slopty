//! The `terminal` target: [`slopty_fuzz::terminal::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::terminal::run(data));
