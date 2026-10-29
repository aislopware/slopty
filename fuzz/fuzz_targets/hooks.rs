//! The `hooks` target: [`slopty_fuzz::hooks::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::hooks::run(data));
