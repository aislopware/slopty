//! A page whose document its machine holds, against a fake link to that machine: it waits while
//! the machine is out of reach, saying for which, asks again the moment the link is back, and
//! fails only when the machine answers that it is gone or, reachable, does not answer in time.

use std::time::Duration;

use gpui::{AppContext as _, Entity, VisualTestContext, WindowId};
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::detail::Clipped;
use slopty_proto::thread::wire::{Expanded, TableFrame, ThreadFrame, ThreadRequest};
use slopty_proto::thread::{
    ContentRef, Cursor, Item, ItemBody, ItemId, ThreadId, ThreadState, ToolCall, ToolDetail,
    ToolState, TurnId,
};
use slopty_proto::visual::{AppAnswer, AppAsk, AppBody, McpApp, PageDetail, PageSource};
use tokio::sync::mpsc::Receiver;

use crate::conversation::thread::fixtures;
use crate::view_model::facts::page_key;
use crate::workspace::rig::{KEY, link, mac};
use crate::workspace::{WorkerStatus, WorkspaceView};

/// Where the clipped page's whole is on its machine.
const WHOLE: &str = "blob:page-whole";
/// The whole page.
const PAGE: &str = "<!doctype html><p>the whole page</p>";

/// A thread whose one call shows a page from `source`.
fn showing(source: PageSource) -> ThreadState {
    let mut state = fixtures::empty();
    let call = ToolCall {
        name: "mcp__slopty__show_page".to_owned(),
        kind: "mcp".to_owned(),
        title: "Latency".to_owned(),
        input: Clipped::whole("{}"),
        state: ToolState::Completed,
        output: None,
        images: Vec::new(),
        detail: Some(ToolDetail::Page(PageDetail { title: "Latency".to_owned(), source })),
        child: None,
        ended_ms: None,
    };
    state.items.push(Item {
        id: ItemId("p1".to_owned()),
        turn: TurnId(1),
        at_ms: WallMs::ZERO,
        body: ItemBody::Tool(Box::new(call)),
    });
    state
}

/// HTML the call cut, its whole at [`WHOLE`] on the machine.
fn clipped() -> PageSource {
    PageSource::Html(Clipped {
        full: Some(ContentRef(WHOLE.to_owned())),
        ..Clipped::whole("<!doctype html><p>the whole")
    })
}

/// A workspace linked to the machine "studio", whose thread `state` it holds, and the window
/// its page shows in.
fn holding<'a>(
    state: &ThreadState,
    cx: &'a mut gpui::TestAppContext,
) -> (Entity<WorkspaceView>, &'a mut VisualTestContext, Receiver<ClientMsg>, WindowId) {
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let cursor = Cursor { epoch: 1, seq: 1 };
    let thread = state.meta.id;
    workspace.update(cx, |v, cx| {
        v.threads_linked(KEY, cx);
        let rows = vec![state.row(WallMs::ZERO)];
        v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
        v.thread_hub(KEY, cx).update(cx, |hub, cx| hub.open(thread, cx));
        let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
        v.thread_frame(KEY, thread, frame, cx);
    });
    cx.run_until_parked();
    while rx.try_recv().is_ok() {}
    let window = cx.update(|window, _| window.window_handle().window_id());
    (workspace, cx, rx, window)
}

/// The link to the machine goes, as the app hears it.
fn drop_link(workspace: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    workspace.update(cx, |v, cx| {
        v.threads_unlinked(KEY, cx);
        v.disconnect_worker(KEY, WorkerStatus::Relinking, cx);
    });
    cx.run_until_parked();
}

/// The link is back: what the machine is sent from now on.
fn restore_link(
    workspace: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Receiver<ClientMsg> {
    let rx = link(workspace, cx, mac(|_| {}), Vec::new());
    workspace.update(cx, |v, cx| v.threads_linked(KEY, cx));
    rx
}

/// Time passes, its timers run.
fn pass(cx: &VisualTestContext, time: Duration) {
    let step = Duration::from_millis(500);
    let mut left = time;
    while !left.is_zero() {
        let now = left.min(step);
        cx.executor().advance_clock(now);
        cx.run_until_parked();
        left = left.saturating_sub(now);
    }
}

/// What the machine was sent: the wholes asked, and the app asks by number.
fn asked(rx: &mut Receiver<ClientMsg>) -> (Vec<ContentRef>, Vec<(u64, AppAsk)>) {
    let (mut wholes, mut apps) = (Vec::new(), Vec::new());
    while let Ok(msg) = rx.try_recv() {
        match msg {
            ClientMsg::Thread(ThreadRequest::Expand { content, .. }) => wholes.push(content),
            ClientMsg::Thread(ThreadRequest::App { id, ask, .. }) => apps.push((id, ask)),
            _ => {}
        }
    }
    (wholes, apps)
}

/// How page `key` stands: the machine it waits for, why it failed, and its document.
fn stands(
    workspace: &Entity<WorkspaceView>,
    key: &str,
    cx: &mut VisualTestContext,
) -> (Option<String>, Option<String>, Option<String>) {
    cx.update(|_, cx| {
        let v = workspace.read(cx);
        let pages = &v.view_model.pages;
        let failed = pages.states.get(key).and_then(|s| s.borrow().failed.clone());
        let document = pages.shown.get(key).and_then(|p| p.document.clone()).map(|(_, html)| html);
        (v.page_waiting(key, cx), failed, document)
    })
}

fn open(
    workspace: &Entity<WorkspaceView>,
    key: &str,
    window: WindowId,
    cx: &mut VisualTestContext,
) {
    workspace.update(cx, |v, cx| v.page_open_call(key, window, cx)).expect("the page opens");
    cx.run_until_parked();
}

fn answer(
    workspace: &Entity<WorkspaceView>,
    thread: ThreadId,
    body: Expanded,
    cx: &mut VisualTestContext,
) {
    let frame = ThreadFrame::Expanded { content: ContentRef(WHOLE.to_owned()), body };
    workspace.update(cx, |v, cx| v.thread_frame(KEY, thread, frame, cx));
    // The wait looks again on its next tick.
    pass(cx, Duration::from_millis(200));
}

/// A clipped page whose link drops mid-fetch waits, "for studio", far past the time a reachable
/// machine gets; the moment the link is back it asks again, and the whole that comes is its
/// document.
#[gpui::test]
fn a_page_fetched_while_its_machine_is_away_waits_and_asks_again(cx: &mut gpui::TestAppContext) {
    let state = showing(clipped());
    let key = page_key(state.meta.id, "p1");
    let (workspace, cx, mut rx, window) = holding(&state, cx);
    open(&workspace, &key, window, cx);
    pass(cx, Duration::from_millis(200));
    assert_eq!(asked(&mut rx).0, [ContentRef(WHOLE.to_owned())], "asked of its machine");
    assert_eq!(stands(&workspace, &key, cx), (None, None, None), "loading, its machine there");

    drop_link(&workspace, cx);
    assert_eq!(stands(&workspace, &key, cx).0.as_deref(), Some("studio"), "waits for studio");
    pass(cx, Duration::from_secs(60));
    assert_eq!(
        stands(&workspace, &key, cx),
        (Some("studio".to_owned()), None, None),
        "a minute away is no failure"
    );

    let mut rx = restore_link(&workspace, cx);
    pass(cx, Duration::from_millis(200));
    assert_eq!(asked(&mut rx).0, [ContentRef(WHOLE.to_owned())], "asked again once back");
    assert_eq!(stands(&workspace, &key, cx), (None, None, None), "loading again");
    answer(&workspace, state.meta.id, Expanded::Text(PAGE.to_owned()), cx);
    assert_eq!(stands(&workspace, &key, cx), (None, None, Some(PAGE.to_owned())), "its document");
}

/// Reachable, a machine that does not answer in time fails the page: the time it was away does
/// not count, only the time linked without an answer.
#[gpui::test]
fn a_page_fails_when_its_reachable_machine_stays_silent(cx: &mut gpui::TestAppContext) {
    let state = showing(clipped());
    let key = page_key(state.meta.id, "p1");
    let (workspace, cx, _rx, window) = holding(&state, cx);
    open(&workspace, &key, window, cx);
    pass(cx, Duration::from_secs(10));
    drop_link(&workspace, cx);
    pass(cx, Duration::from_secs(30));
    let _rx = restore_link(&workspace, cx);
    // Ten seconds before it went and fifteen since it came back are short of the wait.
    pass(cx, Duration::from_secs(15));
    assert_eq!(stands(&workspace, &key, cx).1, None, "not yet");
    pass(cx, Duration::from_secs(6));
    let failed = stands(&workspace, &key, cx).1;
    assert_eq!(failed.as_deref(), Some("Couldn't read its page"), "reachable and silent");
}

/// A machine that answers the whole is gone fails the page at once.
#[gpui::test]
fn a_page_fails_when_its_machine_says_it_is_gone(cx: &mut gpui::TestAppContext) {
    let state = showing(clipped());
    let key = page_key(state.meta.id, "p1");
    let (workspace, cx, _rx, window) = holding(&state, cx);
    open(&workspace, &key, window, cx);
    answer(&workspace, state.meta.id, Expanded::Gone, cx);
    let failed = stands(&workspace, &key, cx).1;
    assert_eq!(failed.as_deref(), Some("Couldn't read its page"), "the machine says it is gone");
}

/// An MCP App's view asked while its machine is out of reach waits for it, and is asked again,
/// by the same number, once the link is back; its answer is the page's document.
#[gpui::test]
fn an_apps_view_asked_while_away_is_asked_again(cx: &mut gpui::TestAppContext) {
    let app = McpApp {
        server: "charts".to_owned(),
        tool: "plot".to_owned(),
        resource_uri: "ui://charts/plot".to_owned(),
        display_mode: None,
        result: None,
    };
    let state = showing(PageSource::App(app));
    let key = page_key(state.meta.id, "p1");
    let (workspace, cx, mut rx, window) = holding(&state, cx);
    drop_link(&workspace, cx);
    open(&workspace, &key, window, cx);
    assert!(asked(&mut rx).1.is_empty(), "nothing goes out with the link down");
    assert_eq!(stands(&workspace, &key, cx).0.as_deref(), Some("studio"));

    let mut rx = restore_link(&workspace, cx);
    pass(cx, Duration::from_millis(200));
    let apps = asked(&mut rx).1;
    let read = AppAsk::Read { server: "charts".to_owned(), uri: "ui://charts/plot".to_owned() };
    assert_eq!(apps.iter().map(|(_, ask)| ask.clone()).collect::<Vec<_>>(), [read]);
    let answer = AppAnswer::Resource {
        uri: "ui://charts/plot".to_owned(),
        mime_type: Some("text/html".to_owned()),
        body: AppBody::Text(PAGE.to_owned()),
        meta: "{}".to_owned(),
    };
    let frame = ThreadFrame::App { id: apps[0].0, answer };
    workspace.update(cx, |v, cx| v.thread_frame(KEY, state.meta.id, frame, cx));
    cx.run_until_parked();
    assert_eq!(stands(&workspace, &key, cx), (None, None, Some(PAGE.to_owned())));
}
