//! A thread's title from the first thing the person said, as plain words.
//!
//! Its first line with the markdown taken out (a link reads as its text, emphasis and code
//! ticks go, a code fence's own lines are skipped), a line that is only a label ending in ":"
//! ("Chart:", "Notes:") joined with the line after it, then cut at a word to [`TITLE_CHARS`]. The
//! markdown goes before the cut, so a link's address never eats the title.

/// The most characters a title keeps before it is cut.
const TITLE_CHARS: usize = 80;

/// A thread's title from `text`, the first thing the person said.
#[must_use]
pub fn title_of(text: &str) -> String {
    let fence = |line: &&str| line.starts_with("```") || line.starts_with("~~~");
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !fence(line))
        .map(|line| plain(block(line)))
        .filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default();
    let line = match lines.next() {
        Some(next) if first.ends_with(':') => format!("{first} {next}"),
        _ => first,
    };
    cut(&line)
}

/// `line` cut to [`TITLE_CHARS`], at the last word that fits, with an ellipsis.
fn cut(line: &str) -> String {
    let Some((at, _)) = line.char_indices().nth(TITLE_CHARS) else { return line.to_owned() };
    let head = line.get(..at).unwrap_or(line);
    let whole_word = line.get(at..).is_some_and(|rest| rest.starts_with(char::is_whitespace));
    let head = match head.rsplit_once(char::is_whitespace) {
        Some((words, _)) if !whole_word && !words.trim().is_empty() => words,
        _ => head,
    };
    format!("{}…", head.trim_end())
}

/// `line` without the marks that begin a block: a heading's `#`s, a quote's `>`, a list item's
/// bullet or number, a task's box.
fn block(line: &str) -> &str {
    let mut line = line;
    loop {
        let before = line;
        line = line.trim_start_matches('>').trim_start();
        let hashes = line.len().saturating_sub(line.trim_start_matches('#').len());
        if (1..=6).contains(&hashes) && line.get(hashes..).is_some_and(|r| r.starts_with(' ')) {
            line = line.get(hashes..).unwrap_or(line).trim_start();
        }
        for bullet in ["- ", "* ", "+ "] {
            if let Some(rest) = line.strip_prefix(bullet) {
                line = rest.trim_start();
            }
        }
        let digits =
            line.len().saturating_sub(line.trim_start_matches(|c: char| c.is_ascii_digit()).len());
        if (1..=9).contains(&digits)
            && let Some(rest) = line
                .get(digits..)
                .and_then(|r| r.strip_prefix(". ").or_else(|| r.strip_prefix(") ")))
        {
            line = rest.trim_start();
        }
        for check in ["[ ] ", "[x] ", "[X] "] {
            if let Some(rest) = line.strip_prefix(check) {
                line = rest.trim_start();
            }
        }
        if line == before {
            return line;
        }
    }
}

/// `text`'s inline markdown taken out: a link or an image as its text, an autolink as its
/// address, code as its words, emphasis and strikethrough marks gone, escapes as the character
/// they escape. A mark between two letters or digits (`snake_case`, `2*3`) stays.
fn plain(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut rest: &[char] = &chars;
    let mut before: Option<char> = None;
    while let Some((&c, after)) = rest.split_first() {
        let next = after.first().copied();
        let took = match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                out.extend(next);
                2
            }
            '`' => {
                let ticks = run_of(rest, '`');
                let body = rest.get(ticks..).unwrap_or_default();
                if let Some(end) = find_run(body, '`', ticks) {
                    let code: String = body.get(..end).unwrap_or_default().iter().collect();
                    out.push_str(code.trim());
                    ticks.saturating_add(end).saturating_add(ticks)
                } else {
                    out.extend(rest.get(..ticks).unwrap_or_default());
                    ticks
                }
            }
            '!' | '[' => {
                let open = if c == '!' { after } else { rest };
                if let (Some('['), Some((words, used))) = (open.first(), link(open)) {
                    out.push_str(&plain(&words));
                    used.saturating_add(usize::from(c == '!'))
                } else {
                    out.push(c);
                    1
                }
            }
            '<' => {
                let inner = after
                    .iter()
                    .position(|&t| t == '>')
                    .map(|n| after.get(..n).unwrap_or_default().iter().collect::<String>());
                if let Some(address) =
                    inner.filter(|s| s.contains("://") || s.starts_with("mailto:"))
                {
                    let used = address.chars().count().saturating_add(2);
                    out.push_str(&address);
                    used
                } else {
                    out.push(c);
                    1
                }
            }
            '*' | '_' | '~' => {
                let run = run_of(rest, c);
                let after_run = rest.get(run).copied();
                let inside_word = before.is_some_and(char::is_alphanumeric)
                    && after_run.is_some_and(char::is_alphanumeric);
                let mark = (c != '~' || run == 2) && !inside_word && run <= 3;
                if !mark {
                    out.extend(rest.get(..run).unwrap_or_default());
                }
                run
            }
            _ => {
                out.push(c);
                1
            }
        };
        let took = took.max(1);
        before = rest.get(took.saturating_sub(1)).copied();
        rest = rest.get(took..).unwrap_or_default();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How many of `mark` `chars` begins with.
fn run_of(chars: &[char], mark: char) -> usize {
    chars.iter().take_while(|&&t| t == mark).count()
}

/// Where in `chars` the first run of exactly `n` of `mark` begins.
fn find_run(chars: &[char], mark: char, n: usize) -> Option<usize> {
    let mut at = 0_usize;
    while let Some(rest) = chars.get(at..).filter(|r| !r.is_empty()) {
        let run = run_of(rest, mark);
        if run == n {
            return Some(at);
        }
        at = at.saturating_add(run.max(1));
    }
    None
}

/// Where the bracket that `chars` begins with closes, counting the brackets nested in it.
fn closing(chars: &[char], open: char, shut: char) -> Option<usize> {
    let mut depth = 0_usize;
    for (at, &c) in chars.iter().enumerate() {
        if c == open {
            depth = depth.saturating_add(1);
        } else if c == shut {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(at);
            }
        }
    }
    None
}

/// The link `chars` begins with (at its `[`): its words, and how many characters it takes. A
/// `[words](address)`, `[words][label]` or `[words][]`; `None` for a bracket that opens none.
fn link(chars: &[char]) -> Option<(String, usize)> {
    let close = closing(chars, '[', ']')?;
    let words: String = chars.get(1..close)?.iter().collect();
    let target = chars.get(close.checked_add(1)?..)?;
    let shut = match target.first()? {
        '(' => closing(target, '(', ')')?,
        '[' => closing(target, '[', ']')?,
        _ => return None,
    };
    Some((words, close.checked_add(shut)?.checked_add(2)?))
}

#[cfg(test)]
mod tests {
    use super::title_of;

    /// Markdown goes before the cut: a link reads as its words, so its address never eats the
    /// title, and emphasis, code ticks, a heading's marks and a list's bullet go too.
    #[test]
    fn markdown_is_stripped_before_the_cut() {
        let long = "Read the [guide](https://example.com/a/very/long/path/that/goes/on/and/on/\
                    and/on/for/a/long/while/index.html) before the **release** and run `cargo t`";
        assert_eq!(title_of(long), "Read the guide before the release and run cargo t");
        assert_eq!(title_of("## *Fix* the __parser__ ~~now~~"), "Fix the parser now");
        assert_eq!(
            title_of("- [ ] see ![the chart](c.png) and <https://x.dev>"),
            "see the chart and https://x.dev"
        );
        assert_eq!(
            title_of("rename snake_case_name and 2*3*4"),
            "rename snake_case_name and 2*3*4"
        );
        assert_eq!(
            title_of(r"keep \*stars\* and [brackets] alone"),
            "keep *stars* and [brackets] alone"
        );
        let words = "word ".repeat(30);
        let cut = title_of(&words);
        assert!(cut.ends_with("word…") && cut.chars().count() <= 81, "cut at a word: {cut}");
    }

    /// A first line that is only a label ending in ":" takes the next line that says something.
    #[test]
    fn a_label_line_joins_the_next_line() {
        assert_eq!(
            title_of("Chart:\n\n  Sales by month as bars\nmore"),
            "Chart: Sales by month as bars"
        );
        assert_eq!(title_of("**Notes:**\n- first point"), "Notes: first point");
        assert_eq!(title_of("Chart:"), "Chart:");
        assert_eq!(title_of("Fix the build\nNotes:"), "Fix the build");
        assert_eq!(title_of("Chart:\n```csv\nmonth,sales\n```"), "Chart: month,sales");
    }
}
