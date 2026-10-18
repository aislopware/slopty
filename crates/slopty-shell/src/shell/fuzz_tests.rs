// slopty: Slopty's own tests (not in zeron).
//! slopty: a model-based fuzzer over the whole shell in a headless window.
//!
//! A seed picks a screen (the self-test's Mac window, a narrow one, an iPhone at 3 ×, an iPad)
//! and a sequence of steps a person and the engine take: chats opened and switched, subagents
//! drilled into and left, drafts typed and sent, turns stopped, the sidebar and the right pane
//! toggled, shortcuts, resizes, turns starting and ending, chats arriving and going, a queue
//! coming and going, the clock and the wall clock moving on, frames delivered mid-motion. After
//! every step: nothing panicked (GPUI's own retained check panics on a stale frame as it draws
//! it), the frame shown paints what a frame from scratch at its moment paints
//! ([`super::retention_tests::stale`]), the shell selected the chat the model says it did, the
//! focus is on something the frame holds, and the accessibility tree is whole.
//!
//! A failure is shrunk (delta debugging: chunks of the sequence dropped, then single steps)
//! while it still fails the same way, and printed as its seed and the steps kept, which rerun
//! it exactly: `SLOPTY_FUZZ_SEED=<seed> SLOPTY_FUZZ_KEEP=<i,j,…>` with the long run's test
//! name. A short fixed-seed run is part of the suite; [`a_long_fuzz_of_the_shell`] is the
//! nightly's (`SLOPTY_FUZZ_RUNS`, `SLOPTY_FUZZ_STEPS`).

use std::fmt::Write as _;
use std::time::Duration;

use gpui::{TestAppContext, TestDispatcher, VisualTestContext};
use slopty_view_doc::{MessagePart, MessageStatus, QueuedMessage};
use slopty_view_proto::SessionStatus;

use super::retention_tests::{PHONE, Screen, chat_row, frames, launch, session_row, stale, turn, wobble};
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
    "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-b", "cmd-r", "cmd-e", "cmd-n", "cmd-[", "cmd-]", "cmd-t",
    "cmd-i", "cmd-k", "cmd-/", "cmd-.", "cmd-;", "cmd-f", "cmd-shift-t", "ctrl-tab", "ctrl-shift-tab",
    "escape", "escape", "tab", "enter",
];

/// The chats a run starts with: two at the top, a subagent of `b` that runs inside it, one of
/// `a` that takes messages.
const START: &[(&str, Option<(&str, bool)>)] =
    &[("a", None), ("b", None), ("c", None), ("look", Some(("b", true))), ("helper", Some(("a", false)))];

#[derive(Clone, Debug, PartialEq)]
enum Step {
    /// A shortcut pressed.
    Key(&'static str),
    /// A chat's row tapped, or a subagent's card opened, while it is listed.
    Open(String),
    /// A subagent's "Back to …".
    Back,
    /// Words typed into the composer, focused first as a tap focuses it.
    Type(&'static str),
    /// The composer's draft sent with Enter.
    Send,
    /// A chat's turn starting or ending.
    Turn(String, bool),
    /// The open chat's transcript in, with `n` streamed paragraphs on its last answer.
    Stream(usize),
    /// A chat started elsewhere arriving.
    Arrive(String),
    /// A chat going away (archived or deleted elsewhere).
    Leave(String),
    /// A message queued under the open chat, or the queue delivered.
    Queue(bool),
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
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
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
            38..44 => Step::Type(rng.pick(&["Say more", "x", "A longer draft that wraps onto a second line of the composer"])),
            44..48 => Step::Send,
            48..56 => Step::Turn(rng.pick(&ids).clone(), rng.below(2) == 0),
            56..64 => Step::Stream(rng.below(6)),
            64..67 => {
                arrived += 1;
                let id = format!("new{arrived}");
                ids.push(id.clone());
                Step::Arrive(id)
            }
            67..70 => Step::Leave(rng.pick(&ids).clone()),
            70..75 => Step::Queue(rng.below(2) == 0),
            75..80 => Step::Resize(rng.below(resizes)),
            80..86 => Step::Wait(*rng.pick(&[5, 40, 120, 400, 1500])),
            86..94 => Step::Frames(1 + rng.below(6)),
            _ => Step::Pause,
        })
        .collect()
}

/// What the model holds of the engine's side: the chats, their sessions and what is open.
struct Model {
    chats: Vec<(String, Option<(String, bool)>)>,
    working: Vec<String>,
}

impl Model {
    fn new() -> Self {
        Self {
            chats: START.iter().map(|(id, parent)| ((*id).to_owned(), parent.map(|(p, i)| (p.to_owned(), i)))).collect(),
            working: vec!["a".into()],
        }
    }

    fn rows(&self) -> Vec<slopty_view_proto::Chat> {
        let now = Utc::now();
        self.chats
            .iter()
            .enumerate()
            .map(|(ago, (id, parent))| {
                chat_row(id, i64::try_from(ago).unwrap_or(0) + 1, parent.as_ref().map(|(p, i)| (p.as_str(), *i)), now)
            })
            .collect()
    }

    fn sessions(&self) -> Vec<slopty_view_proto::Session> {
        let now = Utc::now();
        self.chats
            .iter()
            .map(|(id, _)| {
                let status = if self.working.contains(id) { SessionStatus::Working } else { SessionStatus::Done };
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

/// The selected chat.
fn selected(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
}

/// Take `step` in the window.
fn take(step: &Step, shell: &Entity<Shell>, model: &mut Model, screen: usize, cx: &mut VisualTestContext) {
    let notify_state = |shell: &Entity<Shell>, cx: &mut VisualTestContext, f: &dyn Fn(&mut AppState)| {
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
            shell.update(cx, |shell, cx| {
                shell.leave_drill(cx);
            });
        }
        Step::Type(text) => {
            let input = shell.read_with(cx, |shell, cx| shell.composer.read(cx).input.clone());
            cx.update(|window, cx| window.focus(&input.read(cx).focus_handle(cx), cx));
            cx.run_until_parked();
            cx.simulate_input(text);
        }
        Step::Send => {
            let input = shell.read_with(cx, |shell, cx| shell.composer.read(cx).input.clone());
            cx.update(|window, cx| window.focus(&input.read(cx).focus_handle(cx), cx));
            cx.run_until_parked();
            cx.simulate_keystrokes("enter");
        }
        Step::Turn(id, working) => {
            model.working.retain(|w| w != id);
            if *working {
                model.working.push(id.clone());
            }
            let sessions = model.sessions();
            notify_state(shell, cx, &|state| {
                state.apply_sessions(sessions.clone());
            });
        }
        Step::Stream(paragraphs) => {
            let Some(chat) = selected(shell, cx) else { return };
            let working = model.working.contains(&chat);
            let mut entries = turn(&chat, Utc::now());
            if let Some(last) = entries.last_mut() {
                if !working {
                    last.status = Some(MessageStatus::Complete);
                }
                if let Some(MessagePart::Text { text, .. }) = last.parts.first_mut() {
                    for n in 0..*paragraphs {
                        let _ = write!(text, "\n\nStreamed paragraph {n}, which says a little more.");
                    }
                }
            }
            notify_state(shell, cx, &|state| state.apply_transcript(entries.clone()));
        }
        Step::Arrive(id) => {
            model.chats.insert(0, (id.clone(), None));
            let rows = model.rows();
            let sessions = model.sessions();
            notify_state(shell, cx, &|state| {
                state.apply_chats(rows.clone());
                state.apply_sessions(sessions.clone());
            });
        }
        Step::Leave(id) => {
            model.chats.retain(|(chat, parent)| chat != id && parent.as_ref().is_none_or(|(p, _)| p != id));
            model.working.retain(|w| w != id);
            let rows = model.rows();
            let sessions = model.sessions();
            notify_state(shell, cx, &|state| {
                state.apply_chats(rows.clone());
                state.apply_sessions(sessions.clone());
            });
        }
        Step::Queue(on) => {
            let queue = if *on { vec![QueuedMessage::new("q1", "Say more", "local")] } else { Vec::new() };
            notify_state(shell, cx, &|state| state.queue = queue.clone());
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
    let ids: std::collections::HashMap<_, _> = tree.nodes.iter().map(|(id, node)| (*id, node)).collect();
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
            return Some(format!("{id:?} ({:?} {:?}) is held by {held} parents", node.role(), node.label()));
        }
    }
    (!ids.contains_key(&tree.focus)).then(|| format!("the focus {:?} is on no node", tree.focus))
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
                frames(cx, Duration::from_millis(16), 60);
                for (ix, step) in steps.iter().enumerate() {
                    at.set(ix);
                    let before = selected(&shell, cx);
                    let drill = shell.read_with(cx, |shell, cx| shell.subagent_drill(cx));
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
                        Step::Back => Some(drill.map_or(before, |drill| Some(drill.parent))),
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
                        return Err(Failure { step: ix, kind: "model", detail: format!("{id} is selected but gone") });
                    }
                    let fault = cx.update(|window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                        a11y_fault(window)
                    });
                    if let Some(fault) = fault {
                        return Err(Failure { step: ix, kind: "a11y", detail: fault });
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
    outcome.unwrap_or_else(|_| Err(Failure { step: usize::MAX, kind: "panic", detail: "the run's thread died".into() }))
}

/// The steps of `steps` kept by `keep`.
fn kept(steps: &[Step], keep: &[usize]) -> Vec<Step> {
    keep.iter().map(|&ix| steps[ix].clone()).collect()
}

/// Delta debugging: the fewest of `steps` (by index) that still fail as `kind`, dropping
/// chunks halving in size, then single steps.
fn shrink(seed: u64, steps: &[Step], failed_at: usize, kind: &str) -> Vec<usize> {
    let mut keep: Vec<usize> = (0..=failed_at.min(steps.len() - 1)).collect();
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
    if let (Ok(seed), Ok(keep)) = (std::env::var("SLOPTY_FUZZ_SEED"), std::env::var("SLOPTY_FUZZ_KEEP")) {
        let seed: u64 = seed.parse().expect("SLOPTY_FUZZ_SEED is a number");
        let keep: Vec<usize> = keep.split(',').map(|ix| ix.trim().parse().expect("SLOPTY_FUZZ_KEEP lists indices")).collect();
        let len = keep.iter().max().map_or(0, |max| max + 1).max(len);
        let steps = kept(&steps_of(seed, len), &keep);
        if let Err(failure) = run(seed, &steps) {
            panic!("seed {seed} {:?} fails at {:?}: {}: {}", keep, steps.get(failure.step), failure.kind, failure.detail);
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
    let env = |name: &str, default: u64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    let first = env("SLOPTY_FUZZ_FIRST", 1000);
    fuzz(first, env("SLOPTY_FUZZ_RUNS", 64), usize::try_from(env("SLOPTY_FUZZ_STEPS", 120)).unwrap_or(120), true);
}

/// The long run's three stops on a Mac (2026-10-18), shrunk: a cached spinner in the composer
/// drawn again after the composer's clip moved (a turn started, the window resized, a chat
/// opened). They were the old every-frame check showing its own frame, not the shell's; they
/// stay as the check's regression seeds.
#[test]
fn the_long_run_s_clip_stops_draw_alike() {
    let stops: [(u64, &[usize]); 3] = [
        (1044, &[1, 10, 31, 46, 47, 97]),
        (3140, &[1, 3, 11, 13, 15, 18, 35, 42, 45]),
        (4132, &[51, 54, 56, 66, 76]),
    ];
    for (seed, keep) in stops {
        let len = keep.iter().max().map_or(0, |max| max + 1);
        let steps = kept(&steps_of(seed, len), keep);
        if let Err(failure) = run(seed, &steps) {
            panic!("seed {seed} {keep:?} fails at {:?}: {}: {}", steps.get(failure.step), failure.kind, failure.detail);
        }
    }
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
            panic!("{} at {:?}: {}: {}", SCREENS[screen_of(seed)].0, steps.get(failure.step), failure.kind, failure.detail);
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
