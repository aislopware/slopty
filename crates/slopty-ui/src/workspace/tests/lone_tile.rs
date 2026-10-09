//! A tab's one tile in the headless workspace: its title is said once, by its tab in the title
//! bar, and its pane draws no header; the rest of its header stands in the bar's strip, and a
//! second tile in the tab brings the pane's header back.

use gpui::{MouseButton, MouseDownEvent, MouseUpEvent};
use slopty_core::WallMs;

use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// The title tab of `tile`'s tab.
fn tab_of(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    part: &str,
    tile: TileRef,
) -> &'static str {
    leak(format!("{part}-{}", pos_of(view, cx, tile).tab.get()))
}

/// A shell alone in its tab has no pane header: no row of one tab under the bar's tab, and its
/// body starts at the area's top. The bar's strip holds what else its header said. A second
/// tile beside it brings back both panes' headers and the strip goes; closing that tile again
/// takes the header away.
#[gpui::test]
fn a_one_tile_tab_says_its_title_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let title = selector("title", shell.item);
    let strip = selector("tile-strip", shell.item);
    let alone = |cx: &mut VisualTestContext| {
        assert!(cx.debug_bounds(title).is_none(), "no pane header");
        assert!(cx.debug_bounds(selector("lone-tab", shell.item)).is_none(), "no row of one tab");
        let item = cx.debug_bounds(selector("item", shell.item)).expect("drawn");
        let area = cx.debug_bounds("area").expect("the area");
        assert_eq!(item.top(), area.top(), "the body starts at the area's top");
        let bar = cx.debug_bounds("titlebar").expect("the bar");
        let at = cx.debug_bounds(strip).expect("the bar's strip");
        assert!(bar.contains(&at.center()), "the strip is in the bar: {at:?} {bar:?}");
        let text = cx.debug_bounds(tab_of(&view, cx, "title-tab-text", shell)).expect("its tab");
        assert!(text.right() <= at.left(), "after the tabs: {text:?} {at:?}");
    };
    alone(cx);
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Heading", Some("terminal Terminal"))), "{nodes:#?}");

    let other = paired(&view, cx, &fake, shell, 2);
    for tile in [shell, other] {
        let header = cx.debug_bounds(selector("title", tile.item)).expect("a header again");
        let item = cx.debug_bounds(selector("item", tile.item)).expect("drawn");
        assert_eq!(header.origin, item.origin, "at its tile's top");
    }
    assert!(cx.debug_bounds(strip).is_none(), "the strip goes with the second tile");

    view.update_in(cx, |v, window, cx| v.close_tile(other, window, cx));
    cx.run_until_parked();
    alone(cx);
}

/// A file alone in its tab: its tab says its folder after its title, and "Edited" between them
/// once it holds an edit not yet on disk, as its pane's header did; the strip does not say it
/// again.
#[gpui::test]
fn a_lone_files_tab_says_where_it_is_and_that_it_is_edited(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let path = "/w/docs/notes.txt";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    let text = slopty_proto::file::FileRead::Text {
        text: "# Notes".to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, path, &text, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let place = tab_of(&view, cx, "title-tab-place", tile);
    let edited = tab_of(&view, cx, "title-tab-edited", tile);
    let name = tab_of(&view, cx, "title-tab-text", tile);
    let at = cx.debug_bounds(place).expect("the folder, in the tab");
    assert!(cx.debug_bounds(edited).is_none(), "nothing edited yet");

    cx.simulate_input("x");
    cx.run_until_parked();
    let said = cx.debug_bounds(edited).expect("edited, in the tab");
    let title = cx.debug_bounds(name).expect("the title");
    let at_now = cx.debug_bounds(place).expect("the folder");
    assert!(
        title.right() <= said.left() && said.right() <= at_now.left(),
        "{title:?} {said:?} {at:?}"
    );
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_none(), "said once");
}

/// A double-click on the tab of a tab's one tile opens the field that names the tile, in the
/// title's place; ↩ sends the name.
#[gpui::test]
fn a_double_click_on_a_lone_tiles_tab_names_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();
    let tab = cx.debug_bounds(tab_of(&view, cx, "title-tab", shell)).expect("its tab");
    let at = tab.center();
    for click_count in [1, 2] {
        let (button, modifiers) = (MouseButton::Left, Modifiers::default());
        let first_mouse = false;
        cx.simulate_event(MouseDownEvent {
            button,
            position: at,
            modifiers,
            click_count,
            first_mouse,
        });
        cx.simulate_event(MouseUpEvent { button, position: at, modifiers, click_count });
    }
    cx.run_until_parked();
    let field = cx.debug_bounds(selector("rename", shell.item)).expect("the field is up");
    let tab = cx.debug_bounds(tab_of(&view, cx, "title-tab", shell)).expect("its tab");
    assert!(tab.contains(&field.center()), "in the tab: {field:?} {tab:?}");
    cx.simulate_keystrokes("a p i enter");
    cx.run_until_parked();
    let api = ClientMsg::Items(ItemOp::Rename { id: shell.item, name: Some("api".to_owned()) });
    assert_eq!(fake.drain(), vec![api], "the name alone");
}

/// A tab's one agent says how it is doing once: its glyph in the bar's strip, with its words,
/// and no mark on its tab. A second tile in the tab gives the tab its marks back.
#[gpui::test]
fn a_lone_agents_state_is_said_once_in_the_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let tile = opens(&view, cx, &fake, agent, fake.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(agent), cx);
        v.show_face(agent, false, cx);
    });
    cx.run_until_parked();
    let glyph = cx.debug_bounds(selector("agent", tile.item)).expect("the state's glyph");
    let strip = cx.debug_bounds(selector("tile-strip", tile.item)).expect("the strip");
    assert!(strip.contains(&glyph.center()), "in the strip: {glyph:?} {strip:?}");
    let mark = tab_of(&view, cx, "title-tab-mark", tile);
    let first = leak(format!("{mark}-0"));
    assert!(cx.debug_bounds(first).is_none(), "no mark on its tab");

    paired(&view, cx, &fake, tile, 2);
    assert!(cx.debug_bounds(first).is_some(), "with company, the tab marks it");
}
