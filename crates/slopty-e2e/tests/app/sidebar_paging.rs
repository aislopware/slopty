//! The session list on a real stack (`docs/decisions/layout.md`, "The session list is stable,
//! and its marks carry the attention"): many agents at once in two projects, each row one line
//! whose mark says its state. Judged by the accessibility tree, and drawn in light and dark
//! against `golden/sidebar-states-*`. Inactive's paging, which needs sessions untouched for days,
//! is `shell::sidebar_status::tests::inactive_shows_more_on_request`.

use slopty_e2e::Dump;
use slopty_e2e::harness::ServedApp;

use crate::served::{STEP, TURN, middle, seen, start, thread_until};

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

/// Every state at once, each row where its project keeps it: the session whose question waits on
/// the person, its glyph a `?`; the two at work; the rest done, the two "hello" rows each under
/// its own project; the open session read. Archived, a row leaves its project and the project,
/// left with none, goes.
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
    let grouped = seen(drv, "both projects", TURN, |d| {
        header(d, "better-update, 1") && header(d, "slopty, 6")
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
    // The projects stand by name, each "hello" under its own.
    let top = |role: &str, words: &str| -> Vec<f32> {
        dump.a11y
            .iter()
            .filter(|n| n.role == role && n.label.as_deref() == Some(words))
            .map(|n| n.bounds[1])
            .collect()
    };
    let (other, ours, hellos) =
        (top("Button", "better-update, 1"), top("Button", "slopty, 6"), top("Button", "hello"));
    assert_eq!(hellos.len(), 2, "two hello rows:\n{text}");
    assert!(
        other[0] < hellos[0] && hellos[0] < ours[0] && ours[0] < hellos[1],
        "each under its project:\n{text}"
    );
    let said = |role: &str, words: &str| {
        dump.a11y
            .iter()
            .any(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.starts_with(words)))
    };
    for status in ["Needs you", "Working"] {
        assert!(said("Status", status), "a row says {status}:\n{text}");
    }
    assert!(crate::served::unread(&dump, "hello"), "an answer nobody read:\n{text}");
    // The pointer off the sidebar, so no row is lit and no hover card floats.
    drv.ok(&slopty_e2e::Command::Move { x: 800.0, y: 400.0 }).await.unwrap();
    seen(drv, "no row under the pointer", STEP, |d| {
        d.a11y_node("Button", Some("Archive")).is_none()
    })
    .await;
    // Light and dark.
    let renders = slopty_e2e::harness::artifacts_dir();
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&slopty_e2e::Command::Appearance { dark }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let golden = format!("sidebar-states-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
        slopty_e2e::snapshot::assert_matches_masked(
            &golden,
            &frame,
            slopty_e2e::snapshot::MAC_TOLERANCE,
            &renders,
            &[],
        )
        .unwrap();
    }
    drv.ok(&slopty_e2e::Command::Appearance { dark: false }).await.unwrap();
    // A row's menu: a press of the secondary button opens it with the row's actions; Escape puts
    // it away.
    // The other project's "hello", the first of the two.
    let row = |d: &Dump| d.a11y_node("Button", Some("hello")).map(middle);
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
    // Under the pointer the row offers Archive before its mark; pressed, the row leaves its
    // project for the Archived shelf, and the project, holding none now, goes.
    drv.ok(&slopty_e2e::Command::Move { x, y }).await.unwrap();
    let hovered =
        seen(drv, "the row's archive", STEP, |d| d.a11y_node("Button", Some("Archive")).is_some())
            .await;
    let (ax, ay) = middle(hovered.a11y_node("Button", Some("Archive")).unwrap());
    drv.click(ax, ay).await.unwrap();
    seen(drv, "the row archived", STEP, |d| {
        !header(d, "better-update, 1") && header(d, "Archived, 1")
    })
    .await;
    stack.shutdown().await;
}
