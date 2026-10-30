// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! A session's explorer as the shell keeps it: the files opened lately, which explorer a file
//! event may steer, explorers let go with their sessions, and the explorer following the file
//! in front. Files itself is a view summoned beside the session (`views.rs`).

use super::*;

/// Files kept per chat for the command palette's Recent Files.
const RECENT_FILES_PER_CHAT: usize = 20;

impl Shell {
    /// Record `path` as the chat's most recently opened file.
    pub(super) fn note_recent_file(&mut self, chat_id: &str, path: &str) {
        if chat_id.is_empty() {
            return;
        }
        let recent = self.recent_files.entry(chat_id.to_owned()).or_default();
        recent.retain(|candidate| candidate != path);
        recent.push_front(path.to_owned());
        recent.truncate(RECENT_FILES_PER_CHAT);
    }

    /// The chat's opened files, most recent first.
    pub(super) fn recent_files(&self, chat_id: &str) -> impl Iterator<Item = &String> {
        self.recent_files.get(chat_id).into_iter().flatten()
    }

    /// Whether a file event from `source`, the explorer or editor of the session `owner`, may
    /// open or change files now: only while that session is the one shown.
    pub(super) fn accepts_file_navigation(
        &self,
        owner: &str,
        source: &Entity<FilesSurface>,
        cx: &App,
    ) -> bool {
        matches!(self.route, Route::Chat)
            && self.panel_key(cx) == owner
            && source.read(cx).is_current_target(cx)
    }

    /// Let go of the explorers of sessions that are gone.
    pub(super) fn prune_file_explorers(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if !state.chats_synced {
            return;
        }
        let live = state
            .chats
            .iter()
            .map(|chat| chat.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.files.retain(|key, _| live.contains(key.as_str()));
        self.files_subs.retain(|key, _| live.contains(key.as_str()));
    }

    /// The explorer shows the file in front: the one with the keyboard among the session's
    /// views.
    pub(super) fn sync_explorer_selection(&mut self, cx: &mut Context<Self>) {
        let Some(RightSurface::File(id)) = self.front_view(cx) else {
            return;
        };
        let Some(path) = self.file_surface_paths.get(&id).cloned() else {
            return;
        };
        if let Some(files) = self.files.get(&self.panel_key(cx)).cloned() {
            files.update(cx, |files, cx| files.reveal_file(path, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::panes::Pane;
    use super::*;
    use gpui::TestAppContext;

    /// Each session keeps its own explorer, made once; a file picked opens as a pane of its own
    /// beside the session, once however often it is picked; Files' key again hands the
    /// keyboard back to the terminal and leaves the view standing.
    #[gpui::test]
    fn files_and_a_file_open_as_panes_beside_their_own_session(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.summon_files(window, cx);
                assert!(shell.files.is_empty(), "the new-session canvas has no explorer");
                assert!(shell.panes.is_empty());

                shell.active_chat = "first".into();
                shell.summon_files(window, cx);
                let explorer = shell.files["first"].entity_id();
                assert_eq!(shell.panes_now(cx).panes(), vec![Pane::Session, Pane::View(RightSurface::Files)]);
                assert_eq!(shell.front_view(cx), Some(RightSurface::Files));

                shell.add_file_surface("src/main.rs".into(), window, cx);
                shell.add_file_surface("src/main.rs".into(), window, cx);
                assert_eq!(shell.file_surfaces.len(), 1);
                let file = RightSurface::File(shell.file_surface_seq);
                assert_eq!(
                    shell.panes_now(cx).views(),
                    vec![RightSurface::Files, file],
                    "the file stacks under Files at the session's right"
                );
                assert_eq!(shell.front_view(cx), Some(file));

                // Files' key: focused where it stands, then, focused, the keyboard home.
                shell.summon_files(window, cx);
                assert_eq!(shell.front_view(cx), Some(RightSurface::Files));
                shell.summon_files(window, cx);
                assert_eq!(shell.panes_now(cx).focused(), Pane::Session);
                assert!(shell.view_open(RightSurface::Files, cx), "the view stays");

                shell.active_chat = "second".into();
                assert!(!shell.view_open(RightSurface::Files, cx), "a session has views of its own");
                shell.summon_files(window, cx);
                assert_ne!(shell.files["second"].entity_id(), explorer);
                shell.active_chat = "first".into();
                assert!(shell.view_open(RightSurface::Files, cx));
                assert_eq!(shell.files["first"].entity_id(), explorer);

                shell.close_view(file, cx);
                assert!(shell.file_surfaces.is_empty());
                assert_eq!(shell.panes_now(cx).views(), vec![RightSurface::Files]);
            })
            .unwrap();
    }
}
