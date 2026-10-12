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
//!
//! A parse lands in steps, so a long file shows coloured where the person looks within a frame
//! or two rather than after the whole text: first the rows on show (the last range the editor
//! styled), then [`CHUNK`] rows at a time down the text, the editor drawn again only when a step
//! reaches the rows on show. Rows on show past where the parse has reached are coloured from a
//! guess, parsed alone from the grammar's first state, until the parse reaches them and puts
//! the true colours in their place.

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

/// How many rows a parse step colours before it lands and the next begins.
///
/// About 15 ms of a release build's parse at 80–160 µs a row (`docs/MEASUREMENTS.md`), so the
/// rows on show are never more than a step from their turn.
pub const CHUNK: usize = 128;

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
    /// The rows on show past where the parse had reached, parsed alone from the grammar's
    /// first state: their first row and each row's spans. Drawn where a row has no spans of
    /// its own yet; dropped by the next edit.
    guess: Option<(usize, Vec<Arc<[Span]>>)>,
}

impl Parsed {
    /// The first row the parse has not coloured: the text's end once it has all.
    fn frontier(&self) -> usize {
        self.lines.iter().position(Option::is_none).unwrap_or(self.lines.len())
    }

    /// A row's spans: its own once parsed, else the guess's.
    fn spans(&self, row: usize) -> Option<&Arc<[Span]>> {
        self.lines.get(row).and_then(Option::as_ref).or_else(|| {
            let (first, guessed) = self.guess.as_ref()?;
            guessed.get(row.checked_sub(*first)?)
        })
    }
}

/// What a parse does next.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Step {
    /// Guess the rows on show, which lie past where the parse has reached.
    Guess(Range<usize>),
    /// Parse on down the text, this many rows at most.
    Parse(usize),
}

/// The next step of a parse that has reached `frontier`, the editor showing rows `seen`:
/// the rows on show first. Rows on show the parse reaches in a step are parsed to their end
/// in one; rows past it are guessed once, unless guessed already (`guessed`).
fn next_step(frontier: usize, seen: Option<Range<usize>>, guessed: Option<&Range<usize>>) -> Step {
    match seen.filter(|seen| seen.end > frontier && !seen.is_empty()) {
        Some(seen) if seen.start <= frontier.saturating_add(CHUNK) => {
            Step::Parse(seen.end.saturating_sub(frontier).max(CHUNK))
        }
        Some(seen) if guessed.is_none_or(|g| g.start > seen.start || g.end < seen.end) => {
            Step::Guess(seen)
        }
        _ => Step::Parse(CHUNK),
    }
}

/// Rows `rows` of `text` parsed alone, from the grammar's first state: a guess at their colours
/// until the parse reaches them.
fn guess(text: &Rope, syntax: Syntax, rows: Range<usize>) -> Vec<Arc<[Span]>> {
    let parser = LineParser::new(syntax);
    let total = text.lines_len();
    let mut state = parser.start();
    rows.take_while(|row| *row < total)
        .map(|row| Arc::from(parser.line(&line_text(text, row, total), &mut state)))
        .collect()
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
    /// The rows the editor last styled: the rows on show, which a parse colours first.
    seen: Rc<std::cell::Cell<Option<(usize, usize)>>>,
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
        Self {
            syntax,
            theme,
            text: Rope::new(),
            parsed: Rc::default(),
            seen: Rc::default(),
            parsing: None,
        }
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
    parsed.guess = None;
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

/// How many line states a parse compares a new one with before keeping it apart.
const RECENT_STATES: usize = 64;

/// Parse `text` again where `lines` has a line not parsed yet, from that line (or the nearest
/// line above it whose start is known) down to the first line after it that is parsed and
/// starts in the state it started in before (`starts`): what follows is as it was. At most
/// `budget` rows: whether rows are left to parse once it is spent.
fn reparse(
    text: &Rope,
    syntax: Syntax,
    lines: &[Option<Arc<[Span]>>],
    starts: &[Option<Arc<LineState>>],
    budget: usize,
) -> (Parse, bool) {
    let parser = LineParser::new(syntax);
    let rows = text.lines_len();
    let parsed = |row: usize| lines.get(row).is_some_and(Option::is_some);
    let known = |row: usize| starts.get(row).and_then(Option::as_ref);
    let mut out = Vec::new();
    // Most lines start in one of a few states (at the top level, in a block, in a comment):
    // a state met a moment ago is shared, not kept again for every line.
    let mut recent: Vec<Arc<LineState>> = Vec::with_capacity(RECENT_STATES);
    let mut keep = |state: &LineState| {
        if let Some(met) = recent.iter().find(|met| ***met == *state) {
            return Arc::clone(met);
        }
        let kept = Arc::new(state.clone());
        if recent.len() == RECENT_STATES {
            recent.remove(0);
        }
        recent.push(Arc::clone(&kept));
        kept
    };
    let mut row = 0;
    while let Some(dirty) = (row..rows).find(|&r| !parsed(r)) {
        let from = (1..=dirty).rev().find(|&r| known(r).is_some()).unwrap_or(0);
        let mut state =
            known(from).filter(|_| from > 0).map_or_else(|| parser.start(), |s| (**s).clone());
        row = from;
        loop {
            if out.len() >= budget {
                return (out, true);
            }
            let spans = parser.line(&line_text(text, row, rows), &mut state);
            out.push((row, Arc::from(spans), keep(&state)));
            row = row.saturating_add(1);
            if row >= rows || (parsed(row) && known(row).is_some_and(|s| **s == state)) {
                break;
            }
        }
    }
    (out, false)
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
                // One entry a row of the text, whatever the edit said: a parse that steps
                // down the text stops where the lists end.
                let rows = text.lines_len();
                parsed.lines.resize(rows, None);
                parsed.starts.resize(rows, None);
            } else {
                // The text set whole, nothing of it known.
                let rows = text.lines_len();
                parsed.lines = vec![None; rows];
                parsed.starts = vec![None; rows];
                parsed.guess = None;
            }
            parsed.generation = parsed.generation.wrapping_add(1);
            // Until colours show, the first parse starts at once; after that an edit waits
            // for typing to pause. The editor hands a text set whole over as an edit too.
            (parsed.generation, parsed.landed.then_some(SETTLE))
        };
        let syntax = self.syntax;
        let rope = text.clone();
        let shared = Rc::clone(&self.parsed);
        let seen = Rc::clone(&self.seen);
        let executor = cx.background_executor().clone();
        self.parsing = Some(cx.spawn(async move |editor, cx| {
            if let Some(settle) = settle {
                executor.timer(settle).await;
            }
            loop {
                let rows = rope.lines_len();
                let shown = seen.get().map(|(start, end)| start.min(rows)..end.min(rows));
                let (step, lines, starts, frontier) = {
                    let parsed = shared.borrow();
                    let guessed = parsed
                        .guess
                        .as_ref()
                        .map(|(first, rows)| *first..first.saturating_add(rows.len()));
                    let frontier = parsed.frontier();
                    let step = next_step(frontier, shown.clone(), guessed.as_ref());
                    let lists = matches!(step, Step::Parse(_))
                        .then(|| (parsed.lines.clone(), parsed.starts.clone()));
                    let (lines, starts) = lists.unwrap_or_default();
                    (step, lines, starts, frontier)
                };
                let rope = rope.clone();
                let more = match step {
                    Step::Guess(rows) => {
                        let first = rows.start;
                        let guessed =
                            executor.spawn(async move { guess(&rope, syntax, rows) }).await;
                        let mut parsed = shared.borrow_mut();
                        if parsed.generation != generation {
                            return;
                        }
                        parsed.guess = Some((first, guessed));
                        true
                    }
                    Step::Parse(budget) => {
                        let (parse, more) = executor
                            .spawn(async move { reparse(&rope, syntax, &lines, &starts, budget) })
                            .await;
                        // The editor draws again only for colours it shows.
                        let reaches = shown
                            .as_ref()
                            .is_none_or(|shown| parse.iter().any(|(row, ..)| shown.contains(row)));
                        let mut parsed = shared.borrow_mut();
                        if parsed.generation != generation {
                            return;
                        }
                        land(&mut parsed, parse);
                        // A step that left the parse where it was has nothing more to give.
                        let more = more && parsed.frontier() > frontier;
                        if !reaches && more {
                            continue;
                        }
                        more
                    }
                };
                let _gone = editor.update(cx, |_, cx| cx.notify());
                if !more {
                    return;
                }
            }
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
        let last = self.text.offset_to_point(end).row;
        self.seen.set(Some((row, last.saturating_add(1))));
        let mut at = self.text.line_start_offset(row);
        while at < end {
            let line_end = self.text.line_end_offset(row);
            let mut from = at;
            if let Some(line) = parsed.spans(row) {
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
    use gpui_kit::component::input::{EditorState, Point};

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
        let (parse, _) = reparse(&rope, syntax, &parsed.lines, &parsed.starts, usize::MAX);
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
            let (parse, _) = reparse(&rope, syntax, &lists.lines, &lists.starts, usize::MAX);
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
        let one = include_str!("../../../slopty-ui/src/workspace.rs");
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
            let (parse, _) = reparse(&rope, syntax, &after.lines, &after.starts, usize::MAX);
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

    /// What the kept parse holds for the largest file still coloured: Rust source cut to
    /// [`crate::highlight::COLOURED_BYTES`], the process's resident size before and after the
    /// parse, and how many distinct line states it keeps. Run by hand.
    #[test]
    #[ignore = "measurement, run by hand with --ignored --nocapture"]
    fn memory_of_a_parse_kept() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let one = include_str!("../../../slopty-ui/src/workspace.rs");
        let mut text = String::new();
        for line in one.lines().cycle() {
            if text.len() + line.len() >= crate::highlight::COLOURED_BYTES {
                break;
            }
            text.push_str(line);
            text.push('\n');
        }
        let rss = || -> Result<u64, String> {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().map_err(|e| e.to_string())
        };
        // The grammars load once, on the first parse: not the parse's to keep.
        std::hint::black_box(parsed("fn a() {}\n", syntax));
        let before = rss()?;
        let lists = parsed(&text, syntax);
        let after = rss()?;
        let states = lists.starts.iter().flatten().collect::<Vec<_>>();
        let mut distinct: Vec<*const LineState> = states.iter().map(|s| Arc::as_ptr(s)).collect();
        distinct.sort_unstable();
        distinct.dedup();
        println!(
            "MEASURE a parse kept, {} lines ({} bytes): resident {} KiB more, {} line states, {} \
             held apart",
            lists.lines.len(),
            text.len(),
            after.saturating_sub(before),
            states.len(),
            distinct.len()
        );
        std::hint::black_box(lists);
        Ok(())
    }

    /// The numbers behind the editor's colours: what a frame pays to style the rows it
    /// shows (60 rows of a 2 000-line Rust file, the tile's worst case), and what a keystroke
    /// pays to splice the line list before the settled parse.
    #[test]
    #[ignore = "timing, run by hand with --ignored --nocapture"]
    fn timing_of_a_frame_and_a_keystroke() -> Result<(), String> {
        let syntax = Syntax::for_token("rust").ok_or("rust")?;
        let one = include_str!("../../../slopty-ui/src/workspace.rs");
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

    /// The rows on show come first: in reach of the parse they are parsed to their end in one
    /// step; past it they are guessed once, then the parse goes on down the text.
    #[test]
    fn the_rows_on_show_lead_the_parse() {
        let far = CHUNK.saturating_mul(4);
        let screen = far..far.saturating_add(60);
        assert_eq!(next_step(0, None, None), Step::Parse(CHUNK), "nothing on show");
        assert_eq!(next_step(0, Some(0..60), None), Step::Parse(CHUNK), "the top, in one step");
        let deep = CHUNK.saturating_add(40);
        assert_eq!(next_step(0, Some(40..deep), None), Step::Parse(deep), "to their end");
        assert_eq!(next_step(0, Some(screen.clone()), None), Step::Guess(screen.clone()));
        let guessed = screen.clone();
        assert_eq!(next_step(0, Some(screen.clone()), Some(&guessed)), Step::Parse(CHUNK));
        assert_eq!(next_step(screen.end, Some(screen), None), Step::Parse(CHUNK), "parsed");
    }

    /// `rows` lines of Rust, each a keyword, a string and a comment.
    fn source(rows: usize) -> String {
        use std::fmt::Write as _;
        (0..rows).fold(String::new(), |mut text, n| {
            let _infallible = writeln!(text, "let v{n} = \"{n}\"; // {n}");
            text
        })
    }

    /// The highlighter of an editor in a window, its text set whole and the rows `seen` on
    /// show; ticks the executor until the rows on show are coloured, at most `ticks` times.
    /// How many ticks it took and how long, or `None` when it did not get there.
    fn colour_on_show(
        cx: &mut gpui::TestAppContext,
        text: &str,
        seen: Range<usize>,
        ticks: usize,
    ) -> (EditorHighlighter, Option<(usize, Duration)>) {
        let syntax = Syntax::for_token("rust").expect("rust");
        let (state, cx) = cx.add_window_view(EditorState::new);
        let mut h = EditorHighlighter::new(syntax, Theme::default());
        let rope = Rope::from(text);
        h.text = rope.clone();
        let shown =
            rope.line_start_offset(seen.start)..rope.line_end_offset(seen.end.saturating_sub(1));
        // The editor styles the rows on show as it paints, before the parse runs.
        std::hint::black_box(h.styles(&shown, &Unstyled));
        let t = std::time::Instant::now();
        state.update_in(cx, |_, window, cx| h.update(None, &rope, false, window, cx));
        let coloured = |h: &EditorHighlighter| {
            h.styles(&shown, &Unstyled).iter().any(|(_, style)| style.color.is_some())
        };
        for tick in 0..ticks {
            if coloured(&h) {
                return (h, Some((tick, t.elapsed())));
            }
            if !cx.executor().tick() {
                break;
            }
        }
        (h, None)
    }

    /// Rows on show far down a long text are coloured from a guess a few steps after the text
    /// is set, long before the parse reaches them, and the parse then puts the colours a whole
    /// parse gives in their place.
    #[gpui::test]
    fn rows_on_show_far_down_colour_before_the_parse_reaches_them(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let rows = CHUNK.saturating_mul(12);
        let text = source(rows);
        let top = CHUNK.saturating_mul(10);
        let seen = top..top.saturating_add(40);
        let (h, got) = colour_on_show(cx, &text, seen.clone(), 50);
        let (ticks, _) = got.expect("the rows on show coloured within 50 ticks");
        let frontier = h.parsed.borrow().frontier();
        assert!(frontier < seen.start, "guessed at tick {ticks}, the parse at row {frontier}");
        cx.run_until_parked();
        let parsed = h.parsed.borrow();
        assert_eq!(parsed.frontier(), parsed.lines.len(), "then the whole text");
        assert!(parsed.lines.len() > rows.saturating_sub(1), "{} rows", parsed.lines.len());
        let syntax = Syntax::for_token("rust").expect("rust");
        assert_eq!(colours(&parsed), super::super::spans(&text, syntax), "as a whole parse");
    }

    /// The time to a coloured viewport in a 20 000-line Rust file, before (the whole text
    /// parsed, then landed) and now (the rows on show first), at the top and in the middle.
    /// Run by hand; `docs/MEASUREMENTS.md` has the numbers.
    #[gpui::test]
    #[ignore = "timing, run by hand with --ignored --nocapture"]
    fn timing_of_a_coloured_viewport(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let syntax = Syntax::for_token("rust").expect("rust");
        let one = include_str!("../../../slopty-ui/src/workspace.rs");
        let text: String = one.lines().cycle().take(20_000).collect::<Vec<_>>().join("\n");
        let t = std::time::Instant::now();
        std::hint::black_box(parsed(&text, syntax));
        let before = t.elapsed();
        for (place, seen) in [("the top", 0..60), ("the middle", 10_000..10_060)] {
            let (_h, got) = colour_on_show(cx, &text, seen, 10_000);
            let (ticks, after) = got.expect("coloured");
            println!(
                "MEASURE a coloured viewport at {place} of 20 000 lines: before {before:?}, \
                 now {after:?} ({ticks} ticks)"
            );
        }
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
