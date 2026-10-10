//! The screen the agent drives, which the palette opens beside the thread.
//!
//! It is the window or display the agent's tools last acted on ([`AgentScreen`], named by the
//! worker), which opens beside the thread as a stream the person watches and can take control
//! of.

use gpui::Context;
use slopty_proto::thread::AgentScreen;

use super::{ThreadView, ThreadViewEvent};

impl ThreadView {
    /// The screen the agent drove last, while the worker still offers it.
    pub(super) fn agent_screen(&self, cx: &gpui::App) -> Option<AgentScreen> {
        self.state(cx).and_then(|s| s.screens.first().cloned())
    }

    /// Open the screen the agent drove last beside the thread.
    pub(super) fn watch_screen(&self, cx: &mut Context<Self>) {
        if let Some(screen) = self.agent_screen(cx) {
            cx.emit(ThreadViewEvent::Watch { thread: self.thread, screen });
        }
    }
}
