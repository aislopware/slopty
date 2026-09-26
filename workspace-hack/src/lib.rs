//! The feature union of the workspace's third-party dependencies, kept by `cargo hakari`.
//!
//! No crate depends on this one: that would turn the tests' features (GPUI's `test-support`,
//! tokio's `test-util`) on in shipped binaries. `cargo xtask check` names it beside the crates
//! it checks instead, so every crate set resolves each dependency with the features a
//! `--workspace` build gives it and they all share one build of it
//! (`docs/decisions/tooling.md`, "target/ stays bounded").
