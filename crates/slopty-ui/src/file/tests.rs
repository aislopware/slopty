//! A file tile in a headless GPUI window: real editor, real keys, the worker played by the
//! test through `set_read` and `written`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use slopty_client::unsaved::Unsaved;
use slopty_core::WallMs;

use super::*;

/// What the tile asked the workspace for, in order.
type Events = Rc<RefCell<Vec<FileViewEvent>>>;

fn text_read(text: &str, newline: bool, modified_ms: u64) -> FileRead {
    let size = u64::try_from(text.len()).unwrap_or(0).saturating_add(u64::from(newline));
    FileRead::Text {
        text: text.to_owned(),
        size,
        modified_ms: WallMs::from_millis(modified_ms),
        final_newline: newline,
    }
}

/// A tile for `path` in its own window, its events recorded, drawn once.
fn tile<'a>(
    cx: &'a mut TestAppContext,
    path: &str,
) -> (Entity<FileView>, Events, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
    });
    let path = path.to_owned();
    let (view, cx) = cx.add_window_view(|window, cx| {
        FileView::new(ItemId::new(), WorkerKey::new(1), &path, Theme::default(), window, cx)
    });
    let events: Events = Rc::default();
    let sink = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &FileViewEvent, _cx| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    cx.run_until_parked();
    (view, events, cx)
}

/// The worker answers with `read`; the next frame puts it in the editor.
fn arrives(view: &Entity<FileView>, cx: &mut VisualTestContext, read: FileRead) {
    view.update(cx, |v, cx| v.set_read(read, cx));
    cx.run_until_parked();
}

/// The caret goes to the start of the text and `typed` goes in, as keys would.
fn types(view: &Entity<FileView>, cx: &mut VisualTestContext, typed: &str) {
    view.update_in(cx, |v, window, cx| {
        v.focus(window, cx);
        v.editor().update(cx, |e, cx| e.set_selected_range(0..0, cx));
    });
    cx.run_until_parked();
    cx.simulate_input(typed);
    cx.run_until_parked();
}

fn text(view: &Entity<FileView>, cx: &VisualTestContext) -> String {
    view.read_with(cx, FileView::text)
}

fn click(cx: &mut VisualTestContext, selector: String) {
    let selector: &'static str = Box::leak(selector.into_boxed_str());
    let bounds = cx.debug_bounds(selector);
    assert!(bounds.is_some(), "{selector} is drawn");
    if let Some(bounds) = bounds {
        cx.simulate_click(bounds.center(), Modifiers::none());
    }
    cx.run_until_parked();
}

#[gpui::test]
fn cmd_s_sends_the_edit_based_on_the_version_it_started_from(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/src/main.rs");
    arrives(&view, cx, text_read("fn a() {}\nlet b = 1;", true, 1_000));
    assert_eq!(text(&view, cx), "fn a() {}\nlet b = 1;");
    assert!(!view.read_with(cx, |v, _| v.dirty()), "a fresh read is clean");

    types(&view, cx, "// hi\n");
    assert!(view.read_with(cx, |v, _| v.dirty()), "typing makes it dirty");
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save {
            text: "// hi\nfn a() {}\nlet b = 1;\n".to_owned(),
            base_modified_ms: Some(WallMs::from_millis(1_000)),
        }],
        "the whole text, the file's final newline kept, based on the read"
    );
    assert!(view.read_with(cx, |v, _| v.saving()));
    // ⌘S again while the first is out sends nothing.
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(events.borrow().len(), 1);

    view.update(cx, |v, cx| {
        v.written(WriteResult::Saved { size: 26, modified_ms: WallMs::from_millis(2_000) }, cx);
    });
    view.read_with(cx, |v, _| {
        assert!(!v.dirty() && !v.saving() && v.trouble().is_none(), "saved and clean");
    });
    // The watch's echo of the save changes nothing.
    arrives(&view, cx, text_read("// hi\nfn a() {}\nlet b = 1;", true, 2_000));
    view.read_with(cx, |v, _| assert!(!v.dirty() && v.trouble().is_none()));
    // A clean tile with nothing to save sends nothing.
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(events.borrow().len(), 1);
}

#[gpui::test]
fn a_file_without_a_final_newline_is_saved_without_one(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/notes.txt");
    arrives(&view, cx, text_read("one", false, 5));
    types(&view, cx, "z");
    view.update(cx, FileView::save);
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save {
            text: "zone".to_owned(),
            base_modified_ms: Some(WallMs::from_millis(5))
        }]
    );
}

#[gpui::test]
fn a_change_on_disk_under_an_edit_is_a_conflict_offered_inline(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/a.md");
    arrives(&view, cx, text_read("one\ntwo", true, 1_000));
    types(&view, cx, "mine ");
    // An agent rewrote the file meanwhile.
    arrives(&view, cx, text_read("one\nTWO", true, 1_500));
    view.read_with(cx, |v, _| {
        assert_eq!(v.trouble(), Some(&Trouble::Conflict));
        assert!(v.dirty(), "the edit is kept");
    });
    assert_eq!(text(&view, cx), "mine one\ntwo");
    // ⌘S does not write over it; the bar offers the two ways out.
    cx.simulate_keystrokes("cmd-s");
    assert!(events.borrow().is_empty());
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    click(cx, format!("file-overwrite-{id}"));
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save { text: "mine one\ntwo\n".to_owned(), base_modified_ms: None }],
        "overwrite writes whatever the disk has"
    );
    view.read_with(cx, |v, _| assert_eq!(v.trouble(), None));
}

#[gpui::test]
fn reload_drops_the_edit_for_the_disk_s_text(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/a.md");
    arrives(&view, cx, text_read("one\ntwo", true, 1_000));
    types(&view, cx, "mine ");
    cx.simulate_keystrokes("cmd-s");
    // The worker refused: the file changed since the read.
    view.update(cx, |v, cx| {
        v.written(WriteResult::Conflict { modified_ms: WallMs::from_millis(1_700) }, cx);
    });
    view.read_with(cx, |v, _| assert_eq!(v.trouble(), Some(&Trouble::Conflict)));
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    click(cx, format!("file-reload-{id}"));
    assert_eq!(events.borrow().last(), Some(&FileViewEvent::Reload), "the file is read again");
    arrives(&view, cx, text_read("theirs", true, 1_700));
    assert_eq!(text(&view, cx), "theirs");
    view.read_with(cx, |v, _| assert!(!v.dirty() && v.trouble().is_none()));
}

#[gpui::test]
fn a_clean_tile_takes_a_change_on_disk_and_tints_the_lines(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/src/lib.rs");
    arrives(&view, cx, text_read("a\nb\nc", true, 1));
    arrives(&view, cx, text_read("a\nB\nc\nd", true, 2));
    assert_eq!(text(&view, cx), "a\nB\nc\nd");
    view.read_with(cx, |v, cx| {
        assert_eq!(v.changed(), [1, 3]);
        assert_eq!(v.trouble(), None);
        assert!(!v.dirty());
        assert_eq!(v.reading_line(cx), Some(2), "the caret lands on the first change");
    });
    assert!(events.borrow().is_empty(), "a silent reload asks nothing");
}

/// A file past the cap is not put in the editor: the tile says why and offers to open it in a
/// terminal on the worker, in `$EDITOR` at the tile's line or in `$PAGER`.
#[gpui::test]
fn a_file_past_the_cap_offers_a_terminal_instead(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/logs/it's big.log");
    view.update(cx, |v, cx| v.focus_line(Some(12), cx));
    arrives(&view, cx, FileRead::TooLarge { size: 40 << 20 });
    view.read_with(cx, |v, cx| {
        assert_eq!(
            v.read_only(),
            Some("Too large to open here: 40.0 MB, over the 16.0 MB a tile opens")
        );
        assert_eq!(v.summary(cx), "too large, 40.0 MB");
    });
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    click(cx, format!("file-open-editor-{id}"));
    click(cx, format!("file-open-pager-{id}"));
    assert_eq!(
        events.borrow().as_slice(),
        [
            FileViewEvent::Run(
                "e=${EDITOR:-vi}; [ \"${e##*/}\" = slopty-editor ] && e=${VISUAL:-vi}; \
                 $e +12 '/w/logs/it'\\''s big.log'"
                    .to_owned()
            ),
            FileViewEvent::Run("${PAGER:-less} '/w/logs/it'\\''s big.log'".to_owned()),
        ]
    );
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(events.borrow().len(), 2, "nothing to save");
    assert_eq!(
        terminal_command("${PAGER:-less} 'a'"),
        ["/bin/sh", "-c", r#"exec "${SHELL:-/bin/sh}" -lic "$1""#, "sh", "${PAGER:-less} 'a'"],
        "the login shell runs the line, so its rc files set $EDITOR and $PAGER"
    );
}

/// A file the tile cannot show says what is so in a sentence, then why, as one block in the
/// body: its summary alone ("binary, 2 KB") had stood there lowercase, and the cap's reason
/// had run as one clause.
#[gpui::test]
fn a_file_that_cannot_open_says_what_is_so_then_why(cx: &mut TestAppContext) {
    let notice = |cx: &mut VisualTestContext| {
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        cx.update(|window, _cx| crate::a11y::tree(window))
            .into_iter()
            .find(|n| n.role == "Status")
            .and_then(|n| n.label)
            .unwrap_or_default()
    };
    let (view, _events, cx) = tile(cx, "/w/logo.png");
    arrives(&view, cx, FileRead::Binary { size: 2048 });
    assert_eq!(notice(cx), format!("{NOT_TEXT}: {}", size_label(2048)));
    arrives(&view, cx, FileRead::Missing { error: "No such file or directory".to_owned() });
    assert_eq!(notice(cx), "Cannot read this file: No such file or directory");
    arrives(&view, cx, FileRead::TooLarge { size: 40 << 20 });
    assert_eq!(notice(cx), "Too large to open here: 40.0 MB, over the 16.0 MB a tile opens");
    assert!(view.read_with(cx, |v, _| !v.shows_text()), "no text, so no caret to speak of");
    arrives(&view, cx, text_read("fn main() {}", true, 3));
    assert!(view.read_with(cx, |v, _| v.shows_text()), "the text, in its editor");
    for said in [TOO_LARGE, NOT_TEXT, CANNOT_READ] {
        assert!(said.starts_with(char::is_uppercase), "{said}");
    }
}

/// A file that grows past the cap under an edit keeps the edit, as any change on disk does.
#[gpui::test]
fn a_file_grown_past_the_cap_under_an_edit_keeps_it(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/grows.log");
    arrives(&view, cx, text_read("one", true, 1));
    types(&view, cx, "x");
    arrives(&view, cx, FileRead::TooLarge { size: FILE_BYTES + 1 });
    view.read_with(cx, |v, _| {
        assert!(v.dirty(), "the edit is kept");
        assert_eq!(v.trouble(), Some(&Trouble::Conflict));
    });
    assert_eq!(text(&view, cx), "xone");
}

/// The body of a Rust file of `lines` lines, each source-like.
fn source(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            format!("fn line_{i}() -> u32 {{ {i} * 2 + 1 }} // padding to a source-like width")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A 20 000-line file is edited like a short one: typed into near its end, found in, saved
/// whole. Ten times the old viewer's line limit.
#[gpui::test]
fn a_twenty_thousand_line_file_stays_editable(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/src/big.rs");
    let body = source(20_000);
    arrives(&view, cx, text_read(&body, true, 5));
    view.read_with(cx, |v, cx| {
        assert_eq!(v.line_count(cx), 20_000);
        assert!(v.read_only().is_none());
    });
    view.update_in(cx, |v, window, cx| {
        v.focus(window, cx);
        v.editor().update(cx, |e, cx| {
            let at = e.text().line_start_offset(19_990);
            e.set_selected_range(at..at, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_input("// edited near the end\n");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.dirty()));
    assert_eq!(view.read_with(cx, FileView::caret), (19_992, 1));

    view.update_in(cx, |v, window, cx| v.find_with("LINE_1999", window, cx));
    view.read_with(cx, |v, _| {
        let (hits, _) = v.hits().unwrap_or_default();
        assert!(hits.is_empty(), "a capital matches as typed");
    });
    view.update_in(cx, |v, window, cx| v.find_with("line_1999", window, cx));
    view.read_with(cx, |v, _| {
        let (hits, current) = v.hits().unwrap_or_default();
        assert_eq!(hits.len(), 11, "line_1999 and line_19990..=19999");
        assert_eq!(hits.first(), Some(&1_999));
        assert_eq!(hits.get(1), Some(&19_991), "the edit moved the rest a line down");
        assert!(current.is_some());
    });

    cx.simulate_keystrokes("cmd-s");
    let saved = events.borrow().iter().find_map(|e| match e {
        FileViewEvent::Save { text, base_modified_ms } => Some((text.clone(), *base_modified_ms)),
        _ => None,
    });
    let (saved, base) = saved.unwrap_or_default();
    assert_eq!(base, Some(WallMs::from_millis(5)));
    assert_eq!(saved.lines().count(), 20_001);
    assert!(saved.ends_with("// padding to a source-like width\n"), "the final newline");
    assert_eq!(saved.lines().nth(19_990), Some("// edited near the end"));
}

/// A file past [`COLOURED_BYTES`] is plain text: its whole-text parse would outlast the pauses
/// in typing. One within it is coloured.
#[gpui::test]
fn a_file_past_the_colour_limit_is_plain(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/src/gen.rs");
    arrives(&view, cx, text_read("fn a() {}", true, 1));
    assert_eq!(view.read_with(cx, |v, _| v.coloured_as()), Some("Rust"));
    let long = format!("fn a() {{}}\n{}", "x".repeat(COLOURED_BYTES));
    arrives(&view, cx, text_read(&long, true, 2));
    assert_eq!(view.read_with(cx, |v, _| v.coloured_as()), None);
    assert_eq!(view.read_with(cx, FileView::line_count), 2, "and still editable text");
}

#[test]
fn hit_lines_count_lines_across_a_case_fold_that_changes_lengths() {
    let text = "Alpha beta\nbeta\n\nGAMMA beta beta\nİstanbul\nend";
    let cases: [(&str, &[usize]); 10] = [
        ("beta", &[0, 1, 3]),
        ("Beta", &[]),
        ("gamma", &[3]),
        ("GAMMA", &[3]),
        ("a", &[0, 1, 3, 4]),
        ("\u{130}", &[4]),
        ("i\u{307}", &[4]),
        ("end", &[5]),
        ("", &[]),
        ("zz", &[]),
    ];
    for (needle, lines) in cases {
        assert_eq!(hit_lines(text, needle), lines, "{needle:?}");
    }
}

#[gpui::test]
fn a_failed_save_says_why_until_the_next_keystroke(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/ro.txt");
    arrives(&view, cx, text_read("x", true, 1));
    types(&view, cx, "y");
    view.update(cx, FileView::save);
    view.update(cx, |v, cx| {
        v.written(WriteResult::Failed { error: "Permission denied".to_owned() }, cx);
    });
    view.read_with(cx, |v, _| {
        assert_eq!(v.trouble(), Some(&Trouble::Failed("Permission denied".to_owned())));
        assert!(v.dirty(), "the edit is still there to save");
    });
    types(&view, cx, "z");
    view.read_with(cx, |v, _| assert_eq!(v.trouble(), None));
}

#[test]
fn hits_are_the_lines_holding_the_needle_with_smart_case() {
    let text = "Alpha\nbeta\nalpha beta\ngamma";
    assert_eq!(hit_lines(text, "alpha"), [0, 2], "no capital: any case");
    assert_eq!(hit_lines(text, "Alpha"), [0], "a capital: as typed");
    assert_eq!(hit_lines(text, "BETA"), Vec::<usize>::new());
    assert_eq!(hit_lines(text, ""), Vec::<usize>::new(), "an empty needle finds nothing");
}

#[test]
fn changed_lines_point_at_inserts_replacements_and_deletions() {
    assert_eq!(changed_lines("a\nb\nc", "a\nB\nc"), [1]);
    assert_eq!(changed_lines("a\nc", "a\nb\nb2\nc"), [1, 2]);
    assert_eq!(changed_lines("a\nb\nc", "a\nc"), [1]);
    assert_eq!(changed_lines("a\nb", "a"), [0], "a deleted tail points at the last line");
    assert_eq!(changed_lines("a", "a"), Vec::<usize>::new());
}

/// Before the worker answers, the tile is blank for the loading grace, so a read that lands
/// in time never flashes a word; past it, the tile says it is reading.
#[gpui::test]
fn reading_shows_only_after_the_grace(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/slow.rs");
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    let notice: &'static str = Box::leak(format!("file-notice-{id}").into_boxed_str());
    assert!(cx.debug_bounds(notice).is_none(), "blank within the grace");
    cx.executor().advance_clock(crate::screen::LOADING_GRACE);
    cx.run_until_parked();
    assert!(cx.debug_bounds(notice).is_some(), "past it, a word");
}

/// What a large file costs the tile on the UI thread, headless (no glyphs shaped: the frame
/// numbers are the smooth e2e's): the text put in the editor, a keystroke near the end with its
/// frame, a page scrolled with its frame, and a find over the whole text. 2 000, 20 000 and
/// 200 000 lines of source.
#[gpui::test]
#[ignore = "timing, run by hand with --ignored --nocapture"]
fn timing_of_a_large_file(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};
    let (view, _events, cx) = tile(cx, "/w/src/big.rs");
    for lines in [2_000_usize, 20_000, 200_000] {
        let body = source(lines);
        let t = Instant::now();
        arrives(&view, cx, text_read(&body, true, 1));
        let load = t.elapsed();
        view.update_in(cx, |v, window, cx| {
            v.focus(window, cx);
            v.editor().update(cx, |e, cx| {
                let at = e.text().line_start_offset(lines.saturating_sub(10));
                e.set_selected_range(at..at, cx);
            });
        });
        cx.run_until_parked();
        let rounds = 50_u32;
        let t = Instant::now();
        for _ in 0..rounds {
            cx.simulate_input("x");
            cx.run_until_parked();
        }
        let key = t.elapsed().checked_div(rounds).unwrap_or_default();
        let t = Instant::now();
        for round in 0..rounds {
            let offset =
                gpui::point(px(0.0), px(-(f32::from(u16::try_from(round).unwrap_or(0)) * 400.0)));
            view.update(cx, |v, cx| v.editor().update(cx, |e, cx| e.set_scroll_offset(offset, cx)));
            cx.run_until_parked();
        }
        let scroll = t.elapsed().checked_div(rounds).unwrap_or_default();
        let t = Instant::now();
        view.update_in(cx, |v, window, cx| v.find_with("line_1", window, cx));
        let find = t.elapsed();
        view.update(cx, FileView::close_find);
        let t = Instant::now();
        if let Some(syntax) = Syntax::for_path("big.rs", "") {
            std::hint::black_box(crate::highlight::spans(&body, syntax));
        }
        let parse = t.elapsed();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        println!(
            "{lines} lines: load {:.1} ms, keystroke {:.2} ms, scroll {:.2} ms, find {:.1} ms, \
             background parse {:.0} ms",
            ms(load),
            ms(key),
            ms(scroll),
            ms(find),
            ms(parse)
        );
    }
}

/// What keeping a 16 MiB unsaved edit costs the UI thread: [`FileView::text`] (the whole text
/// copied out, as a backup read it before) against [`FileView::backup_mark`] (what a pass
/// reads of a tile whose backup is current) and [`FileView::backup`] (the rope shared, for one
/// that is behind); and, off the UI thread, [`Backup::unsaved`] reading the rope out. Median
/// of 21 rounds.
#[gpui::test]
#[ignore = "timing: cargo nextest run -p slopty-ui --release --run-ignored only timing_of_a_backup --no-capture"]
fn timing_of_a_backup(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    fn median(mut f: impl FnMut()) -> Duration {
        let mut took: Vec<Duration> = std::iter::repeat_with(|| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .take(21)
        .collect();
        took.sort();
        took.get(10).copied().unwrap_or_default()
    }

    let (view, _events, cx) = tile(cx, "/w/src/big.rs");
    let mut body = source(240_000);
    body.truncate(usize::try_from(FILE_BYTES).unwrap_or(usize::MAX).saturating_sub(1));
    arrives(&view, cx, text_read(&body, false, 1));
    types(&view, cx, "x");
    let bytes = view.read_with(cx, |v, cx| v.text(cx).len());
    assert!(view.read_with(cx, |v, _| v.dirty()), "an unsaved edit");
    let text = view.read_with(cx, |v, cx| median(|| drop(std::hint::black_box(v.text(cx)))));
    let mark = view.read_with(cx, |v, _| median(|| _ = std::hint::black_box(v.backup_mark())));
    let backup = view.read_with(cx, |v, cx| median(|| drop(std::hint::black_box(v.backup(cx)))));
    let kept = view.read_with(cx, FileView::backup).expect("a backup");
    let read_out = median(|| drop(std::hint::black_box(kept.unsaved(WallMs::ZERO))));
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!(
        "{bytes} B, UI thread: text() {:.0} µs, backup_mark() {:.3} µs, backup() {:.3} µs; \
         off it, the rope read out {:.0} µs",
        us(text),
        us(mark),
        us(backup),
        us(read_out)
    );
}

fn edited(events: &Events) -> Vec<(HandoffId, EditOutcome)> {
    events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            FileViewEvent::Edited { id, outcome } => Some((*id, *outcome)),
            _ => None,
        })
        .collect()
}

fn saves(events: &Events) -> usize {
    events.borrow().iter().filter(|e| matches!(e, FileViewEvent::Save { .. })).count()
}

/// "Done" on a file nothing changed answers the program at once; with nothing waiting it is
/// nothing at all.
#[gpui::test]
fn done_on_an_unchanged_file_answers_at_once(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/r/.git/COMMIT_EDITMSG");
    arrives(&view, cx, text_read("message", true, 1_000));
    view.update(cx, FileView::finish_edit);
    assert!(edited(&events).is_empty(), "no program waits");
    view.update(cx, |v, cx| v.set_waiting(Some(9), cx));
    view.update(cx, FileView::finish_edit);
    assert_eq!(edited(&events), [(9, EditOutcome::Done)]);
    assert_eq!(saves(&events), 0, "nothing to save");
    assert_eq!(view.read_with(cx, |v, _| v.waiting()), None);
}

/// What is typed while Done's save is out is saved too, and the program is answered only when
/// the disk has all of it.
#[gpui::test]
fn text_typed_while_done_saves_is_saved_before_the_answer(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/r/.git/COMMIT_EDITMSG");
    arrives(&view, cx, text_read("", true, 1_000));
    view.update(cx, |v, cx| v.set_waiting(Some(2), cx));
    types(&view, cx, "Fix");
    view.update(cx, FileView::finish_edit);
    assert_eq!(saves(&events), 1);
    assert!(view.read_with(cx, |v, _| v.finishing()));
    types(&view, cx, "ed: ");
    let saved = |ms| WriteResult::Saved { size: 4, modified_ms: WallMs::from_millis(ms) };
    view.update(cx, |v, cx| v.written(saved(2_000), cx));
    assert_eq!(saves(&events), 2, "the rest goes after it");
    assert!(edited(&events).is_empty(), "not answered yet");
    view.update(cx, |v, cx| v.written(saved(3_000), cx));
    assert_eq!(edited(&events), [(2, EditOutcome::Done)]);
    assert_eq!(text(&view, cx), "ed: Fix");
}

/// A save lost with the link stops the finishing: the program keeps waiting, and the person
/// asks again once the worker is back.
#[gpui::test]
fn a_save_lost_with_the_link_leaves_the_program_waiting(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/r/.git/COMMIT_EDITMSG");
    arrives(&view, cx, text_read("", true, 1_000));
    view.update(cx, |v, cx| v.set_waiting(Some(4), cx));
    types(&view, cx, "Fix");
    view.update(cx, FileView::finish_edit);
    view.update(cx, FileView::link_lost);
    assert!(!view.read_with(cx, |v, _| v.finishing()), "no longer finishing");
    assert_eq!(view.read_with(cx, |v, _| v.waiting()), Some(4), "still waiting");
    assert!(edited(&events).is_empty());
}

fn kept(text: &str, base_ms: Option<u64>, conflict: bool) -> Backup {
    Backup::kept(&Unsaved {
        worker: WorkerKey::new(1),
        item: ItemId::new(),
        path: "/r/a.md".to_owned(),
        text: text.to_owned(),
        newline: true,
        base_modified_ms: base_ms.map(WallMs::from_millis),
        conflict,
        kept_ms: WallMs::from_millis(5_000),
    })
}

/// An edit kept from before the app ended comes back over the same version of the file as an
/// unsaved edit, the disk's version its base: ⌘S saves it as any edit.
#[gpui::test]
fn a_kept_edit_over_the_version_it_started_from_is_unsaved(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/r/a.md");
    view.update(cx, |v, cx| v.restore(kept("draft\nmore", Some(1_000), false), cx));
    arrives(&view, cx, text_read("draft", true, 1_000));
    assert_eq!(text(&view, cx), "draft\nmore");
    assert!(view.read_with(cx, |v, _| v.dirty()), "unsaved");
    assert_eq!(view.read_with(cx, |v, _| v.trouble().cloned()), None);
    view.update(cx, FileView::save);
    let saved = events.borrow().iter().any(|e| {
        matches!(e, FileViewEvent::Save { text, base_modified_ms: Some(ms) }
            if text == "draft\nmore\n" && *ms == WallMs::from_millis(1_000))
    });
    assert!(saved, "{:?}", events.borrow());
    let backup = view.read_with(cx, |v, cx| v.backup(cx).map(|b| b.text.to_string()));
    assert_eq!(backup.as_deref(), Some("draft\nmore"), "still kept while saving");
}

/// A tile given `kept`, then its first read `read`: its text, whether it is unsaved, and what
/// stops a save.
fn restored(
    cx: &mut TestAppContext,
    kept: Backup,
    read: FileRead,
) -> (String, bool, Option<Trouble>) {
    let (view, _events, cx) = tile(cx, &kept.path.clone());
    view.update(cx, |v, cx| v.restore(kept, cx));
    arrives(&view, cx, read);
    view.read_with(cx, |v, cx| (v.text(cx), v.dirty(), v.trouble().cloned()))
}

/// Over a file that changed on disk since the edit started, or that is gone, the kept edit
/// comes back as a conflict: the person picks the disk's text or theirs. One already marked a
/// conflict stays one. One the disk already holds is no edit at all.
#[gpui::test]
fn a_kept_edit_over_a_moved_disk_is_a_conflict(cx: &mut TestAppContext) {
    let conflict = Some(Trouble::Conflict);
    let moved = restored(cx, kept("mine", Some(1_000), false), text_read("theirs", true, 2_000));
    assert_eq!(moved, ("mine".to_owned(), true, conflict.clone()));
    let gone = FileRead::Missing { error: "No such file".to_owned() };
    let gone = restored(cx, kept("mine", Some(1_000), false), gone);
    assert_eq!(gone, ("mine".to_owned(), true, conflict.clone()), "shown whatever the disk has");
    let marked = restored(cx, kept("mine", Some(1_000), true), text_read("theirs", true, 1_000));
    assert_eq!(marked, ("mine".to_owned(), true, conflict));
    let same = restored(cx, kept("same", Some(1_000), false), text_read("same", true, 3_000));
    assert_eq!(same, ("same".to_owned(), false, None), "the disk has it: clean");
}
