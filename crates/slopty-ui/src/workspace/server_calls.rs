//! Calls to the server: how the workspace reaches it while the app is linked to one (a reply to
//! an agent whose machine is not linked here, a machine's settings, waking a worker).

use slopty_client::server::ServerCaller;

use super::WorkspaceView;

impl WorkspaceView {
    /// How calls reach the server: set while the app is linked to one, and cleared when it
    /// lets the server go.
    pub fn set_server_caller(&mut self, caller: Option<ServerCaller>) {
        self.server = caller;
    }
}
