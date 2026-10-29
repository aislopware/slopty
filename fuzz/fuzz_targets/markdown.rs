//! The `markdown` target: [`slopty_fuzz::markdown::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(
    init: slopty_fuzz::contain_panics_in(slopty_fuzz::markdown::CONTAINED),
    |data| slopty_fuzz::markdown::run(data)
);
