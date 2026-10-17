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

/// The card of `title` once it is live and wholly between `top` and `bottom` (window points).
async fn live_card(drv: &mut Driver, title: &str, top: f32, bottom: f32) -> A11yNode {
    let since = std::time::Instant::now();
    loop {
        let dump = drv.dump().await.unwrap();
        if let Some(found) = card(&dump, title) {
            let [_, y, _, h] = found.bounds;
            let live = found.value.as_deref() == Some("live");
            if live && y >= top && y + h <= bottom {
                return found.clone();
            }
            assert!(
                since.elapsed() < STEP,
                "the {title} card never stood live in sight ({top}..{bottom}): {found:?}\n{}",
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

/// Where the card of `title` is now, its top and height.
async fn where_is(drv: &mut Driver, title: &str) -> (f32, f32) {
    let dump = drv.dump().await.unwrap();
    let found = card(&dump, title).unwrap_or_else(|| panic!("no {title} card"));
    (found.bounds[1], found.bounds[3])
}

/// The chat wheeled toward its top (`up`) or its end, beside the column where no card is.
async fn wheel(drv: &mut Driver, up: bool, times: usize) {
    let dy = if up { 400.0 } else { -400.0 };
    for _ in 0..times {
        drv.scroll(6.0, 300.0, 0.0, dy).await.unwrap();
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
    let tall = page(
        3000,
        "<div style=\"height:3000px;background:linear-gradient(#fdc,#9be)\">Tall page</div>",
        "",
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

    // A tall page takes a drag it can scroll, and the chat stays. The chat opens at its end,
    // where the tall page is.
    let tall = live_card(drv, "Tall", top, bottom + 400.0).await;
    let [x, y, w, h] = tall.bounds;
    // The middle of what shows of its page.
    let (shown_top, shown_bottom) = ((y + HEADER).max(top), (y + h).min(bottom));
    let at = [x + w / 2.0, f32::midpoint(shown_top, shown_bottom)];
    let (before, _) = where_is(drv, "Tall").await;
    drv.ui_finger_drag([at[0], at[1] + DRAG / 2.0], [at[0], at[1] - DRAG / 2.0], MOVES)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (after, _) = where_is(drv, "Tall").await;
    measure(&format!(
        "pages-touch-tall {device}: a {DRAG} pt drag over the tall page moved the chat \
         {:.1} pt",
        after - before
    ));
    assert!(
        (after - before).abs() < 1.0,
        "a tall page takes the drag, the chat stays: {before} -> {after}"
    );

    // A page with nothing to scroll: the drag scrolls the chat by the whole of it, and the
    // fling after the lift carries it on. At the chat's top, where the short page is.
    wheel(drv, true, 30).await;
    let short_card = live_card(drv, "Short", top, bottom).await;
    let [x, y, w, _] = short_card.bounds;
    let from = [x + w / 2.0, y + HEADER + 20.0];
    let (before, _) = where_is(drv, "Short").await;
    drv.ui_finger_drag(from, [from[0], from[1] - DRAG], MOVES).await.unwrap();
    let (lifted, _) = where_is(drv, "Short").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (coasted, _) = where_is(drv, "Short").await;
    measure(&format!(
        "pages-touch-short {device}: a {DRAG} pt drag over a page with nothing to scroll moved \
         the chat {:.1} pt by the lift, {:.1} pt more in its fling",
        before - lifted,
        lifted - coasted
    ));
    assert!(
        before - lifted > DRAG - 20.0,
        "the drag scrolls the chat, all of it: {before} -> {lifted}"
    );
    assert!(lifted - coasted > 20.0, "the chat flings on after the lift: {lifted} -> {coasted}");

    // A tap on the page is the page's: it grows, and says so.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    wheel(drv, true, 30).await;
    let short_card = live_card(drv, "Short", top, bottom).await;
    let [x, y, w, h] = short_card.bounds;
    drv.ui_finger_tap(x + w / 2.0, y + HEADER + 30.0).await.unwrap();
    let grown = drv
        .wait_for("the tapped page grown", STEP, |d| {
            card(d, "Short").is_some_and(|n| n.bounds[3] > h + 100.0)
        })
        .await;
    let (_, tall_now) = where_is(drv, "Short").await;
    measure(&format!(
        "pages-touch-tap {device}: the tapped page's card {h:.0} -> {tall_now:.0} pt tall"
    ));
    if let Err(e) = grown {
        panic!("a tap reaches the page, which grows: {e:#}");
    }
    stack.shutdown().await;
}
