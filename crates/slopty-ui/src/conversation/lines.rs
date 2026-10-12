//! A diff's lines drawn: the one renderer for the thread view's edits and the review tile.
//!
//! As Zed's Delta draws them: each changed line on its version-control band, the words that
//! changed in it on the stronger word wash, every line in its grammar's colours (context and
//! removed lines too), the numbers in the line number's tone, and no sign column (Delta's diff
//! markers are off by default): the band says which side a line is on.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Div, Hsla, IntoElement as _, ParentElement as _, SharedString, Styled as _,
    StyledText, TextRun, div, px,
};
use slopty_kit::colors::hsla;
use slopty_kit::highlight;
use slopty_theme::Theme;

use super::diff::{Block, Kind, Line, TAB_SPACES};

/// What every line is drawn with.
#[derive(Clone, Copy, Debug)]
pub struct Ink<'a> {
    /// The theme.
    pub theme: &'a Theme,
    /// Digits the gutter holds: the widest line number's.
    pub digits: usize,
}

impl Ink<'_> {
    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    /// The code face at the code role (Delta's code, a point over the terminal's), for lines
    /// inside a card of someone else's.
    #[must_use]
    pub fn code(&self) -> Div {
        slopty_kit::kit::typed(div().w_full().font_family(self.mono()), self.theme.roles().code)
    }

    /// A diff as a card of its own: rounded, the hairline round it, no band.
    #[must_use]
    pub fn frame(&self) -> Div {
        self.code()
            .overflow_hidden()
            .rounded(px(self.theme.radii.sm))
            .border(slopty_kit::kit::HAIR)
            .border_color(hsla(self.theme.surfaces.stroke))
    }

    /// The line's band: Delta's `version_control.added` or `.deleted`, none for context.
    #[must_use]
    pub fn tone(&self, kind: Kind) -> Option<Hsla> {
        let s = self.theme.surfaces;
        match kind {
            Kind::Added => Some(hsla(s.vc_added_line)),
            Kind::Removed => Some(hsla(s.vc_deleted_line)),
            Kind::Context => None,
        }
    }

    /// The stronger wash under a changed line's changed words.
    fn emph(&self, kind: Kind) -> Option<Hsla> {
        let s = self.theme.surfaces;
        match kind {
            Kind::Added => Some(hsla(s.vc_word_added)),
            Kind::Removed => Some(hsla(s.vc_word_deleted)),
            Kind::Context => None,
        }
    }

    /// A line number in the gutter, right-aligned in the gutter's width, at the code's size in
    /// the line number's tone, as Delta's review sets them
    /// (`.research/delta-signed-in-2026-10-12.md` §2.8).
    #[must_use]
    pub fn number(&self, n: Option<u32>) -> Div {
        let s = self.theme.surfaces;
        let size = self.theme.roles().code.size;
        let digits = f32::from(u8::try_from(self.digits.max(2)).unwrap_or(u8::MAX));
        // A tabular figure is about 0.62 of the size wide.
        let width = size * 0.62 * digits;
        slopty_kit::kit::tabular(div())
            .flex_none()
            .w(px(width))
            .flex()
            .justify_end()
            .text_size(px(size))
            .text_color(hsla(s.line_number))
            .children(n.map(|n| SharedString::from(n.to_string())))
    }

    /// A line's text in its grammar's colours, its changed words on the stronger wash.
    #[must_use]
    pub fn text(&self, line: &Line) -> AnyElement {
        let s = self.theme.surfaces;
        let (text, spans) = super::diff::detab(&line.text, line.spans.as_deref());
        let text = if text.is_empty() { " ".to_owned() } else { text };
        let emph = self.emph(line.kind).filter(|_| !line.emph.is_empty());
        let coloured = spans.filter(|sp| !sp.is_empty());
        let styled = if coloured.is_none() && emph.is_none() {
            SharedString::from(text).into_any_element()
        } else {
            let font = gpui::font(self.mono());
            let mut runs = highlight::runs(text.len(), coloured.as_deref(), &font, self.theme);
            if let Some(wash) = emph {
                let ranges = detabbed(&line.text, &line.emph);
                runs = washed(runs, &ranges, wash);
            }
            StyledText::new(SharedString::from(text)).with_runs(runs).into_any_element()
        };
        div()
            .flex_1()
            .min_w_0()
            .whitespace_normal()
            .text_color(hsla(s.text))
            .child(styled)
            .into_any_element()
    }

    /// One line in a column: one gutter, Zed's unified diff's, holding the new file's number,
    /// or the old file's for a removed line, then the text, on the line's band.
    #[must_use]
    pub fn unified(&self, line: &Line) -> Div {
        let wash = self.tone(line.kind);
        let spacing = self.theme.spacing;
        div()
            .w_full()
            .flex()
            .items_start()
            .gap(px(spacing.sm))
            .px(px(spacing.sm))
            .when_some(wash, gpui::Styled::bg)
            .child(self.number(gutter(line)))
            .child(self.text(line))
    }

    /// One line in a thread's well: the text alone on the line's band, with no gutter, as
    /// Delta's thread draws an edit; the band says which side it is on. Padded a base unit, so
    /// in a hung well ([`crate::conversation::thread`]) the code starts on the prose's edge.
    #[must_use]
    pub fn bare(&self, line: &Line) -> Div {
        div()
            .w_full()
            .flex()
            .items_start()
            .px(px(self.theme.spacing.sm))
            .when_some(self.tone(line.kind), gpui::Styled::bg)
            .child(self.text(line))
    }

    /// The divider before a hunk, on the band with no rule round it: the line git names it by
    /// (the function it is in), else where it starts in the new file.
    #[must_use]
    pub fn hunk_head(&self, block: &Block) -> Div {
        let s = self.theme.surfaces;
        div()
            .w_full()
            .px(px(self.theme.spacing.sm))
            .py(px(self.theme.spacing.xxs))
            .map(|el| slopty_kit::kit::inset(el, self.theme))
            .text_color(hsla(s.text_muted))
            .text_size(px(self.theme.typography.small()))
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .map(|el| match &block.heading {
                Some(heading) => {
                    el.font_family(self.mono()).child(SharedString::from(heading.trim().to_owned()))
                }
                None => el
                    .font_family(self.theme.typography.ui_family.clone())
                    .child(SharedString::from(format!("Line {}", block.new_start))),
            })
    }
}

/// The one number a unified diff's gutter holds for `line`: the new file's, or the old file's
/// for a removed line, as Zed's unified diff numbers it.
#[must_use]
pub const fn gutter(line: &Line) -> Option<u32> {
    if matches!(line.kind, Kind::Removed) { line.old } else { line.new }
}

/// A path as the eye looks for it, Delta's way: the file's name, then its folder.
#[must_use]
pub fn name_first(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit_once('/').map_or((trimmed, ""), |(dir, name)| (name, dir))
}

/// `ranges` of `text` moved to where they fall once its tabs are spaces ([`super::diff::detab`]).
fn detabbed(text: &str, ranges: &[std::ops::Range<usize>]) -> Vec<std::ops::Range<usize>> {
    let wider = TAB_SPACES.len().saturating_sub(1);
    let at = |byte: usize| {
        let tabs = text.get(..byte).map_or(0, |head| head.matches('\t').count());
        byte.saturating_add(tabs.saturating_mul(wider))
    };
    ranges.iter().map(|r| at(r.start)..at(r.end)).collect()
}

/// `runs` cut where `ranges` start and end, the runs inside them on `wash`.
fn washed(runs: Vec<TextRun>, ranges: &[std::ops::Range<usize>], wash: Hsla) -> Vec<TextRun> {
    let mut out = Vec::with_capacity(runs.len().saturating_add(ranges.len().saturating_mul(2)));
    let mut at = 0_usize;
    for run in runs {
        let end = at.saturating_add(run.len);
        let mut cuts: Vec<usize> =
            ranges.iter().flat_map(|r| [r.start, r.end]).filter(|&c| c > at && c < end).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut from = at;
        for to in cuts.into_iter().chain([end]) {
            let inside = ranges.iter().any(|r| r.start <= from && to <= r.end);
            out.push(TextRun {
                len: to.saturating_sub(from),
                background_color: inside.then_some(wash).or(run.background_color),
                ..run.clone()
            });
            from = to;
        }
        at = end;
    }
    out
}

/// The digits the widest line number in `blocks` takes.
#[must_use]
pub fn digits(blocks: &[Block]) -> usize {
    let widest = blocks
        .iter()
        .flat_map(|b| b.lines.iter())
        .flat_map(|l| [l.old, l.new])
        .flatten()
        .max()
        .unwrap_or(0);
    widest.checked_ilog10().map_or(1, |d| usize::try_from(d).unwrap_or(0).saturating_add(1))
}

#[cfg(test)]
mod tests {
    use gpui::{Hsla, TextRun, font};

    use super::{Ink, detabbed, gutter, name_first, washed};
    use crate::conversation::diff::{Kind, Line};

    /// A unified diff has one gutter (`.research/design-polish-2026-10-12.md` §1.9): one number
    /// per line, the new file's, or the old file's for a removed line.
    #[test]
    fn one_gutter_in_a_unified_diff() {
        let line = |kind, old, new| Line {
            kind,
            old,
            new,
            text: String::new(),
            spans: None,
            no_newline: false,
            emph: Vec::new(),
        };
        assert_eq!(gutter(&line(Kind::Context, Some(10), Some(12))), Some(12), "the new file's");
        assert_eq!(gutter(&line(Kind::Added, None, Some(13))), Some(13));
        assert_eq!(gutter(&line(Kind::Removed, Some(11), None)), Some(11), "the old file's");
    }

    /// A file reads name first, its folder after.
    #[test]
    fn a_file_reads_name_first() {
        assert_eq!(name_first("src/thread/view.rs"), ("view.rs", "src/thread"));
        assert_eq!(name_first("README.md"), ("README.md", ""));
    }

    /// The changed words' wash lands on exactly their bytes, a tab before them counted as the
    /// spaces it is drawn as, and the runs still cover the line.
    #[test]
    fn the_changed_words_are_washed_where_they_are_drawn() {
        let ranges = detabbed("\tlet a = b;", &[1..4, 5..6]);
        assert_eq!(ranges, [4..7, 8..9], "the tab is four spaces wide");
        let run = |len| TextRun {
            len,
            font: font("Menlo"),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let wash = Hsla { a: 0.34, ..Hsla::default() };
        let runs = washed(vec![run(7), run(7)], &[0..1, 5..9], wash);
        let shape: Vec<(usize, bool)> =
            runs.iter().map(|r| (r.len, r.background_color.is_some())).collect();
        assert_eq!(shape, [(1, true), (4, false), (2, true), (2, true), (5, false)]);
    }

    /// A changed line lies on Delta's version-control band, opaque, and context on nothing.
    #[test]
    fn a_changed_line_lies_on_its_version_control_band() {
        for variant in [slopty_theme::Variant::Dark, slopty_theme::Variant::Light] {
            let theme = slopty_theme::Theme::new(variant);
            let ink = Ink { theme: &theme, digits: 2 };
            let s = theme.surfaces;
            assert_eq!(ink.tone(Kind::Added), Some(slopty_kit::colors::hsla(s.vc_added_line)));
            assert_eq!(ink.tone(Kind::Removed), Some(slopty_kit::colors::hsla(s.vc_deleted_line)));
            assert_eq!(ink.tone(Kind::Context), None);
        }
    }
}
