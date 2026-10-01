//! A checkpoint carries the session's state: what the session tells its viewers (the title, the
//! directory, the colours, the pointer shape, the progress), what the frames carry (the modes,
//! the cursor's position, shape, blink and visibility), what shapes input (the mouse and key
//! modes, the kitty keyboard flags), and what only shows once the program goes on writing (the
//! pen, an open hyperlink, the character sets, the saved cursor, the kitty keyboard stack,
//! protected cells, a pending wrap, the margins and the tab stops).

use pretty_assertions::assert_eq;
use slopty_proto::input::CellMetrics;

use super::*;

fn engine(cols: u16, rows: u16) -> GhosttyEngine {
    GhosttyEngine::new(EngineConfig {
        size: TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } },
        scrollback_lines: 100,
    })
    .unwrap()
}

/// What a fresh engine replays a checkpoint of `a` into.
fn replayed(a: &mut GhosttyEngine) -> GhosttyEngine {
    let mut state = Vec::new();
    a.checkpoint(&mut state).unwrap();
    let mut b = engine(a.size.cols, a.size.rows);
    b.write(&state);
    b
}

/// The session's state as the worker keeps it from the engine's events: the last of each.
#[derive(Debug, PartialEq, Default)]
struct Session {
    title: Option<String>,
    cwd: Option<String>,
    colors: ColorOverrides,
    pointer: PointerShape,
    progress: Progress,
}

impl Session {
    fn fold(&mut self, e: &GhosttyEngine) {
        for ev in e.drain_events() {
            match ev {
                EngineEvent::Title(t) => self.title = Some(t).filter(|t| !t.is_empty()),
                EngineEvent::Cwd(c) => self.cwd = Some(c),
                EngineEvent::Colors(c) => self.colors = c,
                EngineEvent::Pointer(p) => self.pointer = p,
                EngineEvent::Progress(p) => self.progress = p,
                _ => {}
            }
        }
    }
}

/// What a frame and the input encoders show of an engine.
#[derive(Debug, PartialEq)]
struct Seen {
    modes: TermModes,
    cursor: Cursor,
    keys: Vec<Vec<u8>>,
    mouse: Vec<u8>,
    focus: Vec<u8>,
    paste: Vec<u8>,
}

fn key(code: KeyCode, text: Option<&str>, mods: Mods) -> KeyEvent {
    KeyEvent {
        seq: 1,
        action: KeyAction::Press,
        code,
        mods,
        consumed_mods: Mods::empty(),
        text: text.map(str::to_owned),
        unshifted: None,
        composing: false,
        option_as_alt: false,
    }
}

fn seen(e: &mut GhosttyEngine) -> Seen {
    let frame = e.full_frame(0).unwrap();
    let keys = [
        key(KeyCode::A, Some("a"), Mods::empty()),
        key(KeyCode::A, Some("a"), Mods::CTRL),
        key(KeyCode::A, Some("a"), Mods::ALT),
        key(KeyCode::Enter, Some("\r"), Mods::SHIFT),
        key(KeyCode::Escape, None, Mods::empty()),
        key(KeyCode::ArrowUp, None, Mods::empty()),
        key(KeyCode::F1, None, Mods::empty()),
        key(KeyCode::Numpad1, Some("1"), Mods::empty()),
        key(KeyCode::NumpadEnter, Some("\r"), Mods::empty()),
        key(KeyCode::Backspace, None, Mods::empty()),
    ]
    .iter()
    .map(|k| {
        let mut out = Vec::new();
        e.encode_key(k, &mut out).unwrap();
        out
    })
    .collect();
    // Past column 95, where the X10 and UTF-8 mouse formats differ from SGR's.
    let at = |action, button| MouseEvent {
        action,
        button,
        mods: Mods::empty(),
        col: 200,
        row: 1,
        px: 1600,
        py: 20,
    };
    let mut mouse = Vec::new();
    let left = Some(MouseButton::Left);
    for (action, button) in [
        (MouseAction::Motion, None),
        (MouseAction::Press, left),
        (MouseAction::Motion, left),
        (MouseAction::Release, left),
        (MouseAction::Wheel { rows: 1, cols: 0 }, None),
    ] {
        e.encode_mouse(&at(action, button), &mut mouse).unwrap();
    }
    let mut focus = Vec::new();
    e.encode_focus(true, &mut focus).unwrap();
    let mut paste = Vec::new();
    e.encode_paste("x", &mut paste).unwrap();
    Seen { modes: frame.modes, cursor: frame.cursor, keys, mouse, focus, paste }
}

/// Each sequence's state is reported by a fresh engine a checkpoint replays into, and shows in
/// its frames and its input encoding as it does in the engine checkpointed.
#[test]
fn a_checkpoint_carries_the_state_the_session_reports_and_shows() {
    let cases: &[(&str, &[u8])] = &[
        ("title", b"\x1b]2;my title\x07"),
        ("icon and title", b"\x1b]0;icon and title\x07"),
        ("directory", b"\x1b]7;file://host/tmp/dir\x07"),
        ("pointer", b"\x1b]22;pointer\x07"),
        ("progress", b"\x1b]9;4;1;40\x07"),
        ("progress error, no value", b"\x1b]9;4;2\x07"),
        ("progress indeterminate", b"\x1b]9;4;3\x07"),
        ("progress paused", b"\x1b]9;4;1;70\x07\x1b]9;4;4\x07"),
        ("palette", b"\x1b]4;1;#123456\x07"),
        ("foreground and background", b"\x1b]10;#010203\x07\x1b]11;#040506\x07"),
        ("bracketed paste", b"\x1b[?2004h"),
        ("mouse 1000, SGR", b"\x1b[?1000h\x1b[?1006h"),
        ("mouse 1002", b"\x1b[?1002h"),
        ("mouse 1003, SGR pixels", b"\x1b[?1003h\x1b[?1016h"),
        ("mouse X10", b"\x1b[?9h"),
        ("mouse UTF-8", b"\x1b[?1000h\x1b[?1005h"),
        ("mouse urxvt", b"\x1b[?1000h\x1b[?1015h"),
        ("focus reports", b"\x1b[?1004h"),
        ("blinking bar", b"\x1b[5 q"),
        ("steady block", b"\x1b[2 q"),
        ("blinking underline", b"\x1b[3 q"),
        ("steady bar, then blink on", b"\x1b[6 q\x1b[?12h"),
        ("cursor hidden", b"\x1b[?25l"),
        ("cursor blink mode", b"\x1b[?12h"),
        ("kitty flags", b"\x1b[>1u"),
        ("every kitty flag", b"\x1b[>31u"),
        ("kitty flags set", b"\x1b[=5;1u"),
        ("modifyOtherKeys", b"\x1b[>4;2m"),
        ("application cursor keys", b"\x1b[?1h"),
        ("application keypad", b"\x1b="),
        ("backarrow sends backspace", b"\x1b[?67h"),
        ("alternate scroll off", b"\x1b[?1049h\x1b[?1007l"),
        ("alternate screen", b"\x1b[?1049hon alt"),
        ("a shape on the alternate screen", b"\x1b[6 q\x1b[?1049h\x1b[0 q"),
        ("in-band resize reports", b"\x1b[?2048h"),
        ("colour scheme reports", b"\x1b[?2031h"),
    ];
    for &(name, bytes) in cases {
        let mut a = engine(20, 4);
        a.write(b"hi\r\n");
        a.write(bytes);
        let mut session = Session::default();
        session.fold(&a);
        let mut b = replayed(&mut a);
        let mut replay = Session::default();
        replay.fold(&b);
        assert_eq!(replay, session, "{name}: the session's state");
        assert_eq!(seen(&mut b), seen(&mut a), "{name}: the frame and the input encoding");
    }
}

/// After a checkpoint, a fresh engine it replays into goes on as the engine checkpointed: the
/// same bytes draw the same lines and report the same state.
#[test]
fn a_replay_goes_on_as_the_engine_checkpointed() {
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("an open hyperlink", b"\x1b]8;;http://x\x07ab", b"cd"),
        ("the pen", b"\x1b[1;31;4mab", b"cd"),
        ("G0 designated", b"\x1b(0", b"qx"),
        ("G1 shifted in", b"\x1b)0\x0e", b"qx"),
        ("a saved position", b"\x1b[3;5H\x1b7\x1b[H", b"\x1b8X"),
        ("a saved pen", b"\x1b[31m\x1b7\x1b[0m", b"\x1b8X"),
        ("a saved character set", b"\x1b(0\x1b7\x1b(B", b"\x1b8q"),
        ("a saved pending wrap", b"\x1b[2;1H01234567890123456789\x1b7\x1b[H", b"\x1b8Y"),
        ("a saved origin", b"\x1b[2;3r\x1b[?6h\x1b[2;2H\x1b7\x1b[?6l\x1b[H", b"\x1b8\x1b[HX"),
        ("insert mode", b"abc\x1b[4h\x1b[1G", b"Z"),
        ("no autowrap", b"\x1b[?7l", b"0123456789012345678901234"),
        ("origin mode in a region", b"\x1b[2;3r\x1b[?6h", b"\x1b[HX"),
        ("a region", b"\x1b[2;3r\x1b[3;1H", b"a\nb\nc\nd"),
        ("left and right margins", b"\x1b[?69h\x1b[3;8s\x1b[1;3H", b"0123456789"),
        ("a pending wrap at the right margin", b"\x1b[?69h\x1b[1;6s\x1b[2;1H123456", b"Y"),
        ("the kitty keyboard stack", b"\x1b[>1u\x1b[>3u", b"\x1b[<u"),
        ("a title pushed", b"\x1b]2;one\x07\x1b[22t\x1b]2;two\x07", b"\x1b[23t"),
        ("protected cells", b"\x1b[1\"qab\x1b[0\"q", b"\x1b[1G\x1b[?2K"),
        ("tab stops", b"\x1b[3g\x1b[5G\x1bH\x1b[1G", b"\tX"),
        ("newline mode", b"\x1b[20h", b"a\nb"),
        ("a pending wrap", b"\x1b[2;1H01234567890123456789", b"Y"),
        ("a pending wrap in a region", b"\x1b[1;2r\x1b[2;1H01234567890123456789", b"Y"),
        (
            "a pending wrap under origin mode",
            b"\x1b[2;3r\x1b[?6h\x1b[2;1H01234567890123456789",
            b"Y",
        ),
        ("the alternate screen and back", b"top\x1b[?1049hALT", b"\x1b[?1049l"),
        ("the alternate screen by 47 and back", b"top\x1b[?47hALT", b"\x1b[?47l"),
        ("grapheme clustering", b"\x1b[?2027h", "\u{1f468}\u{200d}\u{1f469}x".as_bytes()),
        ("reverse wrap", b"\x1b[?45h\x1b[2;1H", b"\x08\x08X"),
    ];
    let text = |e: &mut GhosttyEngine| -> Vec<String> {
        e.full_frame(0).unwrap().updates.iter().map(|u| u.line.text()).collect()
    };
    let lines = |e: &mut GhosttyEngine| -> Vec<Line> {
        e.full_frame(0).unwrap().updates.iter().map(|u| Line::clone(&u.line)).collect()
    };
    for &(name, bytes, then) in cases {
        let mut a = engine(20, 4);
        a.write(b"hi\r\n");
        a.write(bytes);
        let mut b = replayed(&mut a);
        let (mut session, mut replay) = (Session::default(), Session::default());
        session.fold(&a);
        replay.fold(&b);
        a.write(then);
        b.write(then);
        session.fold(&a);
        replay.fold(&b);
        assert_eq!(text(&mut b), text(&mut a), "{name}: the text");
        assert_eq!(lines(&mut b), lines(&mut a), "{name}: the lines");
        assert_eq!(replay, session, "{name}: the session's state");
        assert_eq!(seen(&mut b), seen(&mut a), "{name}: the frame and the input encoding");
    }
}

/// The pointer shape a program asked for comes back from a checkpoint, by every name, so a
/// worker that restarts tells its viewers the shape the program still wants.
#[test]
fn the_pointer_shape_comes_back_from_a_checkpoint() {
    use PointerShape as P;
    let every = [
        P::Default,
        P::ContextMenu,
        P::Help,
        P::Pointer,
        P::Progress,
        P::Wait,
        P::Cell,
        P::Crosshair,
        P::VerticalText,
        P::Alias,
        P::Copy,
        P::Move,
        P::NoDrop,
        P::NotAllowed,
        P::Grab,
        P::Grabbing,
        P::AllScroll,
        P::ColResize,
        P::RowResize,
        P::NResize,
        P::EResize,
        P::SResize,
        P::WResize,
        P::NeResize,
        P::NwResize,
        P::SeResize,
        P::SwResize,
        P::EwResize,
        P::NsResize,
        P::NeswResize,
        P::NwseResize,
        P::ZoomIn,
        P::ZoomOut,
    ];
    for shape in every {
        let mut a = engine(10, 3);
        a.write(format!("\x1b]22;{}\x1b\\", convert::pointer_name(shape)).as_bytes());
        assert_eq!(a.pointer, shape, "{shape:?} is read back by its name");
        let b = replayed(&mut a);
        assert_eq!(b.pointer, shape, "{shape:?} after a checkpoint");
        let mut replay = Session::default();
        replay.fold(&b);
        assert_eq!(replay.pointer, shape, "{shape:?} is reported by the replay");
    }
    let mut text = engine(10, 3);
    text.write(b"\x1b]22;pointer\x07\x1b]22;\x07");
    let mut state = Vec::new();
    text.checkpoint(&mut state).unwrap();
    assert!(memchr::memmem::find(&state, b"\x1b]22;").is_none(), "the default is not written");
}

/// A full reset clears libghostty's title without a callback: the session hears of it. Its
/// directory is the shell's and stays, in the session and in a checkpoint.
#[test]
fn a_full_reset_clears_the_title_and_keeps_the_directory() {
    let mut a = engine(10, 3);
    a.write(b"\x1b]2;t\x07\x1b]7;file://host/tmp\x07");
    let mut session = Session::default();
    session.fold(&a);
    a.write(b"\x1bc");
    let events = a.drain_events();
    assert!(events.contains(&EngineEvent::Title(String::new())), "{events:?}");
    assert!(!events.iter().any(|e| matches!(e, EngineEvent::Cwd(_))), "{events:?}");
    let b = replayed(&mut a);
    let mut replay = Session::default();
    replay.fold(&b);
    assert_eq!(replay, Session { title: None, ..session }, "the replay's state");

    let mut untitled = engine(10, 3);
    untitled.write(b"\x1bc");
    assert!(
        !untitled.drain_events().iter().any(|e| matches!(e, EngineEvent::Title(_))),
        "no title, nothing to clear"
    );
}
