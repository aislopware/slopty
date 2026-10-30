// slopty: Slopty's own tests (not in zeron).
//! slopty: the app's chrome as a person and assistive tech meet it: the sidebar's headers, rows
//! and filter, the palettes, the chat menu and the empty states. Each test draws the whole shell
//! in a headless window (`retention_tests::launch`) and judges by its accessibility tree and its
//! laid-out bounds.

use std::collections::HashMap;

use gpui::accesskit::{NodeId, Role};
use gpui::{TestAppContext, VisualTestContext};

use super::retention_tests::{DESKTOP, launch, session_row};
use super::*;

/// A node of the last frame's accessibility tree.
#[derive(Debug, Clone)]
struct Node {
    role: Role,
    label: String,
    expanded: Option<bool>,
    disabled: bool,
    bounds: gpui::Bounds<Pixels>,
}

fn tree(cx: &mut VisualTestContext) -> HashMap<NodeId, Node> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.set_a11y_active(true);
        window.refresh();
        window.draw(cx).clear(cx);
        let Some(update) = window.a11y_tree() else { return HashMap::new() };
        let scale = f64::from(window.scale_factor());
        update
            .nodes
            .iter()
            .map(|(id, node)| {
                let r = node.bounds().unwrap_or_default();
                let at = |v: f64| px((v / scale) as f32);
                (
                    *id,
                    Node {
                        role: node.role(),
                        label: node.label().unwrap_or_default().to_owned(),
                        expanded: node.is_expanded(),
                        disabled: node.is_disabled(),
                        bounds: gpui::Bounds::from_corners(
                            gpui::point(at(r.x0), at(r.y0)),
                            gpui::point(at(r.x1), at(r.y1)),
                        ),
                    },
                )
            })
            .collect()
    })
}

fn find(nodes: &HashMap<NodeId, Node>, role: Role, label: &str) -> Option<Node> {
    nodes.values().find(|node| node.role == role && node.label == label).cloned()
}

fn find_starting(nodes: &HashMap<NodeId, Node>, role: Role, label: &str) -> Option<Node> {
    nodes.values().find(|node| node.role == role && node.label.starts_with(label)).cloned()
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

/// A project on the local device.
fn space(id: &str) -> slopty_view_proto::Space {
    serde_json::from_value(serde_json::json!({
        "id": id, "deviceId": "local", "path": format!("/work/{id}"), "createdAt": Utc::now(),
    }))
    .unwrap()
}

/// The local device, as the server lists it.
fn device() -> slopty_view_proto::Device {
    serde_json::from_value(serde_json::json!({
        "id": "local", "name": "Studio", "platform": "macos", "createdAt": Utc::now(),
    }))
    .unwrap()
}

/// A sidebar group's header is a button a screen reader names by its group and its count and
/// hears open or shut, and a press folds it.
#[gpui::test]
fn a_sidebar_header_is_a_named_button_that_says_it_is_open(cx: &mut TestAppContext) {
    let (_shell, cx) = launch(cx, DESKTOP);
    let nodes = tree(cx);
    let working = find_starting(&nodes, Role::Button, "Working, ").expect("the Working group's header");
    assert!(working.label["Working, ".len()..].parse::<usize>().is_ok(), "its count: {working:?}");
    assert_eq!(working.expanded, Some(true), "open: {working:?}");
    cx.simulate_click(working.bounds.center(), gpui::Modifiers::default());
    let nodes = tree(cx);
    let working = find_starting(&nodes, Role::Button, "Working, ").expect("the header, still");
    assert_eq!(working.expanded, Some(false), "a press folds it: {working:?}");
}

/// slopty: a group heading reads 12 medium in the muted ink, its count (Amp's) and an open
/// group's chevron (Claude desktop's caret) drawn only under the pointer.
#[gpui::test]
fn a_sidebar_headers_count_shows_under_the_pointer(cx: &mut TestAppContext) {
    let (_shell, cx) = launch(cx, DESKTOP);
    let nodes = tree(cx);
    let working = find_starting(&nodes, Role::Button, "Working, ").expect("the Working group's header");
    let key = "status:status:working";
    let count = |cx: &mut VisualTestContext| {
        draw(cx);
        cx.debug_bounds("sidebar-header-count-status:status:working").expect("the count holds its place")
    };
    let resting = count(cx);
    let header_key = format!("sidebar-header-{key}");
    let rest = cx.update(|_, cx| {
        let theme = Theme::of(cx);
        crate::motion::hover_blend(&header_key, theme.text_muted.opacity(0.0), theme.text_muted)
    });
    assert_eq!(rest.a, 0.0, "unseen at rest");
    let chevron = |cx: &mut VisualTestContext| cx.update(|_, _| spaces::sidebar_hover_reveal(&header_key));
    assert_eq!(chevron(cx), 0.0, "an open group's chevron rests unseen with it");
    // Settled at once, so the frame read is the faded-in one.
    cx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    cx.simulate_mouse_move(working.bounds.center(), None, gpui::Modifiers::default());
    let hovered = count(cx);
    assert_eq!(hovered, resting, "the count fades in where it stood: nothing moves");
    let shown = cx.update(|_, cx| {
        let theme = Theme::of(cx);
        crate::motion::hover_blend(&header_key, theme.text_muted.opacity(0.0), theme.text_muted)
    });
    assert!(shown.a > 0.9, "shown under the pointer: {shown:?}");
    assert!(chevron(cx) > 0.9, "and its chevron");
}

/// slopty: a finished chat untouched for 72 hours folds into Inactive, the last group and shut
/// at first (Amp's); with none in it, it says so.
#[gpui::test]
fn a_chat_untouched_for_three_days_folds_into_inactive(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    let nodes = tree(cx);
    let inactive = find(&nodes, Role::Button, "Inactive, 0").expect("Inactive stands, empty");
    let working = find_starting(&nodes, Role::Button, "Working, ").expect("Working");
    assert!(inactive.bounds.top() > working.bounds.top(), "last");
    assert_eq!(inactive.expanded, Some(false), "shut at first");
    cx.simulate_click(inactive.bounds.center(), gpui::Modifiers::default());
    draw(cx);
    assert!(
        cx.debug_bounds("sidebar-group-empty-status:status:inactive").is_some(),
        "opened, it says it holds none"
    );
    assert!(find(&tree(cx), Role::Status, super::sidebar_status::NO_INACTIVE).is_some());

    // The finished chat goes untouched for three days: it leaves Done for Inactive.
    shell.update(cx, |shell, cx| {
        let now = Utc::now();
        shell.state.update(cx, |state, _| {
            let chat = state.chats.iter_mut().find(|chat| chat.id == "b").unwrap();
            chat.created_at = now - chrono::TimeDelta::hours(80);
            chat.last_message_at = Some(now - chrono::TimeDelta::hours(73));
        });
        shell.sidebar_clock = now;
        cx.notify();
    });
    let nodes = tree(cx);
    assert!(find(&nodes, Role::Button, "Inactive, 1").is_some(), "it holds the chat");
    assert!(find_starting(&nodes, Role::Button, "Done, ").is_none(), "Done holds none now");
    assert!(cx.debug_bounds("sidebar-group-empty-status:status:inactive").is_none());
    let row = cx.debug_bounds("chat-b").expect("its row, in the open group");
    let header = find(&nodes, Role::Button, "Inactive, 1").unwrap();
    assert!(row.top() > header.bounds.top(), "under its header");
}

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

/// A row at rest wears no mark: only a state worth a glance does (one that waits on the person),
/// while its slot keeps the titles on one edge.
#[gpui::test]
fn a_row_at_rest_draws_no_dot(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| {
        let now = Utc::now();
        shell.state.update(cx, |state, _| {
            state.apply_sessions(vec![
                session_row("a", slopty_view_proto::SessionStatus::AwaitingInput, now),
                session_row("b", slopty_view_proto::SessionStatus::Idle, now),
            ]);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("chat-mark-a").is_some(), "a row that needs you has its mark");
    assert!(cx.debug_bounds("chat-mark-b").is_none() && cx.debug_bounds("chat-dot-b").is_none(), "a row at rest has none");
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.apply_sessions(vec![
                session_row("a", slopty_view_proto::SessionStatus::Working, Utc::now()),
                session_row("b", slopty_view_proto::SessionStatus::Idle, Utc::now()),
            ]);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("chat-mark-a").is_some(), "the same mark, now working");
    let (a, b) = (cx.debug_bounds("chat-status-a").unwrap(), cx.debug_bounds("chat-status-b").unwrap());
    assert_eq!(a.left(), b.left(), "the slot stays");
    assert_eq!(a.size, b.size, "the slot stays");
}

/// slopty: a row's one mark says working, waiting, done and failed, and morphs between them in
/// its slot; a row at rest says nothing.
#[test]
fn a_rows_mark_follows_its_state() {
    use crate::loaders::status_mark::MarkState;
    use slopty_view_proto::ChatIndicator;
    let mark = super::sidebar_mark_state;
    assert_eq!(mark(ChatIndicator::Working), Some(MarkState::Working));
    assert_eq!(mark(ChatIndicator::AwaitingInput), Some(MarkState::Waiting));
    assert_eq!(mark(ChatIndicator::Completed), Some(MarkState::Done));
    assert_eq!(mark(ChatIndicator::Errored), Some(MarkState::Failed));
    assert_eq!(mark(ChatIndicator::Idle), None);
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

/// The sidebar's list is one keyboard stop, at the open chat's row; Up and Down walk the rows
/// in the order they draw, and Enter opens the one with the keyboard.
#[gpui::test]
fn the_sidebar_list_is_one_stop_walked_with_up_and_down(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| shell.open_chat("a".into(), cx));
    draw(cx);
    let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
    assert_eq!(order, ["a", "b"], "the rows as they draw");
    shell.read_with(cx, |shell, _| {
        assert!(shell.sidebar_keys.stops_at("a"), "Tab reaches the open chat's row");
        assert!(!shell.sidebar_keys.stops_at("b"), "and only it");
    });
    let (a, b) = shell.update(cx, |shell, cx| (shell.sidebar_keys.focus("a", cx), shell.sidebar_keys.focus("b", cx)));
    cx.update(|window, cx| window.focus(&a, cx));
    draw(cx);
    cx.simulate_keystrokes("down");
    draw(cx);
    cx.update(|window, _| assert!(b.is_focused(window), "Down moves to the row below"));
    cx.simulate_keystrokes("up");
    draw(cx);
    cx.update(|window, _| assert!(a.is_focused(window), "Up moves back"));
    cx.simulate_keystrokes("down");
    draw(cx);
    // A key press clicks as it comes up.
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("enter").unwrap() });
    draw(cx);
    assert_eq!(
        shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone()).as_deref(),
        Some("b"),
        "Enter opens it"
    );
}

/// The project filter says it opens a menu, what it filters to and whether the menu is open.
#[gpui::test]
fn the_project_filter_is_a_named_menu_button(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, _| state.spaces = vec![space("app")]);
    });
    let nodes = tree(cx);
    let filter = find(&nodes, Role::Button, "Filter projects: All projects").expect("the filter");
    assert_eq!(filter.expanded, Some(false), "shut: {filter:?}");
    cx.simulate_click(filter.bounds.center(), gpui::Modifiers::default());
    let nodes = tree(cx);
    let filter = find(&nodes, Role::Button, "Filter projects: All projects").expect("the filter");
    assert_eq!(filter.expanded, Some(true), "open: {filter:?}");
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

/// The command palette stands pinned by its top where a centred palette at its tallest would
/// start (opencode's), and between 280 and 480 tall, wherever its results leave its foot.
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
    let window = DESKTOP.size.1;
    assert!((f32::from(full.top()) - (window - 480.0) / 2.0).abs() < 1.0, "where a tallest palette starts: {full:?}");
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
    for label in ["Rename", "Pin", "Archive"] {
        assert!(find(&nodes, Role::MenuItem, label).is_some(), "{label} is a named menu item");
    }
    assert!(find(&nodes, Role::MenuItem, "Delete\u{2026}").is_none(), "no Delete");
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
            state.chats = vec![chat("pin"), chat("regular"), chat("other")];
        });
        shell.settings.space_filter = None;
        shell.settings.sidebar_pins_mut("local".into()).push("pin".into());
    }

    /// slopty: ⌘I opens the session's inspector beside its terminal, read-only, and closes it
    /// again; the keyboard goes back to the terminal.
    #[gpui::test]
    fn cmd_i_opens_and_closes_the_inspector(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = super::super::retention_tests::launch(cx, super::super::retention_tests::DESKTOP);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_session_terminals([("a".to_owned(), "t-a".to_owned())].into());
                cx.notify();
            });
            shell.open_chat("a".into(), cx);
        });
        cx.run_until_parked();
        let shown = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let open = shell.read_with(cx, |shell, cx| shell.inspector_shown(cx));
            assert_eq!(open, cx.debug_bounds("inspector-header").is_some(), "drawn as it stands");
            open
        };
        let before = shown(cx);
        cx.simulate_keystrokes("cmd-i");
        cx.run_until_parked();
        assert_ne!(shown(cx), before, "⌘I turns it");
        cx.simulate_keystrokes("cmd-i");
        cx.run_until_parked();
        assert_eq!(shown(cx), before, "and back");
        if !before {
            let terminal = cx.update(|window, cx| {
                shell.read(cx).session_terminal.read(cx).focus_handle().contains_focused(window, cx)
            });
            assert!(terminal, "the terminal has the keyboard back");
        }
    }

    /// slopty: an agent's session shown the first time opens on its inspector, the first tab of
    /// its pane, without the person knowing ⌘I. The tab is then the session's like any other: one
    /// the person closed is not given again, in this run or the next, and a plain terminal is
    /// given none.
    #[gpui::test]
    fn an_agent_session_is_given_its_inspector_the_first_time_it_shows(cx: &mut gpui::TestAppContext) {
        use super::super::RightSurface;
        let (shell, cx) = super::super::retention_tests::launch(cx, super::super::retention_tests::DESKTOP);
        let show = |chat: &str, cx: &mut gpui::VisualTestContext| {
            shell.update(cx, |shell, cx| shell.open_chat(chat.into(), cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            shell.read_with(cx, |shell, cx| {
                (shell.right_tabs.get(chat).cloned().unwrap_or_default(), shell.inspector_shown(cx))
            })
        };
        assert_eq!(show("a", cx), (vec![RightSurface::Inspector], true), "given, in front");
        assert!(cx.debug_bounds("inspector-header").is_some(), "and drawn");
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| shell.close_right_surface(RightSurface::Inspector, window, cx));
        });
        assert_eq!(show("b", cx), (vec![RightSurface::Inspector], true), "each session its own");
        assert_eq!(show("a", cx), (Vec::new(), false), "closed, it stays closed");
        let kept = shell.read_with(cx, |shell, _| shell.settings.session_tabs.get("a").cloned());
        assert_eq!(kept.map(|kept| kept.tabs), Some(Vec::new()), "and is kept closed");

        // The next run knows only what was kept.
        shell.update(cx, |shell, _| {
            shell.right_tabs.clear();
            shell.inspector_given.clear();
            shell.unrestored_tabs = shell.settings.session_tabs.clone();
        });
        assert_eq!(show("a", cx), (Vec::new(), false), "nor given in the next run");

        let terminal = format!("{}t", crate::inspector::TERMINAL_CHAT);
        shell.update(cx, |shell, cx| {
            let row = super::super::retention_tests::chat_row(&terminal, 3, None, chrono::Utc::now());
            shell.state.update(cx, |state, cx| {
                state.chats.push(row);
                cx.notify();
            });
        });
        assert_eq!(show(&terminal, cx), (Vec::new(), false), "a plain terminal has nothing to inspect");
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
