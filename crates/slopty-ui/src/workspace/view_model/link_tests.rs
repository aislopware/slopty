//! What the shell reads of the chats through the workspace: how it reaches their machines and
//! the chats found by their words.

use gpui::AppContext as _;
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{TableFrame, ThreadFrame};
use slopty_proto::thread::{Cursor, ThreadState};
use slopty_view_proto::ConnectivityState;
use slopty_view_rpc::methods;
use tokio::sync::mpsc::Receiver;

use crate::conversation::thread::fixtures;
use crate::workspace::rig::{KEY, link, mac};
use crate::workspace::{WorkerStatus, WorkspaceView};

/// A workspace linked to one worker whose table holds `state`'s thread (its row as `row` makes
/// it), its state here, serving a feed, and what the worker has been sent since.
fn holding<'a>(
    state: &ThreadState,
    row: impl FnOnce(&mut slopty_proto::thread::wire::ThreadRow),
    cx: &'a mut gpui::TestAppContext,
) -> (
    gpui::Entity<WorkspaceView>,
    &'a mut gpui::VisualTestContext,
    Receiver<ClientMsg>,
    std::sync::Arc<crate::view_model::Feed>,
) {
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let (_, feed, _asks) = crate::view_model::ViewModel::new("me".to_owned());
    let cursor = Cursor { epoch: 1, seq: 1 };
    let thread = state.meta.id;
    let mut shown = state.row(WallMs::ZERO);
    row(&mut shown);
    workspace.update(cx, |v, cx| {
        v.view_model.feed = Some(std::sync::Arc::clone(&feed));
        v.threads_linked(KEY, cx);
        v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows: vec![shown] }, cx);
        v.thread_hub(KEY, cx).update(cx, |hub, cx| hub.open(thread, cx));
        let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
        v.thread_frame(KEY, thread, frame, cx);
    });
    cx.run_until_parked();
    while rx.try_recv().is_ok() {}
    (workspace, cx, rx, feed)
}

/// With no server, the whole is as reachable as its workers: reconnecting while the only one
/// drops, connected once its link is back.
#[gpui::test]
fn the_whole_follows_its_workers_links(cx: &mut gpui::TestAppContext) {
    let state = fixtures::thread("tools");
    let (workspace, cx, _rx, feed) = holding(&state, |_| {}, cx);
    let link_of = |status: WorkerStatus, cx: &mut gpui::VisualTestContext| {
        workspace.update(cx, |v, cx| {
            v.set_worker_status(KEY, status, cx);
            v.view_publish(cx);
        });
        feed.said_connectivity().state
    };
    assert_eq!(link_of(WorkerStatus::Relinking, cx), ConnectivityState::Reconnecting);
    assert_eq!(link_of(WorkerStatus::NoAnswer, cx), ConnectivityState::Reconnecting);
    assert_eq!(link_of(WorkerStatus::Connected, cx), ConnectivityState::Connected);
    feed.server(Some(slopty_view_proto::ServerPresence {
        name: "hub".to_owned(),
        online: false,
        ..Default::default()
    }));
    assert_eq!(feed.said_connectivity().state, ConnectivityState::Reconnecting, "the server's");
}

/// The chats are found by their words, with the match marked in an excerpt.
#[gpui::test]
fn chats_are_found_by_their_words(cx: &mut gpui::TestAppContext) {
    let state = fixtures::thread("tools");
    let words = state
        .items
        .iter()
        .find_map(|item| match &item.body {
            slopty_proto::thread::ItemBody::User(message) => Some(message.text.text.clone()),
            _ => None,
        })
        .expect("the person's words");
    let word: String = words.split_whitespace().max_by_key(|w| w.len()).expect("a word").to_owned();
    let (workspace, cx, _rx, _feed) = holding(&state, |_| {}, cx);
    let search = |query: &str, cx: &mut gpui::VisualTestContext| {
        let params = serde_json::json!({ "query": query, "limit": 10 });
        workspace
            .update(cx, |v, cx| v.view_call(methods::SEARCH_CHATS, &params, cx))
            .expect("an answer")
    };
    let found = search(&word.to_uppercase(), cx);
    let hit = &found.as_array().expect("a list")[0];
    assert_eq!(hit["chatId"], state.meta.id.to_string());
    assert_eq!(hit["role"], "user");
    let excerpt = hit["excerpt"].as_str().expect("an excerpt");
    let (start, end) = (
        usize::try_from(hit["matchStart"].as_u64().expect("a start")).expect("small"),
        usize::try_from(hit["matchEnd"].as_u64().expect("an end")).expect("small"),
    );
    assert_eq!(excerpt.get(start..end).map(str::to_lowercase), Some(word.to_lowercase()));
    assert_eq!(search("zzqqxx-nowhere", cx), serde_json::json!([]));
    assert_eq!(search("a", cx), serde_json::json!([]), "one letter finds nothing");
}
