//! A long sidebar group on a real stack: twelve chats the stand-in Claude Code finished leave the
//! Done group with its first ten rows and "Show N more", which shows the rest when pressed
//! (`docs/decisions/ui.md`, "The sidebar groups by status", Show more). Judged by the
//! accessibility tree.

use slopty_e2e::Dump;
use slopty_e2e::harness::ServedApp;

use crate::served::{STEP, TURN, middle, seen, start, thread_until};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "paging";

/// Chats started: more than a group's first page of ten, whichever of them is open.
const CHATS: usize = 12;

/// The most that wait behind Show more: twelve less the first page, the open chat among them.
const BEHIND: usize = 2;

/// The sidebar's "Show N more" line, and its N.
fn show_more(dump: &Dump) -> Option<(&slopty_e2e::A11yNode, usize)> {
    dump.a11y.iter().find_map(|n| {
        let label = n.label.as_deref()?;
        let count = label.strip_prefix("Show ")?.strip_suffix(" more")?.parse().ok()?;
        (n.role == "Button").then_some((n, count))
    })
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_long_group_shows_more_on_request() {
    let mut stack = ServedApp::launch(MACHINE).await.unwrap();
    let repo = std::fs::canonicalize(stack.path("repo")).unwrap();
    for n in 0..CHATS {
        let agent = start(&stack, MACHINE, &repo, &format!("Say hello {n}")).await;
        thread_until(&stack, &agent, "answered", |read| read.to_string().contains("Done:")).await;
    }
    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 1100.0 }).await.unwrap();

    let paged =
        seen(drv, "Done's first page and Show more", TURN, |d| show_more(d).is_some()).await;
    let (line, hidden) = show_more(&paged).unwrap();
    assert!((1..=BEHIND).contains(&hidden), "the rows past ten at rest wait: {hidden}");
    let (x, y) = middle(line);
    drv.click(x, y).await.unwrap();
    seen(drv, "every row, and no Show more", STEP, |d| show_more(d).is_none()).await;
}

// A sidebar watching many agents at once, on a real stack: every state a row can stand in, long
// and repeated titles, two projects. Judged by the accessibility tree, and drawn in light and
// dark against `golden/sidebar-states-*` (`docs/decisions/ui.md`, "The sidebar says each row's
// state once").

/// The worker's name for the states' run.
const STATES: &str = "states";

/// A title longer than the sidebar is wide.
const LONG: &str = "Refactor the pairing handshake so a phone that scans an expired code learns why instead of waiting";

/// What the stand-in TUI is asked, and in which project: each prompt's first word picks its
/// turn (`slopty_testkit::claude_tui`). Started oldest first.
const AT_REST: [(&str, &str); 4] = [
    ("slopty", "Tidy the README's install section"),
    ("better-update", "hello"),
    ("slopty", "hello"),
    ("slopty", LONG),
];
const LIVE: [&str; 3] = [
    // Turns that go on until interrupted: at work.
    "hold the settings split into pages",
    "hold a survey of the crates for dead code",
    // A question: it waits on the person.
    "ask which accent reads best on glass",
];

/// The masks over every sidebar row's time, which says the run's own moment ("now", "1m"):
/// the right end of each row's first line, at the frame's `scale`.
fn row_times(
    dump_a11y: &[slopty_e2e::A11yNode],
    scale: f32,
) -> Vec<slopty_e2e::snapshot::PixelRect> {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0).round() as u32;
    dump_a11y
        .iter()
        .filter(|n| {
            n.role == "Button" && n.bounds[0] < 40.0 && n.bounds[3] >= 28.0 && n.bounds[3] <= 48.0
        })
        .map(|n| {
            let [x, y, w, _] = n.bounds;
            let left = x + w - 52.0;
            [px(left), px(y), px(52.0), px(30.0)]
        })
        .collect()
}

/// Every state at once: Needs you holds the session whose question waits on the person, saying
/// why; Working holds the two at work; Done holds the rest, the two "hello" rows told apart by
/// their projects; the open session is read.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn every_state_reads_at_a_glance() {
    let mut stack = ServedApp::launch(STATES).await.unwrap();
    let project = |name: &str| {
        let path = stack.path(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    };
    let (slopty, other) = (project("slopty"), project("better-update"));
    let folder = |name: &str| if name == "slopty" { slopty.clone() } else { other.clone() };
    for (name, prompt) in AT_REST {
        let agent = start(&stack, STATES, &folder(name), prompt).await;
        thread_until(&stack, &agent, "answered", |read| read.to_string().contains("Done:")).await;
    }
    for prompt in LIVE {
        start(&stack, STATES, &slopty, prompt).await;
    }
    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 880.0 }).await.unwrap();
    let header = |d: &Dump, words: &str| {
        d.a11y.iter().any(|n| n.role == "Button" && n.label.as_deref() == Some(words))
    };
    let grouped = seen(drv, "every group", TURN, |d| {
        header(d, "Needs you, 1") && header(d, "Working, 2") && header(d, "Done, 4")
    })
    .await;
    // The read chat: open it.
    let read = grouped
        .a11y
        .iter()
        .find(|n| n.role == "Button" && n.label.as_deref() == Some(AT_REST[0].1))
        .expect("the README's row");
    let (x, y) = middle(read);
    drv.click(x, y).await.unwrap();
    let dump = seen(drv, "the open chat read", STEP, |d| {
        !crate::served::unread(d, AT_REST[0].1) && d.a11y_node("Button", Some("Open in…")).is_some()
    })
    .await;
    let text = slopty_e2e::frame_text(&dump.a11y);
    for words in ["hello, slopty", "hello, better-update"] {
        assert!(header(&dump, words), "the two hello rows told apart ({words}):\n{text}");
    }
    let said = |role: &str, words: &str| {
        dump.a11y
            .iter()
            .any(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.starts_with(words)))
    };
    for status in ["Needs you", "Working"] {
        assert!(said("Status", status), "a row says {status}:\n{text}");
    }
    assert!(said("Label", "Has a question"), "the asking row says why:\n{text}");
    assert!(crate::served::unread(&dump, "hello, slopty"), "an answer nobody read:\n{text}");
    // The pointer off the sidebar, so no row is lit and no hover card floats.
    drv.ok(&slopty_e2e::Command::Move { x: 800.0, y: 400.0 }).await.unwrap();
    seen(drv, "no row under the pointer", STEP, |d| {
        d.a11y_node("Button", Some("Archive")).is_none()
    })
    .await;
    // Light and dark, the rows' times and the open turn's end left out.
    let renders = slopty_e2e::harness::artifacts_dir();
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&slopty_e2e::Command::Appearance { dark }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let golden = format!("sidebar-states-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
        let mut masks = crate::served::always_moving(&frame.a11y, frame.scale);
        masks.extend(row_times(&frame.a11y, frame.scale));
        slopty_e2e::snapshot::assert_matches_masked(
            &golden,
            &frame,
            slopty_e2e::snapshot::MAC_TOLERANCE,
            &renders,
            &masks,
        )
        .unwrap();
    }
    drv.ok(&slopty_e2e::Command::Appearance { dark: false }).await.unwrap();
    // A row's menu: a press of the secondary button opens it with the row's actions; Escape puts
    // it away.
    let row = |d: &Dump| d.a11y_node("Button", Some("hello, better-update")).map(middle);
    let (x, y) = row(&dump).expect("the other hello");
    drv.ok(&slopty_e2e::Command::Click { x, y, button: slopty_e2e::Button::Right, count: 1 })
        .await
        .unwrap();
    let menu =
        seen(drv, "the row's menu", STEP, |d| d.a11y_node("MenuItem", Some("Rename")).is_some())
            .await;
    for item in ["Rename", "Archive"] {
        assert!(menu.a11y_node("MenuItem", Some(item)).is_some(), "the menu offers {item}");
    }
    assert!(menu.a11y_node("MenuItem", Some("Pin")).is_none(), "no Pin: the list keeps its order");
    drv.keys("escape").await.unwrap();
    seen(drv, "the menu put away", STEP, |d| d.a11y_node("MenuItem", Some("Rename")).is_none())
        .await;
    // Under the pointer the row offers Archive in its time's place; pressed, the row leaves Done
    // for the Archived shelf.
    drv.ok(&slopty_e2e::Command::Move { x, y }).await.unwrap();
    let hovered =
        seen(drv, "the row's archive", STEP, |d| d.a11y_node("Button", Some("Archive")).is_some())
            .await;
    let (ax, ay) = middle(hovered.a11y_node("Button", Some("Archive")).unwrap());
    drv.click(ax, ay).await.unwrap();
    seen(drv, "the row archived", STEP, |d| header(d, "Done, 3") && row(d).is_none()).await;
    stack.shutdown().await;
}
