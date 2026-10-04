//! A companion as an element: its frame picked by the clock, painted as snapped quads, one for
//! each run of one ink along a row.

use std::time::Duration;

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Style, Window, fill, point, px, size,
};
use slopty_theme::{Companions, Theme};

use super::pose::{Beat, Pose};
use super::sprites::SIDE;
use super::{Kind, Palette, Size, clock, drawn, rects};
use crate::colors::hsla;
use crate::icons::{SPIN_STEP, breath, steps_now, steps_shown, wake_at_next_step};

/// How long the needs-you wave plays when the state begins, lively.
const WAVE: Duration = Duration::from_secs(2);

/// How long each of the wave's two frames holds: four steps of the clock, three a second.
const WAVE_BEAT: u32 = 4;

/// How high a finished turn's hop lifts it at each step, in cells: up, held, and down.
const HOP: [u32; HOP_STEPS as usize] = [0, 1, 2, 2, 1, 0];

/// How many steps of the clock the hop takes: half a second.
const HOP_STEPS: u32 = 6;

/// How long a finished turn's raised arms hold under Reduce Motion, in place of the hop.
const CHEER_STILL: Duration = Duration::from_secs(1);

/// How many steps of the clock pass between two blinks of a playing companion: four seconds.
const BLINK_EVERY: u64 = 48;

/// How many steps a sleeping companion's "z" holds each place: a second, three to a rise.
const SNORE_STEP: u64 = 12;

/// A one-off moment a change of state plays, while the companion is on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Moment {
    /// Its agent began to wait on the person: the arm waves for [`WAVE`].
    Wave,
    /// Its agent finished a turn: one hop, arms up.
    Hop,
}

impl Moment {
    /// The moment a change from `was` to `now` plays: a wave into needs-you, a hop out of work
    /// into rest. None for anything else.
    const fn of(was: Pose, now: Pose) -> Option<Self> {
        match (was, now) {
            (Pose::NeedsYou, Pose::NeedsYou) => None,
            (_, Pose::NeedsYou) => Some(Self::Wave),
            (Pose::Working(_), Pose::Idle | Pose::Done | Pose::ToReview) => Some(Self::Hop),
            _ => None,
        }
    }

    /// How long it plays, Reduce Motion or not.
    const fn length(self, reduce: bool) -> Duration {
        match (self, reduce) {
            (Self::Wave, false) => WAVE,
            (Self::Wave, true) => Duration::ZERO,
            (Self::Hop, false) => SPIN_STEP.saturating_mul(HOP_STEPS),
            (Self::Hop, true) => CHEER_STILL,
        }
    }
}

/// What an element with an id remembers between frames: the pose it last drew, and the moment
/// playing since then, with when it began (time since the spin clock's start).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Memory {
    pose: Pose,
    pets: u32,
    moment: Option<(Moment, Duration)>,
}

/// A companion, `side` square, ready to paint.
#[derive(Debug)]
pub struct Companion {
    id: Option<ElementId>,
    kind: Kind,
    pose: Pose,
    size: Size,
    side: Pixels,
    palette: Palette,
    mode: Companions,
    one_shots: bool,
    phase: u32,
    playing: bool,
    stride: bool,
    shift: i32,
    fade: f32,
    pets: Option<u32>,
    shut: bool,
}

/// `kind`'s companion in `pose`, at the theme's icon size on the small grid.
#[must_use]
pub fn companion(theme: &Theme, kind: Kind, pose: Pose) -> Companion {
    Companion {
        id: None,
        kind,
        pose,
        size: Size::Small,
        side: px(theme.typography.icon()),
        palette: Palette::of(theme, kind, pose),
        mode: theme.behaviour.companions,
        one_shots: false,
        phase: 0,
        playing: false,
        stride: false,
        shift: 0,
        fade: 1.0,
        pets: None,
        shut: false,
    }
}

impl Companion {
    /// `side` square.
    #[must_use]
    pub const fn side(mut self, side: Pixels) -> Self {
        self.side = side;
        self
    }

    /// On the large grid.
    #[must_use]
    pub const fn large(mut self) -> Self {
        self.size = Size::Large;
        self
    }

    /// Known across frames as `id`, so a change of its pose while it shows can play its moment:
    /// a wave into needs-you, a hop out of a finished turn (lively only, and never for one that
    /// only comes into view later).
    #[must_use]
    pub fn one_shots(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self.one_shots = true;
        self
    }

    /// `phase` steps of the clock from its fellows, so a crowd does not move as one: its
    /// work's frames, its blinks and its "z".
    #[must_use]
    pub const fn phase(mut self, phase: u32) -> Self {
        self.phase = phase;
        self
    }

    /// Playing (lively, in the yard): it blinks and its "z" rises. It rides frames drawn anyway,
    /// for a working companion beside it in the same view, and asks for none of its own.
    #[must_use]
    pub const fn playing(mut self) -> Self {
        self.playing = true;
        self
    }

    /// Mid-stride, as it walks.
    #[must_use]
    pub const fn stride(mut self, stride: bool) -> Self {
        self.stride = stride;
        self
    }

    /// Playing, it walks out `toward` one side (1 right, −1 left) to meet its neighbour and
    /// back, in the paint only: its place in the layout stays.
    #[must_use]
    pub const fn walks(mut self, toward: i32) -> Self {
        self.shift = toward.signum();
        self
    }

    /// At `fade` of its opacity, as a row's receding glyph is.
    #[must_use]
    pub const fn fade(mut self, fade: f32) -> Self {
        self.fade = fade;
        self
    }

    /// Petted `pets` times so far, known as `id`: each new pet is one hop, the one purely
    /// playful thing a person can do with a companion (Dot, where nothing else is on screen).
    #[must_use]
    pub fn petted(mut self, id: impl Into<ElementId>, pets: u32) -> Self {
        self.id = Some(id.into());
        self.pets = Some(pets);
        self
    }

    /// Its eyes shut: Dot's eye is the mark's cursor, unlit with it as it blinks.
    #[must_use]
    pub const fn eyes_shut(mut self, shut: bool) -> Self {
        self.shut = shut;
        self
    }

    /// The pose it was built in.
    #[must_use]
    pub const fn pose(&self) -> Pose {
        self.pose
    }

    /// The moment playing now, remembered under `id`, given the pose it drew last.
    ///
    /// A pet always hops; a change of pose plays only with `lively` moments asked for.
    fn moment(
        &self,
        id: &GlobalElementId,
        now: Duration,
        lively: bool,
        window: &mut Window,
    ) -> Option<(Moment, Duration)> {
        let (pose, pets) = (self.pose, self.pets.unwrap_or(0));
        let changes = lively && self.one_shots;
        window.with_element_state(id, |was: Option<Memory>, _window| {
            let begun = |moment: Moment| Some((moment, below_the_grid(now)));
            let moment = match was {
                Some(was) if pets > was.pets => begun(Moment::Hop),
                Some(was) if was.pose == pose || !changes => was.moment,
                Some(was) => Moment::of(was.pose, pose).and_then(begun),
                None => None,
            };
            (moment, Memory { pose, pets, moment })
        })
    }
}

/// The start of the clock's step `at` is in.
fn below_the_grid(at: Duration) -> Duration {
    let step = SPIN_STEP.as_nanos();
    let into = at.as_nanos().checked_rem(step).unwrap_or(0);
    at.saturating_sub(Duration::from_nanos(u64::try_from(into).unwrap_or(0)))
}

/// Whole steps of the clock in `span`.
fn steps(span: Duration) -> u64 {
    u64::try_from(span.as_nanos().checked_div(SPIN_STEP.as_nanos()).unwrap_or(0)).unwrap_or(0)
}

impl IntoElement for Companion {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Companion {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: size(self.side.into(), self.side.into()),
            flex_shrink: 0.0,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.mode == Companions::Off {
            return;
        }
        let reduce = cx.reduce_motion();
        let lively = self.mode == Companions::Lively;
        let shown = steps_shown(cx);
        let now = steps_now(cx);
        let step = steps(shown);
        let moment = match id {
            Some(id) if (self.one_shots && lively) || self.pets.is_some() => {
                self.moment(id, now, lively, window)
            }
            _ => None,
        };
        let phase = u64::from(self.phase);
        let playing = self.playing && lively && !reduce;
        let mut pose = self.pose;
        let mut beat = Beat { stride: self.stride, blink: self.shut, ..Beat::default() };
        let mut lift = 0;
        let mut alpha = self.fade;
        let mut next: Option<Duration> = None;
        match pose {
            Pose::Working(_) if reduce => alpha *= breath(shown),
            Pose::Working(_) => {
                beat.frame = u32::try_from((step.wrapping_add(phase) / 2) % 4).unwrap_or(0);
            }
            Pose::Asleep if playing => {
                beat.frame =
                    u32::try_from((step.wrapping_add(phase) / SNORE_STEP) % 3).unwrap_or(0);
            }
            _ => {}
        }
        if let Some((moment, began)) = moment {
            let length = moment.length(reduce);
            let into = now.saturating_sub(began);
            if into < length {
                let into_steps = steps(into);
                match moment {
                    Moment::Wave => {
                        let beat_n = into_steps.checked_div(u64::from(WAVE_BEAT)).unwrap_or(0);
                        beat.frame = u32::try_from(beat_n % 2).unwrap_or(0);
                        let beats = u32::try_from(beat_n.saturating_add(1)).unwrap_or(u32::MAX);
                        let wait = SPIN_STEP.saturating_mul(WAVE_BEAT.saturating_mul(beats));
                        next = Some(began.saturating_add(wait));
                    }
                    Moment::Hop => {
                        // Dot has no arms to raise: the mark only hops.
                        if self.kind != Kind::Dot {
                            pose = Pose::Done;
                        }
                        let at = usize::try_from(into_steps).unwrap_or(usize::MAX);
                        let scale = u32::try_from(self.size.cells() / SIDE).unwrap_or(1);
                        let height = HOP.get(at).copied().unwrap_or(0).saturating_mul(scale);
                        lift = if reduce { 0 } else { height };
                        next = Some(if reduce {
                            began.saturating_add(length)
                        } else {
                            let after =
                                u32::try_from(into_steps.saturating_add(1)).unwrap_or(u32::MAX);
                            began.saturating_add(SPIN_STEP.saturating_mul(after))
                        });
                    }
                }
            }
        }
        let mut shift = 0;
        if playing && self.shift != 0 && matches!(pose, Pose::Idle | Pose::Done | Pose::ToReview) {
            let (out, stride) = walk(step.wrapping_add(phase));
            let scale = i32::try_from(self.size.cells() / SIDE).unwrap_or(1);
            shift = self.shift.saturating_mul(out).saturating_mul(scale);
            beat.stride |= stride;
        }
        if playing && !matches!(pose, Pose::Asleep | Pose::Failed | Pose::Silent | Pose::Gone) {
            beat.blink |= step.wrapping_add(phase) % BLINK_EVERY == 0;
        }
        let palette = if pose == self.pose {
            self.palette
        } else {
            Palette { cue: self.palette.cheer, ..self.palette }
        };
        let place = Place { side: self.side, grid: self.size, shift, lift, alpha };
        paint_frame(window, bounds, &place, &drawn(self.kind, pose, beat, self.size), &palette);
        if matches!(self.pose, Pose::Working(_)) {
            wake_at_next_step(window, cx);
        } else if let Some(at) = next {
            clock::wake_at(window, cx, at);
        }
    }
}

/// How many steps a walk out and back takes, with its rests: eight seconds.
const WALK_EVERY: u64 = 96;

/// How far out a walk goes, in cells of the small grid, and whether the feet are mid-stride,
/// `step` steps into the clock: out a cell every four steps, a rest there (meeting the
/// neighbour walking the other way), back, and a longer rest home.
fn walk(step: u64) -> (i32, bool) {
    let at = step % WALK_EVERY;
    let striding = (at / 2) % 2 == 1;
    match at {
        0..12 => (i32::try_from(at / 4).unwrap_or(0), striding),
        12..36 => (3, false),
        36..48 => {
            (3_i32.saturating_sub(i32::try_from(at.saturating_sub(36) / 4).unwrap_or(0)), striding)
        }
        _ => (0, false),
    }
}

/// Where and how a frame is painted in its bounds.
struct Place {
    /// The square it fits in.
    side: Pixels,
    grid: Size,
    /// Cells right of its place.
    shift: i32,
    /// Cells up.
    lift: u32,
    alpha: f32,
}

/// Paint `canvas` centred in `bounds` as `place` says, each cell whole device pixels.
#[expect(clippy::cast_precision_loss, reason = "cell counts and offsets under seventeen")]
fn paint_frame(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    place: &Place,
    canvas: &super::Canvas,
    palette: &Palette,
) {
    let scale = window.scale_factor();
    let cells = place.grid.cells() as f32;
    let device = (f32::from(place.side) * scale / cells).floor().max(1.0);
    let cell = device / scale;
    let span = cell * cells;
    let snap = |v: f32| (v * scale).round() / scale;
    let center = bounds.center();
    let left = cell.mul_add(place.shift as f32, snap(f32::from(center.x) - span / 2.0));
    let top = cell.mul_add(-(place.lift as f32), snap(f32::from(center.y) - span / 2.0));
    for rect in rects(canvas).iter() {
        let Some(rgb) = palette.rgb(rect.ink) else { continue };
        let at = |n: u8, from: f32| px(cell.mul_add(f32::from(n), from));
        let origin = point(at(rect.x, left), at(rect.y, top));
        let quad = Bounds::new(
            origin,
            size(px(cell * f32::from(rect.width)), px(cell * f32::from(rect.height))),
        );
        window.paint_quad(fill(quad, hsla(rgb).opacity(place.alpha)));
    }
}
