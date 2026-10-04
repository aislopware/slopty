//! The sprites, read pixel by pixel, and the frames a companion asks for, counted headless.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px,
};
use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::{AgentId, Liveness, kind};
use slopty_theme::{Companions, Contrast, Theme, Variant};

use super::sprites::{self, Ink};
use super::*;

/// Every frame each pose steps through, with and without a blink and a stride.
fn every_beat(pose: Pose) -> impl Iterator<Item = Beat> {
    (0..pose.frames()).flat_map(|frame| {
        [(false, false), (true, false), (false, true)]
            .into_iter()
            .map(move |(blink, stride)| Beat { frame, blink, stride })
    })
}

/// The cells a frame draws.
fn mask(canvas: &Canvas) -> Vec<bool> {
    canvas.cells().map(|(.., ink)| ink != Ink::Clear).collect()
}

/// Each kind has a frame for every pose and beat on both grids, each its grid's size and none
/// empty; the large one is the small one doubled.
#[test]
fn every_kind_has_every_pose_on_both_grids() {
    for kind in Kind::all() {
        for pose in Pose::ALL {
            for beat in every_beat(pose) {
                let small = drawn(kind, pose, beat, Size::Small);
                let large = drawn(kind, pose, beat, Size::Large);
                assert_eq!(small.side(), 8, "{kind:?} {pose:?}");
                assert_eq!(large.side(), 16, "{kind:?} {pose:?}");
                assert!(mask(&small).contains(&true), "{kind:?} {pose:?} {beat:?} draws nothing");
                let opaque = |c: &Canvas| mask(c).iter().filter(|m| **m).count();
                let (s, l) = (opaque(&small), opaque(&large));
                assert!(
                    l >= s.saturating_mul(3) && l <= s.saturating_mul(5),
                    "{kind:?} {pose:?}: {s} cells doubled to {l}"
                );
            }
        }
    }
}

/// The poses read apart: within a kind no two poses draw the same first frame, and a working
/// pose's frames are not all alike (it moves).
#[test]
fn every_pose_looks_its_own() {
    for kind in Kind::all() {
        let firsts: Vec<(Pose, Canvas)> =
            Pose::ALL.iter().map(|p| (*p, frame(kind, *p, Beat::default()))).collect();
        for (i, (a, ca)) in firsts.iter().enumerate() {
            for (b, cb) in firsts.iter().skip(i.saturating_add(1)) {
                assert_ne!(ca, cb, "{kind:?}: {a:?} and {b:?} look the same");
            }
        }
        for task in Task::ALL {
            let pose = Pose::Working(task);
            let frames: Vec<Canvas> = (0..pose.frames())
                .map(|f| frame(kind, pose, Beat { frame: f, ..Beat::default() }))
                .collect();
            assert!(frames.windows(2).any(|w| w[0] != w[1]), "{kind:?} {task:?} never moves");
        }
    }
}

/// No two characters share a silhouette: at rest, the cells one draws and the other does not
/// are at least a quarter of those either draws, so they read apart in greyscale. Two
/// unknown agents' domes differ by their accessories.
#[test]
fn no_two_kinds_share_a_silhouette() {
    let kinds = [Kind::Ember, Kind::Brace, Kind::Pi, Kind::Op, Kind::Blob(0), Kind::Dot];
    for (i, a) in kinds.iter().enumerate() {
        for b in kinds.iter().skip(i.saturating_add(1)) {
            let (ma, mb) = (
                mask(&frame(*a, Pose::Idle, Beat::default())),
                mask(&frame(*b, Pose::Idle, Beat::default())),
            );
            let differ = ma.iter().zip(&mb).filter(|(x, y)| x != y).count();
            let either = ma.iter().zip(&mb).filter(|(x, y)| **x || **y).count();
            assert!(
                differ.saturating_mul(4) >= either,
                "{a:?} and {b:?}: {differ} of {either} cells differ"
            );
        }
    }
    let blobs: Vec<Canvas> = (0..sprites::ACCESSORIES.len())
        .map(|i| frame(Kind::Blob(u8::try_from(i).unwrap()), Pose::Idle, Beat::default()))
        .collect();
    for (i, a) in blobs.iter().enumerate() {
        for b in blobs.iter().skip(i.saturating_add(1)) {
            assert_ne!(a, b, "two accessories look the same");
        }
    }
}

/// A frame is few quads: its rectangles of one ink, at most 24 on the small grid and 96 on the
/// large, whatever the pose; and they cover exactly the cells it draws.
#[test]
fn a_sprite_is_few_quads() {
    let mut most = (0, 0);
    for kind in Kind::all() {
        for pose in Pose::ALL {
            for beat in every_beat(pose) {
                let small = rects(&drawn(kind, pose, beat, Size::Small)).iter().count();
                let large = rects(&drawn(kind, pose, beat, Size::Large)).iter().count();
                assert!(small <= 24, "{kind:?} {pose:?} {beat:?}: {small} quads small");
                assert!(large <= 96, "{kind:?} {pose:?} {beat:?}: {large} quads large");
                most = (most.0.max(small), most.1.max(large));
                let canvas = drawn(kind, pose, beat, Size::Large);
                let mut painted = vec![Ink::Clear; 256];
                for r in rects(&canvas).iter() {
                    for y in r.y..r.y.saturating_add(r.height) {
                        for x in r.x..r.x.saturating_add(r.width) {
                            let cell = &mut painted
                                [usize::from(y).saturating_mul(16).saturating_add(usize::from(x))];
                            assert_eq!(*cell, Ink::Clear, "{kind:?} {pose:?}: painted twice");
                            *cell = r.ink;
                        }
                    }
                }
                let want: Vec<Ink> = canvas.cells().map(|(.., ink)| ink).collect();
                assert_eq!(painted, want, "{kind:?} {pose:?}: the quads are the frame");
            }
        }
    }
    assert!(most.0 > 0 && most.1 > most.0, "{most:?}");
}

/// The cells at a frame's edge: drawn, with nothing beside them on one side.
fn edge(canvas: &Canvas) -> Vec<Ink> {
    let at = |x: usize, y: usize, dx: isize, dy: isize| match (
        x.checked_add_signed(dx),
        y.checked_add_signed(dy),
    ) {
        (Some(x), Some(y)) => canvas.at(x, y),
        _ => Ink::Clear,
    };
    canvas
        .cells()
        .filter(|(x, y, ink)| {
            *ink != Ink::Clear
                && [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .any(|(dx, dy)| at(*x, *y, *dx, *dy) == Ink::Clear)
        })
        .map(|(.., ink)| ink)
        .collect()
}

/// Every companion reads on the planes it stands on, light and dark, at either contrast: most
/// of its silhouette's edge reaches 3:1 (WCAG's least for a graphic) against the content and
/// the panel. pi keeps its own mark's colours, as its mark in the navigator does; its blue
/// legs carry its edge. Under Increase Contrast the outline is the text itself.
#[test]
fn the_companions_read_on_every_plane() {
    for variant in [Variant::Dark, Variant::Light] {
        for contrast in [Contrast::Standard, Contrast::Increased] {
            let mut theme = Theme::new(variant);
            theme.contrast = contrast;
            theme.derive_chrome();
            // Dot is the brand's mark come alive, in the mark's own fixed green and unlit level,
            // as the mark beside it is drawn (`docs/decisions/brand.md`).
            for kind in Kind::all().filter(|k| *k != Kind::Dot) {
                let palette = Palette::of(&theme, kind, Pose::Idle);
                if contrast == Contrast::Increased {
                    assert_eq!(palette.outline, theme.surfaces.text, "{kind:?}");
                }
                let rim = edge(&frame(kind, Pose::Idle, Beat::default()));
                for plane in [theme.content(), theme.surfaces.panel] {
                    let reads = rim
                        .iter()
                        .filter(|ink| palette.rgb(**ink).is_some_and(|c| c.contrast(plane) >= 3.0))
                        .count();
                    let least = if kind == Kind::Pi { rim.len() / 3 } else { rim.len() / 2 };
                    assert!(
                        reads >= least,
                        "{kind:?} on {variant:?} {contrast:?}: {reads} of {} edge cells read",
                        rim.len()
                    );
                }
                let eye = palette.eye.contrast(palette.body);
                assert!(eye >= 2.0, "{kind:?} {variant:?}: eyes at {eye:.2}");
            }
        }
    }
}

/// An agent's companion is its kind's, directly or over ACP; any other agent's is a dome whose
/// accessory its name picks, the same on every run and machine.
#[test]
fn an_unknown_agent_keeps_its_accessory() {
    assert_eq!(Kind::of(AgentId::CLAUDE_CODE), Kind::Ember);
    assert_eq!(Kind::of(AgentId::CODEX), Kind::Brace);
    assert_eq!(Kind::of(AgentId::PI), Kind::Pi);
    assert_eq!(Kind::of("acp:opencode"), Kind::Op);
    assert_eq!(Kind::of("acp:gemini"), Kind::of("gemini"));
    // FNV-1a's published value, so the hash is the same on every build.
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c, "FNV-1a of \"a\"");
    let names = ["gemini", "goose", "cursor", "amp", "kimi", "qwen", "aider", "droid"];
    let worn: std::collections::HashSet<Kind> = names.iter().map(|n| Kind::of(n)).collect();
    assert!(worn.len() >= 4, "eight agents wear only {worn:?}");
}

/// A working companion takes its task from its newest running call's kind, which is open: a
/// kind it does not know types.
#[test]
fn a_working_companion_works_at_its_call() {
    assert_eq!(Task::of(kind::READ), Task::Reading);
    assert_eq!(Task::of(kind::WEB_SEARCH), Task::Reading);
    assert_eq!(Task::of(kind::EDIT), Task::Typing);
    assert_eq!(Task::of(kind::EXEC), Task::Running);
    assert_eq!(Task::of(kind::AGENT), Task::Delegating);
    assert_eq!(Task::of(kind::TASKS), Task::Listing);
    assert_eq!(Task::of("teleport"), Task::Typing);
}

/// A pose follows the one vocabulary of states: a mark's status, or a thread's rung and
/// liveness.
#[test]
fn a_pose_says_the_state() {
    assert_eq!(Pose::of_status(None), Pose::Idle);
    assert_eq!(Pose::of_status(Some(Status::Working)), Pose::Working(Task::Typing));
    assert_eq!(Pose::of_status(Some(Status::Running)), Pose::Waiting);
    assert_eq!(Pose::of_status(Some(Status::NeedsYou)), Pose::NeedsYou);
    assert_eq!(Pose::of_status(Some(Status::Away)), Pose::Gone);
    let live = Liveness::Live;
    assert_eq!(Pose::of_thread(Rung::Working, live, Task::Reading), Pose::Working(Task::Reading));
    let sleeping = Liveness::Sleeping { until_ms: slopty_core::WallMs::from_millis(1) };
    assert_eq!(Pose::of_thread(Rung::Waiting, sleeping, Task::Typing), Pose::Asleep);
    let exited = Liveness::Exited { resumable: true };
    assert_eq!(Pose::of_thread(Rung::Idle, exited, Task::Typing), Pose::Gone);
    let ranks: Vec<u8> =
        [Rung::NeedsYou, Rung::Failed, Rung::ToReview, Rung::Working, Rung::Waiting, Rung::Idle]
            .iter()
            .map(|r| Pose::of_thread(*r, live, Task::Typing).rank())
            .collect();
    assert!(ranks.windows(2).all(|w| w[0] > w[1]), "a crowd follows the ladder: {ranks:?}");
}

// ---------------------------------------------------------------------------------------------
// Headless: the frames a companion asks the window for.
// ---------------------------------------------------------------------------------------------

/// A view of companions, counting how often it is built.
struct Yard {
    theme: Theme,
    poses: Vec<Pose>,
    builds: Rc<Cell<u64>>,
}

impl Render for Yard {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.builds.set(self.builds.get().saturating_add(1));
        div().size_full().flex().children(self.poses.iter().enumerate().map(|(i, pose)| {
            companion(&self.theme, Kind::Ember, *pose).one_shots(("companion", i)).side(px(16.0))
        }))
    }
}

fn yard(
    cx: &mut TestAppContext,
    mode: Companions,
    poses: Vec<Pose>,
) -> (Entity<Yard>, Rc<Cell<u64>>, &mut VisualTestContext) {
    let builds = Rc::new(Cell::new(0));
    let counted = Rc::clone(&builds);
    let mut theme = Theme::default();
    theme.behaviour.companions = mode;
    let (view, cx) = cx.add_window_view(move |_w, _cx| Yard { theme, poses, builds: counted });
    cx.run_until_parked();
    (view, builds, cx)
}

/// The view's builds in one simulated second at 120 Hz.
fn frames_in_a_second(builds: &Rc<Cell<u64>>, cx: &mut VisualTestContext) -> u64 {
    let before = builds.get();
    for _ in 0..120 {
        cx.executor().advance_clock(Duration::from_nanos(8_333_333));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    }
    builds.get().saturating_sub(before)
}

fn set(view: &Entity<Yard>, cx: &mut VisualTestContext, poses: Vec<Pose>) {
    view.update(cx, |v, cx| {
        v.poses = poses;
        cx.notify();
    });
    cx.run_until_parked();
}

/// A working companion draws on the working mark's steps: twelve frames a second, lively or
/// quiet, the spinner's own count, and none once it rests.
#[gpui::test]
fn a_working_companion_draws_on_the_working_marks_steps(cx: &mut TestAppContext) {
    for mode in [Companions::Quiet, Companions::Lively] {
        let (view, builds, cx) = yard(cx, mode, vec![Pose::Idle; 3]);
        assert_eq!(frames_in_a_second(&builds, cx), 0, "{mode:?}: at rest, nothing draws");
        set(&view, cx, vec![Pose::Working(Task::Typing), Pose::Idle, Pose::Working(Task::Reading)]);
        let working = frames_in_a_second(&builds, cx);
        assert!((11..=13).contains(&working), "{mode:?}: {working} frames in a second");
        set(&view, cx, vec![Pose::Idle; 3]);
        // Lively, the work ending is a hop: half a second of steps, then nothing.
        assert!(frames_in_a_second(&builds, cx) <= 7, "{mode:?}: the last step, a hop at most");
        assert_eq!(frames_in_a_second(&builds, cx), 0, "{mode:?}: then nothing");
    }
}

/// Lively, needs-you waves for two seconds at three frames a second, then holds the arm up and
/// draws nothing; quiet, it only raises the arm.
#[gpui::test]
fn needs_you_waves_for_two_seconds_then_holds(cx: &mut TestAppContext) {
    let (view, builds, cx) = yard(cx, Companions::Lively, vec![Pose::Idle]);
    set(&view, cx, vec![Pose::NeedsYou]);
    let seconds: Vec<u64> =
        std::iter::repeat_with(|| frames_in_a_second(&builds, cx)).take(3).collect();
    // The change's own frame, then one at each of the wave's six beats.
    assert_eq!(seconds.iter().sum::<u64>(), 7, "{seconds:?}");
    assert_eq!(frames_in_a_second(&builds, cx), 0, "the arm stays up, still");

    let (view, builds, cx) = yard(cx, Companions::Quiet, vec![Pose::Idle]);
    set(&view, cx, vec![Pose::NeedsYou]);
    assert_eq!(frames_in_a_second(&builds, cx), 0, "quiet: no wave");
}

/// A companion that only comes into view in its new state plays no moment: there was no state
/// before it on screen.
#[gpui::test]
fn a_moment_is_never_replayed_for_a_companion_scrolled_to(cx: &mut TestAppContext) {
    let (view, builds, cx) = yard(cx, Companions::Lively, Vec::new());
    set(&view, cx, vec![Pose::NeedsYou]);
    assert_eq!(frames_in_a_second(&builds, cx), 0, "it arrived waiting: no wave");
}

/// Under Reduce Motion every companion holds its pose: a working one breathes as the working
/// mark does (five frames a second), and a change of state plays nothing.
#[gpui::test]
fn reduce_motion_holds_every_pose(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, builds, cx) = yard(cx, Companions::Lively, vec![Pose::Idle]);
    set(&view, cx, vec![Pose::NeedsYou]);
    assert_eq!(frames_in_a_second(&builds, cx), 0, "no wave");
    set(&view, cx, vec![Pose::Working(Task::Typing)]);
    let breathing = frames_in_a_second(&builds, cx);
    assert!((4..=6).contains(&breathing), "{breathing} breaths in a second");
}

/// Off, a companion draws nothing and asks for no frame.
#[gpui::test]
fn off_draws_and_asks_nothing(cx: &mut TestAppContext) {
    let (view, builds, cx) = yard(cx, Companions::Off, vec![Pose::Idle]);
    set(&view, cx, vec![Pose::Working(Task::Typing)]);
    assert_eq!(frames_in_a_second(&builds, cx), 0, "off");
}

/// The sheet, as text, for a review of the drawing:
/// `cargo nextest run -p slopty-ui --lib -E 'test(companion_sheet)' --run-ignored only
/// --no-capture`.
#[test]
#[ignore = "prints the sheet for a review; asserts nothing"]
fn companion_sheet() {
    let glyph = |ink: Ink| match ink {
        Ink::Clear | Ink::Erase => ' ',
        Ink::Body => '#',
        Ink::Shade => '=',
        Ink::Accent => '*',
        Ink::Outline => 'o',
        Ink::Eye => '@',
        Ink::Glint => '\'',
        Ink::Prop => '%',
        Ink::Cue => '!',
        Ink::Muted => '.',
    };
    for size in [Size::Small, Size::Large] {
        for kind in Kind::all().take(6) {
            let frames: Vec<(Pose, Canvas)> =
                Pose::ALL.iter().map(|p| (*p, drawn(kind, *p, Beat::default(), size))).collect();
            eprintln!("{kind:?}");
            for chunk in frames.chunks(8) {
                let side = size.cells();
                for y in 0..side {
                    let line: Vec<String> = chunk
                        .iter()
                        .map(|(_, c)| (0..side).map(|x| glyph(c.at(x, y))).collect::<String>())
                        .collect();
                    eprintln!("{}", line.join(" | "));
                }
                eprintln!();
            }
        }
    }
}
