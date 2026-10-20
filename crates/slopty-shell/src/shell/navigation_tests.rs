// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! Real pane rendering and key dispatch without booting an engine.
use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext};

struct NavigationHost {
    shell: Entity<Shell>,
    _data_dir: tempfile::TempDir,
}

impl Render for NavigationHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell.update(cx, |shell, cx| {
            shell
                .navigation_focus
                .remember(&shell.shortcut_focus, window, cx);
            let (preferred, neutral) = shell.navigation_focus_fallback(cx);
            defer_mounted_focus_restore(
                shell.shortcut_focus.clone(),
                preferred,
                neutral,
                window,
                cx,
            );
            let main = shell.render_main(window, 400., 400., cx);
            let right = shell.render_right_pane(window, cx);
            div()
                .size_full()
                .flex()
                .flex_col()
                .track_focus(&shell.shortcut_focus)
                .child(div().track_focus(&shell.unfocused))
                .on_action(cx.listener(|shell, _: &NextSession, window, cx| {
                    shell.cycle_navigation(true, window, cx);
                }))
                .on_action(cx.listener(|shell, _: &PrevSession, window, cx| {
                    shell.cycle_navigation(false, window, cx);
                }))
                // slopty: the window's own keys (`chrome_keys`).
                .map(|el| shell.chrome_actions(el, cx))
                .child(div().h(px(40.)).child(shell.render_right_tab_strip(cx)))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .id("navigation-main")
                                .debug_selector(|| "navigation-main".into())
                                .w(px(400.))
                                .h_full()
                                .child(main),
                        )
                        .when(shell.right_pane_open(cx), |el| el.child(right)),
                )
        })
    }
}

fn setup(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(UiSettings::default(), dir.path(), cx);
        // slopty: no History surface (ruled out).
        apply_keymap(
            cx,
            &KeymapConfig::default(),
            ComposerSendBehavior::default(),
        );
    });
    let (host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    // slopty: Slopty's server is the only backend.
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            );
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.no_project = true;
                state.chats = ["parent", "other"]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, id)| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "title": id, "deviceId": "local", "archived": false,
                            "createdAt": Utc::now() - chrono::Duration::minutes(ix as i64),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.selected_chat = Some("parent".into());
            });
            shell.active_chat = "parent".into();
            crate::motion::set_reduced_motion(cx, true);
            shell
                .composer
                .update(cx, |composer, _| composer.focus_pending = false);
            shell
        });
        NavigationHost {
            shell,
            _data_dir: dir,
        }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    // slopty: two diff tabs stand for any two surfaces (no subagent tab), opened once the
    // selection has settled the panes' owner.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_surfaces_open(true, cx);
            for _ in 0..2 {
                shell.add_diff_surface(window, cx);
            }
        });
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    (shell, cx)
}

fn press(cx: &mut VisualTestContext, forward: bool) {
    let id = if forward {
        ShortcutId::NextSession
    } else {
        ShortcutId::PrevSession
    };
    cx.simulate_keystrokes(&platform_combo(id.default_combo()));
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn assert_surface(shell: &Entity<Shell>, cx: &mut VisualTestContext, surface: RightSurface) {
    cx.update(|window, cx| {
        let shell = shell.read(cx);
        assert_eq!(shell.resolved_right_active(cx), surface);
        assert_eq!(
            shell.state.read(cx).selected_chat.as_deref(),
            Some("parent")
        );
        assert!(shell.navigation_focus.in_right(window, cx));
    });
}

#[gpui::test]
fn repeated_keys_stay_in_the_right_pane_and_follow_reordering(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(tab, gpui::Modifiers::default());
    for expected in [2, 1, 2, 1] {
        press(cx, true);
        assert_surface(&shell, cx, RightSurface::Diff(expected));
    }
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.add_diff_surface(window, cx);
            shell.reorder_right_tabs(2, 0, cx);
        })
    });
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(1));
    press(cx, false);
    assert_surface(&shell, cx, RightSurface::Diff(3));
    press(cx, false);
    assert_surface(&shell, cx, RightSurface::Diff(2));
}

#[gpui::test]
fn clicking_main_transcript_switches_back_to_session_navigation(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(tab, gpui::Modifiers::default());
    press(cx, true);
    let main = cx.debug_bounds("navigation-main").unwrap();
    cx.simulate_click(
        main.origin + gpui::point(px(100.), px(150.)),
        gpui::Modifiers::default(),
    );
    press(cx, true);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("other"));
    });
}

#[gpui::test]
fn side_chat_to_diff_retains_right_focus_and_custom_bindings(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let chat = serde_json::from_value(serde_json::json!({
                "id": "side", "parentChatId": "parent", "deviceId": "local",
                "archived": false, "createdAt": Utc::now(),
            }))
            .unwrap();
            shell.open_side_chat(chat, shell.panel_key(cx), cx);
            shell.add_diff_surface(window, cx);
            shell.activate_right_surface(RightSurface::SideChat(1), window, cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        assert!(
            shell.read(cx).side_chats[&1]
                .composer
                .focus_handle(cx)
                .is_focused(window)
        );
        let mut keymap = KeymapConfig::default();
        keymap.next_session = "alt-down".into();
        keymap.prev_session = "alt-up".into();
        apply_keymap(cx, &keymap, ComposerSendBehavior::default());
    });
    cx.simulate_keystrokes("alt-down");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_surface(&shell, cx, RightSurface::Diff(3));
    cx.simulate_keystrokes("alt-down");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_surface(&shell, cx, RightSurface::Diff(1));
    cx.simulate_keystrokes("alt-up");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_surface(&shell, cx, RightSurface::Diff(3));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_right_surface(RightSurface::SideChat(1), window, cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.close_right_surface(RightSurface::SideChat(1), window, cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    // Closing returns to the most recently visited tab, then cycles in strip order.
    assert_surface(&shell, cx, RightSurface::Diff(3));
    cx.simulate_keystrokes("alt-down");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_surface(&shell, cx, RightSurface::Diff(1));
}

#[gpui::test]
fn closing_active_and_last_tabs_recovers_the_correct_pane(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(tab, gpui::Modifiers::default());
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.close_right_surface(RightSurface::Diff(1), window, cx);
        })
    });
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(2));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.close_right_surface(RightSurface::Diff(2), window, cx);
            assert!(!shell.right_pane_open(cx));
            assert!(shell.composer.focus_handle(cx).is_focused(window));
        })
    });
    press(cx, true);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("other"));
    });
}

#[gpui::test]
fn modal_and_picker_block_navigation_in_both_panes(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    for right in [false, true] {
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                let focus = if right {
                    &shell.navigation_focus.right
                } else {
                    &shell.navigation_focus.main
                };
                window.focus(focus, cx);
                shell.delete_confirm = Some("parent".into());
            })
        });
        press(cx, true);
        press(cx, false);
        shell.update(cx, |shell, cx| {
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("parent")
            );
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Diff(2));
            shell.delete_confirm = None;
        });
    }
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell
                .composer
                .update(cx, |composer, cx| composer.open_model_menu(window, cx));
        })
    });
    press(cx, true);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(
            shell.state.read(cx).selected_chat.as_deref(),
            Some("parent")
        );
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Diff(2));
    });
}

#[gpui::test]
fn empty_picker_and_stale_tabs_do_not_change_sessions(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let key = shell.panel_key(cx);
            shell.right_tabs.insert(key, vec![RightSurface::File(999)]);
            shell.set_right_active(RightSurface::Picker, cx);
            window.focus(&shell.navigation_focus.right, cx);
        })
    });
    press(cx, true);
    press(cx, false);
    assert_surface(&shell, cx, RightSurface::Picker);
}

#[gpui::test]
fn hiding_reopening_and_expanding_preserve_navigation_boundaries(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(tab, gpui::Modifiers::default());
    shell.update(cx, |shell, cx| shell.toggle_right_pane_expand(cx));
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(2));
    shell.update(cx, |shell, cx| shell.set_surfaces_open(false, cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        assert!(!shell.read(cx).navigation_focus.in_right(window, cx));
        assert!(shell.read(cx).composer.focus_handle(cx).is_focused(window));
    });
    shell.update(cx, |shell, cx| shell.set_surfaces_open(true, cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        assert!(shell.read(cx).composer.focus_handle(cx).is_focused(window));
    });
    // Restored tabs do not take navigation away from the main composer.
    press(cx, true);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("other"));
    });
}

#[gpui::test]
fn embedded_terminal_and_browser_focus_route_to_right_tabs(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        // No engine in this harness: reserve a real emulator tab without
        // starting a PTY, then use the normal surface activation path.
        shell.right_terminal_panel(cx).update(cx, |panel, cx| {
            panel.reserve_tab_for_chat("parent".into(), "Test terminal", cx);
        });
        shell.add_terminal_surface(cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        let terminal = shell.read(cx).right_terminal.as_ref().unwrap().read(cx);
        assert!(terminal.focus_handle().is_focused(window));
    });
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(1));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.add_browser_surface(None, window, cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(1));
    // Exercise the GPUI target the native browser bridge focuses before it
    // redispatches a shortcut (the native OS event itself needs platform QA).
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_right_active(RightSurface::Browser(1), cx);
            window.focus(&shell.browsers[&1].focus_handle(cx), cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(1));
}

#[gpui::test]
fn bottom_terminal_keeps_session_navigation(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.terminal_panel(cx).update(cx, |panel, cx| {
                panel.reserve_tab_for_chat("parent".into(), "Test terminal", cx);
            });
            shell.toggle_terminal(window, cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        assert!(
            shell
                .read(cx)
                .terminal
                .as_ref()
                .unwrap()
                .read(cx)
                .focus_handle()
                .is_focused(window)
        );
    });
    press(cx, true);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("other"));
    });
}

#[gpui::test]
fn each_session_cycles_only_its_own_tabs(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| shell.open_chat("other".into(), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_surfaces_open(true, cx);
            shell.add_diff_surface(window, cx);
            shell.activate_right_surface(RightSurface::Diff(3), window, cx);
        })
    });
    press(cx, true);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("other"));
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Diff(3));
    });
    shell.update(cx, |shell, cx| shell.open_chat("parent".into(), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(tab, gpui::Modifiers::default());
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(2));
    press(cx, true);
    assert_surface(&shell, cx, RightSurface::Diff(1));
}

/// Settles a transcript selection and seeds the clipboard with a sentinel, so
/// a keystroke that copies nothing is distinguishable from one that does.
fn select_transcript_text(cx: &mut VisualTestContext) {
    crate::markdown::selection::begin_with_span("transcript-row:0", "selected reply", 0..8);
    crate::markdown::selection::end_active_drag();
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
}

fn clipboard_text(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn clear_transcript_selection() {
    crate::markdown::selection::clear_if_owner("transcript-row:0");
}

#[gpui::test]
fn copy_shortcut_copies_transcript_selection_after_clicking_the_transcript(
    cx: &mut TestAppContext,
) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| window.focus(&shell.read(cx).composer.focus_handle(cx), cx));
    let main = cx.debug_bounds("navigation-main").unwrap();
    cx.simulate_click(
        main.origin + gpui::point(px(100.), px(150.)),
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        let shell = shell.read(cx);
        assert!(!shell.composer.focus_handle(cx).is_focused(window));
        assert!(shell.navigation_focus.main.is_focused(window));
    });
    select_transcript_text(cx);
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("selected"));
    clear_transcript_selection();
}

#[gpui::test]
fn copy_shortcut_copies_side_chat_selection_from_the_right_pane(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        let right = shell.read(cx).navigation_focus.right.clone();
        window.focus(&right, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    select_transcript_text(cx);
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("selected"));
    clear_transcript_selection();
}

#[gpui::test]
fn copy_shortcut_prefers_the_focused_composer_selection(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| window.focus(&shell.read(cx).composer.focus_handle(cx), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_input("draft text");
    select_transcript_text(cx);
    cx.simulate_keystrokes("ctrl-a ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("draft text"));
    clear_transcript_selection();
}

#[gpui::test]
fn ctrl_c_in_a_terminal_is_not_taken_by_a_transcript_selection(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.terminal_panel(cx).update(cx, |panel, cx| {
                panel.reserve_tab_for_chat("parent".into(), "Test terminal", cx);
            });
            shell.toggle_terminal(window, cx);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    select_transcript_text(cx);
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("sentinel"));
    clear_transcript_selection();
}

// slopty: no pull-request pages.
// slopty: ⌘⇧T opens the tab closed last again, in front, with the pane open.
#[gpui::test]
fn a_closed_tab_comes_back_with_its_key(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.add_diff_surface(window, cx);
            shell.close_right_surface(RightSurface::Diff(1), window, cx);
            assert!(!shell.right_tabs[&shell.panel_key(cx)].contains(&RightSurface::Diff(1)));
            // A closed terminal's shell is gone with it: nothing to reopen.
            shell.remember_closed_tab("parent", RightSurface::Terminal(9), cx);
            assert_eq!(shell.closed_tabs.len(), 1);
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes(&platform_combo(ShortcutId::ReopenClosedTab.default_combo()));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|_, cx| {
        let shell = shell.read(cx);
        let key = shell.panel_key(cx);
        assert!(shell.right_tabs[&key].contains(&RightSurface::Diff(4)), "the diff is back");
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Diff(4), "in front");
        assert!(shell.right_pane_open(cx));
        assert!(shell.closed_tabs.is_empty());
    });
    // Nothing more closed here: the key does nothing.
    cx.simulate_keystrokes(&platform_combo(ShortcutId::ReopenClosedTab.default_combo()));
    cx.update(|_, cx| {
        let shell = shell.read(cx);
        assert_eq!(shell.right_tabs[&shell.panel_key(cx)].len(), 3);
    });
}

// slopty: ⌘T opens the right pane's new tab menu; ⌘[ and ⌘] walk the history.
#[gpui::test]
fn new_tab_and_history_keys(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| window.focus(&shell.composer.focus_handle(cx), cx))
    });
    cx.simulate_keystrokes(&platform_combo("mod-t"));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    shell.read_with(cx, |shell, cx| {
        assert!(shell.right_pane_open(cx));
        assert!(shell.right_plus.get().is_some(), "the new tab menu is open");
    });
    shell.update(cx, |shell, cx| shell.close_right_plus(cx));
    for chat in ["parent", "other"] {
        shell.update(cx, |shell, cx| shell.open_chat(chat.into(), cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    cx.simulate_keystrokes(&platform_combo("mod-["));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("parent"), "back");
    });
    cx.simulate_keystrokes(&platform_combo("mod-]"));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("other"), "forward");
    });
}

// slopty: an archive offers Undo; ⌘Z takes it while its toast is up, and only once.
#[gpui::test]
fn an_archive_is_undone_from_its_toast_or_cmd_z(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel(16);
    let (_replies, inbound) = tokio::sync::mpsc::channel::<String>(16);
    let engine = crate::state::EngineHandle::from_test_client(slopty_view_rpc::RpcClient::new(
        out, inbound,
    ));
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        window.activate_window();
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.set_test_engine(engine));
            shell.set_chat_archived("parent".into(), true, cx);
        })
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(crate::toast::messages(cx), ["Archived \u{201c}parent\u{201d}"]);
    });
    cx.simulate_keystrokes(&platform_combo("mod-z"));
    cx.run_until_parked();
    let sent: Vec<serde_json::Value> = std::iter::from_fn(|| requests.try_recv().ok())
        .map(|raw| serde_json::from_str(&raw).unwrap())
        .collect();
    let archived: Vec<bool> =
        sent.iter().filter_map(|request| request["params"]["archived"].as_bool()).collect();
    assert_eq!(archived, [true, false], "archived, then brought back");
    cx.update(|_, cx| assert!(crate::toast::messages(cx).is_empty(), "the toast went"));
    shell.read_with(cx, |shell, _| assert!(shell.last_archive.is_none()));
    // Nothing left to undo: ⌘Z goes on to whatever else takes it.
    cx.simulate_keystrokes(&platform_combo("mod-z"));
    cx.run_until_parked();
    assert!(requests.try_recv().is_err());
}

// slopty: a subagent drilled into from its card.
/// A subagent of a call shows "parent › subagent" over its run and, for the composer, the bar
/// that says it runs inside its parent; Escape there goes back to the parent.
#[gpui::test]
fn a_drilled_in_subagent_leads_back_to_its_parent(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "child", "title": "Count lines", "parentChatId": "parent",
                    "subagent": true, "inParent": true, "deviceId": "local",
                    "archived": false, "createdAt": Utc::now(),
                }))
                .unwrap(),
            );
        });
        shell.open_chat("child".into(), cx);
        shell.drill_focus_pending = true;
    });
    let labels = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            let update = window.a11y_tree().expect("a tree");
            update
                .nodes
                .iter()
                .map(|(_, node)| (node.role(), node.label().unwrap_or_default().to_owned()))
                .collect::<Vec<_>>()
        })
    };
    let nodes = labels(cx);
    use gpui::accesskit::Role as A11y;
    assert!(
        nodes.contains(&(A11y::Navigation, "parent › Count lines".to_owned())),
        "the breadcrumb: {nodes:?}"
    );
    assert!(nodes.contains(&(A11y::Status, "Runs inside parent".to_owned())), "the bar");
    assert!(nodes.contains(&(A11y::Link, "Back to parent".to_owned())));
    let bar = cx.debug_bounds("subagent-bar").expect("the bar drawn");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("parent"));
    });
    let nodes = labels(cx);
    assert!(!nodes.contains(&(A11y::Status, "Runs inside parent".to_owned())), "back home");
    // The bar stands in the composer's own column, edge for edge.
    let pill = shell.read_with(cx, |shell, cx| shell.composer.read(cx).surface_bounds().get()).expect("the composer");
    let edges = |b: gpui::Bounds<gpui::Pixels>| (f32::from(b.left()).round(), f32::from(b.right()).round());
    assert_eq!(edges(bar), edges(pill), "bar {bar:?}, composer {pill:?}");
}

/// slopty: whether the keyboard is in a Changes surface, whose context the diff's find keys are
/// bound in.
fn keyboard_in_changes(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.draw(cx).clear(cx);
        window.context_stack().iter().any(|context| context.contains("Changes"))
    })
}

/// slopty: a diff the person brings to the front takes the keyboard, so ⌘F finds in the diff
/// and not in the chat: one opened from the pane's offers, and one picked by its tab. Diffs
/// opened for no one (a restored tab, an agent's) leave the keyboard where it was.
#[gpui::test]
fn a_diff_brought_to_the_front_takes_the_keyboard(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let composer = cx.update(|_, cx| shell.read(cx).composer.focus_handle(cx));
    cx.update(|window, cx| window.focus(&composer, cx));
    assert!(!keyboard_in_changes(cx), "diffs opened for no one leave the keyboard");

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_pane_offer(super::pane_offers::PaneOffer::Changes, window, cx);
        });
    });
    assert!(keyboard_in_changes(cx), "the diff opened from the offers has the keyboard");

    cx.update(|window, cx| window.focus(&composer, cx));
    assert!(!keyboard_in_changes(cx), "the composer has it back");
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_click(tab, gpui::Modifiers::default());
    assert!(keyboard_in_changes(cx), "the diff picked by its tab has the keyboard");
    assert_eq!(cx.update(|_, cx| shell.read(cx).resolved_right_active(cx)), RightSurface::Diff(1));

    // With a diff to read, ⌘F puts the keyboard in the diff's find, and it stays there.
    cx.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "parent") {
                    chat.cwd = Some("/work".into());
                }
            });
            let changes = shell.diffs.get(&1).cloned().unwrap();
            changes.update(cx, |changes, cx| changes.show_patch("/work", DIFF, cx));
        });
    });
    cx.run_until_parked();
    assert!(keyboard_in_changes(cx));
    cx.simulate_keystrokes(&crate::settings::platform_combo("mod-f"));
    cx.run_until_parked();
    let changes = cx.update(|_, cx| shell.read(cx).diffs.get(&1).cloned().unwrap());
    assert!(
        cx.update(|window, cx| changes.read(cx).find_has_keyboard(window, cx)),
        "the diff's find has the keyboard"
    );
}

/// slopty: a frame drawn while a key is dispatched, before its handlers (a key draws a dirty
/// window first), and the handler opening the diff's find: the shell's restore, deferred from
/// that frame, leaves the find the keyboard the frame never saw it take.
#[gpui::test]
fn a_find_opened_after_a_frame_in_its_key_s_dispatch_keeps_the_keyboard(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let changes = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "parent") {
                    chat.cwd = Some("/work".into());
                }
            });
            shell.open_pane_offer(super::pane_offers::PaneOffer::Changes, window, cx);
            let changes = shell.diffs.get(&shell.diff_seq).cloned().expect("the diff opened");
            changes.update(cx, |changes, cx| changes.show_patch("/work", DIFF, cx));
            changes
        })
    });
    cx.run_until_parked();
    assert!(keyboard_in_changes(cx));
    // One update, as a key's dispatch: the frame first, then the handler.
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
        changes.update(cx, |changes, cx| changes.open_find(window, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| changes.read(cx).find_has_keyboard(window, cx)),
        "the diff's find has the keyboard"
    );
}

const DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
 fn main() {
-    old();
+    new();
";
