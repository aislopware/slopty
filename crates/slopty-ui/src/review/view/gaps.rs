//! The unchanged lines between a file's hunks, folded to one line each that opens them, as
//! `MonoCode`'s diff folds its context (`docs/decisions/ui.md`, "A diff is unified, its files
//! stacked and folding").
//!
//! A hunk carries three lines of context and nothing else of the file reaches the client, so a
//! fold that opens asks the worker for the file's new side whole, by the blob the review named
//! ([`ContentRef::blob`]), through the thread's own `Expand`. The lines come once per blob and
//! are kept while the tile is open. A fold shows before the first hunk and between two; past
//! the last only once the file's length is known. A folder's review has no thread to ask
//! through, so it shows none.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::thread::ContentRef;
use slopty_proto::thread::wire::Expanded;

use super::{ReviewView, Row};
use crate::colors::hsla;
use crate::conversation::diff::{self, Line};
use crate::icons::{IconSize, Symbol};

/// The fold's words: how many lines it holds.
fn unchanged(lines: u32) -> String {
    if lines == 1 { "1 unchanged line".to_owned() } else { format!("{lines} unchanged lines") }
}

/// One stretch of the new side no hunk shows: its first and last line there, and how far the
/// old side's numbers run from the new's along it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Gap {
    pub first: u32,
    pub last: u32,
    pub shift: i64,
}

impl Gap {
    /// How many lines it holds.
    pub(super) const fn lines(self) -> u32 {
        self.last.saturating_sub(self.first).saturating_add(1)
    }
}

/// The stretch before hunk `gap` of `blocks`, or after the last when `gap` is their count and
/// the file is `total` lines long; `None` where the hunks meet or the length is not known.
pub(super) fn gap(blocks: &[diff::Block], gap: usize, total: Option<u32>) -> Option<Gap> {
    let ends = |b: &diff::Block| b.ends();
    let (first, last, shift) = if let Some(block) = blocks.get(gap) {
        let after = gap.checked_sub(1).and_then(|g| blocks.get(g)).map_or(0, |b| ends(b).1);
        let shift = i64::from(block.old_start).saturating_sub(i64::from(block.new_start));
        (after.saturating_add(1), block.new_start.checked_sub(1)?, shift)
    } else {
        let (old_end, new_end) = blocks.last().map(ends)?;
        (new_end.saturating_add(1), total?, i64::from(old_end).saturating_sub(i64::from(new_end)))
    };
    (first >= 1 && first <= last).then_some(Gap { first, last, shift })
}

impl ReviewView {
    /// The file at `at`'s new side, when the tile holds it: its blob, if this is a thread's
    /// review and the file has a new side.
    pub(super) fn new_side(&self, at: usize) -> Option<String> {
        self.own()?;
        let file = self.model.file(at)?;
        file.to.clone().filter(|_| !file.binary)
    }

    /// The lines of the file at `at`'s new side, once they came.
    pub(super) fn side_lines(&self, at: usize) -> Option<Rc<[String]>> {
        self.sides.get(&self.new_side(at)?).cloned()
    }

    /// The rows for the stretch before hunk `g` of the file at `at`: its unchanged lines, once
    /// opened and come, else the fold that opens them; nothing where the hunks meet.
    pub(super) fn push_gap(&mut self, rows: &mut Vec<Row>, at: usize, g: usize) {
        let Some(blob) = self.new_side(at) else { return };
        let Some(blocks) = self.blocks.get(&at).cloned() else { return };
        let lines = self.sides.get(&blob).cloned();
        let total = lines.as_ref().and_then(|l| u32::try_from(l.len()).ok());
        let Some(stretch) = gap(&blocks, g, total) else { return };
        let path = self.model.file(at).map(|f| f.path.clone()).unwrap_or_default();
        let open = self.unfolded.contains(&(path.clone(), g));
        match lines.filter(|_| open) {
            Some(all) => {
                let from = usize::try_from(stretch.first.saturating_sub(1)).unwrap_or(usize::MAX);
                let upto = usize::try_from(stretch.last).unwrap_or(usize::MAX);
                let texts: Vec<&str> =
                    all.get(from..upto).unwrap_or_default().iter().map(String::as_str).collect();
                let old = i64::from(stretch.first).saturating_add(stretch.shift);
                let old = u32::try_from(old).unwrap_or(stretch.first);
                let shown: Rc<[Line]> = diff::context(&path, &texts, (old, stretch.first)).into();
                for ix in 0..shown.len() {
                    rows.push(Row::Context(at, g, ix));
                }
                self.context.insert((at, g), shown);
            }
            None => rows.push(Row::Gap(at, g)),
        }
    }

    /// Open the fold before hunk `g` of the file at `at`: its lines are asked for once, and
    /// shown as they come.
    pub(super) fn unfold(&mut self, at: usize, g: usize, cx: &mut Context<Self>) {
        let (Some(thread), Some(blob)) = (self.own(), self.new_side(at)) else { return };
        let Some(path) = self.model.file(at).map(|f| f.path.clone()) else { return };
        self.unfolded.insert((path, g));
        let content = ContentRef::blob(&blob);
        let held = self.hub.update(cx, |hub, cx| hub.expanded(thread, &content, cx));
        if let Some(body) = held {
            self.side_came(&blob, &body);
        }
        self.rebuild();
        cx.notify();
    }

    /// `content` came from the worker: the lines of a file's side, when it is one a fold asked.
    pub(super) fn expanded(&mut self, content: &ContentRef, cx: &mut Context<Self>) {
        let Some(blob) = content.blob_id().map(str::to_owned) else { return };
        let Some(thread) = self.own() else { return };
        if self.sides.contains_key(&blob) {
            return;
        }
        let held = self.hub.update(cx, |hub, cx| hub.expanded(thread, content, cx));
        if let Some(body) = held {
            self.side_came(&blob, &body);
            self.rebuild();
            cx.notify();
        }
    }

    fn side_came(&mut self, blob: &str, body: &Expanded) {
        if let Expanded::Text(text) = body {
            let lines: Rc<[String]> = text.lines().map(str::to_owned).collect();
            self.sides.insert(blob.to_owned(), lines);
        }
    }

    /// The fold before hunk `g` of the file at `at`: how many lines it holds, a press away.
    pub(super) fn gap_row(&self, at: usize, g: usize, cx: &Context<Self>) -> AnyElement {
        let blocks = self.blocks.get(&at).cloned().unwrap_or_else(|| Rc::from([]));
        let total = self.side_lines(at).and_then(|l| u32::try_from(l.len()).ok());
        let Some(stretch) = gap(&blocks, g, total) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let said = unchanged(stretch.lines());
        let id = format!("review-gap-{at}-{g}");
        let selector = id.clone();
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || selector)
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Show {said}")))
            .aria_expanded(false)
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xxs))
            .map(|el| crate::kit::inset(el, theme))
            .cursor_pointer()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .hover(move |el| el.text_color(hsla(s.text_secondary)))
            .child(
                crate::icons::icon(theme, Symbol::Unfold, IconSize::Inline, hsla(s.text_muted))
                    .size(px(theme.typography.icon())),
            )
            .child(SharedString::from(said))
            .on_click(cx.listener(move |this, _ev, _w, cx| this.unfold(at, g, cx)))
            .into_any_element()
    }

    /// Line `ix` of the opened stretch before hunk `g` of the file at `at`.
    pub(super) fn context_row(&self, at: usize, g: usize, ix: usize) -> AnyElement {
        let Some(line) = self.context.get(&(at, g)).and_then(|l| l.get(ix)) else {
            return div().into_any_element();
        };
        let id = format!("review-context-{at}-{g}-{ix}");
        div()
            .debug_selector(move || id)
            .child(self.ink(at).unified_numbered(line))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::Patch;
    use slopty_proto::thread::detail::Hunk;

    use super::*;

    fn hunk(old_start: u32, new_start: u32, lines: &[&str]) -> Hunk {
        Hunk {
            old_start,
            old_lines: 0,
            new_start,
            new_lines: 0,
            heading: None,
            lines: lines.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    /// A stretch lies before the first hunk, between two, and past the last once the file's
    /// length is known; none where the hunks meet. The old side's numbers follow its shift.
    #[test]
    fn the_stretches_lie_between_the_hunks() {
        let patch = Patch {
            hunks: vec![
                hunk(5, 5, &[" a", " b", " c", "-d", "+D", "+E", " f", " g", " h"]),
                hunk(20, 21, &[" x", " y", " z", "+w", " u", " v", " t"]),
            ],
            ..Patch::default()
        };
        let blocks = diff::thread_blocks("a.rs", &patch);
        assert_eq!(gap(&blocks, 0, None), Some(Gap { first: 1, last: 4, shift: 0 }));
        assert_eq!(gap(&blocks, 1, None), Some(Gap { first: 13, last: 20, shift: -1 }));
        assert_eq!(gap(&blocks, 2, None), None, "the length is not known");
        assert_eq!(gap(&blocks, 2, Some(40)), Some(Gap { first: 28, last: 40, shift: -2 }));
        assert_eq!(gap(&blocks, 2, Some(27)), None, "the last hunk ends the file");
        let at_top = diff::thread_blocks(
            "a.rs",
            &Patch { hunks: vec![hunk(1, 1, &["+a"])], ..Patch::default() },
        );
        assert_eq!(gap(&at_top, 0, None), None, "a hunk at the top");
        assert_eq!(unchanged(1), "1 unchanged line");
    }
}
