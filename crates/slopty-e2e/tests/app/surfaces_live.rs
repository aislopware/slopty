//! The shell's live surfaces on a real stack: a shell of the stack's worker split beside the
//! new-session canvas (⌘D), drawn by Slopty's own terminal, and a window of the worker's drawn
//! screen as a view beside a session, chosen from the machine's list, then filling the view.
//! Each state is a golden in light and in dark.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{
    MAC_TOLERANCE as TOLERANCE, PixelRect, assert_matches_apart, assert_matches_masked,
};
use slopty_e2e::{A11yNode, Button, Command, Driver, Dump, Stack};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
/// The shell's main window as it first opens.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>.png`;
/// the window is light again after.
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) {
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        drv.wait_for("the carets at their prompts", STEP, Dump::prompts_settled).await.unwrap();
        let golden = format!("{name}-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
        assert_matches_masked(&golden, &frame, TOLERANCE, &artifacts_dir(), &[]).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
}

/// The node `role` named `label` in `dump`'s accessibility tree.
fn node<'a>(dump: &'a Dump, role: &str, label: &str) -> Option<&'a A11yNode> {
    dump.a11y.iter().find(|n| n.role == role && n.label.as_deref() == Some(label))
}

/// The foot's "N new lines" pill.
fn new_lines(dump: &Dump) -> Option<&A11yNode> {
    dump.a11y
        .iter()
        .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains("new line")))
}

/// A terminal shows a line that is `text` alone, not a command line that names it.
fn shows_line(dump: &Dump, text: &str) -> bool {
    dump.terminals.iter().flat_map(|t| t.lines.iter()).any(|line| line.trim() == text)
}

/// `value` is `want` points, give or take half a point.
fn about(value: f32, want: f32) -> bool {
    (value - want).abs() <= 0.5
}

/// ⌘D splits a shell of the stack's worker beside the new-session canvas. It is Slopty's terminal
/// of a worker session (the workspace lists it, with its rows), it takes the keyboard, and what
/// is typed runs there.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_split_holds_a_shell_of_the_worker() {
    let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
    let renders = stack.path("");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();

    drv.keys("cmd-d").await.unwrap();
    drv.wait_for("a shell at its prompt", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.prompts_settled()
    })
    .await
    .unwrap();

    drv.type_text("echo live surface").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the echo", STEP, |d| {
        d.lines_containing("live surface").len() >= 2 && d.prompts_settled()
    })
    .await
    .unwrap();

    goldens(drv, &renders, "surface-live-terminal").await;
    stack.shutdown().await;
}

/// What floats over a split shell: a program's progress (`OSC 9;4`), a bar along its pane's top
/// edge while it runs, the "N new lines" pill (the shell's 30 pt pill) when output arrives below a
/// view scrolled back, ghostty's find card (a 28 pt field; Esc gives the keys back and keeps the
/// card, Esc again closes it), the right-click menu (the shell's menu rows, one height), ⌥⌘K's
/// clear and the bar a program that ended leaves at the pane's foot. Each decides on its
/// numbers, and each state is a golden in both appearances.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_terminal_s_floats_wear_the_shell() {
    let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
    let renders = stack.path("");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    drv.wait_for("a shell at its prompt", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.prompts_settled()
    })
    .await
    .unwrap();

    // A program's progress shows along its pane's top edge, as a share a screen reader reads,
    // while it runs (`read` holds it), and goes when the next prompt comes
    // (`docs/decisions/terminal.md`, "The prompt ends it").
    drv.type_text("printf '\\e]9;4;1;40\\a'; read -r answer").await.unwrap();
    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the pane's progress", STEP, |d| {
            node(d, "ProgressIndicator", "Progress").is_some()
        })
        .await
        .unwrap();
    let progress = node(&dump, "ProgressIndicator", "Progress").unwrap();
    assert_eq!(progress.value.as_deref(), Some("40 %"), "{progress:?}");
    drv.keys("enter").await.unwrap();
    drv.wait_for("the progress cleared", STEP, |d| {
        node(d, "ProgressIndicator", "Progress").is_none() && d.prompts_settled()
    })
    .await
    .unwrap();

    // Scrolled back while output arrives, the view stays where it was and the foot counts the
    // lines that came below it; the pill takes it home. The 300 lines, then a pause to scroll in,
    // then a dozen more, so the count is the same every run.
    drv.type_text("seq 1 300; sleep 4; seq 301 312").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the first count", STEP, |d| shows_line(d, "300")).await.unwrap();
    let dump = drv.dump().await.unwrap();
    let [x, y, w, h] = dump.a11y.iter().find(|n| n.role == "Terminal").map(|n| n.bounds).unwrap();
    drv.scroll(x + w / 2.0, y + h / 2.0, 0.0, 40.0).await.unwrap();
    let dump = drv
        .wait_for("the new-lines pill, once the second count is in", STEP, |d| {
            new_lines(d).is_some() && d.terminals.iter().any(|t| t.at_prompt) && d.prompts_settled()
        })
        .await
        .unwrap();
    let pill = new_lines(&dump).unwrap();
    assert!(about(pill.bounds[3], 30.0), "the shell's pill is 30 tall: {pill:?}");
    assert!(!shows_line(&dump, "312"), "the view stayed where it was scrolled to");
    goldens(drv, &renders, "surface-live-terminal-scrolled").await;
    let [px_, py, pw, ph] = pill.bounds;
    drv.click(px_ + pw / 2.0, py + ph / 2.0).await.unwrap();
    drv.wait_for("back at the live end", STEP, |d| new_lines(d).is_none() && shows_line(d, "312"))
        .await
        .unwrap();

    // ⌘F opens ghostty's find card with the keyboard in its field (whose 28 pt the view's own
    // test measures), and its words find their hits.
    drv.keys("cmd-f").await.unwrap();
    let dump = drv
        .wait_for("the find field", STEP, |d| node(d, "SearchInput", "Find").is_some())
        .await
        .unwrap();
    let field = node(&dump, "SearchInput", "Find").unwrap();
    assert!(field.focused, "the find field holds the keyboard: {field:?}");
    let before = drv.dump().await.unwrap();
    println!("FOCUS terminal find opened: {} ({})", before.focus_path, before.focused);
    drv.type_text("29").await.unwrap();
    let tallied = drv
        .wait_for("the tally", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.label.as_deref() == Some("Matches")
                    && n.value.as_deref().is_some_and(|v| !v.is_empty() && !v.starts_with('0'))
            })
        })
        .await;
    if let Err(e) = tallied {
        // Where the typed keys went: the element that holds the keyboard, by its key contexts.
        let shown = drv.dump().await.unwrap();
        panic!(
            "{e:#}\nthe keyboard: {} ({}); when find opened: {} ({})",
            shown.focus_path, shown.focused, before.focus_path, before.focused
        );
    }
    goldens(drv, &renders, "surface-live-terminal-find").await;
    // Esc in the field gives the keys back to the grid and keeps the card and its hits.
    drv.keys("escape").await.unwrap();
    let dump = drv
        .wait_for("the keys back in the grid", STEP, |d| !d.focus_path.contains("TerminalSearch"))
        .await
        .unwrap();
    assert!(node(&dump, "SearchInput", "Find").is_some(), "the card stays: {}", dump.focus_path);
    // Esc again, in the grid, closes it.
    drv.keys("escape").await.unwrap();
    drv.wait_for("the find card closed", STEP, |d| node(d, "SearchInput", "Find").is_none())
        .await
        .unwrap();

    // A right click opens the shell's menu: its rows are the shell's menu rows, one height.
    drv.ok(&Command::Click { x: x + w / 2.0, y: y + h / 2.0, button: Button::Right, count: 1 })
        .await
        .unwrap();
    let dump =
        drv.wait_for("the menu", STEP, |d| d.a11y.iter().any(|n| n.role == "Menu")).await.unwrap();
    let rows: Vec<&A11yNode> = dump.a11y.iter().filter(|n| n.role == "MenuItem").collect();
    assert!(rows.len() >= 3, "{rows:?}");
    let height = rows[0].bounds[3];
    assert!((32.5..=33.5).contains(&height), "a menu row of zeron's (33 pt): {height}");
    assert!(rows.iter().all(|r| about(r.bounds[3], height)), "one row height: {rows:?}");
    goldens(drv, &renders, "surface-live-terminal-menu").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the menu closed", STEP, |d| !d.a11y.iter().any(|n| n.role == "Menu"))
        .await
        .unwrap();

    // ⌥⌘K clears the screen and its history, the prompt kept (⌘K is the session picker's).
    drv.keys("alt-cmd-k").await.unwrap();
    drv.wait_for("the screen cleared", STEP, |d| {
        !shows_line(d, "300") && !shows_line(d, "312") && d.prompts_settled()
    })
    .await
    .unwrap();

    // A shell that exits leaves the bar at the pane's foot that says so, with its code.
    drv.type_text("exit 3").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the exit bar", STEP, |d| node(d, "Status", "The shell exited, code 3").is_some())
        .await
        .unwrap();
    goldens(drv, &renders, "surface-live-terminal-exited").await;
    stack.shutdown().await;
}

/// A page a program in a split shell asks to open at a local address (`$BROWSER` with a dev
/// server's `localhost`) is offered, never opened unasked: a notice names the host and who asks
/// ("Open localhost:5173 from … on …"), with Open, and the program is told where it went.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_local_page_a_shell_asks_for_is_offered() {
    let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
    let renders = stack.path("");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    drv.wait_for("a shell at its prompt", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.prompts_settled()
    })
    .await
    .unwrap();

    drv.type_text("\"$BROWSER\" http://localhost:5173/").await.unwrap();
    drv.keys("enter").await.unwrap();
    let offer = |d: &Dump| {
        d.a11y
            .iter()
            .any(|n| n.label.as_deref().is_some_and(|l| l.contains("Open localhost:5173 from ")))
    };
    let shown = drv.wait_for("the offer", STEP, offer).await;
    if let Err(e) = shown {
        let said = drv.dump().await.unwrap();
        panic!("{e:#}\nthe app said:\n{}", slopty_e2e::frame_text(&said.a11y));
    }
    let dump = drv
        .wait_for("the program told", STEP, |d| {
            !d.lines_containing("is offered").is_empty() && d.prompts_settled()
        })
        .await
        .unwrap();
    assert!(
        node(&dump, "Button", "Open").is_some(),
        "the notice offers Open:\n{}",
        slopty_e2e::frame_text(&dump.a11y)
    );
    goldens(drv, &renders, "toast-offer").await;
    stack.shutdown().await;
}

/// The cases that stream video, a window of the worker's drawn screen: they run in the app
/// suite's coding group, after the rest and behind the encoder probe (`cargo xtask e2e app`).
mod stream {
    use super::*;

    /// The stream's whole layer, in pixels, once its picture moves between two renders a moment
    /// apart: the window's drawn page scrolls every frame, so all of the picture is left out of a
    /// golden while its chrome is held ([`goldens_masked`]). Leaving out only what moved in those
    /// 300 ms kept the rest of the page in, a different scroll on every run. Only the layer is
    /// looked at, so chrome fading in round it (the pane's overflow button, a tooltip) is no sign
    /// of life. `None` when nothing in it moved: the stream is not live.
    async fn moving(drv: &mut Driver, renders: &Path) -> Option<PixelRect> {
        let layer = drv.dump().await.unwrap().screens.iter().find_map(|s| s.layer)?;
        let first = drv.render(&renders.join("moving-a.png")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let second = drv.render(&renders.join("moving-b.png")).await.unwrap();
        let (a, b) = (&first.image, &second.image);
        if a.dimensions() != b.dimensions() {
            return None;
        }
        #[expect(clippy::cast_precision_loss, reason = "window points")]
        let [lx, ly, lw, lh] = layer.map(|v| v as f32 * first.scale);
        let inside = |x: u32, y: u32| {
            #[expect(clippy::cast_precision_loss, reason = "pixels")]
            let (x, y) = (x as f32, y as f32);
            x >= lx && x < lx + lw && y >= ly && y < ly + lh
        };
        let moved = a.enumerate_pixels().filter(|(x, y, _)| inside(*x, *y)).any(|(x, y, pixel)| {
            pixel.0.iter().zip(b.get_pixel(x, y).0).take(3).any(|(p, q)| p.abs_diff(q) > 24)
        });
        #[expect(clippy::cast_precision_loss, reason = "pixels")]
        let (width, height) = (a.width() as f32, a.height() as f32);
        let (left, top) = (lx.max(0.0).floor(), ly.max(0.0).floor());
        let (right, bottom) = ((lx + lw).min(width).ceil(), (ly + lh).min(height).ceil());
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
        let rect = [left, top, right - left, bottom - top].map(|v| v as u32);
        moved.then_some(rect)
    }

    /// What the stream's health mark says when its machine keeps up less well than the stream
    /// asks (`slopty_ui::screen::health::Health`): frames missing the display, or a bitrate the
    /// worker cut. In a guest that is a fact of the run's load, not of the chrome a golden holds,
    /// so the mark is left out wherever it shows. A stall is not: the stream's picture moving is
    /// asserted before each golden, and a stalled one fails it.
    const STRAINED: [&str; 2] = ["Frames late", "Low bandwidth"];

    /// [`goldens`] with the stream's picture left out (`stream`, [`moving`]), and a strained
    /// stream's health mark ([`STRAINED`]). Its pixels go a point wider each way, for the
    /// picture's edge. Its words go a point narrower, so chrome centred on that edge (the
    /// sidebar's splitter, once the picture fills the view) still counts.
    async fn goldens_masked(drv: &mut Driver, renders: &Path, name: &str, stream: PixelRect) {
        for (dark, appearance) in [(false, "light"), (true, "dark")] {
            drv.ok(&Command::Appearance { dark }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
            let golden = format!("{name}-{appearance}");
            let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
            println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
            let strained: Vec<PixelRect> = frame
                .a11y
                .iter()
                .filter(|n| n.label.as_deref().is_some_and(|l| STRAINED.contains(&l)))
                .map(|n| crate::served::mask_of(n, frame.scale))
                .collect();
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
            let edge = frame.scale.ceil() as u32;
            let [x, y, w, h] = stream;
            let (left, top) = (x.saturating_sub(edge), y.saturating_sub(edge));
            let right = x.saturating_add(w).saturating_add(edge);
            let bottom = y.saturating_add(h).saturating_add(edge);
            let picture = [left, top, right.saturating_sub(left), bottom.saturating_sub(top)];
            let (inset_w, inset_h) = (
                w.saturating_sub(edge.saturating_mul(2)),
                h.saturating_sub(edge.saturating_mul(2)),
            );
            let inside = [x.saturating_add(edge), y.saturating_add(edge), inset_w, inset_h];
            let pixels: Vec<PixelRect> = strained.iter().copied().chain([picture]).collect();
            let words: Vec<PixelRect> = strained.iter().copied().chain([inside]).collect();
            let masks = (pixels.as_slice(), words.as_slice());
            assert_matches_apart(&golden, &frame, TOLERANCE, &artifacts_dir(), masks).unwrap();
        }
        drv.ok(&Command::Appearance { dark: false }).await.unwrap();
    }

    /// The palette's Remote window opens one beside the session; its list holds the windows of
    /// the chat's machine, and choosing one shows its stream as a view, which its pane's zoom
    /// then makes fill the view. The worker serves its drawn screen
    /// (`SLOPTY_SYNTHETIC_SCREEN`): real video, encoded, sent and decoded as any worker's, with
    /// no Screen Recording grant and the same windows on every Mac. The picture scrolls, so the
    /// goldens hold the chrome round it, the stream's whole box left out ([`moving`]).
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e app"]
    async fn a_remote_window_is_a_view_beside_the_session_and_fills_it() {
        use crate::served::{open_thread, says, start, thread_until};
        const MACHINE: &str = "e2e-screen";
        let drawn = [("SLOPTY_SYNTHETIC_SCREEN", "1")];
        let mut stack =
            slopty_e2e::harness::ServedApp::launch_with(MACHINE, &[], &drawn).await.unwrap();
        let renders = stack.path("");
        let repo = stack.path("repo");
        let agent = start(&stack, MACHINE, &repo, "Say hello").await;
        thread_until(&stack, &agent, "said hello", |r| says(r, "Done: Say hello")).await;
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        open_thread(drv, 1).await;

        // A remote window beside the session, from the palette.
        drv.keys("cmd-shift-p").await.unwrap();
        drv.type_text("Remote window").await.unwrap();
        drv.wait_for("the palette's Remote window", STEP, |d| {
            d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("Remote window")))
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        let title = "Synthetic editor";
        let listed = drv
            .wait_for("the worker's windows", STEP, |d| {
                d.a11y.iter().any(|n| {
                    n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains(title))
                })
            })
            .await
            .unwrap();
        let window = listed
            .a11y
            .iter()
            .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains(title)))
            .unwrap();
        let (x, y) =
            (window.bounds[0] + window.bounds[2] / 2.0, window.bounds[1] + window.bounds[3] / 2.0);
        drv.click(x, y).await.unwrap();
        drv.wait_for("the window's pictures", STEP, |d| d.screens.iter().any(|s| s.presented > 10))
            .await
            .unwrap();
        let pane =
            moving(drv, &renders).await.expect("the window's picture moves: its stream is live");
        println!("MEASURE remote-window-surface: its picture {pane:?} px");
        goldens_masked(drv, &renders, "surface-live-screen", pane).await;

        // The window's pane, at the right with the keyboard, fills the view by its header's zoom;
        // filled, no pane wears a header.
        let zooms = drv.dump().await.unwrap();
        let [x, y, w, h] = zooms
            .a11y
            .iter()
            .filter(|n| n.role == "Button" && n.label.as_deref() == Some("Fill the view"))
            .max_by(|a, b| a.bounds[0].total_cmp(&b.bounds[0]))
            .map(|n| n.bounds)
            .expect("the window's pane's zoom");
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
        drv.wait_for("the window filling the view", STEP, |d| {
            node(d, "Button", "Fill the view").is_none()
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let whole = moving(drv, &renders).await.expect("the filling picture still moves");
        assert!(
            u64::from(whole[2]).saturating_mul(u64::from(whole[3]))
                > u64::from(pane[2]).saturating_mul(u64::from(pane[3])),
            "filling the view draws the picture larger: {whole:?} against {pane:?}"
        );
        println!("MEASURE remote-window-takeover: its picture {whole:?} px");
        goldens_masked(drv, &renders, "surface-live-screen-takeover", whole).await;
        stack.shutdown().await;
    }
}
