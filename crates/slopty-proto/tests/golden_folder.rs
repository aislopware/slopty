//! Golden byte snapshots of a folder tile's pages and the changes it asks of the worker's files
//! (`slopty_proto::folder`). A changed snapshot is a wire change: accept it deliberately
//! (`cargo insta review`).

#[cfg(test)]
mod golden_folder {
    use slopty_core::WallMs;
    use slopty_proto::folder::{After, FolderEntry, FsOp, FsOutcome, FsRefusal, Listing};
    use slopty_proto::orchestration::FileKind;
    use slopty_proto::{ClientMsg, WorkerMsg, codec};

    fn hex(bytes: &[u8]) -> String {
        bytes
            .chunks(16)
            .map(|row| row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[track_caller]
    fn snap<T: serde::Serialize>(name: &str, msg: &T) {
        let bytes = codec::encode(msg).expect("encodes");
        insta::assert_snapshot!(name, hex(&bytes));
    }

    fn after() -> After {
        After { folder: false, name: "f01999.txt".to_owned() }
    }

    #[test]
    fn client_folder_page() {
        snap(
            "client_folder_page",
            &ClientMsg::FolderPage { path: "~/big".to_owned(), after: after() },
        );
    }

    #[test]
    fn worker_folder_page() {
        let entry = FolderEntry {
            name: "f02000.txt".to_owned(),
            kind: FileKind::File,
            link: false,
            hidden: false,
            size: 5,
            items: None,
            modified_ms: WallMs::from_millis(1_700_000_000_000),
        };
        let listing =
            Listing::Listed { dir: "/Users/dev/big".to_owned(), entries: vec![entry], total: 2001 };
        snap(
            "worker_folder_page",
            &WorkerMsg::FolderPage { path: "~/big".to_owned(), after: after(), listing },
        );
    }

    #[test]
    fn client_fs_ops() {
        let op = |request, op| ClientMsg::FsOp { request, op };
        snap(
            "client_fs_make_dir",
            &op(7, FsOp::MakeDir { parent: "~/src".to_owned(), name: "new folder".to_owned() }),
        );
        snap(
            "client_fs_move",
            &op(8, FsOp::Move { from: "~/src/a.txt".to_owned(), to: "~/src/b.txt".to_owned() }),
        );
        snap("client_fs_trash", &op(9, FsOp::Trash { path: "/w/old".to_owned() }));
    }

    #[test]
    fn worker_fs_done() {
        let done = |request, outcome| WorkerMsg::FsDone { request, outcome };
        snap(
            "worker_fs_done",
            &done(9, FsOutcome::Done { path: "/Users/dev/.Trash/old".to_owned() }),
        );
        snap(
            "worker_fs_failed",
            &done(9, FsOutcome::Failed { error: "Permission denied".to_owned() }),
        );
        let refused = [
            ("not_absolute", FsRefusal::NotAbsolute { path: "src".to_owned() }),
            ("bad_name", FsRefusal::BadName { name: "a/b".to_owned() }),
            ("protected", FsRefusal::Protected { path: "/Users/dev".to_owned() }),
            ("clash", FsRefusal::Clash { path: "/w/b.txt".to_owned() }),
            ("missing", FsRefusal::Missing { path: "/w/gone".to_owned() }),
            ("into_itself", FsRefusal::IntoItself),
            ("other_volume", FsRefusal::OtherVolume),
            ("no_trash", FsRefusal::NoTrash),
        ];
        for (name, why) in refused {
            snap(&format!("worker_fs_refused_{name}"), &done(8, FsOutcome::Refused(why)));
        }
    }
}
