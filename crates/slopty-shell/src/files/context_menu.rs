// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! Tree menus share the editor's popup surface, positioning and dismissal.
use super::*;
use crate::{icons, popover, theme::Theme};
use gpui::{ClipboardItem, KeyDownEvent, MouseButton};
use mutations::WorkspaceInteractionOrigin;

pub(super) struct TreeContextMenu {
    /// Selection roots in visible order; one entry for a plain right-click.
    pub paths: Vec<String>,
    pub origin: WorkspaceInteractionOrigin,
    pub position: Point<Pixels>,
    pub active: usize,
}

fn absolute_workspace_path(root: &str, relative: &str) -> String {
    // Interpret the owning host's path, not the desktop's OS. PathBuf::join
    // would mix separators when viewing a Windows workspace from Linux/macOS.
    let bytes = root.as_bytes();
    let windows = root.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'));
    if windows {
        format!(
            "{}\\{}",
            root.trim_end_matches(['/', '\\']).replace('/', "\\"),
            relative.replace('/', "\\")
        )
    } else {
        format!("{}/{}", root.trim_end_matches('/'), relative)
    }
}

impl FilesSurface {
    pub(super) fn open_tree_context_menu(
        &mut self,
        path: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = self.interaction_origin(cx) else {
            return;
        };
        if self.tree.node(&path).is_none() {
            return;
        }
        // Like other explorers, a right-click inside the selection keeps it.
        if !self.tree.is_selected(&path) {
            self.tree.select(path.clone());
        }
        let paths = self.tree_action_paths();
        self.tree_focus.focus(window, cx);
        self.close_editor_context_menu(cx);
        self.tree_context_menu.open(TreeContextMenu {
            paths,
            origin,
            position,
            active: 0,
        });
        cx.notify();
    }

    /// What keyboard and menu actions apply to: the selection roots when the
    /// cursor is part of the selection, otherwise the cursor alone.
    pub(super) fn tree_action_paths(&self) -> Vec<String> {
        match self.tree.selected() {
            Some(cursor) if self.tree.is_selected(cursor) => self.tree.selection_roots(),
            Some(cursor) => vec![cursor.to_string()],
            None => Vec::new(),
        }
    }

    fn tree_menu_enabled(&self, paths: &[String], action: usize, cx: &gpui::App) -> bool {
        match action {
            0 => true,
            1 => paths.len() == 1 && self.can_mutate_tree_entry(&paths[0], false, cx),
            _ => paths
                .iter()
                .all(|path| self.can_mutate_tree_entry(path, true, cx)),
        }
    }

    pub(super) fn close_tree_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.tree_context_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |files: &mut Self| &mut files.tree_context_menu);
            cx.notify();
        }
    }

    pub(super) fn tree_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.tree_context_menu.is_open() {
            return false;
        }
        match popover::classify_key(
            &event.keystroke.key,
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            popover::MenuKey::Escape => self.close_tree_context_menu(cx),
            key @ (popover::MenuKey::Up | popover::MenuKey::Down) => {
                if let Some(menu) = self.tree_context_menu.as_open() {
                    let mut next = menu.active;
                    for _ in 0..TREE_MENU_ROWS {
                        next = popover::menu_step(
                            Some(next),
                            TREE_MENU_ROWS,
                            if matches!(key, popover::MenuKey::Up) {
                                -1
                            } else {
                                1
                            },
                        )
                        .unwrap_or(0);
                        if self.tree_menu_enabled(&menu.paths, next, cx) {
                            break;
                        }
                    }
                    // slopty: no unwrap.
                    if let Some(open) = self.tree_context_menu.open_mut() {
                        open.active = next;
                    }
                    cx.notify();
                }
            }
            popover::MenuKey::Enter => {
                if let Some(menu) = self.tree_context_menu.as_open() {
                    self.dispatch_tree_menu(menu.active, window, cx);
                }
            }
            _ => {}
        }
        window.prevent_default();
        cx.stop_propagation();
        true
    }

    fn dispatch_tree_menu(&mut self, action: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.tree_context_menu.as_open() else {
            return;
        };
        let paths = menu.paths.clone();
        let origin = menu.origin.clone();
        if !self.accepts_origin(&origin, cx) || !self.tree_menu_enabled(&paths, action, cx) {
            self.close_tree_context_menu(cx);
            return;
        }
        if paths.iter().any(|path| self.tree.node(path).is_none()) {
            self.close_tree_context_menu(cx);
            return;
        }
        self.close_tree_context_menu(cx);
        match action {
            0 => self.copy_full_paths(&paths, &origin.context.cwd, window, cx),
            // slopty: no index: a rename with no path does nothing.
            1 => {
                if let Some(path) = paths.first() {
                    self.begin_tree_rename(path.clone(), window, cx);
                }
            }
            2 => self.begin_tree_delete(paths, window, cx),
            _ => {}
        }
    }

    /// ⌥⌘C in the tree: the selection's full paths, as the menu's "Copy full path" copies them.
    pub(super) fn copy_tree_paths(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.tree_action_paths();
        let Some(origin) = self.interaction_origin(cx) else {
            return;
        };
        if paths.is_empty() || paths.iter().any(|path| self.tree.node(path).is_none()) {
            return;
        }
        self.copy_full_paths(&paths, &origin.context.cwd, window, cx);
    }

    // slopty: a copy says it happened: the menu closes, so a toast confirms (M5).
    fn copy_full_paths(
        &mut self,
        paths: &[String],
        root: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = paths
            .iter()
            .map(|path| absolute_workspace_path(root, path))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        crate::toast::success(cx, copied_paths_message(paths.len()));
        self.tree_focus.focus(window, cx);
    }

    pub(super) fn render_tree_context_menu(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.tree_context_menu.get()?;
        let theme = theme.for_popup();
        let count = menu.paths.len();
        // slopty: each row names its key, read from the keymap in the tree's context (M16, M41).
        let key_of = |action: &dyn gpui::Action| {
            crate::keys::for_action(action, Some(&self.tree_focus), window, cx)
        };
        let keys = [key_of(&super::CopyPath), None, key_of(&super::DeleteSelection)];
        let delete_label = if count > 1 {
            format!("Delete {count} items…")
        } else {
            "Delete…".into()
        };
        let mut card = popover::popover_card(&theme)
            .w(px(220.0))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_tree_context_menu(cx)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        // slopty: no "Add to prompt": the pane is read-only toward the agent.
        for (index, (label, icon_path)) in [
            (
                if count > 1 { "Copy full paths" } else { "Copy full path" }.to_string(),
                icons::COPY,
            ),
            ("Rename…".to_string(), icons::PEN),
            (delete_label, icons::TRASH_BIN_MINIMALISTIC),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 1 {
                card = card.child(popover::menu_separator());
            }
            let enabled = self.tree_menu_enabled(&menu.paths, index, cx);
            card = card.child(
                popover::menu_row(&theme, menu.active == index, format!("tree-menu-{index}"))
                    .id(gpui::SharedString::from(format!("tree-menu-{index}")))
                    .role(gpui::Role::MenuItem)
                    .aria_label(label.clone())
                    .when(index == 2, |row| row.text_color(theme.danger))
                    .when(!enabled, |el| {
                        el.opacity(crate::theme::DISABLED_OPACITY).cursor_default()
                    })
                    .when(enabled, |el| {
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            this.dispatch_tree_menu(index, window, cx)
                        }))
                    })
                    .child(
                        icons::icon(icon_path)
                            .size(px(16.0))
                            .text_color(if index == 2 {
                                theme.danger
                            } else {
                                theme.text_muted
                            }),
                    )
                    .child(div().flex_1().min_w_0().child(label))
                    .children(keys[index].as_ref().map(|keys| popover::kbd_hint(&theme, keys)))
                    .when(index == 1, |row| popover::fixed_key(row, &theme, "f2")),
            );
        }
        Some(popover::menu_at(
            "tree-context-menu",
            menu.position,
            card.into_any_element(),
            self.tree_context_menu.closing_at(crate::clock::now(cx)),
        ))
    }
}

/// The tree menu's rows: Copy full path, Rename…, Delete….
const TREE_MENU_ROWS: usize = 3;

/// What the toast says after a copy of `count` full paths.
fn copied_paths_message(count: usize) -> String {
    if count == 1 {
        "Copied full path".into()
    } else {
        format!("Copied {count} full paths")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[test]
    fn full_paths_follow_the_owning_hosts_path_format() {
        for (root, expected) in [
            ("/workspace", "/workspace/src/a.txt"),
            ("/workspace/", "/workspace/src/a.txt"),
            ("/", "/src/a.txt"),
            (r"C:\work", r"C:\work\src\a.txt"),
            (r"C:\", r"C:\src\a.txt"),
            ("C:/work/", r"C:\work\src\a.txt"),
            (r"\\server\share\work\", r"\\server\share\work\src\a.txt"),
            (r"\\?\C:\work", r"\\?\C:\work\src\a.txt"),
        ] {
            assert_eq!(absolute_workspace_path(root, "src/a.txt"), expected);
        }
    }

    #[gpui::test]
    fn copy_uses_the_remote_workspace_root(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        for (root, expected) in [
            ("/remote/project", "/remote/project/a.txt"),
            (r"D:\remote\project", r"D:\remote\project\a.txt"),
        ] {
            let window = cx.add_window(|_, cx| {
                let state = cx.new(|_| {
                    let mut state = super::super::test_support::state();
                    state.chats[0].device_id = "remote-host".into();
                    state.chats[0].cwd = Some(root.into());
                    state
                });
                super::super::test_support::explorer(state, cx)
            });
            window
                .update(cx, |files, window, cx| {
                    assert_eq!(
                        files
                            .request_context
                            .as_ref()
                            .unwrap()
                            .target_device_id
                            .as_deref(),
                        Some("remote-host")
                    );
                    files.open_tree_context_menu(
                        "a.txt".into(),
                        gpui::point(px(50.), px(50.)),
                        window,
                        cx,
                    );
                    files.dispatch_tree_menu(0, window, cx);
                    assert_eq!(
                        cx.read_from_clipboard().unwrap().text().as_deref(),
                        Some(expected)
                    );
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn menu_selects_without_opening_and_copies_full_path(cx: &mut TestAppContext) {
        let (files, cx) = super::super::test_support::setup(cx);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });
        files.update_in(cx, |files, window, cx| {
            files.open_tree_context_menu("a.txt".into(), gpui::point(px(50.), px(50.)), window, cx);
            assert_eq!(files.tree.selected(), Some("a.txt"));
            assert!(events.borrow().is_empty());
            files.dispatch_tree_menu(0, window, cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("/workspace/a.txt")
            );
            files.open_tree_context_menu(
                "folder".into(),
                gpui::point(px(50.), px(50.)),
                window,
                cx,
            );
            assert!(!files.tree.is_expanded("folder"));
        });
        cx.run_until_parked();
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, FilesEvent::OpenFile(_)))
        );
    }

    // slopty: the tree's Mac keys (⌥⌘C, ⌘⌫) act, the menu names them, and a copy says so.
    #[gpui::test]
    fn the_trees_keys_copy_and_delete_and_its_menu_names_them(cx: &mut TestAppContext) {
        use crate::settings::platform_combo;
        let (files, cx) = super::super::test_support::setup(cx);
        cx.update(|_, cx| super::super::bind_keys(cx));
        files.update_in(cx, |files, window, cx| {
            files.tree.select("a.txt");
            files.tree_focus.focus(window, cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes(&platform_combo("alt-mod-c"));
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard()).and_then(|item| item.text()).as_deref(),
            Some("/workspace/a.txt")
        );
        assert!(
            cx.update(|_, cx| crate::toast::messages(cx))
                .iter()
                .any(|message| message.as_ref() == "Copied full path")
        );
        files.update_in(cx, |files, window, cx| {
            files.open_tree_context_menu("a.txt".into(), gpui::point(px(50.), px(50.)), window, cx);
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let focus = files.read(cx).tree_focus.clone();
            assert_eq!(
                crate::keys::for_action(&super::super::CopyPath, Some(&focus), window, cx),
                Some(crate::keys::combo_text("alt-mod-c"))
            );
            assert_eq!(
                crate::keys::for_action(&super::super::DeleteSelection, Some(&focus), window, cx),
                Some(crate::keys::combo_text("mod-backspace"))
            );
        });
        files.update_in(cx, |files, _, cx| files.close_tree_context_menu(cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes(&platform_combo("mod-backspace"));
        files.read_with(cx, |files, _| {
            assert_eq!(files.tree_delete.as_ref().map(|d| d.paths.clone()), Some(vec!["a.txt".to_owned()]));
        });
    }

    #[test]
    fn a_copy_of_paths_says_how_many() {
        assert_eq!(copied_paths_message(1), "Copied full path");
        assert_eq!(copied_paths_message(3), "Copied 3 full paths");
    }

    #[gpui::test]
    fn menu_inside_a_selection_acts_on_every_root(cx: &mut TestAppContext) {
        let (files, cx) = super::super::test_support::setup(cx);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });
        let at = gpui::point(px(50.), px(50.));
        files.update_in(cx, |files, window, cx| {
            files.tree.select("folder");
            files.tree.toggle_selected("a.txt");
            files.open_tree_context_menu("a.txt".into(), at, window, cx);
            let paths = files.tree_context_menu.as_open().unwrap().paths.clone();
            assert_eq!(paths, ["folder", "a.txt"]);
            assert!(!files.tree_menu_enabled(&paths, 1, cx));
            assert!(files.tree_menu_enabled(&paths, 2, cx));
            files.dispatch_tree_menu(0, window, cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("/workspace/folder\n/workspace/a.txt")
            );
            files.open_tree_context_menu("a.txt".into(), at, window, cx);
            files.dispatch_tree_menu(1, window, cx);
            assert!(files.tree_rename.is_none());
            files.open_tree_context_menu("folder".into(), at, window, cx);
            files.dispatch_tree_menu(2, window, cx);
            assert_eq!(
                files.tree_delete.as_ref().unwrap().paths,
                ["folder", "a.txt"]
            );
            files.dismiss_tree_delete(true, window, cx);
        });
        cx.run_until_parked();
        assert!(events.borrow().iter().any(|event| matches!(
            event,
            FilesEvent::MutateMany(intents)
                if intents.iter().all(|intent| intent.destination.is_none())
                    && intents.len() == 2
        )));

        // Right-clicking outside the selection narrows it to that entry.
        files.update_in(cx, |files, window, cx| {
            files.tree.select("folder");
            files.open_tree_context_menu("a.txt".into(), at, window, cx);
            assert_eq!(files.tree_context_menu.as_open().unwrap().paths, ["a.txt"]);
            assert_eq!(files.tree.selected_paths(), ["a.txt"]);
        });
    }
}
