use slopty_client::WorkerKey;
use slopty_proto::thread::detail::{Hunk, Patch};
use slopty_proto::thread::wire::{Against, FileDiff, FileKind, Review, ReviewScope};

use super::written;

fn file(
    path: &str,
    from: Option<&str>,
    to: Option<&str>,
    kind: FileKind,
    hunks: Vec<Hunk>,
) -> FileDiff {
    FileDiff {
        path: path.to_owned(),
        old_path: None,
        from: from.map(str::to_owned),
        to: to.map(str::to_owned),
        kind,
        modes: None,
        patch: Patch { hunks, added: 1, removed: 1, clipped_lines: 0, full: None },
    }
}

fn review(files: Vec<FileDiff>) -> Review {
    Review {
        scope: ReviewScope::WorkingTree(Against::Head),
        from: None,
        to: None,
        files,
        absent: None,
    }
}

/// A review is written as `git diff` writes it: a header per file, its kind's lines, its hunks
/// with their headings, and a summary row per file with its counts and status.
#[test]
fn a_review_is_written_as_a_git_patch() {
    let hunk = Hunk {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 1,
        heading: Some("fn main()".to_owned()),
        lines: vec!["-old".to_owned(), "+new".to_owned()],
    };
    let files = vec![
        file("src/main.rs", Some("aaa"), Some("bbb"), FileKind::Text, vec![hunk]),
        file("new.txt", None, Some("ccc"), FileKind::Text, Vec::new()),
        file("logo.png", Some("ddd"), Some("eee"), FileKind::Image { bytes: 9 }, Vec::new()),
    ];
    let out = written(WorkerKey::new(1), "/repo", &review(files));
    let patch = out.diff.patch;
    assert!(patch.starts_with("diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@ fn main()\n-old\n+new\n"), "{patch}");
    assert!(patch.contains("diff --git a/new.txt b/new.txt\nnew file mode 100644\n"), "{patch}");
    assert!(patch.contains("Binary files a/logo.png and b/logo.png differ"), "{patch}");
    let statuses: Vec<_> = out.diff.files.iter().map(|f| f.status.as_str()).collect();
    assert_eq!(statuses, ["modified", "added", "modified"]);
    assert!(out.diff.files.iter().any(|f| f.binary), "the picture is binary");
    assert_eq!((out.diff.additions, out.diff.deletions), (3, 3));
    assert_eq!(out.sides.blobs.get("new.txt"), Some(&(None, Some("ccc".to_owned()))));
}

/// The checksum moves with the patch and stays with it, so a watch sends only what changed.
#[test]
fn the_checksum_follows_the_patch() {
    let one = |text: &str| {
        let hunk = Hunk {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            heading: None,
            lines: vec![format!("+{text}")],
        };
        let files = vec![file("a", Some("x"), Some("y"), FileKind::Text, vec![hunk])];
        written(WorkerKey::new(1), "/repo", &review(files)).diff.checksum
    };
    assert_eq!(one("a"), one("a"));
    assert_ne!(one("a"), one("b"));
}

/// A tree with nothing changed writes no patch: the pane says the tree is clean.
#[test]
fn a_clean_tree_writes_nothing() {
    let out = written(WorkerKey::new(1), "/repo", &review(Vec::new()));
    assert!(out.diff.patch.is_empty() && out.diff.files.is_empty());
}
