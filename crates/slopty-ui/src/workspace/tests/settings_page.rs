//! The settings as a page in the panes' place, the navigator's body their section list.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext as _, Modifiers};

use super::*;
use crate::settings_editor::{Mode, SettingsEditor, SettingsEditorEvent};
use crate::settings_form::schema::{Group, Section};

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

fn section(of: Section) -> &'static str {
    leak(format!("settings-section-{}", of.index()))
}

/// Open the settings on `view` as the app does, and hear what the page asks: Dismiss takes it
/// back, as the app's `close_settings` does.
fn open_settings(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (Entity<SettingsEditor>, Rc<RefCell<Vec<SettingsEditorEvent>>>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    let editor = view.update_in(cx, |v, window, cx| {
        let editor =
            cx.new(|cx| SettingsEditor::new("", "~/settings.toml", Theme::default(), window, cx));
        cx.subscribe(&editor, move |v, _, event: &SettingsEditorEvent, cx| {
            heard.borrow_mut().push(event.clone());
            if *event == SettingsEditorEvent::Dismiss {
                v.show_settings(None, cx);
            }
        })
        .detach();
        v.show_settings(Some(editor.clone()), cx);
        editor
    });
    cx.run_until_parked();
    // The app gives it the keyboard once it is drawn: the search, in the navigator.
    editor.update_in(cx, |e, window, cx| e.focus(window, cx));
    cx.run_until_parked();
    (editor, events)
}

/// The page takes the panes' place under the title bar, beside the docked navigator, and the
/// foot bar goes with the panes. The navigator keeps its top row, and its body is the
/// sections under their groups, each a glyph and a name, with the file and Back at its foot;
/// the page draws no list of its own. A section picked there shows its page, and brings the
/// controls back from the file's face. Back gives the panes back.
#[gpui::test]
fn the_settings_fill_the_panes_and_the_navigator_lists_their_sections(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert!(cx.debug_bounds("foot").is_some(), "the foot bar under the panes");
    let (editor, events) = open_settings(&view, cx);

    let page = cx.debug_bounds("settings-editor").expect("the page");
    let nav = cx.debug_bounds("navigator").expect("the navigator, docked");
    let bar = cx.debug_bounds("titlebar").expect("the title bar");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(page.left() >= nav.right() - px(1.0), "beside the navigator: {page:?} {nav:?}");
    assert!(page.top() >= bar.bottom() - px(0.5), "under the title bar");
    assert_eq!((page.right(), page.bottom()), (window.width, window.height), "to the edges");
    assert!(cx.debug_bounds("foot").is_none(), "the foot bar goes with the panes");

    let list = cx.debug_bounds("settings-sections").expect("the section list");
    assert!(nav.contains(&list.center()), "in the navigator: {list:?} {nav:?}");
    assert!(list.top() >= nav.top() + px(titlebar_height(&Theme::default())) - px(1.0));
    let search = cx.debug_bounds("settings-search").expect("the search");
    assert!(nav.contains(&search.center()), "the search at its top");
    for selector in ["settings-edit-toml", "settings-back"] {
        let at = cx.debug_bounds(selector).expect("the list's foot");
        assert!(nav.contains(&at.center()) && at.top() > list.top(), "{selector} at its foot");
    }
    let said: Vec<String> = tree(cx).into_iter().filter_map(|n| n.label).collect();
    for group in Group::ALL.map(Group::label) {
        assert!(said.iter().any(|l| l == group), "{group}: {said:#?}");
    }
    assert!(editor.read_with(cx, |e, cx| e.form().read(cx).aside()), "no list of its own");

    click(cx, section(Section::Keyboard));
    let title = tree(cx).into_iter().find(|n| n.is("Heading", Some(Section::Keyboard.label())));
    assert!(title.is_some(), "the Keyboard page");
    click(cx, "settings-edit-toml");
    assert_eq!(editor.read_with(cx, |e, _| e.mode()), Mode::Toml, "the file's face");
    click(cx, section(Section::Terminal));
    assert_eq!(editor.read_with(cx, |e, _| e.mode()), Mode::Form, "the controls again");

    click(cx, "settings-back");
    assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss));
    assert!(cx.debug_bounds("settings-editor").is_none(), "gone");
    assert!(cx.debug_bounds("settings-sections").is_none(), "the navigator's rows again");
    assert!(cx.debug_bounds("foot").is_some(), "and the foot bar");
}

/// With the navigator hidden the page carries the list as its own sidebar; Esc from the
/// search, in the navigator's list, leaves, and so does a press on a title tab, which turns to the
/// work.
#[gpui::test]
fn the_page_carries_its_list_and_leaves_for_the_work(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (editor, events) = open_settings(&view, cx);
    view.update_in(cx, |v, window, cx| v.toggle_navigator(&ToggleNavigator, window, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "hidden");
    let page = cx.debug_bounds("settings-editor").expect("the page");
    let list = cx.debug_bounds("settings-sections").expect("its own list");
    assert!(page.contains(&list.center()), "inside the page");
    assert!(!editor.read_with(cx, |e, cx| e.form().read(cx).aside()));

    view.update_in(cx, |v, window, cx| v.toggle_navigator(&ToggleNavigator, window, cx));
    cx.run_until_parked();
    editor.update_in(cx, |e, window, cx| e.focus(window, cx));
    cx.run_until_parked();
    let (role, label) = tree(cx).into_iter().find(|n| n.focused).map(|n| (n.role, n.label)).unzip();
    assert_eq!(
        label.flatten().as_deref(),
        Some(crate::settings_form::SEARCH_PLACEHOLDER),
        "{role:?}"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss), "Esc leaves");
    assert!(cx.debug_bounds("settings-editor").is_none());

    let other = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    on_new_tab(&view, cx, other);
    let first = leak(format!("title-tab-{}", pos_of(&view, cx, shell).tab.get()));
    let (_editor, events) = open_settings(&view, cx);
    click(cx, first);
    assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss), "the work");
    assert!(cx.debug_bounds("settings-editor").is_none());
}

/// On a phone the page is one column under the bar, its head's link opening the file's face
/// and Done leaving, with no navigator list beside it.
#[gpui::test]
fn a_phone_s_page_opens_the_file_from_its_head(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(402.0), px(874.0)));
    cx.run_until_parked();
    let (editor, events) = open_settings(&view, cx);
    let page = cx.debug_bounds("settings-editor").expect("the page");
    let file = cx.debug_bounds("settings-edit-toml").expect("the head's link");
    assert!(page.contains(&file.center()), "in the page's head: {file:?} {page:?}");
    click(cx, "settings-edit-toml");
    assert_eq!(editor.read_with(cx, |e, _| e.mode()), Mode::Toml, "the file's face");
    click(cx, "settings-edit-form");
    click(cx, "settings-done");
    assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss), "Done leaves");
}

/// The page leaves the focus where it found it: the tile focused before the settings opened
/// is the focused one once they close, on a phone as on a Mac.
#[gpui::test]
fn the_page_gives_the_focus_back_to_its_tile(cx: &mut TestAppContext) {
    for width in [1280.0, 402.0] {
        let (view, cx) = workspace(cx);
        let studio = connect(&view, cx, 1, "studio");
        let _first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
        let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
        cx.simulate_resize(size(px(width), px(874.0)));
        view.update(cx, |v, cx| v.focus_tile(second, cx));
        cx.run_until_parked();
        let (editor, events) = open_settings(&view, cx);
        editor.update_in(cx, |e, _window, cx| e.close(cx));
        cx.run_until_parked();
        assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss));
        assert_eq!(focused(&view, cx), Some(second), "{width} pt: the tile it found");
    }
}
