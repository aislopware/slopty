//! The `visual` target: [`slopty_fuzz::visual::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

slopty_fuzz::target!(
    init: slopty_fuzz::contain_panics_in(slopty_fuzz::visual::CONTAINED),
    |data| slopty_fuzz::visual::run(data)
);
