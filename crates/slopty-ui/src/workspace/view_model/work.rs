//! slopty: what a chat's thread says beside its transcript: its tray of background work and its
//! dock ([`slopty_view_doc::calls`]), read-only. The agent's TUI does everything that acts on
//! them (`docs/decisions/agents.md`, "TUI-first").

use slopty_client::WorkerKey;
use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{ThreadId, ThreadState};

use crate::view_model::work::{Child, dock, tray};
use crate::view_model::{ChatViews, rows};
use crate::workspace::WorkspaceView;

impl WorkspaceView {
    /// The thread `chat` shows, its worker, and its state as held here.
    fn view_chat_state<'a>(
        &self,
        chat: &str,
        cx: &'a gpui::App,
    ) -> Option<(WorkerKey, ThreadId, &'a ThreadState)> {
        let (key, thread) = self.chat_thread(chat, cx)?;
        let state = self.held_hub(key)?.read(cx).threads().mirror(thread)?.state()?;
        Some((key, thread, state))
    }

    /// The rows of the chats an agent started under `thread`, on any machine.
    fn view_children<'a>(
        &self,
        thread: ThreadId,
        cx: &'a gpui::App,
    ) -> Vec<(WorkerKey, &'a ThreadRow)> {
        self.hubs()
            .flat_map(|(key, hub)| {
                let table = &hub.read(cx).threads().rows().rows;
                table
                    .values()
                    .filter(|row| rows::parent(row) == Some(thread))
                    .map(move |r| (key, r))
            })
            .collect()
    }

    /// What `chat`'s thread says beside its transcript, once its state is here.
    pub(super) fn view_chat_views(&self, chat: &str, cx: &gpui::App) -> Option<ChatViews> {
        let (_, thread, state) = self.view_chat_state(chat, cx)?;
        let children = self.view_children(thread, cx);
        let children: Vec<Child<'_>> = children.iter().map(|&(_, row)| Child { row }).collect();
        let work = tray(state, &children, &|thread| thread.to_string());
        let dock = dock(state);
        Some(ChatViews { work, dock })
    }
}

#[cfg(test)]
mod tests;
