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
