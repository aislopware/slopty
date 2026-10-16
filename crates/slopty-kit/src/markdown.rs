//! Markdown said as plain words, for chrome that draws one line of it.

/// A line of Markdown said as plain words, for chrome that draws one line of text.
///
/// An agent's last line, its question on a note and its state's word draw it. The marks go and
/// what they mark stays: `**1.48.0**` reads 1.48.0, `` `cargo test` `` reads cargo test, a link
/// reads its words, an image its alt text; a heading's, a quote's or a list item's lead is
/// dropped, and a backslash's character is itself. `snake_case` and `2 * 3` keep their marks,
/// since those are not emphasis.
#[must_use]
pub fn plain_line(line: &str) -> String {
    marked_line(line).into_iter().map(|(c, _)| c).collect()
}

/// What a character of a line is to [`plain_line`]: the line's own, which may be a mark of
/// emphasis; one a backslash or an address made literal; or one inside a code span.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mark {
    Own,
    Literal,
    Code,
}

/// The line's characters, each with what it is, its marks taken off.
fn marked_line(line: &str) -> Vec<(char, Mark)> {
    let chars: Vec<char> = without_leads(line.trim()).chars().collect();
    let mut out: Vec<(char, Mark)> = Vec::with_capacity(chars.len());
    let mut i = 0_usize;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i.saturating_add(1)).copied();
        i = match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                out.extend(next.map(|n| (n, Mark::Literal)));
                i.saturating_add(2)
            }
            '`' => code_at(&chars, i, &mut out),
            '!' if next == Some('[') => link_or(&chars, i, i.saturating_add(1), &mut out),
            '[' => link_or(&chars, i, i, &mut out),
            '<' => autolink_at(&chars, i, &mut out),
            _ => {
                out.push((c, Mark::Own));
                i.saturating_add(1)
            }
        };
    }
    unemphasise(out)
}

/// `line` without the leads a block puts before its words: a heading's hashes, a quote's
/// mark, a list item's bullet, number or box, however they nest.
fn without_leads(line: &str) -> &str {
    let mut rest = line;
    loop {
        let before = rest;
        let hashes = rest.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes)
            && let Some(after) = rest.get(hashes..).and_then(|r| r.strip_prefix(' '))
        {
            rest = after.trim_start();
        }
        for lead in ["> ", "- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ "] {
            if let Some(after) = rest.strip_prefix(lead) {
                rest = after.trim_start();
            }
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && let Some(after) = rest
                .get(digits..)
                .and_then(|r| r.strip_prefix(". ").or_else(|| r.strip_prefix(") ")))
        {
            rest = after.trim_start();
        }
        if rest == before {
            return rest;
        }
    }
}

/// How many `c` run from `at`.
fn run_of(chars: &[char], at: usize, c: char) -> usize {
    chars.get(at..).map_or(0, |rest| rest.iter().take_while(|ch| **ch == c).count())
}

/// A code span at `at` (its backticks): its words, not the line's own, and where it ends; an
/// unclosed run of backticks is itself.
fn code_at(chars: &[char], at: usize, out: &mut Vec<(char, Mark)>) -> usize {
    let run = run_of(chars, at, '`');
    let body = at.saturating_add(run);
    let mut j = body;
    while let Some(&c) = chars.get(j) {
        if c == '`' {
            let closing = run_of(chars, j, '`');
            if closing == run {
                let inner: String = chars.get(body..j).unwrap_or_default().iter().collect();
                out.extend(inner.trim().chars().map(|ch| (ch, Mark::Code)));
                return j.saturating_add(run);
            }
            j = j.saturating_add(closing);
        } else {
            j = j.saturating_add(1);
        }
    }
    out.extend(std::iter::repeat_n(('`', Mark::Literal), run));
    body
}

/// A link or an image whose `[` is at `open` (`at` being its `!` for an image): its words,
/// and where it ends; brackets that are no link are themselves.
fn link_or(chars: &[char], at: usize, open: usize, out: &mut Vec<(char, Mark)>) -> usize {
    if let Some((words, end)) = link_at(chars, open) {
        out.extend(words.into_iter().map(|ch| (ch, Mark::Own)));
        return end;
    }
    out.extend(chars.get(at).map(|ch| (*ch, Mark::Own)));
    at.saturating_add(1)
}

/// A link's words and where it ends, for `[words](url)` and `[words][ref]` with its `[` at
/// `open`; `None` when the brackets are not a link.
fn link_at(chars: &[char], open: usize) -> Option<(Vec<char>, usize)> {
    let mut depth = 0_usize;
    let mut close = None;
    for (at, c) in chars.iter().enumerate().skip(open) {
        match c {
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let target = close.saturating_add(1);
    let shut = match chars.get(target)? {
        '(' => ')',
        '[' => ']',
        _ => return None,
    };
    let from = target.saturating_add(1);
    let end = chars.get(from..)?.iter().position(|c| *c == shut)?.saturating_add(from);
    let words = chars.get(open.saturating_add(1)..close)?.to_vec();
    Some((words, end.saturating_add(1)))
}

/// An autolink at `at` (its `<`): its address, and where it ends; a `<` that opens none is
/// itself.
fn autolink_at(chars: &[char], at: usize, out: &mut Vec<(char, Mark)>) -> usize {
    let from = at.saturating_add(1);
    let inner = chars.get(from..).and_then(|rest| {
        let len = rest.iter().position(|c| *c == '>')?;
        let inner = rest.get(..len)?;
        let address = inner.contains(&':') && !inner.iter().any(|c| c.is_whitespace());
        address.then_some((inner, len))
    });
    if let Some((inner, len)) = inner {
        out.extend(inner.iter().map(|ch| (*ch, Mark::Literal)));
        return from.saturating_add(len).saturating_add(1);
    }
    out.push(('<', Mark::Own));
    from
}

/// The line with its emphasis, strong and struck marks taken off where they pair: an opening
/// run touches a word on its right, a closing one on its left, and an underscore's run stands
/// outside a word (`snake_case` keeps its own). Only the line's own characters are marks.
fn unemphasise(mut out: Vec<(char, Mark)>) -> Vec<(char, Mark)> {
    for mark in ["**", "__", "~~", "*", "_"] {
        let mark: Vec<char> = mark.chars().collect();
        let (len, first) = (mark.len(), mark.first().copied().unwrap_or_default());
        let word = first == '_';
        let at = |out: &[(char, Mark)], i: usize| {
            out.get(i..i.saturating_add(len)).is_some_and(|run| {
                run.len() == len
                    && run.iter().zip(&mark).all(|(c, m)| c.1 == Mark::Own && c.0 == *m)
            })
        };
        let ch = |out: &[(char, Mark)], i: Option<usize>| i.and_then(|i| out.get(i)).map(|c| c.0);
        let mut i = 0_usize;
        while i.saturating_add(len) <= out.len() {
            let before = ch(&out, i.checked_sub(1));
            let after = ch(&out, Some(i.saturating_add(len)));
            let opens = at(&out, i)
                && after.is_some_and(|c| !c.is_whitespace() && c != first)
                && before != Some(first)
                && !(word && before.is_some_and(char::is_alphanumeric));
            let close = opens
                .then(|| {
                    (i.saturating_add(len).saturating_add(1)..=out.len().saturating_sub(len)).find(
                        |&j| {
                            let last = ch(&out, j.checked_sub(1));
                            let next = ch(&out, Some(j.saturating_add(len)));
                            at(&out, j)
                                && last.is_some_and(|c| !c.is_whitespace() && c != first)
                                && next != Some(first)
                                && !(word && next.is_some_and(char::is_alphanumeric))
                        },
                    )
                })
                .flatten();
            if let Some(j) = close {
                out.drain(j..j.saturating_add(len));
                out.drain(i..i.saturating_add(len));
            } else {
                i = i.saturating_add(1);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// A line of Markdown reads as its words: the marks of emphasis, code, links, images,
    /// headings, quotes and list items go; what is not a mark stays.
    #[test]
    fn a_line_of_markdown_reads_as_its_words() {
        let cases = [
            ("tokio is at **1.48.0** everywhere", "tokio is at 1.48.0 everywhere"),
            ("Ran `cargo test` and *all* passed", "Ran cargo test and all passed"),
            ("__done__ and ~~gone~~", "done and gone"),
            ("See [the docs](https://a.b/c) and ![a chart](x.png)", "See the docs and a chart"),
            ("## Summary", "Summary"),
            ("> - 1. nested lead", "nested lead"),
            ("- [x] shipped", "shipped"),
            ("keep snake_case_names and 2 * 3 * 4", "keep snake_case_names and 2 * 3 * 4"),
            (r"a \*literal\* star", "a *literal* star"),
            ("``a `tick` inside``", "a `tick` inside"),
            ("open <https://a.b> now", "open https://a.b now"),
            ("an [unclosed bracket", "an [unclosed bracket"),
            ("a lone ` tick", "a lone ` tick"),
            ("**bold with `code` in it**", "bold with code in it"),
            ("#hashtag stays", "#hashtag stays"),
        ];
        for (line, words) in cases {
            assert_eq!(plain_line(line), words, "{line}");
        }
    }

    use super::*;
}
