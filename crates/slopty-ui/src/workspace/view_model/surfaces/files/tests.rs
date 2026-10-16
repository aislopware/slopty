use slopty_client::WorkerKey;
use slopty_proto::git::{GitDone, GitFile, GitOutcome, GitStatus};
use slopty_view_proto::{GitFileState, WorkspaceFileChangeKind, WorkspaceLineEnding};

use super::super::join;
use super::{Seen, base64, changes, git_status, line_ending, version, version_time};

#[test]
fn paths_join_under_the_workspace_and_absolute_ones_stay() {
    assert_eq!(join("/repo", ""), "/repo");
    assert_eq!(join("/repo/", "src/a.rs"), "/repo/src/a.rs");
    assert_eq!(join("/repo", "/etc/hosts"), "/etc/hosts");
    assert_eq!(join("~", "notes"), "~/notes");
    assert_eq!(join("", "a"), "a");
}

#[test]
fn base64_is_the_standard_alphabet_padded() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"a"), "YQ==");
    assert_eq!(base64(b"ab"), "YWI=");
    assert_eq!(base64(b"abc"), "YWJj");
    assert_eq!(base64(&[0xff, 0xfe, 0xfd, 0x00]), "//79AA==");
}

#[test]
fn a_version_carries_the_time_a_save_checks() {
    let v = version(12, slopty_core::WallMs::from_millis(1_700));
    assert_eq!(version_time(&v), Some(slopty_core::WallMs::from_millis(1_700)));
    assert_eq!(version_time("garbage"), None);
}

#[test]
fn line_endings_are_told_apart() {
    assert_eq!(line_ending("one"), WorkspaceLineEnding::None);
    assert_eq!(line_ending("a\nb\n"), WorkspaceLineEnding::Lf);
    assert_eq!(line_ending("a\r\nb\r\n"), WorkspaceLineEnding::Crlf);
    assert_eq!(line_ending("a\r\nb\n"), WorkspaceLineEnding::Mixed);
}

/// A folder listed again says what was made, changed and removed in it; a folder's own time
/// moving is not a change to it.
#[test]
fn a_folder_listed_again_says_what_changed() {
    let entry = |name: &str, dir, modified| Seen { name: name.to_owned(), dir, size: 1, modified };
    let was = [entry("a.rs", false, 1), entry("gone.rs", false, 1), entry("src", true, 1)];
    let now = [entry("a.rs", false, 2), entry("new.rs", false, 1), entry("src", true, 5)];
    let found: Vec<_> = changes("lib", &was, &now).into_iter().map(|c| (c.kind, c.path)).collect();
    assert_eq!(
        found,
        [
            (WorkspaceFileChangeKind::Modified, "lib/a.rs".to_owned()),
            (WorkspaceFileChangeKind::Created, "lib/new.rs".to_owned()),
            (WorkspaceFileChangeKind::Removed, "lib/gone.rs".to_owned()),
        ]
    );
}

/// git's two letters become the index's and the working tree's states; ignored files are left
/// out; a folder that is not a repository has no status.
#[test]
fn git_status_letters_become_states() {
    let file =
        |path: &str, xy: &str| GitFile { path: path.to_owned(), from: None, xy: xy.to_owned() };
    let status = GitStatus {
        files: vec![file("a", ".M"), file("b", "??"), file("c", "UU"), file("d", "!!")],
        ..status_of("/repo")
    };
    let key = WorkerKey::new(7);
    let got = git_status(key, "/repo", GitOutcome::Done(GitDone::Status(Box::new(status))))
        .expect("a status");
    let states: Vec<_> = got.files.iter().map(|f| (f.path.as_str(), f.index, f.worktree)).collect();
    assert_eq!(
        states,
        [
            ("a", GitFileState::Unchanged, GitFileState::Modified),
            ("b", GitFileState::Untracked, GitFileState::Untracked),
            ("c", GitFileState::Unmerged, GitFileState::Unmerged),
        ]
    );
    assert!(got.complete);
    let inside = git_status(
        key,
        "/repo/web",
        GitOutcome::Done(GitDone::Status(Box::new(GitStatus {
            files: vec![file("web/a.ts", ".M"), file("api/b.rs", ".M")],
            ..status_of("/repo")
        }))),
    )
    .expect("a status");
    let paths: Vec<_> = inside.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.ts"], "a workspace inside the repository sees its own files");
    assert!(
        git_status(key, "/x", GitOutcome::Refused { why: "not a repository".to_owned() }).is_none()
    );
}

fn status_of(root: &str) -> GitStatus {
    GitStatus {
        root: root.to_owned(),
        forge: None,
        branch: Some("main".to_owned()),
        head: None,
        upstream: None,
        merge_base: None,
        ahead: 0,
        behind: 0,
        files: Vec::new(),
        more: 0,
    }
}

/// The tree's and the repository's watches against a fake link: they ask the worker nothing
/// while nothing changes, take what it says unasked, and follow again on the next link.
mod watches {
    use std::time::Duration;

    use futures::stream::BoxStream;
    use futures::{FutureExt as _, StreamExt as _};
    use gpui::{AppContext as _, Entity, VisualTestContext};
    use slopty_core::WallMs;
    use slopty_proto::folder::{FolderEntry, Listing};
    use slopty_proto::git::{GitDone, GitFile, GitOp, GitOutcome};
    use slopty_proto::orchestration::FileKind;
    use slopty_proto::{ClientMsg, RequestId, WorkerMsg};
    use slopty_view_proto::{
        WorkspaceFileChangeKind, WorkspaceFileChanges, WorkspaceGitStatusFrame,
    };
    use slopty_view_rpc::{RpcReply, methods};
    use tokio::sync::mpsc;

    use super::status_of;
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac};

    const ROOT: &str = "/w/app";

    /// A workspace linked to its one worker: what it sends the worker comes out of the receiver.
    fn linked(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<WorkspaceView>, &mut VisualTestContext, mpsc::Receiver<ClientMsg>) {
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        (workspace, cx, rx)
    }

    /// Open the watch `method` on the workspace at [`ROOT`].
    fn open(
        workspace: &Entity<WorkspaceView>,
        cx: &mut VisualTestContext,
        method: &str,
    ) -> BoxStream<'static, serde_json::Value> {
        let params = serde_json::json!({ "spaceId": format!("{KEY}:{ROOT}") });
        let task = workspace
            .update(cx, |v, cx| v.view_surface_call(method, &params, cx))
            .expect("a surface's call");
        match task.now_or_never().expect("answered at once").expect("a watch") {
            RpcReply::Stream(frames) => frames,
            RpcReply::Value(value) => panic!("a stream, not {value}"),
        }
    }

    /// What the worker was sent by now, `for_a_while` of the clock run first.
    fn sent(
        rx: &mut mpsc::Receiver<ClientMsg>,
        cx: &VisualTestContext,
        for_a_while: Duration,
    ) -> Vec<ClientMsg> {
        cx.executor().advance_clock(for_a_while);
        cx.run_until_parked();
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// The frames a watch has sent by now.
    fn frames<T: serde::de::DeserializeOwned>(
        stream: &mut BoxStream<'static, serde_json::Value>,
        cx: &VisualTestContext,
    ) -> Vec<T> {
        cx.run_until_parked();
        std::iter::from_fn(|| stream.next().now_or_never().flatten())
            .map(|frame| serde_json::from_value(frame).expect("a frame"))
            .collect()
    }

    /// The status asks among `sent`.
    fn status_asks(sent: &[ClientMsg]) -> Vec<RequestId> {
        sent.iter()
            .filter_map(|msg| match msg {
                ClientMsg::Git { request, repo, op: GitOp::Status } if repo == ROOT => {
                    Some(*request)
                }
                _ => None,
            })
            .collect()
    }

    fn watched_repos(sent: &[ClientMsg]) -> Vec<Vec<String>> {
        sent.iter()
            .filter_map(|msg| match msg {
                ClientMsg::WatchRepos { paths } => Some(paths.clone()),
                _ => None,
            })
            .collect()
    }

    fn answer(
        workspace: &Entity<WorkspaceView>,
        cx: &mut VisualTestContext,
        request: RequestId,
        files: Vec<GitFile>,
    ) {
        let status = slopty_proto::git::GitStatus { files, ..status_of(ROOT) };
        let outcome = GitOutcome::Done(GitDone::Status(Box::new(status)));
        let done = WorkerMsg::GitDone { request, outcome };
        workspace.update(cx, |v, _| assert!(v.surface_heard(KEY, &done), "the watch's answer"));
    }

    fn moved(workspace: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
        let word = WorkerMsg::RepoMoved { repo: ROOT.to_owned() };
        let _taken = workspace.update(cx, |v, _| v.surface_heard(KEY, &word));
    }

    /// The status watch asks once, has the worker follow the repository, and asks again only
    /// on its word: never on a clock, one ask for words that came while one was out, again on
    /// the next link, and the worker lets the repository go when the shell lets the watch go.
    #[gpui::test]
    fn a_status_watch_asks_only_when_the_repository_moved(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = linked(cx);
        let mut stream = open(&workspace, cx, methods::WATCH_WORKSPACE_GIT_STATUS);
        let first = sent(&mut rx, cx, Duration::ZERO);
        assert_eq!(watched_repos(&first), [vec![ROOT.to_owned()]], "followed: {first:?}");
        let asks = status_asks(&first);
        assert_eq!(asks.len(), 1, "asked once: {first:?}");
        answer(&workspace, cx, asks[0], Vec::new());
        let shown: Vec<WorkspaceGitStatusFrame> = frames(&mut stream, cx);
        assert_eq!(shown.len(), 1, "the first status shows");

        let idle = sent(&mut rx, cx, Duration::from_secs(60));
        assert!(status_asks(&idle).is_empty(), "a minute idle asks nothing: {idle:?}");

        moved(&workspace, cx);
        moved(&workspace, cx);
        let asks = status_asks(&sent(&mut rx, cx, Duration::ZERO));
        assert_eq!(asks.len(), 1, "one ask for the word");
        moved(&workspace, cx);
        let file = GitFile { path: "src/a.rs".to_owned(), from: None, xy: ".M".to_owned() };
        answer(&workspace, cx, asks[0], vec![file]);
        let shown: Vec<WorkspaceGitStatusFrame> = frames(&mut stream, cx);
        assert_eq!(shown.len(), 1, "the change shows: {shown:?}");
        let again = status_asks(&sent(&mut rx, cx, Duration::ZERO));
        assert_eq!(again.len(), 1, "the word heard while asking asks once more");
        answer(&workspace, cx, again[0], Vec::new());
        assert!(status_asks(&sent(&mut rx, cx, Duration::from_secs(60))).is_empty(), "idle");

        // The link drops and comes back: the worker that forgot is told again, and what the old
        // link may have missed is asked.
        let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let relinked = sent(&mut rx, cx, Duration::ZERO);
        assert_eq!(watched_repos(&relinked), [vec![ROOT.to_owned()]], "{relinked:?}");
        assert_eq!(status_asks(&relinked).len(), 1, "asked again: {relinked:?}");

        drop(stream);
        let closed = sent(&mut rx, cx, Duration::ZERO);
        assert_eq!(watched_repos(&closed), [Vec::<String>::new()], "let go: {closed:?}");
    }

    /// The tree's watch asks for no listing: the worker follows the folders the shell listed,
    /// and what it lists again unasked reaches the shell as what changed. On the next link it
    /// is told the folders again, and the shell reads the tree again for what the old link
    /// missed.
    #[gpui::test]
    fn a_tree_watch_takes_the_workers_listings_and_asks_none(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = linked(cx);
        let entry = |name: &str, size| FolderEntry {
            name: name.to_owned(),
            kind: FileKind::File,
            link: false,
            hidden: false,
            size,
            items: None,
            repo: false,
            modified_ms: WallMs::from_millis(1),
        };
        let listing = |entries: Vec<FolderEntry>| Listing::Listed {
            dir: ROOT.to_owned(),
            real: ROOT.to_owned(),
            total: u32::try_from(entries.len()).unwrap(),
            entries,
        };
        let mut stream = open(&workspace, cx, methods::WATCH_WORKSPACE_FILES);
        let params = serde_json::json!({ "spaceId": format!("{KEY}:{ROOT}") });
        let page = workspace
            .update(cx, |v, cx| v.view_surface_call(methods::LIST_WORKSPACE_DIRECTORY, &params, cx))
            .expect("a surface's call");
        let asked = sent(&mut rx, cx, Duration::ZERO);
        assert!(asked.iter().any(|m| matches!(m, ClientMsg::ListFolder { path } if path == ROOT)));
        let first =
            WorkerMsg::Folder { path: ROOT.to_owned(), listing: listing(vec![entry("a.rs", 1)]) };
        let _taken = workspace.update(cx, |v, _| v.surface_heard(KEY, &first));
        cx.run_until_parked();
        drop(page);
        let following = sent(&mut rx, cx, Duration::ZERO);
        let folders = |sent: &[ClientMsg]| -> Vec<Vec<String>> {
            sent.iter()
                .filter_map(|m| match m {
                    ClientMsg::WatchFolders { paths } => Some(paths.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(folders(&following), [vec![ROOT.to_owned()]], "{following:?}");

        let idle = sent(&mut rx, cx, Duration::from_secs(60));
        let asks =
            |m: &ClientMsg| matches!(m, ClientMsg::ListFolder { .. } | ClientMsg::Git { .. });
        assert!(!idle.iter().any(asks), "a minute idle asks nothing: {idle:?}");

        let again = listing(vec![entry("a.rs", 2), entry("b.rs", 1)]);
        let unasked = WorkerMsg::Folder { path: ROOT.to_owned(), listing: again };
        let _taken = workspace.update(cx, |v, _| v.surface_heard(KEY, &unasked));
        let changed: Vec<WorkspaceFileChanges> = frames(&mut stream, cx);
        let kinds: Vec<_> = changed
            .iter()
            .flat_map(|frame| frame.changes.iter().map(|c| (c.kind, c.path.clone())))
            .collect();
        assert_eq!(
            kinds,
            [
                (WorkspaceFileChangeKind::Modified, "a.rs".to_owned()),
                (WorkspaceFileChangeKind::Created, "b.rs".to_owned()),
            ]
        );

        let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let relinked = sent(&mut rx, cx, Duration::ZERO);
        assert_eq!(folders(&relinked), [vec![ROOT.to_owned()]], "told again: {relinked:?}");
        let resync: Vec<WorkspaceFileChanges> = frames(&mut stream, cx);
        assert!(resync.iter().any(|f| f.resync_required), "read again: {resync:?}");

        drop(stream);
        let closed = sent(&mut rx, cx, Duration::ZERO);
        assert_eq!(folders(&closed), [Vec::<String>::new()], "let go: {closed:?}");
    }
}
