//! The `registry` target: [`slopty_fuzz::registry::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::registry::run(data));
