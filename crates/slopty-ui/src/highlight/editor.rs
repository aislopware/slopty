//! The file tile's colours inside gpui-kit's code editor.
//!
//! The editor asks a parser-independent seam ([`InputHighlighter`]) for styled byte ranges
//! and tells it about every edit. This adapter answers from lines parsed off the UI thread
//! ([`super::LineParser`]), keeping with each line the state it starts in. An edit splices
//! the lists (the edited lines go plain and their states unknown, the lines after them keep
//! their colours and states at their new rows). A parse then starts, at once while nothing
//! has been parsed yet and after typing pauses for [`SETTLE`] once colours show. It parses
//! from each edited line down to the first line after it that starts in the state it started
//! in before, where everything below is as it was, and its lines land in the lists unless a
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

use super::{LineParser, LineState, Span, Syntax, Token};

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

/// What the parses left behind, shared with the task that is running the next one.
#[derive(Default)]
struct Parsed {
    /// Spans per line of the current text; `None` (drawn plain) where an edit has not been
    /// parsed yet.
    lines: Vec<Option<Arc<[Span]>>>,
    /// The state each line starts in, as the last parse left it, one per line; `None` where
    /// an edit made it unknown. A line below an edit keeps the state it started in before
    /// the edit, which the next parse compares with the one it reaches there.
    starts: Vec<Option<Arc<LineState>>>,
    /// Bumped by every edit; a parse that started before the latest edit is dropped.
    generation: u64,
    /// A parse has landed: colours show, and the next waits for typing to pause.
    landed: bool,
}

/// What one parse coloured: each line it parsed, with its spans and the state the line
/// after it starts in.
type Parse = Vec<(usize, Arc<[Span]>, Arc<LineState>)>;

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

/// The lists after an edit: the rows it touched become as many rows as it left, plain and
/// in states not known yet, so the colours below it move with their text instead of painting
/// over the wrong lines. The first row the edit touched still starts where it did.
fn splice(parsed: &mut Parsed, edit: &InputEdit) {
    let (lines, starts) = (&mut parsed.lines, &mut parsed.starts);
    let start = edit.start_position.row.min(lines.len());
    let old_end = edit.old_end_position.row.saturating_add(1).min(lines.len()).max(start);
    let new_rows =
        edit.new_end_position.row.saturating_sub(edit.start_position.row).saturating_add(1);
    lines.splice(start..old_end, std::iter::repeat_n(None, new_rows));
    let first = start.saturating_add(1).min(starts.len());
    let last = old_end.max(first).min(starts.len());
    starts.splice(first..last, std::iter::repeat_n(None, new_rows.saturating_sub(1)));
    starts.resize(lines.len(), None);
}

/// The text of line `row` of `text`, which has `rows` lines, with its newline when it has one.
fn line_text(text: &Rope, row: usize, rows: usize) -> String {
    let mut line = text.slice_line(row).to_string();
    if row.saturating_add(1) < rows {
        line.push('\n');
    }
    line
}

/// Parse `text` again where `lines` has a line not parsed yet, from that line (or the nearest
/// line above it whose start is known) down to the first line after it that is parsed and
/// starts in the state it started in before (`starts`): what follows is as it was.
fn reparse(
    text: &Rope,
    syntax: Syntax,
    lines: &[Option<Arc<[Span]>>],
    starts: &[Option<Arc<LineState>>],
) -> Parse {
    let parser = LineParser::new(syntax);
    let rows = text.lines_len();
    let parsed = |row: usize| lines.get(row).is_some_and(Option::is_some);
    let known = |row: usize| starts.get(row).and_then(Option::as_ref);
    let mut out = Vec::new();
    let mut row = 0;
    while let Some(dirty) = (row..rows).find(|&r| !parsed(r)) {
        let from = (1..=dirty).rev().find(|&r| known(r).is_some()).unwrap_or(0);
        let mut state =
            known(from).filter(|_| from > 0).map_or_else(|| parser.start(), |s| (**s).clone());
        row = from;
        loop {
            let spans = parser.line(&line_text(text, row, rows), &mut state);
            out.push((row, Arc::from(spans), Arc::new(state.clone())));
            row = row.saturating_add(1);
            if row >= rows || (parsed(row) && known(row).is_some_and(|s| **s == state)) {
                break;
            }
        }
    }
    out
}

/// A parse's lines put in the lists it was made from.
fn land(parsed: &mut Parsed, parse: Parse) {
    for (row, spans, next) in parse {
        if let Some(line) = parsed.lines.get_mut(row) {
            *line = Some(spans);
        }
        if let Some(start) = parsed.starts.get_mut(row.saturating_add(1)) {
            *start = Some(next);
        }
    }
    parsed.landed = true;
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
        let (generation, settle) = {
            let mut parsed = self.parsed.borrow_mut();
            if let Some(edit) = &edit {
                splice(&mut parsed, edit);
            } else {
                // The text set whole, nothing of it known.
                let rows = text.lines_len();
                parsed.lines = vec![None; rows];
                parsed.starts = vec![None; rows];
            }
            parsed.generation = parsed.generation.wrapping_add(1);
            // Until colours show, the first parse starts at once; after that an edit waits
            // for typing to pause. The editor hands a text set whole over as an edit too.
            (parsed.generation, parsed.landed.then_some(SETTLE))
        };
        let syntax = self.syntax;
        let rope = text.clone();
        let shared = Rc::clone(&self.parsed);
        let executor = cx.background_executor().clone();
        self.parsing = Some(cx.spawn(async move |editor, cx| {
            if let Some(settle) = settle {
                executor.timer(settle).await;
            }
            let (lines, starts) = {
                let parsed = shared.borrow();
                (parsed.lines.clone(), parsed.starts.clone())
            };
            let parse =
                executor.spawn(async move { reparse(&rope, syntax, &lines, &starts) }).await;
            {
                let mut parsed = shared.borrow_mut();
                if parsed.generation != generation {
                    return;
                }
                land(&mut parsed, parse);
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
            if let Some(Some(line)) = parsed.lines.get(row) {
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

    /// Parsed lists for `text`, as a parse from scratch leaves them.
    fn parsed(text: &str, syntax: Syntax) -> Parsed {
        let rope = Rope::from(text);
        let rows = rope.lines_len();
        let mut parsed =
            Parsed { lines: vec![None; rows], starts: vec![None; rows], ..Parsed::default() };
        let parse = reparse(&rope, syntax, &parsed.lines, &parsed.starts);
        land(&mut parsed, parse);
        parsed
    }

    /// Every line's spans, plain where not parsed.
    fn colours(parsed: &Parsed) -> Vec<Vec<Span>> {
        parsed
            .lines
            .iter()
            .map(|l| l.as_deref().map(<[Span]>::to_vec).unwrap_or_default())
            .collect()
    }

    /// `text` with the bytes `range` replaced by `with`, as the editor reports it.
    fn replace(text: &str, range: Range<usize>, with: &str) -> (String, InputEdit) {
        let before = Rope::from(text);
        let (head, tail) =
            (text.get(..range.start).unwrap_or(""), text.get(range.end..).unwrap_or(""));
        let after = format!("{head}{with}{tail}");
        let now = Rope::from(after.as_str());
        let new_end = range.start.saturating_add(with.len());
        let edit = InputEdit {
            start_byte: range.start,
            old_end_byte: range.end,
            new_end_byte: new_end,
            start_position: before.offset_to_point(range.start),
            old_end_position: before.offset_to_point(range.end),
            new_end_position: now.offset_to_point(new_end),
        };
        (after, edit)
    }

    #[test]
    fn an_edit_moves_the_colours_below_it_with_their_lines() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let mut lists = parsed("fn a() {}\n\n\"s\"\n", syntax);
        let string = lists.lines.get(2).cloned().flatten();
        // A newline typed on row 1: two rows where there was one, the string one row down.
        splice(&mut lists, &edit(1, 1, 2));
        assert_eq!((lists.lines.len(), lists.starts.len()), (5, 5));
        assert_eq!(lists.lines.get(3).cloned().flatten(), string, "the string moved down");
        assert!(lists.starts.get(3).is_some_and(Option::is_some), "with the state it starts in");
        assert!(lists.lines.get(1).is_some_and(Option::is_none), "the edited rows go plain");
        // Rows 0..=2 joined into one: the string moves up to row 1.
        splice(&mut lists, &edit(0, 2, 0));
        assert_eq!((lists.lines.len(), lists.starts.len()), (3, 3));
        assert_eq!(lists.lines.get(1).cloned().flatten(), string, "the string moved up");
        Ok(())
    }

    /// A parse after an edit colours as a parse of the whole text would, and parses only down
    /// to where the lines below start as they did: a word typed on a line is that line, an
    /// opened block comment runs to the text's end, and closing it again reaches as far as the
    /// comment ran.
    #[test]
    fn an_edit_is_parsed_again_only_as_far_as_it_reaches() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let mut text = String::new();
        for n in 0..40 {
            use std::fmt::Write as _;
            let _infallible = writeln!(text, "let v{n} = \"{n}\"; // {n}");
        }
        let mut lists = parsed(&text, syntax);
        for (at, with, reach) in [
            ("let v10 ", "let value10 ", 1..=1),
            ("let v20 ", "/* let v20 ", 20..=21),
            ("let v30 ", "*/ let v30 ", 10..=11),
        ] {
            let start = text.find(at).ok_or(at)?;
            let (next, edit) = replace(&text, start..start.saturating_add(at.len()), with);
            text = next;
            splice(&mut lists, &edit);
            let rope = Rope::from(text.as_str());
            let parse = reparse(&rope, syntax, &lists.lines, &lists.starts);
            let rows = parse.len();
            land(&mut lists, parse);
            assert_eq!(
                colours(&lists),
                super::super::spans(&text, syntax),
                "{with}: as a whole parse"
            );
            assert!(reach.contains(&rows), "{with}: {rows} rows parsed");
        }
        Ok(())
    }

    /// What a keystroke's parse costs in a 2 000-line Rust file: the whole text, as before,
    /// against the lines the edit reaches. Run by hand; `docs/MEASUREMENTS.md` has the numbers.
    #[test]
    #[ignore = "timing, run by hand with --ignored --nocapture"]
    fn timing_of_a_keystrokes_parse() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let one = include_str!("../workspace.rs");
        let text: String = one.lines().cycle().take(2_000).collect::<Vec<_>>().join("\n");
        let first = std::time::Instant::now();
        let lists = parsed(&text, syntax);
        let first = first.elapsed();
        let at = Rope::from(text.as_str()).line_start_offset(1_000);
        let (typed, edit) = replace(&text, at..at, "x");
        let rope = Rope::from(typed.as_str());
        let rounds = 20_u32;
        let t0 = std::time::Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(super::super::spans(&rope.to_string(), syntax));
        }
        let whole = t0.elapsed() / rounds;
        let mut rows = 0;
        let t1 = std::time::Instant::now();
        for _ in 0..rounds {
            let (lines, starts) = (lists.lines.clone(), lists.starts.clone());
            let mut after = Parsed { lines, starts, ..Parsed::default() };
            splice(&mut after, &edit);
            let parse = reparse(&rope, syntax, &after.lines, &after.starts);
            rows = parse.len();
            std::hint::black_box(parse);
        }
        let incremental = t1.elapsed() / rounds;
        println!(
            "MEASURE a keystroke's parse, 2 000 lines: the whole text {whole:?}, the lines it \
             reaches {incremental:?} ({rows} rows); the first parse, states kept, {first:?}"
        );
        Ok(())
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
        *h.parsed.borrow_mut() = parsed(&text, syntax);
        let mid = h.text.line_start_offset(1_000)..h.text.line_start_offset(1_060);
        let rounds = 1_000_u32;
        let t0 = std::time::Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(h.styles(&mid, &Unstyled));
        }
        let frame = t0.elapsed() / rounds;
        let t1 = std::time::Instant::now();
        for _ in 0..rounds {
            let (lines, starts) = {
                let parsed = h.parsed.borrow();
                (parsed.lines.clone(), parsed.starts.clone())
            };
            let mut lists = Parsed { lines, starts, ..Parsed::default() };
            splice(&mut lists, &edit(1_000, 1_000, 1_001));
            std::hint::black_box(lists);
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
        *h.parsed.borrow_mut() = parsed(text, syntax);
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
