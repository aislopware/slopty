//! A path typed into the folder step completes from the machine's folders.

use slopty_core::WorkerId;
use slopty_proto::folder::{FolderEntry, Listing};
use slopty_proto::orchestration::FileKind;
use slopty_proto::thread::AgentId;

use super::super::actions::NewAgent;
use super::super::agent_start::TYPED_FOLDER;
use super::super::projects::worker_key;
use super::*;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn step_lines(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).matches().iter().map(|l| l.label.clone()).collect())
    })
    .unwrap_or_default()
}

fn listed(fake: &mut Fake) -> Vec<String> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::ListFolder { path } => Some(path),
            _ => None,
        })
        .collect()
}

fn entry(name: &str, kind: FileKind, hidden: bool) -> FolderEntry {
    FolderEntry {
        name: name.to_owned(),
        kind,
        link: false,
        hidden,
        size: 0,
        items: None,
        modified_ms: WallMs::ZERO,
    }
}

/// `~/w` typed in the folder step asks the machine for `~` once, and the folders there that
/// begin with `w`, any case, follow the line for the path as typed; a file, a hidden folder
/// and the other folders do not. Typing on in the same folder asks nothing more.
#[gpui::test]
fn a_typed_path_completes_from_the_machines_folders(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, worker_key(WorkerId::new()).value(), "studio");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    let codex = AgentId::named(AgentId::CODEX);
    let caps = WorkerCaps {
        agents: vec![slopty_proto::server::InstalledAgent {
            agent: codex,
            version: "1.0".to_owned(),
            offers: slopty_proto::thread::Offers::default(),
        }],
        ..healthy()
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, caps, cx);
        v.focus_tile(shell, cx);
    });
    settle(cx);
    studio.drain();
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("~/w");
    settle(cx);
    assert_eq!(listed(&mut studio), ["~"], "the folder it is in, asked once");
    let listing = Listing::Listed {
        dir: "/Users/c".to_owned(),
        entries: vec![
            entry("Work", FileKind::Dir, false),
            entry("www", FileKind::Dir, false),
            entry("notes", FileKind::Dir, false),
            entry(".wine", FileKind::Dir, true),
            entry("wiki.md", FileKind::File, false),
        ],
        total: 5,
    };
    view.update_in(cx, |v, _w, cx| v.folder_listed(key, "~", &listing, cx));
    settle(cx);
    let lines = step_lines(&view, cx);
    let typed = format!("{TYPED_FOLDER} ~/w");
    let at = |line: &str| lines.iter().position(|l| l == line);
    assert!(at(&typed).is_some(), "{lines:?}");
    assert!(at("~/Work").is_some() && at("~/www").is_some(), "{lines:?}");
    assert!(at(&typed) < at("~/Work"), "after the path as typed: {lines:?}");
    for not in ["~/notes", "~/.wine", "~/wiki.md"] {
        assert!(at(not).is_none(), "{not}: {lines:?}");
    }
    cx.simulate_input("w");
    settle(cx);
    assert!(listed(&mut studio).is_empty(), "the same folder: nothing asked");
    let lines = step_lines(&view, cx);
    assert!(lines.iter().any(|l| l == "~/www") && !lines.iter().any(|l| l == "~/Work"));
}
