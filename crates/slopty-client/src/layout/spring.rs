//! Animations: niri's critically damped springs and its two easing curves, in closed form.
//!
//! Ported from niri v26.04 `src/animation/{mod,spring}.rs` (itself after libadwaita's
//! `adw-spring-animation.c`). An animation is a pure function of time: the caller owns the
//! clock and asks for the value at an instant, so a test can be its own clock.

use std::time::Duration;

/// A spring's constants: mass 1, damping from the ratio, and the distance at which it counts
/// as at rest.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SpringParams {
    /// Damping coefficient (`ratio × 2√(mass·stiffness)`).
    pub damping: f64,
    /// Mass; always 1.
    pub mass: f64,
    /// Stiffness.
    pub stiffness: f64,
    /// Distance from the target at which the spring is at rest.
    pub epsilon: f64,
}

impl SpringParams {
    /// Constants for a damping ratio (1 is critical), a stiffness and a rest epsilon.
    #[must_use]
    pub fn new(damping_ratio: f64, stiffness: f64, epsilon: f64) -> Self {
        let damping_ratio = damping_ratio.max(0.0);
        let stiffness = stiffness.max(0.0);
        let epsilon = epsilon.max(0.0);
        let mass = 1.0;
        let critical_damping = 2.0 * (mass * stiffness).sqrt();
        Self { damping: damping_ratio * critical_damping, mass, stiffness, epsilon }
    }
}

/// A spring from `from` to `to`, leaving with `initial_velocity` (units per second).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Spring {
    /// Start value.
    pub from: f64,
    /// Rest value.
    pub to: f64,
    /// Velocity at the start, units per second.
    pub initial_velocity: f64,
    /// Constants.
    pub params: SpringParams,
}

impl Spring {
    /// The value `t` after the start.
    #[must_use]
    pub fn value_at(&self, t: Duration) -> f64 {
        self.oscillate(t.as_secs_f64())
    }

    /// How long until the spring is at rest (within epsilon for good).
    #[must_use]
    pub fn duration(&self) -> Duration {
        const DELTA: f64 = 0.001;
        let beta = self.params.damping / (2.0 * self.params.mass);
        if beta.abs() <= f64::EPSILON || beta < 0.0 {
            return Duration::MAX;
        }
        if (self.to - self.from).abs() <= f64::EPSILON {
            return Duration::ZERO;
        }
        let omega0 = (self.params.stiffness / self.params.mass).sqrt();
        // The envelope's time to fall under epsilon: exact enough for the critical and the
        // oscillating cases, the first guess for the overdamped one.
        let mut x0 = -self.params.epsilon.ln() / beta;
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) || beta < omega0 {
            return secs(x0);
        }
        // Overdamped decays slower than its envelope: Newton on the curve itself.
        let mut y0 = self.oscillate(x0);
        let m = (self.oscillate(x0 + DELTA) - y0) / DELTA;
        let mut x1 = (self.to - y0 + m * x0) / m;
        let mut y1 = self.oscillate(x1);
        let mut i = 0_u32;
        loop {
            if (self.to - y1).abs() <= self.params.epsilon {
                break;
            }
            if i > 1000 {
                return Duration::ZERO;
            }
            x0 = x1;
            y0 = y1;
            let m = (self.oscillate(x0 + DELTA) - y0) / DELTA;
            x1 = (self.to - y0 + m * x0) / m;
            y1 = self.oscillate(x1);
            if !y1.is_finite() {
                return secs(x0);
            }
            i = i.saturating_add(1);
        }
        secs(x1)
    }

    /// How long until the spring first reaches its target, if it does within 3 s.
    #[must_use]
    pub fn clamped_duration(&self) -> Option<Duration> {
        let beta = self.params.damping / (2.0 * self.params.mass);
        if beta.abs() <= f64::EPSILON || beta < 0.0 {
            return Some(Duration::MAX);
        }
        if (self.to - self.from).abs() <= f64::EPSILON {
            return Some(Duration::ZERO);
        }
        let mut i = 1_u16;
        let mut y = self.oscillate(f64::from(i) / 1000.0);
        while (self.to - self.from > f64::EPSILON && self.to - y > self.params.epsilon)
            || (self.from - self.to > f64::EPSILON && y - self.to > self.params.epsilon)
        {
            if i > 3000 {
                return None;
            }
            i = i.saturating_add(1);
            y = self.oscillate(f64::from(i) / 1000.0);
        }
        Some(Duration::from_millis(u64::from(i)))
    }

    /// Position `t` seconds after the start: `m·ẍ + b·ẋ + k·x = 0` solved in closed form.
    fn oscillate(&self, t: f64) -> f64 {
        let b = self.params.damping;
        let m = self.params.mass;
        let k = self.params.stiffness;
        let v0 = self.initial_velocity;
        let beta = b / (2.0 * m);
        let omega0 = (k / m).sqrt();
        let x0 = self.from - self.to;
        let envelope = (-beta * t).exp();
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
            // Critically damped.
            envelope.mul_add(beta.mul_add(x0, v0).mul_add(t, x0), self.to)
        } else if beta < omega0 {
            // Underdamped.
            let omega1 = omega0.mul_add(omega0, -(beta * beta)).sqrt();
            let wave = (beta.mul_add(x0, v0) / omega1)
                .mul_add((omega1 * t).sin(), x0 * (omega1 * t).cos());
            envelope.mul_add(wave, self.to)
        } else {
            // Overdamped.
            let omega2 = beta.mul_add(beta, -(omega0 * omega0)).sqrt();
            let decay = (beta.mul_add(x0, v0) / omega2)
                .mul_add((omega2 * t).sinh(), x0 * (omega2 * t).cosh());
            envelope.mul_add(decay, self.to)
        }
    }
}

/// Seconds as a `Duration`, saturating on nonsense.
fn secs(s: f64) -> Duration {
    Duration::try_from_secs_f64(s.max(0.0)).unwrap_or(Duration::MAX)
}

/// An easing curve over `0..=1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(clippy::enum_variant_names, reason = "the curves' standard names, as niri spells them")]
pub enum Curve {
    /// `1 - (1 - x)²`.
    EaseOutQuad,
    /// `1 - (1 - x)³`.
    EaseOutCubic,
    /// `1 - 2^(-10x)`.
    EaseOutExpo,
}

impl Curve {
    /// The curve at `x` (clamped to `0..=1`).
    #[must_use]
    pub fn y(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::EaseOutQuad => (1.0 - x).mul_add(-(1.0 - x), 1.0),
            Self::EaseOutCubic => 1.0 - (1.0 - x).powi(3),
            Self::EaseOutExpo => 1.0 - (-10.0 * x).exp2(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Easing(Curve),
    Spring(Spring),
}

/// One value moving from `from` to `to`, started at a clock instant.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Animation {
    from: f64,
    to: f64,
    start: Duration,
    duration: Duration,
    kind: Kind,
}

impl Animation {
    /// A spring from `from` to `to` leaving at `velocity` (units per second), started `start`.
    #[must_use]
    pub fn spring(
        start: Duration,
        from: f64,
        to: f64,
        velocity: f64,
        params: SpringParams,
    ) -> Self {
        let spring = Spring { from, to, initial_velocity: velocity, params };
        Self { from, to, start, duration: spring.duration(), kind: Kind::Spring(spring) }
    }

    /// An eased move from `from` to `to` over `duration`, started `start`.
    #[must_use]
    pub const fn ease(
        start: Duration,
        from: f64,
        to: f64,
        duration: Duration,
        curve: Curve,
    ) -> Self {
        Self { from, to, start, duration, kind: Kind::Easing(curve) }
    }

    /// The value at `now`: `from` at or before the start, `to` once done, and a spring's
    /// output clamped to `from ± 10 × range` against numerical blow-ups.
    #[must_use]
    pub fn value_at(&self, now: Duration) -> f64 {
        if now <= self.start {
            return self.from;
        }
        let passed = now.saturating_sub(self.start);
        if passed >= self.duration {
            return self.to;
        }
        match self.kind {
            Kind::Easing(curve) => {
                let x = passed.as_secs_f64() / self.duration.as_secs_f64();
                curve.y(x).mul_add(self.to - self.from, self.from)
            }
            Kind::Spring(spring) => {
                let value = spring.value_at(passed);
                let range = (self.to - self.from) * 10.0;
                let (a, b) = (self.from - range, self.to + range);
                if self.from <= self.to { value.clamp(a, b) } else { value.clamp(b, a) }
            }
        }
    }

    /// Rate of change at `now`, units per second (a one-millisecond difference), for handing
    /// a retargeted animation the velocity this one had.
    #[must_use]
    pub fn velocity_at(&self, now: Duration) -> f64 {
        const STEP: Duration = Duration::from_millis(1);
        if self.is_done(now) {
            return 0.0;
        }
        let later = now.saturating_add(STEP);
        (self.value_at(later) - self.value_at(now)) / STEP.as_secs_f64()
    }

    /// Whether it has landed at `now`.
    #[must_use]
    pub fn is_done(&self, now: Duration) -> bool {
        now >= self.start.saturating_add(self.duration)
    }

    /// Where it goes.
    #[must_use]
    pub const fn to(&self) -> f64 {
        self.to
    }

    /// Where it started.
    #[must_use]
    pub const fn from(&self) -> f64 {
        self.from
    }

    /// When it started.
    #[must_use]
    pub const fn start(&self) -> Duration {
        self.start
    }

    /// How long it runs.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Shift the whole curve by `delta` (a re-based coordinate), keeping its shape.
    pub fn offset(&mut self, delta: f64) {
        self.from += delta;
        self.to += delta;
        if let Kind::Spring(spring) = &mut self.kind {
            spring.from += delta;
            spring.to += delta;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn view() -> SpringParams {
        SpringParams::new(1.0, 800.0, 0.0001)
    }

    #[test]
    fn a_critically_damped_spring_reaches_its_target_without_overshoot() {
        let spring = Spring { from: 0.0, to: 100.0, initial_velocity: 0.0, params: view() };
        let d = spring.duration();
        // √800 ≈ 28.28/s: the envelope falls under 1e-4 after −ln(1e-4)/28.28 ≈ 0.326 s.
        assert!((d.as_secs_f64() - 0.3257).abs() < 0.001, "{d:?}");
        let mut last = 0.0;
        for ms in (0..400).step_by(5) {
            let v = spring.value_at(MS(ms));
            assert!(v <= 100.0 + 1e-9, "overshoot at {ms} ms: {v}");
            assert!(v >= last - 1e-9, "monotonic at {ms} ms");
            last = v;
        }
        // The envelope ignores the critical curve's (1 + βt) factor: ~0.1 % of the way remains.
        assert!((spring.value_at(d) - 100.0).abs() < 0.15, "at rest after its duration");
        // Halfway there after about 60 ms, as niri's view movement feels.
        let at = spring.value_at(MS(60));
        assert!((40.0..70.0).contains(&at), "{at}");
    }

    #[test]
    fn an_initial_velocity_leaves_at_that_rate() {
        let spring = Spring { from: 0.0, to: 0.0001, initial_velocity: 1000.0, params: view() };
        let dt = 1e-5;
        let v = spring.oscillate(dt) / dt;
        assert!((v - 1000.0).abs() < 5.0, "{v}");
    }

    #[test]
    fn under_and_over_damped_springs_settle() {
        let under = Spring {
            from: 0.0,
            to: 1.0,
            initial_velocity: 0.0,
            params: SpringParams::new(0.5, 800.0, 0.0001),
        };
        let peak = (0..300).map(|ms| under.value_at(MS(ms))).fold(0.0, f64::max);
        assert!(peak > 1.05, "an underdamped spring overshoots: {peak}");
        assert!((under.value_at(under.duration()) - 1.0).abs() < 0.01, "and settles");
        let over = Spring {
            from: 0.0,
            to: 1.0,
            initial_velocity: 0.0,
            params: SpringParams::new(6.0, 1200.0, 0.0001),
        };
        let d = over.duration();
        assert!(d > Duration::ZERO && d < Duration::from_secs(10), "{d:?}");
        assert!((over.value_at(d) - 1.0).abs() < 0.001, "{}", over.value_at(d));
        // niri's own regression: equal from and to never produces NaN.
        let still = Spring { from: 0.0, to: 0.0, ..over };
        assert_eq!(still.duration(), Duration::ZERO, "nothing to travel");
        assert!(still.value_at(Duration::ZERO).is_finite(), "no NaN");
    }

    #[test]
    fn the_clamped_duration_is_when_the_target_is_first_reached() {
        let spring = Spring { from: 0.0, to: 1.0, initial_velocity: 0.0, params: view() };
        let clamped = spring.clamped_duration().unwrap();
        assert!(clamped > MS(100) && clamped < MS(3000), "{clamped:?}");
        assert!((spring.value_at(clamped) - 1.0).abs() <= 0.0001, "within epsilon by then");
        let before = clamped.saturating_sub(MS(1));
        assert!((spring.value_at(before) - 1.0).abs() > 0.0001, "and not a millisecond earlier");
    }

    #[test]
    fn an_animation_is_from_before_its_start_and_to_after_its_end() {
        let a = Animation::spring(MS(100), 10.0, 20.0, 0.0, view());
        assert!((a.value_at(MS(50)) - 10.0).abs() < f64::EPSILON, "before");
        assert!((a.value_at(MS(100)) - 10.0).abs() < f64::EPSILON, "at the start");
        assert!(!a.is_done(MS(200)), "still moving");
        assert!(a.is_done(MS(1000)), "landed");
        assert!((a.value_at(MS(1000)) - 20.0).abs() < f64::EPSILON, "exactly the target");
        assert!(a.velocity_at(MS(120)) > 0.0, "moving towards the target");
        assert!(a.velocity_at(MS(1000)).abs() < f64::EPSILON, "still after");
    }

    #[test]
    fn a_wild_spring_is_clamped_to_ten_ranges() {
        // A huge initial velocity against a tiny range would fly off; the output stays within
        // from ± 10 × range.
        let a = Animation::spring(Duration::ZERO, 0.0, 1.0, 1.0e6, view());
        for ms in 0..50 {
            let v = a.value_at(MS(ms));
            assert!((-10.0..=11.0).contains(&v), "{v} at {ms} ms");
        }
    }

    #[test]
    fn easing_curves_start_at_zero_and_end_at_one() {
        for curve in [Curve::EaseOutQuad, Curve::EaseOutCubic, Curve::EaseOutExpo] {
            assert!(curve.y(0.0).abs() < 1e-9, "{curve:?}");
            assert!((curve.y(1.0) - 1.0).abs() < 1e-3, "{curve:?}");
            assert!(curve.y(0.5) > 0.5, "ease out is ahead at the middle: {curve:?}");
        }
        let open = Animation::ease(Duration::ZERO, 0.0, 1.0, MS(150), Curve::EaseOutExpo);
        let mid = open.value_at(MS(75));
        assert!((mid - (1.0 - (-5.0_f64).exp2())).abs() < 1e-9, "{mid}");
        assert!(open.is_done(MS(150)), "150 ms");
    }

    #[test]
    fn an_offset_moves_the_whole_curve() {
        let mut a = Animation::spring(Duration::ZERO, 0.0, 10.0, 0.0, view());
        let before = a.value_at(MS(30));
        a.offset(5.0);
        assert!((a.value_at(MS(30)) - before - 5.0).abs() < 1e-9, "same shape, shifted");
        assert!((a.to() - 15.0).abs() < f64::EPSILON, "target moved");
    }
}
