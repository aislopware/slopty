//! The preview tab: a file opened in passing takes the place of the last one, until an edit, a
//! double-click or a pick on purpose keeps it.

use gpui::{Modifiers, MouseButton, MouseDownEvent};

use super::*;

/// The file tile for `path`, if one is open.
fn file_tile(view: &Entity<WorkspaceView>, cx: &VisualTestContext, path: &str) -> Option<TileRef> {
    view.read_with(cx, |v, _| {
        v.items().find_map(|(worker, item)| match &item.kind {
            ItemKind::File { path: p } if p == path => Some(TileRef { worker, item: item.id }),
            _ => None,
        })
    })
}

fn preview(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> bool {
    view.read_with(cx, |v, _| v.is_preview(tile))
}

/// Files opened in passing one after another leave one tab: each takes the last one's pane and
/// place, and the last one goes. An edit keeps the preview, so the next opens beside it; a file
/// picked on purpose opens kept and keeps the preview it lands on.
#[gpui::test]
fn a_file_opened_in_passing_takes_the_previews_place(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let open = |cx: &mut VisualTestContext, path: &'static str| {
        view.update_in(cx, |v, _w, cx| v.open_file_on(Some(key), path, None, cx));
        cx.run_until_parked();
        file_tile(&view, cx, path).expect("its tile")
    };
    let first = open(cx, "/w/a.rs");
    assert!(preview(&view, cx, first), "opened in passing: a preview");
    let at = pos_of(&view, cx, first);
    assert!(
        cx.debug_bounds(selector("name", first.item)).is_some(),
        "its name is drawn, set as a preview's"
    );

    let second = open(cx, "/w/b.rs");
    assert!(file_tile(&view, cx, "/w/a.rs").is_none(), "the last preview goes");
    assert_eq!(pos_of(&view, cx, second), at, "the new one stands in its pane");
    assert!(preview(&view, cx, second));
    assert_eq!(focused(&view, cx), Some(second), "and takes the focus");

    // A double-click on its name keeps it: the next opens beside, as a preview of its own.
    let name = cx.debug_bounds(selector("name", second.item)).expect("its name");
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: name.center(),
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.run_until_parked();
    assert!(!preview(&view, cx, second), "kept by a double-click");
    assert!(
        view.read_with(cx, |v, _| v.rename.is_none()),
        "a preview's double-click keeps it rather than naming it"
    );
    let third = open(cx, "/w/c.rs");
    assert!(file_tile(&view, cx, "/w/b.rs").is_some(), "a kept tab stays");
    assert!(preview(&view, cx, third));

    // An edit keeps it.
    let text = slopty_proto::file::FileRead::Text {
        text: "fn c() {}".to_owned(),
        size: 9,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, "/w/c.rs", &text, cx);
        v.focus_tile(third, cx);
    });
    cx.run_until_parked();
    cx.simulate_input("x");
    cx.run_until_parked();
    assert!(!preview(&view, cx, third), "kept by an edit");
    let fourth = open(cx, "/w/d.rs");
    assert!(file_tile(&view, cx, "/w/c.rs").is_some(), "an edited file is never replaced");

    // Picked on purpose, the preview's own file is kept, and a new file opens kept.
    view.update_in(cx, |v, _w, cx| {
        let _shown = v.show_file(Some(key), "/w/d.rs", None, cx);
    });
    cx.run_until_parked();
    assert!(!preview(&view, cx, fourth), "picked on purpose: kept");
    view.update_in(cx, |v, _w, cx| {
        let _shown = v.show_file(Some(key), "/w/e.rs", None, cx);
    });
    cx.run_until_parked();
    let fifth = file_tile(&view, cx, "/w/e.rs").expect("its tile");
    assert!(!preview(&view, cx, fifth), "a file picked on purpose opens kept");
    let sixth = open(cx, "/w/f.rs");
    assert!(file_tile(&view, cx, "/w/e.rs").is_some(), "and is never replaced");
    assert!(preview(&view, cx, sixth));
}

/// A preview in a tab out of sight is not reached into: the next file opens beside the focus
/// in the tab on show, and the one out of sight stays.
#[gpui::test]
fn a_preview_out_of_sight_stays_where_it_is(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    view.update_in(cx, |v, _w, cx| v.open_file_on(Some(key), "/w/a.rs", None, cx));
    cx.run_until_parked();
    let a = file_tile(&view, cx, "/w/a.rs").expect("its tile");
    let other = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    on_new_tab(&view, cx, other);
    view.update_in(cx, |v, _w, cx| v.open_file_on(Some(key), "/w/b.rs", None, cx));
    cx.run_until_parked();
    let b = file_tile(&view, cx, "/w/b.rs").expect("its tile");
    assert!(file_tile(&view, cx, "/w/a.rs").is_some(), "the preview out of sight stays");
    assert_ne!(pos_of(&view, cx, a).tab, pos_of(&view, cx, b).tab);
    assert_eq!(pos_of(&view, cx, b).tab, pos_of(&view, cx, other).tab, "in the tab on show");
}
