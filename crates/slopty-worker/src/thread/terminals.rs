//! The worker's terminals, as an adapter runs an agent's own TUI in them: Claude Code started
//! from a client, pi's TUI when a session is handed to it.

use std::future::Future;
use std::pin::Pin;

use slopty_core::SessionId;

/// A future the terminals hand back.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The worker's terminals.
pub trait Terminals: Send + Sync + 'static {
    /// Open a terminal in `cwd` running `command`, with `env` over the worker's own; its
    /// session, or why not.
    fn open(
        &self,
        command: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    ) -> Pending<'_, Result<SessionId, String>>;

    /// Done once `session`'s program has exited, or at once when there is no such session.
    fn exited(&self, session: SessionId) -> Pending<'static, ()>;

    /// End `session`'s program and close the terminal.
    fn close(&self, session: SessionId) -> Pending<'_, ()>;
}
