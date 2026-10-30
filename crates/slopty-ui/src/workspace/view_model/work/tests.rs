use gpui::AppContext as _;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{TableFrame, ThreadFrame};
use slopty_proto::thread::{
    BackgroundTask, ContentRef, Cursor, Draft, DraftImage, Image, ThreadState,
};

use crate::conversation::thread::fixtures;
use crate::view_model::{transcript, work};
use crate::workspace::WorkspaceView;
use crate::workspace::rig::{KEY, link, mac};

/// A workspace linked to one worker whose table holds `states`' threads, the first followed
/// as a chat.
fn holding<'a>(
    states: &[ThreadState],
    cx: &'a mut gpui::TestAppContext,
) -> (gpui::Entity<WorkspaceView>, &'a mut gpui::VisualTestContext) {
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let cursor = Cursor { epoch: 1, seq: 1 };
    let rows = states.iter().map(|state| state.row(WallMs::ZERO)).collect();
    let first = states.first().map(|state| state.meta.id).expect("a thread");
    workspace.update(cx, |v, cx| {
        v.threads_linked(KEY, cx);
        v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
        v.view_follow(first.to_string(), cx);
        for state in states {
            let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
            v.thread_frame(KEY, state.meta.id, frame, cx);
        }
    });
    cx.run_until_parked();
    (workspace, cx)
}

/// A followed chat's tray and dock go out beside its transcript.
#[gpui::test]
fn a_followed_chat_publishes_its_tray(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::empty();
    state.tasks =
        vec![BackgroundTask::new("b1", BackgroundTask::SHELL, "npm run dev", WallMs::ZERO)];
    let chat = state.meta.id.to_string();
    let (workspace, cx) = holding(&[state], cx);
    let views = workspace.update(cx, |v, cx| v.view_chat_views(&chat, cx)).expect("its views");
    let ids: Vec<&str> = views.work.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["task:b1"]);
}

/// A followed chat's dock mirrors its agent's prompt box: the draft as the box shows it, each
/// pasted picture by a path read back through the thread, and the prompts queued behind the
/// turn. A box with nothing in it shows nothing.
#[gpui::test]
fn a_followed_chat_mirrors_its_prompt_box(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::empty();
    let image = Image {
        digest: "ab".repeat(32),
        media_type: "image/png".to_owned(),
        bytes: 68,
        width: 1,
        height: 1,
        at: ContentRef("draft-image:1.png".to_owned()),
    };
    state.draft = Some(Draft {
        text: "Look at [Image #1]".to_owned(),
        images: vec![DraftImage { n: 1, image }],
        at_ms: WallMs::ZERO,
    });
    state.queued = vec!["then run the tests".to_owned()];
    let (thread, chat) = (state.meta.id, state.meta.id.to_string());
    let mut sent = state.clone();
    let (workspace, cx) = holding(&[state], cx);
    let views = workspace.update(cx, |v, cx| v.view_chat_views(&chat, cx)).expect("its views");
    let prompt = views.dock.prompt.expect("its prompt box");
    assert_eq!(prompt.text, "Look at [Image #1]");
    assert_eq!(prompt.queued, ["then run the tests"]);
    let [picture] = prompt.images.as_slice() else { panic!("one picture: {:?}", prompt.images) };
    assert_eq!((picture.number, picture.mime_type.as_str()), (1, "image/png"));
    assert_eq!(transcript::image_of(&picture.path), Some((thread, work::DRAFT_ITEM, 0)));
    // The TUI sent it: the box is empty again.
    sent.draft = None;
    sent.queued.clear();
    let cursor = Cursor { epoch: 1, seq: 2 };
    workspace.update(cx, |v, cx| {
        let frame = ThreadFrame::Snapshot { cursor, state: Box::new(sent) };
        v.thread_frame(KEY, thread, frame, cx);
    });
    cx.run_until_parked();
    let views = workspace.update(cx, |v, cx| v.view_chat_views(&chat, cx)).expect("its views");
    assert_eq!(views.dock.prompt, None, "an empty box shows nothing");
}
