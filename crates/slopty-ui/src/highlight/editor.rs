//! The file tile's colours inside gpui-kit's code editor.
//!
//! The editor asks a parser-independent seam ([`InputHighlighter`]) for styled byte ranges
//! and tells it about every edit. This adapter answers from [`super::spans`], parsed off the
//! UI thread: an edit first splices the line list (the edited lines go plain, the lines
//! after it keep their colours at their new rows), then a parse of the whole text starts
//! once typing pauses for [`SETTLE`], and its lines replace the list when it lands, unless a
//! newer edit has overtaken it.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, FontStyle, FontWeight, HighlightStyle, SharedString, Task, Window};
use gpui_kit::component::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope, RopeExt as _,
};
use slopty_theme::Theme;

use super::{Span, Syntax, Token, spans};

/// How long typing must pause before the text is parsed again. Below a keystroke's gap at
/// speed, so colours catch up between words, not between letters.
pub const SETTLE: Duration = Duration::from_millis(60);

/// The factory the editor calls with its language: this file's grammar, whatever the name
/// (the tile knows the grammar from the path; the name is only for gpui-kit's own
/// indentation and bracket rules).
#[must_use]
pub fn factory(syntax: Option<Syntax>, theme: Theme) -> InputHighlighterFactory {
    Rc::new(move |_language| {
        syntax.map(|syntax| -> Box<dyn InputHighlighter> {
            Box::new(EditorHighlighter::new(syntax, theme.clone()))
        })
    })
}

/// What a parse left behind, shared with the task that is running the next one.
#[derive(Default)]
struct Parsed {
    /// Spans per line of the current text, plain (empty) where an edit has not been
    /// parsed yet.
    lines: Vec<Arc<[Span]>>,
    /// Bumped by every edit; a parse that started before the latest edit is dropped.
    generation: u64,
}

/// One editor's highlighter.
pub struct EditorHighlighter {
    syntax: Syntax,
    theme: Theme,
    /// The text as of the last edit, for mapping byte ranges to lines.
    text: Rope,
    parsed: Rc<RefCell<Parsed>>,
    /// The parse waiting out [`SETTLE`] or running; dropped (cancelled) by the next edit.
    parsing: Option<Task<()>>,
}

impl std::fmt::Debug for EditorHighlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorHighlighter").field("syntax", &self.syntax).finish_non_exhaustive()
    }
}

impl EditorHighlighter {
    /// Nothing parsed yet: plain text until the first parse lands.
    #[must_use]
    pub fn new(syntax: Syntax, theme: Theme) -> Self {
        Self { syntax, theme, text: Rope::new(), parsed: Rc::default(), parsing: None }
    }

    /// The style of one span in this theme; the default style for plain text.
    fn style(&self, span: &Span) -> HighlightStyle {
        if span.token == Token::Plain && !span.italic && !span.bold {
            return HighlightStyle::default();
        }
        HighlightStyle {
            color: Some(span.token.color(&self.theme)),
            font_style: span.italic.then_some(FontStyle::Italic),
            font_weight: span.bold.then_some(FontWeight::BOLD),
            ..HighlightStyle::default()
        }
    }
}

/// `lines` after an edit: the rows it touched become as many plain rows as it left, so the
/// colours below it move with their text instead of painting over the wrong lines.
pub fn splice(lines: &mut Vec<Arc<[Span]>>, edit: &InputEdit) {
    let start = edit.start_position.row.min(lines.len());
    let old_end = edit.old_end_position.row.saturating_add(1).min(lines.len()).max(start);
    let new_rows =
        edit.new_end_position.row.saturating_sub(edit.start_position.row).saturating_add(1);
    let plain: Arc<[Span]> = Arc::from([]);
    lines.splice(start..old_end, std::iter::repeat_n(plain, new_rows));
}

impl InputHighlighter for EditorHighlighter {
    fn language(&self) -> SharedString {
        SharedString::new_static(self.syntax.name())
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        self.text = text.clone();
        let generation = {
            let mut parsed = self.parsed.borrow_mut();
            if let Some(edit) = &edit {
                splice(&mut parsed.lines, edit);
            }
            parsed.generation = parsed.generation.wrapping_add(1);
            parsed.generation
        };
        let syntax = self.syntax;
        let rope = text.clone();
        let shared = Rc::clone(&self.parsed);
        // The first parse (a text set whole) starts at once; an edit waits for a pause.
        let settle = edit.is_some().then_some(SETTLE);
        let executor = cx.background_executor().clone();
        self.parsing = Some(cx.spawn(async move |editor, cx| {
            if let Some(settle) = settle {
                executor.timer(settle).await;
            }
            let lines = executor
                .spawn(async move {
                    let text = rope.to_string();
                    spans(&text, syntax).into_iter().map(Arc::from).collect::<Vec<Arc<[Span]>>>()
                })
                .await;
            {
                let mut parsed = shared.borrow_mut();
                if parsed.generation != generation {
                    return;
                }
                parsed.lines = lines;
            }
            let _gone = editor.update(cx, |_, cx| cx.notify());
        }));
    }

    fn styles(
        &self,
        range: &Range<usize>,
        _resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let parsed = self.parsed.borrow();
        let end = range.end.min(self.text.len());
        let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut push = |run: Range<usize>, style: HighlightStyle| {
            let run = run.start.max(range.start)..run.end.min(range.end);
            if run.is_empty() {
                return;
            }
            // A run that continues the last one in the same style extends it.
            let continues = out.last().is_some_and(|(last, _)| last.end == run.start);
            match out.last_mut() {
                Some((last, last_style)) if continues && *last_style == style => {
                    last.end = run.end;
                }
                _ => out.push((run, style)),
            }
        };
        let mut row = self.text.offset_to_point(range.start.min(self.text.len())).row;
        let mut at = self.text.line_start_offset(row);
        while at < end {
            let line_end = self.text.line_end_offset(row);
            let mut from = at;
            if let Some(line) = parsed.lines.get(row) {
                for span in line.iter() {
                    let to = from.saturating_add(span.len).min(line_end);
                    if to <= from {
                        break;
                    }
                    push(from..to, self.style(span));
                    from = to;
                }
            }
            // The rest of the line and its newline.
            let next = self.text.line_start_offset(row.saturating_add(1)).max(line_end);
            let next = if next <= at { self.text.len() } else { next };
            push(from..next.max(from), HighlightStyle::default());
            at = next;
            row = row.saturating_add(1);
        }
        // Whatever the text does not reach (a range past its end) is plain, so the runs
        // cover the range exactly.
        let covered = out.last().map_or(range.start, |(r, _)| r.end);
        if covered < range.end {
            out.push((covered..range.end, HighlightStyle::default()));
        }
        out
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::input::Point;

    use super::*;

    struct Unstyled;

    impl HighlightStyleResolver for Unstyled {
        fn style(&self, _: &str) -> Option<HighlightStyle> {
            None
        }
    }

    fn span(len: usize, token: Token) -> Span {
        Span { len, token, italic: false, bold: false }
    }

    fn edit(start: usize, old_end: usize, new_end: usize) -> InputEdit {
        InputEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 0,
            start_position: Point { row: start, column: 0 },
            old_end_position: Point { row: old_end, column: 0 },
            new_end_position: Point { row: new_end, column: 0 },
        }
    }

    #[test]
    fn an_edit_moves_the_colours_below_it_with_their_lines() {
        let keyword: Arc<[Span]> = Arc::from([span(2, Token::Keyword)]);
        let string: Arc<[Span]> = Arc::from([span(3, Token::String)]);
        let mut lines = vec![Arc::clone(&keyword), Arc::from([]), Arc::clone(&string)];
        // A newline typed on row 1: two rows where there was one, the string one row down.
        splice(&mut lines, &edit(1, 1, 2));
        assert_eq!(lines.len(), 4);
        assert_eq!(lines.get(3).map(|l| l.len()), Some(1));
        assert_eq!(lines.get(3).and_then(|l| l.first()).map(|s| s.token), Some(Token::String));
        // Rows 0..=2 joined into one: the string moves up to row 1.
        splice(&mut lines, &edit(0, 2, 0));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.get(1).and_then(|l| l.first()).map(|s| s.token), Some(Token::String));
        assert!(lines.first().is_some_and(|l| l.is_empty()), "the edited row goes plain");
    }

    /// The numbers behind the editor's colours: what a frame pays to style the rows it
    /// shows (60 rows of a 2 000-line Rust file, the tile's worst case), and what a keystroke
    /// pays to splice the line list before the settled parse.
    #[test]
    #[ignore = "timing, run by hand with --ignored --nocapture"]
    fn timing_of_a_frame_and_a_keystroke() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let one = include_str!("../workspace.rs");
        let text: String = one.lines().cycle().take(2_000).collect::<Vec<_>>().join("\n");
        let mut h = EditorHighlighter::new(syntax, Theme::default());
        h.text = Rope::from(text.as_str());
        let lines: Vec<Arc<[Span]>> = spans(&text, syntax).into_iter().map(Arc::from).collect();
        h.parsed.borrow_mut().lines = lines;
        let mid = h.text.line_start_offset(1_000)..h.text.line_start_offset(1_060);
        let rounds = 1_000_u32;
        let t0 = std::time::Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(h.styles(&mid, &Unstyled));
        }
        let frame = t0.elapsed() / rounds;
        let t1 = std::time::Instant::now();
        for _ in 0..rounds {
            let mut lines = h.parsed.borrow().lines.clone();
            splice(&mut lines, &edit(1_000, 1_000, 1_001));
            std::hint::black_box(lines);
        }
        let keystroke = t1.elapsed() / rounds;
        println!("60 rows styled {frame:?} a frame; a newline spliced {keystroke:?}");
        Ok(())
    }

    #[test]
    fn styles_cover_the_range_exactly_and_colour_by_line() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let mut h = EditorHighlighter::new(syntax, Theme::default());
        let text = "fn a() {}\n// b\n";
        h.text = Rope::from(text);
        h.parsed.borrow_mut().lines = spans(text, syntax).into_iter().map(Arc::from).collect();
        let resolver = Unstyled;
        let runs = h.styles(&(0..text.len()), &resolver);
        let mut at = 0;
        for (range, _) in &runs {
            assert_eq!(range.start, at, "runs are contiguous: {runs:?}");
            at = range.end;
        }
        assert_eq!(at, text.len(), "and reach the end");
        let keyword = Token::Keyword.color(&Theme::default());
        assert_eq!(runs.first().map(|(r, s)| (r.clone(), s.color)), Some((0..2, Some(keyword))));
        let comment = runs.iter().find(|(r, _)| r.start == 10).map(|(_, s)| s.font_style);
        assert_eq!(comment, Some(Some(FontStyle::Italic)), "the comment line: {runs:?}");
        // A range in the middle is clipped to it.
        let mid = h.styles(&(3..12), &resolver);
        assert_eq!(mid.first().map(|(r, _)| r.start), Some(3));
        assert_eq!(mid.last().map(|(r, _)| r.end), Some(12));
        Ok(())
    }
}
