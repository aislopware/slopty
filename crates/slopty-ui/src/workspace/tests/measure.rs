//! What one workspace frame costs with many items, few of them on screen: the per-frame work
//! that grows with the registry rather than with what is drawn. Run by hand (it prints, it does
//! not judge); `docs/MEASUREMENTS.md` has the numbers and the command.

use std::time::Instant;

use super::*;

/// Notes, file tiles and shells on one worker; a note's text is this many bytes.
const NOTES: usize = 120;
const FILES: usize = 60;
const SHELLS: usize = 12;
const NOTE_BYTES: usize = 4096;
/// Frames drawn for the numbers, after a few to warm the caches.
const FRAMES: usize = 400;
const WARM: usize = 20;

#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_a_frame_over_a_large_registry(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let line = "- [ ] a line of a long note, to be cloned or not\n";
    let text = line.repeat(NOTE_BYTES.checked_div(line.len()).unwrap_or(1));
    let mut items: Vec<Item> = Vec::new();
    let mut sessions = Vec::new();
    for _ in 0..NOTES {
        let kind = ItemKind::Note { text: text.clone() };
        items.push(Item { id: ItemId::new(), kind, name: None, facts: BTreeMap::new() });
    }
    for n in 0..FILES {
        let kind = ItemKind::File { path: format!("/w/src/file_{n}.rs") };
        items.push(Item { id: ItemId::new(), kind, name: None, facts: BTreeMap::new() });
    }
    for _ in 0..SHELLS {
        let session = SessionId::new();
        sessions.push(summary(session, Some("/w")));
        items.push(Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: BTreeMap::new(),
        });
    }
    let key = studio.key;
    let shell = sessions.first().map(|s| s.id).expect("a shell");
    view.update_in(cx, |v, _window, cx| {
        for s in sessions {
            v.session_opened(key, s, cx);
        }
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    let own = time(cx, |cx| view.update(cx, |_, cx| cx.notify()));
    // A shell's echo, the shell focused: the workspace is drawn again as the terminal's parent,
    // the chrome is not.
    let tile = view.read_with(cx, |v, _| v.tile_of_session(shell)).expect("the shell's tile");
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let terminal = view.read_with(cx, |v, _| v.terminal(shell).cloned()).expect("attached");
    let echo = time(cx, |cx| terminal.update(cx, |_, cx| cx.notify()));
    println!(
        "MEASURE workspace frame, {NOTES} notes × {NOTE_BYTES} B, {FILES} files, {SHELLS} shells, \
         {FRAMES} frames: {own}; echo {echo}"
    );
}

/// A few long notes of task lines beside a shell, all near the view: what a frame another tile
/// causes (a shell's echo) costs in the notes' headers.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_an_echo_frame_beside_long_notes(cx: &mut TestAppContext) {
    const LONG_NOTES: usize = 3;
    const LONG_BYTES: usize = 64 * 1024;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let lines = ["- [ ] a task in a long note\n", "- [x] one done\n", "some prose between\n"];
    let mut text = String::new();
    for line in lines.iter().cycle() {
        if text.len().saturating_add(line.len()) > LONG_BYTES {
            break;
        }
        text.push_str(line);
    }
    let notes: Vec<Item> = std::iter::repeat_with(|| Item {
        id: ItemId::new(),
        kind: ItemKind::Note { text: text.clone() },
        name: None,
        facts: BTreeMap::new(),
    })
    .take(LONG_NOTES)
    .collect();
    let session = SessionId::new();
    let shell = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        name: None,
        facts: BTreeMap::new(),
    };
    let ids: Vec<ItemId> = notes.iter().map(|n| n.id).collect();
    let key = studio.key;
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary(session, Some("/w")), cx);
        let items = notes.into_iter().chain([shell]).collect();
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    let tile = view.read_with(cx, |v, _| v.tile_of_session(session)).expect("the shell's tile");
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let near = ids.iter().filter(|id| cx.debug_bounds(selector("item", **id)).is_some()).count();
    assert!(near >= 1, "a note is drawn beside the shell: {near}");
    let terminal = view.read_with(cx, |v, _| v.terminal(session).cloned()).expect("attached");
    let echo = time(cx, |cx| terminal.update(cx, |_, cx| cx.notify()));
    println!(
        "MEASURE echo frame beside {near} drawn notes of {LONG_BYTES} B, {FRAMES} frames: {echo}"
    );
}

/// A conversation face with a transcript in it, focused (its composer has the keyboard), beside
/// a shell: what a frame the shell's echo causes costs, and how often it draws the face.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_an_echo_frame_beside_a_focused_face(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let busy = SessionId::new();
    opens(&view, cx, &studio, busy, studio.me, 1);
    let agent = SessionId::new();
    let tile = opens(&view, cx, &studio, agent, studio.me, 2);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(
            AgentEvent { status: AgentStatus::Idle, attention: false, ..blocked(agent) },
            cx,
        );
        v.focus_tile(tile, cx);
        v.show_face(agent, true, cx);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        for event in crate::conversation::fixtures::events("tools") {
            v.conversation_event(agent, event, cx);
        }
    });
    cx.run_until_parked();
    let face = view.read_with(cx, |v, _| v.conversation(agent).cloned()).expect("a face");
    let composing = cx.update(|window, cx| face.read(cx).focus_handle(cx).is_focused(window));
    assert!(composing, "the composer has the keyboard");
    let terminal = view.read_with(cx, |v, _| v.terminal(busy).cloned()).expect("attached");
    let before = face.read_with(cx, |f, _| f.renders());
    let echo = time(cx, |cx| terminal.update(cx, |_, cx| cx.notify()));
    let drawn = face.read_with(cx, |f, _| f.renders()).saturating_sub(before);
    println!(
        "MEASURE echo frame beside a focused face, {FRAMES} frames: {echo}; face rendered \
         {drawn} times"
    );
}

/// `FRAMES` frames each caused by `frame`, after `WARM`: mean, p50, p95 and max, in ms.
fn time(cx: &mut VisualTestContext, frame: impl Fn(&mut VisualTestContext)) -> String {
    let mut took = Vec::with_capacity(FRAMES);
    for n in 0..WARM + FRAMES {
        let start = Instant::now();
        frame(cx);
        cx.run_until_parked();
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p);
    let mean =
        took.iter().sum::<Duration>().checked_div(u32::try_from(took.len()).unwrap()).unwrap();
    format!(
        "mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        mean.as_secs_f64() * 1e3,
        pct(50).as_secs_f64() * 1e3,
        pct(95).as_secs_f64() * 1e3,
        took.last().copied().unwrap_or_default().as_secs_f64() * 1e3,
    )
}

/// A remote window's frame beside the chrome: 60 shells and 60 notes, the navigator docked, the
/// window focused with a picture up and the last shell drawn beside it. Each stream frame is a
/// `notify` of its view, as the pump sends one, and GPUI marks every ancestor view dirty with
/// it, so the workspace root renders for every frame of video. This times that frame, counts
/// the root's renders and the view's own, and times the shell's echo and the workspace's own
/// frame in the same crowd beside it.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_a_stream_frame_beside_the_chrome(cx: &mut TestAppContext) {
    const CROWD_SHELLS: usize = 60;
    const CROWD_NOTES: usize = 60;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let mut items: Vec<Item> = Vec::new();
    let mut sessions = Vec::new();
    for n in 0..CROWD_SHELLS {
        let session = SessionId::new();
        sessions.push(summary(session, Some(&format!("/Users/me/src/project_{n}"))));
        items.push(Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: BTreeMap::new(),
        });
    }
    // The window sits between the shells and the notes, so the last shell is drawn beside it.
    let window = slopty_core::WindowId(7);
    let streamed = ItemId::new();
    items.push(Item {
        id: streamed,
        kind: ItemKind::Window { window },
        name: None,
        facts: BTreeMap::new(),
    });
    for _ in 0..CROWD_NOTES {
        let kind = ItemKind::Note { text: "a note\n".into() };
        items.push(Item { id: ItemId::new(), kind, name: None, facts: BTreeMap::new() });
    }
    let shell = sessions.last().map(|s| s.id).expect("a shell");
    let key = studio.key;
    view.update_in(cx, |v, _window, cx| {
        for s in sessions {
            v.session_opened(key, s, cx);
        }
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    let tile = TileRef { worker: key, item: streamed };
    view.update_in(cx, |v, _w, cx| {
        v.screen_event(
            key,
            ScreenEvent::Opened {
                stream: StreamId(1),
                target: CaptureTarget::Window(window),
                codec: slopty_proto::screen::VideoCodec::Hevc,
                width: 2560,
                height: 1600,
                scale: 2.0,
                stripes: Vec::new(),
            },
            cx,
        );
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_some());
    let shell_tile = view.read_with(cx, |v, _| v.tile_of_session(shell)).expect("the shell's tile");
    assert!(cx.debug_bounds(selector("item", shell_tile.item)).is_some(), "the shell is drawn");
    let screen = view.read_with(cx, |v, _| v.screen(tile.item).cloned()).expect("streaming");
    let picture = core_video::pixel_buffer::CVPixelBuffer::new(
        core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        2560,
        1600,
        None,
    )
    .expect("pixel buffer");
    screen.update(cx, |s, cx| s.show_picture(picture, cx));
    cx.run_until_parked();
    let roots = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.drawn.builds.get());
    let screens = |cx: &VisualTestContext| screen.read_with(cx, |s, _| s.renders());
    let (root_before, screen_before) = (roots(cx), screens(cx));
    let stream = time(cx, |cx| screen.update(cx, |_, cx| cx.notify()));
    let root_renders = roots(cx).wrapping_sub(root_before);
    let screen_renders = screens(cx).wrapping_sub(screen_before);
    let terminal = view.read_with(cx, |v, _| v.terminal(shell).cloned()).expect("attached");
    let echo = time(cx, |cx| terminal.update(cx, |_, cx| cx.notify()));
    let own = time(cx, |cx| view.update(cx, |_, cx| cx.notify()));
    println!(
        "MEASURE stream frame beside the chrome, {CROWD_SHELLS} shells + {CROWD_NOTES} notes, \
         {FRAMES} frames after {WARM}: stream {stream}; the root rendered {root_renders} and \
         the view {screen_renders} times; echo {echo}; workspace {own}"
    );
}
