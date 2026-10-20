//! slopty: the new-thread hero's living glyph field, its wind.
//!
//! Claude desktop's Code landing draws a field of monospace characters that wind moves
//! through in gusts; the pointer warms it and a click sends a ripple through it, every third
//! one a heart (`.research/claude-desktop-2.42304.2-2026-10-19.md`, §A). This is that
//! behaviour, worked out on the CPU at one sample per character cell: a pane holds about ten
//! thousand cells, and what paints them is GPUI's glyph path
//! (`new_thread_background_glyphs`). Only behaviour and values are taken; the code is
//! Slopty's.
//!
//! Everything here is a pure function of a visit's seed, the pane, the wind's own clock and
//! the hand's marks, so a frame drawn twice at one moment draws alike and a test can hold any
//! moment still.

use std::f32::consts::{PI, TAU};

/// A character cell's width in points (Claude's 6.5 CSS px), rounded to whole device pixels.
const CELL_WIDTH: f32 = 6.5;
/// The fewest columns and rows a field is drawn with; a smaller pane has none.
const MIN_COLUMNS: usize = 24;
const MIN_ROWS: usize = 8;
/// The most device pixels a field covers; a larger pane gets fewer, larger cells.
const MAX_DEVICE_PIXELS: f32 = 21.0e6;
/// The characters' size, in points.
pub(crate) const GLYPH_SIZE: f32 = 9.0;

/// How much light a character keeps where wind has just been: under it nothing shows.
pub(crate) const REST: f32 = 0.03;
/// How far a ripple's glow may outshine the plain ink.
pub(crate) const HEADROOM: f32 = 3.0;
/// The plain ink's strength at full wind, against the glow's: the field never nears full ink.
pub(crate) const BASE: f32 = 0.26;

// The wind's character (Claude's values).
const FLICK: f32 = 1.2;
const LULL: f32 = 0.5;
const STREAK: f32 = 0.4;
const DETAIL: f32 = 0.6;
const EDGE: f32 = 0.75;
const CURL: f32 = 0.65;
const WISP: f32 = 0.5;
const TAIL: f32 = 0.35;
const SWEEP: f32 = 0.3;
const MORPH: f32 = 3.0;
/// How a stroke facing the wind catches more light, on average over a character's strokes.
const CAUGHT: f32 = 0.9;

/// The calm behind the composer: its radii in points and how thin the field gets at its heart.
const CALM_RADII: (f32, f32) = (250.0, 80.0);
const CALM_FLOOR: f32 = 0.3;

/// How long a click's ripple and a stroke's warmth last, in seconds.
pub(crate) const TAP_LIFE: f32 = 3.6;
pub(crate) const WAKE_LIFE: f32 = 2.4;
const MAX_TAPS: usize = 6;
const MAX_WAKES: usize = 48;
/// Every this many clicks, the ripple is a heart.
const HEART_EVERY: u32 = 3;

/// One of the three sets of characters a visit steps through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Mixed,
    Lettered,
    Figured,
}

impl Look {
    pub(crate) const ALL: [Self; 3] = [Self::Mixed, Self::Lettered, Self::Figured];

    /// The characters, in the order a cell steps through them.
    pub(crate) const fn characters(self) -> &'static str {
        match self {
            Self::Mixed => "ab0cAde1fEgh2iGjk3lHmn4oKpq5rMst6uRvw7xSyz8nT9eW",
            Self::Lettered => "abcd3efgRhijk7lmnAopqr0stuEvwxy5zaeKiou2gcS9",
            Self::Figured => "0a1b2C3d4e5F6g7h8i9J0k1m2N3p4q5R6s7t8U9v0w1X2y3z",
        }
    }
}

// ---------------------------------------------------------------------------
// Hashing and noise
// ---------------------------------------------------------------------------

/// A PCG permutation of 32 bits.
#[inline(always)]
const fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (w >> 22) ^ w
}

/// A hash as a number in `[0, 1)`.
#[inline(always)]
fn unit(h: u32) -> f32 {
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[inline(always)]
fn value(v: u32) -> f32 {
    unit(pcg(v)).mul_add(2.0, -1.0)
}

#[inline(always)]
fn fade(f: f32) -> f32 {
    f * f * f * f.mul_add(f.mul_add(6.0, -15.0), 10.0)
}

#[inline(always)]
fn mix(a: f32, b: f32, t: f32) -> f32 {
    (b - a).mul_add(t, a)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * 2.0f32.mul_add(-t, 3.0)
}

/// One plane of the lattice along its third axis: the corner hashes and the weight every
/// sample on it shares. The third axis is time, the same for every sample of a frame, so a
/// frame works its planes out once and each sample hashes only its own two axes.
#[derive(Debug, Clone, Copy)]
struct Plane {
    z0: u32,
    z1: u32,
    fz: f32,
    wz: f32,
}

impl Plane {
    fn new(z: f32, salt: u32) -> Self {
        let fl = z.floor();
        let iz = fl as i32;
        let fz = z - fl;
        Self {
            z0: pcg((iz as u32) ^ salt),
            z1: pcg((iz.wrapping_add(1) as u32) ^ salt),
            fz,
            wz: fade(fz),
        }
    }
}

/// A sample's place on the lattice: its corners' x hashes, the four (y, z) hashes, and its
/// weights across the cell.
#[derive(Debug, Clone, Copy)]
struct Corner {
    x0: u32,
    x1: u32,
    h: [u32; 4],
    fx: f32,
    fy: f32,
    wx: f32,
    wy: f32,
}

#[inline(always)]
fn corner(x: f32, y: f32, plane: &Plane) -> Corner {
    let (xf, yf) = (x.floor(), y.floor());
    let (fx, fy) = (x - xf, y - yf);
    let (ix, iy) = (xf as i32, yf as i32);
    let (y0, y1) = (iy as u32, iy.wrapping_add(1) as u32);
    Corner {
        x0: ix as u32,
        x1: ix.wrapping_add(1) as u32,
        h: [pcg(y0 ^ plane.z0), pcg(y1 ^ plane.z0), pcg(y0 ^ plane.z1), pcg(y1 ^ plane.z1)],
        fx,
        fy,
        wx: fade(fx),
        wy: fade(fy),
    }
}

/// Value noise on an integer lattice salted by the visit, from −1 to 1.
fn noise(p: [f32; 3], salt: u32) -> f32 {
    noise2(p[0], p[1], &Plane::new(p[2], salt))
}

/// [`noise`] at `(x, y)` on a plane.
#[inline(always)]
fn noise2(x: f32, y: f32, plane: &Plane) -> f32 {
    let c = corner(x, y, plane);
    let [a, b, cc, d] = c.h;
    mix(
        mix(
            mix(value(c.x0 ^ a), value(c.x1 ^ a), c.wx),
            mix(value(c.x0 ^ b), value(c.x1 ^ b), c.wx),
            c.wy,
        ),
        mix(
            mix(value(c.x0 ^ cc), value(c.x1 ^ cc), c.wx),
            mix(value(c.x0 ^ d), value(c.x1 ^ d), c.wx),
            c.wy,
        ),
        plane.wz,
    )
}

/// A lattice point's slope, each axis from −1 to 1.
#[inline(always)]
fn slope(v: u32) -> [f32; 3] {
    let h = pcg(v);
    let k = 2.0 / 1023.0;
    [
        ((h & 1023) as f32).mul_add(k, -1.0),
        (((h >> 10) & 1023) as f32).mul_add(k, -1.0),
        (((h >> 20) & 1023) as f32).mul_add(k, -1.0),
    ]
}

#[inline(always)]
fn dot3(g: [f32; 3], x: f32, y: f32, z: f32) -> f32 {
    g[0].mul_add(x, g[1].mul_add(y, g[2] * z))
}

/// Gradient noise on the same lattice, about −1 to 1, at `(x, y)` on a plane: it crosses zero
/// between every pair of points, so it has no soft blobs.
#[inline(always)]
fn swirl2(x: f32, y: f32, plane: &Plane) -> f32 {
    let c = corner(x, y, plane);
    let [a, b, cc, d] = c.h;
    let (fx, fy, fz) = (c.fx, c.fy, plane.fz);
    let (gx, gy, gz) = (fx - 1.0, fy - 1.0, fz - 1.0);
    1.5 * mix(
        mix(
            mix(dot3(slope(c.x0 ^ a), fx, fy, fz), dot3(slope(c.x1 ^ a), gx, fy, fz), c.wx),
            mix(dot3(slope(c.x0 ^ b), fx, gy, fz), dot3(slope(c.x1 ^ b), gx, gy, fz), c.wx),
            c.wy,
        ),
        mix(
            mix(dot3(slope(c.x0 ^ cc), fx, fy, gz), dot3(slope(c.x1 ^ cc), gx, fy, gz), c.wx),
            mix(dot3(slope(c.x0 ^ d), fx, gy, gz), dot3(slope(c.x1 ^ d), gx, gy, gz), c.wx),
            c.wy,
        ),
        plane.wz,
    )
}

// ---------------------------------------------------------------------------
// A visit
// ---------------------------------------------------------------------------

/// What one visit to the hero draws: its seed, its characters and its wind. Each visit draws a
/// new one, so the field never shows the same weather twice in a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Visit {
    pub seed: u32,
    pub look: Look,
    /// The wind's heading, in radians, y down: rightwards or leftwards, 18° to 42° off level.
    heading: f32,
    /// Points per second.
    speed: f32,
    /// A gust's length along the wind and width across it, in points.
    gust: (f32, f32),
    /// How far the wind's path bends, and the size of a bend.
    bend: f32,
    bend_scale: f32,
}

/// A stream of numbers in `[0, 1)` from a seed and a name.
fn stream(seed: u32, name: &str) -> impl FnMut() -> f32 {
    let mut n = pcg(seed);
    for byte in name.bytes() {
        n = pcg(n ^ u32::from(byte));
    }
    move || {
        n = n.wrapping_add(0x9E37_79B9);
        pcg(n) as f32 / 4_294_967_296.0
    }
}

impl Visit {
    /// The visit drawn from `seed`, with a look other than `last`'s.
    pub(crate) fn new(seed: u32, last: Option<Look>) -> Self {
        let mut wind = stream(seed, "wind");
        let mut range = |lo: f32, hi: f32| mix(lo, hi, wind());
        let side = range(0.0, 1.0);
        let off_level = mix(PI / 10.0, PI / 4.3, (side * 2.0).fract());
        let heading = if side < 0.5 { off_level } else { PI - off_level };
        let speed = range(42.0, 64.0);
        let gust = (range(380.0, 560.0), range(120.0, 190.0));
        let bend = range(60.0, 110.0);
        let bend_scale = range(300.0, 440.0);
        let looks: Vec<Look> = Look::ALL.into_iter().filter(|look| Some(*look) != last).collect();
        let pick = (stream(seed, "look")() * looks.len() as f32) as usize;
        let look = looks.get(pick).copied().unwrap_or(Look::Mixed);
        Self { seed, look, heading, speed, gust, bend, bend_scale }
    }
}

// ---------------------------------------------------------------------------
// The grid
// ---------------------------------------------------------------------------

/// The character cells over a pane, centred, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Grid {
    pub columns: usize,
    pub rows: usize,
    pub cell: (f32, f32),
    /// The first cell's top-left corner.
    pub origin: (f32, f32),
}

impl Grid {
    /// The cells over a pane `width` by `height` points at `scale` device pixels a point, or
    /// `None` for a pane too small for a field.
    pub(crate) fn new(width: f32, height: f32, scale: f32) -> Option<Self> {
        if !(width > 0.0 && height > 0.0 && scale > 0.0) || !(width * height * scale).is_finite() {
            return None;
        }
        let cell_device = (CELL_WIDTH * scale).round().max(5.0);
        let (cw, ch) = (cell_device / scale, 2.0 * cell_device / scale);
        let mut columns = (width / cw).ceil();
        let mut rows = (height / ch).ceil();
        let over = columns * cw * rows * ch * scale * scale / MAX_DEVICE_PIXELS;
        if over > 1.0 {
            columns = (columns / over.sqrt()).floor();
            rows = (rows / over.sqrt()).floor();
        }
        let (columns, rows) = (columns as usize, rows as usize);
        if columns < MIN_COLUMNS || rows < MIN_ROWS {
            return None;
        }
        let snap = |v: f32| (v * scale).round() / scale;
        let origin = (
            snap((width - columns as f32 * cw) / 2.0),
            snap((height - rows as f32 * ch) / 2.0),
        );
        Some(Self { columns, rows, cell: (cw, ch), origin })
    }

    pub(crate) const fn len(&self) -> usize {
        self.columns * self.rows
    }

    /// A cell's centre, in points of the pane.
    pub(crate) fn centre(&self, column: usize, row: usize) -> (f32, f32) {
        (
            (column as f32 + 0.5).mul_add(self.cell.0, self.origin.0),
            (row as f32 + 0.5).mul_add(self.cell.1, self.origin.1),
        )
    }
}

// ---------------------------------------------------------------------------
// The hand: clicks and strokes
// ---------------------------------------------------------------------------

/// A click's ripple: where, when (on the visit's clock, in seconds) and how hard; less than
/// nothing is a heart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tap {
    x: f32,
    y: f32,
    at: f32,
    force: f32,
}

/// A stretch of the pointer's path that warms the air.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Wake {
    x: f32,
    y: f32,
    at: f32,
    force: f32,
    way: (f32, f32),
}

/// A wake as it stands at one moment: middle, warmth, one over its reach squared, direction
/// and half length.
/// A tap as one frame sees it: what depends on its age alone, worked out once a frame rather
/// than at every sample.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LiveTap {
    x: f32,
    y: f32,
    /// A heart's front rather than a ring's.
    heart: bool,
    /// How far the front has run.
    radius: f32,
    /// The front's width, as `1 / width²`.
    narrow: f32,
    /// The push and lift at the front's crest.
    swell: f32,
    /// The glow at the front, the afterlight behind it, and the colour behind it.
    flash: f32,
    after: f32,
    hue: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LiveWake {
    x: f32,
    y: f32,
    warmth: f32,
    reach: f32,
    way: (f32, f32),
    half: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f32,
    y: f32,
    at: f32,
}

/// What the pointer has done to the field: Claude's touch model, on the visit's clock.
#[derive(Debug, Clone, Default)]
pub(crate) struct Hand {
    taps: Vec<Tap>,
    wakes: Vec<Wake>,
    last: Option<Point>,
    anchor: Option<Point>,
    speed: f32,
    clicks: u32,
}

impl Hand {
    /// The pointer moved to `(x, y)` at `at`; whether that left a new stretch of warmth.
    pub(crate) fn moved(&mut self, x: f32, y: f32, at: f32) -> bool {
        let here = Point { x, y, at };
        let (Some(last), Some(anchor)) = (self.last, self.anchor) else {
            self.reset(here);
            return false;
        };
        if at - last.at > 0.25 {
            self.reset(here);
            return false;
        }
        let dt = (at - last.at).max(1.0 / 240.0);
        let distance = (x - last.x).hypot(y - last.y);
        self.speed += (distance / dt - self.speed) * (dt / 0.08).min(1.0);
        self.last = Some(here);
        let length = (x - anchor.x).hypot(y - anchor.y);
        if length < 18.0 || at - anchor.at < 0.035 {
            return false;
        }
        let stir = smoothstep(60.0, 800.0, self.speed);
        self.anchor = Some(here);
        if stir < 0.04 {
            return false;
        }
        if self.wakes.len() >= MAX_WAKES {
            self.wakes.remove(0);
        }
        self.wakes.push(Wake {
            x: (x + anchor.x) / 2.0,
            y: (y + anchor.y) / 2.0,
            at,
            force: stir * length / (length + 90.0),
            way: ((x - anchor.x) / 2.0, (y - anchor.y) / 2.0),
        });
        true
    }

    fn reset(&mut self, here: Point) {
        self.last = Some(here);
        self.anchor = Some(here);
        self.speed = 0.0;
    }

    /// The pointer left the pane.
    pub(crate) fn left(&mut self) {
        self.last = None;
    }

    /// A click at `(x, y)` at `at`: a ripple, every third a heart.
    pub(crate) fn tapped(&mut self, x: f32, y: f32, at: f32) {
        self.clicks = self.clicks.wrapping_add(1);
        let force = if self.clicks.is_multiple_of(HEART_EVERY) { -1.15 } else { 1.0 };
        if self.taps.len() >= MAX_TAPS {
            self.taps.remove(0);
        }
        self.taps.push(Tap { x, y, at, force });
    }

    /// Whether a ripple or a stroke's warmth is still alive at `now`.
    pub(crate) fn busy(&self, now: f32) -> bool {
        self.rippling(now) || self.wakes.iter().any(|w| now - w.at < WAKE_LIFE)
    }

    /// Whether a ripple is still running at `now`.
    pub(crate) fn rippling(&self, now: f32) -> bool {
        self.taps.iter().any(|t| now - t.at < TAP_LIFE)
    }

    /// Forget what has died by `now`.
    pub(crate) fn prune(&mut self, now: f32) {
        self.taps.retain(|t| now - t.at < TAP_LIFE);
        self.wakes.retain(|w| now - w.at < WAKE_LIFE);
    }

    fn live_taps(&self, now: f32) -> Vec<LiveTap> {
        self.taps
            .iter()
            .filter_map(|tap| {
                let age = now - tap.at;
                if !(0.0..TAP_LIFE).contains(&age) {
                    return None;
                }
                let force = tap.force.abs();
                let width = 60.0f32.mul_add(age, 85.0);
                Some(LiveTap {
                    x: tap.x,
                    y: tap.y,
                    heart: tap.force < 0.0,
                    radius: 620.0 * (1.0 - (-age / 1.3).exp()),
                    narrow: 1.0 / (width * width),
                    swell: force * (-age / 1.5).exp(),
                    flash: force * (-age).exp(),
                    after: force * 0.7 * (-age / 1.3).exp(),
                    hue: force * (1.0 - (-age / 0.2).exp()) * (-age / 1.2).exp(),
                })
            })
            .collect()
    }

    fn live_wakes(&self, now: f32) -> Vec<LiveWake> {
        let spare = (MAX_WAKES - self.wakes.len().min(MAX_WAKES)) as f32;
        self.wakes
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                let age = (now - w.at).max(0.0);
                let warmth = w.force
                    * ((i as f32 + 1.0 + spare) / 8.0).min(1.0)
                    * (1.0 - (-age / 0.05).exp())
                    * (-age / 0.65).exp();
                if warmth < 0.002 {
                    return None;
                }
                let reach = 40.0f32.mul_add(age, 50.0);
                let half = w.way.0.hypot(w.way.1);
                let unit = if half > 0.0 { (w.way.0 / half, w.way.1 / half) } else { (0.0, 0.0) };
                Some(LiveWake {
                    x: w.x,
                    y: w.y,
                    warmth,
                    reach: 1.0 / (reach * reach),
                    way: unit,
                    half,
                })
            })
            .collect()
    }
}

/// How far out a heart's outline lies in direction `u` (a unit vector, y down), against a
/// circle's 1.
fn heart(u: (f32, f32)) -> f32 {
    let k = -u.0 * u.0 * u.1 * u.1 * u.1;
    let c = k.signum() * k.abs().cbrt();
    let mut r = 1.0f32;
    for _ in 0..6 {
        r = c.mul_add(r.powf(5.0 / 3.0), 1.0).max(0.05).sqrt();
    }
    r
}

/// How far a heart's outline lies at most, against a circle's 1 (`heart` rises from 1 to its
/// fixed point, under 1.43).
const HEART_MOST: f32 = 1.5;

/// What the clicks do at a point: the air's push (x, y), the wind raised or lowered, how
/// roused; and a ripple's colour and afterlight.
fn taps_at(taps: &[LiveTap], p: (f32, f32)) -> ([f32; 4], (f32, f32)) {
    let mut sum = [0.0f32; 4];
    let mut glow = (0.0f32, 0.0f32);
    for tap in taps {
        let away = (p.0 - tap.x, p.1 - tap.y);
        let d = away.0.hypot(away.1);
        // Ahead of the front by more than its leading edge nothing reaches: no front, no trail.
        if d / if tap.heart { HEART_MOST } else { 1.0 } - tap.radius > 16.0 {
            continue;
        }
        let reach = if tap.heart { d / heart((away.0 / d.max(1.0), away.1 / d.max(1.0))) } else { d };
        let behind = tap.radius - reach;
        let front = smoothstep(-16.0, 10.0, behind) * (-behind * behind * tap.narrow).exp();
        let swell = tap.swell * front;
        let push = 44.0 * swell / d.max(1.0);
        sum[0] += away.0 * push;
        sum[1] += away.1 * push;
        sum[2] += 1.9 * swell * 0.75f32.mul_add((behind * (TAU / 130.0)).cos(), 0.25);
        sum[3] += swell;
        let trail = smoothstep(0.0, 70.0, behind) * (-behind.max(0.0) / 260.0).exp();
        glow.0 += tap.flash.mul_add(front, tap.after * trail);
        glow.1 += tap.hue * trail;
    }
    (sum, glow)
}

/// How warm the pointer's path has left a point, from 0 to just under 1.
fn warmth_at(wakes: &[LiveWake], p: (f32, f32)) -> f32 {
    let mut warmth = 0.0;
    for wake in wakes {
        let mut away = (p.0 - wake.x, p.1 - wake.y);
        let along = away.0.mul_add(wake.way.0, away.1 * wake.way.1).clamp(-wake.half, wake.half);
        away.0 -= wake.way.0 * along;
        away.1 -= wake.way.1 * along;
        warmth += wake.warmth * (-away.0.mul_add(away.0, away.1 * away.1) * wake.reach).exp();
    }
    1.0 - (-1.5 * warmth).exp()
}

// ---------------------------------------------------------------------------
// One frame of the field
// ---------------------------------------------------------------------------

/// What a frame of the field is drawn at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Moment {
    /// The wind's clock, in seconds: it runs while the field animates and stops when it rests.
    pub wind: f32,
    /// The visit's clock, in seconds, which the hand's marks are stamped on.
    pub now: f32,
    /// How much of the wind has arrived, 0 to 1.
    pub reveal: f32,
}

/// One cell as a frame lights it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Lit {
    /// The ink's strength, 0 to 1, before the field's own opacity; 0 draws nothing.
    pub alpha: f32,
    /// How far its colour has gone from the ink to the glow, 0 to 1.
    pub tint: f32,
    /// Which character of the visit's look it shows.
    pub glyph: u16,
}

/// The pane and what sits on it, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Pane {
    pub width: f32,
    pub height: f32,
    /// The middle of the calm: behind the composer.
    pub calm: (f32, f32),
}

/// The salts of the wind's noises, one per use.
mod salt {
    pub const TURN: [u32; 2] = [0x51, 0x52];
    pub const THREAD: u32 = 0x57;
    pub const EDDY: [u32; 2] = [0x59, 0x5a];
    pub const FINE: [u32; 2] = [0x5b, 0x5c];
    pub const TINY: [u32; 2] = [0x5d, 0x5e];
    pub const GUST: u32 = 0x60;
    pub const STRAND: u32 = 0x66;
    pub const SWELL: u32 = 0x67;
    pub const NEAR: [u32; 2] = [0x71, 0x72];
}

/// Per-frame constants of a visit's wind: its frame of reference, how far it has carried, and
/// every noise's plane at this moment.
#[derive(Debug)]
struct Air {
    heading: (f32, f32),
    across: (f32, f32),
    carried: f32,
    span: f32,
    gust: (f32, f32),
    bend: f32,
    bend_scale: f32,
    t: f32,
    turn: [Plane; 2],
    eddy: [Plane; 2],
    fine: [Plane; 2],
    tiny: [Plane; 2],
    octaves: [Plane; 4],
    strand: Plane,
    swell: Plane,
    thread: Plane,
    near: [Plane; 2],
}

impl Air {
    fn new(visit: &Visit, pane: &Pane, moment: Moment) -> Self {
        let t = moment.wind;
        let seed = visit.seed;
        let plane = |z: f32, salt: u32| Plane::new(z, seed ^ salt);
        let heading = (visit.heading.cos(), visit.heading.sin());
        // It surges and slackens as it goes.
        let carried = visit.speed * (t + noise([t * 0.09, 3.7, 0.0], seed ^ 0x58));
        let span = 0.58 * heading.0.abs().mul_add(pane.width, heading.1.abs() * pane.height);
        let morph = MORPH * t;
        Self {
            heading,
            across: (-heading.1, heading.0),
            carried,
            span,
            gust: visit.gust,
            bend: visit.bend,
            bend_scale: visit.bend_scale,
            t,
            turn: [plane(t * 0.04, salt::TURN[0]), plane(t.mul_add(0.04, 5.0), salt::TURN[1])],
            eddy: [plane(morph * 0.04, salt::EDDY[0]), plane(morph.mul_add(0.04, 3.0), salt::EDDY[1])],
            fine: [plane(morph * 0.06, salt::FINE[0]), plane(morph.mul_add(0.06, 7.0), salt::FINE[1])],
            tiny: [plane(t * 0.07, salt::TINY[0]), plane(t.mul_add(0.07, 1.0), salt::TINY[1])],
            octaves: [0u32, 1, 2, 3].map(|k| {
                let kf = k as f32;
                plane(morph.mul_add(0.018f32.mul_add(kf, 0.03), 11.0 * kf), salt::GUST + k)
            }),
            strand: plane(morph.mul_add(0.05, 2.0), salt::STRAND),
            swell: plane(7.0, salt::SWELL),
            thread: plane(t * 0.16, salt::THREAD),
            near: [
                plane(moment.now * 0.12, salt::NEAR[0]),
                plane(moment.now.mul_add(0.12, 4.0), salt::NEAR[1]),
            ],
        }
    }

    /// How the wind's path bends at `from`, and the eddies it curls in there, before the
    /// hand's push: both change only over hundreds of points.
    fn sway(&self, from: (f32, f32)) -> [f32; 4] {
        let s = self.bend_scale;
        let turn = (
            noise2(from.0 / s, from.1 / s, &self.turn[0]),
            noise2(from.0 / s + 17.3, from.1 / s + 17.3, &self.turn[1]),
        );
        let riding = self.riding(from, turn);
        let size = 1.15 * self.gust.1;
        [
            turn.0,
            turn.1,
            swirl2(riding.0 / size, riding.1 / size, &self.eddy[0]),
            swirl2(riding.0 / size + 31.7, riding.1 / size + 31.7, &self.eddy[1]),
        ]
    }

    /// A point in the wind's own frame, riding with it.
    fn riding(&self, from: (f32, f32), turn: (f32, f32)) -> (f32, f32) {
        let bent = (self.bend.mul_add(turn.0, from.0), self.bend.mul_add(turn.1, from.1));
        (
            0.85f32.mul_add(-self.carried, bent.0.mul_add(self.heading.0, bent.1 * self.heading.1)),
            bent.0.mul_add(self.across.0, bent.1 * self.across.1),
        )
    }
}

/// How the wind is sampled: every other column of cells (13 by 13 points at the usual cell),
/// the columns between read from their neighbours. Claude samples twice a cell, for a shader
/// that lights each pixel; a cell here takes one light, and the eye cannot tell the two apart
/// under the characters' own steps.
const SAMPLE_STRIDE: usize = 2;
/// The bends and eddies change only over hundreds of points: they are worked out every other
/// sample each way and read between.
const SWAY_STRIDE: usize = 2;
/// The samples worked out together: a row is padded to a multiple of this, and each noise
/// runs over a whole row at once, which the compiler turns into vector instructions.
const LANES: usize = 8;

/// [`swirl2`] over a row of points.
fn swirl_row(x: &[f32], y: &[f32], plane: &Plane, out: &mut [f32]) {
    for ((x, y), out) in x.chunks_exact(LANES).zip(y.chunks_exact(LANES)).zip(out.chunks_exact_mut(LANES)) {
        for ((x, y), out) in x.iter().zip(y).zip(out.iter_mut()) {
            *out = swirl2(*x, *y, plane);
        }
    }
}

/// [`noise2`] over a row of points.
fn noise_row(x: &[f32], y: &[f32], plane: &Plane, out: &mut [f32]) {
    for ((x, y), out) in x.chunks_exact(LANES).zip(y.chunks_exact(LANES)).zip(out.chunks_exact_mut(LANES)) {
        for ((x, y), out) in x.iter().zip(y).zip(out.iter_mut()) {
            *out = noise2(*x, *y, plane);
        }
    }
}

/// A cosine good to a few parts in ten thousand, which vectorises where the library's does not.
#[inline(always)]
fn cosine(x: f32) -> f32 {
    // Reduce to a quarter turn either side of zero, then a polynomial.
    let turns = x * (1.0 / TAU);
    let r = turns - turns.round();
    // cos(2πr) for r in [-0.5, 0.5]: fold to |r| ≤ 0.25 with the sign.
    let a = r.abs();
    let (a, sign) = if a > 0.25 { (0.5 - a, -1.0) } else { (a, 1.0) };
    let z = TAU * a;
    let z2 = z * z;
    sign * z2.mul_add(z2.mul_add(z2.mul_add(-1.0 / 720.0, 1.0 / 24.0), -0.5), 1.0)
}

/// The row buffers a frame works its samples out in, and what its later passes read.
#[derive(Debug, Default)]
struct Rows {
    ax: Vec<f32>,
    ay: Vec<f32>,
    n0: Vec<f32>,
    n1: Vec<f32>,
    fold: (Vec<f32>, Vec<f32>),
    eddy: (Vec<f32>, Vec<f32>),
    curled: (Vec<f32>, Vec<f32>),
    pull: Vec<f32>,
    gust: Vec<f32>,
}

impl Rows {
    fn size(&mut self, n: usize) {
        for v in [
            &mut self.ax,
            &mut self.ay,
            &mut self.n0,
            &mut self.n1,
            &mut self.fold.0,
            &mut self.fold.1,
            &mut self.eddy.0,
            &mut self.eddy.1,
            &mut self.curled.0,
            &mut self.curled.1,
            &mut self.pull,
            &mut self.gust,
        ] {
            v.clear();
            v.resize(n, 0.0);
        }
    }
}

/// Every sample of a frame, one vector per quantity.
#[derive(Debug, Default)]
struct Samples {
    still: Vec<f32>,
    field: Vec<f32>,
    curled: (Vec<f32>, Vec<f32>),
    fine: Vec<f32>,
    tiny: Vec<f32>,
    warm: Vec<f32>,
    roused: Vec<f32>,
    glow: (Vec<f32>, Vec<f32>),
    mask: Vec<f32>,
    strength: Vec<f32>,
}

impl Samples {
    fn clear(&mut self, n: usize) {
        for v in [
            &mut self.still,
            &mut self.field,
            &mut self.curled.0,
            &mut self.curled.1,
            &mut self.fine,
            &mut self.tiny,
            &mut self.warm,
            &mut self.roused,
            &mut self.glow.0,
            &mut self.glow.1,
            &mut self.mask,
            &mut self.strength,
        ] {
            v.clear();
            v.reserve(n);
        }
    }
}

/// One frame of the field, and the buffers it is worked out in, kept between frames.
#[derive(Debug, Default)]
pub(crate) struct Frame {
    /// Each cell's light, row by row.
    pub cells: Vec<Lit>,
    samples: Samples,
    rows: Rows,
    sway: Vec<[f32; 4]>,
    strength: Vec<f32>,
}

/// Read `values` (`columns` wide) at fractional `(x, y)`, clamped to its edges.
fn bilinear<T: Copy>(values: &[T], columns: usize, x: f32, y: f32, lerp: impl Fn(T, T, f32) -> T) -> Option<T> {
    let rows = values.len() / columns.max(1);
    if columns == 0 || rows == 0 {
        return None;
    }
    let x = x.clamp(0.0, (columns - 1) as f32);
    let y = y.clamp(0.0, (rows - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(columns - 1), (y0 + 1).min(rows - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |c: usize, r: usize| values.get(r * columns + c).copied();
    let top = lerp(at(x0, y0)?, at(x1, y0)?, fx);
    let bottom = lerp(at(x0, y1)?, at(x1, y1)?, fx);
    Some(lerp(top, bottom, fy))
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t), mix(a[3], b[3], t)]
}

/// The calm's thinning along one axis: a factor of its Gaussian, which is separable.
fn calm_axis(offset: f32, radius: f32) -> f32 {
    let e = offset / radius;
    (-0.9 * e * e).exp()
}

/// Light every cell of `grid` for `moment`, into `frame.cells` (one per cell, row by row).
pub(crate) fn light(visit: &Visit, grid: &Grid, pane: &Pane, hand: &Hand, moment: Moment, frame: &mut Frame) {
    let air = Air::new(visit, pane, moment);
    let wakes = hand.live_wakes(moment.now);
    let taps = hand.live_taps(moment.now);
    let touched = !taps.is_empty() || !wakes.is_empty();
    let t = moment.wind;
    let level = mix(0.02, 0.82, LULL);
    let band = mix(0.26, 0.03, EDGE);
    let floor = 0.55f32.mul_add(-TAIL, level - band);

    // The samples: every SAMPLE_STRIDE-th column and one past the last, padded to the lanes.
    let columns = (grid.columns.div_ceil(SAMPLE_STRIDE) + 1).next_multiple_of(LANES);
    let rows = grid.rows;
    let step = (grid.cell.0 * SAMPLE_STRIDE as f32, grid.cell.1);
    let first = grid.centre(0, 0);
    let at = |c: usize, r: usize| (step.0.mul_add(c as f32, first.0), step.1.mul_add(r as f32, first.1));

    // The bends and eddies, coarsely.
    let sway_columns = (columns - 1).div_ceil(SWAY_STRIDE) + 1;
    let sway_rows = (rows - 1).div_ceil(SWAY_STRIDE) + 1;
    frame.sway.clear();
    for r in 0..sway_rows {
        for c in 0..sway_columns {
            frame.sway.push(air.sway(at(c * SWAY_STRIDE, r * SWAY_STRIDE)));
        }
    }

    // The calm, by its two axes.
    let calm_x: Vec<f32> = (0..columns).map(|c| calm_axis(at(c, 0).0 - pane.calm.0, CALM_RADII.0)).collect();

    let n = columns * rows;
    let s = &mut frame.samples;
    s.clear(n);
    let w = &mut frame.rows;
    w.size(columns);
    let fine_size = 0.38 * air.gust.1;
    let fold = CURL * 0.9 * air.gust.1;
    let crease = 0.3 * air.gust.1;
    let gain = mix(0.38, 0.62, DETAIL);
    for r in 0..rows {
        let calm_y = calm_axis(at(0, r).1 - pane.calm.1, CALM_RADII.1);
        // What the hand has done to each sample, and where its air comes from.
        for c in 0..columns {
            let p = at(c, r);
            let (mut push, mut glow, mut warm) = ([0.0f32; 4], (0.0, 0.0), 0.0f32);
            if touched {
                // Read a little off true, so that a ripple's outline is never a drawn circle.
                let near = (
                    24.0f32.mul_add(noise2(p.0 / 95.0, p.1 / 95.0, &air.near[0]), p.0),
                    24.0f32.mul_add(noise2(p.0 / 95.0 + 9.1, p.1 / 95.0 + 9.1, &air.near[1]), p.1),
                );
                (push, glow) = taps_at(&taps, near);
                warm = warmth_at(&wakes, near);
                push[2] += 0.25 * warm;
                push[3] += 0.05 * warm;
            }
            let sway = bilinear(
                &frame.sway,
                sway_columns,
                c as f32 / SWAY_STRIDE as f32,
                r as f32 / SWAY_STRIDE as f32,
                lerp4,
            )
            .unwrap_or_default();
            // Air that has been pushed shows here what was over there.
            let riding = air.riding((p.0 - push[0], p.1 - push[1]), (sway[0], sway[1]));
            if let (Some(fx), Some(fy), Some(ex), Some(ey), Some(pull), Some(gust)) = (
                w.fold.0.get_mut(c),
                w.fold.1.get_mut(c),
                w.eddy.0.get_mut(c),
                w.eddy.1.get_mut(c),
                w.pull.get_mut(c),
                w.gust.get_mut(c),
            ) {
                // The air curls as it travels: small eddies carried on large.
                *fx = fold.mul_add(sway[2], riding.0);
                *fy = fold.mul_add(sway[3], riding.1);
                *ex = sway[2];
                *ey = sway[3];
                // Roused air curls further.
                *pull = CURL * 0.9f32.mul_add(push[3].min(1.5), 1.0);
                *gust = push[2];
            }
            let calm = (1.0 - CALM_FLOOR).mul_add(-calm_x.get(c).copied().unwrap_or(0.0) * calm_y, 1.0);
            // What the hand does shows at once, however little of the wind has arrived.
            let shown = moment.reveal.max(2.0f32.mul_add(push[3], warm).min(1.0));
            s.mask.push(calm * shown);
            s.warm.push(warm);
            s.roused.push(push[3]);
            s.glow.0.push(glow.0);
            s.glow.1.push(glow.1);
        }
        // Fine folds, then the smallest, two or three rows across, so no edge runs straight
        // down the rows.
        for i in 0..columns {
            if let (Some(ax), Some(ay), Some(fx), Some(fy)) =
                (w.ax.get_mut(i), w.ay.get_mut(i), w.fold.0.get(i), w.fold.1.get(i))
            {
                *ax = fx / fine_size;
                *ay = fy / fine_size;
            }
        }
        swirl_row(&w.ax, &w.ay, &air.fine[0], &mut w.n0);
        for v in w.ax.iter_mut().chain(w.ay.iter_mut()) {
            *v += 13.1;
        }
        swirl_row(&w.ax, &w.ay, &air.fine[1], &mut w.n1);
        s.fine.extend_from_slice(&w.n1);
        for i in 0..columns {
            if let (Some(ax), Some(ay), Some(fx), Some(fy), Some(f0), Some(f1)) = (
                w.ax.get_mut(i),
                w.ay.get_mut(i),
                w.fold.0.get(i),
                w.fold.1.get(i),
                w.n0.get(i),
                w.n1.get(i),
            ) {
                *ax = crease.mul_add(*f0, *fx) / 24.0;
                *ay = crease.mul_add(*f1, *fy) / 24.0;
            }
        }
        // The fine folds are kept in the curled buffers until the tiny ones are known.
        w.curled.0.copy_from_slice(&w.n0);
        w.curled.1.copy_from_slice(&w.n1);
        swirl_row(&w.ax, &w.ay, &air.tiny[0], &mut w.n0);
        for v in w.ax.iter_mut().chain(w.ay.iter_mut()) {
            *v += 5.3;
        }
        swirl_row(&w.ax, &w.ay, &air.tiny[1], &mut w.n1);
        s.tiny.extend_from_slice(&w.n0);
        for i in 0..columns {
            if let (Some(cx), Some(cy), Some(fx), Some(fy), Some(ex), Some(ey), Some(tx), Some(ty), Some(pull)) = (
                w.curled.0.get_mut(i),
                w.curled.1.get_mut(i),
                w.fold.0.get(i),
                w.fold.1.get(i),
                w.eddy.0.get(i),
                w.eddy.1.get(i),
                w.n0.get(i),
                w.n1.get(i),
                w.pull.get(i),
            ) {
                let (fine_x, fine_y) = (*cx, *cy);
                // Back from the folded frame to the wind's own, then curled.
                let q = (fx - fold * ex + 0.85 * air.carried, fy - fold * ey);
                let by = |e: f32, f: f32, ti: f32| pull * air.gust.1.mul_add(0.9f32.mul_add(e, 0.3 * f), DETAIL * 30.0 * ti);
                *cx = q.0 + by(*ex, fine_x, *tx);
                *cy = q.1 + by(*ey, fine_y, *ty);
            }
        }
        s.curled.0.extend_from_slice(&w.curled.0);
        s.curled.1.extend_from_slice(&w.curled.1);

        // The gusts in the wind's own frame: longer along it than across, four sizes
        // travelling nearly together, thin strands between, and long swells they gather on.
        let carried = air.carried;
        let mut size = (0.85 * air.gust.0, 0.75 * air.gust.1);
        let (mut weight, mut power) = (1.0f32, 0.0f32);
        w.n1.fill(0.0);
        for (k, plane) in air.octaves.iter().enumerate() {
            let lead = 0.1f32.mul_add(k as f32, 1.0) * carried;
            for ((ax, ay), (cx, cy)) in
                w.ax.iter_mut().zip(w.ay.iter_mut()).zip(w.curled.0.iter().zip(&w.curled.1))
            {
                *ax = (cx - lead) / size.0;
                *ay = cy / size.1;
            }
            swirl_row(&w.ax, &w.ay, plane, &mut w.n0);
            for (sum, v) in w.n1.iter_mut().zip(&w.n0) {
                *sum = weight.mul_add(*v, *sum);
            }
            power += weight * weight;
            weight *= gain;
            size = (size.0 * 0.5, size.1 * 0.58);
        }
        let norm = 1.0 / power.sqrt();
        for v in &mut w.n1 {
            *v *= norm;
        }
        for ((ax, ay), (cx, cy)) in w.ax.iter_mut().zip(w.ay.iter_mut()).zip(w.curled.0.iter().zip(&w.curled.1)) {
            *ax = 1.15f32.mul_add(-carried, *cx) / (0.7 * air.gust.0);
            *ay = cy / (0.95 * air.gust.1);
        }
        swirl_row(&w.ax, &w.ay, &air.strand, &mut w.n0);
        for (sum, v) in w.n1.iter_mut().zip(&w.n0) {
            let strand = 1.0 - v.abs();
            *sum += WISP * 0.5 * (strand * strand * strand - 0.35);
        }
        // A swell's crest runs now ahead of its time and now behind it: it keeps no beat.
        let span = air.span;
        for (ax, (ay, cy)) in w.ax.iter_mut().zip(w.ay.iter_mut().zip(&w.curled.1)) {
            *ax = t * 0.045;
            *ay = cy / (2.5 * air.gust.1);
        }
        noise_row(&w.ax, &w.ay, &air.swell, &mut w.n0);
        for ((sum, drift), cx) in w.n1.iter_mut().zip(&w.n0).zip(&w.curled.0) {
            *sum += SWEEP * cosine((0.5 * span).mul_add(*drift, cx - carried) * (TAU / span));
        }
        s.still.extend_from_slice(&w.n1);
        s.field.extend(w.n1.iter().zip(&w.gust).map(|(a, b)| a + b));
    }

    // Light starts at once on the side a gust heads for and thins out behind, so a still
    // frame shows which way the wind goes: what lies 56 points ahead is read off the samples.
    let ahead = (56.0 * air.heading.0 / step.0, 56.0 * air.heading.1 / step.1);
    for r in 0..rows {
        for c in 0..columns {
            let index = r * columns + c;
            let get = |v: &Vec<f32>| v.get(index).copied().unwrap_or(0.0);
            let (field, still, warm) = (get(&s.field), get(&s.still), get(&s.warm));
            let blowing = field > floor;
            let mut lit = 0.0f32;
            let mut streaks = 0.0f32;
            if blowing || warm > 0.0 {
                // Within a gust, thin quick streaks, as wind draws on water.
                let thread = noise2(
                    1.6f32.mul_add(-air.carried, get(&s.curled.0)) / 140.0,
                    get(&s.curled.1) / 30.0,
                    &air.thread,
                );
                streaks = 1.0 - smoothstep(0.0, 0.5, thread.abs());
            }
            if blowing {
                let next = bilinear(&s.still, columns, c as f32 + ahead.0, r as f32 + ahead.1, mix).unwrap_or(still);
                let leaving = smoothstep(-0.05, 0.07, next - still);
                let on = smoothstep((0.55 * TAIL).mul_add(-leaving, level - band), level + band, field);
                let broad = on
                    * on
                    * mix(0.45, 1.0, ((field - level) / 0.45).clamp(0.0, 1.0))
                    * 0.18f32.mul_add(get(&s.fine), 0.82);
                lit = broad * mix(1.0, 0.95f32.mul_add(streaks, 0.25), STREAK);
            }
            if warm > 0.0 {
                // Where the hand has warmed the air it shows, wind or no wind, in the air's
                // own fine grain.
                let grain = 0.3f32.mul_add(streaks - 0.4, 0.55f32.mul_add(get(&s.fine), 0.45 * get(&s.tiny)));
                lit = 1.0 - (1.0 - lit) * (0.85 * warm).mul_add(-smoothstep(-0.5, 0.25, grain), 1.0);
            }
            s.strength.push(lit * get(&s.mask));
        }
    }

    // Each cell reads its samples.
    let read = |v: &Vec<f32>, column: usize, r: usize| {
        let c0 = column / SAMPLE_STRIDE;
        let fx = (column % SAMPLE_STRIDE) as f32 / SAMPLE_STRIDE as f32;
        let a = v.get(r * columns + c0).copied().unwrap_or(0.0);
        let b = v.get(r * columns + c0 + 1).copied().unwrap_or(a);
        mix(a, b, fx)
    };
    frame.strength.clear();
    for r in 0..rows {
        for column in 0..grid.columns {
            frame.strength.push(read(&s.strength, column, r));
        }
    }

    let count = Look::characters(visit.look).chars().count().max(1) as u32;
    frame.cells.clear();
    frame.cells.resize(grid.len(), Lit::default());
    for row in 0..rows {
        for column in 0..grid.columns {
            let index = row * grid.columns + column;
            let strength = frame.strength.get(index).copied().unwrap_or(0.0);
            let glow = (read(&s.glow.0, column, row), read(&s.glow.1, column, row));
            let cell = pcg((column as u32).wrapping_mul(0x9E37_79B9) ^ pcg(row as u32 ^ visit.seed));
            let pace = pcg(cell);
            // Every cell glows with its neighbours, some a little more than others.
            let glow = (glow.0 * 0.35f32.mul_add(unit(pcg(pace ^ 0x51)), 0.65), glow.1);
            if glow.0 < 0.04 && glow.1 < 0.015 {
                // No wind on the cell or beside it: only its resting light is left, which
                // cannot be seen, so it draws nothing.
                let mut most = 0.0f32;
                for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
                    for c in column.saturating_sub(1)..=(column + 1).min(grid.columns - 1) {
                        most = most.max(frame.strength.get(r * grid.columns + c).copied().unwrap_or(0.0));
                    }
                }
                if most <= 0.0 {
                    continue;
                }
            }
            let mask = read(&s.mask, column, row);
            let rouse = read(&s.roused, column, row);
            // Each cell steps through the set at its own pace, from its own place in it; a
            // roused one runs on through it.
            let steps = t.mul_add(FLICK * 0.8f32.mul_add(unit(pace), 0.6), unit(pcg(pace))).floor()
                + 6.0f32.mul_add(rouse, unit(pcg(pace ^ 0x33))).floor();
            let glyph = ((cell >> 8).wrapping_add(steps.max(0.0) as u32) % count) as u16;
            let light = (strength * CAUGHT).min(1.0) + REST * mask;
            // The colour comes in whole and goes out quickly: halfway is only mud.
            let tint = smoothstep(0.04, 0.3, glow.0);
            let bright = light.min(1.0) * mix(1.0, HEADROOM, tint);
            let left = 0.3 * glow.1 * mask;
            let alpha = ((bright + left) / HEADROOM).min(1.0);
            let tint = if bright + left > 0.0 { tint.mul_add(bright, left) / (bright + left) } else { 0.0 };
            if let Some(lit) = frame.cells.get_mut(index) {
                *lit = Lit { alpha, tint, glyph };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Pane {
        Pane { width: 1060.0, height: 880.0, calm: (530.0, 700.0) }
    }

    #[test]
    fn a_visit_never_repeats_the_look_before_it() {
        for seed in 0..64 {
            for last in Look::ALL {
                assert_ne!(Visit::new(seed, Some(last)).look, last);
            }
        }
    }

    #[test]
    fn the_grid_is_centred_in_whole_device_pixels() {
        let grid = Grid::new(1060.0, 880.0, 2.0).unwrap();
        assert_eq!(grid.cell, (6.5, 13.0));
        assert_eq!((grid.columns, grid.rows), (164, 68));
        assert!(Grid::new(100.0, 50.0, 2.0).is_none(), "too small for a field");
    }

    /// The calm ellipse: behind the composer the field thins to 30 % and far from it it is
    /// whole; the same wind lit at the calm's heart and away from it differs by that much.
    #[test]
    fn the_calm_thins_the_field_behind_the_composer() {
        let visit = Visit::new(7, None);
        let grid = Grid::new(1060.0, 880.0, 2.0).unwrap();
        let mut out = Frame::default();
        let calm_mask = |calm: (f32, f32), out: &mut Frame| {
            let pane = Pane { calm, ..pane() };
            light(&visit, &grid, &pane, &Hand::default(), Moment { wind: 20.0, now: 20.0, reveal: 1.0 }, out);
            // The sum of light in the cells around the pane's middle.
            let mut sum = 0.0;
            for row in 30..38 {
                for column in 70..94 {
                    sum += out.cells[row * grid.columns + column].alpha;
                }
            }
            sum
        };
        let away = calm_mask((-5_000.0, -5_000.0), &mut out);
        let behind = calm_mask((530.0, 440.0), &mut out);
        assert!(away > 0.0, "some wind blows in the middle at this moment");
        let ratio = behind / away;
        assert!((0.25..0.45).contains(&ratio), "thinned to about 30 %: {ratio}");
    }

    /// The taps skip a point ahead of the front by the heart's farthest reach.
    #[test]
    fn a_heart_never_reaches_past_its_bound() {
        for step in 0..3_600 {
            let angle = step as f32 * (TAU / 3_600.0);
            let reach = heart((angle.cos(), angle.sin()));
            assert!((0.2..HEART_MOST).contains(&reach), "{angle}: {reach}");
        }
    }

    #[test]
    fn every_third_click_is_a_heart() {
        let mut hand = Hand::default();
        for at in 0..3 {
            hand.tapped(10.0, 10.0, at as f32 * 0.1);
        }
        let forces: Vec<f32> = hand.taps.iter().map(|t| t.force).collect();
        assert_eq!(forces, [1.0, 1.0, -1.15]);
        assert!(hand.rippling(1.0));
        assert!(!hand.rippling(4.0));
    }

    #[test]
    fn a_quick_stroke_warms_and_a_slow_one_does_not() {
        let mut quick = Hand::default();
        let mut slow = Hand::default();
        let mut warmed = (false, false);
        for i in 0..20 {
            let at = i as f32 * 0.016;
            warmed.0 |= quick.moved(i as f32 * 20.0, 100.0, at);
            warmed.1 |= slow.moved(i as f32 * 0.5, 100.0, at);
        }
        assert!(warmed.0 && !warmed.1);
    }

    #[test]
    fn nothing_lights_before_the_wind_arrives() {
        let visit = Visit::new(3, None);
        let grid = Grid::new(800.0, 600.0, 2.0).unwrap();
        let mut out = Frame::default();
        let p = Pane { width: 800.0, height: 600.0, calm: (400.0, 400.0) };
        light(&visit, &grid, &p, &Hand::default(), Moment { wind: 0.0, now: 0.0, reveal: 0.0 }, &mut out);
        assert!(out.cells.iter().all(|lit| lit.alpha == 0.0));
    }
    /// The wind's cost a frame on a 1060 × 880 pane at 2×, the size the e2e window draws: 600
    /// frames after 60 of warm-up, calm and with a ripple running. Run by hand, in release:
    /// `cargo test -p slopty-shell --release --lib wind_frame_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn wind_frame_cost() {
        const FRAMES: usize = 600;
        const WARM: usize = 60;
        let visit = Visit::new(7, None);
        let grid = Grid::new(1060.0, 880.0, 2.0).unwrap();
        let mut frame = Frame::default();
        // With taps, the clock loops within their life so every frame lights a ripple.
        let mut measure = |hand: &Hand, label: &str, span: usize| {
            let mut samples: Vec<std::time::Duration> = (0..FRAMES + WARM)
                .map(|index| {
                    let now = 10.2 + (index % span) as f32 * 0.04;
                    let moment = Moment { wind: now, now, reveal: 1.0 };
                    let started = std::time::Instant::now();
                    light(&visit, &grid, &pane(), hand, moment, &mut frame);
                    started.elapsed()
                })
                .skip(WARM)
                .collect();
            samples.sort_unstable();
            let at = |q: f32| samples[((samples.len() - 1) as f32 * q) as usize].as_secs_f64() * 1e3;
            let lit = frame.cells.iter().filter(|cell| cell.alpha > 0.0).count();
            println!(
                "MEASURE wind {label}, {} cells, {lit} lit (p50 / p95 / p99 ms): {:.3} / {:.3} / {:.3}",
                grid.len(),
                at(0.5),
                at(0.95),
                at(0.99),
            );
        };
        measure(&Hand::default(), "calm", FRAMES + WARM);
        let mut hand = Hand::default();
        for at in 0..3 {
            hand.tapped(500.0 + at as f32 * 20.0, 300.0, 10.0 + at as f32 * 0.1);
        }
        measure(&hand, "with three taps rippling", 80);
    }
}
