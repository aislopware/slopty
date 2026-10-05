//! "Compare" on a file tile's conflict: the disk's text against the edit, drawn as the thread
//! view and the review tile draw a diff (`conversation::lines`), the disk's side as the old and
//! the edit as the new, so what saving would change reads as additions.
//!
//! The diff is worked out off the UI thread, since Myers' worst case is quadratic in the lines
//! that differ; the tile says "Comparing…" until it is in.

use std::ops::Range;
use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, div, px,
};
use similar::{Algorithm, DiffOp, DiffTag};
use slopty_proto::thread::Patch;
use slopty_proto::thread::detail::{Hunk, heading};

use super::FileView;
use crate::conversation::diff::{self, Block};
use crate::conversation::lines::{self, Ink};
use crate::icons::Symbol;

/// Lines of context around each change, as `git diff` gives.
const CONTEXT: usize = 3;

/// The most diff lines the comparison draws; past them it says how many more there are.
pub(super) const SHOWN_LINES: usize = 2_000;

/// What the body says while the diff is worked out.
pub(crate) const COMPARING: &str = "Comparing…";
/// What the body says when the disk and the edit hold the same text.
pub(crate) const SAME_TEXT: &str = "The disk has the same text as the edit";
/// The conflict bar's way to see the comparison.
pub(crate) const COMPARE: &str = "Compare";
/// The conflict bar's way back to the edit from the comparison.
pub(crate) const BACK_TO_EDIT: &str = "Back to edit";

/// The comparison, while the tile shows it.
#[derive(Default)]
pub(super) struct Comparing {
    /// The diff last worked out.
    shown: Option<Shown>,
    /// The diff being worked out, for what.
    working: Option<(Inputs, Task<()>)>,
}

/// What a diff is worked out from: the edit, numbered as the tile numbers them, against the
/// disk's version modified then.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Inputs {
    edit: u64,
    disk: slopty_core::WallMs,
}

/// A diff worked out, and what from.
struct Shown {
    from: Inputs,
    blocks: Rc<[Block]>,
}

/// The hunks from `old` to `new`, with `CONTEXT` lines round each and git's headings.
#[must_use]
pub(super) fn patch(old: &str, new: &str) -> Patch {
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let ops = similar::capture_diff_slices(Algorithm::Myers, &old_lines, &new_lines);
    let mut patch = Patch { hunks: Vec::new(), added: 0, removed: 0, clipped_lines: 0, full: None };
    for group in similar::group_diff_ops(ops, CONTEXT) {
        let (old_span, new_span) = span(&group);
        let mut text = Vec::new();
        for op in &group {
            let (tag, old_range, new_range) = op.as_tag_tuple();
            match tag {
                DiffTag::Equal => text.extend(marked(&old_lines, old_range, ' ')),
                DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                    let count = |r: &Range<usize>| u32::try_from(r.len()).unwrap_or(u32::MAX);
                    patch.removed = patch.removed.saturating_add(count(&old_range));
                    patch.added = patch.added.saturating_add(count(&new_range));
                    text.extend(marked(&old_lines, old_range, '-'));
                    text.extend(marked(&new_lines, new_range, '+'));
                }
            }
        }
        let number = |r: &Range<usize>| {
            u32::try_from(r.start.saturating_add(usize::from(!r.is_empty()))).unwrap_or(u32::MAX)
        };
        let count = |r: &Range<usize>| u32::try_from(r.len()).unwrap_or(u32::MAX);
        let above = old_lines.get(..old_span.start).unwrap_or_default();
        patch.hunks.push(Hunk {
            old_start: number(&old_span),
            old_lines: count(&old_span),
            new_start: number(&new_span),
            new_lines: count(&new_span),
            heading: heading(above.iter().copied()),
            lines: text,
        });
    }
    patch
}

/// `side`'s lines in `range`, each after `mark`.
fn marked(side: &[&str], range: Range<usize>, mark: char) -> Vec<String> {
    side.get(range).unwrap_or_default().iter().map(|l| format!("{mark}{l}")).collect()
}

/// What a group of ops covers on each side.
fn span(group: &[DiffOp]) -> (Range<usize>, Range<usize>) {
    let first = group.first().map_or((0..0, 0..0), |op| (op.old_range(), op.new_range()));
    let last = group.last().map_or((0..0, 0..0), |op| (op.old_range(), op.new_range()));
    (first.0.start..last.0.end, first.1.start..last.1.end)
}

impl FileView {
    /// "Compare" on the conflict bar: show the disk's text against the edit, or go back to the
    /// edit. When the disk's text is not here (a save was refused without a read), the file is
    /// read again, the edit kept, and the comparison waits for it.
    pub fn toggle_compare(&mut self, cx: &mut Context<Self>) {
        if self.comparing.take().is_some() {
            cx.notify();
            return;
        }
        self.comparing = Some(Comparing::default());
        if self.disk.is_none() {
            cx.emit(super::FileViewEvent::Reload);
        }
        self.refresh_compare(cx);
        cx.notify();
    }

    /// Whether the tile shows the comparison.
    #[must_use]
    pub const fn comparing(&self) -> bool {
        self.comparing.is_some()
    }

    /// The comparison's hunks, once worked out: each as its lines' signs and texts.
    #[must_use]
    pub fn compared(&self) -> Option<Vec<Vec<String>>> {
        let shown = self.comparing.as_ref()?.shown.as_ref()?;
        Some(
            shown
                .blocks
                .iter()
                .map(|b| {
                    b.lines
                        .iter()
                        .map(|l| {
                            let sign = match l.kind {
                                diff::Kind::Added => '+',
                                diff::Kind::Removed => '-',
                                diff::Kind::Context => ' ',
                            };
                            format!("{sign}{}", l.text)
                        })
                        .collect()
                })
                .collect(),
        )
    }

    /// Work the diff out again when the edit or the disk's text moved since it was, unless it
    /// is being worked out for them already.
    pub(super) fn refresh_compare(&mut self, cx: &Context<Self>) {
        let Some(disk) = self.disk.clone() else { return };
        let from = Inputs { edit: self.edit, disk: disk.modified_ms };
        let Some(comparing) = self.comparing.as_mut() else { return };
        let asked = comparing.working.as_ref().map(|(inputs, _)| *inputs);
        if comparing.shown.as_ref().map(|s| s.from).or(asked) == Some(from) {
            return;
        }
        let text = match &self.pending_text {
            Some(text) => text.clone(),
            None => self.editor.read(cx).text().to_string(),
        };
        let path = self.path.clone();
        let task = cx.spawn(async move |this, cx| {
            let old = disk.text;
            let patch = cx.background_spawn(async move { patch(&old, &text) }).await;
            let _gone = this.update(cx, |this, cx| {
                let blocks = diff::thread_blocks(&path, &patch);
                if let Some(comparing) = this.comparing.as_mut() {
                    comparing.shown = Some(Shown { from, blocks });
                    cx.notify();
                }
            });
        });
        comparing.working = Some((from, task));
    }

    /// The comparison's body: the hunks in one scrolling column, or a word while it is worked
    /// out or when there is nothing to show.
    pub(super) fn render_compare(&self, comparing: &Comparing) -> AnyElement {
        let id = self.id.as_uuid();
        let Some(shown) = &comparing.shown else {
            return self.notice(Symbol::PlusForwardslashMinus, COMPARING, None, None);
        };
        if shown.blocks.is_empty() {
            return self.notice(Symbol::PlusForwardslashMinus, SAME_TEXT, None, None);
        }
        let ink = Ink { theme: &self.theme, zoom: self.zoom, digits: lines::digits(&shown.blocks) };
        let mut left = SHOWN_LINES;
        let mut column = div().w_full().flex().flex_col();
        for block in shown.blocks.iter() {
            if left == 0 {
                break;
            }
            column = column.child(ink.hunk_head(block));
            for line in block.lines.iter().take(left) {
                column = column.child(ink.unified(line));
            }
            left = left.saturating_sub(block.lines.len());
        }
        let total: usize = shown.blocks.iter().map(|b| b.lines.len()).sum();
        let more = total.saturating_sub(SHOWN_LINES);
        let s = &self.theme.surfaces;
        let k = self.zoom;
        let more = (more > 0).then(|| {
            div()
                .px(px(self.theme.spacing.inset() * k))
                .py(px(self.theme.spacing.xs * k))
                .text_color(crate::colors::hsla(s.text_muted))
                .font_family(self.theme.typography.ui_family.clone())
                .child(SharedString::from(format!("{more} more lines not shown")))
        });
        div()
            .id("file-comparison")
            .debug_selector(move || format!("file-comparison-{id}"))
            .role(Role::Document)
            .aria_label(SharedString::from(format!(
                "The disk's text against the edit of {}",
                self.path
            )))
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .child(ink.code().child(column))
            .children(more)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hunks name their lines in each file, carry git's heading, and count what changed;
    /// the same text is no hunk at all.
    #[test]
    fn a_patch_is_the_hunks_from_the_disk_to_the_edit() {
        let old = "fn main() {\n    one();\n    two();\n}\n";
        let new = "fn main() {\n    one();\n    three();\n}\n";
        let one = patch(old, new);
        assert_eq!((one.added, one.removed), (1, 1));
        let [hunk] = one.hunks.as_slice() else { panic!("{one:?}") };
        assert_eq!((hunk.old_start, hunk.new_start), (1, 1));
        assert_eq!(
            hunk.lines,
            [" fn main() {", "     one();", "-    two();", "+    three();", " }", " "]
        );
        assert!(patch(old, old).hunks.is_empty(), "nothing differs");
        let far = (0..40).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let edited = far.replace("line 2\n", "line two\n").replace("line 37", "line 37!");
        let two = patch(&far, &edited);
        assert_eq!(two.hunks.len(), 2, "changes far apart are hunks of their own");
        assert_eq!(two.hunks.get(1).and_then(|h| h.heading.clone()).as_deref(), Some("line 33"));
        let from_nothing = patch("", "# New\n");
        assert_eq!((from_nothing.added, from_nothing.removed), (1, 0), "{from_nothing:?}");
    }
}
