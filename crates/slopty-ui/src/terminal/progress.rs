//! The bar a program's progress report (`OSC 9;4`) draws along the terminal's top edge.
//!
//! An indeterminate report sweeps a segment across at the working mark's pace (twelve steps a
//! second), stepping rather than gliding, and stands still under Reduce Motion.

use std::time::Duration;

use gpui::{Div, InteractiveElement as _, ParentElement as _, Styled as _, div, px, relative};
use slopty_proto::terminal::{Progress, ProgressState};
use slopty_theme::{Rgb, Theme, alpha};

use crate::colors::hsla_alpha;
use crate::icons::SPIN_STEP;

/// Steps an indeterminate bar's segment takes to cross the edge: two seconds at the working
/// mark's pace.
pub(super) const SWEEP_STEPS: u32 = 24;

/// The share of the edge an indeterminate bar's segment covers.
const SEGMENT: f32 = 0.3;

/// What the bar fills: the tone, and the span of the edge as fractions of its width.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Fill {
    /// The state's tone: the accent while it runs, `warn` paused, `error` failed.
    pub tone: Rgb,
    /// How much of the tone shows: all of it, but for a sweep standing still.
    pub alpha: f32,
    /// Where the fill starts.
    pub start: f32,
    /// How much of the edge it covers.
    pub width: f32,
}

/// The fill for `progress` at `step` of an indeterminate sweep; `None` with nothing to show.
///
/// A `step` of `None` is a sweep standing still (Reduce Motion), which fills the edge set
/// back. A report without a figure fills the edge too: a failure or a pause that never said
/// how far.
#[must_use]
pub(super) fn fill(theme: &Theme, progress: Progress, step: Option<u32>) -> Option<Fill> {
    let s = &theme.surfaces;
    let tone = match progress.state {
        ProgressState::None => return None,
        ProgressState::Set | ProgressState::Indeterminate => s.accent,
        ProgressState::Error => s.error,
        ProgressState::Paused => s.warn,
    };
    if progress.state != ProgressState::Indeterminate {
        let done = progress.percent.map_or(1.0, |p| f32::from(p.min(100)) / 100.0);
        return Some(Fill { tone, alpha: 1.0, start: 0.0, width: done });
    }
    let Some(step) = step else {
        return Some(Fill { tone, alpha: alpha::STRONG, start: 0.0, width: 1.0 });
    };
    // The segment enters from the left edge and leaves past the right one.
    #[expect(clippy::cast_precision_loss, reason = "a step under two dozen")]
    let at = (step % SWEEP_STEPS) as f32 / SWEEP_STEPS as f32;
    let lead = at.mul_add(1.0 + SEGMENT, -SEGMENT);
    let (from, to) = (lead.max(0.0), (lead + SEGMENT).min(1.0));
    Some(Fill { tone, alpha: 1.0, start: from, width: (to - from).max(0.0) })
}

/// The step of a sweep that began `since` ago, [`SPIN_STEP`] apiece.
///
/// Read from the clock, so a step not drawn on time (a key waiting for its echo held it) is
/// where it would have been.
#[must_use]
pub(super) fn sweep_step(since: Duration) -> u32 {
    let steps = since.as_nanos().checked_div(SPIN_STEP.as_nanos()).unwrap_or(0);
    u32::try_from(steps.checked_rem(u128::from(SWEEP_STEPS)).unwrap_or(0)).unwrap_or(0)
}

/// The bar along the terminal's top edge, `spacing.xxs` tall; `None` with nothing to fill.
#[must_use]
pub(super) fn bar(theme: &Theme, fill: Option<Fill>) -> Option<Div> {
    let fill = fill?;
    Some(
        div()
            .debug_selector(|| "terminal-progress".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(theme.spacing.xxs))
            .child(
                div()
                    .debug_selector(|| "terminal-progress-fill".to_owned())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(fill.start))
                    .w(relative(fill.width))
                    .bg(hsla_alpha(fill.tone, fill.alpha)),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(state: ProgressState, percent: Option<u8>) -> Progress {
        Progress { state, percent }
    }

    /// Each state wears its tone, a figure is the share filled, and a report without one fills
    /// the edge.
    #[test]
    fn a_report_fills_its_share_in_its_states_tone() {
        let theme = Theme::default();
        let s = theme.surfaces;
        assert_eq!(fill(&theme, Progress::default(), None), None, "nothing reported");
        let half = fill(&theme, progress(ProgressState::Set, Some(50)), None).unwrap();
        assert_eq!((half.tone, half.start, half.width, half.alpha), (s.accent, 0.0, 0.5, 1.0));
        let failed = fill(&theme, progress(ProgressState::Error, Some(80)), None).unwrap();
        assert_eq!((failed.tone, failed.width), (s.error, 0.8));
        let paused = fill(&theme, progress(ProgressState::Paused, None), None).unwrap();
        assert_eq!((paused.tone, paused.width), (s.warn, 1.0), "no figure: the whole edge");
        let over = fill(&theme, progress(ProgressState::Set, Some(250)), None).unwrap();
        assert!((over.width - 1.0).abs() < f32::EPSILON, "never past the edge");
    }

    /// An indeterminate report sweeps a segment in from the left and out past the right, and
    /// stands still over the whole edge, set back, when it may not move.
    #[test]
    fn an_indeterminate_report_sweeps_or_stands_set_back() {
        let theme = Theme::default();
        let working = progress(ProgressState::Indeterminate, None);
        let still = fill(&theme, working, None).unwrap();
        assert_eq!((still.start, still.width, still.alpha), (0.0, 1.0, alpha::STRONG));
        let spans: Vec<(f32, f32)> = (0..SWEEP_STEPS)
            .map(|step| fill(&theme, working, Some(step)).unwrap())
            .map(|f| (f.start, f.width))
            .collect();
        assert_eq!(spans[0], (0.0, 0.0), "the first step enters from off the left edge");
        assert!(spans.windows(2).all(|w| w[1].0 >= w[0].0), "it only moves right: {spans:?}");
        assert!(spans.iter().all(|(start, width)| start + width <= 1.0 + f32::EPSILON));
        assert!(spans.iter().any(|(_, width)| (width - SEGMENT).abs() < 1e-6), "whole mid-edge");
        assert_eq!(fill(&theme, working, Some(SWEEP_STEPS)), fill(&theme, working, Some(0)));
    }

    #[test]
    fn the_sweep_steps_at_the_working_marks_pace() {
        assert_eq!(sweep_step(Duration::ZERO), 0);
        assert_eq!(sweep_step(SPIN_STEP), 1);
        assert_eq!(sweep_step(SPIN_STEP * (SWEEP_STEPS + 2)), 2, "and wraps round");
    }
}
