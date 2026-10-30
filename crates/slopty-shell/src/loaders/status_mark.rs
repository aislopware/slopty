//! slopty: one status mark that morphs between a run's states (Clanker UI's `StatusMark`;
//! `.research/design-inspiration-2026-10-19.md` item 1), in place of a check, a spinner and a dot
//! swapped on a hard cut:
//!
//! - **Working**: a ring of six dashes (36° on, 24° off) turning once every 2 s on the shared pulse
//!   clock, so every working mark on screen turns in step (and calms as a long wait does).
//! - **Waiting** (the person is needed): the gaps close to a whole ring over 200 ms, in amber.
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
    /// It waits on the person: a question, an approval.
    Waiting,
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
/// The `!`'s draw, and how far in it starts.
const BANG: Duration = Duration::from_millis(200);
const BANG_DELAY: Duration = Duration::from_millis(60);
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
    /// The disc's scale (0.6..1) and opacity.
    pub disc_scale: f32,
    pub disc_opacity: f32,
    /// How much of the tick, and of the `!`, is drawn, 0..1.
    pub tick: f32,
    pub bang: f32,
    pub color: Hsla,
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
    let shown_of = |state| if state == MarkState::Done { 0.0 } else { 1.0 };
    let from_state = from.unwrap_or(to);
    let disc = eased(elapsed, Duration::ZERO, DISC, still);
    let disc_from = if from_state == MarkState::Done { 1.0 } else { 0.0 };
    let disc_to = if to == MarkState::Done { 1.0 } else { 0.0 };
    let disc_t = motion::lerp(disc_from, disc_to, if from.is_some() { disc } else { 1.0 });
    let settled = from.is_none();
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
        disc_scale: 0.6 + 0.4 * disc_t,
        disc_opacity: disc_t,
        tick: match (to, settled) {
            (MarkState::Done, true) => 1.0,
            (MarkState::Done, false) => eased(elapsed, TICK_DELAY, TICK, still),
            _ => 0.0,
        },
        bang: match (to, settled) {
            (MarkState::Failed, true) => 1.0,
            (MarkState::Failed, false) => eased(elapsed, BANG_DELAY, BANG, still),
            _ => 0.0,
        },
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
        let radius = 10.0 * unit * frame.disc_scale;
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
        let dot = 1.3 * unit * frame.bang;
        let mut point_path = PathBuilder::fill();
        let middle = local(12.0, 16.0);
        point_path.move_to(point(middle.x + px(dot), middle.y));
        for step in 1..=16 {
            let theta = TAU * step as f32 / 16.0;
            point_path
                .line_to(point(middle.x + px(dot * theta.cos()), middle.y + px(dot * theta.sin())));
        }
        point_path.close();
        if let Ok(path) = point_path.build() {
            window.paint_path(path, frame.color);
        }
    }
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

    /// Working to waiting: the six dashes close to a ring over 200 ms, in amber by 150.
    #[test]
    fn a_working_ring_closes_when_the_person_is_needed() {
        let theme = Theme::dark();
        assert_eq!(frame(MarkState::Working, MarkState::Waiting, 0).closed, 0.0);
        let mid = frame(MarkState::Working, MarkState::Waiting, 120).closed;
        assert!(mid > 0.5 && mid < 1.0, "most of the way at 120 ms on the quint: {mid}");
        let settled = frame(MarkState::Working, MarkState::Waiting, 400);
        assert_eq!(settled.closed, 1.0);
        assert_eq!(settled.color, hue(MarkState::Waiting, &theme));
        assert_eq!(settled.disc_opacity, 0.0);
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
        for state in [MarkState::Working, MarkState::Waiting, MarkState::Done, MarkState::Failed] {
            let contrast = crate::theme::contrast(hue(state, &theme), theme.bg);
            assert!(contrast >= crate::theme::SPINNER_CONTRAST - 0.01, "{state:?}: {contrast}");
        }
    }
}
