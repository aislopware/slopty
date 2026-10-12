//! The settings as a page in the panes' place, the navigator's body their section list.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext as _, Modifiers};

use super::*;
use crate::settings_editor::{SettingsEditor, SettingsEditorEvent};
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
/// back, as the app's `close_settings` does. `file_opens` as where this Mac has a worker.
fn open_settings_with(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    file_opens: bool,
) -> (Entity<SettingsEditor>, Rc<RefCell<Vec<SettingsEditorEvent>>>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    let editor = view.update_in(cx, |v, window, cx| {
        let editor = cx.new(|cx| {
            let editor = SettingsEditor::new("", Theme::default(), window, cx);
            editor.set_file_opens(file_opens, cx);
            editor
        });
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

/// [`open_settings_with`] on a Mac with a worker of its own.
fn open_settings(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (Entity<SettingsEditor>, Rc<RefCell<Vec<SettingsEditorEvent>>>) {
    open_settings_with(view, cx, true)
}

/// The page takes the panes' place under the window's own top line, beside the docked
/// navigator. The navigator's header row holds ← and the settings' filter, and its body is the
/// sections under their groups, each by its name, with the file at its foot; the page draws no
/// list of its own. A section picked there shows its page. Back gives the
/// panes back.
#[gpui::test]
fn the_settings_fill_the_panes_and_the_navigator_lists_their_sections(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (editor, events) = open_settings(&view, cx);

    let page = cx.debug_bounds("settings-editor").expect("the page");
    let nav = cx.debug_bounds("navigator").expect("the navigator, docked");
    let bar = cx.debug_bounds("titlebar").expect("the title bar");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(page.left() >= nav.right() - px(1.0), "beside the navigator: {page:?} {nav:?}");
    assert!(page.top() >= bar.bottom() - px(0.5), "under the title bar");
    assert_eq!((page.right(), page.bottom()), (window.width, window.height), "to the edges");

    let list = cx.debug_bounds("settings-sections").expect("the section list");
    assert!(nav.contains(&list.center()), "in the navigator: {list:?} {nav:?}");
    assert!(list.top() >= nav.top() + px(titlebar_height(&Theme::default())) - px(1.0));
    let header = cx.debug_bounds("nav-header").expect("the navigator's header row");
    for selector in ["settings-back", "settings-search"] {
        let at = cx.debug_bounds(selector).expect("the header row's controls");
        assert!(header.contains(&at.center()), "{selector} in the header row: {at:?}");
    }
    assert!(cx.debug_bounds("nav-new-agent").is_none(), "the starts give the row up");
    let file = cx.debug_bounds("settings-open-file").expect("the list's foot");
    assert!(nav.contains(&file.center()) && file.top() > list.top(), "the file at its foot");
    let said: Vec<String> = tree(cx).into_iter().filter_map(|n| n.label).collect();
    for group in Group::ALL.map(Group::label) {
        assert!(said.iter().any(|l| l == group), "{group}: {said:#?}");
    }
    assert!(editor.read_with(cx, |e, cx| e.form().read(cx).aside()), "no list of its own");

    click(cx, section(Section::Keyboard));
    let title = tree(cx).into_iter().find(|n| n.is("Heading", Some(Section::Keyboard.label())));
    assert!(title.is_some(), "the Keyboard page");

    click(cx, "settings-back");
    assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss));
    assert!(cx.debug_bounds("settings-editor").is_none(), "gone");
    assert!(cx.debug_bounds("settings-sections").is_none(), "the navigator's rows again");
    assert!(cx.debug_bounds("titlebar").is_none(), "the panes' tab bars are the top line");
}

/// The settings are one left column (`.research/design-polish-2026-10-12.md` §4.5): the page's
/// name, its group heads and its rows stand on one edge 32 in from the page's side, and on a
/// wide window they keep to the 720 measure, left aligned, rather than stretch.
#[gpui::test]
fn settings_have_one_left_column(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(1600.0), px(900.0)));
    cx.run_until_parked();
    let (_editor, _events) = open_settings(&view, cx);
    let page = cx.debug_bounds("settings-page").expect("the page");
    let title = cx.debug_bounds("settings-title").expect("the page's name");
    let first = |cx: &mut VisualTestContext, what: &str| {
        (0..32).find_map(|i| cx.debug_bounds(leak(format!("settings-{what}-{i}"))))
    };
    let heading = first(cx, "heading").expect("a group's head");
    let row = first(cx, "ruled").expect("a row");
    let side = Theme::default().spacing.page();
    assert!((title.left() - page.left() - px(side)).abs() < px(0.5), "32 in: {title:?} {page:?}");
    for (what, at) in [("head", heading), ("row", row)] {
        assert!((at.left() - title.left()).abs() < px(0.5), "the {what} on the edge: {at:?}");
    }
    assert!(row.size.width <= px(crate::settings_form::MEASURE) + px(0.5), "{row:?}");
    assert!(
        page.size.width > px(2.0_f32.mul_add(side, crate::settings_form::MEASURE)),
        "a wide page"
    );
    // Delta's anatomy (`.research/delta-re-2026-10-12.md` §4.9): the title at 16 on its 22
    // line, and each group's head ruled under.
    let line = Theme::default().roles().page_heading.line;
    assert!(title.size.height <= px(line) + px(0.5), "the title's one line: {title:?}");
    let ruled = cx.update(|window, _| {
        window.painted_quads().into_iter().any(|q| {
            q.border_widths.bottom.0 > 0.0
                && (q.bounds.bottom().0 / window.scale_factor() - f32::from(heading.bottom())).abs()
                    < 1.0
        })
    });
    assert!(ruled, "a rule under the group's head: {heading:?}");
}

/// With the navigator hidden the page carries the list as its own sidebar; Esc from the
/// search, in the navigator's list, leaves, and so does showing a tab, which turns to the work.
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
    let first = pos_of(&view, cx, shell).tab;
    let (_editor, events) = open_settings(&view, cx);
    view.update_in(cx, |v, _w, cx| v.show_tab(first, cx));
    cx.run_until_parked();
    assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss), "the work");
    assert!(cx.debug_bounds("settings-editor").is_none());
}

/// The settings keep the navigator's rhythm (`.research/warp-composition-2026-10-12.md` §2.7):
/// over a list of its own, ← stands on the rows' words' edge; every group head but the first
/// has a group's gap over it, and a page's section label stands a section's 24 under what is
/// over it.
#[gpui::test]
fn the_settings_keep_one_rhythm(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (_editor, _events) = open_settings(&view, cx);
    view.update_in(cx, |v, window, cx| v.toggle_navigator(&ToggleNavigator, window, cx));
    cx.run_until_parked();
    let theme = Theme::default();
    let at = |cx: &mut VisualTestContext, s: &'static str| {
        cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} is drawn"))
    };
    let back = at(cx, "settings-back");
    let row = at(cx, section(Section::Appearance));
    let pad = (slopty_kit::kit::icon_button_side(&theme) - theme.glyph()) / 2.0;
    let glyph = back.left() + px(pad);
    let words = row.left() + px(theme.spacing.sm);
    assert!((glyph - words).abs() < px(0.5), "← on the rows' edge: {glyph:?} {words:?}");
    let (first, second) = (at(cx, "settings-group-0"), at(cx, "settings-group-1"));
    let more = second.size.height - first.size.height;
    let gap = theme.spacing.lg - theme.spacing.xs;
    assert!((f32::from(more) - gap).abs() < 0.5, "a group's gap over the head: {more:?}");

    click(cx, section(Section::Terminal));
    let heads: Vec<(usize, _)> = (0..80_usize)
        .filter_map(|n| cx.debug_bounds(leak(format!("settings-heading-{n}"))).map(|b| (n, b)))
        .collect();
    let [_, (n, second), ..] = heads.as_slice() else { panic!("two sections: {heads:?}") };
    let before = n.saturating_sub(1);
    let over = [format!("settings-footer-{before}"), format!("settings-ruled-{before}")]
        .into_iter()
        .find_map(|s| cx.debug_bounds(leak(s)))
        .expect("what stands over the second section");
    let gap = f32::from(second.top() - over.bottom());
    assert!((gap - theme.spacing.xl).abs() < 0.5, "a section's 24: {gap}");
}

/// On a phone the page is one column under the bar, Done in its head leaving, with no
/// navigator list beside it. A phone has no worker of its own to open the file in, so the
/// head offers no way to it.
#[gpui::test]
fn a_phone_s_page_is_the_form_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(402.0), px(874.0)));
    cx.run_until_parked();
    let (_editor, events) = open_settings_with(&view, cx, false);
    let page = cx.debug_bounds("settings-editor").expect("the page");
    let done = cx.debug_bounds("settings-done").expect("Done");
    assert!(page.contains(&done.center()), "in the page's head: {done:?} {page:?}");
    assert!(cx.debug_bounds("settings-open-file").is_none(), "no tile to open it in");
    assert!(cx.debug_bounds("settings-open-file-glyph").is_none(), "nor its glyph");
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
