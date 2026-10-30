//! A model-based fuzzer over the whole shell in a headless window.
//!
//! A seed picks a screen (the self-test's Mac window, a narrow one, an iPhone at 3 ×, an iPad)
//! and a sequence of steps a person and the engine take: sessions opened and switched, a
//! subagent's timeline opened and left for its parent's, sessions' terminals coming and going,
//! the sidebar toggled, views summoned and shells split beside a session, its panes walked and
//! zoomed, shortcuts, resizes,
//! turns starting and ending, chats arriving and going, the clock and the wall clock moving on,
//! frames delivered mid-motion. After
//! every step: nothing panicked (GPUI's own retained check panics on a stale frame as it draws
//! it), the frame shown paints what a frame from scratch at its moment paints
//! ([`super::retention_tests::stale`]), the shell selected the chat the model says it did, the
//! focus is on something the frame holds, the accessibility tree is whole, and what it says of
//! the layout holds ([`layout_fault`]): every node inside the window, every control named,
//! no two controls side by side over each other, every shortcut spelled as a screen reader
//! reads it.
//!
//! A failure is shrunk (delta debugging: chunks of the sequence dropped, then single steps)
//! while it still fails the same way, and printed as its seed and the steps kept, which rerun
//! it exactly: `SLOPTY_FUZZ_SEED=<seed> SLOPTY_FUZZ_KEEP=<i,j,…>` with the long run's test
//! name. Exactly, because every date the engine's side stamps reads the shell's own clock, the
//! test's dispatcher's: a stamp from the wall clock set a turn's once-a-second redraw at a moment
//! the machine's load chose, and a stop no rerun found again. A short fixed-seed run is part of
//! the suite; [`a_long_fuzz_of_the_shell`] is the nightly's (`SLOPTY_FUZZ_RUNS`,
//! `SLOPTY_FUZZ_STEPS`).

use std::fmt::Write as _;
use std::time::Duration;

use chrono::DateTime;
use gpui::{TestAppContext, TestDispatcher, VisualTestContext};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use slopty_view_proto::SessionStatus;

use super::retention_tests::{
    PHONE, Screen, chat_row, frames, launch, session_row, stale, turn, wobble,
};
use super::*;

/// The screens a run picks from.
const SCREENS: [(&str, Screen); 4] = [
    ("desktop", super::retention_tests::DESKTOP),
    ("narrow", Screen { size: (720.0, 800.0), scale: 2.0, touch: false, reduced: false }),
    ("phone", PHONE),
    ("ipad", Screen { size: (1366.0, 1024.0), scale: 2.0, touch: true, reduced: false }),
];

/// The sizes a resize picks from on each kind of screen: a touch screen's turn and split view,
/// a Mac window dragged.
const RESIZES: [&[(f32, f32)]; 4] = [
    &[(1320.0, 880.0), (980.0, 700.0), (1600.0, 1000.0), (760.0, 600.0)],
    &[(720.0, 800.0), (1100.0, 800.0), (701.0, 640.0)],
    &[(402.0, 874.0), (390.0, 844.0), (440.0, 956.0)],
    &[(1366.0, 1024.0), (1024.0, 1366.0), (678.0, 1024.0)],
];

/// The shortcuts a step presses.
const KEYS: &[&str] = &[
    "cmd-1",
    "cmd-2",
    "cmd-3",
    "cmd-4",
    "cmd-b",
    "cmd-r",
    "cmd-e",
    "cmd-n",
    "cmd-[",
    "cmd-]",
    "cmd-i",
    "cmd-k",
    "cmd-l",
    "cmd-/",
    "cmd-.",
    "cmd-;",
    "cmd-f",
    "cmd-shift-t",
    "ctrl-tab",
    "ctrl-shift-tab",
    "cmd-d",
    "cmd-shift-d",
    "cmd-enter",
    "cmd-shift-enter",
    "alt-cmd-left",
    "alt-cmd-right",
    "alt-cmd-up",
    "alt-cmd-down",
    "alt-cmd-[",
    "alt-cmd-]",
    "escape",
    "escape",
    "tab",
    "enter",
];

/// The chats a run starts with: three at the top, two subagents of `b` that run inside it, one
/// of `a`.
const START: &[(&str, Option<(&str, bool)>)] = &[
    ("a", None),
    ("b", None),
    ("c", None),
    ("look", Some(("b", true))),
    ("peek", Some(("b", true))),
    ("helper", Some(("a", false))),
];

#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// A shortcut pressed.
    Key(&'static str),
    /// A chat's row tapped, or a subagent's card opened, while it is listed.
    Open(String),
    /// The inspector's way back from a subagent's timeline to its parent's.
    Back,
    /// A session's terminal coming (the session runs in one) or going (it ended).
    Terminal(String, bool),
    /// A chat's turn starting or ending.
    Turn(String, bool),
    /// The open chat's transcript in, with `n` streamed paragraphs on its last answer.
    Stream(usize),
    /// A chat started elsewhere arriving.
    Arrive(String),
    /// A chat going away (archived or deleted elsewhere).
    Leave(String),
    /// The window resized to one of its screen's sizes.
    Resize(usize),
    /// The clock moving on by this many ms, its frames drawn.
    Wait(u64),
    /// This many frames delivered 16 ms apart, the check taken mid-motion; the step before it
    /// is judged after one frame, not at rest.
    Frames(usize),
    /// The wall clock moving on before the check, as it does before the self-test's dump; the
    /// step before it is judged after one frame, not at rest.
    Pause,
}

/// splitmix64: the run's own randomness, the same for a seed on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// The screen a seed's run is on.
fn screen_of(seed: u64) -> usize {
    usize::try_from(seed % SCREENS.len() as u64).unwrap_or(0)
}

/// The steps a seed's run takes, `len` of them.
fn steps_of(seed: u64, len: usize) -> Vec<Step> {
    let mut rng = Rng(seed ^ 0x5106_7700);
    let resizes = RESIZES[screen_of(seed)].len();
    let mut ids: Vec<String> = START.iter().map(|(id, _)| (*id).to_owned()).collect();
    let mut arrived = 0;
    (0..len)
        .map(|_| match rng.below(100) {
            0..18 => Step::Key(rng.pick(KEYS)),
            18..32 => Step::Open(rng.pick(&ids).clone()),
            32..38 => Step::Back,
            38..48 => Step::Terminal(rng.pick(&ids).clone(), rng.below(3) != 0),
            48..56 => Step::Turn(rng.pick(&ids).clone(), rng.below(2) == 0),
            56..64 => Step::Stream(rng.below(6)),
            64..67 => {
                arrived += 1;
                let id = format!("new{arrived}");
                ids.push(id.clone());
                Step::Arrive(id)
            }
            67..75 => Step::Leave(rng.pick(&ids).clone()),
            75..80 => Step::Resize(rng.below(resizes)),
            80..86 => Step::Wait(*rng.pick(&[5, 40, 120, 400, 1500])),
            86..94 => Step::Frames(1 + rng.below(6)),
            _ => Step::Pause,
        })
        .collect()
}

/// What the model holds of the engine's side: the chats, their sessions and their terminals.
struct Model {
    chats: Vec<(String, Option<(String, bool)>)>,
    working: Vec<String>,
    /// The sessions running in a terminal, each in its own.
    terminals: Vec<String>,
}

impl Model {
    fn new() -> Self {
        Self {
            chats: START
                .iter()
                .map(|(id, parent)| ((*id).to_owned(), parent.map(|(p, i)| (p.to_owned(), i))))
                .collect(),
            working: vec!["a".into()],
            terminals: vec!["a".into(), "b".into()],
        }
    }

    /// The parent of chat `id`, when it is a subagent.
    fn parent(&self, id: &str) -> Option<String> {
        self.chats
            .iter()
            .find(|(chat, _)| chat == id)
            .and_then(|(_, parent)| parent.as_ref().map(|(p, _)| p.clone()))
    }

    /// Each session running in a terminal, by its chat: the view model's `WatchSessionTerminals`.
    fn session_terminals(&self) -> std::collections::BTreeMap<String, String> {
        self.terminals.iter().map(|chat| (chat.clone(), format!("t-{chat}"))).collect()
    }

    fn rows(&self, now: DateTime<Utc>) -> Vec<slopty_view_proto::Chat> {
        self.chats
            .iter()
            .enumerate()
            .map(|(ago, (id, parent))| {
                chat_row(
                    id,
                    i64::try_from(ago).unwrap_or(0) + 1,
                    parent.as_ref().map(|(p, i)| (p.as_str(), *i)),
                    now,
                )
            })
            .collect()
    }

    fn sessions(&self, now: DateTime<Utc>) -> Vec<slopty_view_proto::Session> {
        self.chats
            .iter()
            .map(|(id, _)| {
                let status = if self.working.contains(id) {
                    SessionStatus::Working
                } else {
                    SessionStatus::Done
                };
                session_row(id, status, now)
            })
            .collect()
    }
}

/// How a run failed: at which step, and what was found.
#[derive(Debug)]
struct Failure {
    step: usize,
    kind: &'static str,
    detail: String,
}

/// Now on the shell's clock, the test's dispatcher's: the date the engine's side stamps, which
/// the shell's own dates ([`crate::clock::utc`]) are measured against.
fn shell_now(cx: &mut VisualTestContext) -> DateTime<Utc> {
    crate::clock::utc_at(cx.executor().now())
}

/// The model's chats and sessions, stamped now, put in the shell's state.
fn apply_model(model: &Model, shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    let now = shell_now(cx);
    let (rows, sessions) = (model.rows(now), model.sessions(now));
    let terminals = model.session_terminals();
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_chats(rows);
            state.apply_sessions(sessions);
            state.apply_session_terminals(terminals);
            cx.notify();
        });
    });
}

/// The selected chat.
fn selected(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
}

/// Take `step` in the window.
fn take(
    step: &Step,
    shell: &Entity<Shell>,
    model: &mut Model,
    screen: usize,
    cx: &mut VisualTestContext,
) {
    let notify_state =
        |shell: &Entity<Shell>, cx: &mut VisualTestContext, f: &dyn Fn(&mut AppState)| {
            shell.update(cx, |shell, cx| {
                shell.state.update(cx, |state, cx| {
                    f(state);
                    cx.notify();
                });
            });
        };
    match step {
        Step::Key(keys) => cx.simulate_keystrokes(keys),
        // A row only stands for a chat that is listed: a gone one's way back is ⌘[ or a note.
        Step::Open(id) if model.chats.iter().any(|(chat, _)| chat == id) => {
            shell.update(cx, |shell, cx| shell.open_chat(id.clone(), cx));
        }
        Step::Open(_) => {}
        Step::Back => {
            if let Some(parent) = selected(shell, cx).and_then(|chat| model.parent(&chat)) {
                shell.update(cx, |shell, cx| shell.open_chat(parent, cx));
            }
        }
        Step::Terminal(id, running) => {
            model.terminals.retain(|chat| chat != id);
            if *running
                && model.parent(id).is_none()
                && model.chats.iter().any(|(chat, _)| chat == id)
            {
                model.terminals.push(id.clone());
            }
            let terminals = model.session_terminals();
            notify_state(shell, cx, &|state| {
                state.apply_session_terminals(terminals.clone());
            });
        }
        Step::Turn(id, working) => {
            model.working.retain(|w| w != id);
            if *working {
                model.working.push(id.clone());
            }
            let sessions = model.sessions(shell_now(cx));
            notify_state(shell, cx, &|state| {
                state.apply_sessions(sessions.clone());
            });
        }
        Step::Stream(paragraphs) => {
            let Some(chat) = selected(shell, cx) else { return };
            let working = model.working.contains(&chat);
            let mut entries = turn(&chat, shell_now(cx));
            if let Some(last) = entries.last_mut() {
                if !working {
                    last.status = Some(MessageStatus::Complete);
                }
                if let Some(MessagePart::Text { text, .. }) = last.parts.first_mut() {
                    for n in 0..*paragraphs {
                        let _ =
                            write!(text, "\n\nStreamed paragraph {n}, which says a little more.");
                    }
                }
            }
            notify_state(shell, cx, &|state| state.apply_transcript(entries.clone()));
        }
        Step::Arrive(id) => {
            model.chats.insert(0, (id.clone(), None));
            apply_model(model, shell, cx);
        }
        Step::Leave(id) => {
            model.chats.retain(|(chat, parent)| {
                chat != id && parent.as_ref().is_none_or(|(p, _)| p != id)
            });
            model.working.retain(|w| w != id);
            model.terminals.retain(|chat| model.chats.iter().any(|(listed, _)| listed == chat));
            apply_model(model, shell, cx);
        }
        Step::Resize(ix) => {
            let (w, h) = RESIZES[screen][*ix];
            cx.simulate_resize(gpui::size(px(w), px(h)));
        }
        Step::Wait(ms) => {
            cx.executor().advance_clock(Duration::from_millis(*ms));
        }
        Step::Frames(_) | Step::Pause => {}
    }
}

/// What a frame's accessibility tree gets wrong: a child named that is not in it, a node twice,
/// a node no parent holds, the focus on no node.
fn a11y_fault(window: &Window) -> Option<String> {
    let tree = window.a11y_tree()?;
    let ids: std::collections::HashMap<_, _> =
        tree.nodes.iter().map(|(id, node)| (*id, node)).collect();
    if ids.len() != tree.nodes.len() {
        return Some(format!("{} nodes listed, {} distinct", tree.nodes.len(), ids.len()));
    }
    let mut parents: std::collections::HashMap<_, usize> = std::collections::HashMap::new();
    for (id, node) in &tree.nodes {
        for child in node.children() {
            if !ids.contains_key(child) {
                return Some(format!("{id:?} names a child {child:?} the tree does not hold"));
            }
            *parents.entry(*child).or_default() += 1;
        }
    }
    let root = tree.tree.as_ref().map(|tree| tree.root);
    for (id, node) in &tree.nodes {
        let held = parents.get(id).copied().unwrap_or(0);
        if Some(*id) != root && held != 1 {
            return Some(format!(
                "{id:?} ({:?} {:?}) is held by {held} parents",
                node.role(),
                node.label()
            ));
        }
    }
    (!ids.contains_key(&tree.focus)).then(|| format!("the focus {:?} is on no node", tree.focus))
}

/// The roles of the controls a person acts on: a screen reader names each.
const CONTROLS: &[gpui::Role] = &[
    gpui::Role::Button,
    gpui::Role::Link,
    gpui::Role::CheckBox,
    gpui::Role::Switch,
    gpui::Role::RadioButton,
    gpui::Role::Tab,
    gpui::Role::MenuItem,
    gpui::Role::MenuItemCheckBox,
    gpui::Role::MenuItemRadio,
    gpui::Role::ComboBox,
    gpui::Role::TextInput,
    gpui::Role::MultilineTextInput,
    gpui::Role::SearchInput,
    gpui::Role::Slider,
    gpui::Role::DisclosureTriangle,
];

/// The rounding a node's bounds may take past the window's edge or into a neighbour's, in
/// device pixels.
const SLACK: f64 = 1.0;

/// What a frame's accessibility tree says the layout gets wrong: a node that shows outside
/// the window (its bounds are what its clip leaves of it) or is turned inside out, a control with
/// nothing to name it by, two controls side by side laid over each other (a click finds only one),
/// a shortcut not in `aria-keyshortcuts` spelling. Controls side by side are those of one
/// container: the window's own children are its layers (the page, and the menus, popovers and
/// tooltips over it). On a touch screen a control takes presses over a finger's 44 pt around what
/// it draws, into its neighbours' gaps and past them by design (#23), so there controls may lie
/// over each other.
fn layout_fault(window: &Window, touch: bool) -> Option<String> {
    let tree = window.a11y_tree()?;
    let scale = f64::from(window.scale_factor());
    let size = window.viewport_size();
    let (width, height) =
        (f64::from(f32::from(size.width)) * scale, f64::from(f32::from(size.height)) * scale);
    let nodes: std::collections::HashMap<_, _> =
        tree.nodes.iter().map(|(id, node)| (*id, node)).collect();
    let says = |text: Option<&str>| text.is_some_and(|t| !t.trim().is_empty());
    let root = tree.tree.as_ref().map(|tree| tree.root);
    for (id, node) in &tree.nodes {
        let what = || format!("{id:?} ({:?} {:?})", node.role(), node.label());
        if let Some(r) = node.bounds() {
            if ![r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) || r.x0 > r.x1 || r.y0 > r.y1
            {
                return Some(format!("{} has bounds {r:?}", what()));
            }
            // A node clipped out whole keeps a rect of no size where it would be.
            let shows = r.x1 - r.x0 > SLACK && r.y1 - r.y0 > SLACK;
            if shows
                && (r.x0 < -SLACK || r.y0 < -SLACK || r.x1 > width + SLACK || r.y1 > height + SLACK)
            {
                return Some(format!(
                    "{} at {r:?} is outside the {width} × {height} window",
                    what()
                ));
            }
        }
        if CONTROLS.contains(&node.role()) && !node.is_hidden() {
            // Named by itself or by what it holds, as a screen reader names a button by its
            // text.
            let mut held = vec![(*id, 0_u8)];
            let mut named = false;
            while let Some((at, depth)) = held.pop() {
                let Some(n) = nodes.get(&at) else { continue };
                if says(n.label()) || says(n.value()) || !n.labelled_by().is_empty() {
                    named = true;
                    break;
                }
                if depth < 4 {
                    held.extend(n.children().iter().map(|child| (*child, depth + 1)));
                }
            }
            if !named {
                return Some(format!("{} has nothing to name it by", what()));
            }
        }
        if let Some(keys) = node.keyboard_shortcut()
            && !aria_spelling(keys)
        {
            return Some(format!(
                "{} has the shortcut {keys:?}, not in aria-keyshortcuts spelling",
                what()
            ));
        }
        // The window's own children are its layers: menus, popovers and tooltips lie over the
        // page by design.
        if touch || Some(*id) == root {
            continue;
        }
        let sides: Vec<_> = node
            .children()
            .iter()
            .filter_map(|child| nodes.get(child).map(|n| (child, n)))
            .filter(|(_, n)| CONTROLS.contains(&n.role()) && !n.is_hidden())
            .filter_map(|(child, n)| n.bounds().map(|r| (child, n, r)))
            .filter(|(_, _, r)| r.x1 - r.x0 > SLACK && r.y1 - r.y0 > SLACK)
            .collect();
        for (i, (a, an, ar)) in sides.iter().enumerate() {
            for (b, bn, br) in &sides[i + 1..] {
                let across = ar.x1.min(br.x1) - ar.x0.max(br.x0);
                let down = ar.y1.min(br.y1) - ar.y0.max(br.y0);
                if across > SLACK && down > SLACK {
                    return Some(format!(
                        "{a:?} ({:?} {:?}) at {ar:?} and {b:?} ({:?} {:?}) at {br:?}, side by side in {}, lie over each other",
                        an.role(),
                        an.label(),
                        bn.role(),
                        bn.label(),
                        what()
                    ));
                }
            }
        }
    }
    None
}

/// `keys` in `aria-keyshortcuts` spelling, as [`crate::keys::aria`] writes it: shortcuts apart
/// by spaces, each its modifiers in the order Control, Alt, Shift, Meta and then one key.
fn aria_spelling(keys: &str) -> bool {
    const MODIFIERS: [&str; 4] = ["Control", "Alt", "Shift", "Meta"];
    !keys.is_empty()
        && keys.split(' ').all(|stroke| {
            // The plus key itself is the one key a `+` may end on.
            let (modifiers, key) = match stroke.strip_suffix("++") {
                Some(modifiers) => (modifiers, "+"),
                None => stroke.rsplit_once('+').unwrap_or(("", stroke)),
            };
            let mut order = MODIFIERS.iter();
            !key.is_empty()
                && !MODIFIERS.contains(&key)
                && (modifiers.is_empty()
                    || modifiers.split('+').all(|m| order.any(|known| *known == m)))
        })
}

/// Run `steps` on the seed's screen, checking after each; the first failure.
fn run(seed: u64, steps: &[Step]) -> Result<(), Failure> {
    let screen = screen_of(seed);
    let steps = steps.to_vec();
    // A fresh thread each run: the shell's thread-locals (a forced touch screen, the motion
    // morphs) start clean, and a panic ends only it.
    let outcome = std::thread::Builder::new()
        .name(format!("fuzz-{seed}"))
        .stack_size(64 << 20)
        .spawn(move || {
            let step_at = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
            let at = step_at.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                // Leaked: a panic unwinds no app, whose teardown would panic again.
                let app: &'static mut TestAppContext =
                    Box::leak(Box::new(TestAppContext::build(TestDispatcher::new(seed), None)));
                let (shell, cx) = launch(app, SCREENS[screen].1);
                cx.update(|window, _| window.set_a11y_active(true));
                let mut model = Model::new();
                // The launch's chats stamped again on the shell's clock, as the model holds them.
                apply_model(&model, &shell, cx);
                frames(cx, Duration::from_millis(16), 60);
                for (ix, step) in steps.iter().enumerate() {
                    at.set(ix);
                    let before = selected(&shell, cx);
                    let parent = before.as_deref().and_then(|chat| model.parent(chat));
                    take(step, &shell, &mut model, screen, cx);
                    // A step followed by frames or a pause is judged mid-motion: one frame
                    // drawn, then those; any other comes to rest first.
                    let paced = matches!(steps.get(ix + 1), Some(Step::Frames(_) | Step::Pause));
                    let one_frame = |cx: &mut VisualTestContext| {
                        cx.executor().advance_clock(Duration::from_millis(16));
                        cx.update(|window, cx| window.simulate_next_frame(cx));
                        cx.run_until_parked();
                    };
                    match step {
                        Step::Frames(n) => (0..*n).for_each(|_| one_frame(cx)),
                        Step::Pause => {}
                        _ if paced => one_frame(cx),
                        _ => {
                            frames(cx, Duration::from_millis(16), 40);
                        }
                    }
                    if matches!(step, Step::Pause) {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    if let Some(found) = stale(cx) {
                        return Err(Failure { step: ix, kind: "stale", detail: found });
                    }
                    if let Some(found) = wobble(cx) {
                        return Err(Failure { step: ix, kind: "wall clock", detail: found });
                    }
                    let now = selected(&shell, cx);
                    let present = |id: &str| model.chats.iter().any(|(chat, _)| chat == id);
                    let expected = match step {
                        Step::Open(id) if present(id) => Some(Some(id.clone())),
                        Step::Back => Some(parent.clone().or(before)),
                        _ => None,
                    };
                    if let Some(expected) = expected
                        && expected.as_ref().is_some_and(|id| present(id))
                        && now != expected
                    {
                        return Err(Failure {
                            step: ix,
                            kind: "model",
                            detail: format!("selected {now:?}, the model says {expected:?}"),
                        });
                    }
                    if let Some(id) = &now
                        && !present(id)
                    {
                        return Err(Failure {
                            step: ix,
                            kind: "model",
                            detail: format!("{id} is selected but gone"),
                        });
                    }
                    let fault = cx.update(|window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                        a11y_fault(window).map(|fault| ("a11y", fault)).or_else(|| {
                            layout_fault(window, SCREENS[screen].1.touch)
                                .map(|fault| ("layout", fault))
                        })
                    });
                    if let Some((kind, fault)) = fault {
                        return Err(Failure { step: ix, kind, detail: fault });
                    }
                }
                Ok(())
            }));
            match result {
                Ok(outcome) => outcome,
                Err(payload) => {
                    let detail = payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_else(|| "a panic".into());
                    std::mem::forget(payload);
                    let step = step_at.get();
                    Err(Failure { step, kind: "panic", detail })
                }
            }
        })
        .expect("a fuzz thread")
        .join();
    outcome.unwrap_or_else(|_| {
        Err(Failure { step: usize::MAX, kind: "panic", detail: "the run's thread died".into() })
    })
}

/// The steps of `steps` kept by `keep`.
fn kept(steps: &[Step], keep: &[usize]) -> Vec<Step> {
    keep.iter().map(|&ix| steps[ix].clone()).collect()
}

/// Delta debugging: the fewest of `steps` (by index) that still fail as `kind`, dropping
/// chunks halving in size, then single steps.
fn shrink(seed: u64, steps: &[Step], failed_at: usize, kind: &str) -> Vec<usize> {
    // The frames or pause after the failing step are part of it: they had it judged mid-motion,
    // and the same step brought to rest may draw alike (seed 4255's stop, which no try without
    // its pause failed again).
    let paced = matches!(steps.get(failed_at + 1), Some(Step::Frames(_) | Step::Pause));
    let last = (failed_at + usize::from(paced)).min(steps.len() - 1);
    let mut keep: Vec<usize> = (0..=last).collect();
    // Each try is a whole run: a bounded number of them, the shortest found by then kept.
    let tries = std::cell::Cell::new(0_u32);
    let fails = |keep: &[usize]| {
        tries.set(tries.get() + 1);
        tries.get() <= 300 && run(seed, &kept(steps, keep)).err().is_some_and(|f| f.kind == kind)
    };
    let mut chunk = keep.len() / 2;
    while chunk >= 1 {
        let mut start = 0;
        while start < keep.len() {
            let mut candidate = keep.clone();
            let end = (start + chunk).min(candidate.len());
            candidate.drain(start..end);
            if !candidate.is_empty() && fails(&candidate) {
                keep = candidate;
            } else {
                start += chunk;
            }
        }
        chunk /= 2;
    }
    keep
}

/// Run `runs` seeds from `first`, `len` steps each, and report the first failure: shrunk to
/// its fewest steps when `shrink` (each try a whole run: the long run's), else at once with the
/// rerun that shrinks it.
fn fuzz(first: u64, runs: u64, len: usize, shrink_it: bool) {
    if let (Ok(seed), Ok(keep)) =
        (std::env::var("SLOPTY_FUZZ_SEED"), std::env::var("SLOPTY_FUZZ_KEEP"))
    {
        let seed: u64 = seed.parse().expect("SLOPTY_FUZZ_SEED is a number");
        let keep: Vec<usize> = keep
            .split(',')
            .map(|ix| ix.trim().parse().expect("SLOPTY_FUZZ_KEEP lists indices"))
            .collect();
        let len = keep.iter().max().map_or(0, |max| max + 1).max(len);
        let steps = kept(&steps_of(seed, len), &keep);
        if let Err(failure) = run(seed, &steps) {
            panic!(
                "seed {seed} {:?} fails at {:?}: {}: {}",
                keep,
                steps.get(failure.step),
                failure.kind,
                failure.detail
            );
        }
        return;
    }
    for seed in first..first + runs {
        let steps = steps_of(seed, len);
        let Err(failure) = run(seed, &steps) else { continue };
        if !shrink_it {
            panic!(
                "the shell fuzz failed on {} (seed {seed}) at step {} {:?}, {}: {}\n\nshrink it: \
                 SLOPTY_FUZZ_FIRST={seed} SLOPTY_FUZZ_RUNS=1 SLOPTY_FUZZ_STEPS={len} cargo nextest run \
                 -p slopty-shell --run-ignored all a_long_fuzz_of_the_shell",
                SCREENS[screen_of(seed)].0,
                failure.step,
                steps.get(failure.step),
                failure.kind,
                failure.detail,
            );
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let keep = shrink(seed, &steps, failure.step, failure.kind);
        std::panic::set_hook(hook);
        let minimal = run(seed, &kept(&steps, &keep)).err();
        let mut report = format!(
            "the shell fuzz failed on {} (seed {seed}), {}: {}\n\nshrunk to {} steps:\n",
            SCREENS[screen_of(seed)].0,
            failure.kind,
            failure.detail,
            keep.len()
        );
        for ix in &keep {
            let _ = writeln!(report, "  {ix}: {:?}", steps[*ix]);
        }
        if let Some(minimal) = minimal {
            let _ = write!(report, "\nwhich fails {}: {}\n", minimal.kind, minimal.detail);
        }
        let list = keep.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
        let _ = write!(
            report,
            "\nrerun: SLOPTY_FUZZ_SEED={seed} SLOPTY_FUZZ_KEEP={list} cargo nextest run -p slopty-shell --run-ignored all a_long_fuzz_of_the_shell"
        );
        panic!("{report}");
    }
}

/// A few fixed seeds, one per screen, a short sequence each, every oracle after every step and
/// no shrinking: about a second here, a few on the gate's three-core runner (its `tests ui` shard
/// is the gate's critical path). The depth is the long run's.
#[test]
fn a_short_fuzz_of_the_shell_finds_nothing() {
    fuzz(1, 4, 8, false);
}

/// The nightly's: many seeds, long sequences.
#[test]
#[ignore = "long: the nightly runs it"]
fn a_long_fuzz_of_the_shell() {
    let env = |name: &str, default: u64| {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    };
    let first = env("SLOPTY_FUZZ_FIRST", 1000);
    fuzz(
        first,
        env("SLOPTY_FUZZ_RUNS", 64),
        usize::try_from(env("SLOPTY_FUZZ_STEPS", 120)).unwrap_or(120),
        true,
    );
}

/// The long run's stop on the iPad (seed 4255, 2026-10-18) brought down to itself: a prompt that
/// shows open until its first layout wraps it past five lines folds in a frame of its own, the
/// window otherwise at rest. The timeline is the inspector's now. A prompt's bubble learns its
/// wrapped height as it paints, and a view notified while its window paints asks for no frame, so
/// the fold once waited for whatever drew next.
#[gpui::test]
fn a_prompt_wrapped_past_five_lines_folds_with_nothing_else_drawing(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, super::retention_tests::DESKTOP);
    let now = shell_now(cx);
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            // No turn at work: no spinner and no turn's clock draws a frame of their own.
            state.apply_sessions(vec![
                session_row("a", SessionStatus::Done, now),
                session_row("b", SessionStatus::Done, now),
            ]);
            cx.notify();
        });
        shell.open_chat("b".into(), cx);
    });
    cx.update(|window, _| window.set_a11y_active(true));
    frames(cx, Duration::from_millis(16), 60);
    // ⌘I opens the session's inspector, the timeline in it.
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.toggle_inspector(cx)));
    frames(cx, Duration::from_millis(16), 60);
    assert!(shell.read_with(cx, |shell, cx| shell.inspector_shown(cx)), "the inspector shows");
    // Under the 400 characters that fold a prompt before it is measured, on one line: only its
    // layout knows it wraps past five lines in the column.
    let prompt = format!("{}word", "word ".repeat(79));
    let entry = |id: &str, role, text: &str, status| SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text { id: format!("{id}:t"), text: text.into() }],
        created_at: now.timestamp_millis(),
        device_id: "local".into(),
        status,
        continuation_of: None,
        duration_ms: None,
    };
    let entries = vec![
        entry("b:ask", MessageRole::User, &prompt, None),
        entry("b:answer", MessageRole::Assistant, "Done.", Some(MessageStatus::Complete)),
    ];
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(entries);
            cx.notify();
        });
    });
    frames(cx, Duration::from_millis(16), 60);
    assert_eq!(stale(cx), None, "the prompt as the window shows it at rest");
    let folded = cx.update(|window, _| {
        window.a11y_tree().is_some_and(|tree| {
            tree.nodes.iter().any(|(_, node)| node.label() == Some("Expand message"))
        })
    });
    assert!(folded, "the prompt wraps past five lines and folds");
}

/// What the fuzzer found first (seed 6 on the phone, shrunk to two steps): a chat left, then
/// gone, came back as an empty page in its name. The way back skips to the canvas instead.
#[test]
fn a_gone_chat_is_not_opened_again_by_the_way_back() {
    let steps = [
        Step::Open("c".into()),
        Step::Open("a".into()),
        Step::Leave("c".into()),
        Step::Key("cmd-["),
    ];
    for seed in [0, 2] {
        if let Err(failure) = run(seed, &steps) {
            panic!(
                "{} at {:?}: {}: {}",
                SCREENS[screen_of(seed)].0,
                steps.get(failure.step),
                failure.kind,
                failure.detail
            );
        }
    }
}

/// The fuzzer's own parts: the same seed takes the same steps, and shrinking keeps a failing
/// step that a run needs.
#[test]
fn a_seed_takes_the_same_steps_every_time() {
    assert_eq!(steps_of(7, 40), steps_of(7, 40));
    assert_ne!(steps_of(7, 40), steps_of(8, 40));
    assert_eq!(screen_of(2), 2, "seed 2 is a phone's");
}

/// The spelling the layout check holds a shortcut to is the one the shell writes.
#[test]
fn a_shortcut_is_held_to_the_spelling_the_shell_writes() {
    for (combo, mac) in [
        ("cmd-shift-t", true),
        ("ctrl-alt-shift-cmd-k", true),
        ("ctrl-tab", false),
        ("cmd-k cmd-s", true),
    ] {
        let aria = crate::keys::aria_combo_on(mac, combo);
        assert!(aria_spelling(&aria), "{combo} as {aria}");
    }
    for wrong in ["", "Meta+", "Shift+Control+A", "Meta+Meta+A", "Command+A", "Meta+Shift"] {
        assert!(!aria_spelling(wrong), "{wrong:?}");
    }
    assert!(aria_spelling("Meta++"), "the plus key");
}
