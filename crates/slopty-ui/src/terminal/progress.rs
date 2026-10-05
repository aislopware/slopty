//! What a program's progress report (`OSC 9;4`) says, in the kit's progress language
//! ([`crate::kit::progress`]).
//!
//! The terminal draws no chrome of its own for it. A report used to be a square line along the
//! terminal's top edge, which read as a stray rule. Now the tile's header shows it, where a tile
//! says how it stands: the kit's bar with its figure, beside the title.

use slopty_proto::terminal::{Progress as Report, ProgressState};

use crate::kit::progress::Progress;

/// What `report` shows, or `None` when nothing is reported.
///
/// A figure is its share. With no figure, a running report is under way with its share unknown,
/// and a failed or paused one has no share.
#[must_use]
pub(crate) fn shown(report: Report) -> Option<Progress> {
    let share = report.percent.map(|p| f32::from(p.min(100)) / 100.0);
    match report.state {
        ProgressState::None => None,
        ProgressState::Set => Some(share.map_or(Progress::Busy, Progress::Share)),
        ProgressState::Indeterminate => Some(Progress::Busy),
        ProgressState::Error => Some(Progress::Failed(share)),
        ProgressState::Paused => Some(Progress::Paused(share)),
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::terminal::{Progress as Report, ProgressState};

    use super::shown;
    use crate::kit::progress::Progress;

    fn report(state: ProgressState, percent: Option<u8>) -> Report {
        Report { state, percent }
    }

    /// Each state shows as its kind of progress, a figure is the share, never past the whole,
    /// and an unknown share is under way.
    #[test]
    fn a_report_shows_as_the_kits_progress() {
        assert_eq!(shown(Report::default()), None, "nothing reported");
        assert_eq!(shown(report(ProgressState::Set, Some(50))), Some(Progress::Share(0.5)));
        assert_eq!(shown(report(ProgressState::Set, Some(250))), Some(Progress::Share(1.0)));
        assert_eq!(shown(report(ProgressState::Set, None)), Some(Progress::Busy));
        assert_eq!(shown(report(ProgressState::Indeterminate, None)), Some(Progress::Busy));
        assert_eq!(
            shown(report(ProgressState::Error, Some(80))),
            Some(Progress::Failed(Some(0.8)))
        );
        assert_eq!(shown(report(ProgressState::Paused, None)), Some(Progress::Paused(None)));
    }
}
