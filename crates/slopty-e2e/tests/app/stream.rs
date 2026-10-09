//! Remote windows and displays in the real app, streamed for real: the worker serves its drawn
//! screen (`SLOPTY_SYNTHETIC_SCREEN`, `slopty_worker::screen::synthetic::Synthetic`), so every
//! frame is a `Canvas` picture encoded by VideoToolbox, cut into datagrams with their parity,
//! carried by QUIC over loopback, put back together, decoded and presented on the app's own
//! surface. Nothing is captured and nothing asks the machine for a grant, and the drawn screen
//! is the same on every Mac, so the tiles can be held to goldens.
//!
//! A drawn picture moves: a page of glyphs scrolls in its middle half while the desktop around
//! it and the strip along its top stand still. A golden masks the page and holds everything
//! else to its pixels, and the page itself is held to the golden's mix of luma
//! ([`luma_distance`]), which a scroll keeps and a black, torn or misplaced picture does not.
//!
//! The timings come from the app's own pacer (arrival → present on its surface) and from the
//! worker's counters; `frame_time` times the whole path from the drawn frame's capture stamp.

use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{
    PixelRect, assert_matches_apart, assert_matches_masked, golden_dir, luma_distance,
    luma_histogram,
};
use slopty_e2e::{Command, Driver, Dump, ScreenInfo, Stack};

use super::gallery::{STEP, first_shell, settled};

/// The worker's switch to its drawn screen.
const SYNTHETIC: (&str, &str) = ("SLOPTY_SYNTHETIC_SCREEN", "1");
/// The drawn screen's first window, as `synthetic::WINDOWS` lists it: 1280 × 800 points.
const EDITOR: (u32, &str) = (7001, "Synthetic editor");
/// Its display, as `synthetic::DISPLAY` lists it.
const DISPLAY: u32 = 1;
/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Frames a stream shows before it is judged: two seconds of the drawn display's 60 Hz.
const FRAMES: u64 = 120;
/// The first frame's wait: an encoder and a decoder session to build, which on a volume
/// mounted without ownership revalidates each binary's signature (`docs/decisions/testing.md`).
const FIRST_FRAMES: Duration = Duration::from_secs(90);
/// Arrival → present on the app's surface, 95th percentile, past which a loopback stream is
/// broken rather than slow: three periods of a 60 Hz display.
const PRESENT_P95: Duration = Duration::from_millis(50);
/// Pixels of a golden outside the moving page and the live readouts allowed to differ: the
/// chrome's own `MAC_TOLERANCE`. The codec's rendering of the still desktop and strip differs
/// by 0 to 0.033 % between runs; at 0.4 %, the old bound, a golden a day behind the chrome's
/// icons (0.34 %) passed until one live figure tipped it over.
const TOLERANCE: f64 = slopty_e2e::snapshot::MAC_TOLERANCE;
/// How far the page's mix of luma may move from the golden's: a scroll changes which lines are
/// on it by a few percent.
const PAGE_DISTANCE: f64 = 0.08;
/// Device pixels the page's mask grows by, for the codec's ringing along its edges and the
/// scaling's.
const PAGE_MARGIN: u32 = 6;

/// The first stream has put up [`FRAMES`] frames.
fn shown(d: &Dump) -> bool {
    d.screens.first().is_some_and(|s| s.frames >= FRAMES)
}

/// The body of the tile of the stream labelled `label` and the picture in it, in device
/// pixels: the body (its accessibility node) holds the picture at the stream's own aspect, as
/// large as it goes and centred.
fn picture(dump: &Dump, label: &str) -> (PixelRect, PixelRect) {
    let node = dump
        .a11y_node("Image", Some(label))
        .unwrap_or_else(|| panic!("no picture {label}: {:#?}", dump.a11y));
    let [sw, sh] = dump.screens.first().map_or([1, 1], |s| s.size);
    #[expect(clippy::cast_precision_loss, reason = "stream pixels")]
    let (sw, sh) = (sw.max(1) as f32, sh.max(1) as f32);
    let [x, y, w, h] = node.bounds;
    let fit = (w / sw).min(h / sh);
    let (fw, fh) = (sw * fit, sh * fit);
    let scale = dump.window.scale;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window pixels")]
    let px = |points: f32| (points * scale).round().max(0.0) as u32;
    ([px(x), px(y), px(w), px(h)], [px(x + (w - fw) / 2.0), px(y + (h - fh) / 2.0), px(fw), px(fh)])
}

/// Device pixels next to the picture's edge left out of the bars: the edge's own filtering.
const BAR_MARGIN: u32 = 3;

/// How far the tile's panel paints over the body's edge, in points: its corners, rounded to the
/// theme's `radii.md`, with its ring along them. The panel's edge, not the body's.
const PANEL_EDGE: f32 = 8.0;

/// The body beside the picture is bare: every pixel of the bars above and below it (or left and
/// right of it) is the body's one colour, so the picture kept its aspect instead of being
/// stretched over them. The panel's edge round the body (`edge` device pixels) is left out.
/// Returns how many device pixels of bar there were.
fn assert_bare_beside(
    frame: &image::RgbaImage,
    body: PixelRect,
    picture: PixelRect,
    edge: u32,
) -> u64 {
    let [bx, by, bw, bh] = body;
    let (bx, by) = (bx.saturating_add(edge), by.saturating_add(edge));
    let (bw, bh) =
        (bw.saturating_sub(edge.saturating_mul(2)), bh.saturating_sub(edge.saturating_mul(2)));
    let [px, py, pw, ph] = picture;
    let below = py.saturating_add(ph).saturating_add(BAR_MARGIN);
    let right = px.saturating_add(pw).saturating_add(BAR_MARGIN);
    let bars: Vec<PixelRect> = [
        [bx, by, bw, py.saturating_sub(by).saturating_sub(BAR_MARGIN)],
        [bx, below, bw, by.saturating_add(bh).saturating_sub(below)],
        [bx, by, px.saturating_sub(bx).saturating_sub(BAR_MARGIN), bh],
        [right, by, bx.saturating_add(bw).saturating_sub(right), bh],
    ]
    .into_iter()
    .filter(|[_, _, w, h]| *w > 0 && *h > 0)
    .collect();
    let mut seen = 0_u64;
    let mut colour = None;
    for [x, y, w, h] in bars {
        for yy in y..y.saturating_add(h).min(frame.height()) {
            for xx in x..x.saturating_add(w).min(frame.width()) {
                let pixel = frame.get_pixel(xx, yy).0;
                let first = *colour.get_or_insert(pixel);
                let off = pixel.iter().zip(first).map(|(a, b)| a.abs_diff(b)).max().unwrap_or(0);
                assert!(off <= 2, "({xx}, {yy}) is {pixel:?} beside the picture, not {first:?}");
                seen = seen.saturating_add(1);
            }
        }
    }
    seen
}

/// The stream's layer was last presented over `picture` (device pixels), to a pixel: the picture
/// the render draws with GPUI for the golden is where the window server shows the layer's.
fn assert_layer_on(dump: &Dump, picture: PixelRect) {
    let layer = dump.screens.first().and_then(|s| s.layer).expect("the picture's layer placed");
    let scale = dump.window.scale;
    #[expect(clippy::cast_precision_loss, reason = "window points and pixels")]
    let off = layer
        .iter()
        .zip(picture)
        .map(|(points, pixels)| (*points as f32).mul_add(scale, -(pixels as f32)).abs())
        .fold(0.0_f32, f32::max);
    assert!(off <= scale + 1.0, "layer at {layer:?} points, picture at {picture:?} pixels");
}

/// The part of a drawn picture that moves: `Canvas` scrolls its page in the middle half each
/// way, grown by [`PAGE_MARGIN`].
const fn page(picture: PixelRect) -> PixelRect {
    let [x, y, w, h] = picture;
    [
        x.saturating_add(w / 4).saturating_sub(PAGE_MARGIN),
        y.saturating_add(h / 4).saturating_sub(PAGE_MARGIN),
        (w / 2).saturating_add(PAGE_MARGIN.saturating_mul(2)),
        (h / 2).saturating_add(PAGE_MARGIN.saturating_mul(2)),
    ]
}

/// Render the frame as it rests and hold it to `golden/<name>.png`: everything but the page to
/// its pixels, the page to its mix. Returns the page's distance.
async fn golden_stream(
    drv: &mut Driver,
    dir: &std::path::Path,
    name: &str,
    label: &str,
    heading: &str,
) -> f64 {
    let dump = settled(drv).await;
    let (body, picture) = picture(&dump, label);
    assert_layer_on(&dump, picture);
    let page = page(picture);
    let status = header_status(&dump, heading);
    let frame = drv.render(&dir.join(format!("{name}.png"))).await.unwrap();
    // The stats overlay floats at the body's corner, over whatever is there.
    if dump.a11y_node("Status", Some("Stream stats")).is_none() {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "window pixels"
        )]
        let edge = (PANEL_EDGE * dump.window.scale).ceil() as u32;
        let bare = assert_bare_beside(&frame, body, picture, edge);
        println!("MEASURE {name}: picture {picture:?} in body {body:?}, {bare} px of bare body");
    }
    let existing = golden_dir().join(format!("{name}.png"));
    let before = image::open(&existing).ok().map(image::DynamicImage::into_rgba8);
    let words: Vec<PixelRect> = [page, status].into_iter().chain(live_readouts(&dump)).collect();
    let pixels: Vec<PixelRect> = words.iter().copied().chain(overlay_band(&dump, body)).collect();
    assert_matches_apart(name, &frame, TOLERANCE, &artifacts_dir(), (&pixels, &words)).unwrap();
    let golden = before.unwrap_or_else(|| frame.image.clone());
    let distance = luma_distance(&luma_histogram(&frame, page), &luma_histogram(&golden, page));
    println!("MEASURE {name}: page luma distance {distance:.4}");
    distance
}

/// Where the tile's header says how the stream is doing: between its title and its buttons.
///
/// The word there ("Frames late") follows the pacing of the last second, which the load on this
/// Mac moves, so a golden masks it; the accessibility tree says which word it is.
fn header_status(dump: &Dump, heading: &str) -> PixelRect {
    let [hx, hy, hw, hh] = dump
        .a11y_node("Heading", Some(heading))
        .unwrap_or_else(|| panic!("no heading {heading}: {:#?}", dump.a11y))
        .bounds;
    let right = dump
        .items
        .iter()
        .map(|i| i.bounds)
        .find(|[x, y, w, h]| hx >= *x && hx <= x + w && hy >= *y && hy <= y + h)
        .map_or(hx + hw, |[x, _, w, _]| x + w - STATUS_BUTTONS);
    let scale = dump.window.scale;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window pixels")]
    let px = |points: f32| (points * scale).round().max(0.0) as u32;
    let left = hx + hw + 4.0;
    [px(left), px(hy - 6.0), px((right - left).max(0.0)), px(hh + 12.0)]
}

/// The readouts a stream keeps live, the foot bar's frame time and the stats overlay's
/// figures: what they say follows the load on this Mac (59 fps one run, 60 the next), so a
/// golden masks them, in pixels and in words.
fn live_readouts(dump: &Dump) -> Vec<PixelRect> {
    let scale = dump.window.scale;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window pixels")]
    let px = |points: f32| (points * scale).round().max(0.0) as u32;
    dump.a11y
        .iter()
        .filter(|n| n.role == "Status" || n.role == "Label")
        .filter(|n| n.label.as_deref().is_some_and(is_live))
        .map(|n| {
            let [x, y, w, h] = n.bounds;
            [px(x), px(y), px(w), px(h)]
        })
        .collect()
}

/// The stats overlay's band across the body, when it shows: its pixels are masked and its
/// words ("Details") held.
///
/// The overlay keeps to the body's right edge and widens with its figures (none for the glass
/// until a frame is shown, then "21 ms to glass"), so its left edge falls anywhere along the
/// band from one run to the next.
fn overlay_band(dump: &Dump, body: PixelRect) -> Option<PixelRect> {
    let [x, y, w, h] = dump.a11y_node("Status", Some("Stream stats"))?.bounds;
    let scale = dump.window.scale;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window pixels")]
    let px = |points: f32| (points * scale).round().max(0.0) as u32;
    let right = px(x + w);
    Some([body[0], px(y), right.saturating_sub(body[0]), px(h)])
}

/// A label made of figures that move: one of its parts, as the readouts join them with " · ",
/// is a rate or a time.
fn is_live(label: &str) -> bool {
    label.split(" \u{b7} ").any(|part| part.ends_with(" fps") || part.ends_with(" ms"))
}

/// The stats overlay's figures, as it says them: the rate first.
fn overlay_figures(dump: &Dump) -> Option<String> {
    dump.a11y
        .iter()
        .filter(|n| n.role == "Label")
        .filter_map(|n| n.label.clone())
        .find(|l| l.split(" \u{b7} ").next().is_some_and(|rate| rate.ends_with(" fps")))
}

/// Points the header's own buttons take at its right end, which the status mask leaves out.
const STATUS_BUTTONS: f32 = 60.0;

/// One stream's numbers, one line.
fn row(s: &ScreenInfo) -> String {
    let ms = |us: u64| Duration::from_micros(us).as_secs_f64() * 1e3;
    let r = &s.recovery;
    format!(
        "{}×{}: {} frames painted, {} presented, {} skipped, {} repeats, {} late | arrival → \
         present p50 {:.2} / p95 {:.2} / max {:.2} ms, decode p50 {:.2} ms, every {:.2} ms ± \
         {:.2} | {} datagrams, {} fec, {} retransmitted, {} lost, {} nacks, {} refreshes, {} \
         stalls",
        s.size[0],
        s.size[1],
        s.frames,
        s.presented,
        s.skipped,
        s.repeats,
        s.late,
        ms(s.latency_p50_us),
        ms(s.latency_p95_us),
        ms(s.latency_max_us),
        ms(s.decode_p50_us),
        ms(s.interval_p50_us),
        ms(s.interval_jitter_us),
        r.datagrams,
        r.frames_fec,
        r.frames_retransmit,
        r.frames_lost,
        r.nacks,
        r.refreshes,
        r.stalls,
    )
}

/// What a stream on loopback must be: live, drawing, nothing lost, nothing repaired and no
/// picture asked for again, and presented within a few display periods of its arrival.
///
/// A NACK alone is allowed: its delay is a quarter of the round trip floored at 1 ms, and a
/// fragment the busy machine delivers later than that is asked for and then arrives anyway,
/// which `frames_retransmit` staying at zero shows.
///
/// The presentation is timed only when the window server says when each frame reached the
/// display. It says nothing for a window that is covered, as the app's is under a remote
/// session to this Mac: the paints go on, and no frame is reported shown. That is said, and
/// the timing is left to `frame_time`.
fn assert_clean(s: &ScreenInfo) {
    let r = &s.recovery;
    assert_eq!(s.source, "live", "{s:#?}");
    assert!(s.frames >= FRAMES, "{s:#?}");
    assert_eq!((r.frames_lost, r.datagrams_lost), (0, 0), "loss on loopback: {s:#?}");
    assert_eq!((r.frames_fec, r.frames_retransmit), (0, 0), "repairs on loopback: {s:#?}");
    assert_eq!((r.refreshes, r.stalls), (0, 0), "a refresh or a stall on loopback: {s:#?}");
    if s.presented == 0 {
        slopty_testkit::live::skip(
            "no frame reported presented: the app's window is covered; arrival → present unmeasured",
        );
        return;
    }
    // A wall-clock budget holds on the Mac it was set on. A hosted runner is a shared
    // three-core virtual Mac whose display paces presentation as no panel does: p50 20 ms,
    // p95 61 ms (CI e2e run 37403206620). There the numbers are printed and the rest is held.
    println!(
        "MEASURE arrival → present: p50 {} µs, p95 {} µs, max {} µs over {} presented",
        s.latency_p50_us, s.latency_p95_us, s.latency_max_us, s.presented
    );
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        assert!(
            Duration::from_micros(s.latency_p95_us) < PRESENT_P95,
            "arrival → present p95 over {PRESENT_P95:?}: {s:#?}"
        );
    }
}

/// The worker's own counters for one stream, in microseconds where they are times.
struct WorkerSide {
    captured: u64,
    /// The source's frames coded and sent.
    encoded: u64,
    /// A still picture's refinements, coded and sent besides [`Self::encoded`].
    refined: u64,
    /// Captures a newer one replaced before the encoder took them.
    superseded: u64,
    dropped: u64,
    encode_p50: u64,
    encode_p95: u64,
    /// Capture → packetized, the mean.
    packetized: u64,
    /// Heartbeats sent, and the longest silence one of them ended.
    beats: u64,
    beat_worst: u64,
}

impl WorkerSide {
    /// One line of them.
    fn row(&self) -> String {
        format!(
            "captured {} encoded {} refined {} superseded {} dropped {}, encode p50 {} / p95 {} \
             µs, capture → packetized mean {} µs, {} beats, the longest silence one ended {} µs",
            self.captured,
            self.encoded,
            self.refined,
            self.superseded,
            self.dropped,
            self.encode_p50,
            self.encode_p95,
            self.packetized,
            self.beats,
            self.beat_worst
        )
    }
}

/// The worker's own counters for the one live stream of `client`.
async fn worker_side(stack: &Stack, client: &str) -> WorkerSide {
    let live = stack.worker_screens().await.unwrap();
    let mine: Vec<_> = live.iter().filter(|s| s["client"] == client).collect();
    assert_eq!(mine.len(), 1, "{live:#?}");
    let stats = &mine[0]["stats"];
    let n = |key: &str| stats[key].as_u64().unwrap_or_else(|| panic!("{key}: {stats}"));
    let q = |key: &str, p: &str| stats[key][p].as_u64().unwrap_or_default();
    WorkerSide {
        captured: n("captured"),
        encoded: n("encoded"),
        refined: n("refined"),
        superseded: n("superseded"),
        dropped: n("dropped"),
        encode_p50: q("encode", "p50_us"),
        encode_p95: q("encode", "p95_us"),
        packetized: n("latency_sum_us").checked_div(n("encoded")).unwrap_or_default(),
        beats: n("heartbeats"),
        beat_worst: n("beat_gap_worst_us"),
    }
}

/// Open the drawn editor window in a pane and wait until it has shown [`FRAMES`].
async fn open_editor(drv: &mut Driver) -> Dump {
    drv.ok(&Command::PickWindow { window: EDITOR.0, title: EDITOR.1.to_owned() }).await.unwrap();
    drv.wait_for("the drawn window's frames", FIRST_FRAMES, shown).await.unwrap()
}

/// A window of the drawn screen streams into its tile: the picture is the window's shape, two
/// seconds of it arrive with nothing lost and nothing repaired, each goes up within a few
/// display periods, and the worker encoded what it drew. The stats overlay then shows over it,
/// and the tile pops out into a window of its own, where it keeps presenting, and comes back.
/// Goldens `stream-window`, `stream-window-stats` and `stream-window-popped`.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_drawn_window_streams_into_its_tile() {
    let mut stack = Stack::launch_with("e2e-worker", &[SYNTHETIC]).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    let dump = open_editor(drv).await;
    let label = format!("Remote window {}", EDITOR.0);
    let screen = dump.screens[0].clone();
    println!("MEASURE drawn window: {}", row(&screen));
    println!("MEASURE drawn window, UI frames: {}", dump.frames.row());
    // Before the verdict: a stall's cause is in the worker's line (its longest silence is its
    // own; the rest of what the client waited through, the link's).
    let worker = worker_side(&stack, &dump.client).await;
    println!("MEASURE drawn window, worker: {}", worker.row());
    assert_clean(&screen);
    assert!(
        dump.a11y_node("Heading", Some(&format!("window {}", EDITOR.1))).is_some(),
        "{:#?}",
        dump.a11y
    );
    // 1280 × 800 points: whatever scale the tile asked for, the stream keeps the window's shape.
    let [w, h] = screen.size;
    assert!((f64::from(w) / f64::from(h) - 1.6).abs() < 0.01, "{w}×{h}");
    // A still picture's refinements are painted as frames and counted apart from the source's.
    let sent = worker.encoded.saturating_add(worker.refined);
    assert!(sent >= screen.frames, "the app painted more than the worker sent: {}", worker.row());
    assert!(worker.captured >= worker.encoded, "{}", worker.row());

    let drv = &mut stack.driver;
    let heading = format!("window {}", EDITOR.1);
    let distance = golden_stream(drv, &dir, "stream-window", &label, &heading).await;
    assert!(distance <= PAGE_DISTANCE);

    // ⌘⇧I from the palette: the overlay over the picture, its figures filled in.
    open_command(drv, "Stream stats").await;
    drv.wait_for("the stats overlay", STEP, |d| d.a11y_node("Button", Some("Details")).is_some())
        .await
        .unwrap();
    // The overlay samples once a second: two periods, and its figures are the stream's own.
    // A screen reader hears them, and that is how the golden finds them to mask.
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    let dump = drv.dump().await.unwrap();
    assert!(dump.a11y_node("Status", Some("Stream stats")).is_some(), "{:#?}", dump.a11y);
    let figures = overlay_figures(&dump);
    println!("MEASURE stream-window-stats: the overlay says {figures:?}");
    assert!(figures.as_deref().is_some_and(|f| f.contains(" to glass")), "{figures:?}");
    let distance = golden_stream(drv, &dir, "stream-window-stats", &label, &heading).await;
    assert!(distance <= PAGE_DISTANCE);
    open_command(drv, "Stream stats").await;
    drv.wait_for("the overlay gone", STEP, |d| d.a11y_node("Button", Some("Details")).is_none())
        .await
        .unwrap();

    // Its own window: the tile says where the picture went, and the picture keeps being painted
    // there from the same stream.
    let before = drv.dump().await.unwrap().screens[0].frames;
    open_command(drv, "Open in its own window").await;
    let dump = drv
        .wait_for("the picture in its own window", STEP, |d| {
            d.a11y_node("Status", Some("In its own window")).is_some()
                && d.screens.first().is_some_and(|s| s.frames >= before + FRAMES)
        })
        .await
        .unwrap();
    println!("MEASURE drawn window, popped out: {}", row(&dump.screens[0]));
    assert!(dump.a11y_node("Image", Some(&label)).is_none(), "{:#?}", dump.a11y);
    let dump = settled(drv).await;
    let status = header_status(&dump, &heading);
    let frame = drv.render(&dir.join("stream-window-popped.png")).await.unwrap();
    let masks: Vec<PixelRect> = std::iter::once(status).chain(live_readouts(&dump)).collect();
    assert_matches_masked(
        "stream-window-popped",
        &frame,
        slopty_e2e::snapshot::MAC_TOLERANCE,
        &artifacts_dir(),
        &masks,
    )
    .unwrap();
    open_command(drv, "Back to the workspace").await;
    drv.wait_for("the picture back in its tile", STEP, |d| {
        d.a11y_node("Status", Some("In its own window")).is_none()
            && d.a11y_node("Image", Some(&label)).is_some()
    })
    .await
    .unwrap();
    stack.shutdown().await;
}

/// The drawn display, as "add a display" picks the worker's first: streamed whole at its 60 Hz,
/// nothing lost. Golden `stream-display`.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_drawn_display_streams_into_its_tile() {
    let mut stack = Stack::launch_with("e2e-worker", &[SYNTHETIC]).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.ok(&Command::AddDisplay).await.unwrap();
    let dump = drv.wait_for("the drawn display's frames", FIRST_FRAMES, shown).await.unwrap();
    let screen = dump.screens[0].clone();
    println!("MEASURE drawn display: {}", row(&screen));
    let worker = worker_side(&stack, &dump.client).await;
    println!("MEASURE drawn display, worker: {}", worker.row());
    assert_clean(&screen);
    let drv = &mut stack.driver;
    assert!(
        dump.a11y_node("Heading", Some(&format!("display Display {DISPLAY}"))).is_some(),
        "{:#?}",
        dump.a11y
    );
    let [w, h] = screen.size;
    assert!((f64::from(w) / f64::from(h) - 1512.0 / 982.0).abs() < 0.01, "{w}×{h}");
    let label = format!("Remote display {DISPLAY}");
    let heading = format!("display Display {DISPLAY}");
    let distance = golden_stream(drv, &dir, "stream-display", &label, &heading).await;
    assert!(distance <= PAGE_DISTANCE);
    stack.shutdown().await;
}

/// Run `command` from the palette, as its line names it. The palette opens from the title
/// bar's "…" menu: ⌘⇧P is the remote window's own while its picture has the keyboard.
async fn open_command(drv: &mut Driver, command: &str) {
    park(drv).await;
    click(drv, "Button", "More").await;
    drv.wait_for("the … menu", STEP, |d| {
        d.a11y_node("MenuItem", Some("Command palette")).is_some()
    })
    .await
    .unwrap();
    click(drv, "MenuItem", "Command palette").await;
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text(&command.to_lowercase()).await.unwrap();
    drv.wait_for(command, STEP, |d| {
        d.a11y.iter().any(|n| {
            n.role == "ListBoxOption" && n.label.as_deref().is_some_and(|l| l.starts_with(command))
        })
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the palette closed", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_none())
        .await
        .unwrap();
    park(drv).await;
}

/// Click the middle of the `role` node labelled `label`.
async fn click(drv: &mut Driver, role: &str, label: &str) {
    let dump = drv.dump().await.unwrap();
    let node =
        dump.a11y_node(role, Some(label)).unwrap_or_else(|| panic!("{label}: {:#?}", dump.a11y));
    let [x, y, w, h] = node.bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
}

/// The pointer off every tile: over the picture it would be the worker's pointer, drawn.
async fn park(drv: &mut Driver) {
    drv.ok(&Command::Move { x: 1.0, y: 1.0 }).await.unwrap();
}

/// The frame path timed whole, run by `cargo xtask e2e smooth` alone.
mod frame_time {
    use tokio::io::AsyncBufReadExt as _;

    use super::*;

    /// How long each part of the measurement streams.
    const RUN: Duration = Duration::from_secs(20);

    /// Run `slopty-glass` against the stack's worker at `scale` for [`RUN`], echo what it
    /// measured, read the worker's side of its stream halfway, and hold it to a clean loopback.
    async fn glass(stack: &Stack, scale: &str) {
        let bin = slopty_e2e::harness::bin_dir().unwrap().join("slopty-glass");
        let seconds = RUN.as_secs().to_string();
        let mut child = tokio::process::Command::new(bin)
            .args([stack.address.as_str(), &DISPLAY.to_string(), scale, &seconds])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::sleep(RUN.checked_div(2).unwrap_or_default()).await;
        let live = stack.worker_screens().await.unwrap();
        let mut verdict = serde_json::Value::Null;
        while let Some(line) = lines.next_line().await.unwrap() {
            match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(value) if value.is_object() => verdict = value,
                _ => println!("{line}"),
            }
        }
        assert!(child.wait().await.unwrap().success(), "slopty-glass failed");
        let client = verdict["client"].as_str().unwrap_or_default();
        let stats = &live.iter().find(|s| s["client"] == client).expect("the stream")["stats"];
        let n = |key: &str| stats[key].as_u64().unwrap_or_default();
        println!(
            "  worker: captured {} encoded {} dropped {}, encode p50 {} / p95 {} µs, capture → \
             packetized mean {} µs (halfway)",
            n("captured"),
            n("encoded"),
            n("dropped"),
            stats["encode"]["p50_us"],
            stats["encode"]["p95_us"],
            n("latency_sum_us").checked_div(n("encoded")).unwrap_or_default(),
        );
        assert!(verdict["timed"].as_u64() > Some(600), "{verdict}");
        assert_eq!(verdict["frames_lost"].as_u64(), Some(0), "loss on loopback: {verdict}");
    }

    /// Capture → glass on loopback, from drawn frames through the real worker and QUIC
    /// (`docs/MEASUREMENTS.md`, "Drawn frames to the glass").
    ///
    /// First `slopty-glass`, the app's receive path without its window (the link, the
    /// reassembler, VideoToolbox, the pacer) painting on a 60 Hz beat, at half the display's
    /// scale and at its whole: every frame is timed from the capture stamp the worker drew it
    /// with to the paint that showed it. The two clocks are one on loopback, so the stamp
    /// converts exactly. Then the app, on the same worker: the drawn display in a tile for
    /// [`RUN`], read back as arrival → present on its own surface when the window server
    /// reports it.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn drawn_frames_reach_the_glass_on_loopback() {
        let mut stack = Stack::launch_with("e2e-worker", &[SYNTHETIC]).await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        first_shell(drv).await;
        for scale in ["0.5", "1"] {
            glass(&stack, scale).await;
        }

        let drv = &mut stack.driver;
        drv.ok(&Command::AddDisplay).await.unwrap();
        drv.wait_for("the drawn display's frames", FIRST_FRAMES, shown).await.unwrap();
        tokio::time::sleep(RUN).await;
        let dump = drv.dump().await.unwrap();
        let app = dump.screens[0].clone();
        println!("MEASURE glass, app surface: {}", row(&app));
        println!("MEASURE glass, app UI frames: {}", dump.frames.row());
        let worker = worker_side(&stack, &dump.client).await;
        println!("MEASURE glass, worker under the app: {}", worker.row());
        stack.shutdown().await;
    }
}
