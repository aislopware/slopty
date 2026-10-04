//! Search in files in the headless workspace: the directory it asks the worker to search, the
//! pages it groups by file as they come, the toggles, a stale page dropped, a match opened as
//! a file tile at its line, the search stopped when the surface closes, and the same query
//! on the open tiles.

use slopty_proto::search::{
    FileHits, LineHit, SearchEvent, SearchQuery, SearchRequest, SearchSummary, Span,
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

/// `path`'s matching `lines`.
fn file(path: &str, lines: Vec<LineHit>) -> FileHits {
    FileHits { path: path.to_owned(), lines, context: Vec::new() }
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

    cx.simulate_keystrokes("cmd-shift-f");
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

    cx.simulate_keystrokes("cmd-shift-f");
    cx.run_until_parked();
    type_query(cx, "Foo");
    cx.simulate_keystrokes("cmd-alt-c");
    cx.run_until_parked();
    let asked = requests(&mut studio);
    let starts: Vec<_> = asked
        .iter()
        .filter_map(|r| match r {
            SearchRequest::Start { root, query, .. } => Some((root.as_str(), query.match_case)),
            SearchRequest::Stop { .. } => None,
        })
        .collect();
    assert_eq!(starts, [("/w/lib", false), ("/w/lib", true)], "{asked:#?}");
    let case =
        tree(cx).into_iter().find(|n| n.label.as_deref() == Some(crate::kit::find::MATCH_CASE));
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

    cx.simulate_keystrokes("cmd-shift-f");
    cx.run_until_parked();
    let again = requests(&mut studio);
    assert!(
        matches!(again.as_slice(), [SearchRequest::Start { query, .. }] if query.pattern == "Foo" && query.match_case),
        "the stopped search runs again as it was: {again:#?}"
    );
}

/// ⌘⇧F's scope chip turns the query on the open tiles: each shell is asked for the query as
/// one pattern, its toggles written in, and a shell's answer is a row with its count. ↩ goes
/// to that shell with its own find bar open on the query.
#[gpui::test]
fn the_tiles_scope_finds_in_the_open_tiles_and_goes_to_one(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (one, two) = (SessionId::new(), SessionId::new());
    let first = opens_in(&view, cx, &studio, one, studio.me, 1, Some("/w/a"));
    opens_in(&view, cx, &studio, two, studio.me, 2, Some("/w/b"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-f");
    cx.run_until_parked();
    let chip = cx.debug_bounds("search-scope-tiles").expect("the scope chip");
    cx.simulate_click(chip.center(), Modifiers::default());
    cx.run_until_parked();
    // The click left the keyboard on the chip; the field takes it back.
    view.update_in(cx, |v, window, cx| {
        if let Some(search) = v.search_view() {
            search.update(cx, |s, cx| s.focus(window, cx));
        }
    });
    type_query(cx, "needle");
    let asked: Vec<(SessionId, String, bool)> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Term { session, req: TermRequest::Search { needle, regex, .. } } => {
                Some((session, needle, regex))
            }
            _ => None,
        })
        .collect();
    let pattern = "(?im)needle".to_owned();
    assert!(asked.contains(&(one, pattern.clone(), true)), "{asked:?}");
    assert!(asked.contains(&(two, pattern.clone(), true)), "{asked:?}");

    let answer = |total| TermEvent::Matches { needle: pattern.clone(), total, matches: Vec::new() };
    view.update_in(cx, |v, _w, cx| {
        v.term_event(one, answer(0), cx);
        v.term_event(two, answer(3), cx);
    });
    cx.run_until_parked();
    let listed = rows(cx);
    assert_eq!(listed.len(), 1, "the shell with no hit is left out: {listed:?}");
    assert!(listed[0].ends_with("3 matches"), "{listed:?}");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("search").is_none(), "the surface hides");
    let query = view
        .read_with(cx, |v, cx| v.terminal(two).and_then(|t| t.read(cx).search_query().cloned()));
    assert_eq!(query.map(|q| q.needle), Some("needle".to_owned()), "its own find bar, open");
}
