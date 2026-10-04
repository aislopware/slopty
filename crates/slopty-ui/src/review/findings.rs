//! What an agent's own review found, read from its answer: each finding's words and, where it
//! names one, the file and lines it is about.
//!
//! Codex writes its reviewer's findings one way (`- {title} — {path}:{start}-{end}`, the body
//! indented under it); Claude Code's `/code-review` answers in prose, a list of findings each
//! naming its place as `path:line`, `path:12-18`, `path#L12-L18` or "`path` line 12". The reading
//! is tolerant: a list item names a finding whether or not its place can be read, and one with
//! no place is kept, never dropped. Nothing here draws.

/// Where a finding is: a file as the agent named it, and its lines when it named them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Place {
    /// The file, as the agent wrote it: from the repository's root, from the thread's folder,
    /// or absolute.
    pub path: String,
    /// The first and last lines, on the new side.
    pub lines: Option<(u32, u32)>,
}

impl Place {
    /// How it reads: `src/a.rs:12-18`, `src/a.rs:12`, `src/a.rs`.
    #[must_use]
    pub fn words(&self) -> String {
        match self.lines {
            Some((line, end)) if end > line => format!("{}:{line}-{end}", self.path),
            Some((line, _)) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

/// One thing the review raised.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finding {
    /// Its first line, its place taken out of it.
    pub title: String,
    /// The rest of what the agent wrote of it.
    pub body: String,
    /// Where it is, when the agent said.
    pub place: Option<Place>,
}

impl Finding {
    /// Its words as one text: the title, then the body under it.
    #[must_use]
    pub fn text(&self) -> String {
        if self.body.is_empty() {
            self.title.clone()
        } else {
            format!("{}\n{}", self.title, self.body)
        }
    }
}

/// The findings of `answer`, in its order.
#[must_use]
pub fn read(answer: &str) -> Vec<Finding> {
    items(answer).into_iter().filter_map(|item| finding(&item)).collect()
}

/// The answer's first paragraph, for a review that raised nothing: what the agent said of it.
#[must_use]
pub fn summary(answer: &str) -> Option<String> {
    let first: Vec<&str> = answer
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty() || heading(l))
        .take_while(|l| !l.is_empty())
        .collect();
    let said = plain(&first.join(" "));
    (!said.is_empty()).then_some(said)
}

/// One list item of an answer: its first line, its marker taken off, and the lines under it.
struct Item {
    head: String,
    rest: Vec<String>,
}

/// The answer's list items and bold-led paragraphs. A heading ends an item; a line indented
/// under one, a nested list included, is its.
fn items(answer: &str) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let mut open = false;
    for line in answer.lines() {
        let indent = line.len().saturating_sub(line.trim_start().len());
        let trimmed = line.trim();
        if heading(trimmed) {
            open = false;
            continue;
        }
        if indent <= 1
            && let Some(head) = marked(trimmed)
        {
            out.push(Item { head: head.to_owned(), rest: Vec::new() });
            open = true;
            continue;
        }
        if trimmed.is_empty() {
            if let Some(item) = out.last_mut().filter(|_| open) {
                item.rest.push(String::new());
            }
            continue;
        }
        match out.last_mut() {
            Some(item)
                if open && (indent >= 2 || item.rest.last().is_some_and(|l| !l.is_empty())) =>
            {
                item.rest.push(trimmed.to_owned());
            }
            Some(item) if open && item.rest.is_empty() => item.rest.push(trimmed.to_owned()),
            _ => open = false,
        }
    }
    out
}

/// Whether `line` is a Markdown heading, or a line of its own that only names a section
/// (Codex's "Review comment:").
fn heading(line: &str) -> bool {
    line.starts_with('#')
        || (line.ends_with(':') && !line.contains(' ') && line.len() > 1)
        || matches!(line, "Review comment:" | "Full review comments:")
}

/// The words of a list item's first line, its marker off: `- `, `* `, `+ `, `1. `, `1) `, or
/// a line led by bold words.
fn marked(line: &str) -> Option<&str> {
    for bullet in ["- ", "* ", "+ ", "\u{2022} "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some(rest.trim());
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) =
            line.get(digits..).and_then(|r| r.strip_prefix(". ").or_else(|| r.strip_prefix(") ")))
    {
        return Some(rest.trim());
    }
    line.starts_with("**").then_some(line)
}

/// The finding an item holds: its place from its first line or, failing that, from the lines
/// under it.
fn finding(item: &Item) -> Option<Finding> {
    let (title, lead, place) = if let Some((place, start, end)) = locate(&item.head) {
        let (before, after) = cut(&item.head, start, end);
        if before.is_empty() {
            (after, String::new(), Some(place))
        } else {
            (before, after, Some(place))
        }
    } else {
        let place = item.rest.iter().find_map(|l| locate(l)).map(|(p, ..)| p);
        (item.head.clone(), String::new(), place)
    };
    let title = plain(&title);
    let rest = item.rest.join("\n");
    let body = if lead.is_empty() { rest } else { format!("{lead}\n{rest}") };
    let body = body.trim().to_owned();
    if title.is_empty() && body.is_empty() {
        return None;
    }
    Some(Finding { title, body, place })
}

/// The words of `line` before the bytes from `start` to `end` and after them, without the
/// joints that tied them to the place.
fn cut(line: &str, start: usize, end: usize) -> (String, String) {
    let before = line.get(..start).unwrap_or_default();
    let after = line.get(end..).unwrap_or_default();
    let joint = |c: char| c.is_whitespace() || "`()\u{2014}\u{2013}-:,\u{b7}".contains(c);
    let mut before = before.trim_end_matches(joint);
    // "at src/a.rs:3": the words that only led to the place go with it.
    for led in [" at", " in", " on", " see", " See"] {
        before = before.strip_suffix(led).unwrap_or(before);
    }
    let before = before.trim_end_matches(joint).to_owned();
    let after = after.trim_start_matches(|c: char| joint(c) || c == '.').trim_end().to_owned();
    (before, after)
}

/// A finding's words without Markdown's emphasis around them.
fn plain(words: &str) -> String {
    words.replace("**", "").replace("__", "").trim().to_owned()
}

/// The first place `line` names, and the bytes it spans there, including what wraps it: a
/// path with its lines (`a.rs:12`, `a.rs:12-18`, `a.rs:L12-L18`, `a.rs#L12`), or a path and
/// "line 12" or "lines 12-18" after it, or a path alone.
fn locate(line: &str) -> Option<(Place, usize, usize)> {
    let mut alone: Option<(Place, usize, usize)> = None;
    for (start, word) in words(line) {
        let token = word
            .trim_start_matches(|c: char| "`'\"*([<".contains(c))
            .trim_end_matches(|c: char| "`'\"*)]>,;.:".contains(c));
        let Some(offset) = word.find(token) else { continue };
        let at = start.saturating_add(offset);
        let end = at.saturating_add(token.len());
        let (path, lines) = split(token);
        if !pathish(path) {
            continue;
        }
        let path = repo_path(path);
        if lines.is_some() {
            return Some((Place { path, lines }, start, start.saturating_add(word.len())));
        }
        let rest = line.get(end..).unwrap_or_default();
        if let Some((lines, used)) = said_lines(rest) {
            let reach = end.saturating_add(used);
            return Some((Place { path, lines: Some(lines) }, start, reach));
        }
        if alone.is_none() {
            alone = Some((Place { path, lines: None }, start, start.saturating_add(word.len())));
        }
    }
    alone
}

/// The words of `line` with where each starts.
fn words(line: &str) -> impl Iterator<Item = (usize, &str)> {
    line.split_whitespace().map(move |w| {
        let start = (w.as_ptr() as usize).saturating_sub(line.as_ptr() as usize);
        (start, w)
    })
}

/// A token's path and the lines after it: `a.rs:12-18` is `a.rs` and (12, 18).
fn split(token: &str) -> (&str, Option<(u32, u32)>) {
    for sep in ["#L", ":L", ":"] {
        if let Some((path, rest)) = token.split_once(sep)
            && let Some(lines) = range(rest)
        {
            return (path, Some(lines));
        }
    }
    (token, None)
}

/// `12`, `12-18`, `12–18`, `L12-L18`, `12:5` (a column, dropped).
fn range(text: &str) -> Option<(u32, u32)> {
    let text = text.split(':').next().unwrap_or(text);
    let (a, b) = text
        .split_once('-')
        .or_else(|| text.split_once('\u{2013}'))
        .map_or((text, None), |(a, b)| (a, Some(b)));
    let number = |s: &str| s.trim().trim_start_matches('L').parse::<u32>().ok();
    let first = number(a)?;
    let last = match b {
        Some(b) => number(b)?,
        None => first,
    };
    (first > 0).then_some((first.min(last), first.max(last)))
}

/// Lines said in words right after a path: " line 12", " (lines 12-18)", " at line 3".
fn said_lines(rest: &str) -> Option<((u32, u32), usize)> {
    let lower = rest.to_ascii_lowercase();
    let lead =
        lower.len().saturating_sub(lower.trim_start_matches([' ', '(', ',', '`', '"', ')']).len());
    let tail = lower.get(lead..)?;
    let tail_at = tail.strip_prefix("at ").map_or(lead, |_| lead.saturating_add(3));
    let tail = lower.get(tail_at..)?;
    let word = ["lines ", "line ", "l"].into_iter().find(|w| tail.starts_with(w))?;
    let from = tail_at.saturating_add(word.len());
    let numbers: String = rest
        .get(from..)?
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '-' | '\u{2013}' | 'L'))
        .collect();
    let lines = range(numbers.trim_end_matches(['-', '\u{2013}']))?;
    Some((lines, from.saturating_add(numbers.len())))
}

/// Whether `token` reads as a file's path: no scheme but a forge's file link, a name with an
/// extension or a folder in it, and no spaces.
fn pathish(token: &str) -> bool {
    if token.is_empty() || token.contains("://") && !token.contains("/blob/") {
        return false;
    }
    let name = token.rsplit('/').next().unwrap_or(token);
    let dotted = name.split_once('.').is_some_and(|(stem, ext)| {
        !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()) && !stem.is_empty()
    });
    let named = token.contains('/') && !name.is_empty();
    (dotted || named)
        && token.chars().all(|c| c.is_alphanumeric() || "/._-@+~".contains(c) || c == ':')
        && !token.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// A path as the repository names it where the agent's words say more: a forge's file link
/// becomes the path after its commit, and `./` and git's `a/`, `b/` go.
fn repo_path(token: &str) -> String {
    let token = token
        .split_once("/blob/")
        .map_or(token, |(_, rest)| rest.split_once('/').map_or(rest, |(_commit, path)| path));
    let token = token.strip_prefix("./").unwrap_or(token);
    let token = token.strip_prefix("a/").or_else(|| token.strip_prefix("b/")).unwrap_or(token);
    token.to_owned()
}

/// The file among `files` that `path` names: the same path, or one it ends with (an absolute
/// path, or one from a folder above), or the one file that ends with it.
#[must_use]
pub fn resolve<'a>(
    path: &str,
    files: impl IntoIterator<Item = &'a str> + Clone,
) -> Option<&'a str> {
    let path = path.trim_start_matches('/');
    if let Some(same) = files.clone().into_iter().find(|f| *f == path) {
        return Some(same);
    }
    if let Some(under) = files.clone().into_iter().find(|f| path.ends_with(&format!("/{f}"))) {
        return Some(under);
    }
    let mut ending = files.into_iter().filter(|f| f.ends_with(&format!("/{path}")));
    match (ending.next(), ending.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(path: &str, lines: Option<(u32, u32)>) -> Place {
        Place { path: path.to_owned(), lines }
    }

    /// Codex's reviewer, as `render_review_output_text` writes it: the overall explanation,
    /// a header, then each finding with its absolute place and its body indented.
    #[test]
    fn codexs_findings_are_read_with_their_places_and_bodies() {
        let said = "The change is mostly sound.\n\nFull review comments:\n\n\
                    - [P1] Off by one \u{2014} /work/repo/src/a.rs:3-4\n  The loop skips the last line.\n  It should be inclusive.\n\n\
                    - [P2] Unused import \u{2014} /work/repo/src/b.rs:1-1\n  `std::io` is never used.";
        let found = read(said);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!(found[0].title, "[P1] Off by one");
        assert_eq!(found[0].body, "The loop skips the last line.\nIt should be inclusive.");
        assert_eq!(found[0].place, Some(place("/work/repo/src/a.rs", Some((3, 4)))));
        assert_eq!(found[1].place, Some(place("/work/repo/src/b.rs", Some((1, 1)))));
        assert_eq!(summary(said).as_deref(), Some("The change is mostly sound."));
    }

    /// Claude Code's prose: numbered or bulleted findings, each naming its place its own way,
    /// a place in the body when the title has none, and one with no place kept all the same.
    /// Headings and the opening paragraph are no findings.
    #[test]
    fn prose_findings_are_read_whichever_way_they_name_their_place() {
        let said = "## Code review\n\nI found four issues.\n\n\
                    1. **Missing bounds check** (`src/parse.rs:42`)\n   Indexing panics on empty input.\n\
                    2. **Race on the cache** in `src/cache.rs` lines 10-14: two writers.\n\
                    - Leaked handle at src/io.rs#L7-L9\n\
                    - Wrong error kind\n  See `src/err.rs:L20`.\n\
                    - The README still says v1\n";
        let found = read(said);
        let places: Vec<Option<String>> =
            found.iter().map(|f| f.place.as_ref().map(Place::words)).collect();
        assert_eq!(
            places,
            [
                Some("src/parse.rs:42".to_owned()),
                Some("src/cache.rs:10-14".to_owned()),
                Some("src/io.rs:7-9".to_owned()),
                Some("src/err.rs:20".to_owned()),
                None,
            ]
        );
        assert_eq!(found[0].title, "Missing bounds check");
        assert_eq!(found[0].body, "Indexing panics on empty input.");
        assert_eq!(
            (found[1].title.as_str(), found[1].body.as_str()),
            ("Race on the cache", "two writers.")
        );
        assert_eq!(found[3].title, "Wrong error kind");
        assert_eq!(found[4].title, "The README still says v1", "kept, though it names no place");
        assert_eq!(summary(said).as_deref(), Some("I found four issues."));
    }

    /// A review that raised nothing has no findings; what the agent said of it is its summary.
    #[test]
    fn a_review_with_nothing_to_raise_says_so() {
        let said = "No issues found. The change does what it says.";
        assert_eq!(read(said), []);
        assert_eq!(summary(said).as_deref(), Some(said));
    }

    /// Versions, numbers and links are no places; a forge's file link is its file.
    #[test]
    fn what_only_looks_like_a_place_is_not_one() {
        assert_eq!(locate("bump to 1.2.3 now"), None);
        assert_eq!(locate("see https://example.com/x.html"), None);
        let linked = locate("at https://github.com/o/r/blob/abc123/src/a.rs#L5-L6").map(|l| l.0);
        assert_eq!(linked, Some(Place { path: "src/a.rs".to_owned(), lines: Some((5, 6)) }));
        assert_eq!(range("18-12"), Some((12, 18)), "a range given backwards");
        assert_eq!(range("0"), None, "lines count from 1");
    }

    /// A place is the review's file it ends with, an absolute path or one from the folder
    /// above included; a short name that two files end with is no one's.
    #[test]
    fn a_place_resolves_to_the_reviews_file() {
        let files = ["src/a.rs", "crates/x/src/lib.rs", "crates/y/src/lib.rs"];
        assert_eq!(resolve("src/a.rs", files), Some("src/a.rs"));
        assert_eq!(resolve("/work/repo/src/a.rs", files), Some("src/a.rs"));
        assert_eq!(resolve("x/src/lib.rs", files), Some("crates/x/src/lib.rs"));
        assert_eq!(resolve("src/lib.rs", files), None, "two files end with it");
        assert_eq!(resolve("src/b.rs", files), None);
    }
}
