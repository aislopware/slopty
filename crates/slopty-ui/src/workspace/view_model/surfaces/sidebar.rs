//! The sidebar's pins: this client's own, as the person arranges them, kept in
//! `interface/sidebar.json` under Slopty's data directory and said to the shell's watch.
//!
//! Each change is applied as the shell's own projection applies it (`SidebarPinChange::project`),
//! so what the shell drew while it waited is what is kept.

use std::cell::OnceCell;
use std::path::PathBuf;

use gpui::{AppContext as _, Context};
use slopty_view_proto::{SidebarPinChange, SidebarPreferencesState};
use slopty_view_rpc::RpcError;

use crate::workspace::WorkspaceView;

/// The pins, read from their file the first time they are asked of.
#[derive(Debug, Default)]
pub(in crate::workspace) struct Sidebar(OnceCell<SidebarPreferencesState>);

/// Where the pins are kept.
fn sidebar_path() -> PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("sidebar.json")
}

impl Sidebar {
    /// The pins, read from their file the first time.
    pub(in crate::workspace) fn state(&self) -> &SidebarPreferencesState {
        self.0.get_or_init(|| {
            let path = sidebar_path();
            let read = std::fs::read(&path).ok().and_then(|bytes| {
                serde_json::from_slice::<SidebarPreferencesState>(&bytes)
                    .inspect_err(
                        |e| tracing::warn!(path = %path.display(), %e, "the pins do not read"),
                    )
                    .ok()
            });
            SidebarPreferencesState { synced: true, initialized: true, ..read.unwrap_or_default() }
        })
    }

    /// Apply `change`; the state after it.
    fn apply(&mut self, change: &SidebarPinChange) -> SidebarPreferencesState {
        self.state();
        let Some(state) = self.0.get_mut() else { return SidebarPreferencesState::default() };
        change.project(&mut state.pinned_session_ids);
        state.revision = state.revision.saturating_add(1);
        state.clone()
    }
}

impl WorkspaceView {
    /// `changeSidebarPin`: a pin changed, kept and answered with the state after it;
    /// `None` for every other operation.
    pub(in crate::workspace) fn sidebar_mutate(
        &mut self,
        op: &str,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<Result<serde_json::Value, RpcError>> {
        if op != "changeSidebarPin" {
            return None;
        }
        let change: Option<SidebarPinChange> =
            params.get("change").cloned().and_then(|change| serde_json::from_value(change).ok());
        let Some(change) = change else {
            return Some(Err(RpcError::BadParams("a sidebar change".to_owned())));
        };
        let state = self.view_model.surfaces.sidebar.apply(&change);
        if let Some(feed) = &self.view_model.feed {
            feed.sidebar(state.clone());
        }
        let kept = state.clone();
        cx.background_spawn(async move {
            let path = sidebar_path();
            let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                let text = serde_json::to_vec(&kept).map_err(std::io::Error::other)?;
                std::fs::write(&path, text)
            });
            if let Err(e) = written {
                tracing::warn!(path = %path.display(), %e, "the pins were not kept");
            }
        })
        .detach();
        Some(Ok(serde_json::json!({ "sidebarPreferences": state })))
    }

    /// The pins as they stand.
    pub(in crate::workspace) fn sidebar_state(&self) -> SidebarPreferencesState {
        self.view_model.surfaces.sidebar.state().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_applied_as_the_shell_draws_it() {
        let mut sidebar = Sidebar(OnceCell::from(SidebarPreferencesState {
            synced: true,
            initialized: true,
            ..SidebarPreferencesState::default()
        }));
        let pin = |id: &str| SidebarPinChange::Pin {
            session_id: id.to_owned(),
            after: None,
            before: None,
        };
        sidebar.apply(&pin("a"));
        let state = sidebar.apply(&pin("b"));
        assert_eq!(state.pinned_session_ids, ["a", "b"]);
        assert_eq!(state.revision, 2);
        let state = sidebar.apply(&SidebarPinChange::Unpin { session_id: "a".to_owned() });
        assert_eq!(state.pinned_session_ids, ["b"]);
    }

    /// A file kept while the sidebar had sections of its own still reads, its pins kept.
    #[test]
    fn a_file_with_the_deleted_sections_still_reads() {
        let kept = serde_json::json!({
            "revision": 4,
            "synced": true,
            "initialized": true,
            "pinnedSessionIds": ["a"],
            "sections": [{"id": "s", "name": "Work", "session_ids": ["b"], "collapsed": false}],
        });
        let state: SidebarPreferencesState = serde_json::from_value(kept).expect("it reads");
        assert_eq!(state.pinned_session_ids, ["a"]);
    }
}
