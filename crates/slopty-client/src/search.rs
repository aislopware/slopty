//! A text search on a worker, as the client holds it while the pages come in.
//!
//! [`SearchResults::start`] numbers a search and makes the request; each
//! [`slopty_proto::search::SearchEvent`] is applied in turn ([`SearchResults::apply`]), and one
//! for any other search (a page still in flight for the query before this one) is dropped.
//! The worker sends files in the order its threads find them; they are kept in path order
//! here, so a search reads the same every time and a file's neighbours in a folder sit
//! together. [`SearchResults::rows`] is the list the surface draws: a file, then its lines
//! with the context round them.

use std::ops::Range;

use slopty_proto::search::{
    FileHits, LineHit, SearchEvent, SearchQuery, SearchRequest, SearchSummary,
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

    /// Take `event` in: `false` when it is about another search, or comes after this one
    /// ended.
    pub fn apply(&mut self, event: SearchEvent) -> bool {
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

    /// Matching lines so far.
    #[must_use]
    pub const fn lines(&self) -> u32 {
        self.lines
    }

    /// Where it stands.
    #[must_use]
    pub const fn state(&self) -> &SearchState {
        &self.state
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

/// Where `hit`'s matches fall in its text, each on a character's boundary.
#[must_use]
pub fn matches(hit: &LineHit) -> Vec<Range<usize>> {
    hit.spans
        .iter()
        .filter_map(|span| {
            let start = usize::try_from(span.start).ok()?;
            let end = usize::try_from(span.end).ok()?;
            (start < end && hit.text.is_char_boundary(start) && hit.text.is_char_boundary(end))
                .then_some(start..end)
        })
        .collect()
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
    use slopty_proto::search::{ContextLine, Span};

    use super::*;

    fn file(path: &str, lines: &[u32]) -> FileHits {
        FileHits {
            path: path.to_owned(),
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

    /// A line's matches are its spans, less any that is empty or falls inside a character.
    #[test]
    fn a_lines_matches_are_its_whole_spans() {
        let hit = LineHit {
            line: 1,
            text: "let é = foo(1) + foo(22);".to_owned(),
            spans: vec![
                Span { start: 9, end: 15 },
                Span { start: 5, end: 5 },
                Span { start: 4, end: 5 },
                Span { start: 18, end: 25 },
            ],
            cut_before: false,
            cut_after: false,
        };
        let found: Vec<&str> = matches(&hit).into_iter().filter_map(|r| hit.text.get(r)).collect();
        assert_eq!(found, ["foo(1)", "foo(22)"]);
    }

    #[test]
    fn a_files_field_reads_as_globs() {
        assert_eq!(globs(" *.rs, !tests/** src/*.md ,"), ["*.rs", "!tests/**", "src/*.md"]);
        assert_eq!(globs("  "), Vec::<String>::new());
        assert_eq!(join("/w/", "a/b.rs"), "/w/a/b.rs");
        assert_eq!(join("", "a"), "a");
    }
}
