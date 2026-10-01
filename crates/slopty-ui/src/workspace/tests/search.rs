//! Search in files in the headless workspace: the directory it asks the worker to search, the
//! pages it groups by file as they come, the toggles, a stale page dropped, a match opened as
//! a file tile at its line, and the search stopped when the surface closes.

use slopty_proto::search::{
    FileHits, FileReplaced, FileStamp, LineHit, MatchAt, SearchEvent, SearchQuery, SearchRequest,
    SearchSummary, SkipReason, Skipped, Span,
};

use super::*;
use crate::search::DEBOUNCE;

fn hit(line: u32, text: &str, start: u32, end: u32) -> LineHit {
    LineHit {
        line,
        text: text.to_owned(),
        spans: vec![Span { start, end }],
        cut_before: false,
        cut_after: false,
    }
}

/// `path`'s matching `lines`, stamped as a file of that many bytes.
fn file(path: &str, lines: Vec<LineHit>) -> FileHits {
    let size = u64::try_from(lines.len()).unwrap_or(0).saturating_mul(10);
    FileHits {
        path: path.to_owned(),
        stamp: FileStamp { size, modified_ns: 7 },
        lines,
        context: Vec::new(),
    }
}

/// The searches the workspace asked `fake` for, in order.
fn requests(fake: &mut Fake) -> Vec<SearchRequest> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Search(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// Type `text` into the focused field and let the typing pause.
fn type_query(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    cx.executor().advance_clock(DEBOUNCE);
    cx.run_until_parked();
}

fn deliver(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    event: SearchEvent,
) {
    view.update_in(cx, |v, _w, cx| v.search_event(key, event, cx));
    cx.run_until_parked();
}

/// The results list as a screen reader reads it.
fn rows(cx: &mut VisualTestContext) -> Vec<String> {
    tree(cx).into_iter().filter(|n| n.role == "ListBoxOption").filter_map(|n| n.label).collect()
}

/// The selected row: its file, and the number of its line when it is one.
fn selected(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<(String, Option<u32>)> {
    view.read_with(cx, |v, cx| {
        let search = v.search_view()?;
        let search = search.read(cx);
        let (file, line) = search.selected()?;
        Some((file.path.clone(), line.map(|l| l.line)))
    })
}

/// ⌥⌘F over a shell searches its directory once the typing pauses; pages arriving out of
/// order are grouped by file in path order with the first match selected, a page for an
/// earlier query is dropped, the foot counts what was found, and ↩ on a line hides the
/// surface and opens the file tile landing on that line.
#[gpui::test]
fn a_search_streams_grouped_matches_and_opens_one_at_its_line(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/proj"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    studio.drain();

    cx.simulate_keystrokes("cmd-alt-f");
    cx.run_until_parked();
    assert!(cx.debug_bounds("search").is_some(), "the surface is up");
    assert!(requests(&mut studio).is_empty(), "nothing is searched before a query");
    cx.simulate_input("nee");
    cx.executor().advance_clock(DEBOUNCE.checked_div(2).unwrap());
    type_query(cx, "dle");
    let asked = requests(&mut studio);
    let [SearchRequest::Start { id, root, query }] = asked.as_slice() else {
        panic!("one search for the paused typing: {asked:#?}");
    };
    assert_eq!(root, "/w/proj", "the shell's directory");
    assert_eq!(query, &SearchQuery { pattern: "needle".into(), ..SearchQuery::default() });
    let id = *id;

    let key = studio.key;
    let stale = SearchEvent::Hits {
        id: id.wrapping_sub(1),
        files: vec![file("old.rs", vec![hit(1, "needle", 0, 6)])],
    };
    deliver(&view, cx, key, stale);
    let z = file("src/z.rs", vec![hit(3, "let needle = 1;", 4, 10), hit(9, "needle();", 0, 6)]);
    let a = file("a.md", vec![hit(1, "a needle", 2, 8)]);
    deliver(&view, cx, key, SearchEvent::Hits { id, files: vec![z] });
    deliver(&view, cx, key, SearchEvent::Hits { id, files: vec![a] });
    assert_eq!(
        rows(cx),
        [
            "a.md, 1 match",
            "Line 1: a needle",
            "src/z.rs, 2 matches",
            "Line 3: let needle = 1;",
            "Line 9: needle();",
        ],
        "grouped by file in path order, the stale page dropped"
    );
    assert_eq!(selected(&view, cx), Some(("a.md".into(), Some(1))), "the first match is selected");

    let summary = SearchSummary { files: 2, lines: 3, searched: 40, capped: false, elapsed_ms: 9 };
    deliver(&view, cx, key, SearchEvent::Done { id, summary });
    let said = tree(cx).into_iter().any(|n| n.is("Status", Some("3 results in 2 files")));
    assert!(said, "the foot counts what was found");

    cx.simulate_keystrokes("down down");
    assert_eq!(selected(&view, cx), Some(("src/z.rs".into(), Some(3))));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("search").is_none(), "the surface hides");
    let sent = studio.drain();
    let opened = sent.iter().find_map(|m| match m {
        ClientMsg::Items(ItemOp::Add(Item { id, kind: ItemKind::File { path }, .. })) => {
            Some((*id, path.clone()))
        }
        _ => None,
    });
    let (file, path) = opened.unwrap_or_else(|| panic!("a file tile: {sent:#?}"));
    assert_eq!(path, "/w/proj/src/z.rs");
    assert!(
        !sent.iter().any(|m| matches!(m, ClientMsg::Search(_))),
        "a finished search has nothing to stop: {sent:#?}"
    );
    let text = slopty_proto::file::FileRead::Text {
        text: "one\ntwo\nlet needle = 1;\nfour\n".to_owned(),
        size: 32,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    view.update_in(cx, |v, _w, cx| v.file_read(key, &path, &text, cx));
    cx.run_until_parked();
    let line =
        view.read_with(cx, |v, cx| v.files.get(&file).and_then(|f| f.read(cx).reading_line(cx)));
    assert_eq!(line, Some(3), "the tile lands on the match's line");
}

/// The toggles search again with what they set; Esc closes the surface and stops the search
/// still going; opening it again over the same directory shows the same query and searches
/// it again.
#[gpui::test]
fn the_toggles_search_again_and_closing_stops_the_search(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let folder = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/lib".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    cx.run_until_parked();
    studio.drain();

    cx.simulate_keystrokes("cmd-alt-f");
    cx.run_until_parked();
    type_query(cx, "Foo");
    cx.simulate_keystrokes("cmd-alt-c");
    cx.run_until_parked();
    let asked = requests(&mut studio);
    let starts: Vec<_> = asked
        .iter()
        .filter_map(|r| match r {
            SearchRequest::Start { root, query, .. } => Some((root.as_str(), query.match_case)),
            SearchRequest::Stop { .. } | SearchRequest::Replace(_) => None,
        })
        .collect();
    assert_eq!(starts, [("/w/lib", false), ("/w/lib", true)], "{asked:#?}");
    let case = tree(cx).into_iter().find(|n| n.label.as_deref() == Some(crate::search::MATCH_CASE));
    assert!(case.is_some(), "the toggle is there by name");

    view.read_with(cx, |v, cx| {
        let search = v.search_view().expect("kept");
        assert!(
            search.read(cx).results().is_some_and(slopty_client::search::SearchResults::running)
        );
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("search").is_none(), "Esc closes it");
    let last = view.read_with(cx, |v, cx| {
        v.search_view()
            .and_then(|s| s.read(cx).results().map(slopty_client::search::SearchResults::id))
    });
    let asked = requests(&mut studio);
    assert_eq!(asked, [SearchRequest::Stop { id: last.expect("a search") }], "the search stops");

    cx.simulate_keystrokes("cmd-alt-f");
    cx.run_until_parked();
    let again = requests(&mut studio);
    assert!(
        matches!(again.as_slice(), [SearchRequest::Start { query, .. }] if query.pattern == "Foo" && query.match_case),
        "the stopped search runs again as it was: {again:#?}"
    );
}

/// Whether a status on screen, the foot's among them, says `text`.
fn says(cx: &mut VisualTestContext, text: &str) -> bool {
    tree(cx).into_iter().any(|n| n.is("Status", Some(text)))
}

/// Replace across files. With the replace field in use each match reads struck through with
/// its replacement after it. ↩ there replaces the selected line's matches alone, and once the
/// answer takes that line away the selection is on the next match. ⌘↩ then replaces every
/// match left, each file named with the stamp it was searched at (the replaced file's new one).
/// A file the worker skipped as changed on disk stays in the list, and the foot says so.
#[gpui::test]
fn replace_one_line_then_every_match_and_hear_what_was_skipped(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/proj"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    studio.drain();

    cx.simulate_keystrokes("cmd-alt-f");
    cx.run_until_parked();
    type_query(cx, "needle");
    let asked = requests(&mut studio);
    let [SearchRequest::Start { id, .. }] = asked.as_slice() else { panic!("{asked:#?}") };
    let (id, key) = (*id, studio.key);
    let a = file("a.rs", vec![hit(2, "let needle = 1;", 4, 10), hit(7, "needle();", 0, 6)]);
    let b = file("b.rs", vec![hit(1, "a needle", 2, 8)]);
    let (a_stamp, b_stamp) = (a.stamp, b.stamp);
    deliver(&view, cx, key, SearchEvent::Hits { id, files: vec![a, b] });
    let summary = SearchSummary { files: 2, lines: 3, ..SearchSummary::default() };
    deliver(&view, cx, key, SearchEvent::Done { id, summary });

    cx.simulate_keystrokes("tab");
    cx.simulate_input("thread");
    cx.run_until_parked();
    let drawn = view.read_with(cx, |v, cx| {
        let search = v.search_view()?;
        let search = search.read(cx);
        let (_, line) = search.selected()?;
        Some(search.drawn_line(line?))
    });
    assert_eq!(drawn.as_deref(), Some("let needlethread = 1;"), "struck, then its replacement");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let asked = requests(&mut studio);
    let [SearchRequest::Replace(one)] = asked.as_slice() else { panic!("{asked:#?}") };
    assert_eq!((one.root.as_str(), one.with.as_str()), ("/w/proj", "thread"));
    let [only] = one.files.as_slice() else { panic!("{one:#?}") };
    assert_eq!((only.path.as_str(), only.stamp), ("a.rs", a_stamp));
    assert_eq!(only.matches, [MatchAt { line: 2, index: 0 }], "the selected line alone");
    let a_now = FileStamp { size: 22, modified_ns: 8 };
    let done = FileReplaced { path: "a.rs".into(), matches: 1, stamp: a_now };
    let answer = SearchEvent::Replaced { id: one.id, files: vec![done], skipped: Vec::new() };
    deliver(&view, cx, key, answer);
    assert_eq!(
        rows(cx),
        ["a.rs, 1 match", "Line 7: needle();", "b.rs, 1 match", "Line 1: a needle"],
        "the replaced line is gone"
    );
    assert_eq!(selected(&view, cx), Some(("a.rs".into(), Some(7))), "on to the next match");
    assert!(says(cx, "Replaced 1 match in 1 file"), "the foot says what it did");

    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    let asked = requests(&mut studio);
    let [SearchRequest::Replace(all)] = asked.as_slice() else { panic!("{asked:#?}") };
    let named: Vec<(&str, FileStamp, &[MatchAt])> =
        all.files.iter().map(|f| (f.path.as_str(), f.stamp, f.matches.as_slice())).collect();
    assert_eq!(
        named,
        [
            ("a.rs", a_now, &[MatchAt { line: 7, index: 0 }][..]),
            ("b.rs", b_stamp, &[MatchAt { line: 1, index: 0 }][..]),
        ]
    );
    let done = FileReplaced { path: "a.rs".into(), matches: 1, stamp: a_now };
    let skipped = vec![Skipped { path: "b.rs".into(), why: SkipReason::Changed }];
    deliver(&view, cx, key, SearchEvent::Replaced { id: all.id, files: vec![done], skipped });
    assert_eq!(rows(cx), ["b.rs, 1 match", "Line 1: a needle"], "the skipped file stays");
    let skipped = "Replaced 1 match in 1 file. 1 file changed since the search and was skipped";
    assert!(says(cx, skipped), "the foot says what was skipped");
}
