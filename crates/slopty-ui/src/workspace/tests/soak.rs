//! The app soak: a tile of every kind opened and closed many times over, with the leak check of
//! `leaks` after the last. Every view a closed tile had is released, GPUI holds no entity made
//! since the first cycle and no more callbacks than after it, and every map and list of the
//! workspace is back to its length before.
//! A leak a single close hides (a handle kept per close, an entry per reopen) shows as growth.
//!
//! [`CYCLES_ENV`] sets how many cycles run: a few in the gate, many at night.

use std::cell::{Cell, RefCell};

use gpui::AnyWeakEntity;
use slopty_proto::file::FileRead;

use super::leaks::{close, closes_clean_times, next, studio};
use super::*;
use crate::conversation::fixtures;

/// How many times the soak opens and closes every kind; unset, [`GATE_CYCLES`].
const CYCLES_ENV: &str = "SLOPTY_SOAK_CYCLES";

/// Enough for the gate to see a leak per close as growth, cheap enough to run on every change.
const GATE_CYCLES: usize = 3;

fn cycles() -> usize {
    std::env::var(CYCLES_ENV).ok().and_then(|n| n.parse().ok()).unwrap_or(GATE_CYCLES)
}

/// Views a cycle holds on to: a terminal, an agent, its face, a file, a folder, a note, a
/// screen and a page.
const KINDS: usize = 8;

/// The views the closed tiles had, by kind, to be asked whether any is still held.
type Held = RefCell<Vec<(&'static str, AnyWeakEntity)>>;

fn hold<T: 'static>(held: &Held, kind: &'static str, view: Option<&Entity<T>>) {
    let view = view.unwrap_or_else(|| panic!("the {kind} tile has a view"));
    held.borrow_mut().push((kind, view.downgrade().into()));
}

/// A shell with a screen of output, and an agent's shell with its conversation face shown and
/// fed, each closed and ended.
fn shells(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    version: &Cell<u64>,
    held: &Held,
) {
    let session = SessionId::new();
    let tile = opens(view, cx, fake, session, fake.me, next(version));
    view.update_in(cx, |v, _w, cx| v.term_event(session, frame(&["~ % ls", "a b"]), cx));
    view.read_with(cx, |v, _| hold(held, "terminal", v.terminals.get(&session)));
    close(view, cx, tile);

    let agent = SessionId::new();
    let tile = opens(view, cx, fake, agent, fake.me, next(version));
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(agent) }, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        for event in fixtures::events("edit") {
            v.conversation_event(agent, event, cx);
        }
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        hold(held, "agent", v.terminals.get(&agent));
        hold(held, "conversation", v.conversation(agent));
    });
    close(view, cx, tile);

    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        v.session_closed(session, cx);
        v.session_closed(agent, cx);
    });
}

/// A file with an edit not saved, a folder, a note, a streaming window and a page, each closed.
fn items(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    version: &Cell<u64>,
    held: &Held,
) {
    let key = fake.key;
    let path = format!("/w/notes-{}.md", version.get());
    let tile = arrives(view, cx, fake, ItemKind::File { path: path.clone() }, next(version));
    let text = FileRead::Text {
        text: "# Notes".to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
    };
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, &path, &text, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.simulate_input("x");
    cx.run_until_parked();
    view.read_with(cx, |v, _| hold(held, "file", v.files.get(&tile.item)));
    close(view, cx, tile);

    let tile = arrives(view, cx, fake, ItemKind::Folder { path: "/w/proj".into() }, next(version));
    view.read_with(cx, |v, _| hold(held, "folder", v.folders.get(&tile.item)));
    close(view, cx, tile);

    let note = ItemKind::Note { text: "Release\n- [ ] tag".into() };
    let tile = arrives(view, cx, fake, note, next(version));
    view.read_with(cx, |v, _| hold(held, "note", v.notes.get(&tile.item)));
    close(view, cx, tile);

    let n = next(version);
    let id = u32::try_from(n).unwrap();
    let window = slopty_core::WindowId(id);
    let tile = arrives(view, cx, fake, ItemKind::Window { window }, n);
    view.update_in(cx, |v, _w, cx| {
        let opened = ScreenEvent::Opened {
            stream: StreamId(id),
            target: CaptureTarget::Window(window),
            codec: slopty_proto::screen::VideoCodec::Hevc,
            width: 1280,
            height: 800,
            scale: 2.0,
            stripes: Vec::new(),
        };
        v.screen_event(key, opened, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| hold(held, "screen", v.screen(tile.item)));
    close(view, cx, tile);

    let page = ItemKind::Browser { url: format!("http://127.0.0.1:5173/{}", version.get()) };
    let tile = arrives(view, cx, fake, page, next(version));
    view.read_with(cx, |v, _| hold(held, "browser", v.browsers.get(&tile.item)));
    close(view, cx, tile);
}

/// Every tile kind opened, used and closed [`cycles`] times over: nothing any of them made is
/// still held after, and the workspace keeps no entry for them.
#[gpui::test]
fn every_tile_kind_opened_and_closed_many_times_leaves_nothing(cx: &mut TestAppContext) {
    let (view, cx, mut fake) = studio(cx);
    let version = Cell::new(1);
    let held = Held::default();
    let mut said = Vec::new();
    closes_clean_times(&view, cx, cycles(), |cx| {
        shells(&view, cx, &fake, &version, &held);
        items(&view, cx, &fake, &version, &held);
        // What a link would have sent the worker: after the first cycle, which asks for what is
        // asked once per app, the same each cycle, or a request (a watch, a follow) is being
        // made again and never taken back.
        said.push(fake.drain().len());
    });
    let steady = said.get(1..).unwrap_or_default();
    assert!(steady.windows(2).all(|w| w[0] == w[1]), "told the worker per cycle: {said:?}");
    let held = held.into_inner();
    let opened = KINDS.checked_mul(cycles().saturating_add(1));
    assert_eq!(Some(held.len()), opened, "every kind was opened each cycle");
    let kept: Vec<&str> =
        held.iter().filter(|(_, view)| view.upgrade().is_some()).map(|(kind, _)| *kind).collect();
    assert!(kept.is_empty(), "closed tiles' views still held: {kept:?}");
}
