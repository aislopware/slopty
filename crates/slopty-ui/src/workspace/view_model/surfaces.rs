//! The interface's right-pane surfaces and settings pages, answered from the workspace: the
//! calls the shell's changes, files, terminal and settings make of the view model
//! ([`crate::view_model`]).
//!
//! A call is the surfaces' when [`WorkspaceView::view_surface_call`] gives its answer, a value
//! or a watch's stream; the rest go to the chats' calls, and what nobody answers is an unknown
//! method.

use gpui::{Context, Task};
use slopty_view_rpc::{RpcError, RpcReply};

use crate::workspace::WorkspaceView;

impl WorkspaceView {
    /// The answer to `method` when a surface makes it; `None` for every other call.
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        clippy::missing_const_for_fn,
        unused_variables,
        reason = "no surface answers yet; the surfaces' calls come with them"
    )]
    pub(super) fn view_surface_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        None
    }
}
