use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::TermSize;
use slopty_testkit::bench::Bench;

use super::*;
use crate::EngineConfig;

fn engine(cols: u16, rows: u16, scrollback_lines: u32) -> GhosttyEngine {
    GhosttyEngine::new(EngineConfig {
        size: TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } },
        scrollback_lines,
    })
    .unwrap()
}

/// Plain output: a line of text, no styles.
fn plain(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..lines {
        out.extend_from_slice(
            format!("line {i:05} the quick brown fox jumps over the lazy dog\r\n").as_bytes(),
        );
    }
    out
}

/// Coloured output, as a highlighter or `ls --color` writes it: a style per word.
fn coloured(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..lines {
        for (w, word) in
            ["line", "the", "quick", "brown", "fox", "jumps", "over", "lazy"].iter().enumerate()
        {
            let colour = i.wrapping_add(w.wrapping_mul(37)) % 256;
            let bold = if w % 3 == 0 { ";1" } else { "" };
            out.extend_from_slice(format!("\x1b[38;5;{colour}{bold}m{word}\x1b[0m ").as_bytes());
        }
        out.extend_from_slice(format!("{i:05}\r\n").as_bytes());
    }
    out
}

/// A shell session: a marked prompt, a command, and a listing with coloured, linked names.
fn shell(lines: usize) -> Vec<u8> {
    const LISTING: usize = 20;
    let mut out = Vec::new();
    let mut written = 0;
    let mut n = 0_usize;
    while written < lines {
        out.extend_from_slice(
            b"\x1b]133;A\x07\x1b[32m~/src/slopty\x1b[0m % \x1b]133;B\x07ls -l\r\n",
        );
        out.extend_from_slice(b"\x1b]133;C\x07");
        for f in 0..LISTING {
            n = n.wrapping_add(1);
            out.extend_from_slice(
                format!(
                    "-rw-r--r--  1 me  staff  {:>6} Sep 25 12:{:02} \x1b]8;;file:///Users/me/src/slopty/f{n}.rs\x1b\\\x1b[1;34mf{n}.rs\x1b[0m\x1b]8;;\x1b\\\r\n",
                    n.wrapping_mul(97) % 100_000,
                    f % 60
                )
                .as_bytes(),
            );
        }
        out.extend_from_slice(format!("\x1b]133;D;{}\x07", n % 2).as_bytes());
        written = written.saturating_add(LISTING + 1);
    }
    out
}

fn fill(e: &mut GhosttyEngine, out: &[u8]) {
    for chunk in out.chunks(65_536) {
        e.write(chunk);
    }
}

/// Steps the compression of `e`'s history until it is done, counting the steps.
fn compress_all(e: &mut GhosttyEngine) -> (Compression, usize) {
    let mut steps = 0_usize;
    loop {
        steps = steps.saturating_add(1);
        match e.compress_history().unwrap() {
            Compression::Pending => assert!(steps < 100_000, "compression never finished"),
            done => return (done, steps),
        }
    }
}

fn all_lines(e: &GhosttyEngine) -> Vec<String> {
    let total = e.total_lines().unwrap();
    let (_, lines) =
        e.lines(slopty_grid::LineIndex(e.base), u32::try_from(total).unwrap()).unwrap();
    lines.iter().map(slopty_grid::Line::text).collect()
}

/// What a scrollback line costs in memory, plain, coloured and from a shell session, and what
/// compressing the history while idle saves and costs. `cargo xtask bench --filter memory_cost`
/// runs it; MEASUREMENTS records it.
#[test]
#[ignore = "measurement, run by hand"]
#[expect(clippy::cast_precision_loss, reason = "a measurement's report")]
fn memory_cost() {
    const LINES: usize = 10_000;
    let bench = Bench::new("engine.memory_cost");
    for (name, out) in
        [("plain", plain(LINES)), ("coloured", coloured(LINES)), ("shell", shell(LINES))]
    {
        let empty = engine(80, 24, 100_000).memory().unwrap();
        let mut e = engine(80, 24, 100_000);
        fill(&mut e, &out);
        let history = (e.total_lines().unwrap() - 24) as f64;
        let full = e.memory().unwrap();
        // The screen's format, not `checkpoint`, which keeps the last one until the terminal
        // changes.
        let mut resident = bench.series(&format!("{name}.format_resident"));
        let state = resident.time(|| e.format_active_screen().unwrap());
        let mut step = bench.series(&format!("{name}.compress_step"));
        let mut steps = 0_usize;
        loop {
            steps = steps.saturating_add(1);
            if step.time(|| e.compress_history().unwrap()) != Compression::Pending {
                break;
            }
        }
        let compressed = e.memory().unwrap();
        let per_line = |m: &Memory| (m.resident_bytes - empty.resident_bytes) as f64 / history;
        eprintln!(
            "memory_cost.{name}: {history} history lines, {} pages; resident {} -> {} bytes \
             ({:.0} -> {:.0} a line, {} of {} pages compressed into {} bytes, {steps} steps); \
             virtual {}; empty terminal {}",
            full.pages,
            full.resident_bytes,
            compressed.resident_bytes,
            per_line(&full),
            per_line(&compressed),
            compressed.compressed_pages,
            compressed.pages,
            compressed.compressed_bytes,
            compressed.virtual_bytes,
            empty.resident_bytes,
        );
        let mut checkpoint = bench.series(&format!("{name}.format_compressed"));
        let again = checkpoint.time(|| e.format_active_screen().unwrap());
        assert!(again == state, "a checkpoint reads compressed history as it was");
        let read = e.memory().unwrap();
        eprintln!(
            "memory_cost.{name}: after a checkpoint, resident {} bytes, {} pages compressed",
            read.resident_bytes, read.compressed_pages
        );
        resident.report().unwrap();
        step.report().unwrap();
        checkpoint.report().unwrap();
    }
}

/// The scrollback limit bounds the memory a session holds, however long its output runs: a
/// coloured line costs under a kibibyte, and output five times the limit holds no more than
/// the limit's worth and the page being pruned.
#[test]
fn scrollback_memory_stays_within_its_bound() {
    const LIMIT: u32 = 2_000;
    let mut short = engine(80, 24, LIMIT);
    fill(&mut short, &coloured(LIMIT as usize));
    let at_limit = short.memory().unwrap();
    let mut long = engine(80, 24, LIMIT);
    fill(&mut long, &coloured(5 * LIMIT as usize));
    let past = long.memory().unwrap();
    let page = at_limit.resident_bytes / at_limit.pages;
    assert!(
        at_limit.resident_bytes <= u64::from(LIMIT + 24) * 1024 + page,
        "{at_limit:?} for {LIMIT} lines"
    );
    assert!(past.resident_bytes <= at_limit.resident_bytes + page, "{past:?} against {at_limit:?}");
}

/// Compressing an idle session's history frees most of what it holds, and nothing that reads
/// the history sees a difference: the lines a viewer fetches, and a checkpoint, which leaves
/// the history compressed. A fetch brings pages back, and the next idle pass compresses them
/// again.
#[test]
fn idle_compression_frees_memory_and_every_read_is_the_same() {
    let mut e = engine(80, 24, 10_000);
    fill(&mut e, &coloured(3_000));
    let full = e.memory().unwrap();
    let lines = all_lines(&e);
    let format = e.format_active_screen().unwrap();
    let (done, steps) = compress_all(&mut e);
    if done == Compression::Unsupported {
        assert!(!full.compression_supported);
        return;
    }
    assert!(steps > 1, "compressed in steps");
    let compressed = e.memory().unwrap();
    assert!(compressed.compressed_pages > 0, "{compressed:?}");
    assert!(compressed.resident_bytes < full.resident_bytes / 2, "{compressed:?} from {full:?}");

    assert!(e.format_active_screen().unwrap() == format, "a checkpoint reads the same");
    assert_eq!(e.memory().unwrap(), compressed, "and leaves the history compressed");
    // Once a pass is done, a step does nothing until the terminal changes.
    assert_eq!(e.compress_history().unwrap(), Compression::Done);

    assert_eq!(all_lines(&e), lines, "a fetch reads the same");
    assert!(e.memory().unwrap().compressed_pages < compressed.compressed_pages, "and restores");
    compress_all(&mut e);
    assert_eq!(e.memory().unwrap().compressed_pages, compressed.compressed_pages);
}
