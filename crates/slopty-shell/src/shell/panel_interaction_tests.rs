// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! The sidebar beside a session's panes, and the panes themselves, through Shell's actions and
//! GPUI's real layout.
use super::panes::Pane;
use super::*;
use gpui::{AppContext, Bounds, TestAppContext, VisualTestContext};

fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        let mut config = UiSettings::default();
        config.sidebar_width = 300.0;
        settings::init(config, dir.path(), cx);
    });
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.no_project = true;
            state.selected_chat = Some("a".into());
            state.chats = ["a", "b"]
                .into_iter()
                .map(|id| {
                    serde_json::from_value(serde_json::json!({
                        "id": id, "title": id, "deviceId": "local", "archived": false,
                        "createdAt": Utc::now(),
                    }))
                    .unwrap()
                })
                .collect();
            // Each session runs in its terminal, the main column.
            state.session_terminals = [("a", "t-a"), ("b", "t-b")].map(|(c, t)| (c.into(), t.into())).into();
            state
        });
        let mut shell = Shell::new(
            state,
            EngineBootConfig {
                data_dir: dir.path().into(),
                default_harness: slopty_view_proto::HarnessId::MOCK,
            },
            cx,
        );
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        shell.active_chat = "a".into();
        shell
    });
    cx.simulate_resize(gpui::size(px(1600.0), px(900.0)));
    (dir, shell, cx)
}

fn change(
    shell: &Entity<Shell>,
    cx: &mut VisualTestContext,
    action: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>),
) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            action(shell, window, cx);
            cx.notify();
        })
    });
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    /// The sidebar's column and the session's, beside it.
    widths: [f32; 2],
    /// The session's terminal.
    session: Bounds<Pixels>,
    /// How much narrower than its pane the session's terminal fits its grid.
    hold: f32,
}

fn draw(shell: &Entity<Shell>, cx: &mut VisualTestContext, millis: u64) -> Frame {
    // The shell's motion reads the dispatcher's clock, which the test moves.
    cx.executor().advance_clock(Duration::from_millis(millis).mul_f32(motion::speed_scale()));
    shell.update(cx, |_, cx| cx.notify());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let hold =
        shell.read_with(cx, |shell, cx| f32::from(shell.session_terminal.read(cx).fit_hold().width));
    let mut width = |id| cx.debug_bounds(id).map_or(0.0, |b| f32::from(b.size.width));
    Frame {
        widths: [width("sidebar-column"), width("conversation-column")],
        session: cx.debug_bounds("session-terminal").unwrap_or_default(),
        hold,
    }
}

#[track_caller]
fn near(actual: f32, expected: f32) {
    assert!((actual - expected).abs() <= 0.5, "expected {expected}px, got {actual}px");
}

#[track_caller]
fn expect(frame: Frame, widths: [f32; 2]) {
    for (actual, expected) in frame.widths.into_iter().zip(widths) {
        near(actual, expected);
    }
}

fn settled(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Frame {
    draw(shell, cx, 600);
    draw(shell, cx, 600)
}

fn select(shell: &mut Shell, chat: Option<&str>, cx: &mut Context<Shell>) {
    let state = shell.state.clone();
    state.update(cx, |s, cx| s.select_chat(chat.map(str::to_owned), cx));
    shell.on_state_changed(&state, cx);
}

/// Put away and brought back, the sidebar glides with no jump or overshoot, and the session's
/// panes fill the room it leaves on every frame; each terminal in them, whatever its share,
/// holds the grid of the glide's narrower end and lets it go as it lands. A view summoned beside
/// the session docks at its right inside that room, the terminal keeping the larger part. A
/// window resize and reduced motion land at once, and settings take the whole room.
#[gpui::test]
fn the_panes_fill_the_room_the_sidebar_leaves_through_its_glide(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    let initial = settled(&shell, cx);
    expect(initial, [300.0, 1300.0]);
    near(f32::from(initial.session.left()), 300.0);
    near(f32::from(initial.session.size.width), 1300.0);

    // A view docks at the session's right, in the room the sidebar leaves.
    change(&shell, cx, |s, window, cx| s.add_diff_surface(window, cx));
    let docked = settled(&shell, cx);
    expect(docked, [300.0, 1300.0]);
    let session = cx.debug_bounds("pane-session").expect("the session's pane");
    let view = cx.debug_bounds("pane-diff-1").expect("the view's pane");
    near(f32::from(session.left()), 300.0);
    near(f32::from(view.right()), 1600.0);
    assert!(session.right() <= view.left(), "the view stands at the session's right");
    assert!(view.size.width < session.size.width, "the terminal keeps the larger part");

    // Each way, the sidebar glides; the panes follow it frame by frame.
    let rail = sidebar::SIDEBAR_RAIL_WIDTH;
    for target in [rail, 300.0] {
        let before = draw(&shell, cx, 0);
        change(&shell, cx, |s, _, cx| s.toggle_sidebar(cx));
        let first = draw(&shell, cx, 0);
        expect(first, before.widths);
        near(f32::from(first.session.left()), f32::from(before.session.left()));
        let source = before.widths[0];
        let mut previous = source;
        let mut held = None;
        let mut last = first;
        for _ in 0..15 {
            let frame = draw(&shell, cx, 16);
            let width = frame.widths[0];
            assert!(width >= source.min(target) - 0.5 && width <= source.max(target) + 0.5, "{width}");
            assert!((width - previous) * (target - source) >= -0.5, "the glide turns back: {width}");
            near(width + frame.widths[1], 1600.0);
            near(f32::from(frame.session.left()), width);
            // The terminal fits one grid through the glide: its pane less the hold stays put.
            if frame.hold > 0.0 {
                let fits = f32::from(frame.session.size.width) - frame.hold;
                near(fits, *held.get_or_insert(fits));
            }
            previous = width;
            last = frame;
        }
        near(previous, target);
        near(last.hold, 0.0);
        // The grid held is the narrower end's: where the glide started as the panes widen,
        // where it lands as they narrow.
        let held = held.expect("the glide holds the terminal's grid");
        near(held, f32::from(before.session.size.width).min(f32::from(last.session.size.width)));
    }

    // A window resize lands at once, with nothing lagging the window's edge.
    for width in [1024.0, 800.0, 1600.0] {
        cx.simulate_resize(gpui::size(px(width), px(900.0)));
        let frame = draw(&shell, cx, 0);
        near(frame.widths.iter().sum(), width);
        expect(draw(&shell, cx, 300), frame.widths);
    }
    // So does every toggle under reduced motion.
    cx.update(|_, cx| motion::set_reduced_motion(cx, true));
    for _ in 0..2 {
        change(&shell, cx, |s, _, cx| s.toggle_sidebar(cx));
        let landed = draw(&shell, cx, 0);
        near(landed.widths.iter().sum(), 1600.0);
        expect(settled(&shell, cx), landed.widths);
    }

    // A session keeps its own panes; the new-session canvas has none; settings take the room.
    change(&shell, cx, |s, _, cx| select(s, Some("b"), cx));
    settled(&shell, cx);
    assert!(cx.debug_bounds("pane-diff-1").is_none(), "b's view is its own");
    change(&shell, cx, |s, _, cx| select(s, None, cx));
    settled(&shell, cx);
    assert!(cx.debug_bounds("pane-diff-1").is_none());
    change(&shell, cx, |s, _, cx| select(s, Some("a"), cx));
    settled(&shell, cx);
    assert!(cx.debug_bounds("pane-diff-1").is_some(), "a's view is back with it");
    change(&shell, cx, |s, _, cx| s.open_settings(SettingsSection::Appearance, cx));
    draw(&shell, cx, 0);
    assert!(cx.debug_bounds("conversation-column").is_none());
    change(&shell, cx, |s, _, cx| s.close_settings(cx));
    expect(draw(&shell, cx, 0), docked.widths);
}

/// A subagent of a call runs inside its agent's session, so its row shows that agent's
/// panes as the agent left them, and ⌘I there summons the agent's inspector among them; an agent
/// another agent started has panes of its own, its terminal alone until the person summons one.
/// The panes are kept, its files among them, and come back in a new launch once its chat shows.
#[gpui::test]
fn a_subagent_shows_its_agents_panes_and_the_panes_come_back(cx: &mut TestAppContext) {
    let (_dir, shell, cx) = setup(cx);
    change(&shell, cx, |shell, window, cx| {
        shell.state.update(cx, |state, _| {
            let main = state.chats[0].clone();
            let mut sub = main.clone();
            sub.id = "s".into();
            sub.parent_chat_id = Some("a".into());
            (sub.subagent, sub.in_parent) = (true, true);
            let mut started = sub.clone();
            started.id = "o".into();
            started.in_parent = false;
            state.chats.extend([sub, started]);
        });
        shell.add_diff_surface(window, cx);
    });
    settled(&shell, cx);
    let views = shell.read_with(cx, |shell, cx| shell.panes_now(cx).views());
    assert!(matches!(views.as_slice(), [RightSurface::Diff(_)]), "{views:?}");
    let diff = views[0];

    // Its agent's panes, as the agent left them; the inspector summoned there is the agent's.
    change(&shell, cx, |shell, _, cx| select(shell, Some("s"), cx));
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panel_key(cx), "a");
        assert_eq!(shell.panes_now(cx).views(), [diff], "the agent's view, nothing added");
    });
    change(&shell, cx, |shell, _, cx| shell.toggle_inspector(cx));
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panes_now(cx).views(), [diff, RightSurface::Inspector]);
        assert!(shell.inspector_shown(cx), "⌘I summons it among the agent's panes");
    });
    change(&shell, cx, |shell, _, cx| select(shell, Some("o"), cx));
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panel_key(cx), "o");
        assert_eq!(shell.panes_now(cx).panes(), [Pane::Session], "its terminal alone");
    });
    change(&shell, cx, |shell, _, cx| shell.toggle_inspector(cx));
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panes_now(cx).views(), [RightSurface::Inspector]);
        assert!(shell.inspector_shown(cx));
    });
    change(&shell, cx, |shell, window, cx| shell.summon_files(window, cx));
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panes_now(cx).views(), [RightSurface::Inspector, RightSurface::Files]);
    });

    change(&shell, cx, |shell, _, cx| shell.keep_tabs(cx));
    let kept = shell.read_with(cx, |shell, _| shell.settings.session_tabs.clone());
    assert_eq!(kept.keys().collect::<Vec<_>>(), ["a", "o"]);
    use crate::settings::SavedSurface;
    assert_eq!(kept["a"].tabs, [SavedSurface::Diff, SavedSurface::Inspector]);
    assert_eq!(kept["a"].active, Some(1), "the inspector had the keyboard");
    assert_eq!(kept["o"].tabs, [SavedSurface::Inspector, SavedSurface::Files], "the files are kept too");
    assert_eq!(kept["o"].active, Some(1));

    // A new launch: no panes in this run, and the kept ones come back once "a" shows.
    change(&shell, cx, |shell, _, cx| {
        shell.panes.clear();
        shell.unrestored_tabs = kept.clone();
        select(shell, Some("s"), cx);
    });
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        let views = shell.panes_now(cx).views();
        assert!(matches!(views.as_slice(), [RightSurface::Diff(_), RightSurface::Inspector]), "{views:?}");
        assert_eq!(shell.front_view(cx), Some(RightSurface::Inspector), "with the keyboard where it was");
        // "o"'s wait until it shows.
        assert_eq!(shell.unrestored_tabs.keys().collect::<Vec<_>>(), ["o"]);
    });
    change(&shell, cx, |shell, _, cx| select(shell, Some("o"), cx));
    settled(&shell, cx);
    shell.read_with(cx, |shell, cx| {
        assert_eq!(shell.panes_now(cx).views(), [RightSurface::Inspector, RightSurface::Files]);
        assert_eq!(shell.front_view(cx), Some(RightSurface::Files));
        assert!(shell.files.contains_key("o"), "the files come back with their explorer");
        assert!(shell.unrestored_tabs.is_empty());
    });
}
