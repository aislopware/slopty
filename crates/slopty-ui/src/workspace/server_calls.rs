//! Calls to the server: how the workspace reaches it while the app is linked to one (a reply to
//! an agent whose machine is not linked here, a machine's settings, waking a worker).

use gpui::Context;
use slopty_client::server::ServerCaller;
use slopty_proto::orchestration::{Outcome, Verb};

use super::WorkspaceView;

/// What an action that needs the server says with no server linked.
pub(crate) const NOT_SENT: &str = "Not sent: the server is away";

impl WorkspaceView {
    /// How calls reach the server: set while the app is linked to one, and cleared when it
    /// lets the server go.
    pub fn set_server_caller(&mut self, caller: Option<ServerCaller>) {
        self.server = caller;
    }

    /// An action pressed with no server linked says so ([`NOT_SENT`]) rather than doing
    /// nothing.
    fn not_sent(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| this.show_failure(NOT_SENT.to_owned(), cx))
        })
        .detach();
    }

    /// Send `verb` to the server and hand its answer to `then`; a refusal is said as a
    /// notice, in the server's words.
    pub(super) fn send_to_server(
        &self,
        verb: Verb,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) {
        let Some(caller) = self.server.clone() else {
            Self::not_sent(cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            this.update(cx, |this, cx| match outcome {
                Outcome::Error { message, .. } => this.show_failure(message, cx),
                _ => then(this, cx),
            })
        })
        .detach();
    }
}
