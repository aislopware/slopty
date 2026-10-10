//! Tile bodies and their neighbours in the headless workspace: what a body says while it waits
//! on its worker, where a file's "Edited" sits, the header slot beside a failure the grid
//! shows, and the sash's double-click.

use gpui::{Bounds, MouseButton, MouseDownEvent, MouseUpEvent};
use slopty_core::WallMs;

use super::*;
use crate::icons::Status;
use crate::screen::LOADING_GRACE;

/// Within a point: bounds are laid out in floats.
#[track_caller]
fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.5, "{a} vs {b}");
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

/// A remote window whose stream has not opened keeps a blank body for the loading grace, so a
/// fast answer never flashes a word; past it the body says what is opening and on which
/// worker. Its header slot turns the working mark the whole time.
#[gpui::test]
fn a_remote_window_waits_blank_then_says_what_is_opening(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window: slopty_core::WindowId(7) }, 1);
    let waiting = selector("waiting", tile.item);
    assert!(cx.debug_bounds(waiting).is_none(), "blank within the grace");
    let status = view.read_with(cx, |v, _| v.item(tile).and_then(|item| v.tile_status(tile, item)));
    assert_eq!(status, Some(Status::Working), "the header slot turns");

    cx.executor().advance_clock(LOADING_GRACE);
    cx.run_until_parked();
    let text = bounds(cx, waiting);
    let body = bounds(cx, selector("item", tile.item));
    near(f32::from(text.center().x), f32::from(body.center().x));
    let said = tree(cx)
        .into_iter()
        .any(|n| n.role == "Status" && n.label.as_deref() == Some("Opening Window 7 on studio…"));
    assert!(said, "past the grace it says what opens where");
}

/// A remote window that did not open ends its wait in its own pane: the opening words go, the
/// pane says what is so and why, the header turns nothing, and no notice repeats it far away.
/// "Choose another window" asks the worker for its windows, and the one picked takes the
/// failed pane's place.
#[gpui::test]
fn a_window_that_did_not_open_says_so_in_its_pane_and_gives_way_to_another(
    cx: &mut TestAppContext,
) {
    use slopty_proto::screen::{CaptureTarget, OpenAsk, ScreenEvent, ScreenFailure, ScreenRequest};

    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let window = slopty_core::WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    let asked = fake.drain().into_iter().any(|m| {
        matches!(m, ClientMsg::Screen(ScreenRequest::Open { target: CaptureTarget::Window(w), .. }) if w == window)
    });
    assert!(asked, "its stream was asked for");
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let asked = OpenAsk::Target(CaptureTarget::Window(window));
        v.screen_event(key, ScreenEvent::OpenFailed { asked, why: ScreenFailure::Gone }, cx);
    });
    cx.executor().advance_clock(LOADING_GRACE);
    cx.run_until_parked();

    assert!(cx.debug_bounds(selector("waiting", tile.item)).is_none(), "no longer opening");
    let failed = bounds(cx, selector("failed", tile.item));
    let body = bounds(cx, selector("item", tile.item));
    near(f32::from(failed.center().x), f32::from(body.center().x));
    let said = tree(cx).into_iter().any(|n| {
        n.role == "Status"
            && n.label.as_deref()
                == Some("Window is no longer available. Window 7 is not open on studio any more.")
    });
    assert!(said, "what is so, and why");
    let status = view.read_with(cx, |v, _| v.item(tile).and_then(|item| v.tile_status(tile, item)));
    assert_eq!(status, None, "the header waits on nothing");
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "said once, in the pane");
    assert!(
        !fake.drain().iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Open { .. }))),
        "not asked again on this link"
    );

    let at = bounds(cx, "choose-another").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    let listed =
        fake.drain().into_iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::List)));
    assert!(listed, "the worker is asked for its windows");
    view.update_in(cx, |v, _w, cx| v.pick_window(slopty_core::WindowId(9), "Notes".to_owned(), cx));
    cx.run_until_parked();
    let ops: Vec<ItemOp> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(op) => Some(op),
            _ => None,
        })
        .collect();
    assert!(
        ops.iter().any(|op| matches!(op, ItemOp::Add(i) if i.kind == ItemKind::Window { window: slopty_core::WindowId(9) })),
        "{ops:?}"
    );
    assert!(ops.contains(&ItemOp::Remove(tile.item)), "the failed pane gives way: {ops:?}");
}

/// A stream the worker ends on its own takes its last picture with it and is asked for again at
/// once. Ended again soon after, it is not asked for again: the pane says it stopped and why,
/// the header waits on nothing, and "Reopen" asks for it once more.
#[gpui::test]
fn a_stream_the_worker_ends_reopens_once_then_offers_to_reopen(cx: &mut TestAppContext) {
    use slopty_proto::screen::{CaptureTarget, ScreenEvent, ScreenRequest, VideoCodec};

    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let window = slopty_core::WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    let asked = |sent: Vec<ClientMsg>| {
        sent.into_iter().filter(|m| {
            matches!(m, ClientMsg::Screen(ScreenRequest::Open { target: CaptureTarget::Window(w), .. }) if *w == window)
        }).count()
    };
    assert_eq!(asked(fake.drain()), 1, "its stream was asked for");
    let key = fake.key;
    let opened = |stream: u32| ScreenEvent::Opened {
        stream: StreamId(stream),
        target: CaptureTarget::Window(window),
        codec: VideoCodec::Hevc,
        width: 1280,
        height: 800,
        scale: 2.0,
        stripes: Vec::new(),
    };
    let closed = |stream: u32| ScreenEvent::Closed {
        stream: StreamId(stream),
        reason: "stream stopped: the display slept".to_owned(),
    };

    view.update_in(cx, |v, _w, cx| v.screen_event(key, opened(1), cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_some()), "streaming");
    view.update_in(cx, |v, _w, cx| v.screen_event(key, closed(1), cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_none()), "its picture went");
    assert_eq!(asked(fake.drain()), 1, "asked for again at once");
    assert!(cx.debug_bounds(selector("stopped", tile.item)).is_none(), "not said to stop");

    view.update_in(cx, |v, _w, cx| v.screen_event(key, opened(2), cx));
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.screen_event(key, closed(2), cx));
    cx.executor().advance_clock(LOADING_GRACE);
    cx.run_until_parked();
    assert_eq!(asked(fake.drain()), 0, "ended again soon after: not asked for again");
    let said = tree(cx).into_iter().any(|n| {
        n.role == "Status"
            && n.label.as_deref()
                == Some(
                    "Window 7 stopped. studio ended its stream: stream stopped: the display slept",
                )
    });
    assert!(said, "the pane says it stopped and why");
    let status = view.read_with(cx, |v, _| v.item(tile).and_then(|item| v.tile_status(tile, item)));
    assert_eq!(status, None, "the header waits on nothing");

    let at = bounds(cx, "reopen-stream").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(asked(fake.drain()), 1, "reopen asks for it once more");
    assert!(cx.debug_bounds(selector("stopped", tile.item)).is_none(), "and waits for it");
}

/// A file with an unsaved edit says "Edited" right after its title's text, a half unit on,
/// before the directory the file is in, and not at the far end of the header.
#[gpui::test]
fn edited_follows_the_title(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.txt";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    paired(&view, cx, &studio, tile, 2);
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
    cx.simulate_input("x");
    cx.run_until_parked();
    let name = bounds(cx, selector("name", tile.item));
    let edited = bounds(cx, selector("unsaved", tile.item));
    let place = bounds(cx, selector("place", tile.item));
    near(f32::from(edited.left() - name.right()), Theme::default().spacing.xs);
    assert!(place.left() > edited.right(), "the directory after them: {place:?} {edited:?}");
}

/// A file in a pane of tabs says "Edited" in its tab, after its name, as a lone tile's header
/// does.
#[gpui::test]
fn a_files_tab_says_edited(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let path = "/w/notes.txt";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 2);
    one_pane(&view, cx, &[shell, tile]);
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
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_none(), "nothing to say yet");
    cx.simulate_input("x");
    cx.run_until_parked();
    let tab = bounds(cx, selector("tab", tile.item));
    let edited = bounds(cx, selector("unsaved", tile.item));
    assert!(tab.left() < edited.left() && edited.right() <= tab.right(), "{tab:?} {edited:?}");
    assert!(tree(cx).iter().any(|n| n.role == "Label" && n.label.as_deref() == Some(tile::EDITED)));
}

/// A Markdown file's header carries the toggle between its preview and its source, which the
/// file opens on the preview; a click swaps the body and the toggle's words.
#[gpui::test]
fn a_markdown_file_s_header_swaps_its_preview_and_source(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let path = "/w/PLAN.md";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    paired(&view, cx, &studio, tile, 2);
    let text = slopty_proto::file::FileRead::Text {
        text: "# Plan\n\n- [ ] ship".to_owned(),
        size: 19,
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
    let toggle = selector("preview", tile.item);
    let said = |cx: &mut VisualTestContext, label: &str| {
        tree(cx).iter().any(|n| n.role == "Button" && n.label.as_deref() == Some(label))
    };
    assert!(said(cx, crate::file::SHOW_SOURCE), "{:#?}", tree(cx));
    assert!(cx.debug_bounds(selector("file-preview", tile.item)).is_some(), "the preview");
    // The toggle is drawn under the pointer, as the header's controls are.
    let header = bounds(cx, selector("title", tile.item)).center();
    cx.simulate_mouse_move(header, None, Modifiers::none());
    cx.run_until_parked();
    let at = bounds(cx, toggle).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert!(said(cx, crate::file::SHOW_PREVIEW));
    assert!(cx.debug_bounds(selector("file-preview", tile.item)).is_none(), "the source");
}

/// A shell whose last command failed leaves the failure to the grid while the grid shows it,
/// washed and barred: the header slot keeps the kind. With the failed rows off screen the
/// slot says it.
#[gpui::test]
fn a_failure_the_grid_shows_leaves_the_header_slot_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (shown, away) = (SessionId::new(), SessionId::new());
    let shown_tile = opens(&view, cx, &fake, shown, fake.me, 1);
    let away_tile = opens(&view, cx, &fake, away, fake.me, 2);
    let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
    view.update_in(cx, |v, _w, cx| {
        let rows = [("$ false", prompt(None)), ("$ ", prompt(Some(1)))];
        v.term_event(shown, marked_frame(1, &rows, 1), cx);
        v.term_event(away, marked_frame(1, &[("$ ", prompt(Some(1)))], 0), cx);
    });
    cx.run_until_parked();
    let status = |cx: &mut VisualTestContext, tile: TileRef| {
        view.read_with(cx, |v, _| v.item(tile).and_then(|i| v.tile_status(tile, i)))
    };
    assert_eq!(status(cx, shown_tile), None, "the grid shows `false` failing");
    assert_eq!(status(cx, away_tile), Some(Status::Failed), "its rows are off screen");
}

/// A double-click on the sash between two panes makes their shares equal again, after a drag
/// had made the one above taller.
#[gpui::test]
fn a_double_click_on_a_sash_makes_its_panes_equal(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), ..] = three_shells(&view, cx, &fake);
    let height = |cx: &mut VisualTestContext| {
        f32::from(bounds(cx, selector("item", first.item)).size.height)
    };
    let opened = height(cx);
    let grab = bounds(cx, "sash--0").center();
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    let to = point(grab.x, grab.y + px(40.0));
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    near(height(cx), opened + 40.0);

    let at = bounds(cx, "sash--0").center();
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
    near(height(cx), opened);
}

/// Under the touch density a tile's header is a finger's 44 pt, its body starts under it, and
/// the area's hit test puts the header's edge in the same place.
#[gpui::test]
fn the_header_and_its_hit_test_follow_the_density(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let theme = Theme { density: slopty_theme::Density::TOUCH, ..Theme::default() };
    view.update(cx, |v, cx| v.set_theme(theme, cx));
    let fake = connect(&view, cx, 1, "studio");
    let tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    // Company that draws no grid, so the shell's is the one there is.
    let window =
        arrives(&view, cx, &fake, ItemKind::Window { window: slopty_core::WindowId(7) }, 2);
    beside(&view, cx, window, tile, slopty_client::layout::Side::Right);
    let header = bounds(cx, selector("title", tile.item));
    near(f32::from(header.size.height), 44.0);
    let grid = bounds(cx, "terminal");
    near(f32::from(grid.top()), f32::from(header.bottom()));
    let item = bounds(cx, selector("item", tile.item));
    near(f32::from(grid.size.height), f32::from(item.size.height) - 44.0);
    let x = header.center().x;
    let at = |y: Pixels| view.read_with(cx, |v, _| v.under(point(x, y)));
    assert_eq!(at(header.bottom() - px(1.0)), Some((tile, false)), "still the header");
    assert_eq!(at(header.bottom() + px(1.0)), Some((tile, true)), "the body");
}

/// A window refused for want of Screen Recording offers no other window, which would be
/// refused the same way: its pane offers "Restart its worker", the restart a running worker
/// needs to see a grant made at its desk, sent to the server on the press.
#[gpui::test]
fn a_window_refused_for_screen_recording_offers_the_restart(cx: &mut TestAppContext) {
    use slopty_proto::orchestration::{Outcome, Verb};
    use slopty_proto::screen::{CaptureTarget, OpenAsk, ScreenEvent, ScreenFailure};

    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let window = slopty_core::WindowId(7);
    let _tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        let asked = OpenAsk::Target(CaptureTarget::Window(window));
        v.screen_event(
            key,
            ScreenEvent::OpenFailed { asked, why: ScreenFailure::NotPermitted },
            cx,
        );
    });
    cx.executor().advance_clock(LOADING_GRACE);
    cx.run_until_parked();
    assert!(cx.debug_bounds("choose-another").is_none(), "another window would fail the same");
    let at = bounds(cx, "restart-worker").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    let (verb, reply) = queue.try_next().expect("the server is asked");
    let _gone = reply.send(Outcome::Done);
    let worker = crate::workspace::projects::worker_id(key).expect("a server id");
    assert_eq!(verb, Verb::RestartWorker { worker });
}
