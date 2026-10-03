use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use slopty_core::{ClientId, WorkerId};
use slopty_input::pasteboard::Held;
use slopty_proto::transfer::{ClipEntry, ClipType, Offer, Rep};

use super::*;
use crate::clip::Sink;

fn rep(format: ClipFormat, bytes: &[u8], inline: bool) -> Rep {
    Rep {
        kind: ClipType::Format(format),
        size: Some(bytes.len() as u64),
        hash: Some(digest(bytes)),
        inline: inline.then(|| bytes.to_vec()),
    }
}

fn offer(client: ClientId, generation: u64, concealed: bool, reps: Vec<Rep>) -> Offer {
    Offer {
        origin: Peer::Client(client),
        generation,
        age_ms: 0,
        concealed,
        items: vec![ClipEntry { reps }],
    }
}

/// A clipboard with one client linked as `link`, and the fetches sent to it.
fn linked(link: Link) -> (Clipboard<Held>, Arc<Mutex<Vec<ClipMsg>>>) {
    let clip = Clipboard::new(Held::default(), Peer::Worker(WorkerId::new()));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&sent);
    let sink: Sink = Arc::new(move |msg| log.lock().push(msg));
    clip.attach(link, sink);
    (clip, sent)
}

const PNG: &[u8] = b"\x89PNG picture";

/// A paste carries the paster's copy as rich text and pictures while it shares its clipboard
/// and the text is that copy's. What is missing is asked for once, and what arrives is taken.
#[test]
fn a_paste_carries_the_shared_copy_and_asks_for_the_rest_once() {
    let (link, client) = (1, ClientId::new());
    let (c, sent) = linked(link);
    let reps = vec![
        rep(ClipFormat::Text, b"hello", true),
        rep(ClipFormat::Html, b"<b>hello</b>", true),
        rep(ClipFormat::Png, PNG, false),
        rep(ClipFormat::FileUrls, b"file:///Users/me/a.png", true),
    ];
    let _mirror = c.offered(link, offer(client, 1, false, reps), Instant::now());
    assert_eq!(c.paste_plan(client, "hello", 1 << 20), None, "not shared");
    c.watch(link, true);
    assert_eq!(c.paste_plan(client, "something else", 1 << 20), None, "not the copy's text");
    assert_eq!(c.paste_plan(ClientId::new(), "hello", 1 << 20), None, "another client's paste");

    let plan = c.paste_plan(client, "hello", 1 << 20).unwrap();
    assert_eq!(plan.formats().collect::<Vec<_>>(), [ClipFormat::Html, ClipFormat::Png]);
    let missing = c.paste_missing(&plan);
    assert_eq!(missing.len(), 1);
    let arrivals = c.arrivals();
    c.paste_fetch(&plan, &missing);
    c.paste_fetch(&plan, &missing);
    assert_eq!(
        *sent.lock(),
        [ClipMsg::Fetch { rep: missing[0].clone(), max: Some(1 << 20), urgent: true }],
        "asked once"
    );
    assert!(!arrivals.has_changed().unwrap());
    let _whole = c.supply(link, &missing[0], PNG.to_vec());
    assert!(arrivals.has_changed().unwrap());
    assert_eq!(c.paste_missing(&plan), []);
    assert_eq!(
        c.paste_take(&plan),
        [(ClipFormat::Html, b"<b>hello</b>".to_vec()), (ClipFormat::Png, PNG.to_vec())]
    );
    assert!(c.paste_plan(client, "", 1 << 20).is_some(), "an empty paste is the copy's");
}

/// A secret carries nothing, a representation past the budget is not carried, one the client
/// will not send is no longer waited for, and a new copy voids the plan.
#[test]
fn a_paste_carries_no_secret_and_nothing_past_its_budget() {
    let (link, client) = (2, ClientId::new());
    let (c, _sent) = linked(link);
    c.watch(link, true);
    let secret =
        vec![rep(ClipFormat::Text, b"pw", true), rep(ClipFormat::Html, b"<i>pw</i>", true)];
    let _mirror = c.offered(link, offer(client, 1, true, secret), Instant::now());
    assert_eq!(c.paste_plan(client, "pw", 1 << 20), None);

    let big = vec![b'x'; 2_000];
    let reps = vec![
        rep(ClipFormat::Text, b"hi", true),
        rep(ClipFormat::Png, &big, false),
        rep(ClipFormat::Tiff, PNG, false),
    ];
    let _mirror = c.offered(link, offer(client, 2, false, reps), Instant::now());
    let plan = c.paste_plan(client, "hi", 1_000).unwrap();
    assert_eq!(plan.formats().collect::<Vec<_>>(), [ClipFormat::Tiff]);
    let missing = c.paste_missing(&plan);
    let _whole = c.refused(link, &missing[0]);
    assert!(c.paste_missing(&plan).is_empty(), "refused is not coming");
    assert_eq!(c.paste_take(&plan), []);

    let _mirror = c.offered(link, offer(client, 3, false, Vec::new()), Instant::now());
    assert!(c.paste_missing(&plan).is_empty() && c.paste_take(&plan).is_empty());
}
