//! Codex, through its app-server.
//!
//! Codex runs one app-server daemon per `CODEX_HOME`, which every Codex client (its TUI, its
//! desktop app, Slopty) talks to over a WebSocket on a Unix socket. Slopty joins as one more
//! client: it reads the threads' turns and items as they stream, and answers an approval the
//! way the TUI would. [`protocol`] holds the wire's types, generated from the pinned Codex
//! build.

pub mod protocol;
pub mod rpc;
pub mod shared;
