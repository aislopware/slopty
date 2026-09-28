//! What the tests measure with, in one place: never a dependency of a shipped binary.
//!
//! - [`alloc`]: a counting global allocator, so a test can hold a hot path to a number of
//!   allocations and bytes. The counts are per thread and deterministic, so they gate.
//! - [`stats`]: the percentiles every measurement prints.
//! - [`process`]: a process's retired instructions, footprint, open descriptors and threads, read
//!   from the kernel with no root (`proc_pid_rusage`, `proc_pidinfo`).
//! - [`bench`](mod@bench): a measurement's samples, timed and counted in instructions, printed and
//!   written as one JSON line for `cargo xtask bench` to hold against its budget.

pub mod alloc;
pub mod bench;
pub mod process;
pub mod stats;
