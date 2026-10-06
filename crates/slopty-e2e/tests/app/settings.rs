//! The settings form in the real app: ⌘, opens it on its sections, and a change made in it lands
//! in `settings.toml` and applies at once, with no Save and the dialog still open, the rest of
//! the file as it was. The terminal's page is held as a golden. The Keyboard page lists what
//! the running app binds, its own ⌘, among the workspace's keys. The Input page shows a map's
//! entries, the clipboard's machines by name.

use slopty_e2e::{Command, Driver, Dump, Stack};

use super::gallery::{STEP, first_shell, golden};

/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Where the pointer rests before a golden, over nothing that answers a hover.
const PARK: (f32, f32) = (1.0, 1.0);

/// Click the centre of the node with `role` and `label`.
async fn press(drv: &mut Driver, dump: &Dump, role: &str, label: &str) {
    let [x, y, w, h] =
        dump.a11y_node(role, Some(label)).unwrap_or_else(|| panic!("{label}")).bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_settings_form_edits_the_file() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let settings = dir.join("app").join("settings.toml");
    // Two machines by name, so the Input page shows a map's entries.
    let mut before = std::fs::read_to_string(&settings).unwrap();
    before.push_str("\n[clipboard.workers]\nlaptop = false\nstudio = true\n");
    std::fs::write(&settings, &before).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    drv.keys("cmd-,").await.unwrap();
    let dump = drv
        .wait_for("the settings form", STEP, |d| {
            d.a11y_node("Dialog", Some("Settings")).is_some()
                && d.a11y_node("RadioGroup", Some("Theme")).is_some()
        })
        .await
        .unwrap();
    let sections = ["Appearance", "Terminal", "Input", "Streams", "Network", "Keyboard", "About"];
    for section in sections {
        assert!(dump.a11y_node("Tab", Some(section)).is_some(), "{section}: {:#?}", dump.a11y);
    }
    assert!(dump.a11y_node("Button", Some("Edit as TOML")).is_some(), "{:#?}", dump.a11y);

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
    // workspace's, on the keys that open this dialog.
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
        .wait_for("the input page", STEP, |d| d.a11y_node("ListItem", Some("laptop")).is_some())
        .await
        .unwrap();
    assert!(input.a11y_node("Switch", Some("studio")).is_some(), "{:#?}", input.a11y);
    // Scrolled until the map's lines are in the page, under the dialog's title and over its foot.
    let mut lines = input;
    for _ in 0..20 {
        let [_, y, _, h] = lines.a11y_node("ListItem", Some("studio")).expect("studio").bounds;
        let [_, top, _, _] = lines.a11y_node("ListItem", Some("laptop")).expect("laptop").bounds;
        if top > 200.0 && y + h < 500.0 {
            break;
        }
        drv.ok(&Command::Scroll { x: 520.0, y: 350.0, dx: 0.0, dy: -2.0 }).await.unwrap();
        lines = drv.dump().await.unwrap();
    }
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "settings-input").await;
    press(drv, &keys, "Tab", "Terminal").await;
    let dump = drv
        .wait_for("back on the terminal's page", STEP, |d| {
            d.a11y_node("Switch", Some("Ligatures")).is_some()
        })
        .await
        .unwrap();

    // A switch writes the file as it turns, and the dialog stays open.
    let file = || std::fs::read_to_string(&settings).unwrap_or_default();
    press(drv, &dump, "Switch", "Ligatures").await;
    drv.wait_for("ligatures off in the file, the dialog open", STEP, |d| {
        file().contains("ligatures = false") && d.a11y_node("Dialog", Some("Settings")).is_some()
    })
    .await
    .unwrap();

    // A choice writes it too, and the app applies it: the theme turns dark under the dialog.
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
    drv.wait_for("the theme dark, the dialog open", STEP, |d| {
        d.dark
            && file().contains("appearance = \"dark\"")
            && d.a11y_node("Dialog", Some("Settings")).is_some()
    })
    .await
    .unwrap();

    let dump = drv.dump().await.unwrap();
    press(drv, &dump, "Button", "Done").await;
    drv.wait_for("the settings closed", STEP, |d| {
        d.a11y_node("Dialog", Some("Settings")).is_none()
    })
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
