// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! The keyboard among a session's panes, through the shell's real panes and key dispatch,
//! without booting an engine.
use gpui::{TestAppContext, VisualTestContext};

use super::panes::{Direction, Pane};
use super::*;

struct NavigationHost {
    shell: Entity<Shell>,
    _data_dir: tempfile::TempDir,
}

impl Render for NavigationHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell.update(cx, |shell, cx| {
            let (preferred, neutral) = shell.navigation_focus_fallback(cx);
            defer_mounted_focus_restore(
                shell.shortcut_focus.clone(),
                preferred,
                neutral,
                window,
                cx,
            );
            let main = shell.render_main(window, cx);
            div()
                .size_full()
                .flex()
                .track_focus(&shell.shortcut_focus)
                .child(div().track_focus(&shell.unfocused))
                .on_action(
                    cx.listener(|shell, _: &NextSession, _, cx| shell.cycle_navigation(true, cx)),
                )
                .on_action(
                    cx.listener(|shell, _: &PrevSession, _, cx| shell.cycle_navigation(false, cx)),
                )
                // The window's own keys (`chrome_keys`), and the keys the root's own
                // listeners take (Esc out of a view among them).
                .map(|el| Shell::chrome_actions(el, cx))
                .capture_key_down(cx.listener(Shell::on_key_down_capture))
                .on_key_down(cx.listener(Shell::on_key_down))
                .child(
                    div()
                        .id("navigation-main")
                        .debug_selector(|| "navigation-main".into())
                        .w(px(1100.))
                        .h_full()
                        .flex()
                        .child(main),
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
        apply_keymap(cx, &KeymapConfig::default());
    });
    let (host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
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
                            "createdAt": Utc::now()
                                .checked_sub_signed(chrono::Duration::minutes(
                                    i64::try_from(ix).unwrap(),
                                ))
                                .unwrap(),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.selected_chat = Some("parent".into());
                // Each session runs in its terminal, the main column.
                state.session_terminals = [("parent", "t-parent"), ("other", "t-other")]
                    .map(|(c, t)| (c.into(), t.into()))
                    .into();
            });
            shell.active_chat = "parent".into();
            motion::set_reduced_motion(cx, true);
            shell
        });
        NavigationHost { shell, _data_dir: dir }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    draw(cx);
    // Two views beside the parent session, stacked at its right, the newer with the keyboard.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            for _ in 0..2 {
                shell.add_diff_surface(window, cx);
            }
        });
    });
    draw(cx);
    (shell, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

/// The session's terminal, the main column's keyboard.
fn session_focus(shell: &Entity<Shell>, cx: &App) -> FocusHandle {
    shell.read(cx).session_terminal.read(cx).focus_handle()
}

fn press(cx: &mut VisualTestContext, forward: bool) {
    let id = if forward { ShortcutId::NextSession } else { ShortcutId::PrevSession };
    cx.simulate_keystrokes(&platform_combo(id.default_combo()));
    draw(cx);
}

fn selected(shell: &Entity<Shell>, cx: &VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
}

/// The pane with the keyboard, which the model and the window agree on.
#[track_caller]
fn keyboard_in(shell: &Entity<Shell>, cx: &mut VisualTestContext, pane: Pane) {
    cx.update(|window, cx| {
        let shell = shell.read(cx);
        assert_eq!(shell.panes_now(cx).focused(), pane);
        assert!(shell.pane_holds_keyboard(pane, window, cx), "{pane:?} holds the keyboard");
    });
}

/// ⌃⇥ walks the sessions from whichever pane holds the keyboard, a view or a shell split beside
/// the terminal too, and each session keeps the pane that had its keyboard.
#[gpui::test]
fn the_session_keys_switch_sessions_from_any_pane(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let newer = Pane::View(RightSurface::Diff(2));
    keyboard_in(&shell, cx, newer);
    press(cx, true);
    assert_eq!(selected(&shell, cx).as_deref(), Some("other"));
    keyboard_in(&shell, cx, Pane::Session);
    press(cx, false);
    assert_eq!(selected(&shell, cx).as_deref(), Some("parent"));
    keyboard_in(&shell, cx, newer);

    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.split_pane(Some(Direction::Right), cx)));
    draw(cx);
    let split = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
    assert!(matches!(split, Pane::Shell(_)), "{split:?}");
    keyboard_in(&shell, cx, split);
    press(cx, true);
    assert_eq!(selected(&shell, cx).as_deref(), Some("other"));
}

/// ⌘] and ⌘[ walk a session's panes in reading order, ⌥⌘arrows by direction, from the terminal
/// as from a view: the terminal never eats them.
#[gpui::test]
fn the_pane_keys_walk_the_panes_from_the_terminal_too(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    let (older, newer) = (Pane::View(RightSurface::Diff(1)), Pane::View(RightSurface::Diff(2)));
    for (key, lands) in
        [("mod-]", Pane::Session), ("mod-]", older), ("mod-[", Pane::Session), ("mod-[", newer)]
    {
        cx.simulate_keystrokes(&platform_combo(key));
        draw(cx);
        keyboard_in(&shell, cx, lands);
    }
    for (key, lands) in [("alt-mod-left", Pane::Session), ("alt-mod-right", older)] {
        cx.simulate_keystrokes(&platform_combo(key));
        draw(cx);
        keyboard_in(&shell, cx, lands);
    }
    cx.update(|window, cx| assert!(!session_focus(&shell, cx).is_focused(window)));
}

/// Esc in a view that takes it for nothing of its own gives the keyboard back to the session's
/// terminal, as the view's key again does.
#[gpui::test]
fn escape_in_a_view_gives_the_keyboard_back_to_the_terminal(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    keyboard_in(&shell, cx, Pane::View(RightSurface::Diff(2)));
    cx.simulate_keystrokes("escape");
    draw(cx);
    keyboard_in(&shell, cx, Pane::Session);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panes_now(cx).views().len(), 2, "the views stay");
    });
}

/// A press anywhere in a pane gives it the keyboard: on its header, which takes no keyboard of
/// its own, as on what it shows.
#[gpui::test]
fn a_press_on_a_pane_s_header_gives_it_the_keyboard(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    keyboard_in(&shell, cx, Pane::View(RightSurface::Diff(2)));
    for (header, pane) in [
        ("pane-header-diff-1", Pane::View(RightSurface::Diff(1))),
        ("pane-header-session", Pane::Session),
    ] {
        let at = cx.debug_bounds(header).unwrap_or_else(|| panic!("{header} drawn"));
        cx.simulate_click(
            gpui::point(at.left() + px(40.0), at.center().y),
            gpui::Modifiers::default(),
        );
        draw(cx);
        keyboard_in(&shell, cx, pane);
    }
}

/// Under a dialog or an overlay that holds the keyboard the session keys wait, whichever pane
/// the keyboard was in.
#[gpui::test]
fn a_dialog_or_a_picker_holds_the_session_keys_in_every_pane(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    for pane in [Pane::Session, Pane::View(RightSurface::Diff(2))] {
        cx.update(|_, cx| shell.update(cx, |shell, cx| shell.focus_pane(pane, cx)));
        draw(cx);
        shell.update(cx, |shell, _| shell.delete_space_confirm = Some("space".into()));
        press(cx, true);
        press(cx, false);
        assert_eq!(selected(&shell, cx).as_deref(), Some("parent"));
        shell.update(cx, |shell, _| shell.delete_space_confirm = None);
    }
    // New session's picker, an overlay that owns the keyboard.
    shell.update(cx, |shell, cx| shell.open_new_session_picker(None, cx));
    draw(cx);
    press(cx, true);
    assert_eq!(selected(&shell, cx).as_deref(), Some("parent"));
}

/// A view closed with the keyboard hands it to the pane that stood nearest, and the last one
/// back to the terminal, where the session keys go on working.
#[gpui::test]
fn a_closed_view_hands_the_keyboard_to_its_neighbour(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.close_view(RightSurface::Diff(2), cx)));
    draw(cx);
    keyboard_in(&shell, cx, Pane::View(RightSurface::Diff(1)));
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.close_view(RightSurface::Diff(1), cx)));
    draw(cx);
    keyboard_in(&shell, cx, Pane::Session);
    cx.update(|window, cx| assert!(session_focus(&shell, cx).is_focused(window)));
    press(cx, true);
    assert_eq!(selected(&shell, cx).as_deref(), Some("other"));
}

// ⇧⌘T opens the view closed last again, docked beside its session with the keyboard.
#[gpui::test]
fn a_closed_view_comes_back_with_its_key(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    cx.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.close_view(RightSurface::Diff(1), cx);
            assert_eq!(shell.panes_now(cx).views(), [RightSurface::Diff(2)]);
            assert_eq!(shell.closed_tabs.len(), 1);
        });
    });
    draw(cx);
    cx.simulate_keystrokes(&platform_combo(ShortcutId::ReopenClosedTab.default_combo()));
    draw(cx);
    let back = Pane::View(RightSurface::Diff(3));
    shell.read_with(cx, |shell, cx| {
        assert_eq!(
            shell.panes_now(cx).views(),
            [RightSurface::Diff(2), RightSurface::Diff(3)],
            "the diff is back"
        );
        assert_eq!(shell.closed_tabs, []);
    });
    keyboard_in(&shell, cx, back);
    // Nothing more closed here: the key does nothing.
    cx.simulate_keystrokes(&platform_combo(ShortcutId::ReopenClosedTab.default_combo()));
    draw(cx);
    shell.read_with(cx, |shell, cx| assert_eq!(shell.panes_now(cx).views().len(), 2));
}

// ⌥⌘[ and ⌥⌘] walk the history.
#[gpui::test]
fn the_history_keys_walk_back_and_forward(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    for chat in ["parent", "other"] {
        shell.update(cx, |shell, cx| shell.open_chat(chat.into(), cx));
        draw(cx);
    }
    cx.simulate_keystrokes(&platform_combo("alt-mod-["));
    draw(cx);
    assert_eq!(selected(&shell, cx).as_deref(), Some("parent"), "back");
    cx.simulate_keystrokes(&platform_combo("alt-mod-]"));
    draw(cx);
    assert_eq!(selected(&shell, cx).as_deref(), Some("other"), "forward");
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

/// The copy key in a view's pane copies a transcript's selection (the inspector's) once nothing
/// in the view took it.
#[gpui::test]
fn the_copy_key_copies_a_transcript_selection_from_a_view(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        let handle = shell.update(cx, |shell, cx| shell.view_handle(RightSurface::Diff(2), cx));
        window.focus(&handle, cx);
    });
    draw(cx);
    select_transcript_text(cx);
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("selected"));
    clear_transcript_selection();
}

/// ⌃C in a terminal, the session's or a split shell's, is the terminal's: a transcript's
/// selection never takes it.
#[gpui::test]
fn ctrl_c_in_a_terminal_is_not_taken_by_a_transcript_selection(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let (shell, cx) = setup(cx);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.focus_pane(Pane::Session, cx)));
    draw(cx);
    keyboard_in(&shell, cx, Pane::Session);
    select_transcript_text(cx);
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("sentinel"));
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.split_pane(Some(Direction::Down), cx)));
    draw(cx);
    let split = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
    keyboard_in(&shell, cx, split);
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("sentinel"));
    clear_transcript_selection();
}

// An archive offers Undo; ⌘Z takes it while its toast is up, and only once.
#[gpui::test]
fn an_archive_is_undone_from_its_toast_or_cmd_z(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel(16);
    let (_replies, inbound) = tokio::sync::mpsc::channel::<String>(16);
    let engine =
        crate::state::EngineHandle::from_test_client(slopty_view_rpc::RpcClient::new(out, inbound));
    let (shell, cx) = setup(cx);
    cx.update(|window, cx| {
        window.activate_window();
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.set_test_engine(engine));
            shell.set_chat_archived("parent".into(), true, cx);
        });
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
    assert_eq!(requests.try_recv().ok(), None, "nothing sent");
}

/// Whether the keyboard is in a Changes surface, whose context the diff's find keys are
/// bound in.
fn keyboard_in_changes(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.draw(cx).clear(cx);
        window.context_stack().iter().any(|context| context.contains("Changes"))
    })
}

/// A diff given the keyboard has the diff's own keys, so ⌘F finds in the diff and not in
/// the terminal: one summoned by Changes' key, and one the person clicks into.
#[gpui::test]
fn a_diff_given_the_keyboard_has_the_diffs_keys(cx: &mut TestAppContext) {
    let (shell, cx) = setup(cx);
    assert!(keyboard_in_changes(cx), "the diff opened last has the keyboard");
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.focus_pane(Pane::Session, cx)));
    assert!(!keyboard_in_changes(cx), "the session's terminal has it back");

    // Changes' key summons the session's changes with the keyboard.
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.summon_changes(window, cx)));
    assert!(keyboard_in_changes(cx), "the diff its key summoned has the keyboard");
    assert_eq!(cx.update(|_, cx| shell.read(cx).front_view(cx)), Some(RightSurface::Diff(1)));

    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.focus_pane(Pane::Session, cx)));
    assert!(!keyboard_in_changes(cx));
    let pane = cx.debug_bounds("pane-diff-2").expect("the newer diff's pane").center();
    cx.simulate_click(pane, gpui::Modifiers::default());
    assert!(keyboard_in_changes(cx), "the diff clicked into has the keyboard");
    assert_eq!(cx.update(|_, cx| shell.read(cx).front_view(cx)), Some(RightSurface::Diff(2)));

    // With a diff to read, ⌘F puts the keyboard in the diff's find, and it stays there.
    cx.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "parent") {
                    chat.cwd = Some("/work".into());
                }
            });
            let changes = shell.diffs.get(&2).cloned().unwrap();
            changes.update(cx, |changes, cx| changes.show_patch("/work", DIFF, cx));
        });
    });
    cx.run_until_parked();
    assert!(keyboard_in_changes(cx));
    cx.simulate_keystrokes(&platform_combo("mod-f"));
    cx.run_until_parked();
    let changes = cx.update(|_, cx| shell.read(cx).diffs.get(&2).cloned().unwrap());
    assert!(
        cx.update(|window, cx| changes.read(cx).find_has_keyboard(window, cx)),
        "the diff's find has the keyboard"
    );
}

/// A frame drawn while a key is dispatched, before its handlers (a key draws a dirty
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
            shell.summon_changes(window, cx);
            let changes = shell.diffs.get(&1).cloned().expect("the diff summoned");
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
