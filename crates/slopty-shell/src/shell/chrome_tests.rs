// slopty: Slopty's own tests (not in zeron).
//! slopty: the app's chrome as a person and assistive tech meet it: the palettes, the chat menu
//! and the empty states (the sidebar's own are `sidebar`'s). Each test draws the whole shell in a
//! headless window (`retention_tests::launch`) and judges by its accessibility tree
//! (`retention_tests::tree`) and its laid-out bounds.

use gpui::accesskit::Role;
use gpui::{TestAppContext, VisualTestContext};

use super::retention_tests::{DESKTOP, device, draw, find, launch, space, tree};
use super::*;

/// slopty: a title the agent gives a chat rises in over the old on its sidebar row and in the
/// titlebar (Amp's), on the house's roll; under Reduce Motion it is simply there.
#[gpui::test]
fn a_chat_title_the_agent_gives_rolls_in(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
    draw(cx);
    let retitle = |title: &'static str, cx: &mut VisualTestContext| {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats.iter_mut().find(|chat| chat.id == "b").unwrap().title = Some(title.into());
            });
            cx.notify();
        });
        draw(cx);
    };
    let rolls = |key: &'static str, title: &'static str, cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            crate::roll_text::rolling(key, title.into(), false, crate::clock::now(cx)).is_some()
        })
    };
    cx.update(|_, cx| crate::motion::set_reduced_motion(cx, false));
    retitle("Fix the lexer", cx);
    assert!(rolls("sidebar-title-b", "Fix the lexer", cx), "the row's title rolls in");
    assert!(rolls("titlebar-title-b", "Fix the lexer", cx), "the titlebar's too");
    assert!(cx.debug_bounds("chat-titlebar-title").is_some(), "in the title's place");
    cx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    retitle("Parse the lexer", cx);
    assert!(!rolls("sidebar-title-b", "Parse the lexer", cx), "under Reduce Motion, still");
    assert!(!rolls("titlebar-title-b", "Parse the lexer", cx));
}

/// A selected row under the pointer goes a step past its selection, never back toward the
/// plain hover, in both appearances.
#[test]
fn a_selected_row_under_the_pointer_goes_past_its_selection() {
    let _appearance = crate::theme::lock_appearance();
    for appearance in [crate::theme::Appearance::Light, crate::theme::Appearance::Dark] {
        crate::theme::set_current_appearance(appearance);
        let (selected, hovered) = (crate::theme::glass_selected_bg(), crate::theme::selected_hover_wash());
        assert!(hovered.a > selected.a, "darker than selected: {selected:?} {hovered:?}");
        assert_eq!((hovered.h, hovered.s, hovered.l), (selected.h, selected.s, selected.l), "the same ink");
    }
}

/// Before any project, the sidebar says so and offers no key that cannot make a chat, and the
/// filter, with nothing to filter, is gone; the canvas's one action is a named button.
#[gpui::test]
fn before_any_project_the_sidebar_says_so_and_offers_only_what_works(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.chats.clear();
            state.devices = vec![device()];
        });
    });
    let nodes = tree(cx);
    assert!(find(&nodes, Role::Label, "No projects yet").is_some(), "the empty line says what fills it first");
    assert!(cx.debug_bounds("sidebar-empty-key").is_none(), "no ⌘N: it makes no chat yet");
    assert!(
        !nodes.values().any(|node| node.label.starts_with("Filter projects")),
        "no filter with nothing to filter"
    );
    assert!(find(&nodes, Role::Heading, "Add a project to get started").is_some(), "the canvas's headline");
    assert!(find(&nodes, Role::Button, "Add a project").is_some(), "its one action, named");
    // A project, and the list says what fills it now, with its key.
    shell.update(cx, |shell, cx| shell.state.update(cx, |state, _| state.spaces = vec![space("app")]));
    let nodes = tree(cx);
    assert!(find(&nodes, Role::Label, "No sessions yet").is_some());
    assert!(cx.debug_bounds("sidebar-empty-key").is_some(), "⌘N makes one now");
}

/// The command palette stands pinned by its top, Ghostty's 5 % down, and between 280 and 480
/// tall, wherever its results leave its foot.
#[gpui::test]
fn the_palette_keeps_its_top_as_its_results_change(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.toggle_command_palette(window, cx)));
    draw(cx);
    let full = cx.debug_bounds("command-palette").expect("the palette");
    cx.simulate_input("zzzz-no-such-thing");
    draw(cx);
    let none = cx.debug_bounds("command-palette").expect("the palette, still");
    assert_eq!(full.top(), none.top(), "the top stays: {full:?} {none:?}");
    assert!(none.size.height <= full.size.height, "only the foot moves: {full:?} {none:?}");
    for card in [full, none] {
        let height = f32::from(card.size.height);
        assert!((279.5..=480.5).contains(&height), "between 280 and 480: {card:?}");
    }
    // Ghostty's: 5 % down the terminal under the title row, at most 500 wide.
    let window = DESKTOP.size.1;
    let top = (window - Theme::TITLEBAR_HEIGHT).mul_add(0.05, Theme::TITLEBAR_HEIGHT);
    assert!((f32::from(full.top()) - top).abs() < 1.0, "5 % down, under the row: {full:?}");
    assert!((f32::from(full.size.width) - 500.0).abs() < 0.5, "Ghostty's 500 wide: {full:?}");
}

/// Every row of a chat's menu is a menu item a screen reader names, Copy saying whether its
/// submenu is open; none deletes the session, whose record is its agent's own.
#[gpui::test]
fn the_chat_menu_names_its_rows(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| {
        shell.chat_menu.open(ChatMenuState {
            tab: None,
            chat_id: "b".into(),
            position: gpui::point(px(40.0), px(120.0)),
            page: ChatMenuPage::Root,
        });
        cx.notify();
    });
    let nodes = tree(cx);
    for label in ["Rename", "Archive"] {
        assert!(find(&nodes, Role::MenuItem, label).is_some(), "{label} is a named menu item");
    }
    assert!(find(&nodes, Role::MenuItem, "Delete\u{2026}").is_none(), "no Delete");
    assert!(find(&nodes, Role::MenuItem, "Pin").is_none(), "no Pin: the list keeps its order");
    let copy = find(&nodes, Role::MenuItem, "Copy").expect("Copy");
    assert_eq!(copy.expanded, Some(false), "its submenu shut: {copy:?}");
    assert!(nodes.values().all(|node| node.role != Role::MenuItem || !node.label.is_empty()), "no row unnamed");
}

/// Escape puts away whichever context menu is up, a chat's, a project's or the account's, as a
/// system menu does.
#[gpui::test]
fn escape_puts_a_context_menu_away(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    let at = gpui::point(px(40.0), px(120.0));
    let open: [fn(&mut Shell, Point<Pixels>); 3] = [
        |shell, at| {
            shell.chat_menu.open(ChatMenuState {
                tab: None,
                chat_id: "b".into(),
                position: at,
                page: ChatMenuPage::Root,
            });
        },
        |shell, at| shell.space_menu.open(("p".into(), at)),
        |shell, _| shell.user_menu.open(()),
    ];
    for (which, open) in open.into_iter().enumerate() {
        shell.update(cx, |shell, cx| {
            open(shell, at);
            cx.notify();
        });
        draw(cx);
        cx.simulate_keystrokes("escape");
        draw(cx);
        let up = shell.read_with(cx, |shell, _| {
            [shell.chat_menu.is_open(), shell.space_menu.is_open(), shell.user_menu.is_open()]
        });
        assert_eq!(up, [false; 3], "menu {which} put away");
    }
}

/// Back and Forward, with no history to walk, say they are off as well as look it.
#[gpui::test]
fn back_and_forward_say_when_they_are_off(cx: &mut TestAppContext) {
    let (_shell, cx) = launch(cx, DESKTOP);
    let nodes = tree(cx);
    let forward = find(&nodes, Role::Button, "Forward").expect("Forward");
    assert!(forward.disabled, "nothing ahead: {forward:?}");
}

/// The titlebar's title opens its chat's menu on a right-click, as the chat's sidebar row does,
/// under the bar so the title stays in sight.
#[gpui::test]
fn the_title_opens_its_chats_menu(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
    draw(cx);
    let title = cx.debug_bounds("chat-titlebar-title").expect("the title");
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Right,
        position: title.center(),
        click_count: 1,
        ..Default::default()
    });
    cx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Right,
        position: title.center(),
        click_count: 1,
        ..Default::default()
    });
    draw(cx);
    let (chat, top) = shell
        .read_with(cx, |shell, _| {
            shell.chat_menu.as_open().map(|menu| (menu.chat_id.clone(), menu.position.y))
        })
        .expect("the chat menu");
    assert_eq!(chat, "b", "the open chat's menu");
    assert!(top >= title.bottom(), "under the title: {top:?} {title:?}");
    assert!(find(&tree(cx), Role::MenuItem, "Rename").is_some(), "drawn");
}

/// The app's first frame rises in as zeron's does; each move between Settings and the chats
/// fades the new page in afresh, and moving between Settings' own pages does not.
#[gpui::test]
fn each_move_between_settings_and_the_chats_fades_the_page_in(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    let changes = |cx: &mut VisualTestContext| {
        draw(cx);
        shell.read_with(cx, |shell, _| shell.route_shown.map(|(_, changes)| changes))
    };
    assert_eq!(changes(cx), Some(0), "the first frame");
    shell.update(cx, |shell, cx| shell.open_settings(SettingsSection::Appearance, cx));
    assert_eq!(changes(cx), Some(1), "into Settings");
    shell.update(cx, |shell, cx| shell.open_settings(SettingsSection::Shortcuts, cx));
    assert_eq!(changes(cx), Some(1), "a page within it is no move");
    shell.update(cx, |shell, cx| shell.close_settings(cx));
    assert_eq!(changes(cx), Some(2), "back to the chats");
}

/// The inspector and the chat menu, drawn over a shell holding three local chats.
mod session_card {
    use super::super::*;

    fn test_shell(cx: &mut gpui::TestAppContext, path: &std::path::Path) -> gpui::WindowHandle<Shell> {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: path.into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        })
    }

    fn chat(id: &str) -> slopty_view_proto::Chat {
        serde_json::from_value(serde_json::json!({"id":id,"title":id,"deviceId":"local","archived":false,"createdAt":"2026-09-20T00:00:00Z"})).unwrap()
    }

    fn prepare(shell: &mut Shell, cx: &mut Context<Shell>) {
        shell.state.update(cx, |state, _| {
            state.workspace_scope = Some(WorkspaceScope::Local);
            state.local_device_id = Some("local".into());
            state.chats_synced = true;
            state.chats = vec![chat("first"), chat("regular"), chat("other")];
        });
        shell.settings.space_filter = None;
    }

    /// A session opens on its terminal alone, the side pane shut; ⌘I opens its inspector
    /// beside it, its tab first in the pane, and closes it again, the keyboard back with the
    /// terminal. A plain terminal has nothing to inspect.
    #[gpui::test]
    fn a_session_opens_on_its_terminal_and_cmd_i_opens_its_inspector(cx: &mut gpui::TestAppContext) {
        use super::super::RightSurface;
        let (shell, cx) = super::super::retention_tests::launch(cx, super::super::retention_tests::DESKTOP);
        let show = |chat: &str, cx: &mut gpui::VisualTestContext| {
            shell.update(cx, |shell, cx| shell.open_chat(chat.into(), cx));
            cx.run_until_parked();
        };
        let shown = |chat: &str, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let (tabs, open, pane) = shell.read_with(cx, |shell, cx| {
                let tabs = shell.right_tabs.get(chat).cloned().unwrap_or_default();
                (tabs, shell.inspector_shown(cx), shell.right_pane_open(cx))
            });
            assert_eq!(open, cx.debug_bounds("inspector").is_some(), "drawn as it stands");
            assert_eq!(open, pane, "the pane open only on the inspector");
            (tabs, open)
        };
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_session_terminals([("a".to_owned(), "t-a".to_owned())].into());
                cx.notify();
            });
        });
        show("a", cx);
        assert_eq!(shown("a", cx), (Vec::new(), false), "the terminal alone");
        cx.simulate_keystrokes("cmd-i");
        cx.run_until_parked();
        assert_eq!(shown("a", cx), (vec![RightSurface::Inspector], true), "⌘I opens it");
        cx.simulate_keystrokes("cmd-i");
        cx.run_until_parked();
        assert_eq!(shown("a", cx), (vec![RightSurface::Inspector], false), "and closes it");
        let terminal = cx.update(|window, cx| {
            shell.read(cx).session_terminal.read(cx).focus_handle().contains_focused(window, cx)
        });
        assert!(terminal, "the terminal has the keyboard back");

        let terminal = format!("{}t", crate::inspector::TERMINAL_CHAT);
        shell.update(cx, |shell, cx| {
            let row = super::super::retention_tests::chat_row(&terminal, 3, None, chrono::Utc::now());
            shell.state.update(cx, |state, cx| {
                state.chats.push(row);
                cx.notify();
            });
        });
        show(&terminal, cx);
        cx.simulate_keystrokes("cmd-i");
        cx.run_until_parked();
        assert_eq!(shown(&terminal, cx), (Vec::new(), false), "a plain terminal has nothing to inspect");
    }

    /// The inspector does not repeat its session's title, which the terminal's title bar
    /// and the sidebar row say: a session at work has its status and time on one line over its
    /// timeline, and an idle one has no header at all.
    #[gpui::test]
    fn the_inspector_does_not_repeat_its_sessions_title(cx: &mut gpui::TestAppContext) {
        use super::super::retention_tests::{DESKTOP, chat_row, launch};
        let (shell, cx) = launch(cx, DESKTOP);
        let now = chrono::Utc::now();
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.chats.push(chat_row("c", 3, None, now)));
        });
        let inspect = |chat: &str, cx: &mut gpui::VisualTestContext| {
            shell.update(cx, |shell, cx| shell.open_chat(chat.into(), cx));
            cx.run_until_parked();
            cx.simulate_keystrokes("cmd-i");
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert!(shell.read_with(cx, |shell, cx| shell.inspector_shown(cx)), "{chat}'s inspector is open");
            assert!(cx.debug_bounds("inspector-timeline").is_some(), "its timeline is drawn");
            assert!(cx.debug_bounds("inspector-title").is_none(), "{chat}'s title is not said again");
            cx.debug_bounds("inspector-header").is_some()
        };
        assert!(inspect("a", cx), "a session at work says so over its timeline");
        assert!(!inspect("c", cx), "an idle one has nothing to say over it");
    }

    /// slopty: a toast never stands over the session's terminal, whose TUI keeps its prompt in
    /// the window's bottom-right corner: it rises in the sidebar's foot, above its bottom row,
    /// and with the sidebar put away it hangs at the top right under the title strip.
    #[gpui::test]
    fn a_toast_keeps_off_the_terminal(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = test_shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                prepare(shell, cx);
                shell.debug_gate = Some(GatePhase::Ready);
                cx.notify();
            })
            .unwrap();
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(gpui::size(px(1280.0), px(800.0)));
        cx.update(|_, cx| crate::toast::info(cx, "Link copied"));
        let settle = |cx: &mut gpui::VisualTestContext| {
            cx.executor().advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        settle(&mut cx);
        let toaster = cx.debug_bounds("toaster").expect("the toast shows");
        let sidebar = cx.debug_bounds("sidebar-column").expect("the sidebar");
        let foot = cx.debug_bounds("sidebar-footer-row").expect("its bottom row");
        assert!(toaster.right() <= sidebar.right(), "inside the sidebar: {toaster:?} in {sidebar:?}");
        assert!(toaster.left() >= sidebar.left(), "{toaster:?} in {sidebar:?}");
        assert!(toaster.bottom() <= foot.top(), "above its bottom row: {toaster:?} over {foot:?}");

        window.update(&mut cx, |shell, _, cx| shell.toggle_sidebar(cx)).unwrap();
        settle(&mut cx);
        let toaster = cx.debug_bounds("toaster").expect("the toast still shows");
        let top = px(Theme::TITLEBAR_HEIGHT);
        assert!(toaster.top() >= top && toaster.top() <= top + px(Theme::SPACE_MD), "{toaster:?}");
        assert!(px(1280.0) - toaster.right() <= px(Theme::SPACE_MD), "at the right: {toaster:?}");
    }

    #[gpui::test]
    fn copying_from_the_copy_submenu_shows_a_toast(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let window = test_shell(cx, dir.path());
        window
            .update(cx, |shell, _, cx| {
                prepare(shell, cx);
                shell.debug_gate = Some(GatePhase::Ready);
                shell.chat_menu.open(ChatMenuState {
                    chat_id: "regular".into(),
                    tab: None,
                    position: gpui::point(px(40.0), px(120.0)),
                    page: ChatMenuPage::Copy,
                });
                cx.notify();
            })
            .unwrap();
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        // The item sits in the submenu, outside the menu card: pressing it
        // must copy rather than dismiss the menu first.
        let item = cx
            .debug_bounds("chat-copy-slopty")
            .expect("the Copy submenu is open");
        cx.simulate_click(item.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(crate::toast::messages(cx).len(), 1, "one toast");
        });
        assert!(cx.debug_bounds("toaster").is_some(), "the shell paints it");
    }

}
