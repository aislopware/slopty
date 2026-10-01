use slopty_proto::drag::DragOp;
use slopty_proto::transfer::ClipFormat;

use super::*;

const DIR: &str = "/Users/w/.slopty/drag/d";

fn moved(x: f32, y: f32) -> ScreenInput {
    ScreenInput::Move { x, y }
}

fn left(down: bool) -> ScreenInput {
    ScreenInput::Button {
        button: MouseButton::Left,
        down,
        x: 0.0,
        y: 0.0,
        clicks: 1,
        mods: Mods::empty(),
    }
}

/// A drag out begun at (10, 20) and carried to the tile's edge at (2, 30).
fn out() -> DragOut {
    let (mut out, acts) =
        DragOut::began(DragId::new(), (10.0, 20.0), Vec::new(), PathBuf::from(DIR));
    assert!(matches!(acts[..], [OutAct::Tell(DragEvent::OutBegan { .. })]), "{acts:?}");
    out.hear(OutHeard::Moved { x: 2.0, y: 30.0 });
    out
}

/// Caught onto the catcher: located, placed and reached.
fn reached() -> DragOut {
    let mut out = out();
    let drag = out.drag();
    out.hear(OutHeard::Catch);
    out.hear(OutHeard::Located(Ok((402.0, 330.0))));
    out.hear(OutHeard::Helper(FromHelper::Ready { drag }));
    out
}

/// A press marks the drag pasteboard's count, the first move with the button held arms the
/// watch and the release disarms it; moves with nothing held, another button and a release
/// with nothing held ask nothing.
#[test]
fn a_drag_pasteboard_change_is_watched_only_while_the_left_button_is_held() {
    let mut presses = Presses::default();
    let middle = ScreenInput::Button {
        button: MouseButton::Middle,
        down: true,
        x: 0.0,
        y: 0.0,
        clicks: 1,
        mods: Mods::empty(),
    };
    let asked: Vec<Option<Watching>> = [
        moved(1.0, 1.0),
        left(false),
        middle,
        left(true),
        moved(2.0, 2.0),
        moved(3.0, 3.0),
        left(false),
        moved(4.0, 4.0),
    ]
    .iter()
    .map(|input| presses.input(input))
    .collect();
    assert_eq!(
        asked,
        [
            None,
            None,
            None,
            Some(Watching::Mark),
            Some(Watching::Arm),
            None,
            Some(Watching::Disarm),
            None
        ]
    );
    assert!(!presses.held());
}

/// The client's leaving puts the catcher under the real pointer where the drag last was, then
/// carries the drag out and back over it, again until the catcher says it is over it, and only
/// then lets go there.
#[test]
fn catch_wiggles_over_the_catcher_and_lets_go() {
    let mut out = out();
    let drag = out.drag();
    assert_eq!(
        out.hear(OutHeard::Catch),
        [OutAct::Locate { x: 2.0, y: 30.0 }, OutAct::Deadline(Some(START_WAIT))]
    );
    assert_eq!(out.hear(OutHeard::Moved { x: 50.0, y: 50.0 }), [], "off the tile by now");
    assert_eq!(
        out.hear(OutHeard::Located(Ok((402.0, 330.0)))),
        [
            OutAct::Helper(ToHelper::CatcherAt { drag, x: 402.0, y: 330.0, dir: DIR.to_owned() }),
            OutAct::Deadline(Some(START_WAIT)),
        ]
    );
    let wiggle = [
        OutAct::Input(moved(6.0, 30.0)),
        OutAct::Input(moved(2.0, 30.0)),
        OutAct::Deadline(Some(REACH_WAIT)),
    ];
    assert_eq!(out.hear(OutHeard::Helper(FromHelper::Ready { drag })), wiggle);
    assert_eq!(out.hear(OutHeard::Late), wiggle, "not there yet: again");
    let other = FromHelper::Operation { drag: DragId::new(), op: DragOp::Copy };
    assert_eq!(out.hear(OutHeard::Helper(other)), [], "another drag's");
    let over = FromHelper::Operation { drag, op: DragOp::Copy };
    assert_eq!(
        out.hear(OutHeard::Helper(over)),
        [OutAct::Input(release(2.0, 30.0)), OutAct::Deadline(Some(CATCH_WAIT))]
    );
}

/// What the catch took reaches the client: the named file and the promised one by their paths
/// here, then the data, inline up to the budget and kept past it under the item it went out as.
/// A promise not kept is left out, and data past the catcher's cap is never offered.
#[test]
fn the_catch_says_what_it_took_and_keeps_what_does_not_ride_inline() {
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join("named.txt");
    let called = dir.path().join("called in.pdf");
    std::fs::write(&named, b"named").unwrap();
    std::fs::write(&called, b"%PDF").unwrap();
    let mut out = reached();
    let drag = out.drag();
    out.hear(OutHeard::Helper(FromHelper::Operation { drag, op: DragOp::Copy }));
    let big = vec![7_u8; INLINE_CLIP_BYTES];
    let data = vec![
        CaughtData {
            item: 2,
            uti: "public.utf8-plain-text".to_owned(),
            bytes: Some(b"fox".to_vec()),
            size: 3,
        },
        CaughtData {
            item: 2,
            uti: "public.png".to_owned(),
            bytes: Some(big.clone()),
            size: big.len() as u64,
        },
        CaughtData { item: 3, uti: "public.tiff".to_owned(), bytes: None, size: 1 << 30 },
    ];
    let files = vec![named.to_string_lossy().into_owned()];
    assert_eq!(
        out.hear(OutHeard::Helper(FromHelper::Caught { drag, files, data, promises: 2 })),
        [OutAct::Deadline(Some(CATCH_WAIT))],
        "two promises to come"
    );
    let path = Some(called.to_string_lossy().into_owned());
    assert_eq!(out.hear(OutHeard::Helper(FromHelper::Promised { drag, path, error: None })), []);
    let broken = FromHelper::Promised { drag, path: None, error: Some("refused".to_owned()) };
    let acts = out.hear(OutHeard::Helper(broken));
    assert!(out.over());
    let [
        OutAct::Keep(kept),
        OutAct::Tell(DragEvent::OutCaught { items, .. }),
        OutAct::Helper(ToHelper::Stop { .. }),
        OutAct::Deadline(None),
        OutAct::Over,
    ] = &*acts
    else {
        panic!("{acts:?}")
    };
    let png = ClipType::Format(ClipFormat::Png);
    assert_eq!(kept, &[(2, png.clone(), Bytes::from(big))], "past the budget");
    assert_eq!(items.len(), 3, "{items:?}");
    let path_of = |n: usize| items[n].file.as_ref().and_then(|f| f.path.clone());
    assert_eq!(path_of(0), Some(named.to_string_lossy().into_owned()));
    assert_eq!(path_of(1), Some(called.to_string_lossy().into_owned()));
    assert_eq!(items[0].file.as_ref().map(|f| (f.name.as_str(), f.size)), Some(("named.txt", 5)));
    let reps = &items[2].reps;
    assert_eq!(reps[0].inline.as_deref(), Some(&b"fox"[..]));
    assert_eq!((&reps[1].kind, reps[1].inline.as_ref()), (&png, None), "fetched under the drag");
}

/// A release on the tile drops on the worker: the drag is over with nothing caught. A catch the
/// catcher never shows for, or the drag never reaches, is let go of with nothing dropped, and
/// the client hears why; once released, the button is the catcher's and nothing is cancelled.
#[test]
fn a_drag_out_ends_on_the_tile_or_says_why_it_was_not_caught() {
    let mut on_tile = out();
    assert_eq!(on_tile.hear(OutHeard::Released), [OutAct::Deadline(None), OutAct::Over]);

    let mut unplaced = out();
    unplaced.hear(OutHeard::Catch);
    unplaced.hear(OutHeard::Located(Ok((1.0, 1.0))));
    let acts = unplaced.hear(OutHeard::Late);
    assert_eq!(acts.first(), Some(&OutAct::Cancel));
    assert!(acts.iter().any(
        |a| matches!(a, OutAct::Tell(DragEvent::OutFailed { error, .. }) if error == NO_CATCHER)
    ));

    let mut unreached = reached();
    let mut acts = Vec::new();
    for _ in 0..REACH_TRIES {
        acts = unreached.hear(OutHeard::Late);
    }
    assert_eq!(acts.first(), Some(&OutAct::Cancel), "{acts:?}");
    assert!(acts.iter().any(
        |a| matches!(a, OutAct::Tell(DragEvent::OutFailed { error, .. }) if error == NOT_REACHED)
    ));

    let mut silent = reached();
    let drag = silent.drag();
    silent.hear(OutHeard::Helper(FromHelper::Operation { drag, op: DragOp::Copy }));
    let acts = silent.hear(OutHeard::Late);
    assert!(!acts.contains(&OutAct::Cancel), "let go already: {acts:?}");
    assert!(acts.iter().any(
        |a| matches!(a, OutAct::Tell(DragEvent::OutFailed { error, .. }) if error == NOT_CAUGHT)
    ));
    assert!(silent.over());
}
