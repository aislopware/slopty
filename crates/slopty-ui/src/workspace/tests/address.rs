//! A page's address in its tile's header: ⌘L or a click on the host turns the header into the
//! address field, ↩ sends the page there through its item, and Esc leaves it where it was.

use super::*;

const HOME: &str = "http://127.0.0.1:5173/";

/// A page tile from `url` on `fake`, focused, with `name` as the human's name for it.
fn page(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    name: Option<&str>,
) -> TileRef {
    let tile = arrives(view, cx, fake, ItemKind::Browser { url: HOME.into() }, 1);
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        if let Some(name) = name {
            let op = ItemOp::Rename { id: tile.item, name: Some(name.to_owned()) };
            v.apply_sync(key, ItemSync::Delta { version: 2, by: ClientId::new(), op }, cx);
        }
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    tile
}

/// The address field's text and what of it is selected, while it is up.
fn field(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<(String, String)> {
    view.read_with(cx, |v, cx| {
        let rename = v.rename.as_ref().filter(|r| r.field == Field::Address)?;
        let input = rename.input.read(cx);
        Some((input.value().to_string(), input.selected_value().to_string()))
    })
}

fn url_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> String {
    view.read_with(cx, |v, cx| v.browser(tile.item).map(|b| b.read(cx).url().to_owned()))
        .expect("a page view")
}

/// ⌘L puts the whole address in the header's field, selected, so typing replaces it; ↩ sends
/// the new address to the worker as the item's, and the header names the new host.
#[gpui::test]
fn command_l_opens_the_address_and_return_goes_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake, None);
    fake.drain();
    assert!(cx.debug_bounds(selector("address", tile.item)).is_none(), "at rest, no field");
    cx.simulate_keystrokes("cmd-l");
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("address", tile.item)).is_some(), "the field is up");
    assert_eq!(field(&view, cx), Some((HOME.to_owned(), HOME.to_owned())), "all of it selected");

    cx.simulate_input("localhost:3000/docs");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let url = "http://localhost:3000/docs".to_owned();
    let sent = ClientMsg::Items(ItemOp::SetUrl { id: tile.item, url: url.clone() });
    assert_eq!(fake.drain(), vec![sent], "the address alone, as the item's");
    assert!(cx.debug_bounds(selector("address", tile.item)).is_none(), "and the field closed");
    assert_eq!(url_of(&view, cx, tile), url, "the page goes there");
    let title = view.read_with(cx, |v, _| v.tile_title(v.item(tile).unwrap()));
    assert_eq!(title, "localhost:3000/docs", "an untitled page is named by its new address");
}

/// Esc closes the field with the address as it was, sends nothing, and gives the keyboard
/// back to the workspace.
#[gpui::test]
fn escape_leaves_the_address_as_it_was(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake, None);
    fake.drain();
    cx.simulate_keystrokes("cmd-l");
    cx.run_until_parked();
    cx.simulate_input("elsewhere.test");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(field(&view, cx), None, "the field closed");
    assert!(fake.drain().is_empty(), "nothing sent");
    assert_eq!(url_of(&view, cx, tile), HOME);
    let workspace_focused = cx.update(|window, cx| view.read(cx).focus.is_focused(window));
    assert!(workspace_focused, "the keyboard is the workspace's again");
}

/// An address that is none keeps the field open to be put right, and sends nothing.
#[gpui::test]
fn a_bad_address_keeps_the_field(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake, None);
    fake.drain();
    cx.simulate_keystrokes("cmd-l");
    cx.run_until_parked();
    cx.simulate_input("ftp://files.test");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(fake.drain().is_empty(), "nothing sent");
    assert_eq!(field(&view, cx).map(|(text, _)| text).as_deref(), Some("ftp://files.test"));
    assert_eq!(url_of(&view, cx, tile), HOME);
}

/// At rest a titled page's place is its address, a button named "Address" whose value is the
/// whole of it; a click turns the header into the field, a text input named "Address" that
/// holds the keyboard.
#[gpui::test]
fn the_address_is_a_button_at_rest_and_a_field_when_clicked(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake, Some("dev server"));
    let nodes = tree(cx);
    let place = nodes.iter().find(|n| n.is("Button", Some("Address"))).expect("the place");
    assert_eq!(place.value.as_deref(), Some(HOME), "{nodes:#?}");

    let at = cx.debug_bounds(selector("place", tile.item)).expect("drawn").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(field(&view, cx), Some((HOME.to_owned(), HOME.to_owned())));
    let nodes = tree(cx);
    let input = nodes.iter().find(|n| n.focused).expect("a focused node");
    assert!(input.role.ends_with("TextInput"), "{input:?}");
    assert_eq!(input.label.as_deref(), Some("Address"));
    assert!(!nodes.iter().any(|n| n.is("Button", Some("Address"))), "the field is the place");
}

/// A page shown in a pane of tabs keeps its address in the row after the tabs, a button that
/// a click turns into the field, as a lone tile's header does.
#[gpui::test]
fn a_tabbed_page_keeps_its_address_after_the_tabs(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake, Some("dev server"));
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 3);
    one_pane(&view, cx, &[shell, tile]);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let nodes = tree(cx);
    let place = nodes.iter().find(|n| n.is("Button", Some("Address"))).expect("the place");
    assert_eq!(place.value.as_deref(), Some(HOME), "{nodes:#?}");
    let tab = cx.debug_bounds(selector("tab", tile.item)).expect("its tab");
    let at = cx.debug_bounds(selector("place", tile.item)).expect("drawn");
    assert!(at.left() >= tab.right(), "after the tabs: {at:?} {tab:?}");
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(field(&view, cx), Some((HOME.to_owned(), HOME.to_owned())));
}

/// Another client's new address for the page moves this client's page too: the item is the
/// one address they share.
#[gpui::test]
fn a_new_address_from_another_client_moves_the_page(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake, None);
    let url = "https://docs.test/guide".to_owned();
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let op = ItemOp::SetUrl { id: tile.item, url: url.clone() };
        v.apply_sync(key, ItemSync::Delta { version: 3, by: ClientId::new(), op }, cx);
    });
    cx.run_until_parked();
    assert_eq!(url_of(&view, cx, tile), url);
    let shown =
        view.read_with(cx, |v, cx| v.browser(tile.item).map(|b| b.read(cx).page().url.clone()));
    assert_eq!(shown, Some(url), "the header names it at once");
}

/// With no page focused, ⌘L is "Open URL…".
#[gpui::test]
fn command_l_without_a_page_opens_a_url(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    cx.simulate_keystrokes("cmd-l");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.palette_open()), "the palette, for an address");
    assert_eq!(field(&view, cx), None);
}

/// ⌘← and ⌘→ are the page's history while a page is focused, and nothing's elsewhere: a
/// shell keeps them.
#[gpui::test]
fn the_page_keys_are_bound_only_on_a_page(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let page = page(&view, cx, &fake, None);
    // Matched against the focused element's path in the frame drawn, as a key is dispatched:
    // `Window::bindings_for_action` reads the context stack the dispatch tree was last built
    // with, which a frame drawn partly from last frame's can leave behind.
    let bound = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            let path = window.context_stack();
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let on = |action: &dyn gpui::Action| {
                keymap
                    .bindings_for_action(action)
                    .filter(|b| b.predicate().is_none_or(|p| p.eval(&path)))
                    .count()
            };
            (on(&PageBack), on(&PageForward))
        })
    };
    assert_eq!(focused(&view, cx), Some(page));
    assert_eq!(bound(cx), (1, 1), "on the page");
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    assert_eq!(bound(cx), (0, 0), "not on a shell");
}
