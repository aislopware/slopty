//! The settings form in the real app: ⌘, opens it on its sections, and a change made in it lands
//! in `settings.toml` and applies at once, with no Save and the page still up, the rest of the
//! file as it was. The terminal's page is held as a golden. The Keyboard page lists what
//! the running app binds, its own ⌘, among the workspace's keys. The Input page shows a map's
//! entries, the clipboard's machines by name.

use std::fmt::Write as _;

use slopty_e2e::{Command, Driver, Dump, Stack};

use super::gallery::{STEP, first_shell, golden};

/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Where the pointer rests before a golden, over nothing that answers a hover.
const PARK: (f32, f32) = (1.0, 1.0);
/// A worker id no machine has.
const GONE: &str = "0199c0de-0000-7000-8000-000000000001";
/// What its entry says.
const GONE_SAID: &str = "Machine not on the server (0199c0de)";

/// Click the centre of the node with `role` and `label` in the settings: in their page or in
/// their section list, which the navigator holds. A pane's own tab elsewhere may share a
/// section's name ("Terminal"), and the title bar's arrows a way's ("Back").
async fn press(drv: &mut Driver, dump: &Dump, role: &str, label: &str) {
    let areas: Vec<[f32; 4]> = [("Group", "Settings"), ("Group", "Settings sections")]
        .iter()
        .filter_map(|(r, l)| dump.a11y_node(r, Some(l)).map(|n| n.bounds))
        .collect();
    assert!(!areas.is_empty(), "the settings: {:#?}", dump.a11y);
    let inside = |b: [f32; 4]| {
        let [x, y, w, h] = b;
        areas.iter().any(|&[dx, dy, dw, dh]| {
            x >= dx - 0.5 && y >= dy - 0.5 && x + w <= dx + dw + 0.5 && y + h <= dy + dh + 0.5
        })
    };
    let [x, y, w, h] = dump
        .a11y
        .iter()
        .find(|n| n.role == role && n.label.as_deref() == Some(label) && inside(n.bounds))
        .unwrap_or_else(|| panic!("{label} in the settings: {:#?}", dump.a11y))
        .bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_settings_form_edits_the_file() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let settings = dir.join("app").join("settings.toml");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    // Two machines by id, so the Input page shows a map's entries: the run's worker, by its
    // name, and one no machine has, which says so.
    let worker = slopty_e2e::harness::worker_id(&dir).expect("the worker's id");
    let mut before = std::fs::read_to_string(&settings).unwrap();
    write!(before, "\n[clipboard.workers]\n\"{GONE}\" = false\n\"{worker}\" = true\n").unwrap();
    std::fs::write(&settings, &before).unwrap();

    drv.keys("cmd-,").await.unwrap();
    let dump = drv
        .wait_for("the settings form", STEP, |d| {
            d.a11y_node("Group", Some("Settings")).is_some()
                && d.a11y_node("RadioGroup", Some("Theme")).is_some()
        })
        .await
        .unwrap();
    let sections = ["Appearance", "Terminal", "Input", "Agents", "Network", "Keyboard", "About"];
    for section in sections {
        assert!(dump.a11y_node("Tab", Some(section)).is_some(), "{section}: {:#?}", dump.a11y);
    }

    press(drv, &dump, "Tab", "Terminal").await;
    let dump = drv
        .wait_for("the terminal's page", STEP, |d| {
            d.a11y_node("Switch", Some("Ligatures")).is_some()
        })
        .await
        .unwrap();
    assert_eq!(
        dump.a11y_node("ComboBox", Some("Family")).and_then(|n| n.value.as_deref()),
        Some("JetBrains Mono"),
        "{:#?}",
        dump.a11y
    );
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-form").await;

    // The Keyboard page reads the app's keymap: the app's own binding is there beside the
    // workspace's, on the keys that open these settings.
    press(drv, &dump, "Tab", "Keyboard").await;
    let keys = drv
        .wait_for("the keyboard page", STEP, |d| {
            d.a11y_node("ListItem", Some("Open settings")).is_some()
        })
        .await
        .unwrap();
    let bound = |label: &str| keys.a11y_node("ListItem", Some(label)).and_then(|n| n.value.clone());
    assert_eq!(bound("Open settings").as_deref(), Some("⌘,"), "{:#?}", keys.a11y);
    assert_eq!(bound("New terminal").as_deref(), Some("⇧⌘T"), "{:#?}", keys.a11y);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-keyboard").await;
    press(drv, &keys, "Tab", "About").await;
    drv.wait_for("the about page", STEP, |d| d.a11y_node("Group", Some("About")).is_some())
        .await
        .unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-about").await;
    // A map's entries are lines under its row: a machine's name and its switch.
    press(drv, &keys, "Tab", "Input").await;
    let input = drv
        .wait_for("the input page", STEP, |d| d.a11y_node("ListItem", Some(GONE_SAID)).is_some())
        .await
        .unwrap();
    assert!(input.a11y_node("Switch", Some("e2e-worker")).is_some(), "{:#?}", input.a11y);
    // Scrolled until the map's lines are in the page, under its title and over the window's foot.
    let mut lines = input;
    for _ in 0..20 {
        let [_, y, _, h] =
            lines.a11y_node("ListItem", Some("e2e-worker")).expect("the worker").bounds;
        let [_, top, _, _] = lines.a11y_node("ListItem", Some(GONE_SAID)).expect("gone").bounds;
        if top > 200.0 && y + h < 500.0 {
            break;
        }
        drv.ok(&Command::Scroll { x: 520.0, y: 350.0, dx: 0.0, dy: -2.0 }).await.unwrap();
        lines = drv.dump().await.unwrap();
    }
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-input").await;
    // A page of a daemon's keys says whose they are at the end of its head: this Mac's, which
    // runs a worker, until another machine is picked; the worker it is linked to is in the
    // picker's menu.
    press(drv, &keys, "Tab", "Agents").await;
    let picked =
        |d: &Dump| d.a11y_node("ComboBox", Some("Settings of")).and_then(|n| n.value.clone());
    let agents = drv
        .wait_for("the agents page with its picker", STEP, |d| {
            picked(d).as_deref() == Some("This Mac")
        })
        .await
        .unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-agents").await;
    press(drv, &agents, "ComboBox", "Settings of").await;
    drv.wait_for("the machines menu", STEP, |d| {
        d.a11y_node("MenuItemRadio", Some("e2e-worker")).is_some()
    })
    .await
    .unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-machines").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the menu closed", STEP, |d| {
        d.a11y_node("MenuItemRadio", Some("e2e-worker")).is_none()
    })
    .await
    .unwrap();
    press(drv, &keys, "Tab", "Terminal").await;
    let dump = drv
        .wait_for("back on the terminal's page", STEP, |d| {
            d.a11y_node("Switch", Some("Ligatures")).is_some()
        })
        .await
        .unwrap();

    // A switch writes the file as it turns, and the page stays up.
    let file = || std::fs::read_to_string(&settings).unwrap_or_default();
    press(drv, &dump, "Switch", "Ligatures").await;
    drv.wait_for("ligatures off in the file, the page up", STEP, |d| {
        file().contains("ligatures = false") && d.a11y_node("Group", Some("Settings")).is_some()
    })
    .await
    .unwrap();

    // A choice writes it too, and the app applies it: the theme turns dark around the page.
    let dump = drv.dump().await.unwrap();
    assert!(!dump.dark, "the stack starts light");
    press(drv, &dump, "Tab", "Appearance").await;
    let dump = drv
        .wait_for("the appearance page", STEP, |d| {
            d.a11y_node("RadioButton", Some("Dark")).is_some()
        })
        .await
        .unwrap();
    press(drv, &dump, "RadioButton", "Dark").await;
    drv.wait_for("the theme dark, the page up", STEP, |d| {
        d.dark
            && file().contains("appearance = \"dark\"")
            && d.a11y_node("Group", Some("Settings")).is_some()
    })
    .await
    .unwrap();

    let dump = drv.dump().await.unwrap();
    press(drv, &dump, "Button", "Back").await;
    drv.wait_for("the settings closed", STEP, |d| d.a11y_node("Group", Some("Settings")).is_none())
        .await
        .unwrap();
    let after = file();
    let kept = before
        .lines()
        .filter(|line| !line.starts_with("appearance"))
        .all(|line| after.lines().any(|l| l == line));
    assert!(kept, "every other line of the file stays:\n{before}\n---\n{after}");
    stack.shutdown().await;
}
