//! One status mark that morphs between a run's states (Clanker UI's `StatusMark`;
//! `.research/design-inspiration-2026-10-19.md` item 1), in place of a check, a spinner and a dot
//! swapped on a hard cut:
//!
//! - **Working**: a ring of six dashes (36° on, 24° off) turning once every 2 s on the shared pulse
//!   clock, so every working mark on screen turns in step (and calms as a long wait does).
//! - **Needs you**, in amber, its glyph naming the need (`docs/decisions/layout.md`, "A state is
//!   a glyph"): a `?` drawn in the closed ring for a question, a raised hand for an approval, a
//!   checklist for a plan to review, and a dot for any other need, each drawn in over 260 ms as the
//!   ring closes or fades.
//! - **Done**: the ring fades over 200 ms while a disc grows in from 0.6 of its size over 220 ms,
//!   and the tick is drawn through it over 240 ms, 80 ms in, in the colour of the ground beneath.
//! - **Failed**: a whole ring in red, its `!` drawn over 200 ms, 60 ms in.
//!
//! Every move is on [`motion::EASE_OUT_QUINT`] and the colour crossfades over 150 ms. Under
//! reduced motion each state is drawn at once and the working ring stands still with its gaps.
//! Its hues are the one table of the session states (`shell::marks`): at work blue, waiting
//! amber, done green and failed red, each on light in the tone of itself that clears 3:1.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use gpui::{
    App, ElementId, Hsla, IntoElement, ParentElement as _, PathBuilder, Pixels, Point, RenderOnce,
    Styled as _, Window, canvas, div, point, px,
};

use crate::motion::{self, EASE_OUT_QUINT, MotionSpec};
use crate::theme::Theme;

/// What a status mark says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkState {
    /// The agent is at work.
    Working,
    /// It waits on the person for what its records name no glyph for: an amber dot.
    Waiting,
    /// It asks the person a question: a `?` in an amber ring.
    Question,
    /// It needs the person's approval for an action: a raised hand.
    Approval,
    /// It has a plan for the person to review: a checklist.
    Plan,
    /// Finished.
    Done,
    /// Ended in an error.
    Failed,
}

/// How long the ring takes to close or open, and to fade as a run finishes.
const RING: Duration = Duration::from_millis(200);
/// How long the done disc takes to grow in.
const DISC: Duration = Duration::from_millis(220);
/// The tick's draw, and how far into the change it starts.
const TICK: Duration = Duration::from_millis(240);
const TICK_DELAY: Duration = Duration::from_millis(80);
/// The `!`'s draw, and how far in it starts; a `?` draws on the same.
const BANG: Duration = Duration::from_millis(200);
const BANG_DELAY: Duration = Duration::from_millis(60);
/// A hand's or a checklist's draw, and how far in it starts.
const GLYPH: Duration = Duration::from_millis(260);
const GLYPH_DELAY: Duration = Duration::from_millis(60);
/// The radius of the done disc and of the needs-you dot, on the 24 grid: the dot is T3 Code's,
/// two thirds of its slot.
const DISC_RADIUS: f32 = 10.0;
const DOT_RADIUS: f32 = 6.5;
/// The colour's crossfade.
const HUE: Duration = Duration::from_millis(150);
/// The whole of a change, after which nothing moves but a working ring.
const SETTLE: Duration = Duration::from_millis(320);
/// One turn of the working ring.
pub const MARK_SPIN: MotionSpec = MotionSpec::new(2000, motion::EASE);

/// The size of a mark in a sidebar row, and in a tray row or a session lane.
pub const MARK_SMALL: f32 = 12.0;
pub const MARK_LARGE: f32 = 15.0;

/// A status mark, drawn `size` square. It remembers, under `id`, the state it drew and when that
/// changed, and morphs from it.
#[derive(IntoElement)]
pub struct StatusMark {
    id: ElementId,
    state: MarkState,
    size: f32,
    ground: Option<Hsla>,
}

impl StatusMark {
    pub fn new(id: impl Into<ElementId>, state: MarkState) -> Self {
        Self { id: id.into(), state, size: MARK_LARGE, ground: None }
    }

    /// Its size in points ([`MARK_SMALL`], [`MARK_LARGE`]).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// The colour beneath it, which the done tick is drawn in, as if cut through the disc;
    /// the theme's page colour when unset.
    pub fn ground(mut self, ground: Hsla) -> Self {
        self.ground = Some(ground);
        self
    }
}

/// What a mark remembers between frames.
#[derive(Clone, Copy)]
struct Memory {
    state: MarkState,
    from: Option<MarkState>,
    at: Instant,
}

/// Everything a mark paints at one moment, worked out before painting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MarkFrame {
    /// 0 a ring of six dashes, 1 a whole ring.
    pub closed: f32,
    /// The ring's turn, 0..1.
    pub turn: f32,
    pub ring_opacity: f32,
    /// The disc's radius on the 24 grid (the done disc's or the needs-you dot's), its scale
    /// (0.6..1) and its opacity.
    pub disc_radius: f32,
    pub disc_scale: f32,
    pub disc_opacity: f32,
    /// How much of the tick, the `!`, the `?`, the hand and the checklist is drawn, 0..1.
    pub tick: f32,
    pub bang: f32,
    pub ask: f32,
    pub hand: f32,
    pub plan: f32,
    pub color: Hsla,
}

/// The disc a state is drawn as, by its radius on the 24 grid, if it is one.
fn disc_of(state: MarkState) -> Option<f32> {
    match state {
        MarkState::Done => Some(DISC_RADIUS),
        MarkState::Waiting => Some(DOT_RADIUS),
        _ => None,
    }
}

/// The colour a state is drawn in: the one hue table every surface's mark reads
/// (`shell::marks::hue`).
fn hue(state: MarkState, theme: &Theme) -> Hsla {
    crate::shell::marks::hue(state, theme)
}

/// Eased progress `elapsed` into a move of `duration` that starts `delay` in; whole under
/// reduced motion.
fn eased(elapsed: Duration, delay: Duration, duration: Duration, still: bool) -> f32 {
    if still {
        return 1.0;
    }
    let t = elapsed.saturating_sub(delay).as_secs_f32() / duration.as_secs_f32();
    EASE_OUT_QUINT.eval(t.clamp(0.0, 1.0))
}

/// What a mark that changed from `from` to `to` `elapsed` ago paints, turned to `turn`.
pub(crate) fn mark_frame(
    from: Option<MarkState>,
    to: MarkState,
    elapsed: Duration,
    turn: f32,
    still: bool,
    theme: &Theme,
) -> MarkFrame {
    let ring = eased(elapsed, Duration::ZERO, RING, still);
    let closed_of = |state| if state == MarkState::Working { 0.0 } else { 1.0 };
    // The ring stands for at work, a failure and a question; a disc, a hand and a checklist
    // take its place.
    let shown_of = |state| match state {
        MarkState::Working | MarkState::Failed | MarkState::Question => 1.0,
        MarkState::Waiting | MarkState::Approval | MarkState::Plan | MarkState::Done => 0.0,
    };
    let from_state = from.unwrap_or(to);
    let disc = eased(elapsed, Duration::ZERO, DISC, still);
    let disc_from = if disc_of(from_state).is_some() { 1.0 } else { 0.0 };
    let disc_to = if disc_of(to).is_some() { 1.0 } else { 0.0 };
    let disc_progress = if from.is_some() { disc } else { 1.0 };
    let disc_t = motion::lerp(disc_from, disc_to, disc_progress);
    // A dot that fills to the done disc grows to it; a disc that comes or goes keeps its size.
    let disc_radius = match (disc_of(from_state), disc_of(to)) {
        (Some(from), Some(to)) => motion::lerp(from, to, disc_progress),
        (_, Some(radius)) | (Some(radius), None) => radius,
        (None, None) => DISC_RADIUS,
    };
    let settled = from.is_none();
    // How much of `state`'s own glyph is drawn: whole once settled, else from `delay` over
    // `duration`; nothing for another state.
    let drawn = |state, delay, duration| match (to == state, settled) {
        (true, true) => 1.0,
        (true, false) => eased(elapsed, delay, duration, still),
        (false, _) => 0.0,
    };
    MarkFrame {
        closed: motion::lerp(
            closed_of(from_state),
            closed_of(to),
            if settled { 1.0 } else { ring },
        ),
        turn: if still { 0.0 } else { turn },
        ring_opacity: motion::lerp(
            shown_of(from_state),
            shown_of(to),
            if settled { 1.0 } else { ring },
        ),
        disc_radius,
        disc_scale: 0.6 + 0.4 * disc_t,
        disc_opacity: disc_t,
        tick: drawn(MarkState::Done, TICK_DELAY, TICK),
        bang: drawn(MarkState::Failed, BANG_DELAY, BANG),
        ask: drawn(MarkState::Question, BANG_DELAY, BANG),
        hand: drawn(MarkState::Approval, GLYPH_DELAY, GLYPH),
        plan: drawn(MarkState::Plan, GLYPH_DELAY, GLYPH),
        color: motion::mix(
            hue(from_state, theme),
            hue(to, theme),
            if settled { 1.0 } else { eased(elapsed, Duration::ZERO, HUE, still) },
        ),
    }
}

impl RenderOnce for StatusMark {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let now = window.frame_time();
        let state = self.state;
        let memory = window.with_global_id(self.id.clone(), |id, window| {
            window.with_element_state(id, |previous: Option<Memory>, _| {
                let memory = match previous {
                    Some(memory) if memory.state == state => memory,
                    Some(memory) => Memory { state, from: Some(memory.state), at: now },
                    // A mark drawn for the first time shows its state, with no change to play.
                    None => Memory { state, from: None, at: now },
                };
                (memory, memory)
            })
        });
        let still = cx.reduce_motion();
        let elapsed = now.saturating_duration_since(memory.at);
        let changing = memory.from.is_some() && elapsed < SETTLE && !still;
        // The working ring turns on the shared pulse clock (in step, ~30 Hz, calming over a long
        // wait); a change runs on the frame clock until it settles.
        let turn = if state == MarkState::Working && !still {
            motion::pulse_delta_slow(&MARK_SPIN, window.current_view(), cx)
        } else {
            0.0
        };
        if changing {
            window.request_animation_frame();
        }
        let theme = Theme::of(cx);
        let frame =
            mark_frame(memory.from.filter(|_| changing), state, elapsed, turn, still, theme);
        let ground = self.ground.unwrap_or(theme.bg);
        let size = self.size;
        div().size(px(size)).flex_none().child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| paint(frame, ground, size, bounds.center(), window),
            )
            .size_full(),
        )
    }
}

/// Paints `frame` centred on `center`, `size` square.
fn paint(frame: MarkFrame, ground: Hsla, size: f32, center: Point<Pixels>, window: &mut Window) {
    let unit = size / 24.0;
    let at = |radius: f32, theta: f32| {
        point(center.x + px(radius * theta.cos()), center.y + px(radius * theta.sin()))
    };
    if frame.ring_opacity > 0.0 {
        let radius = 9.0 * unit;
        let color = frame.color.opacity(frame.ring_opacity);
        // Six dashes of 36° and gaps of 24° that close to nothing; whole, one circle.
        let (dashes, dash) = if frame.closed >= 1.0 {
            (1, TAU)
        } else {
            (6, (36.0 + 24.0 * frame.closed).to_radians())
        };
        for k in 0..dashes {
            let start = -FRAC_PI_2 + TAU * frame.turn + k as f32 * TAU / 6.0;
            let steps = ((dash / TAU) * 48.0).ceil().max(2.0) as usize;
            let mut path = PathBuilder::stroke(px(2.4 * unit));
            path.move_to(at(radius, start));
            for step in 1..=steps {
                path.line_to(at(radius, start + dash * step as f32 / steps as f32));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
    if frame.disc_opacity > 0.0 {
        let radius = frame.disc_radius * unit * frame.disc_scale;
        let mut disc = PathBuilder::fill();
        disc.move_to(at(radius, 0.0));
        for step in 1..=48 {
            disc.line_to(at(radius, TAU * step as f32 / 48.0));
        }
        disc.close();
        if let Ok(path) = disc.build() {
            window.paint_path(path, frame.color.opacity(frame.disc_opacity));
        }
    }
    let local =
        |x: f32, y: f32| point(center.x + px((x - 12.0) * unit), center.y + px((y - 12.0) * unit));
    if frame.tick > 0.0 {
        // `M9 12 l2 2 l4 -4`, drawn along its length.
        stroke_part(
            &[local(9.0, 12.0), local(11.0, 14.0), local(15.0, 10.0)],
            frame.tick,
            2.0 * unit,
            ground,
            window,
        );
    }
    if frame.bang > 0.0 {
        // `M12 8 v4`, then the point at 16.
        stroke_part(
            &[local(12.0, 7.5), local(12.0, 12.5)],
            frame.bang,
            2.4 * unit,
            frame.color,
            window,
        );
        dot(local(12.0, 16.0), 1.3 * unit * frame.bang, frame.color, window);
    }
    let glyph = |strokes: Vec<Vec<(f32, f32)>>, share: f32, width: f32, window: &mut Window| {
        for stroke in strokes {
            let points: Vec<_> = stroke.into_iter().map(|(x, y)| local(x, y)).collect();
            stroke_part(&points, share, width * unit, frame.color, window);
        }
    };
    if frame.ask > 0.0 {
        glyph(ask(), frame.ask, 2.4, window);
        dot(local(12.0, 17.0), 1.3 * unit * frame.ask, frame.color, window);
    }
    if frame.hand > 0.0 {
        glyph(hand(), frame.hand, 2.0, window);
    }
    if frame.plan > 0.0 {
        glyph(plan(), frame.plan, 2.0, window);
    }
}

/// Fills a dot of `radius` at `middle`.
fn dot(middle: Point<Pixels>, radius: f32, color: Hsla, window: &mut Window) {
    let mut path = PathBuilder::fill();
    path.move_to(point(middle.x + px(radius), middle.y));
    for step in 1..=16 {
        let theta = TAU * step as f32 / 16.0;
        path.line_to(point(middle.x + px(radius * theta.cos()), middle.y + px(radius * theta.sin())));
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// The points of an arc about `center` of `radius`, from `from`° to `to`° (clockwise on screen
/// as the angle grows), a point every 15° or so, its first point left out (the stroke is there).
fn arc(points: &mut Vec<(f32, f32)>, center: (f32, f32), radius: f32, from: f32, to: f32) {
    let steps = ((to - from).abs() / 15.0).ceil().max(2.0) as usize;
    for step in 1..=steps {
        let theta = (from + (to - from) * step as f32 / steps as f32).to_radians();
        points.push((center.0 + radius * theta.cos(), center.1 + radius * theta.sin()));
    }
}

/// The points of a cubic from the last point of `points` through `a` and `b` to `to`.
fn cubic(points: &mut Vec<(f32, f32)>, a: (f32, f32), b: (f32, f32), to: (f32, f32)) {
    let from = *points.last().expect("a cubic goes on from a point");
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let u = 1.0 - t;
        let at = |p: f32, q: f32, r: f32, s: f32| {
            u * u * u * p + 3.0 * u * u * t * q + 3.0 * u * t * t * r + t * t * t * s
        };
        points.push((at(from.0, a.0, b.0, to.0), at(from.1, a.1, b.1, to.1)));
    }
}

/// A question's `?` on the 24 grid, inside the ring: Lucide's `circle-help` hook
/// (`M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3`); its point is drawn apart.
fn ask() -> Vec<Vec<(f32, f32)>> {
    let mut hook = vec![(9.09, 9.0)];
    arc(&mut hook, (11.92, 10.0), 3.0, 199.5, 360.0);
    cubic(&mut hook, (14.92, 12.0), (11.92, 13.0), (11.92, 13.0));
    vec![hook]
}

/// A raised hand on the 24 grid: Lucide's `hand`, three fingers, then the fourth down the palm
/// and round to the thumb.
fn hand() -> Vec<Vec<(f32, f32)>> {
    // Up the finger's right side at `x`, round its tip at `top`, and down its left to `end`.
    let finger = |x: f32, top: f32, bottom: f32, end: f32| {
        let mut points = vec![(x, bottom), (x, top)];
        arc(&mut points, (x - 2.0, top), 2.0, 0.0, -180.0);
        if end > top {
            points.push((x - 4.0, end));
        }
        points
    };
    let mut palm = vec![(18.0, 8.0)];
    arc(&mut palm, (20.0, 8.0), 2.0, 180.0, 360.0);
    palm.push((22.0, 14.0));
    arc(&mut palm, (14.0, 14.0), 8.0, 0.0, 90.0);
    palm.push((12.0, 22.0));
    cubic(&mut palm, (9.2, 22.0), (7.5, 21.14), (6.01, 19.66));
    palm.push((2.41, 16.06));
    arc(&mut palm, (3.825, 14.65), 2.0, 135.0, 315.0);
    palm.push((7.0, 15.0));
    vec![finger(18.0, 6.0, 11.0, 6.0), finger(14.0, 4.0, 10.0, 6.0), finger(10.0, 6.0, 10.5, 14.0), palm]
}

/// A plan's checklist on the 24 grid: Lucide's `list-checks`, two ticked rows and three lines.
fn plan() -> Vec<Vec<(f32, f32)>> {
    vec![
        vec![(3.0, 7.0), (5.0, 9.0), (9.0, 5.0)],
        vec![(3.0, 17.0), (5.0, 19.0), (9.0, 15.0)],
        vec![(13.0, 5.0), (21.0, 5.0)],
        vec![(13.0, 12.0), (21.0, 12.0)],
        vec![(13.0, 19.0), (21.0, 19.0)],
    ]
}

/// Strokes the first `share` of the polyline through `points`, by length.
pub(super) fn stroke_part(
    points: &[Point<Pixels>],
    share: f32,
    width: f32,
    color: Hsla,
    window: &mut Window,
) {
    let length =
        |a: Point<Pixels>, b: Point<Pixels>| f32::from(b.x - a.x).hypot(f32::from(b.y - a.y));
    let total: f32 = points.windows(2).map(|pair| length(pair[0], pair[1])).sum();
    let mut left = total * share.clamp(0.0, 1.0);
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(points[0]);
    for pair in points.windows(2) {
        let span = length(pair[0], pair[1]);
        if left <= 0.0 || span <= 0.0 {
            break;
        }
        let t = (left / span).min(1.0);
        let (a, b) = (pair[0], pair[1]);
        path.line_to(point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        left -= span;
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn frame(from: MarkState, to: MarkState, ms: u64) -> MarkFrame {
        mark_frame(Some(from), to, MS(ms), 0.25, false, &Theme::dark())
    }

    /// Working to a question: the six dashes close to a ring over 200 ms, in amber by 150, and
    /// its `?` draws in from 60 ms.
    #[test]
    fn a_working_ring_closes_round_a_question() {
        let theme = Theme::dark();
        assert_eq!(frame(MarkState::Working, MarkState::Question, 0).closed, 0.0);
        let mid = frame(MarkState::Working, MarkState::Question, 120);
        assert!(mid.closed > 0.5 && mid.closed < 1.0, "most of the way at 120 ms: {}", mid.closed);
        assert!(mid.ask > 0.0 && mid.ask < 1.0, "the `?` part drawn at 120 ms: {}", mid.ask);
        assert_eq!(frame(MarkState::Working, MarkState::Question, 50).ask, 0.0, "not before 60 ms");
        let settled = frame(MarkState::Working, MarkState::Question, 400);
        assert_eq!((settled.closed, settled.ring_opacity, settled.ask), (1.0, 1.0, 1.0));
        assert_eq!(settled.color, hue(MarkState::Question, &theme));
        assert_eq!((settled.disc_opacity, settled.bang, settled.hand, settled.plan), (0.0, 0.0, 0.0, 0.0));
    }

    /// Each need draws its own glyph in amber where the ring was (`docs/decisions/layout.md`, "A
    /// state is a glyph"): an approval a hand and a plan a checklist, the ring fading as they
    /// draw in; any other need a dot, smaller than the done disc, that fills to it when the run
    /// finishes.
    #[test]
    fn a_need_draws_its_own_glyph() {
        let theme = Theme::dark();
        for (need, drawn) in [
            (MarkState::Approval, (|frame: MarkFrame| frame.hand) as fn(MarkFrame) -> f32),
            (MarkState::Plan, |frame| frame.plan),
        ] {
            let mid = frame(MarkState::Working, need, 160);
            assert!(mid.ring_opacity < 0.5, "{need:?}: the ring fades as the glyph comes");
            assert!(drawn(mid) > 0.0 && drawn(mid) < 1.0, "{need:?} drawn part way: {}", drawn(mid));
            let end = frame(MarkState::Working, need, 400);
            assert_eq!((end.ring_opacity, drawn(end), end.disc_opacity, end.ask), (0.0, 1.0, 0.0, 0.0));
            assert_eq!(end.color, hue(MarkState::Waiting, &theme), "{need:?} is needs you's amber");
        }
        let dot = frame(MarkState::Working, MarkState::Waiting, 400);
        assert_eq!((dot.ring_opacity, dot.disc_opacity, dot.disc_radius), (0.0, 1.0, DOT_RADIUS));
        assert_eq!((dot.tick, dot.ask, dot.hand, dot.plan), (0.0, 0.0, 0.0, 0.0));
        assert!(DOT_RADIUS < DISC_RADIUS * 0.7, "a dot reads apart from the done disc");
        let filling = frame(MarkState::Waiting, MarkState::Done, 100).disc_radius;
        assert!(filling > DOT_RADIUS && filling < DISC_RADIUS, "the dot grows to the disc: {filling}");
        assert_eq!(frame(MarkState::Waiting, MarkState::Done, 400).disc_radius, DISC_RADIUS);
        // A glyph whole on its first draw, and its strokes inside the mark's square.
        let first = mark_frame(None, MarkState::Approval, MS(0), 0.0, false, &theme);
        assert_eq!((first.hand, first.ring_opacity), (1.0, 0.0));
        for strokes in [ask(), hand(), plan()] {
            for (x, y) in strokes.into_iter().flatten() {
                assert!((1.0..=23.0).contains(&x) && (1.0..=23.0).contains(&y), "({x}, {y}) in the square");
            }
        }
    }

    /// Working to done: the ring fades, the disc grows in from 0.6, the tick waits 80 ms, then
    /// draws to its end by 320.
    #[test]
    fn a_finished_run_fills_to_a_disc_and_draws_its_tick() {
        let start = frame(MarkState::Working, MarkState::Done, 0);
        assert_eq!(
            (start.ring_opacity, start.disc_scale, start.disc_opacity, start.tick),
            (1.0, 0.6, 0.0, 0.0)
        );
        let mid = frame(MarkState::Working, MarkState::Done, 120);
        assert!(mid.ring_opacity < 0.5 && mid.disc_opacity > 0.5 && mid.disc_scale > 0.8);
        assert!(mid.tick > 0.0 && mid.tick < 1.0, "the tick part drawn at 120 ms: {}", mid.tick);
        assert_eq!(frame(MarkState::Working, MarkState::Done, 60).tick, 0.0, "not before 80 ms");
        let end = frame(MarkState::Working, MarkState::Done, 400);
        assert_eq!(
            (end.ring_opacity, end.disc_scale, end.disc_opacity, end.tick),
            (0.0, 1.0, 1.0, 1.0)
        );
    }

    /// A failure closes the ring in red and draws its `!` from 60 ms.
    #[test]
    fn a_failure_closes_the_ring_and_draws_its_bang() {
        assert_eq!(frame(MarkState::Working, MarkState::Failed, 50).bang, 0.0);
        assert!(frame(MarkState::Working, MarkState::Failed, 120).bang > 0.0);
        let end = frame(MarkState::Working, MarkState::Failed, 400);
        assert_eq!((end.closed, end.bang, end.tick), (1.0, 1.0, 0.0));
        assert_eq!(end.color, hue(MarkState::Failed, &Theme::dark()));
    }

    /// Back to work from done: the disc goes and the dashes come back.
    #[test]
    fn a_run_resumed_opens_its_ring_again() {
        let end = frame(MarkState::Done, MarkState::Working, 400);
        assert_eq!(
            (end.closed, end.ring_opacity, end.disc_opacity, end.tick),
            (0.0, 1.0, 0.0, 0.0)
        );
    }

    /// Under reduced motion every state is whole at once and the working ring does not turn;
    /// a mark drawn for the first time shows its state with nothing to play.
    #[test]
    fn reduced_motion_and_a_first_draw_show_the_state_at_once() {
        let still =
            mark_frame(Some(MarkState::Working), MarkState::Done, MS(0), 0.4, true, &Theme::dark());
        assert_eq!((still.disc_opacity, still.tick, still.turn), (1.0, 1.0, 0.0));
        let first = mark_frame(None, MarkState::Done, MS(0), 0.0, false, &Theme::dark());
        assert_eq!((first.disc_opacity, first.tick, first.ring_opacity), (1.0, 1.0, 0.0));
        let working = mark_frame(None, MarkState::Working, MS(0), 0.4, true, &Theme::dark());
        assert_eq!((working.closed, working.turn), (0.0, 0.0));
    }

    /// On light every state's hue clears 3:1 against the page.
    #[test]
    fn every_state_reads_on_light() {
        let theme = Theme::light();
        for state in [
            MarkState::Working,
            MarkState::Waiting,
            MarkState::Question,
            MarkState::Approval,
            MarkState::Plan,
            MarkState::Done,
            MarkState::Failed,
        ] {
            let contrast = crate::theme::contrast(hue(state, &theme), theme.bg);
            assert!(contrast >= crate::theme::SPINNER_CONTRAST - 0.01, "{state:?}: {contrast}");
        }
    }
}
