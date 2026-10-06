//! The tabs' commands in the headless workspace: ⌘T's agent in a tab of its own, and the
//! palette's "Other tabs…", "Close other tabs" and "Move to project…", each offered only while
//! it would do something.

use slopty_client::layout::Tab;
use slopty_client::layout::tiling::TabId;

use super::super::actions::{
    CloseOtherTabs, MOVE_TO_PROJECT, MoveToProject, OTHER_TABS, OtherTabs,
};
use super::palette::shell_in;
use super::*;

/// The labels the palette offers the focus now.
fn offered(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update_in(cx, |v, w, cx| v.offered_lines(w, cx)).into_iter().map(|l| l.label).collect()
}

/// The lines of the step that is up, in order.
fn step(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).matches().iter().map(|l| l.label.clone()).collect())
    })
    .unwrap_or_default()
}

/// The ids of the tabs of the project on show, in order.
fn tabs(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<TabId> {
    view.read_with(cx, |v, _| {
        v.layout().shown_project().map(|p| p.tabs().iter().map(Tab::id).collect())
    })
    .unwrap_or_default()
}

/// ⌘T opens an agent's composer in a tab of its own, in the project on show, on the focused
/// shell's machine and in its folder, the composer focused; the shell's tab stays as it was.
/// Asked of a machine out of reach, it opens nothing and says so.
#[gpui::test]
fn cmd_t_starts_an_agent_in_a_tab_of_its_own_where_the_focus_works(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens_in(&view, cx, &fake, SessionId::new(), fake.me, 1, Some("/src/app"));
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, healthy(), cx);
        v.focus_tile(shell, cx);
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    let start = focused(&view, cx).expect("the start has the focus");
    assert_ne!(start, shell);
    let (cwd, agent) = view
        .read_with(cx, |v, _| v.starting.get(start.item).map(|s| (s.cwd.clone(), s.agent.clone())))
        .expect("a start");
    assert_eq!(cwd, "/src/app", "the shell's folder");
    assert!(agent.is(slopty_proto::thread::AgentId::CLAUDE_CODE), "{agent:?}");
    let (a, b) = (pos_of(&view, cx, shell), pos_of(&view, cx, start));
    assert_eq!(a.project, b.project, "in the project on show");
    assert_ne!(a.tab, b.tab, "a tab of its own");
    assert_eq!(tabs(&view, cx).len(), 2);

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(tabs(&view, cx).len(), 2, "nothing opened");
    let said = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert!(said.starts_with("The agent did not open: studio is"), "{said}");
}

/// "Other tabs…" and "Close other tabs" are offered only while the project on show has two
/// tabs or more. The first lists them by their focused work, the one on show ticked, and a
/// line picked shows its tab; the second closes every tab but the one on show and leaves the
/// focus there.
#[gpui::test]
fn other_tabs_lists_the_tabs_and_close_other_tabs_keeps_the_one_on_show(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let lines = offered(&view, cx);
    assert!(!lines.iter().any(|l| l == OTHER_TABS || l == "Close other tabs"), "{lines:?}");

    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let third = opens(&view, cx, &fake, SessionId::new(), fake.me, 3);
    for tile in [second, third] {
        on_new_tab(&view, cx, tile);
    }
    let ids = tabs(&view, cx);
    assert_eq!(ids.len(), 3, "a tab each");
    let lines = offered(&view, cx);
    assert!(lines.iter().any(|l| l == OTHER_TABS), "{lines:?}");
    assert!(lines.iter().any(|l| l == "Close other tabs"), "{lines:?}");

    cx.dispatch_action(OtherTabs);
    cx.run_until_parked();
    let listed = step(&view, cx);
    assert_eq!(listed.len(), 3, "a line a tab: {listed:?}");
    let keys: Option<Vec<String>> = view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).matches().iter().map(|l| l.keys.clone()).collect())
    });
    let shown = ids.iter().position(|id| *id == pos_of(&view, cx, third).tab);
    let ticks: Vec<bool> = keys.unwrap_or_default().iter().map(|k| k == "\u{2713}").collect();
    assert_eq!(ticks.iter().filter(|t| **t).count(), 1, "one tick");
    assert_eq!(ticks.iter().position(|t| *t), shown, "on the tab on show");
    // The first line is the first tab's: ↩ on it shows that tab.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(first), "the first tab shown, its tile focused");

    view.update(cx, |v, cx| v.focus_tile(second, cx));
    cx.dispatch_action(CloseOtherTabs);
    cx.run_until_parked();
    // Each closed shell's session is the worker's to end: it goes when its item does.
    for (tile, version) in [(first, 4), (third, 5)] {
        let key = fake.key;
        view.update_in(cx, |v, _w, cx| {
            let op = ItemOp::Remove(tile.item);
            let by = fake.me;
            v.apply_sync(key, ItemSync::Delta { version, by, op }, cx);
        });
    }
    cx.run_until_parked();
    assert_eq!(tabs(&view, cx).len(), 1, "one tab left");
    assert_eq!(focused(&view, cx), Some(second), "the one on show kept, focused");
}

/// "Move to project…" lists the projects but the focused tile's, and is offered only while
/// there is another; the one picked takes the tile in a tab of its own, shown and focused.
#[gpui::test]
fn move_to_project_takes_the_tile_to_a_tab_of_the_project_picked(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &fake, 1, "/w/atlas", true);
    let lines = offered(&view, cx);
    assert!(!lines.iter().any(|l| l == MOVE_TO_PROJECT), "one project: {lines:?}");
    // Another device's: it arrives in its own project, in the background.
    let (_, bolt) = shell_in(&view, cx, &fake, 2, "/w/bolt", false);
    let (a, b) = (pos_of(&view, cx, atlas), pos_of(&view, cx, bolt));
    assert_ne!(a.project, b.project, "two projects");
    view.update(cx, |v, cx| v.focus_tile(bolt, cx));
    cx.run_until_parked();
    assert!(offered(&view, cx).iter().any(|l| l == MOVE_TO_PROJECT));

    cx.dispatch_action(MoveToProject);
    cx.run_until_parked();
    let listed = step(&view, cx);
    let atlas_name = view.read_with(cx, |v, _| v.project_name_at(a.project));
    assert_eq!(listed, [atlas_name], "the other project alone");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let moved = pos_of(&view, cx, bolt);
    assert_eq!(moved.project, a.project, "in atlas's project now");
    assert_ne!(moved.tab, a.tab, "a tab of its own");
    assert_eq!(focused(&view, cx), Some(bolt), "and focused");
}

/// The panes drawn in the tab on show.
fn panes(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.layout().frame().panes.len())
}

/// The shells asked of `fake` since last drained.
fn asked(fake: &mut Fake) -> usize {
    fake.drain().iter().filter(|m| matches!(m, ClientMsg::OpenSession { .. })).count()
}

/// ⌘⌥T asks for one shell, however often it is pressed before the shell comes, and that shell
/// is the tab's terminal: a pane below the whole tab, a third of its height, focused. Pressed
/// again it is put away, the focus back on the work; again, the same shell comes back.
#[gpui::test]
fn cmd_alt_t_shows_hides_and_shows_the_same_shell(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let work = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();

    cx.simulate_keystrokes("cmd-alt-t");
    cx.simulate_keystrokes("cmd-alt-t");
    cx.run_until_parked();
    assert_eq!(asked(&mut fake), 1, "one shell asked for");
    let session = SessionId::new();
    let terminal = opens(&view, cx, &fake, session, fake.me, 2);
    assert_eq!(pos_of(&view, cx, terminal).tab, pos_of(&view, cx, work).tab, "in the tab");
    assert_eq!(panes(&view, cx), 2);
    assert_eq!(focused(&view, cx), Some(terminal));
    let (above, below) = (
        drawn_at(&view, cx, work).expect("the work drawn"),
        drawn_at(&view, cx, terminal).expect("the terminal drawn"),
    );
    assert!(below.top() >= above.bottom() - px(0.5), "below: {above:?} {below:?}");
    let third = f32::from(below.size.height) / f32::from(below.bottom() - above.top());
    assert!((third - 1.0 / 3.0).abs() < 0.05, "a third of the height: {third}");

    cx.simulate_keystrokes("cmd-alt-t");
    cx.run_until_parked();
    assert_eq!(panes(&view, cx), 1, "put away");
    assert_eq!(focused(&view, cx), Some(work), "the focus back on the work");
    assert!(drawn_at(&view, cx, terminal).is_none(), "not drawn");

    cx.simulate_keystrokes("cmd-alt-t");
    cx.run_until_parked();
    assert_eq!(panes(&view, cx), 2, "back");
    assert_eq!(focused(&view, cx), Some(terminal), "the same shell, focused");
    assert!(terminal_focused(&view, cx, session), "with the keyboard");
    assert_eq!(asked(&mut fake), 0, "nothing more asked");
}

/// ⇧⌘↩ zooms the focused pane over the whole tab and puts the docked navigator away; pressed
/// again, both come back.
#[gpui::test]
fn a_zoom_fills_the_tab_and_both_come_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _left = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    cx.simulate_keystrokes("cmd-d");
    let right = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert!(navigator_docked(&view, cx), "docked to begin with");
    let area = |cx: &mut VisualTestContext| cx.debug_bounds("area").expect("the area");

    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert_eq!(panes(&view, cx), 1, "one pane over the tab");
    assert!(!navigator_docked(&view, cx), "the navigator put away");
    let (zoomed, room) = (drawn_at(&view, cx, right).expect("drawn"), area(cx));
    assert!((zoomed.size.width - room.size.width).abs() < px(2.0), "{zoomed:?} {room:?}");

    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert_eq!(panes(&view, cx), 2, "both panes back");
    assert!(navigator_docked(&view, cx), "and the navigator");
    assert_eq!(focused(&view, cx), Some(right));
}
