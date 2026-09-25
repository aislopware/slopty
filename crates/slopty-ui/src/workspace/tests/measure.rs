//! What one workspace frame costs with many items, few of them on screen: the per-frame work
//! that grows with the registry rather than with what is drawn. Run by hand (it prints, it does
//! not judge); `docs/MEASUREMENTS.md` has the numbers and the command.

use std::time::Instant;

use super::*;

/// Notes, file cards and shells on one worker; a note's text is this many bytes.
const NOTES: usize = 120;
const FILES: usize = 60;
const SHELLS: usize = 12;
const NOTE_BYTES: usize = 4096;
/// Frames drawn for the numbers, after a few to warm the caches.
const FRAMES: usize = 400;
const WARM: usize = 20;

#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_a_frame_over_a_large_registry(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let line = "- [ ] a line of a long note, to be cloned or not\n";
    let text = line.repeat(NOTE_BYTES.checked_div(line.len()).unwrap_or(1));
    let mut items: Vec<Item> = Vec::new();
    let mut sessions = Vec::new();
    for _ in 0..NOTES {
        let kind = ItemKind::Note { text: text.clone() };
        items.push(Item { id: ItemId::new(), kind, sleeping: false, name: None });
    }
    for n in 0..FILES {
        let kind = ItemKind::File { path: format!("/w/src/file_{n}.rs") };
        items.push(Item { id: ItemId::new(), kind, sleeping: false, name: None });
    }
    for _ in 0..SHELLS {
        let session = SessionId::new();
        sessions.push(summary(session, Some("/w")));
        items.push(Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            sleeping: false,
            name: None,
        });
    }
    let key = studio.key;
    view.update_in(cx, |v, _window, cx| {
        for s in sessions {
            v.session_opened(key, s, cx);
        }
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    let mut took = Vec::with_capacity(FRAMES);
    for n in 0..WARM + FRAMES {
        let start = Instant::now();
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p);
    let mean =
        took.iter().sum::<Duration>().checked_div(u32::try_from(took.len()).unwrap()).unwrap();
    println!(
        "MEASURE workspace frame, {NOTES} notes × {NOTE_BYTES} B, {FILES} files, {SHELLS} shells, \
         {FRAMES} frames: mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        mean.as_secs_f64() * 1e3,
        pct(50).as_secs_f64() * 1e3,
        pct(95).as_secs_f64() * 1e3,
        took.last().copied().unwrap_or_default().as_secs_f64() * 1e3,
    );
}
