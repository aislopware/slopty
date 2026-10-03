use std::path::Path;

use slopty_core::{ClientId, WallMs};
use slopty_proto::dnd::FromHelper;
use slopty_proto::drag::{FileMeta, Promised};
use slopty_proto::transfer::{ClipFormat, Rep};

use super::*;

fn file(name: &str) -> DragItem {
    let meta = FileMeta {
        name: name.to_owned(),
        size: 3,
        folder: false,
        mode: 0o644,
        mtime_ms: WallMs::ZERO,
        path: None,
    };
    DragItem { file: Some(meta), promised: None, reps: Vec::new() }
}

fn promised(kind: &str) -> DragItem {
    DragItem { file: None, promised: Some(kind.to_owned()), reps: Vec::new() }
}

/// Text inline and a picture still coming.
fn data() -> DragItem {
    let text = Rep {
        kind: ClipType::Format(ClipFormat::Text),
        size: Some(3),
        hash: None,
        inline: Some(b"fox".to_vec()),
    };
    let png =
        Rep { kind: ClipType::Format(ClipFormat::Png), size: Some(9), hash: None, inline: None };
    DragItem { file: None, promised: None, reps: vec![text, png] }
}

const DIR: &str = "/Users/w/.slopty/drop/d";

/// A drop of `items` entered at (10, 20), its source placed and pressed, its session on.
fn live(items: &[DragItem]) -> DropIn {
    let (mut drop_in, _acts) =
        DropIn::enter(DragId::new(), (10.0, 20.0), DragOps::COPY, items, Path::new(DIR));
    let drag = drop_in.drag();
    drop_in.hear(Heard::Mapped(Ok((110.0, 220.0))));
    drop_in.hear(Heard::Helper(FromHelper::Ready { drag }));
    drop_in.hear(Heard::Helper(FromHelper::Began { drag }));
    drop_in
}

fn input(drop_in: &DropIn, f: impl FnOnce(DragId) -> DragInput) -> Heard {
    Heard::Input(f(drop_in.drag()))
}

/// The entry maps the point first, then puts the helper's source at the global point with the
/// items (each named file at the URL it will land at), then presses into it; moves before the
/// session begins wait, and the session carries the drag to the newest one.
#[test]
fn enter_maps_then_places_the_source_then_presses_and_drags() {
    let drag = DragId::new();
    let items = [file("a.txt"), data()];
    let (mut drop_in, acts) =
        DropIn::enter(drag, (10.0, 20.0), DragOps::COPY, &items, Path::new(DIR));
    assert_eq!(acts, [Act::Enter { x: 10.0, y: 20.0 }, Act::Deadline(Some(START_WAIT))]);
    let acts = drop_in.hear(Heard::Mapped(Ok((110.0, 220.0))));
    let [Act::Helper(ToHelper::SourceAt { x, y, items: source, .. }), Act::Deadline(_)] =
        acts.as_slice()
    else {
        panic!("{acts:?}")
    };
    assert_eq!((*x, *y), (110.0, 220.0), "global points");
    assert_eq!(source[0].file.as_deref(), Some("/Users/w/.slopty/drop/d/a.txt"));
    assert!(source[1].given.iter().any(|g| g.bytes == b"fox"), "text at once");
    assert!(!source[1].given.iter().any(|g| g.uti == "public.png"), "the picture when it comes");
    assert_eq!(
        drop_in.hear(Heard::Helper(FromHelper::Ready { drag })),
        [Act::Press { x: 10.0, y: 20.0 }, Act::Deadline(Some(START_WAIT))]
    );
    assert_eq!(
        drop_in.hear(Heard::Input(DragInput::Move { drag, x: 30.0, y: 20.0 })),
        Vec::<Act>::new()
    );
    assert_eq!(
        drop_in.hear(Heard::Input(DragInput::Move { drag, x: 40.0, y: 25.0 })),
        Vec::<Act>::new()
    );
    assert_eq!(
        drop_in.hear(Heard::Helper(FromHelper::Began { drag })),
        [Act::Deadline(None), Act::Move { x: 40.0, y: 25.0 }]
    );
}

/// A drag out of this worker's app coming back names its files where they are: the source
/// gives each at its own path from the start, and the drop is let go at once, with nothing to
/// wait for.
#[test]
fn a_drag_back_names_the_workers_own_files_and_lets_go_at_once() {
    let drag = DragId::new();
    let mut own = file("report.pdf");
    if let Some(meta) = own.file.as_mut() {
        meta.path = Some("/Users/w/Documents/report.pdf".to_owned());
    }
    let (mut drop_in, _acts) =
        DropIn::enter(drag, (10.0, 20.0), DragOps::COPY, &[own], Path::new(DIR));
    let acts = drop_in.hear(Heard::Mapped(Ok((110.0, 220.0))));
    let [Act::Helper(ToHelper::SourceAt { items: source, .. }), Act::Deadline(_)] = acts.as_slice()
    else {
        panic!("{acts:?}")
    };
    assert_eq!(source[0].file.as_deref(), Some("/Users/w/Documents/report.pdf"));
    drop_in.hear(Heard::Helper(FromHelper::Ready { drag }));
    drop_in.hear(Heard::Helper(FromHelper::Began { drag }));
    let dropped = drop_in.hear(Heard::Input(DragInput::Drop {
        drag,
        x: 10.0,
        y: 20.0,
        promised: Vec::new(),
    }));
    assert_eq!(dropped, [Act::Release, Act::Deadline(Some(LAND_WAIT))]);
}

/// A live drag is carried only where it went: a move to the point it is at posts nothing, and
/// another drag's input does nothing.
#[test]
fn moves_coalesce_and_a_still_hover_posts_nothing() {
    let mut drop_in = live(&[file("a.txt")]);
    let to = |x| input(&drop_in, |drag| DragInput::Move { drag, x, y: 20.0 });
    let (first, again) = (to(50.0), to(50.0));
    assert_eq!(drop_in.hear(first), [Act::Move { x: 50.0, y: 20.0 }]);
    assert_eq!(drop_in.hear(again), Vec::<Act>::new());
    let other = DragInput::Move { drag: DragId::new(), x: 60.0, y: 20.0 };
    assert_eq!(drop_in.hear(Heard::Input(other)), Vec::<Act>::new());
}

/// What the target under the drag would do goes to the client when it changes, within what the
/// source allows, and the first word always goes: a refusal from the start too, which the
/// client would otherwise take for the copy it assumes.
#[test]
fn the_operation_is_told_when_it_changes() {
    let mut drop_in = live(&[file("a.txt")]);
    let drag = drop_in.drag();
    let op = |op| Heard::Helper(FromHelper::Operation { drag, op });
    assert_eq!(
        drop_in.hear(op(DragOp::None)),
        [Act::Tell(DragEvent::Operation { drag, op: DragOp::None })]
    );
    assert_eq!(
        drop_in.hear(op(DragOp::Copy)),
        [Act::Tell(DragEvent::Operation { drag, op: DragOp::Copy })]
    );
    assert!(drop_in.hear(op(DragOp::Copy)).is_empty(), "no change");
    assert_eq!(
        drop_in.hear(op(DragOp::Link)),
        [Act::Tell(DragEvent::Operation { drag, op: DragOp::None })]
    );
}

/// The drop lets go only once every file the items name is whole and every piece of data
/// still coming is here, each handed to the helper as it comes; then the session's end says
/// what the target did.
#[test]
fn the_release_waits_for_every_file() {
    let mut drop_in = live(&[file("a.txt"), data()]);
    let drag = drop_in.drag();
    let dropped = drop_in.hear(Heard::Input(DragInput::Drop {
        drag,
        x: 70.0,
        y: 80.0,
        promised: Vec::new(),
    }));
    assert_eq!(dropped, [Act::Move { x: 70.0, y: 80.0 }], "carried to the drop, not let go");
    let landed = Path::new(DIR).join("a.txt");
    let acts = drop_in.hear(Heard::Landed { name: "a.txt".to_owned(), path: landed.clone() });
    let url = landed.to_string_lossy().as_bytes().to_vec();
    assert_eq!(
        acts,
        [Act::Helper(ToHelper::Data { drag, item: 0, uti: FILE_URL.to_owned(), bytes: Some(url) })],
        "the picture is still coming"
    );
    let acts = drop_in.hear(Heard::Data {
        item: 1,
        kind: ClipType::Format(ClipFormat::Png),
        bytes: Some(b"png".to_vec()),
    });
    assert_eq!(
        acts,
        [
            Act::Helper(ToHelper::Data {
                drag,
                item: 1,
                uti: "public.png".to_owned(),
                bytes: Some(b"png".to_vec())
            }),
            Act::Release,
            Act::Deadline(Some(LAND_WAIT)),
        ]
    );
    let acts = drop_in.hear(Heard::Helper(FromHelper::Ended { drag, op: DragOp::Copy }));
    assert_eq!(
        acts,
        [
            Act::Helper(ToHelper::Stop { drag }),
            Act::Tell(DragEvent::Ended { drag, op: DragOp::Copy, error: None }),
            Act::Deadline(None),
            Act::Over,
        ]
    );
    assert!(drop_in.over());
}

/// A promised file is named by the drop, and may land before the drop says its name; a promise
/// the drop says failed gives no file, and the drop goes on without it.
#[test]
fn a_promised_file_is_named_at_the_drop_and_may_land_before_it() {
    let mut drop_in = live(&[promised("public.jpeg"), promised("public.jpeg")]);
    let drag = drop_in.drag();
    let path = Path::new(DIR).join("IMG_1.jpg");
    assert_eq!(
        drop_in.hear(Heard::Landed { name: "IMG_1.jpg".to_owned(), path: path.clone() }),
        Vec::<Act>::new()
    );
    let mut meta = file("IMG_1.jpg").file;
    let promised = vec![Promised { item: 0, file: meta.take() }, Promised { item: 1, file: None }];
    let acts = drop_in.hear(Heard::Input(DragInput::Drop { drag, x: 10.0, y: 20.0, promised }));
    let url = path.to_string_lossy().as_bytes().to_vec();
    assert_eq!(
        acts,
        [
            Act::Helper(ToHelper::Data { drag, item: 1, uti: FILE_URL.to_owned(), bytes: None }),
            Act::Helper(ToHelper::Data {
                drag,
                item: 0,
                uti: FILE_URL.to_owned(),
                bytes: Some(url)
            }),
            Act::Release,
            Act::Deadline(Some(LAND_WAIT)),
        ]
    );
}

/// Leaving ends the worker's drag with nothing dropped: a session on is cancelled, a press not
/// yet made is let go of, and the helper's source goes.
#[test]
fn leave_cancels_the_upload_and_the_session() {
    let mut drop_in = live(&[file("a.txt")]);
    let drag = drop_in.drag();
    assert_eq!(
        drop_in.hear(Heard::Input(DragInput::Leave { drag })),
        [Act::Cancel, Act::Helper(ToHelper::Stop { drag }), Act::Deadline(None), Act::Over]
    );
    let (mut mapping, _acts) =
        DropIn::enter(drag, (0.0, 0.0), DragOps::COPY, &[file("a.txt")], Path::new(DIR));
    assert_eq!(
        mapping.hear(Heard::Input(DragInput::Leave { drag })),
        [Act::Release, Act::Helper(ToHelper::Stop { drag }), Act::Deadline(None), Act::Over]
    );
    assert!(mapping.hear(Heard::Helper(FromHelper::Ready { drag })).is_empty(), "over");
}

/// A drop that fails or never finishes says why: an upload that failed cancels the session it
/// holds, a session that never began lets go of its press, and a release that never ended
/// tells the client so.
#[test]
fn a_drop_that_cannot_land_says_why() {
    let mut failing = live(&[file("a.txt")]);
    let drag = failing.drag();
    let acts = failing.hear(Heard::Failed("a.txt: the disk is full".to_owned()));
    assert_eq!(acts[0], Act::Cancel);
    assert!(acts.contains(&Act::Tell(DragEvent::Ended {
        drag,
        op: DragOp::None,
        error: Some("a.txt: the disk is full".to_owned())
    })));

    let (mut pressed, _acts) =
        DropIn::enter(drag, (0.0, 0.0), DragOps::COPY, &[file("a.txt")], Path::new(DIR));
    pressed.hear(Heard::Mapped(Ok((0.0, 0.0))));
    pressed.hear(Heard::Helper(FromHelper::Ready { drag }));
    let acts = pressed.hear(Heard::Late);
    assert_eq!(acts[0], Act::Cancel, "the press is let go of with nothing dropped");
    assert!(acts.contains(&Act::Tell(DragEvent::Ended {
        drag,
        op: DragOp::None,
        error: Some(NOT_STARTED.to_owned())
    })));

    let mut released = live(&[data()]);
    let drag = released.drag();
    released.hear(Heard::Data { item: 0, kind: ClipType::Format(ClipFormat::Png), bytes: None });
    let acts = released.hear(Heard::Input(DragInput::Drop {
        drag,
        x: 10.0,
        y: 20.0,
        promised: Vec::new(),
    }));
    assert!(acts.contains(&Act::Release), "{acts:?}");
    let acts = released.hear(Heard::Late);
    assert_eq!(
        acts,
        [
            Act::Helper(ToHelper::Stop { drag }),
            Act::Tell(DragEvent::Ended {
                drag,
                op: DragOp::None,
                error: Some(UNFINISHED.to_owned())
            }),
            Act::Deadline(None),
            Act::Over,
        ],
        "let go already: nothing more is posted"
    );
}

/// One drag crosses a worker at a time; what is heard for a drag before its stream claims it
/// waits for the claim, and the worker is free again once the claim goes.
#[test]
fn one_drag_crosses_a_worker_at_a_time() {
    let drags = Arc::new(Drags::default());
    let (first, second) = (DragId::new(), DragId::new());
    let client = ClientId::new();
    drags.tell(first, Heard::Failed("early".to_owned()));
    let mut claim = drags.claim(client, first).expect("free");
    assert!(matches!(claim.news().try_recv(), Ok(Heard::Failed(e)) if e == "early"));
    assert_eq!(drags.live(), Some((first, client)));
    assert!(drags.claim(ClientId::new(), second).is_none(), "busy");
    drags.tell(first, Heard::Late);
    assert!(matches!(claim.news().try_recv(), Ok(Heard::Late)));
    drags.helper_gone();
    assert!(matches!(claim.news().try_recv(), Ok(Heard::HelperGone)));
    drop(claim);
    assert_eq!(drags.live(), None);
    drags.tell(second, Heard::Late);
    drags.forget(second);
    let mut claim = drags.claim(client, second).expect("free again");
    assert!(claim.news().try_recv().is_err(), "forgotten");
}
