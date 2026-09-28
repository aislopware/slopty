//! One markdown look for every surface that draws it with gpui-kit's `TextView`.
//!
//! A note tile ([`crate::note`]) and a file tile read the same, because both take
//! their sizes, colours and corners from the theme through [`style`].

use std::rc::Rc;

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use gpui_kit::component::text::{TextView, TextViewStyle};
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize};

/// A piece of a Markdown text: prose for gpui-kit's `TextView`, or a fenced block drawn as
/// its own element so it can carry buttons.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Segment {
    /// Markdown between fenced blocks.
    Prose(String),
    /// One fenced block: the info string after the fence, and the lines inside.
    Code {
        /// The language after the opening fence (may be empty).
        lang: String,
        /// The block's lines, joined with `\n`.
        body: String,
    },
    /// One task-list line (`- [ ] text` or `- [x] text`), drawn as its own row so the box
    /// can be a button.
    Task(Task),
}

/// A task-list line, out of its text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Task {
    /// Its place among the text's task lines, the index [`toggle_task`] takes.
    pub ix: usize,
    /// The box is ticked.
    pub done: bool,
    /// The Markdown after the box.
    pub text: String,
}

/// `line` as a task-list item: the tick and the text after the box. A list mark (`-`, `*`,
/// `+`), a space, `[ ]` or `[x]`/`[X]`, a space; indentation ignored.
#[must_use]
pub fn task_line(line: &str) -> Option<(bool, &str)> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix(['-', '*', '+'])?.strip_prefix(' ')?;
    if let Some(text) = rest.strip_prefix("[ ] ") {
        return Some((false, text));
    }
    let text = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] "))?;
    Some((true, text))
}

/// `markdown` with its `ix`-th task line (counted as [`segments`] does, fences skipped)
/// ticked or unticked; `None` when there is no such line.
#[must_use]
pub fn toggle_task(markdown: &str, ix: usize) -> Option<String> {
    let mut seen = 0;
    let mut fenced = false;
    let mut out = String::with_capacity(markdown.len());
    let mut flipped = false;
    for (n, line) in markdown.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced && !flipped && task_line(line).is_some() {
            if seen == ix {
                let at = line.find('[').unwrap_or(0);
                out.push_str(line.get(..at).unwrap_or(line));
                let mark = if line.get(at..).is_some_and(|m| m.starts_with("[ ]")) {
                    "[x]"
                } else {
                    "[ ]"
                };
                out.push_str(mark);
                out.push_str(line.get(at.saturating_add(3)..).unwrap_or(""));
                flipped = true;
                continue;
            }
            seen = seen.saturating_add(1);
        }
        out.push_str(line);
    }
    flipped.then_some(out)
}

/// Split `markdown` into prose, fenced blocks and task lines.
///
/// A fence is a line starting with ```` ``` ````, indentation ignored; an unclosed fence runs
/// to the end. A task line is one [`task_line`] reads, outside a fence. Blank prose is dropped.
#[must_use]
pub fn segments(markdown: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut code: Option<(String, Vec<&str>)> = None;
    let mut tasks = 0;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        match code.take() {
            Some((lang, body)) if trimmed.starts_with("```") => {
                out.push(Segment::Code { lang, body: body.join("\n") });
            }
            Some((lang, mut body)) => {
                body.push(line);
                code = Some((lang, body));
            }
            None if trimmed.starts_with("```") => {
                if prose.iter().any(|l| !l.trim().is_empty()) {
                    out.push(Segment::Prose(prose.join("\n")));
                }
                prose.clear();
                code = Some((trimmed.trim_start_matches('`').trim().to_owned(), Vec::new()));
            }
            None => {
                if let Some((done, text)) = task_line(line) {
                    if prose.iter().any(|l| !l.trim().is_empty()) {
                        out.push(Segment::Prose(prose.join("\n")));
                    }
                    prose.clear();
                    out.push(Segment::Task(Task { ix: tasks, done, text: text.to_owned() }));
                    tasks = tasks.saturating_add(1);
                } else {
                    prose.push(line);
                }
            }
        }
    }
    if let Some((lang, body)) = code {
        out.push(Segment::Code { lang, body: body.join("\n") });
    }
    if prose.iter().any(|l| !l.trim().is_empty()) {
        out.push(Segment::Prose(prose.join("\n")));
    }
    out
}

/// How many of `markdown`'s task lines are ticked, and how many there are, as [`segments`]
/// finds them (fences skipped), without building a segment.
#[must_use]
pub fn task_counts(markdown: &str) -> (usize, usize) {
    let (mut done, mut total, mut fenced) = (0_usize, 0_usize, false);
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced && let Some((ticked, _)) = task_line(line) {
            total = total.saturating_add(1);
            done = done.saturating_add(usize::from(ticked));
        }
    }
    (done, total)
}

/// What a "run" button does with the block's code (the surface's owner knows the shells).
pub type Run = Rc<dyn Fn(String, &mut App)>;

/// A fenced block as its own element, with buttons.
///
/// Its language, a "Copy" button and — with `run` — a "Run" button, over the code in the
/// mono face on the raised surface. `ids` are the copy and run buttons' element ids and
/// debug selectors; `scale` as for [`style`].
#[must_use]
pub fn code_block(
    ids: (String, String),
    lang: &str,
    body: &str,
    theme: &Theme,
    scale: f32,
    run: Option<Run>,
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    let button = |id: String, label: &'static str, text: &'static str| {
        div()
            .id(ElementId::Name(id.clone().into()))
            .debug_selector(move || id)
            .role(Role::Button)
            .aria_label(label)
            .px(px(spacing.xs * scale))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_muted))
            .hover(move |st| st.bg(hsla_alpha(s.text, alpha::FAINT)))
            // The surface under it may take a press as "edit me": the button keeps its own.
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .child(text)
    };
    let (copy_id, run_id) = ids;
    let text = body.to_owned();
    let copy = button(copy_id, "Copy code", "Copy").on_click(move |_ev, _window, cx| {
        cx.stop_propagation();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
    });
    let run = run.map(|run| {
        let code = body.to_owned();
        button(run_id, "Run in shell", "Run").on_click(move |_ev, _window, cx| {
            cx.stop_propagation();
            run(code.clone(), cx);
        })
    });
    div()
        .flex()
        .flex_col()
        .rounded(px(theme.radii.xs))
        .bg(hsla(s.raised))
        .px(px(spacing.sm * scale))
        .py(px(spacing.xs * scale))
        .font_family(mono)
        .text_size(px(theme.typography.small() * scale))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(lang.to_owned()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(spacing.xs * scale))
                        .children(run)
                        .child(copy),
                ),
        )
        .child(
            div()
                .whitespace_normal()
                .text_color(hsla(s.text))
                .child(SharedString::from(body.to_owned())),
        )
        .into_any_element()
}

/// A task box's side, as a share of the prose size.
const TASK_BOX: f32 = 0.95;

/// How much narrower than its box a done task's check is drawn, in points at zoom 1.
const TASK_CHECK_INSET: f32 = 4.0;

/// What a task row's box does with the row's index (the surface's owner has the text).
pub type Toggle = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A task-list line as its own row: a box, ticked when `done`, before the line's Markdown.
///
/// With `toggle` the box is a button that flips the line (a note); without, it only shows
/// (an answer). `id` names the box's element id and debug selector; `scale` as for [`style`].
#[must_use]
pub fn task_row(
    id: String,
    task: &Task,
    theme: &Theme,
    mono: &str,
    scale: f32,
    toggle: Option<Toggle>,
) -> AnyElement {
    let Task { ix, done, text } = task;
    let (ix, done) = (*ix, *done);
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    // Linear's and Things' box: a notch under the prose's size, the small radius and a
    // hairline, on the first line's middle. The font's tick at caption size, in a 12 pt box
    // with a sharp corner, read as an unstyled browser form.
    let side = theme.typography.prose() * TASK_BOX * scale;
    let line = theme.typography.ui_size * theme.typography.markdown_line_height * scale;
    let toggled = if done { Toggled::True } else { Toggled::False };
    let mut label = String::from(if done { "Done: " } else { "To do: " });
    label.push_str(text);
    let text_id = format!("{id}-text");
    let mut boxed = div()
        .id(ElementId::Name(id.clone().into()))
        .debug_selector(move || id)
        .role(Role::CheckBox)
        .aria_label(SharedString::from(label))
        .aria_toggled(toggled)
        .flex_none()
        .size(px(side))
        .mt(px(((line - side) / 2.0).max(0.0)))
        .rounded(px(theme.radii.xs * scale))
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(hsla(if done { s.accent_fill } else { s.border }))
        // A ticked box is the accent fill with a drawn check in it, as a native checkbox is.
        .when(done, |b| {
            b.bg(hsla(s.accent_fill)).child(
                crate::icons::icon(theme, IconName::Check, IconSize::Inline, hsla(s.accent_ink))
                    .size(px((side - TASK_CHECK_INSET).max(1.0))),
            )
        });
    // The box is drawn at the text's size, but a finger or a pointer gets the density's
    // target round it: the pad spills into the gap and the margin, and moves nothing.
    let pad = (theme.density.hit.mul_add(scale, -side) / 2.0).max(spacing.xs * scale);
    let mut hit = div().id(ElementId::Name(format!("{text_id}-hit").into())).flex_none();
    if let Some(toggle) = toggle {
        if !done {
            boxed = boxed.hover(move |st| st.border_color(hsla(s.text_muted)));
        }
        hit = hit
            .p(px(pad))
            .m(px(-pad))
            .cursor_pointer()
            // The surface under it may take a press as "edit me": the box keeps its own.
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(move |_ev, window, cx| {
                cx.stop_propagation();
                toggle(ix, window, cx);
            });
    }
    div()
        .flex()
        .items_start()
        .gap(px(spacing.xs * scale))
        .child(hit.child(boxed))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                // A ticked task goes muted with a faint strike, as a done item does in a list
                // app: what is left to do reads first, and the strike does not shout.
                .when(done, |mut el| {
                    el.text_style().strikethrough = Some(gpui::StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(hsla_alpha(s.text_muted, alpha::STRONG)),
                    });
                    el
                })
                .child(
                    TextView::markdown(
                        ElementId::Name(text_id.into()),
                        SharedString::from(text.to_owned()),
                    )
                    .style(style(theme, mono, scale))
                    .when(done, |text| text.text_color(hsla(s.text_muted)))
                    .selectable(false),
                ),
        )
        .into_any_element()
}

/// Markdown on the theme's tokens.
///
/// Paragraphs one base unit apart, headings stepping down from the title size to the base,
/// code in the terminal mono at `small()` on the raised surface with `radii.xs` corners.
/// Colours come from the gpui-kit theme, which [`crate::kit::sync`] keeps on the same tokens.
///
/// `scale` multiplies every size, for a surface that is drawn at a zoom of its own (a note
/// in the overview); the chrome passes `1.0`.
#[must_use]
pub fn style(theme: &Theme, mono: &str, scale: f32) -> TextViewStyle {
    let small = theme.typography.small() * scale;
    let code_block = gpui::StyleRefinement::default()
        .font_family(mono.to_owned())
        .text_size(px(small))
        .bg(hsla(theme.surfaces.raised))
        .rounded(px(theme.radii.xs))
        .px(px(theme.spacing.sm * scale))
        .py(px(theme.spacing.xs * scale));
    let inline_code = gpui::HighlightStyle {
        background_color: Some(hsla(theme.surfaces.raised)),
        color: Some(hsla(theme.surfaces.text)),
        ..gpui::HighlightStyle::default()
    };
    let (title, base) = (theme.typography.title(), theme.typography.ui_size);
    TextViewStyle {
        paragraph_gap: gpui::rems(theme.spacing.sm / base),
        heading_base_font_size: px(base * scale),
        heading_font_size: Some(std::sync::Arc::new(move |level: u8, _base| {
            px((title - f32::from(level.saturating_sub(1))).max(base) * scale)
        })),
        code_block,
        inline_code,
        ..TextViewStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{IntoElement, Modifiers, Render, point};

    use super::*;

    /// A task box is drawn at the text's size, but a press a few points beside it still ticks
    /// it: the target is the density's, not the glyph's.
    #[gpui::test]
    fn a_press_beside_the_box_still_ticks_it(cx: &mut gpui::TestAppContext) {
        struct Row(Toggle);
        impl Render for Row {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                let task = Task { ix: 2, done: false, text: "ship".to_owned() };
                let theme = Theme::default();
                div().p(px(40.0)).child(task_row(
                    "task".to_owned(),
                    &task,
                    &theme,
                    "Menlo",
                    1.0,
                    Some(Rc::clone(&self.0)),
                ))
            }
        }
        cx.update(gpui_kit::init);
        let ticked = Rc::new(Cell::new(None));
        let sink = Rc::clone(&ticked);
        let toggle: Toggle = Rc::new(move |ix, _window, _cx| sink.set(Some(ix)));
        let (_row, cx) = cx.add_window_view(|_window, _cx| Row(toggle));
        let at = cx.debug_bounds("task").expect("the box is drawn");
        let beside = point(at.left() - px(4.0), at.center().y);
        cx.simulate_click(beside, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(ticked.get(), Some(2));
    }

    /// A ticked task's text is struck through and set back; an open one's is neither.
    #[gpui::test]
    fn a_done_task_is_struck_through_and_set_back(cx: &mut gpui::TestAppContext) {
        struct Rows;
        impl Render for Rows {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                let theme = Theme::default();
                let row = |ix, done| {
                    let task = Task { ix, done, text: "ship the bundle".to_owned() };
                    task_row(format!("task-{ix}"), &task, &theme, "Menlo", 1.0, None)
                };
                div().p(px(40.0)).w(px(400.0)).child(row(0, false)).child(row(1, true))
            }
        }
        cx.update(gpui_kit::init);
        let (_rows, cx) = cx.add_window_view(|_window, _cx| Rows);
        cx.run_until_parked();
        let (open, done) = (
            cx.debug_bounds("task-0").expect("the open box"),
            cx.debug_bounds("task-1").expect("the done box"),
        );
        let (scale, lines) =
            cx.update(|window, _| (window.scale_factor(), window.painted_underlines()));
        let on_row = |row: gpui::Bounds<gpui::Pixels>| {
            lines
                .iter()
                .filter(|l| {
                    let y = l.bounds.origin.y.0 / scale;
                    y >= f32::from(row.top()) - 8.0 && y <= f32::from(row.bottom()) + 8.0
                })
                .collect::<Vec<_>>()
        };
        assert!(on_row(open).is_empty(), "an open task is not struck");
        let struck = on_row(done);
        assert!(!struck.is_empty(), "a done task is struck through: {lines:?}");
        assert!(
            struck.iter().all(|l| (l.color.a - alpha::STRONG).abs() < 0.05),
            "and set back: {struck:?}"
        );
    }

    /// A task's box is Linear's and Things': a notch under the prose's size, the small radius
    /// and a hairline, the done one the accent fill with a drawn check, not the font's tick in
    /// a 12 pt box with a sharp corner.
    #[gpui::test]
    fn a_task_box_is_drawn_not_typed(cx: &mut gpui::TestAppContext) {
        struct Rows;
        impl Render for Rows {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                let theme = Theme::default();
                let row = |ix, done| {
                    let task = Task { ix, done, text: "ship".to_owned() };
                    task_row(format!("task-{ix}"), &task, &theme, "Menlo", 1.0, None)
                };
                div().p(px(40.0)).w(px(400.0)).child(row(0, false)).child(row(1, true))
            }
        }
        cx.update(gpui_kit::init);
        let (_rows, cx) = cx.add_window_view(|_window, _cx| Rows);
        cx.run_until_parked();
        let theme = Theme::default();
        let side = theme.typography.prose() * TASK_BOX;
        let (open, done) = (
            cx.debug_bounds("task-0").expect("the open box"),
            cx.debug_bounds("task-1").expect("the done box"),
        );
        for b in [open, done] {
            assert!((f32::from(b.size.width) - side).abs() < 0.5, "{side} pt: {b:?}");
        }
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let at = |b: gpui::Bounds<gpui::Pixels>| {
            quads
                .iter()
                .find(|q| {
                    (q.bounds.origin.y.0 / scale - f32::from(b.top())).abs() < 0.5
                        && (q.bounds.size.width.0 / scale - f32::from(b.size.width)).abs() < 0.5
                })
                .copied()
                .unwrap_or_else(|| panic!("a quad at {b:?}"))
        };
        let (open_q, done_q) = (at(open), at(done));
        let s = &theme.surfaces;
        assert_eq!(open_q.border_color, hsla(s.border), "a hairline");
        assert!((open_q.corner_radii.top_left.0 / scale - theme.radii.xs).abs() < 0.01);
        assert_eq!(done_q.background, gpui::Background::from(hsla(s.accent_fill)));
    }

    #[test]
    fn a_task_line_is_a_mark_a_box_and_its_text() {
        assert_eq!(task_line("- [ ] ship"), Some((false, "ship")));
        assert_eq!(task_line("  * [x] **test**"), Some((true, "**test**")));
        assert_eq!(task_line("+ [X] shout"), Some((true, "shout")));
        assert_eq!(task_line("- [] no"), None, "the box needs its space");
        assert_eq!(task_line("-[ ] no"), None, "the mark needs its space");
        assert_eq!(task_line("[ ] no"), None, "no list mark");
        assert_eq!(task_line("- plain"), None);
    }

    #[test]
    fn task_lines_are_their_own_segments_and_a_fence_hides_them() {
        assert_eq!(
            segments("todo\n- [ ] ship\n- [x] test\nnote\n```\n- [ ] not\n```"),
            [
                Segment::Prose("todo".to_owned()),
                Segment::Task(Task { ix: 0, done: false, text: "ship".to_owned() }),
                Segment::Task(Task { ix: 1, done: true, text: "test".to_owned() }),
                Segment::Prose("note".to_owned()),
                Segment::Code { lang: String::new(), body: "- [ ] not".to_owned() },
            ]
        );
    }

    #[test]
    fn task_counts_agree_with_the_segments() {
        let text = "- [x] a\n```\n- [ ] x\n```\n  - [x] b\n* [ ] c\n```rust\n- [x] unclosed";
        let tasks: Vec<bool> = segments(text)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Task(task) => Some(task.done),
                Segment::Prose(_) | Segment::Code { .. } => None,
            })
            .collect();
        let done = tasks.iter().filter(|d| **d).count();
        assert_eq!(task_counts(text), (done, tasks.len()));
        assert_eq!(task_counts(text), (2, 3));
    }

    #[test]
    fn a_toggle_flips_the_nth_task_and_nothing_else() {
        const TEXT: &str = "- [ ] a\n```\n- [ ] x\n```\n  - [x] b\n- [ ] c\n";
        assert_eq!(
            toggle_task(TEXT, 0).as_deref(),
            Some("- [x] a\n```\n- [ ] x\n```\n  - [x] b\n- [ ] c\n")
        );
        assert_eq!(
            toggle_task(TEXT, 1).as_deref(),
            Some("- [ ] a\n```\n- [ ] x\n```\n  - [ ] b\n- [ ] c\n")
        );
        assert_eq!(
            toggle_task(TEXT, 2).as_deref(),
            Some("- [ ] a\n```\n- [ ] x\n```\n  - [x] b\n- [x] c\n")
        );
        assert_eq!(toggle_task(TEXT, 3), None, "no fourth task");
        assert_eq!(
            toggle_task("- [X] up", 0).as_deref(),
            Some("- [ ] up"),
            "a capital tick unticks"
        );
        let twice = toggle_task(&toggle_task(TEXT, 0).unwrap_or_default(), 0);
        assert_eq!(twice.as_deref(), Some(TEXT), "a toggle undoes itself");
    }
}
