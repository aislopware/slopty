//! A finger over a live page card on the simulator, as UIKit itself delivers it
//! ([`slopty_e2e::Command::UiFinger`]), so the page's web view and its scroll views see it as a
//! person's: over a page with nothing to scroll a drag scrolls the chat by the whole drag and the
//! fling carries it on; a tap reaches the page; a tall page scrolls itself and the chat stays.

use std::time::Duration;

use serde_json::json;
use slopty_e2e::harness::{Simulator, Stack, measure, slopty_json};
use slopty_e2e::{A11yNode, Driver, Dump};

/// Per-step wait.
const STEP: Duration = Duration::from_secs(30);
/// A viewport narrower than this is a phone's (the shell's `PHONE_BELOW`).
const PHONE_BELOW: f32 = 700.0;
/// A card's header line over its page (the shell's `HEADER_HEIGHT`).
const HEADER: f32 = 26.0;
/// How far each drag goes, points, in its moves 16 ms apart.
const DRAG: f32 = 160.0;
const MOVES: u32 = 10;
/// The short page's height before and after a tap on it, CSS pixels.
const SHORT: u32 = 160;
const TAPPED: u32 = 400;
/// The heights the tall page says once a finger touched it, and once it has scrolled, CSS
/// pixels: its card's height tells which reached it.
const TOUCHED: f32 = 500.0;
const SCROLLED: f32 = 200.0;

/// The booted simulator `cargo xtask e2e ios` installed the app on.
fn simulator() -> Simulator {
    let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
    let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
    Simulator { udid, bundle_id }
}

/// A page that says its own height to its host (MCP Apps' `size-changed`), `body` its content
/// and `script` more of its own.
fn page(height: u32, body: &str, script: &str) -> String {
    format!(
        "<!doctype html><html><body style=\"margin:0\">{body}<script>\
         const size = (height) => parent.postMessage({{ jsonrpc: '2.0', \
           method: 'ui/notifications/size-changed', params: {{ height }} }}, '*');\
         size({height});{script}</script></body></html>"
    )
}

/// The card of the page titled `title`.
fn card<'a>(d: &'a Dump, title: &str) -> Option<&'a A11yNode> {
    d.a11y
        .iter()
        .find(|n| n.role == "Group" && n.label.as_deref() == Some(&format!("Page: {title}")))
}

/// The card of `title` once it has drawn (its first picture, or live) and stands wholly between
/// `top` and `bottom` (window points).
async fn drawn_card(drv: &mut Driver, title: &str, top: f32, bottom: f32) -> A11yNode {
    let since = std::time::Instant::now();
    loop {
        let dump = drv.dump().await.unwrap();
        if let Some(found) = card(&dump, title) {
            let [_, y, _, h] = found.bounds;
            let drawn = matches!(found.value.as_deref(), Some("snapshot" | "live"));
            if drawn && y >= top && y + h <= bottom {
                return found.clone();
            }
            assert!(
                since.elapsed() < STEP,
                "the {title} card never stood drawn in sight ({top}..{bottom}): {found:?}\n{}",
                slopty_e2e::frame_text(&dump.a11y)
            );
        } else {
            assert!(
                since.elapsed() < STEP,
                "no {title} card:\n{}",
                slopty_e2e::frame_text(&dump.a11y)
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The card of `title`, drawn in sight, made live: a card goes live on hover or a press
/// (`page.rs`, the page body's `engage`), and a touch screen has no hover, so a press on its
/// picture through GPUI (which the page itself never sees) wakes it. Its page is then the web
/// view a finger lands on.
async fn live_card(drv: &mut Driver, title: &str, top: f32, bottom: f32) -> A11yNode {
    let drawn = drawn_card(drv, title, top, bottom).await;
    if drawn.value.as_deref() != Some("live") {
        let [x, y, w, h] = drawn.bounds;
        let body_bottom = (y + h).min(bottom);
        drv.ui_tap(x + w / 2.0, f32::midpoint(y + HEADER, body_bottom)).await.unwrap();
    }
    let woke = drv
        .wait_for("the card live after a press", STEP, |d| {
            card(d, title).is_some_and(|n| n.value.as_deref() == Some("live"))
        })
        .await;
    match woke {
        Ok(dump) => card(&dump, title).cloned().unwrap_or(drawn),
        Err(e) => panic!("a press makes the {title} card live: {e:#}"),
    }
}

/// Where the card of `title` is now, its top and height.
async fn where_is(drv: &mut Driver, title: &str) -> (f32, f32) {
    let dump = drv.dump().await.unwrap();
    let found = card(&dump, title).unwrap_or_else(|| panic!("no {title} card"));
    (found.bounds[1], found.bounds[3])
}

/// The natives the window presents and where each takes the pointer, in words: its visible
/// bounds and the hitboxes above it there.
async fn natives_said(drv: &mut Driver) -> String {
    let dump = drv.dump().await.unwrap();
    let hits: Vec<String> = dump
        .native_hits
        .iter()
        .map(|hit| format!("{:?} under {:?}", hit.visible, hit.occluders))
        .collect();
    format!("{} natives presented; hit map: [{}]", dump.natives, hits.join("; "))
}

/// The chat wheeled toward its top (`up`) or its end at `x`, in its gutter beside the cards.
async fn wheel(drv: &mut Driver, x: f32, up: bool, times: usize) {
    let dy = if up { 400.0 } else { -400.0 };
    for _ in 0..times {
        drv.scroll(x, 300.0, 0.0, dy).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_finger_over_a_live_page_scrolls_the_chat_or_the_page() {
    let prompt = "Show the pages";
    let short = page(
        SHORT,
        &format!(
            "<div id=\"box\" style=\"height:{SHORT}px;background:#cde;font:600 18px \
             -apple-system\">Short page</div>"
        ),
        &format!(
            "document.body.addEventListener('click', () => {{\
               document.getElementById('box').style.height = '{TAPPED}px'; size({TAPPED}); }});"
        ),
    );
    // Taller than a card shows, so it scrolls in its card. A finger's touch has it say one
    // height and its first scroll a shorter one, which the card takes: the page's own word of
    // what reached it.
    let tall = page(
        3000,
        "<div style=\"height:3000px;background:linear-gradient(#fdc,#9be)\">Tall page</div>",
        &format!(
            "addEventListener('touchstart', () => size({TOUCHED}), {{ once: true }});\
             addEventListener('scroll', () => size({SCROLLED}), {{ once: true }});"
        ),
    );
    let filler = |n: u32| {
        page(300, &format!("<div style=\"height:300px;background:#eee\">Filler {n}</div>"), "")
    };
    let calls = json!([
        { "name": "show_page", "arguments": { "title": "Short", "html": short } },
        { "name": "show_page", "arguments": { "title": "Filler 1", "html": filler(1) } },
        { "name": "show_page", "arguments": { "title": "Filler 2", "html": filler(2) } },
        { "name": "show_page", "arguments": { "title": "Filler 3", "html": filler(3) } },
        { "name": "show_page", "arguments": { "title": "Tall", "html": tall } },
    ]);
    let turns = json!({ prompt: calls }).to_string();
    let mut stack = Stack::launch_on_simulator_with_stand_in(
        "e2e-ios-pages",
        simulator(),
        &[("STUB_TURNS", &turns)],
    )
    .await
    .unwrap();
    let drv = &mut stack.driver;
    let dump = drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let phone = dump.window.width < PHONE_BELOW;
    let (address, cli) = (stack.server.address().to_owned(), stack.path("cli"));
    let folder = stack.path("repo");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = folder.to_string_lossy().into_owned();
    let start = ["agent", "start", "e2e-ios-pages", "--folder", &folder, prompt];
    let started = slopty_json(&address, &cli, &start, b"").await.unwrap();
    let agent = started["agent"].as_str().expect("an agent started").to_owned();
    let since = std::time::Instant::now();
    loop {
        // A read may meet the worker between two links (a busy machine's worker goes silent a
        // few seconds and links again): read again, until the deadline.
        let args = ["agent", "read", "--thread", agent.as_str()];
        let read = match slopty_json(&address, &cli, &args, b"").await {
            Ok(read) if read.to_string().contains("Done: Show the pages") => break,
            Ok(read) => read.to_string(),
            Err(e) => format!("{e:#}"),
        };
        assert!(since.elapsed() < STEP * 3, "the thread never answered: {read}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Open the chat: from Chats on a phone, from the sidebar's list on an iPad.
    let drv = &mut stack.driver;
    let listed = drv
        .wait_for("the chat listed", STEP, |d| {
            d.a11y
                .iter()
                .any(|n| n.label.as_deref() == Some(prompt) || n.value.as_deref() == Some(prompt))
        })
        .await
        .unwrap();
    let row = listed
        .a11y
        .iter()
        .find(|n| n.label.as_deref() == Some(prompt) || n.value.as_deref() == Some(prompt))
        .map(|n| n.bounds)
        .unwrap();
    drv.ui_tap(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0).await.unwrap();
    let chat = drv
        .wait_for("the chat's composer", STEP, |d| {
            d.a11y.iter().any(|n| n.role == "MultilineTextInput")
        })
        .await
        .unwrap();
    let composer = chat.a11y.iter().find(|n| n.role == "MultilineTextInput").unwrap().bounds;
    // What of the chat shows: under the bars at the top, over the composer.
    let (top, bottom) = (100.0, composer[1] - 8.0);
    let device = if phone { "phone" } else { "pad" };

    // Each check that fails is said at the end, with the others' numbers.
    let mut missing: Vec<String> = Vec::new();

    // A tall page takes a drag it can scroll, and the chat stays. The chat opens at its end,
    // where the tall page is. A page takes up to 0.8 of the window, so on a phone the tall
    // card over its reply stands taller than what shows: its top is under the bars (the dump
    // cuts the card's bounds to what shows) and its end in sight.
    let tall_top = if phone { 0.0 } else { top };
    let tall = live_card(drv, "Tall", tall_top, bottom + 400.0).await;
    let [x, y, w, h] = tall.bounds;
    // The chat's gutter, right of the cards, where a wheel scrolls the chat and no page.
    let gutter = f32::midpoint(x + w, chat.window.width);
    // The middle of what shows of its page.
    let (shown_top, shown_bottom) = ((y + HEADER).max(top), (y + h).min(bottom));
    let at = [x + w / 2.0, f32::midpoint(shown_top, shown_bottom)];
    // Where its card ends: the page's word of its scroll makes the card shorter, and the chat,
    // which stands at its end, keeps that edge where it was (run 47: its top moved 520 pt, the
    // card going 765 -> 245 pt, while its end stayed).
    let (top_before, h_before) = where_is(drv, "Tall").await;
    let before = top_before + h_before;
    let natives = natives_said(drv).await;
    let landed = drv
        .ui_finger_drag([at[0], at[1] + DRAG / 2.0], [at[0], at[1] - DRAG / 2.0], MOVES)
        .await
        .unwrap();
    measure(&format!("pages-touch-landed {device} tall drag from {at:?}: {landed}; {natives}"));
    if !landed.contains("WKWebView") {
        missing.push(format!("a finger over the tall page lands in its web view: {landed}"));
    }
    let scrolled_below = f32::midpoint(TOUCHED, SCROLLED) + HEADER;
    let scrolled = drv
        .wait_for("the tall page's word that it scrolled", Duration::from_secs(5), |d| {
            card(d, "Tall").is_some_and(|n| n.bounds[3] < scrolled_below)
        })
        .await;
    let (top_after, now) = where_is(drv, "Tall").await;
    let after = top_after + now;
    let reached = if now < scrolled_below {
        "it scrolled"
    } else if now < h - 50.0 {
        "touched, not scrolled"
    } else {
        "no word from the page: no touch reached it"
    };
    measure(&format!(
        "pages-touch-tall {device}: a {DRAG} pt drag over the tall page moved the card's end \
         {:.1} pt; its card {h:.0} -> {now:.0} pt ({reached})",
        after - before
    ));
    if scrolled.is_err() {
        missing.push(format!("a drag over a tall page scrolls the page: {reached}"));
    }
    if (after - before).abs() >= 1.0 {
        missing.push(format!("a tall page takes the drag, the chat stays: {before} -> {after}"));
    }

    // A tap on the page is the page's: it grows, and says so. At the chat's top, where the
    // short page is.
    wheel(drv, gutter, true, 30).await;
    let short_card = live_card(drv, "Short", top, bottom).await;
    let [x, y, w, h] = short_card.bounds;
    let natives = natives_said(drv).await;
    let landed = drv.ui_finger_tap(x + w / 2.0, y + HEADER + 30.0).await.unwrap();
    measure(&format!("pages-touch-landed {device} short tap: {landed}; {natives}"));
    if !landed.contains("WKWebView") {
        missing.push(format!("a finger over the short page lands in its web view: {landed}"));
    }
    let grown = drv
        .wait_for("the tapped page grown", Duration::from_secs(5), |d| {
            card(d, "Short").is_some_and(|n| n.bounds[3] > h + 100.0)
        })
        .await;
    let (_, grown_to) = where_is(drv, "Short").await;
    measure(&format!(
        "pages-touch-tap {device}: the tapped page's card {h:.0} -> {grown_to:.0} pt tall"
    ));
    if grown.is_err() {
        missing.push(format!("a tap reaches the page, which grows: {h:.0} -> {grown_to:.0}"));
    }

    // A page with nothing to scroll: the drag scrolls the chat by the whole of it, and the
    // fling after the lift carries it on.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    wheel(drv, gutter, true, 30).await;
    let short_card = live_card(drv, "Short", top, bottom).await;
    let [x, y, w, _] = short_card.bounds;
    let from = [x + w / 2.0, y + HEADER + 20.0];
    let (before, _) = where_is(drv, "Short").await;
    let natives = natives_said(drv).await;
    let landed = drv.ui_finger_drag(from, [from[0], from[1] - DRAG], MOVES).await.unwrap();
    measure(&format!("pages-touch-landed {device} short drag: {landed}; {natives}"));
    let (lifted, _) = where_is(drv, "Short").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (coasted, _) = where_is(drv, "Short").await;
    measure(&format!(
        "pages-touch-short {device}: a {DRAG} pt drag over a page with nothing to scroll moved \
         the chat {:.1} pt by the lift, {:.1} pt more in its fling",
        before - lifted,
        lifted - coasted
    ));
    if before - lifted <= DRAG - 20.0 {
        missing.push(format!("the drag scrolls the chat, all of it: {before} -> {lifted}"));
    }
    if lifted - coasted <= 20.0 {
        missing.push(format!("the chat flings on after the lift: {lifted} -> {coasted}"));
    }
    assert!(missing.is_empty(), "a finger over pages:\n{}", missing.join("\n"));
    stack.shutdown().await;
}
