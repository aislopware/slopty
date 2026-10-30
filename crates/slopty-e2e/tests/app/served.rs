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

/// Whether the sidebar's row of the chat titled `title` says its last turn is unread: its
/// button followed by a "Done" status. The app counts its window as looked at whether or not it
/// is in front (`slopty_shell::presence`), so the chat in front is marked seen as it opens and as
/// its turns end; a test that renders an opened chat waits for the mark, which goes to the worker
/// and comes back in the chat's row.
pub fn unread(dump: &Dump, title: &str) -> bool {
    dump.a11y.windows(2).any(|pair| {
        let [row, status] = pair else { return false };
        row.role == "Button"
            && row.label.as_deref() == Some(title)
            && status.role == "Status"
            && status.label.as_deref() == Some("Done")
    })
}

/// How tall the titlebar row stands at the window's top (`Theme::TITLEBAR_HEIGHT`).
pub const TITLEBAR: f32 = 38.0;

/// The titlebar's identity: the button in the row at the window's top named as the session
/// shown is, by its row in the sidebar's list or, with the sidebar hidden, by its pane (the group
/// under the titlebar that holds its terminal), which the row names while that session is shown.
pub fn identity(dump: &Dump) -> Option<&A11yNode> {
    let named = |n: &A11yNode| n.label.is_some() && n.bounds[1] >= TITLEBAR;
    let holds = |outer: &A11yNode, inner: &A11yNode| {
        let ([left, top, width, height], [x, y, w, h]) = (outer.bounds, inner.bounds);
        x >= left && y >= top && x + w <= left + width && y + h <= top + height
    };
    let session_pane = |n: &A11yNode| {
        n.role == "Group" && dump.a11y.iter().any(|t| t.role == "Terminal" && holds(n, t))
    };
    dump.a11y
        .iter()
        .filter(|n| n.role == "Button" && n.label.is_some() && n.bounds[1] < TITLEBAR)
        .find(|top| {
            dump.a11y.iter().any(|row| {
                named(row) && row.label == top.label && (row.role == "Button" || session_pane(row))
            })
        })
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

/// The nodes from the agent's first reply on (the `Claude Code` article and what follows it) up
/// to the titlebar's row, which reads after the window's body: the person's own message, the
/// `You` article above, holds the words they typed, and so does the titlebar's identity, the
/// session named by its first prompt.
pub fn replies(dump: &Dump) -> &[A11yNode] {
    let nodes = dump.a11y.as_slice();
    let from = nodes
        .iter()
        .position(|n| n.role == "Article" && n.label.as_deref() == Some("Claude Code"))
        .unwrap_or(nodes.len());
    let to = nodes
        .iter()
        .skip(from)
        .position(|n| n.bounds[1] < TITLEBAR)
        .map_or(nodes.len(), |past| from.saturating_add(past));
    nodes.get(from..to).unwrap_or_default()
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
/// the window is light again after. A node whose label holds one of `moving` says something the
/// run decides, so its pixels and its words are left out.
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
        let moving_nodes = frame.a11y.iter().filter(|n| labelled(n, moving));
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

/// The pixel rectangle of `node` at the window's `scale`, a few points wider each way.
pub fn mask_of(node: &A11yNode, scale: f32) -> PixelRect {
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

/// Type `words` into the first session's TUI and send them, as the person does in its
/// terminal: the CLI's `agent tell` reaches an agent as a report from another, which Claude Code
/// is not asked to act on alone.
pub async fn tell(stack: &mut ServedApp, words: &str) {
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    type_in_terminal(drv, words).await;
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

/// Open the `nth` session of the sidebar (⌘1 is the first started) at the window's size, once its
/// titlebar is drawn.
pub async fn open_thread(drv: &mut Driver, nth: u8) -> Dump {
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys(&format!("cmd-{nth}")).await.unwrap();
    seen(drv, "the session's titlebar", STEP, |d| identity(d).is_some()).await
}

/// Summon the shown session's inspector as the person does, with ⌘I, once its terminal is drawn:
/// a session opens on its terminal alone (`docs/decisions/ui.md`, "A session opens on its
/// terminal alone"). The frame that shows it beside the session.
pub async fn show_inspector(drv: &mut Driver) -> Dump {
    let session =
        seen(drv, "the session's terminal", STEP, |d| d.a11y.iter().any(|n| n.role == "Terminal"))
            .await;
    if node(&session, "Group", "Inspector").is_none() {
        drv.keys("cmd-i").await.unwrap();
    }
    seen(drv, "the inspector beside the session", STEP, |d| node(d, "Group", "Inspector").is_some())
        .await
}

/// Type `text` into the open session's terminal, its TUI's prompt: the keyboard is the
/// terminal's once the session opens, and a click on it gives it back from elsewhere.
pub async fn type_in_terminal(drv: &mut Driver, text: &str) {
    let dump =
        seen(drv, "the session's terminal", STEP, |d| d.a11y.iter().any(|n| n.role == "Terminal"))
            .await;
    if !dump.focused.starts_with("terminal:") {
        let terminal = dump.a11y.iter().find(|n| n.role == "Terminal").unwrap();
        let (x, y) = middle(terminal);
        drv.click(x, y).await.unwrap();
        seen(drv, "the terminal holding the keyboard", STEP, |d| {
            d.focused.starts_with("terminal:")
        })
        .await;
    }
    drv.type_text(text).await.unwrap();
}

/// What the stand-in heard, as its record (`STUB_RECORD`) at `path` says it.
pub fn heard(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// Claude Code's name for a folder's project directory: every character but a letter or a digit
/// made `-`.
pub fn escaped(dir: &Path) -> String {
    dir.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `at` (ms since the epoch) as Claude Code's transcripts write a moment: RFC 3339 in UTC, to the
/// millisecond (the days-to-date step is Howard Hinnant's `civil_from_days`).
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the days-to-civil formula cannot overflow an i64 for any moment since the epoch"
)]
pub fn rfc3339(at: u64) -> String {
    let (secs, ms) = (i64::try_from(at / 1000).unwrap(), at % 1000);
    let (days, of_day) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = (of_day / 3600, of_day % 3600 / 60, of_day % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// The moment an RFC 3339 time in UTC to the millisecond names, as Claude Code's transcripts
/// write one ("2026-10-19T01:46:38.286Z"), in ms since the epoch (Howard Hinnant's
/// `days_from_civil`).
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the civil-to-days formula cannot overflow an i64 for any year a transcript names"
)]
pub fn epoch_ms(at: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| at.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let ms = if at.as_bytes().get(19) == Some(&b'.') { number(20..23)? } else { 0 };
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000) + ms).ok()
}
