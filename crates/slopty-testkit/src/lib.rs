//! What the tests measure with, in one place: never a dependency of a shipped binary.
//!
//! - [`bins`]: the daemons and doubles a test spawns, from the test's own build.
//! - [`env`](mod@env): the clean environment each of them starts from, never the person's.
//! - [`alloc`]: a counting global allocator, so a test can hold a hot path to a number of
//!   allocations and bytes. The counts are per thread and deterministic, so they gate.
//! - [`stats`]: the percentiles every measurement prints.
//! - [`process`]: a process's retired instructions, footprint, open descriptors and threads, read
//!   from the kernel with no root (`proc_pid_rusage`, `proc_pidinfo`).
//! - [`bench`](mod@bench): a measurement's samples, timed and counted in instructions, printed and
//!   written as one JSON line for `cargo xtask bench` to hold against its budget.
//! - [`soc`]: what the Mac's encode engines and GPU did over a span, from `IOReport`, with no root.
//! - [`live`]: how a live test that cannot run here skips, and why it fails instead in the VM live
//!   lane.
//!
//! And two doubles, binaries, so tests start agents without spending anyone's plan:
//! `slopty-stub-claude`, a stand-in for `claude` that speaks the hook protocol, calls Slopty's
//! tools through the `slopty mcp` it is handed, and records what it was given; and
//! `slopty-stub-pi`, a stand-in for `pi --mode rpc` that replays a recording of pi's RPC mode
//! against what it is sent.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod alloc;
pub mod bench;
pub mod bins;
pub mod env;
pub mod live;
pub mod process;
pub mod soc;
pub mod stats;
