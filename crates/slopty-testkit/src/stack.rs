//! What the self-test's stack is, shared with the tests that stand in for it.
//!
//! The moment its app's clock is pinned to and the name its server goes by, so a golden drawn by
//! the stack and one drawn in the test process read the same.

/// The moment an app beside the stand-in `claude` reads as now: 2026-10-13T09:00:00Z.
///
/// The self-test pins its app's clock to it (`slopty_e2e::harness`), and the shell's in-process
/// goldens theirs, so a golden that shows an age or a date holds the same one in every run. It
/// lies before every run, so what the daemons stamp by the system's clock (a session's lease, a
/// machine's heartbeat) reads as fresh, never stale (`docs/decisions/testing.md`, "The
/// stand-in's times are a fixture's").
pub const PINNED_MS: u64 = 1_791_882_000_000;

/// The name the self-test's server goes by, which its app shows as the server it follows.
pub const SERVER: &str = "e2e-server";
