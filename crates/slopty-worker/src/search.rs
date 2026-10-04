//! Project-wide text search: the files under a directory that `.gitignore` lets through, their
//! lines matching a query, streamed to the client in pages as they are found.
//!
//! ripgrep's own libraries do the work: `ignore` walks the tree on several threads with the
//! ignore files honoured, `grep-regex` builds the matcher (a literal, a regex, either case,
//! whole words) and `grep-searcher` reads each file line by line, stopping at the first NUL
//! byte as ripgrep does, so a binary file reports nothing. A file's lines are gathered before
//! they are sent, so a file found binary half way leaves no trace. [`search`] is the whole
//! search on the caller's thread, and [`collect`] the same gathered into one answer for an
//! orchestration verb; [`Searches`] runs one per connection off the runtime, a new one stopping
//! the last.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use slopty_proto::search::{
    ContextLine, FileHits, LINE_BYTES, LineHit, MAX_CONTEXT, MAX_LINES, PAGE_BYTES, SearchEvent,
    SearchQuery, SearchRequest, SearchSummary, Span,
};
use slopty_proto::{RequestId, WorkerMsg};

/// How long a found file waits for others to share its page: short enough that the first
/// match shows while the walk is still going, long enough that a burst goes as one page.
const FLUSH: Duration = Duration::from_millis(16);

/// Matches a line reports at most; a line of `a` searched for `a` needs no more to be seen.
const MAX_SPANS: usize = 64;

/// How much of a long line is kept before its first match when it is cut.
const LEAD: usize = 40;

/// Files found and not yet taken into a page, between the walk and the pager: the walk waits
/// when the client reads slower than the disk.
const QUEUE: usize = 64;

/// Directories no search looks in: a version control system's own store.
const STORES: [&str; 4] = [".git", ".hg", ".jj", ".svn"];

/// Search the files under `root` for `query`, handing each page of files with matches to
/// `emit` as it fills. `emit` returning `false`, or `cancel` set from elsewhere, stops the
/// search where it is.
///
/// Hidden files are searched (a `.github` workflow is part of a project), what the ignore
/// files exclude is not, git repository or not, nor a version control store. Past `limit`
/// matching lines in all (never more than [`MAX_LINES`]) the search stops, and the summary
/// says it was capped.
///
/// # Errors
///
/// For a person: the root is not a directory, or the pattern or a glob does not parse.
pub fn search(
    root: &Path,
    query: &SearchQuery,
    limit: u32,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Vec<FileHits>) -> bool,
) -> Result<SearchSummary, String> {
    search_until(root, query, limit, Stop { cancel, until: None }, emit)
}

/// [`search`], also stopped at `stop.until`: the files not reached by then are not searched,
/// and the summary says it was capped.
fn search_until(
    root: &Path,
    query: &SearchQuery,
    limit: u32,
    stop: Stop<'_>,
    emit: &mut dyn FnMut(Vec<FileHits>) -> bool,
) -> Result<SearchSummary, String> {
    let Stop { cancel, .. } = stop;
    let started = Instant::now();
    if !root.is_dir() {
        return Err(format!("No folder at {}", root.display()));
    }
    if query.pattern.is_empty() {
        return Ok(SearchSummary::default());
    }
    let matcher = matcher(query)?;
    let walk = walker(root, &query.globs)?;
    let found = Found { limit: limit.min(MAX_LINES), ..Found::default() };
    let context = query.context.min(MAX_CONTEXT);
    let shared = Walk { root, matcher: &matcher, found: &found, context, stop };
    let (tx, rx) = std::sync::mpsc::sync_channel::<FileHits>(QUEUE);
    let pages = std::thread::scope(|scope| {
        let walking =
            std::thread::Builder::new().name("search-walk".to_owned()).spawn_scoped(scope, || {
                walk.run(|| {
                    let tx = tx.clone();
                    let shared = &shared;
                    let mut searcher = searcher(context);
                    Box::new(move |entry| visit(entry, shared, &mut searcher, &tx))
                });
                drop(tx);
            });
        if let Err(e) = walking {
            return Err(format!("The search could not start: {e}"));
        }
        Ok(page_out(&rx, cancel, emit))
    })?;
    Ok(SearchSummary {
        files: pages.files,
        lines: pages.lines,
        searched: found.searched.load(Ordering::Relaxed),
        capped: found.capped.load(Ordering::Relaxed),
        elapsed_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
    })
}

/// [`search`] to its end, every file it found in path order: the answer to an orchestration
/// verb, which has one reply and nothing to stream to.
///
/// Nobody sees a page before the end, so nothing but `cancel` and `within` stops a search
/// that finds little in a large tree: `cancel` is for the caller to set when the request it
/// answers is gone, and `within` bounds it when the request stays but its caller gave up.
/// Stopped by either, what it found by then is the answer, and the summary says it was capped.
///
/// # Errors
///
/// As [`search`]'s.
pub fn collect(
    root: &Path,
    query: &SearchQuery,
    limit: u32,
    cancel: &AtomicBool,
    within: Duration,
) -> Result<(Vec<FileHits>, SearchSummary), String> {
    let stop = Stop { cancel, until: Instant::now().checked_add(within) };
    let mut files = Vec::new();
    let summary = search_until(root, query, limit, stop, &mut |page| {
        files.extend(page);
        true
    })?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((files, summary))
}

/// Sets its flag when dropped: a search's stop, held by the request it answers, so the walk
/// ends with the request however that ends.
#[derive(Debug, Default)]
pub struct StopOnDrop(Arc<AtomicBool>);

impl StopOnDrop {
    /// The flag to hand the search.
    #[must_use]
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// What stops a search besides its cap: `cancel` set from elsewhere, or the clock passing
/// `until`.
#[derive(Clone, Copy, Debug)]
struct Stop<'a> {
    cancel: &'a AtomicBool,
    until: Option<Instant>,
}

impl Stop<'_> {
    /// The deadline has passed.
    fn late(&self) -> bool {
        self.until.is_some_and(|until| Instant::now() >= until)
    }
}

/// The matcher for `query`: a line never matches across its end, and no pattern can ask for
/// the NUL byte the binary check stops at.
fn matcher(query: &SearchQuery) -> Result<RegexMatcher, String> {
    RegexMatcherBuilder::new()
        .case_insensitive(!query.match_case)
        .word(query.whole_word)
        .fixed_strings(!query.regex)
        .line_terminator(Some(b'\n'))
        .ban_byte(Some(0))
        .build(&query.pattern)
        .map_err(|e| e.to_string())
}

/// The walk under `root`, narrowed by ripgrep-style `globs`, on a few of the machine's cores:
/// the worker also runs shells and streams while it searches.
///
/// The globs narrow what the ignore files let through, as VS Code's "files to include" does.
/// Given to the walk as overrides they would outrank `.gitignore`, as `rg -g` does, and `*.rs`
/// would bring back every ignored Rust file.
fn walker(root: &Path, globs: &[String]) -> Result<ignore::WalkParallel, String> {
    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    for glob in globs.iter().map(|g| g.trim()).filter(|g| !g.is_empty()) {
        overrides.add(glob).map_err(|e| format!("Bad glob {glob}: {e}"))?;
    }
    let overrides = overrides.build().map_err(|e| e.to_string())?;
    let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 8));
    Ok(WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .threads(threads)
        .filter_entry(move |entry| {
            let dir = entry.file_type().is_some_and(|t| t.is_dir());
            !STORES.iter().any(|store| entry.file_name() == *store)
                && !overrides.matched(entry.path(), dir).is_ignore()
        })
        .build_parallel())
}

/// A searcher that numbers lines and gives `context` lines each side of a match.
fn searcher(context: u32) -> Searcher {
    let context = usize::try_from(context).unwrap_or(0);
    SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        .before_context(context)
        .after_context(context)
        .build()
}

/// What the walk's threads share: the lines the search may report, the files searched, the
/// lines taken against the cap, and whether the cap cut the search short.
#[derive(Debug, Default)]
struct Found {
    limit: u32,
    searched: AtomicU32,
    lines: AtomicU32,
    capped: AtomicBool,
}

impl Found {
    /// Lines still to be had under the cap.
    fn left(&self) -> u32 {
        self.limit.saturating_sub(self.lines.load(Ordering::Relaxed))
    }

    /// Take up to `wanted` lines against the cap: how many were granted. Fewer than wanted
    /// means the cap was reached with lines left over.
    fn claim(&self, wanted: usize) -> usize {
        let wanted = u32::try_from(wanted).unwrap_or(u32::MAX);
        let limit = self.limit;
        let before = self
            .lines
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                Some(used.saturating_add(wanted).min(limit))
            })
            .unwrap_or(limit);
        let granted = limit.saturating_sub(before).min(wanted);
        if granted < wanted {
            self.capped.store(true, Ordering::Relaxed);
        }
        usize::try_from(granted).unwrap_or(0)
    }

    fn full(&self) -> bool {
        self.left() == 0
    }
}

/// What every thread of the walk reads: where it started, what it matches, the context it
/// gives, the shared counts and the stop.
#[derive(Clone, Copy, Debug)]
struct Walk<'a> {
    root: &'a Path,
    matcher: &'a RegexMatcher,
    found: &'a Found,
    context: u32,
    stop: Stop<'a>,
}

/// One entry of the walk: a regular file is searched, and its lines, when it has any, go to
/// the pager whole.
fn visit(
    entry: Result<ignore::DirEntry, ignore::Error>,
    walk: &Walk<'_>,
    searcher: &mut Searcher,
    tx: &SyncSender<FileHits>,
) -> WalkState {
    let Walk { root, matcher, found, context, stop } = *walk;
    let cancel = stop.cancel;
    if cancel.load(Ordering::Relaxed) || found.full() {
        return WalkState::Quit;
    }
    if stop.late() {
        found.capped.store(true, Ordering::Relaxed);
        return WalkState::Quit;
    }
    let Ok(entry) = entry else { return WalkState::Continue };
    #[expect(clippy::filetype_is_file, reason = "a FIFO or a device would block or never end")]
    let regular = entry.file_type().is_some_and(|t| t.is_file());
    if !regular {
        return WalkState::Continue;
    }
    found.searched.fetch_add(1, Ordering::Relaxed);
    let mut sink = Gathered {
        matcher,
        cancel,
        left: found.left(),
        lines: Vec::new(),
        context: Vec::new(),
        more: false,
    };
    let read = searcher.search_path(matcher, entry.path(), &mut sink);
    if read.is_err() || sink.lines.is_empty() {
        return WalkState::Continue;
    }
    if sink.more {
        found.capped.store(true, Ordering::Relaxed);
    }
    let granted = found.claim(sink.lines.len());
    sink.lines.truncate(granted);
    let Some(last) = sink.lines.last().map(|l| l.line) else { return WalkState::Quit };
    // The lines round a match the cap cut off go with it.
    let reach = last.saturating_add(context);
    sink.context.retain(|c| c.line <= reach);
    let path = entry.path().strip_prefix(root).unwrap_or_else(|_| entry.path());
    let path = path.to_string_lossy().into_owned();
    let file = FileHits { path, lines: sink.lines, context: sink.context };
    if tx.send(file).is_err() || found.full() {
        return WalkState::Quit;
    }
    WalkState::Continue
}

/// One file's matching lines and the lines round them, gathered until it ends, the cap's share
/// is used up, or the search is stopped. A file found binary keeps none.
#[derive(Debug)]
struct Gathered<'a> {
    matcher: &'a RegexMatcher,
    cancel: &'a AtomicBool,
    /// Lines this file may add under the cap, as it stood when the file was started.
    left: u32,
    lines: Vec<LineHit>,
    context: Vec<ContextLine>,
    /// It had more matching lines than it was let take.
    more: bool,
}

impl Sink for Gathered<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.cancel.load(Ordering::Relaxed) {
            self.lines.clear();
            return Ok(false);
        }
        if u32::try_from(self.lines.len()).unwrap_or(u32::MAX) >= self.left {
            self.more = true;
            return Ok(false);
        }
        let number = mat.line_number().unwrap_or(0);
        self.lines.push(line_hit(number, mat.bytes(), self.matcher));
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let number = context.line_number().unwrap_or(0);
        self.context.push(context_line(number, context.bytes()));
        Ok(true)
    }

    fn binary_data(
        &mut self,
        _searcher: &Searcher,
        _binary_byte_offset: u64,
    ) -> Result<bool, Self::Error> {
        self.lines.clear();
        self.context.clear();
        self.more = false;
        Ok(false)
    }
}

/// A line's text without its end: the `\n`, and a `\r` before it.
fn content(raw: &[u8]) -> &[u8] {
    let line = raw.strip_suffix(b"\n").unwrap_or(raw);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Where `matcher` matches in `line`, as a hit shows them: the matches that are not empty, in
/// order, [`MAX_SPANS`] at most.
fn spans_in(line: &[u8], matcher: &RegexMatcher) -> Vec<(usize, usize)> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    let _searched = matcher.find_iter(line, |m| {
        if m.start() < m.end() {
            found.push((m.start(), m.end()));
        }
        found.len() < MAX_SPANS
    });
    found
}

/// How many spaces and tabs `line` starts with.
fn indent(line: &[u8]) -> usize {
    line.iter().take_while(|b| matches!(b, b' ' | b'\t')).count()
}

/// A matching line as the client shows it: its end and its indentation off, cut to
/// [`LINE_BYTES`] round its first match, the matches marked in what is left.
///
/// The cut never starts past the first match, so the line shown holds the match it is listed
/// for. Only matches past the end may go, and those come last.
fn line_hit(number: u64, raw: &[u8], matcher: &RegexMatcher) -> LineHit {
    let line = content(raw);
    let found = spans_in(line, matcher);
    let indent = indent(line);
    let first = found.first().map_or(indent, |(start, _)| *start);
    let mut start = boundary_before(line, indent.min(first));
    let mut end = line.len();
    if end.saturating_sub(start) > LINE_BYTES {
        start = boundary_before(line, first.saturating_sub(LEAD).max(indent).min(first));
        end = start.saturating_add(LINE_BYTES).min(line.len());
    }
    let end = boundary_before(line, end).max(start);
    let mut text = String::with_capacity(end.saturating_sub(start));
    let mut spans = Vec::with_capacity(found.len());
    let mut at = start;
    for (from, to) in found {
        let (from, to) = (from.clamp(start, end), to.clamp(start, end));
        if from >= to || from < at {
            continue;
        }
        push_shown(&mut text, line.get(at..from).unwrap_or_default());
        let span_start = text.len();
        push_shown(&mut text, line.get(from..to).unwrap_or_default());
        spans.push(Span { start: offset(span_start), end: offset(text.len()) });
        at = to;
    }
    push_shown(&mut text, line.get(at..end).unwrap_or_default());
    LineHit {
        line: u32::try_from(number).unwrap_or(u32::MAX),
        text,
        spans,
        cut_before: start > indent,
        cut_after: end < line.len(),
    }
}

/// A line round a match as the client shows it: as a hit is, its end and indentation off, cut
/// to [`LINE_BYTES`] from its start.
fn context_line(number: u64, raw: &[u8]) -> ContextLine {
    let line = content(raw);
    let start = indent(line);
    let end = boundary_before(line, start.saturating_add(LINE_BYTES).min(line.len())).max(start);
    let mut text = String::with_capacity(end.saturating_sub(start));
    push_shown(&mut text, line.get(start..end).unwrap_or_default());
    ContextLine {
        line: u32::try_from(number).unwrap_or(u32::MAX),
        text,
        cut_after: end < line.len(),
    }
}

/// `bytes` onto `text` as the client can draw it: bytes that are not UTF-8 replaced, and a tab
/// or another control character a space, byte for byte, so the spans still line up.
fn push_shown(text: &mut String, bytes: &[u8]) {
    let shown = String::from_utf8_lossy(bytes);
    text.extend(shown.chars().map(|c| if c.is_ascii_control() { ' ' } else { c }));
}

fn offset(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// Whether `byte` continues a UTF-8 character rather than starting one.
const fn continues(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// `at`, moved back onto the start of a character, so the text starts and ends on whole ones.
fn boundary_before(line: &[u8], mut at: usize) -> usize {
    while at > 0 && line.get(at).is_some_and(|b| continues(*b)) {
        at = at.saturating_sub(1);
    }
    at
}

/// What the pager handed on.
#[derive(Debug, Default)]
struct Paged {
    files: u32,
    lines: u32,
}

/// The text a file adds to a page: its matching lines and the lines round them.
fn text_bytes(file: &FileHits) -> usize {
    let lines = file.lines.iter().map(|l| l.text.len()).sum::<usize>();
    lines.saturating_add(file.context.iter().map(|c| c.text.len()).sum::<usize>())
}

/// Gather the files the walk finds into pages and hand each to `emit` once it holds
/// [`PAGE_BYTES`] of text or its first file has waited [`FLUSH`]. Ends when the walk does; a
/// refused page stops the search, and the rest is drained unsent.
fn page_out(
    rx: &Receiver<FileHits>,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Vec<FileHits>) -> bool,
) -> Paged {
    let mut paged = Paged::default();
    let mut page: Vec<FileHits> = Vec::new();
    let mut bytes = 0_usize;
    let mut since: Option<Instant> = None;
    let mut flush = |page: &mut Vec<FileHits>, paged: &mut Paged| {
        if page.is_empty() || cancel.load(Ordering::Relaxed) {
            page.clear();
            return;
        }
        let files = u32::try_from(page.len()).unwrap_or(u32::MAX);
        let lines = page.iter().map(|f| f.lines.len()).sum::<usize>();
        if emit(std::mem::take(page)) {
            paged.files = paged.files.saturating_add(files);
            paged.lines = paged.lines.saturating_add(u32::try_from(lines).unwrap_or(u32::MAX));
        } else {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    loop {
        let next = match since {
            None => rx.recv().map_err(|_closed| RecvTimeoutError::Disconnected),
            Some(first) => rx.recv_timeout(FLUSH.saturating_sub(first.elapsed())),
        };
        match next {
            Ok(file) => {
                bytes = bytes.saturating_add(text_bytes(&file));
                page.push(file);
                since.get_or_insert_with(Instant::now);
                if bytes >= PAGE_BYTES {
                    flush(&mut page, &mut paged);
                    (bytes, since) = (0, None);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                flush(&mut page, &mut paged);
                (bytes, since) = (0, None);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    flush(&mut page, &mut paged);
    paged
}

/// The one search a connection runs: a new start stops the last, and so does dropping it
/// with the connection.
#[derive(Debug, Default)]
pub struct Searches {
    current: Option<(RequestId, Arc<AtomicBool>)>,
}

impl Searches {
    /// Start or stop a search for the connection whose messages go out on `out`. A search runs
    /// on the blocking pool; its pages go out as they fill, then `Done` or `Failed`, unless it
    /// was stopped first.
    pub fn handle(&mut self, request: SearchRequest, out: &tokio::sync::mpsc::Sender<WorkerMsg>) {
        match request {
            SearchRequest::Start { id, root, query } => {
                self.stop();
                let cancel = Arc::new(AtomicBool::new(false));
                self.current = Some((id, Arc::clone(&cancel)));
                let out = out.clone();
                let _running = tokio::task::spawn_blocking(move || {
                    run(id, &root, &query, &cancel, &out);
                });
            }
            SearchRequest::Stop { id } => {
                if self.current.as_ref().is_some_and(|(current, _)| *current == id) {
                    self.stop();
                }
            }
        }
    }

    /// The search in progress, if any, and whether it has been told to stop.
    #[must_use]
    pub fn current(&self) -> Option<(RequestId, bool)> {
        self.current.as_ref().map(|(id, cancel)| (*id, cancel.load(Ordering::Relaxed)))
    }

    fn stop(&mut self) {
        if let Some((_, cancel)) = self.current.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Searches {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Search `id` on this thread, its pages and its end sent on `out` as they come.
fn run(
    id: RequestId,
    root: &str,
    query: &SearchQuery,
    cancel: &AtomicBool,
    out: &tokio::sync::mpsc::Sender<WorkerMsg>,
) {
    let dir = crate::file::expand_home(Path::new(root));
    let mut emit = |files: Vec<FileHits>| {
        !cancel.load(Ordering::Relaxed)
            && out.blocking_send(WorkerMsg::Search(SearchEvent::Hits { id, files })).is_ok()
    };
    let result = search(&dir, query, MAX_LINES, cancel, &mut emit);
    if cancel.load(Ordering::Relaxed) {
        tracing::debug!(id, root, "search stopped");
        return;
    }
    let event = match result {
        Ok(summary) => {
            tracing::info!(
                id,
                root,
                files = summary.files,
                lines = summary.lines,
                searched = summary.searched,
                capped = summary.capped,
                ms = summary.elapsed_ms,
                "search done"
            );
            SearchEvent::Done { id, summary }
        }
        Err(error) => SearchEvent::Failed { id, error },
    };
    let _sent = out.blocking_send(WorkerMsg::Search(event));
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn query(pattern: &str) -> SearchQuery {
        SearchQuery { pattern: pattern.to_owned(), ..SearchQuery::default() }
    }

    /// Every file `query` finds under `root`, sorted by path, and the summary.
    fn found(root: &Path, query: &SearchQuery) -> (Vec<FileHits>, SearchSummary) {
        let mut files = Vec::new();
        let cancel = AtomicBool::new(false);
        let summary = search(root, query, MAX_LINES, &cancel, &mut |page| {
            files.extend(page);
            true
        })
        .unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        (files, summary)
    }

    fn paths(files: &[FileHits]) -> Vec<&str> {
        files.iter().map(|f| f.path.as_str()).collect()
    }

    /// What `.gitignore` excludes is not searched, outside a repository too, nor a version
    /// control store; a hidden file is. A binary file with the word in it reports nothing,
    /// even where the word comes before its first NUL byte.
    #[test]
    fn ignored_files_stores_and_binary_files_are_skipped_and_hidden_ones_searched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::write(root.join(".gitignore"), "target\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {\n    needle();\n}\n").unwrap();
        fs::write(root.join("target/out.rs"), "needle\n").unwrap();
        fs::write(root.join(".git/config"), "needle\n").unwrap();
        fs::write(root.join(".github/ci.yml"), "run: needle\n").unwrap();
        fs::write(root.join("blob.bin"), b"needle\n\0\0\x01needle").unwrap();

        let (files, summary) = found(root, &query("needle"));
        assert_eq!(paths(&files), [".github/ci.yml", "src/main.rs"], "{files:#?}");
        let main = files.iter().find(|f| f.path == "src/main.rs").unwrap();
        let hit = main.lines.first().unwrap();
        assert_eq!((hit.line, hit.text.as_str()), (2, "needle();"), "indentation is dropped");
        assert_eq!(hit.spans, [Span { start: 0, end: 6 }]);
        assert_eq!((summary.files, summary.lines, summary.capped), (2, 2, false));
        assert!(summary.searched >= 3, "the binary file was read: {summary:?}");
    }

    /// Case, whole words and regular expressions are the query's toggles; globs narrow and
    /// exclude as ripgrep's do; a pattern that does not parse is said so.
    #[test]
    fn the_toggles_and_the_globs_decide_what_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("a.rs"), "Foo foo foobar\n").unwrap();
        fs::write(root.join("b.md"), "foo\n").unwrap();
        fs::write(root.join("tests/c.rs"), "foo\n").unwrap();

        let spans = |q: &SearchQuery| {
            let (files, _) = found(root, q);
            let a = files.into_iter().find(|f| f.path == "a.rs").unwrap();
            let line = a.lines.into_iter().next().unwrap();
            line.spans.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>()
        };
        assert_eq!(spans(&query("foo")), [(0, 3), (4, 7), (8, 11)], "any case by default");
        let cased = SearchQuery { match_case: true, ..query("Foo") };
        assert_eq!(spans(&cased), [(0, 3)]);
        let word = SearchQuery { whole_word: true, ..query("foo") };
        assert_eq!(spans(&word), [(0, 3), (4, 7)]);
        let regex = SearchQuery { regex: true, ..query(r"fo+b\w+") };
        assert_eq!(spans(&regex), [(8, 14)]);
        let literal = query("o+");
        assert!(found(root, &literal).0.is_empty(), "a literal's `+` is a plus sign");

        let globbed =
            SearchQuery { globs: vec!["*.rs".into(), "!tests/**".into()], ..query("foo") };
        assert_eq!(paths(&found(root, &globbed).0), ["a.rs"]);

        let cancel = AtomicBool::new(false);
        let bad = SearchQuery { regex: true, ..query("(unclosed") };
        let error = search(root, &bad, MAX_LINES, &cancel, &mut |_| true).unwrap_err();
        assert!(error.contains("unclosed"), "{error}");
        let error = search(&root.join("nowhere"), &query("x"), MAX_LINES, &cancel, &mut |_| true);
        assert!(error.unwrap_err().starts_with("No folder at"));
    }

    /// Past [`MAX_LINES`] matching lines the search stops, sends exactly that many and says it
    /// was capped; the pages it sent in the meantime each stay under the page size.
    #[test]
    fn a_search_stops_at_the_cap_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let per_file = 300_usize;
        for n in 0..10 {
            let text = "hit and some text to pad the page\n".repeat(per_file);
            fs::write(dir.path().join(format!("f{n}.txt")), text).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let mut pages = Vec::new();
        let summary = search(dir.path(), &query("hit"), MAX_LINES, &cancel, &mut |page| {
            pages.push(page);
            true
        })
        .unwrap();
        let lines: usize = pages.iter().flatten().map(|f| f.lines.len()).sum();
        assert_eq!(lines, usize::try_from(MAX_LINES).unwrap());
        assert_eq!(summary.lines, MAX_LINES);
        assert!(summary.capped, "{summary:?}");
        for page in &pages {
            let bytes: usize = page.iter().flat_map(|f| &f.lines).map(|l| l.text.len()).sum();
            assert!(bytes < PAGE_BYTES.saturating_add(per_file.saturating_mul(40)), "{bytes}");
        }
    }

    /// A page the caller refuses stops the search where it is, and a search told to stop
    /// before it starts reads nothing. Each file's line fills a hit, so the first page is
    /// sent on its size well before the walk is through.
    #[test]
    fn a_refused_page_or_a_stop_ends_the_search() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("hit {}\n", "x".repeat(LINE_BYTES));
        for n in 0..400 {
            fs::write(dir.path().join(format!("f{n}.txt")), &line).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let mut pages = 0_u32;
        let summary = search(dir.path(), &query("hit"), MAX_LINES, &cancel, &mut |_| {
            pages = pages.saturating_add(1);
            false
        })
        .unwrap();
        assert_eq!(pages, 1, "nothing is sent after a refusal");
        assert!(cancel.load(Ordering::Relaxed));
        assert!(summary.searched < 400, "the walk stopped early: {summary:?}");

        let stopped = AtomicBool::new(true);
        let mut sent = false;
        let summary = search(dir.path(), &query("hit"), MAX_LINES, &stopped, &mut |_| {
            sent = true;
            true
        })
        .unwrap();
        assert!(!sent);
        assert_eq!(summary.searched, 0);
    }

    /// A long line is cut round its first match on characters' boundaries, says so at both
    /// ends, and its spans still point at the match; a tab reads as a space.
    #[test]
    fn a_long_line_is_cut_round_its_match() {
        let matcher = matcher(&query("needle")).unwrap();
        let line = format!("{}\tneedle{}\n", "é".repeat(200), "x".repeat(400));
        let hit = line_hit(9, line.as_bytes(), &matcher);
        assert!(hit.cut_before && hit.cut_after, "{hit:?}");
        assert!(hit.text.len() <= LINE_BYTES, "{}", hit.text.len());
        let span = hit.spans.first().unwrap();
        let start = usize::try_from(span.start).unwrap();
        let end = usize::try_from(span.end).unwrap();
        assert_eq!(hit.text.get(start..end), Some("needle"));
        assert!(hit.text.get(..start).unwrap().ends_with(' '), "the tab is a space");

        let short = line_hit(1, b"  \tx needle y\r\n", &matcher);
        assert_eq!(short.text, "x needle y");
        assert!(!short.cut_before && !short.cut_after);
        let invalid = line_hit(1, b"\xff needle", &matcher);
        assert_eq!(invalid.text, "\u{fffd} needle");
        assert_eq!(invalid.spans, [Span { start: 4, end: 10 }], "offsets are the text's");
    }

    /// A new search stops the one in progress, a stop for another search leaves it running,
    /// and dropping the connection's searches stops the last.
    #[tokio::test]
    async fn a_new_search_stops_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let (out, _rx) = tokio::sync::mpsc::channel(4);
        let mut searches = Searches::default();
        let start = |id| SearchRequest::Start { id, root: root.clone(), query: query("x") };
        searches.handle(start(1), &out);
        let first = Arc::clone(&searches.current.as_ref().unwrap().1);
        assert_eq!(searches.current(), Some((1, false)));
        searches.handle(start(2), &out);
        assert!(first.load(Ordering::Relaxed), "the first was told to stop");
        assert_eq!(searches.current(), Some((2, false)));
        searches.handle(SearchRequest::Stop { id: 1 }, &out);
        assert_eq!(searches.current(), Some((2, false)), "a stale stop changes nothing");
        let second = Arc::clone(&searches.current.as_ref().unwrap().1);
        drop(searches);
        assert!(second.load(Ordering::Relaxed), "the connection's end stops it");
    }
}
