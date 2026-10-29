//! The `reassemble` target: [`slopty_fuzz::reassemble::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(|data| slopty_fuzz::reassemble::run(data));
