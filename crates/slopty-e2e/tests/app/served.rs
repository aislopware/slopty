//! What the acceptance cases of an agent's surfaces share: a stack whose stand-in Claude Code
//! (`slopty-stub-claude`) works in a folder of the run's own, the thread started through the CLI
//! as any agent's is, and the app driven and read through its accessibility tree.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use slopty_e2e::harness::{ServedApp, artifacts_dir};
use slopty_e2e::snapshot::{MAC_TOLERANCE, PixelRect, assert_matches_masked};
use slopty_e2e::{A11yNode, Command, Driver, Dump};

/// How long a worker round trip may take.
pub const STEP: Duration = Duration::from_secs(30);
/// How long a stand-in's turn may take, start to end.
pub const TURN: Duration = Duration::from_secs(90);
/// The shell's main window as it first opens.
pub const WINDOW: (f32, f32) = (1320.0, 880.0);
/// Time for what the accessibility tree cannot show (a fold's tween, a row's reveal) to settle.
pub const SETTLE: Duration = Duration::from_secs(2);

/// The node `role` named exactly `label`.
pub fn node<'a>(dump: &'a Dump, role: &str, label: &str) -> Option<&'a A11yNode> {
    dump.a11y.iter().find(|n| n.role == role && n.label.as_deref() == Some(label))
}

/// The first node `role` whose label holds `words`.
pub fn holding<'a>(dump: &'a Dump, role: &str, words: &str) -> Option<&'a A11yNode> {
    dump.a11y
        .iter()
        .find(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.contains(words)))
}

/// Every node `role` whose label holds `words`.
pub fn all_holding<'a>(dump: &'a Dump, role: &str, words: &str) -> Vec<&'a A11yNode> {
    dump.a11y
        .iter()
        .filter(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.contains(words)))
        .collect()
}

/// The middle of `node`, in window points.
pub fn middle(node: &A11yNode) -> (f32, f32) {
    let [x, y, w, h] = node.bounds;
    (w.mul_add(0.5, x), h.mul_add(0.5, y))
}

/// Dump until `ready` holds, within `within`; a frame that never says it fails with what it said
/// last, as its words.
pub async fn seen(
    drv: &mut Driver,
    what: &str,
    within: Duration,
    mut ready: impl FnMut(&Dump) -> bool,
) -> Dump {
    let since = std::time::Instant::now();
    loop {
        let dump = drv.dump().await.unwrap();
        if ready(&dump) {
            return dump;
        }
        assert!(
            since.elapsed() < within,
            "the app never showed {what}; it said:\n{}",
            slopty_e2e::frame_text(&dump.a11y)
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// The nodes from the agent's first reply on (the `Claude` article and what follows it): the
/// person's own message, the `You` article above, holds the words they typed.
pub fn replies(dump: &Dump) -> &[A11yNode] {
    let from = dump
        .a11y
        .iter()
        .position(|n| n.role == "Article" && n.label.as_deref() == Some("Claude"))
        .unwrap_or(dump.a11y.len());
    dump.a11y.get(from..).unwrap_or_default()
}

/// Click the middle of the node `role` named `label` once it is drawn; the frame it was found in.
pub async fn click(drv: &mut Driver, role: &str, label: &str) -> Dump {
    let what = format!("the {role} {label:?}");
    let dump = seen(drv, &what, STEP, |d| node(d, role, label).is_some()).await;
    let (x, y) = node(&dump, role, label).map(middle).unwrap_or_default();
    drv.click(x, y).await.unwrap();
    dump
}

/// Point at the middle of `node`, as a hover would.
pub async fn hover(drv: &mut Driver, node: &A11yNode) {
    let (x, y) = middle(node);
    drv.ok(&Command::Move { x, y }).await.unwrap();
}

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>.png`;
/// the window is light again after. A node whose label holds one of `moving` says a moment of the
/// run (a time since, a turn's end in the person's time), so its pixels and its words are left out.
/// The frame's words, as the light render said them.
pub async fn goldens(drv: &mut Driver, renders: &Path, name: &str, moving: &[&str]) -> String {
    goldens_with(drv, renders, name, moving, &[]).await
}

/// [`goldens`], with the whole line of the transcript column each node named by one of `rows`
/// sits on left out too: a message's hover line, whose stamp ("Oct 16, 11:03 PM") is no node of
/// its own, beside the node that is (its Copy message).
pub async fn goldens_with(
    drv: &mut Driver,
    renders: &Path,
    name: &str,
    moving: &[&str],
    rows: &[&str],
) -> String {
    let mut words = String::new();
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        // The appearance's first frame, and its hover and fade tweens settled.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let golden = format!("{name}-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        let text = slopty_e2e::frame_text(&frame.a11y);
        println!("A11Y {golden}\n{text}");
        if !dark {
            words = text;
        }
        let labelled = |n: &&A11yNode, of: &[&str]| {
            n.label.as_deref().is_some_and(|l| of.iter().any(|m| l.contains(m)))
        };
        let moving_nodes =
            frame.a11y.iter().filter(|n| labelled(n, &ALWAYS_MOVING) || labelled(n, moving));
        let row_nodes = frame.a11y.iter().filter(|n| labelled(n, rows)).map(|n| {
            let [x, y, w, h] = n.bounds;
            let left = TRANSCRIPT_LEFT.min(x);
            A11yNode { bounds: [left, y, x + w - left, h], ..n.clone() }
        });
        let masks: Vec<PixelRect> =
            moving_nodes.cloned().chain(row_nodes).map(|n| mask_of(&n, frame.scale)).collect();
        assert_matches_masked(&golden, &frame, MAC_TOLERANCE, &artifacts_dir(), &masks).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
    words
}

/// Where the transcript's column starts at the window's size, in points: its messages' left
/// edge with the sidebar shown.
const TRANSCRIPT_LEFT: f32 = 400.0;

/// What moves in every frame of a thread: a finished turn's done line names the time it ended in
/// the person's clock ("Worked for 2s · done 3:42 PM"), a fact of the run, not of the clock the
/// test pins.
const ALWAYS_MOVING: [&str; 1] = ["Worked for "];

/// The pixel rectangle of `node` at the window's `scale`, a few points wider each way.
fn mask_of(node: &A11yNode, scale: f32) -> PixelRect {
    let [x, y, w, h] = node.bounds;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0).round() as u32;
    let pad = 4.0;
    [px(x - pad), px(y - pad), px(2.0_f32.mul_add(pad, w)), px(2.0_f32.mul_add(pad, h))]
}

/// Start the stand-in on `machine` in `folder` with `prompt`, as one word of the CLI's; the
/// agent's id.
pub async fn start(stack: &ServedApp, machine: &str, folder: &Path, prompt: &str) -> String {
    let folder = folder.to_string_lossy().into_owned();
    let started =
        stack.slopty(&["agent", "start", machine, "--folder", &folder, prompt]).await.unwrap();
    let agent = started["agent"].as_str().unwrap_or_default().to_owned();
    assert!(!agent.is_empty(), "an agent started: {started}");
    agent
}

/// Send the first thread `words` from its composer, as the person does: the CLI's `agent tell`
/// reaches an agent as a report from another, which Claude Code is not asked to act on alone.
pub async fn tell(stack: &mut ServedApp, words: &str) {
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    type_in_composer(drv, words).await;
    drv.keys("enter").await.unwrap();
}

/// Read the agent's thread (`slopty agent read`) until `ready` holds; the read that held.
pub async fn thread_until(
    stack: &ServedApp,
    agent: &str,
    what: &str,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    let since = std::time::Instant::now();
    loop {
        let read = stack.slopty(&["agent", "read", "--thread", agent]).await;
        if let Ok(read) = &read
            && ready(read)
        {
            return read.clone();
        }
        assert!(since.elapsed() < TURN, "the agent's thread {what}: {read:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Whether the thread's words, as read, hold `words`.
pub fn says(read: &Value, words: &str) -> bool {
    read.to_string().contains(words)
}

/// Whether the thread has a request waiting on the person.
pub fn asking(read: &Value) -> bool {
    read["requests"].as_array().is_some_and(|r| !r.is_empty())
}

/// Open the `nth` thread of the sidebar (⌘1 is the first started) at the window's size, once its
/// composer is drawn.
pub async fn open_thread(drv: &mut Driver, nth: u8) -> Dump {
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys(&format!("cmd-{nth}")).await.unwrap();
    seen(drv, "the thread's composer", STEP, |d| d.a11y_node("Button", Some("Open in…")).is_some())
        .await
}

/// Click the composer and type `text` into it.
pub async fn type_in_composer(drv: &mut Driver, text: &str) {
    let dump =
        seen(drv, "the composer", STEP, |d| d.a11y.iter().any(|n| n.role == "MultilineTextInput"))
            .await;
    let composer = dump.a11y.iter().find(|n| n.role == "MultilineTextInput").unwrap();
    let (x, y) = middle(composer);
    drv.click(x, y).await.unwrap();
    drv.type_text(text).await.unwrap();
}

/// What the stand-in heard, as its record (`STUB_RECORD`) at `path` says it.
pub fn heard(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The control requests of `subtype` the stand-in heard, in order.
pub fn control_requests(record: &Value, subtype: &str) -> Vec<Value> {
    record["received"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| match line {
                    Value::String(text) => serde_json::from_str::<Value>(text).ok(),
                    other => Some(other.clone()),
                })
                .filter(|line| {
                    line["type"] == "control_request" && line["request"]["subtype"] == subtype
                })
                .collect()
        })
        .unwrap_or_default()
}
