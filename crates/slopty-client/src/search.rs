//! A text search on a worker, as the client holds it while the pages come in, and the replaces
//! made from it.
//!
//! [`SearchResults::start`] numbers a search and makes the request; each
//! [`slopty_proto::search::SearchEvent`] is applied in turn ([`SearchResults::apply`]), and one
//! for any other search (a page still in flight for the query before this one) is dropped.
//! The worker sends files in the order its threads find them; they are kept in path order
//! here, so a search reads the same every time and a file's neighbours in a folder sit
//! together. [`SearchResults::rows`] is the list the surface draws: a file, then its lines
//! with the context round them.
//!
//! [`SearchResults::replace`] asks the worker to replace a line's matches, a file's or all of
//! them, one replace at a time; its answer takes the replaced lines out of the list and keeps
//! the rest replaceable ([`SearchResults::apply`]). [`Preview`] shows what a line becomes.

use std::collections::HashMap;
use std::ops::Range;

use slopty_proto::search::{
    FileHits, FileReplace, FileReplaced, LineHit, MatchAt, Replace, SearchEvent, SearchQuery,
    SearchRequest, SearchSummary, Skipped,
};
use slopty_proto::{ClientMsg, RequestId};

/// Where a search stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SearchState {
    /// Pages are still coming.
    Running,
    /// The worker went through everything, or stopped at the cap.
    Done(SearchSummary),
    /// The worker could not search: why, for a person.
    Failed(String),
    /// This client stopped it before it was done.
    Stopped,
}

/// One line of the results as drawn: a file's heading, one of its matching lines, or a line
/// round them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    /// The file at this index of [`SearchResults::files`].
    File(usize),
    /// Matching line `line` of file `file`.
    Line {
        /// Index into [`SearchResults::files`].
        file: usize,
        /// Index into that file's lines.
        line: usize,
    },
    /// Context line `line` of file `file`.
    Context {
        /// Index into [`SearchResults::files`].
        file: usize,
        /// Index into that file's context.
        line: usize,
    },
}

/// What a replace covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Every match shown.
    All,
    /// Every match shown in the file at this index.
    File(usize),
    /// The matches on one line.
    Line {
        /// Index into [`SearchResults::files`].
        file: usize,
        /// Index into that file's lines.
        line: usize,
    },
}

/// How the last replace went.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Replaced {
    /// It is on its way.
    Pending,
    /// The worker went through it: how many matches in how many files, and the files it left
    /// alone.
    Done {
        /// Files rewritten.
        files: u32,
        /// Matches replaced.
        matches: u32,
        /// Files left alone, and why.
        skipped: Vec<Skipped>,
    },
    /// The worker could not start it: why, for a person.
    Failed(String),
}

/// A replace sent and not yet answered: what it asked for, to take out when it is done.
#[derive(Clone, Debug)]
struct Asked {
    id: RequestId,
    /// The matches asked for, by file.
    matches: HashMap<String, Vec<MatchAt>>,
    /// Lines each replaced match adds: the newlines in the replacement.
    adds: u32,
}

/// One search's results, as far as they have come.
#[derive(Clone, Debug)]
pub struct SearchResults {
    id: RequestId,
    root: String,
    query: SearchQuery,
    /// In path order.
    files: Vec<FileHits>,
    lines: u32,
    state: SearchState,
    asked: Option<Asked>,
    replaced: Option<Replaced>,
}

impl SearchResults {
    /// Search `id` for `query` under `root` (absolute, or `~/…`), and the request that starts
    /// it on the worker.
    #[must_use]
    pub fn start(id: RequestId, root: &str, query: SearchQuery) -> (Self, ClientMsg) {
        let request = ClientMsg::Search(SearchRequest::Start {
            id,
            root: root.to_owned(),
            query: query.clone(),
        });
        let results = Self {
            id,
            root: root.to_owned(),
            query,
            files: Vec::new(),
            lines: 0,
            state: SearchState::Running,
            asked: None,
            replaced: None,
        };
        (results, request)
    }

    /// The request that stops it, while it runs; it is then stopped here too.
    pub fn stop(&mut self) -> Option<ClientMsg> {
        (self.state == SearchState::Running).then(|| {
            self.state = SearchState::Stopped;
            ClientMsg::Search(SearchRequest::Stop { id: self.id })
        })
    }

    /// Take `event` in: `false` when it is about neither this search nor its replace, or
    /// comes after the search ended.
    pub fn apply(&mut self, event: SearchEvent) -> bool {
        if self.asked.as_ref().is_some_and(|a| a.id == event.id()) {
            return self.answered(event);
        }
        if event.id() != self.id || self.state != SearchState::Running {
            return false;
        }
        match event {
            SearchEvent::Hits { files, .. } => {
                for file in files {
                    self.add(file);
                }
            }
            SearchEvent::Done { summary, .. } => self.state = SearchState::Done(summary),
            SearchEvent::Failed { error, .. } => self.state = SearchState::Failed(error),
            SearchEvent::Replaced { .. } => return false,
        }
        true
    }

    fn add(&mut self, file: FileHits) {
        let count = u32::try_from(file.lines.len()).unwrap_or(u32::MAX);
        self.lines = self.lines.saturating_add(count);
        match self.files.binary_search_by(|f| f.path.as_str().cmp(&file.path)) {
            Ok(at) => {
                if let Some(same) = self.files.get_mut(at) {
                    same.lines.extend(file.lines);
                    same.lines.sort_by_key(|l| l.line);
                    same.context.extend(file.context);
                    same.context.sort_by_key(|l| l.line);
                    same.context.dedup_by_key(|l| l.line);
                }
            }
            Err(at) => self.files.insert(at, file),
        }
    }

    /// Ask the worker to replace the matches `scope` covers with `with` (a regular
    /// expression's groups expanded in it when the query is one). `None` while another replace
    /// is on its way, or when the scope holds no match.
    pub fn replace(&mut self, id: RequestId, with: &str, scope: Scope) -> Option<ClientMsg> {
        if self.asked.is_some() {
            return None;
        }
        let whole =
            |hits: &FileHits| -> Vec<MatchAt> { hits.lines.iter().flat_map(matches_on).collect() };
        let files: Vec<FileReplace> = match scope {
            Scope::All => self.files.iter().map(|f| file_replace(f, whole(f))).collect(),
            Scope::File(file) => {
                self.files.get(file).map(|f| file_replace(f, whole(f))).into_iter().collect()
            }
            Scope::Line { file, line } => self
                .files
                .get(file)
                .and_then(|f| Some(file_replace(f, matches_on(f.lines.get(line)?).collect())))
                .into_iter()
                .collect(),
        };
        let files: Vec<FileReplace> = files.into_iter().filter(|f| !f.matches.is_empty()).collect();
        if files.is_empty() {
            return None;
        }
        let matches = files.iter().map(|f| (f.path.clone(), f.matches.clone())).collect();
        let adds = u32::try_from(with.matches('\n').count()).unwrap_or(u32::MAX);
        self.asked = Some(Asked { id, matches, adds });
        self.replaced = Some(Replaced::Pending);
        Some(ClientMsg::Search(SearchRequest::Replace(Replace {
            id,
            root: self.root.clone(),
            query: self.query.clone(),
            with: with.to_owned(),
            files,
        })))
    }

    /// The answer to the replace on its way.
    fn answered(&mut self, event: SearchEvent) -> bool {
        let Some(asked) = self.asked.take() else { return false };
        self.replaced = Some(match event {
            SearchEvent::Replaced { files, skipped, .. } => {
                let matches = files.iter().map(|f| f.matches).fold(0, u32::saturating_add);
                for done in &files {
                    self.take_out(done, &asked);
                }
                let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
                Replaced::Done { files: count, matches, skipped }
            }
            SearchEvent::Failed { error, .. } => Replaced::Failed(error),
            SearchEvent::Hits { .. } | SearchEvent::Done { .. } => {
                self.asked = Some(asked);
                return false;
            }
        });
        true
    }

    /// Take the lines a replace rewrote out of the list, move the lines under them by what it
    /// added, and keep the file's new stamp for the replaces after it.
    fn take_out(&mut self, done: &FileReplaced, asked: &Asked) {
        let Some(wanted) = asked.matches.get(&done.path) else { return };
        let Ok(at) = self.files.binary_search_by(|f| f.path.as_str().cmp(&done.path)) else {
            return;
        };
        let Some(file) = self.files.get_mut(at) else { return };
        file.stamp = done.stamp;
        let before = file.lines.len();
        file.lines.retain(|l| !wanted.iter().any(|m| m.line == l.line));
        let removed = u32::try_from(before.saturating_sub(file.lines.len())).unwrap_or(u32::MAX);
        self.lines = self.lines.saturating_sub(removed);
        if asked.adds > 0 {
            let shift = |line: u32| {
                let above = wanted.iter().filter(|m| m.line < line).count();
                let above = u32::try_from(above).unwrap_or(u32::MAX);
                line.saturating_add(above.saturating_mul(asked.adds))
            };
            file.lines.iter_mut().for_each(|l| l.line = shift(l.line));
            file.context.iter_mut().for_each(|c| c.line = shift(c.line));
        }
        if file.lines.is_empty() {
            self.files.remove(at);
        }
    }

    /// Its number.
    #[must_use]
    pub const fn id(&self) -> RequestId {
        self.id
    }

    /// The directory searched, as asked.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// What was looked for.
    #[must_use]
    pub const fn query(&self) -> &SearchQuery {
        &self.query
    }

    /// The files with matches so far, in path order.
    #[must_use]
    pub fn files(&self) -> &[FileHits] {
        &self.files
    }

    /// Matching lines so far, less those replaced.
    #[must_use]
    pub const fn lines(&self) -> u32 {
        self.lines
    }

    /// Where it stands.
    #[must_use]
    pub const fn state(&self) -> &SearchState {
        &self.state
    }

    /// How the last replace went, once one was asked for.
    #[must_use]
    pub const fn replaced(&self) -> Option<&Replaced> {
        self.replaced.as_ref()
    }

    /// Whether pages are still coming.
    #[must_use]
    pub fn running(&self) -> bool {
        self.state == SearchState::Running
    }

    /// The worker path of a file it found: the root joined to the file's relative path.
    #[must_use]
    pub fn path_of(&self, file: &FileHits) -> String {
        join(&self.root, &file.path)
    }

    /// The list as drawn: each file, then its lines and the context round them in line order,
    /// unless `folded` says the file is folded.
    #[must_use]
    pub fn rows(&self, folded: impl Fn(&str) -> bool) -> Vec<Row> {
        let context = self.files.iter().map(|f| f.context.len()).sum::<usize>();
        let lines = usize::try_from(self.lines).unwrap_or(0).saturating_add(context);
        let mut rows = Vec::with_capacity(self.files.len().saturating_add(lines));
        for (file, hits) in self.files.iter().enumerate() {
            rows.push(Row::File(file));
            if folded(&hits.path) {
                continue;
            }
            let mut around = 0;
            for (line, hit) in hits.lines.iter().enumerate() {
                while hits.context.get(around).is_some_and(|c| c.line < hit.line) {
                    rows.push(Row::Context { file, line: around });
                    around = around.saturating_add(1);
                }
                rows.push(Row::Line { file, line });
            }
            rows.extend((around..hits.context.len()).map(|line| Row::Context { file, line }));
        }
        rows
    }
}

/// The matches a search showed on `hit`, as a replace names them.
fn matches_on(hit: &LineHit) -> impl Iterator<Item = MatchAt> + '_ {
    (0..hit.spans.len())
        .map(|index| MatchAt { line: hit.line, index: u32::try_from(index).unwrap_or(u32::MAX) })
}

fn file_replace(file: &FileHits, matches: Vec<MatchAt>) -> FileReplace {
    FileReplace { path: file.path.clone(), stamp: file.stamp, matches }
}

/// How a piece of a drawn line reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// A match, found.
    Match,
    /// A match a replace takes out.
    Removed,
    /// What a replace puts in its place.
    Inserted,
}

/// What a replace makes of the lines it covers, before it is made: each match struck and its
/// replacement after it.
#[derive(Clone, Debug)]
pub struct Preview {
    with: String,
    /// The query's pattern as a regular expression, for its groups; `None` for a literal query
    /// or a replacement with no `$` to expand.
    regex: Option<regex::Regex>,
}

impl Preview {
    /// The preview of replacing `query`'s matches with `with`.
    #[must_use]
    pub fn new(query: &SearchQuery, with: &str) -> Self {
        let regex = (query.regex && with.contains('$'))
            .then(|| {
                regex::RegexBuilder::new(&query.pattern)
                    .case_insensitive(!query.match_case)
                    .multi_line(true)
                    .build()
                    .ok()
            })
            .flatten();
        Self { with: with.to_owned(), regex }
    }

    /// What the match at `span` of `text` becomes: its groups expanded when the query is a
    /// regular expression, else the replacement as it is.
    #[must_use]
    pub fn replacement(&self, text: &str, span: Range<usize>) -> String {
        let Some(regex) = &self.regex else { return self.with.clone() };
        let found = regex.captures_at(text, span.start).filter(|caps| {
            caps.get(0).is_some_and(|m| m.start() == span.start && m.end() == span.end)
        });
        let found = found.or_else(|| {
            let alone = text.get(span.clone())?;
            regex.captures(alone).filter(|caps| caps.get(0).is_some_and(|m| m.len() == alone.len()))
        });
        let Some(caps) = found else { return self.with.clone() };
        let mut out = String::new();
        caps.expand(&self.with, &mut out);
        out
    }
}

/// `hit`'s text as drawn, and where its marks fall in it: each match marked, or with a
/// `preview`, struck and followed by what replaces it.
#[must_use]
pub fn marked(hit: &LineHit, preview: Option<&Preview>) -> (String, Vec<(Range<usize>, Mark)>) {
    let spans = hit.spans.iter().filter_map(|span| {
        let start = usize::try_from(span.start).ok()?;
        let end = usize::try_from(span.end).ok()?;
        (start < end && hit.text.is_char_boundary(start) && hit.text.is_char_boundary(end))
            .then_some(start..end)
    });
    let Some(preview) = preview else {
        return (hit.text.clone(), spans.map(|range| (range, Mark::Match)).collect());
    };
    let mut text = String::with_capacity(hit.text.len());
    let mut marks = Vec::new();
    let mut at = 0;
    for span in spans {
        if span.start < at {
            continue;
        }
        text.push_str(hit.text.get(at..span.start).unwrap_or_default());
        let removed = text.len();
        text.push_str(hit.text.get(span.clone()).unwrap_or_default());
        marks.push((removed..text.len(), Mark::Removed));
        let inserted = text.len();
        text.push_str(&preview.replacement(&hit.text, span.clone()));
        if text.len() > inserted {
            marks.push((inserted..text.len(), Mark::Inserted));
        }
        at = span.end;
    }
    text.push_str(hit.text.get(at..).unwrap_or_default());
    (text, marks)
}

/// `relative` under `root`, one slash between them.
#[must_use]
pub fn join(root: &str, relative: &str) -> String {
    if root.is_empty() {
        return relative.to_owned();
    }
    format!("{}/{relative}", root.trim_end_matches('/'))
}

/// The globs a "files" field holds: comma- or space-separated, as ripgrep's `--glob` takes
/// each (`*.rs`, `!tests/**`).
#[must_use]
pub fn globs(text: &str) -> Vec<String> {
    text.split([',', ' ']).map(str::trim).filter(|g| !g.is_empty()).map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use slopty_proto::search::{ContextLine, FileStamp, SkipReason, Span};

    use super::*;

    fn file(path: &str, lines: &[u32]) -> FileHits {
        FileHits {
            path: path.to_owned(),
            stamp: FileStamp { size: 10, modified_ns: 1 },
            lines: lines
                .iter()
                .map(|n| LineHit {
                    line: *n,
                    text: "hit".to_owned(),
                    spans: vec![Span { start: 0, end: 3 }],
                    cut_before: false,
                    cut_after: false,
                })
                .collect(),
            context: Vec::new(),
        }
    }

    fn context(line: u32) -> ContextLine {
        ContextLine { line, text: "around".to_owned(), cut_after: false }
    }

    /// Pages land in path order whatever order they came in, a page for another search is
    /// dropped, the end is taken once, and nothing is taken after it.
    #[test]
    fn pages_are_kept_in_path_order_and_stale_ones_dropped() {
        let (mut results, request) = SearchResults::start(4, "~/w/", SearchQuery::default());
        assert!(matches!(request, ClientMsg::Search(SearchRequest::Start { id: 4, .. })));
        let page = |id, files| SearchEvent::Hits { id, files };
        assert!(results.apply(page(4, vec![file("src/z.rs", &[3]), file("a.md", &[1, 9])])));
        assert!(!results.apply(page(3, vec![file("old.rs", &[1])])), "the last query's page");
        assert!(results.apply(page(4, vec![file("src/b.rs", &[2])])));
        let paths: Vec<&str> = results.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "src/b.rs", "src/z.rs"]);
        assert_eq!(results.lines(), 4);
        let first = results.files().first().unwrap();
        assert_eq!(results.path_of(first), "~/w/a.md");

        let summary = SearchSummary { files: 3, lines: 4, ..SearchSummary::default() };
        assert!(results.apply(SearchEvent::Done { id: 4, summary }));
        assert_eq!(results.state(), &SearchState::Done(summary));
        assert!(!results.apply(page(4, vec![file("late.rs", &[1])])), "after its end");
        assert!(results.stop().is_none(), "a finished search has nothing to stop");
    }

    /// The rows are each file then its lines, the context between them in line order; a
    /// folded file keeps only its heading.
    #[test]
    fn rows_list_each_file_then_its_lines_and_their_context() {
        let (mut results, _) = SearchResults::start(1, "/w", SearchQuery::default());
        let mut a = file("a", &[3, 5]);
        a.context = vec![context(2), context(4), context(6)];
        results.apply(SearchEvent::Hits { id: 1, files: vec![file("b", &[1]), a] });
        assert_eq!(
            results.rows(|_| false),
            [
                Row::File(0),
                Row::Context { file: 0, line: 0 },
                Row::Line { file: 0, line: 0 },
                Row::Context { file: 0, line: 1 },
                Row::Line { file: 0, line: 1 },
                Row::Context { file: 0, line: 2 },
                Row::File(1),
                Row::Line { file: 1, line: 0 },
            ]
        );
        assert_eq!(
            results.rows(|path| path == "a"),
            [Row::File(0), Row::File(1), Row::Line { file: 1, line: 0 }]
        );
    }

    /// A stop is sent once, while it runs, and nothing is taken after it.
    #[test]
    fn a_stopped_search_takes_nothing_more() {
        let (mut results, _) = SearchResults::start(2, "/w", SearchQuery::default());
        assert_eq!(results.stop(), Some(ClientMsg::Search(SearchRequest::Stop { id: 2 })));
        assert_eq!(results.stop(), None);
        assert!(!results.apply(SearchEvent::Hits { id: 2, files: vec![file("a", &[1])] }));
        assert_eq!(results.state(), &SearchState::Stopped);
    }

    /// A replace names every match its scope covers with the file's stamp, one at a time; its
    /// answer takes the replaced lines out, keeps the new stamp, leaves a skipped file as it
    /// was, and drops a file with nothing left. It is taken after the search ended.
    #[test]
    fn a_replace_asks_for_its_matches_and_its_answer_takes_them_out() {
        let (mut results, _) = SearchResults::start(1, "/w", SearchQuery::default());
        let mut two = file("two", &[4, 8]);
        two.lines[0].spans.push(Span { start: 0, end: 1 });
        results.apply(SearchEvent::Hits { id: 1, files: vec![file("one", &[2]), two] });
        results.apply(SearchEvent::Done { id: 1, summary: SearchSummary::default() });

        let asked = results.replace(9, "new", Scope::File(1)).unwrap();
        let ClientMsg::Search(SearchRequest::Replace(asked)) = asked else { panic!("{asked:?}") };
        assert_eq!((asked.id, asked.root.as_str(), asked.with.as_str()), (9, "/w", "new"));
        let [only] = asked.files.as_slice() else { panic!("{asked:?}") };
        assert_eq!(only.path, "two");
        assert_eq!(only.stamp, FileStamp { size: 10, modified_ns: 1 });
        let at = |line, index| MatchAt { line, index };
        assert_eq!(only.matches, [at(4, 0), at(4, 1), at(8, 0)]);
        assert!(results.replace(10, "x", Scope::All).is_none(), "one replace at a time");
        assert_eq!(results.replaced(), Some(&Replaced::Pending));

        let stamp = FileStamp { size: 12, modified_ns: 2 };
        let done = FileReplaced { path: "two".into(), matches: 3, stamp };
        assert!(results.apply(SearchEvent::Replaced { id: 9, files: vec![done], skipped: vec![] }));
        assert_eq!(results.files().iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["one"]);
        assert_eq!(results.lines(), 1);
        assert_eq!(
            results.replaced(),
            Some(&Replaced::Done { files: 1, matches: 3, skipped: vec![] })
        );

        let asked = results.replace(11, "n", Scope::Line { file: 0, line: 0 }).unwrap();
        assert!(
            matches!(asked, ClientMsg::Search(SearchRequest::Replace(r)) if r.files.len() == 1)
        );
        let skipped = vec![Skipped { path: "one".into(), why: SkipReason::Changed }];
        let answer = SearchEvent::Replaced { id: 11, files: vec![], skipped: skipped.clone() };
        assert!(results.apply(answer));
        assert_eq!(results.lines(), 1, "a skipped file stays");
        assert_eq!(results.replaced(), Some(&Replaced::Done { files: 0, matches: 0, skipped }));
    }

    /// A replacement that adds lines moves the lines under the ones it rewrote, so a second
    /// replace names them where they now are; lines above stay put.
    #[test]
    fn a_replacement_with_newlines_moves_the_lines_under_it() {
        let (mut results, _) = SearchResults::start(1, "/w", SearchQuery::default());
        let mut a = file("a", &[2, 5, 9]);
        a.context = vec![context(1), context(10)];
        results.apply(SearchEvent::Hits { id: 1, files: vec![a] });
        let _asked = results.replace(2, "x\ny", Scope::Line { file: 0, line: 1 }).unwrap();
        let done = FileReplaced { path: "a".into(), matches: 1, stamp: FileStamp::default() };
        results.apply(SearchEvent::Replaced { id: 2, files: vec![done], skipped: vec![] });
        let a = results.files().first().unwrap();
        assert_eq!(a.lines.iter().map(|l| l.line).collect::<Vec<_>>(), [2, 10]);
        assert_eq!(a.context.iter().map(|c| c.line).collect::<Vec<_>>(), [1, 11]);
    }

    /// The preview strikes each match and puts its replacement after it; a regular
    /// expression's groups are expanded from the match, and a literal query's `$1` is text.
    #[test]
    fn a_preview_strikes_each_match_and_expands_its_groups() {
        let hit = LineHit {
            line: 1,
            text: "let a = foo(1) + foo(22);".to_owned(),
            spans: vec![Span { start: 8, end: 14 }, Span { start: 17, end: 24 }],
            cut_before: false,
            cut_after: false,
        };
        let regex =
            SearchQuery { pattern: r"foo\((\d+)\)".into(), regex: true, ..SearchQuery::default() };
        let preview = Preview::new(&regex, "bar[$1]");
        let (text, marks) = marked(&hit, Some(&preview));
        assert_eq!(text, "let a = foo(1)bar[1] + foo(22)bar[22];");
        let pieces: Vec<(&str, Mark)> =
            marks.iter().map(|(r, m)| (text.get(r.clone()).unwrap(), *m)).collect();
        assert_eq!(
            pieces,
            [
                ("foo(1)", Mark::Removed),
                ("bar[1]", Mark::Inserted),
                ("foo(22)", Mark::Removed),
                ("bar[22]", Mark::Inserted),
            ]
        );
        let literal = SearchQuery { pattern: "foo".into(), ..SearchQuery::default() };
        assert_eq!(Preview::new(&literal, "$1").replacement("foo", 0..3), "$1");
        let (_, deleted) = marked(&hit, Some(&Preview::new(&literal, "")));
        assert!(deleted.iter().all(|(_, m)| *m == Mark::Removed), "nothing is put in");
        let (plain, found) = marked(&hit, None);
        assert_eq!(plain, hit.text);
        assert_eq!(found, [(8..14, Mark::Match), (17..24, Mark::Match)]);
    }

    #[test]
    fn a_files_field_reads_as_globs() {
        assert_eq!(globs(" *.rs, !tests/** src/*.md ,"), ["*.rs", "!tests/**", "src/*.md"]);
        assert_eq!(globs("  "), Vec::<String>::new());
        assert_eq!(join("/w/", "a/b.rs"), "/w/a/b.rs");
        assert_eq!(join("", "a"), "a");
    }
}
