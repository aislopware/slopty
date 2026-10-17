//! What an agent shows in its chat beyond words, on a real stack: a stand-in Claude Code
//! (`slopty-stub-claude`) calls Slopty's own `show_visual` and `show_page` through the `slopty`
//! MCP server the worker hands it (`STUB_TURNS`), and answers with a chart fence, a Mermaid
//! diagram and formulas. Each is judged by the accessibility tree and by numbers: what a page
//! reached, what the card's actions left on the run's pasteboard and in the folder, where a
//! formula sits on its line, what a tab changed in the picture.

#![cfg(target_os = "macos")]

use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use slopty_e2e::harness::{ServedApp, pasteboard_name};
use slopty_e2e::{A11yNode, Dump};
use slopty_platform::pasteboard::{MacPasteboard, Pasteboard as _};

use crate::served::{
    SETTLE, STEP, click, goldens, holding, hover, middle, node, open_thread, says, seen, start,
    tell, thread_until,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "media";

/// zeron's `show_visual` example: a tab per metric over a bar chart of the metric.
const LATENCY: &str = r#"
local s = ui.state { metric = "p95" }
local data = { { region = "us-east", p95 = 182, p99 = 240 }, { region = "eu-west", p95 = 140, p99 = 210 }, { region = "ap-south", p95 = 96, p99 = 300 } }
return function()
  return ui.column { gap = 16,
    ui.text { "Latency by region", size = "lg", weight = 600 },
    ui.tabs { options = { "p95", "p99" }, value = s.metric, on_change = function(v) s.metric = v end },
    ui.chart { type = "bar", height = 220, unit = "ms",
      x = ui.map(data, function(d) return d.region end),
      series = { { name = s.metric, values = ui.map(data, function(d) return d[s.metric] end) } } },
  }
end
"#;

/// A stack whose stand-in calls `turns` (`STUB_TURNS`) for the prompts they name.
async fn stack(turns: &serde_json::Value) -> ServedApp {
    let turns = turns.to_string();
    ServedApp::launch_with(MACHINE, &[], &[("STUB_TURNS", &turns)]).await.unwrap()
}

/// How many of `a`'s pixels inside `rect` (points, at `scale`) differ from `b`'s.
fn changed_inside(
    before: &image::RgbaImage,
    after: &image::RgbaImage,
    rect: [f32; 4],
    scale: f32,
) -> usize {
    let [left, top, width, height] = rect;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0) as u32;
    let (x0, y0) = (px(left), px(top));
    let (x1, y1) = (px(left + width).min(before.width()), px(top + height).min(before.height()));
    (y0..y1)
        .flat_map(|row| (x0..x1).map(move |col| (col, row)))
        .filter(|&(col, row)| before.get_pixel(col, row) != after.get_pixel(col, row))
        .count()
}

/// How many pixels inside `rect` (points, at `scale`) stand apart from the colour at its middle
/// top edge, its background.
fn ink_inside(image: &image::RgbaImage, rect: [f32; 4], scale: f32) -> usize {
    let [left, top, width, height] = rect;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0) as u32;
    let (x0, y0) = (px(left), px(top));
    let (x1, y1) = (px(left + width).min(image.width()), px(top + height).min(image.height()));
    if x1 <= x0 || y1 <= y0 {
        return 0;
    }
    let middle = x0.saturating_add(x1.saturating_sub(x0) / 2);
    let ground = *image.get_pixel(middle, y0.saturating_add(2).min(y1.saturating_sub(1)));
    let apart = |pixel: &image::Rgba<u8>| {
        pixel.0.iter().zip(ground.0).take(3).any(|(one, other)| one.abs_diff(other) > 24)
    };
    (y0..y1)
        .flat_map(|row| (x0..x1).map(move |col| (col, row)))
        .filter(|&(col, row)| apart(image.get_pixel(col, row)))
        .count()
}

/// The nodes `role` drawn inside `group`.
fn inside<'a>(dump: &'a Dump, group: &A11yNode, role: &str) -> Vec<&'a A11yNode> {
    let [gx, gy, gw, gh] = group.bounds;
    dump.a11y
        .iter()
        .filter(|n| n.role == role)
        .filter(|n| {
            let (x, y) = middle(n);
            x >= gx && x <= gx + gw && y >= gy && y <= gy + gh
        })
        .collect()
}

/// An agent's `show_visual` draws natively in its chat: the stand-in calls Slopty's tool with
/// zeron's latency example, the view model shows the visual after the call, and the shell reads
/// its script through the thread (`ReadAttachmentChunk` of `slopty-visual:{thread}/{item}`) and
/// runs it. The visual is a group named for its title holding the script's tab list; a click on
/// "p99" redraws the chart's bars, and only inside the visual.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_visual_draws_from_its_script_read_through_the_thread() {
    let prompt = "Show the latency";
    let turns = json!({ prompt: [{ "name": "show_visual", "arguments": {
        "title": "Latency", "script": LATENCY,
    }}]});
    let mut stack = stack(&turns).await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, prompt).await;
    thread_until(&stack, &agent, "showed the visual", |r| says(r, "Done: Show the latency")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let drawn = seen(drv, "the visual's tabs", STEP, |d| {
        node(d, "Group", "Latency").is_some() && node(d, "Tab", "p99").is_some()
    })
    .await;
    let visual = node(&drawn, "Group", "Latency").cloned().unwrap();
    let tabs = inside(&drawn, &visual, "Tab");
    assert_eq!(tabs.len(), 2, "the script's two tabs, inside its visual");
    assert!(
        drawn.a11y.iter().all(|n| n.label.as_deref() != Some("Loading visual…")),
        "the script was read and ran"
    );
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "media-visual", &[]).await;

    // The pointer rests on the sidebar for both pictures, so no hover strip or wash differs.
    let rest = slopty_e2e::Command::Move { x: 100.0, y: 600.0 };
    drv.ok(&rest).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    let before = drv.render(&renders.join("media-visual-p95.png")).await.unwrap();
    click(drv, "Tab", "p99").await;
    drv.ok(&rest).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    let after = drv.render(&renders.join("media-visual-p99.png")).await.unwrap();
    let inside_visual = changed_inside(&before.image, &after.image, visual.bounds, after.scale);
    let [vx, vy, vw, vh] = visual.bounds;
    let whole = [0.0, 0.0, f32::MAX / 4.0, f32::MAX / 4.0];
    let everywhere = changed_inside(&before.image, &after.image, whole, after.scale);
    println!(
        "MEASURE visual-tab: {inside_visual} pixels changed inside the visual \
         ({vw:.0}x{vh:.0} at {vx:.0},{vy:.0}), {everywhere} in the window"
    );
    assert!(inside_visual > 200, "p99 redrew the bars: {inside_visual} pixels");
    assert!(
        everywhere.saturating_sub(inside_visual) < 400,
        "nothing outside the visual moved: {} pixels",
        everywhere.saturating_sub(inside_visual)
    );
    stack.shutdown().await;
}

/// Count the connections `listener` takes, for as long as the run lasts.
fn count_connections(listener: std::net::TcpListener) -> Arc<AtomicUsize> {
    let reached = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reached);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0_u8; 512];
            let _read = stream.read(&mut buf);
        }
    });
    reached
}

/// An inline page is sandboxed and its card acts for the person: a stand-in's `show_page` of a
/// page that tries to reach a server on this Mac (a fetch, a beacon), which never
/// hears from it. The card is the page's group, live; on hover its header offers Copy image,
/// Save, Open in pane and Expand page. Copy image leaves a PNG on the run's pasteboard, Save
/// writes the page into the chat's folder, and Open in pane leaves the card's one line, whose
/// Bring back returns it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_page_card_is_sandboxed_and_acts_for_the_person() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let reached = count_connections(listener);
    let html = format!(
        "<!doctype html><html><body style=\"margin:0\">\
         <div style=\"height:220px;display:flex;align-items:center;justify-content:center;\
         font:600 20px -apple-system\">Probe page</div>\
         <script>fetch('http://127.0.0.1:{port}/fetch').catch(() => {{}});\
         navigator.sendBeacon && navigator.sendBeacon('http://127.0.0.1:{port}/beacon', 'x');\
         </script></body></html>"
    );
    let prompt = "Show the page";
    let turns = json!({ prompt: [{ "name": "show_page", "arguments": {
        "title": "Probe", "html": html,
    }}]});
    let mut stack = stack(&turns).await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, prompt).await;
    thread_until(&stack, &agent, "showed the page", |r| says(r, "Done: Show the page")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    // The card rests on its picture until the pointer comes to it, which makes it live.
    let resting = seen(drv, "the page's card", STEP, |d| {
        node(d, "Group", "Page: Probe")
            .is_some_and(|n| matches!(n.value.as_deref(), Some("live" | "snapshot")))
    })
    .await;
    let group = node(&resting, "Group", "Page: Probe").cloned().unwrap();
    hover(drv, &group).await;
    seen(drv, "the page's card, live", STEP, |d| {
        node(d, "Group", "Page: Probe").is_some_and(|n| n.value.as_deref() == Some("live"))
    })
    .await;
    // The page ran (it is live) for a while: long enough to have reached the server if it could.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(reached.load(Ordering::SeqCst), 0, "the page reached nothing on this Mac");
    println!("MEASURE page-sandbox: 0 connections from a live page that asked for 2");

    hover(drv, &group).await;
    let offered = seen(drv, "the card's actions", STEP, |d| {
        ["Copy image", "Save", "Open in pane", "Expand page"]
            .iter()
            .all(|label| node(d, "Button", label).is_some())
    })
    .await;
    let [_, gy, _, gh] = group.bounds;
    println!("MEASURE page-card: {gh:.0} pt tall at y {gy:.0}");
    assert!(gh >= 120.0, "the card holds the page's 220 px: {gh}");
    drop(offered);
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "media-page", &[]).await;

    // Copy image: a PNG on the run's pasteboard.
    let board = pasteboard_name(stack.dir.path(), "app");
    let drv = &mut stack.driver;
    hover(drv, &group).await;
    click(drv, "Button", "Copy image").await;
    let since = std::time::Instant::now();
    let png = loop {
        let name = board.clone();
        let png = tokio::task::spawn_blocking(move || {
            MacPasteboard::named(&name).item_data(0, "public.png")
        })
        .await
        .unwrap();
        if let Some(png) = png {
            break png;
        }
        assert!(since.elapsed() < STEP, "Copy image put a PNG on the pasteboard");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let picture = image::load_from_memory(&png).unwrap();
    println!("MEASURE page-copy: a {}x{} PNG", picture.width(), picture.height());
    assert!(picture.width() > 100 && picture.height() > 100, "the page's picture");

    // Save: the page, written into the chat's folder.
    let drv = &mut stack.driver;
    hover(drv, &group).await;
    click(drv, "Button", "Save").await;
    let since = std::time::Instant::now();
    let saved = loop {
        let saved: Vec<_> = std::fs::read_dir(&repo)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "html"))
            .collect();
        if let Some(saved) = saved.first() {
            break saved.clone();
        }
        assert!(since.elapsed() < STEP, "Save wrote the page into the folder");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let text = std::fs::read_to_string(&saved).unwrap_or_default();
    assert!(text.contains("Probe page"), "the page's own document: {}", saved.display());

    // Open in pane, then Bring back.
    let drv = &mut stack.driver;
    hover(drv, &group).await;
    click(drv, "Button", "Open in pane").await;
    seen(drv, "the card's line", STEP, |d| {
        node(d, "Group", "Shown in the pane").is_some() && node(d, "Button", "Bring back").is_some()
    })
    .await;
    click(drv, "Button", "Bring back").await;
    seen(drv, "the page back in its card", STEP, |d| {
        node(d, "Group", "Shown in the pane").is_none()
            && node(d, "Group", "Page: Probe").is_some_and(|n| n.value.as_deref() == Some("live"))
    })
    .await;
    assert_eq!(reached.load(Ordering::SeqCst), 0, "nor from the pane");
    stack.shutdown().await;
}

/// A one-page PDF, written by hand: a page 300 × 400 points with a filled square on it.
fn pdf() -> Vec<u8> {
    let content = "0.2 0.5 0.8 rg 50 150 200 200 re f";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (n, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        let number = n.saturating_add(1);
        out.extend_from_slice(format!("{number} 0 obj\n{object}\nendobj\n").as_bytes());
    }
    let xref = out.len();
    let size = objects.len().saturating_add(1);
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// What a chart fence holds: a Vega-Lite bar chart with its own title.
const VEGA_LITE: &str = r#"{"$schema":"https://vega.github.io/schema/vega-lite/v6.json","title":"Requests by day","data":{"values":[{"day":"Mon","n":12},{"day":"Tue","n":30},{"day":"Wed","n":21}]},"mark":"bar","encoding":{"x":{"field":"day","type":"nominal"},"y":{"field":"n","type":"quantitative"}}}"#;

/// A chart fence, a PDF and a CSV draw in the chat: a reply's ```` ```vega-lite ```` fence is a
/// card of its own named by the chart's title, drawn (live, or resting on its picture) from the
/// libraries the app vendors, with
/// Copy source; `show_page` of a PDF draws its page natively, an image "page 1 of 1" that is
/// drawn; and of a CSV, a table node with a row per record and a cell per field, with Copy table.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_chart_fence_a_pdf_and_a_csv_draw_in_the_chat() {
    let files = "Show the files";
    let turns = json!({ files: [
        { "name": "show_page", "arguments": { "title": "Report", "file": "report.pdf" } },
        { "name": "show_page", "arguments": { "title": "Latency", "file": "latency.csv" } },
    ]});
    let mut stack = stack(&turns).await;
    let repo = stack.path("repo");
    std::fs::write(repo.join("report.pdf"), pdf()).unwrap();
    let csv = "region,p50,p99\nus-east,40,180\neu-west,41,190\nap-south,42,200\nsa-east,43,210\n";
    std::fs::write(repo.join("latency.csv"), csv).unwrap();

    let chart = format!("Chart:\n\n```vega-lite\n{VEGA_LITE}\n```");
    let agent = start(&stack, MACHINE, &repo, &chart).await;
    thread_until(&stack, &agent, "answered with the chart", |r| says(r, "Done: Chart:")).await;
    let renders = stack.path("");

    // The chart first, on screen alone: the files' cards below it push it out of view.
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let charted = seen(drv, "the chart", STEP, |d| {
        node(d, "Group", "Page: Requests by day")
            .is_some_and(|n| matches!(n.value.as_deref(), Some("live" | "snapshot")))
    })
    .await;
    let chart = node(&charted, "Group", "Page: Requests by day").cloned().unwrap();
    hover(drv, &chart).await;
    seen(drv, "the chart's actions", STEP, |d| node(d, "Button", "Copy source").is_some()).await;

    // Then the PDF and the CSV, each a card named by the title the agent gave it.
    tell(&mut stack, files).await;
    thread_until(&stack, &agent, "showed the files", |r| says(r, "Done: Show the files")).await;
    let drv = &mut stack.driver;
    let shown = seen(drv, "the PDF and the table", STEP, |d| {
        node(d, "Image", "page 1 of 1").is_some_and(|n| n.value.as_deref() == Some("drawn"))
            && node(d, "Table", "Latency").is_some()
    })
    .await;
    let table = node(&shown, "Table", "Latency").cloned().unwrap();
    let rows = inside(&shown, &table, "Row").len();
    let cells = inside(&shown, &table, "Cell").len();
    let heads = inside(&shown, &table, "ColumnHeader").len();
    println!("MEASURE csv: {rows} rows, {heads} column headers, {cells} cells");
    assert_eq!((rows, heads, cells), (5, 3, 12), "a header and four records of three fields");
    assert!(node(&shown, "Cell", "sa-east").is_some(), "the last record");

    // The page is taller than the chat's band, so its node is clipped: back up until its top
    // shows, then judge its shape by what it draws. The PDF fills a square, 200 by 200 of its
    // 300 by 400, so a page drawn in its own shape shows a square.
    let [x, y, w, h] = table.bounds;
    let mut page = node(&shown, "Image", "page 1 of 1").cloned().unwrap();
    for _ in 0..20 {
        if page.bounds[1] > 60.0 {
            break;
        }
        drv.scroll(x + w / 2.0, y + h / 2.0, 0.0, 3.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let now = drv.dump().await.unwrap();
        page = node(&now, "Image", "page 1 of 1").cloned().unwrap_or(page);
    }
    tokio::time::sleep(SETTLE).await;
    // Each card's header links its file by path, in the run's own scratch folder.
    goldens(drv, &renders, "media-files", &["/repo/"]).await;
    let frame = drv.render(&renders.join("media-files-shape.png")).await.unwrap();
    let (square_w, square_h) = blue_extent(&frame, page.bounds);
    println!("MEASURE pdf: its square drawn {square_w}x{square_h} px in a page {:?}", page.bounds);
    assert!(square_w > 20, "the page's square is drawn: {square_w}x{square_h} px");
    assert!(
        square_w.abs_diff(square_h) <= 2,
        "the page keeps its shape: its square is {square_w}x{square_h} px"
    );
    stack.shutdown().await;
}

/// The width and height in pixels of the PDF's blue square (0.2 0.5 0.8) inside `bounds`, as
/// `frame` draws it.
fn blue_extent(frame: &slopty_e2e::snapshot::Frame, bounds: [f32; 4]) -> (u32, u32) {
    let [left_pt, top_pt, width_pt, height_pt] = bounds;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * frame.scale).max(0.0) as u32;
    let (x0, y0) = (px(left_pt), px(top_pt));
    let x1 = px(left_pt + width_pt).min(frame.image.width());
    let y1 = px(top_pt + height_pt).min(frame.image.height());
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for row in y0..y1 {
        for column in x0..x1 {
            let [red, green, blue, _] = frame.image.get_pixel(column, row).0;
            if blue > 150 && red < 90 && green > 90 && green < 160 {
                (left, top) = (left.min(column), top.min(row));
                (right, bottom) = (right.max(column), bottom.max(row));
            }
        }
    }
    if right < left || bottom < top {
        return (0, 0);
    }
    (right.saturating_sub(left).saturating_add(1), bottom.saturating_sub(top).saturating_add(1))
}

/// Formulas and diagrams in a reply: an inline `$…$` is typeset on its line, a math node
/// labelled with its TeX no taller than the line around it, and a display `$$…$$` set apart and
/// taller; a ```` ```mermaid ```` fence draws as a diagram.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn formulas_sit_on_their_line_and_diagrams_draw() {
    let reply = "Notes:\n\nThe area is $\\pi r^2$ for a circle of radius $r$.\n\n\
                 $$\\int_0^1 x^2\\,dx = \\frac{1}{3}$$\n\n\
                 ```mermaid\nflowchart LR\n  plan[Plan] --> build[Build] --> ship[Ship]\n```";
    let mut stack = stack(&json!({})).await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, reply).await;
    thread_until(&stack, &agent, "answered", |r| says(r, "Done: Notes:")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let shown = seen(drv, "the formulas", STEP, |d| {
        node(d, "Math", "\\pi r^2").is_some()
            && node(d, "Math", "\\int_0^1 x^2\\,dx = \\frac{1}{3}").is_some()
    })
    .await;
    let inline = node(&shown, "Math", "\\pi r^2").cloned().unwrap();
    let display = node(&shown, "Math", "\\int_0^1 x^2\\,dx = \\frac{1}{3}").cloned().unwrap();
    let [inline_x, inline_y, inline_w, inline_h] = inline.bounds;
    let [_, dy, _, dh] = display.bounds;
    println!(
        "MEASURE math: inline {inline_w:.1}x{inline_h:.1} at {inline_x:.0},{inline_y:.0}; \
         display {dh:.1} tall at {dy:.0}"
    );
    assert!(inline_h > 6.0 && inline_h <= 24.0, "the inline formula fits its line: {inline_h}");
    assert!(dh > inline_h, "the display formula is taller than the inline one: {dh} vs {inline_h}");
    assert!(dy > inline_y + inline_h, "the display formula sits below the line");
    // The line's words around the inline formula: the paragraph holds it.
    // Not the person's message, which holds the words too: the reply's.
    let line = crate::served::replies(&shown)
        .iter()
        .find(|n| n.label.as_deref().is_some_and(|l| l.contains("The area is")))
        .cloned();
    if let Some(line) = line {
        let [_, ly, _, lh] = line.bounds;
        assert!(
            inline_y >= ly - 2.0 && inline_y + inline_h <= ly + lh + 2.0,
            "inside its line: {:?} in {:?}",
            inline.bounds,
            line.bounds
        );
    }
    // The diagram is an image ("Enlarge image") drawn off the main thread: it has drawn once
    // its box holds ink beyond its background.
    let diagram =
        seen(drv, "the diagram's image", STEP, |d| node(d, "Button", "Enlarge image").is_some())
            .await;
    let diagram = node(&diagram, "Button", "Enlarge image").cloned().unwrap();
    let since = std::time::Instant::now();
    let ink = loop {
        let frame = drv.render(&renders.join("media-math-diagram.png")).await.unwrap();
        let ink = ink_inside(&frame.image, diagram.bounds, frame.scale);
        if ink > 400 || since.elapsed() > STEP {
            break ink;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    println!("MEASURE mermaid: {ink} pixels of ink in the diagram's box");
    assert!(ink > 400, "the Mermaid diagram drew: {ink} pixels of ink in {:?}", diagram.bounds);
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "media-math", &[]).await;
    // The person's own message keeps the fence they sent; the reply draws it.
    let shown = drv.dump().await.unwrap();
    let source_shown =
        slopty_e2e::frame_text(crate::served::replies(&shown)).contains("```mermaid");
    assert!(!source_shown, "the diagram is drawn, not its source");
    stack.shutdown().await;
}

unsafe extern "C" {
    /// libsystem's process responsible for `pid`: for a `WebKit` XPC service, the app whose
    /// `WebView` started it (as Activity Monitor files a "Web Content" under its app).
    safe fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
}

/// The `WebKit` content processes started for the app `app`, now: those whose responsible
/// process is the app's own. An app the harness started may answer to the process that started
/// it, whose other apps' pages then count too: an upper bound, never fewer than the app's own.
fn web_content_of(app: u32) -> usize {
    let Ok(app) = i32::try_from(app) else { return 0 };
    let owner = responsibility_get_pid_responsible_for_pid(app);
    let listed = std::process::Command::new("pgrep")
        .args(["-f", "com.apple.WebKit.WebContent"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    listed
        .split_whitespace()
        .filter_map(|pid| pid.parse::<i32>().ok())
        .filter(|&pid| {
            let responsible = responsibility_get_pid_responsible_for_pid(pid);
            responsible == app || responsible == owner
        })
        .count()
}

/// Count `app`'s `WebKit` content processes every 200 ms until `stop`; the most seen at once.
fn watch_web_content(app: u32, stop: Arc<AtomicUsize>) -> tokio::task::JoinHandle<usize> {
    tokio::spawn(async move {
        let count = move || tokio::task::spawn_blocking(move || web_content_of(app));
        let mut most = 0;
        while stop.load(Ordering::SeqCst) == 0 {
            most = most.max(count().await.unwrap_or(0));
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        most.max(count().await.unwrap_or(0))
    })
}

/// `us` microseconds in milliseconds.
fn ms(us: u64) -> f64 {
    Duration::from_micros(us).as_secs_f64() * 1000.0
}

/// Where the chat is scrolled: in the transcript's list, right of its column, where no card
/// sits, so the wheel reaches the list and not a page.
const SCROLL_AT: (f32, f32) = (1240.0, 400.0);

/// Whether a page card's group, `Page: …`.
fn is_card(n: &A11yNode) -> bool {
    n.role == "Group" && n.label.as_deref().is_some_and(|l| l.starts_with("Page: "))
}

/// Scroll the open chat to its top (where `first` reads), then down 40 steps and back up,
/// reading the frame probe: draw p95 and the frames drawn.
async fn scrolled(drv: &mut slopty_e2e::Driver, first: &str) -> (u64, u64) {
    let (x, y) = SCROLL_AT;
    for _ in 0..30 {
        drv.scroll(x, y, 0.0, 400.0).await.unwrap();
    }
    seen(drv, "the chat's top", STEP, |d| holding(d, "Paragraph", first).is_some()).await;
    tokio::time::sleep(SETTLE).await;
    drv.frames_reset().await.unwrap();
    for dy in std::iter::repeat_n(-60.0, 40).chain(std::iter::repeat_n(60.0, 40)) {
        drv.scroll(x, y, 0.0, dy).await.unwrap();
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let frames = drv.dump().await.unwrap().frames;
    (frames.draw_p95_us, frames.frames)
}

/// Twelve pages in one chat cost about what words do: the window presents at most 3 of their
/// `WebView`s at once (the cards off screen or at rest are pictures), the app holds at most 8
/// `WebKit` content processes through the whole run, and scrolling the chat draws its frames with
/// a p95 within 0.3 ms of a chat as long with no page in it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn twelve_pages_in_one_chat_stay_within_budget() {
    use std::fmt::Write as _;
    const PAGES: &str = "Show twelve pages";
    const NATIVES: usize = 3;
    const WEB_CONTENT: usize = 8;
    const P95_OVER_US: u64 = 300;
    const WINDOW_HEIGHT: f32 = crate::served::WINDOW.1;
    let calls: Vec<serde_json::Value> = (1..=12_u32)
        .map(|n| {
            let hue = n.saturating_mul(30);
            let html = format!(
                "<!doctype html><html><body style=\"margin:0\"><div style=\"height:240px;\
                 display:flex;align-items:center;justify-content:center;\
                 background:hsl({hue} 60% 85%);font:600 20px -apple-system\">Page {n}</div>\
                 </body></html>"
            );
            json!({ "name": "show_page", "arguments": { "title": format!("Page {n:02}"), "html": html } })
        })
        .collect();
    let turns = json!({ PAGES: calls });
    // As long a chat in words: twelve sections of ten lines, about a page card's height each.
    let mut baseline = "Baseline\n\n".to_owned();
    for section in 1..=12 {
        for line in 1..=10 {
            write!(baseline, "Section {section}, line {line}: words where a page would be.\n\n")
                .unwrap();
        }
    }
    let mut stack = stack(&turns).await;
    let app = stack.app.id().unwrap();
    let stop = Arc::new(AtomicUsize::new(0));
    let web_content = watch_web_content(app, Arc::clone(&stop));
    let repo = stack.path("repo");
    let words = start(&stack, MACHINE, &repo, &baseline).await;
    thread_until(&stack, &words, "answered", |r| says(r, "Done: Baseline")).await;
    let pages = start(&stack, MACHINE, &repo, PAGES).await;
    thread_until(&stack, &pages, "showed the pages", |r| says(r, "Done: Show twelve pages")).await;

    // The sidebar lists the newest first: ⌘1 is the pages, ⌘2 the words.
    let drv = &mut stack.driver;
    open_thread(drv, 2).await;
    seen(drv, "the words", STEP, |d| {
        holding(d, "Paragraph", "words where a page would be").is_some()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    let (words_p95, words_frames) = scrolled(drv, "Section 1, line 1:").await;
    open_thread(drv, 1).await;
    seen(drv, "a page's card", STEP, |d| {
        d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Page: Page ")))
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    let (pages_p95, pages_frames) = scrolled(drv, PAGES).await;

    // Down the chat a step at a time from its top, the pointer resting on each card in sight
    // (a card under it goes live): the natives presented at each rest, and the cards live.
    let (x, y) = SCROLL_AT;
    let (mut most_natives, mut most_live, mut cards) = (0, 0, std::collections::BTreeSet::new());
    // The driver scrolls in lines, eight points each in the transcript: 18 lines are 144 pt, and
    // 25 of them go past the chat's 3600 pt.
    for _ in 0..25 {
        drv.scroll(x, y, 0.0, -18.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let shown = drv.dump().await.unwrap();
        let in_sight: Vec<A11yNode> = shown
            .a11y
            .iter()
            .filter(|n| is_card(n) && n.bounds[1] >= 0.0 && n.bounds[1] < WINDOW_HEIGHT)
            .cloned()
            .collect();
        for card in &in_sight {
            cards.insert(card.label.clone().unwrap_or_default());
            hover(drv, card).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            let dump = drv.dump().await.unwrap();
            let live = dump
                .a11y
                .iter()
                .filter(|n| is_card(n) && n.value.as_deref() == Some("live"))
                .count();
            most_natives = most_natives.max(dump.natives);
            most_live = most_live.max(live);
        }
    }
    // The wheel over a live card: whether the chat under it moves (the page has nothing of its
    // own to scroll).
    let shown = drv.dump().await.unwrap();
    if let Some(card) = shown.a11y.iter().find(|n| is_card(n) && n.bounds[1] > 100.0).cloned() {
        hover(drv, &card).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (cx, cy) = middle(&card);
        drv.scroll(cx, cy, 0.0, 18.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        let after = drv.dump().await.unwrap();
        let moved = after
            .a11y
            .iter()
            .find(|n| n.label == card.label && n.role == "Group")
            .map_or(f32::NAN, |n| n.bounds[1] - card.bounds[1]);
        slopty_e2e::harness::measure(&format!(
            "pages-wheel-over-card: the chat moved {moved:.0} pt for a 144 pt wheel"
        ));
    }
    stop.store(1, Ordering::SeqCst);
    let most_web_content = web_content.await.unwrap();
    slopty_e2e::harness::measure(&format!(
        "pages-stress: 12 page cards ({} seen), at most {most_natives} natives presented \
         and {most_live} cards live at once, at most {most_web_content} WebContent processes; \
         scroll draw p95 {:.2} ms with pages ({pages_frames} frames), {:.2} ms in words \
         ({words_frames} frames)",
        cards.len(),
        ms(pages_p95),
        ms(words_p95),
    ));
    assert_eq!(cards.len(), 12, "every page has its card: {cards:?}");
    assert!(most_natives <= NATIVES, "at most {NATIVES} natives presented: {most_natives}");
    assert!(
        most_web_content <= WEB_CONTENT,
        "at most {WEB_CONTENT} WebContent processes: {most_web_content}"
    );
    assert!(
        pages_p95 <= words_p95.saturating_add(P95_OVER_US),
        "scrolling past pages draws within {P95_OVER_US} µs of words at p95: {pages_p95} µs \
         against {words_p95} µs"
    );
    stack.shutdown().await;
}

/// The MCP App's prompt, as the recording below says it.
const APP_PROMPT: &str = "Echo hi through the app";

/// A recorded turn (`STUB_REPLAY`'s folder) whose one call is the stand-in MCP App's `echo`
/// (`slopty-stub-mcp-apps`), then an answer: what Claude Code writes for a call of an app's
/// tool.
fn echo_recording(folder: &std::path::Path) {
    const ID: &str = "00000000-0000-4000-8000-0000000000";
    const SESSION: &str = "00000000-0000-4000-8000-000000000001";
    let record = |n: u32, parent: Option<u32>, kind: &str, at: u32, message: serde_json::Value| {
        let mut line = json!({
            "cwd": "/work", "entrypoint": "sdk-cli", "gitBranch": "HEAD", "isSidechain": false,
            "message": message, "parentUuid": parent.map(|p| format!("{ID}{p:02}")),
            "promptId": format!("{ID}02"), "sessionId": SESSION,
            "timestamp": format!("2026-10-16T06:00:{at:02}.000Z"), "type": kind,
            "userType": "external", "uuid": format!("{ID}{n:02}"), "version": "2.1.301",
        });
        if kind == "assistant" {
            line["requestId"] = json!(format!("req_{n:02}"));
        }
        line
    };
    let said = |content: serde_json::Value, id: &str, stop: &str| {
        json!({
            "content": content, "id": id, "model": "claude-haiku-5-5", "role": "assistant",
            "stop_reason": stop, "type": "message", "usage": { "input_tokens": 2, "output_tokens": 20 },
        })
    };
    let echoed = json!([{ "type": "text", "text": "hi" }]);
    let mut result = record(
        5,
        Some(4),
        "user",
        2,
        json!({ "role": "user", "content": [{
        "type": "tool_result", "tool_use_id": "toolu_01", "content": echoed,
    }] }),
    );
    result["sourceToolAssistantUUID"] = json!(format!("{ID}04"));
    result["toolUseResult"] =
        json!({ "content": echoed, "structuredContent": { "echo": "hi", "hostUi": true } });
    let lines = [
        record(3, None, "user", 0, json!({ "role": "user", "content": APP_PROMPT })),
        record(
            4,
            Some(3),
            "assistant",
            1,
            said(
                json!([{
                    "type": "tool_use", "id": "toolu_01", "name": "mcp__apps__echo",
                    "input": { "text": "hi" },
                }]),
                "msg_01",
                "tool_use",
            ),
        ),
        result,
        record(
            6,
            Some(5),
            "assistant",
            3,
            said(
                json!([{
                    "type": "text", "text": "Echoed hi through the app's page.",
                }]),
                "msg_02",
                "end_turn",
            ),
        ),
    ];
    std::fs::create_dir_all(folder).unwrap();
    let transcript = lines.iter().fold(String::new(), |mut text, line| {
        use std::fmt::Write as _;
        writeln!(text, "{line}").unwrap();
        text
    });
    std::fs::write(folder.join("transcript.jsonl"), transcript).unwrap();
    std::fs::write(folder.join("hooks.jsonl"), "").unwrap();
}

/// An MCP App's card, end to end: Claude Code (the stand-in, playing a recorded call of the
/// stand-in app's `echo`) lists the folder's `apps` server with its `ui://` page, so the call
/// shows as the app's page card; the card reads its page through Claude Code
/// (`mcp_read_resource`), never from the server itself, and draws it live.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn an_mcp_app_call_shows_its_page_read_through_claude_code() {
    let mut stack = ServedApp::launch_rooted(MACHINE, &[], |root| {
        let replay = json!({ APP_PROMPT: root.join("echo-app") });
        vec![
            ("STUB_REPLAY".to_owned(), replay.to_string()),
            ("STUB_RECORD".to_owned(), root.join("heard.json").to_string_lossy().into_owned()),
        ]
    })
    .await
    .unwrap();
    echo_recording(&stack.path("echo-app"));
    let apps = slopty_e2e::harness::testkit_bin("slopty-stub-mcp-apps").await.unwrap();
    let log = stack.path("apps.log");
    let servers = json!({ "mcpServers": { "apps": {
        "command": apps, "args": ["--log", log],
    } } });
    let repo = stack.path("repo");
    std::fs::write(repo.join(".mcp.json"), servers.to_string()).unwrap();
    // A first turn, so Claude Code has listed its servers' apps (`mcp_status`, asked once it
    // starts) before the app's tool is called.
    let agent = start(&stack, MACHINE, &repo, "Say hello").await;
    thread_until(&stack, &agent, "said hello", |r| says(r, "Done: Say hello")).await;
    tell(&mut stack, APP_PROMPT).await;
    thread_until(&stack, &agent, "echoed", |r| says(r, "Echoed hi")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    let shown = seen(drv, "the app's card", STEP, |d| {
        node(d, "Group", "Page: echo")
            .is_some_and(|n| matches!(n.value.as_deref(), Some("live" | "snapshot")))
    })
    .await;
    let card = node(&shown, "Group", "Page: echo").cloned().unwrap();
    hover(drv, &card).await;
    seen(drv, "the app's card, live", STEP, |d| {
        node(d, "Group", "Page: echo").is_some_and(|n| n.value.as_deref() == Some("live"))
    })
    .await;
    // Its page came through Claude Code's door, by the app's own URI.
    let record = stack.path("heard.json");
    let since = std::time::Instant::now();
    let read = loop {
        let reads =
            crate::served::control_requests(&crate::served::heard(&record), "mcp_read_resource");
        if let Some(read) = reads.into_iter().find(|r| r["request"]["uri"] == "ui://apps/echo") {
            break read;
        }
        assert!(since.elapsed() < STEP, "the card read its page through Claude Code");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(read["request"]["serverName"], "apps", "{read}");
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(logged.contains("resources/read"), "the app's server was read: {logged}");
    let [_, gy, _, gh] = card.bounds;
    println!(
        "MEASURE mcp-app-card: {gh:.0} pt tall at y {gy:.0}, its page read through mcp_read_resource"
    );
    tokio::time::sleep(SETTLE).await;
    let drv = &mut stack.driver;
    goldens(drv, &renders, "media-app", &[]).await;
    stack.shutdown().await;
}
