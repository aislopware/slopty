//! The control stream to a worker is bounded and shared with the terminal and screen views,
//! which fill it while they stream. What the workspace sends behind a full stream waits for
//! room, in order (`crate::outbox`): none of it is dropped but a watch set the next one
//! outdates, and it is reported sent because it will be.

use slopty_proto::terminal::TermSize;

use super::*;

/// How many messages the fake link's control stream holds ([`connect`]).
const DEPTH: usize = 256;

#[gpui::test]
fn a_full_control_stream_holds_what_must_arrive_in_order(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let fill = |n: usize| ClientMsg::ReadFile { path: format!("/fill/{n}") };
    let watch = |path: &str| ClientMsg::WatchFiles { paths: vec![path.to_owned()] };
    let session = SessionId::new();
    let attach =
        ClientMsg::Term { session, req: TermRequest::Attach { size: TermSize::default() } };
    let save = ClientMsg::WriteFile {
        path: "/w/notes.md".to_owned(),
        text: "two\n".to_owned(),
        base_modified_ms: None,
    };
    let close = ClientMsg::Screen(ScreenRequest::Close(StreamId(7)));
    let behind = [watch("/w/a.rs"), attach.clone(), save.clone(), watch("/w/b.rs"), close.clone()];
    view.update(cx, |v, _cx| {
        let w = v.workers.get(&fake.key).expect("linked");
        for n in 0..DEPTH {
            assert!(w.send(fill(n)), "room for {n}");
        }
        for msg in behind {
            assert!(w.send(msg), "behind a full stream, still on its way");
        }
    });

    let mut got = Vec::new();
    loop {
        cx.run_until_parked();
        let more = fake.drain();
        if more.is_empty() {
            break;
        }
        got.extend(more);
    }
    let fills: Vec<ClientMsg> = (0..DEPTH).map(fill).collect();
    assert_eq!(got[..DEPTH], fills[..], "what fit went first");
    assert_eq!(
        got[DEPTH..],
        [attach, save, watch("/w/b.rs"), close],
        "the rest in order, the first watch set outdated by the second"
    );

    // Down, nothing is on its way.
    view.update(cx, |v, cx| {
        v.disconnect_worker(fake.key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    let sent = view.read_with(cx, |v, _| v.workers.get(&fake.key).map(|w| w.send(fill(0))));
    assert_eq!(sent, Some(false), "a worker down says so");
}
