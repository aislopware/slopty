//! Project-wide text search on a worker: a query in, the matching lines streamed back as the
//! worker finds them.
//!
//! The client numbers each search ([`SearchRequest::Start`]); a connection runs one at a time,
//! so a new start stops the last, and [`SearchRequest::Stop`] stops it with nothing new. The
//! worker answers on the control stream in pages ([`SearchEvent::Hits`], each under
//! [`PAGE_BYTES`] of text so a page never holds another message back for long), then once with
//! how it ended ([`SearchEvent::Done`] or [`SearchEvent::Failed`]). A stopped search says
//! nothing more. Every event names its search, so a page still in flight for a search the
//! client has moved on from is told from the current one.
//!
//! A replace ([`SearchRequest::Replace`]) names the matches a search showed, file by file, with
//! the [`FileStamp`] each file had when it was searched. The worker rewrites each file whole,
//! refuses one that changed since, and answers once ([`SearchEvent::Replaced`]).

use serde::{Deserialize, Serialize};

use crate::RequestId;

/// Matching lines a search reports at most; past them it stops and says it was capped.
///
/// Enough to show everything a query is about in a project, bounded so a search for `e` in a
/// home directory is a few hundred kilobytes on the wire rather than the disk.
pub const MAX_LINES: u32 = 2_000;

/// How much of a matching line a hit carries at most, in bytes: a minified bundle is one line
/// megabytes long. A longer line is cut round its first match ([`LineHit::cut_before`]).
pub const LINE_BYTES: usize = 240;

/// The text a page of hits carries before it is sent, in bytes.
pub const PAGE_BYTES: usize = 32 * 1024;

/// Lines of context a search gives round each match at most, before and after.
pub const MAX_CONTEXT: u32 = 5;

/// What to look for.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    /// The text typed: a literal unless [`Self::regex`].
    pub pattern: String,
    /// The pattern is a regular expression (Rust's `regex` syntax, as ripgrep takes it).
    pub regex: bool,
    /// Upper and lower case differ; otherwise they match each other.
    pub match_case: bool,
    /// A match must stand as a whole word.
    pub whole_word: bool,
    /// Which files to look in, as ripgrep's `--glob` takes them, relative to the root: a file
    /// must match one of the plain globs when there is any, and none of the `!` ones. Empty
    /// looks everywhere `.gitignore` allows.
    pub globs: Vec<String>,
    /// Lines shown before and after each matching line ([`FileHits::context`]), up to
    /// [`MAX_CONTEXT`]; none at 0.
    pub context: u32,
}

/// Client → worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SearchRequest {
    /// Search the files under `root`, stopping this connection's search in progress.
    Start {
        /// The client's number for it, which every answer carries.
        id: RequestId,
        /// An absolute directory on the worker, or `~/…` in its home.
        root: String,
        /// What to look for.
        query: SearchQuery,
    },
    /// Stop search `id` if it is still going: the query changed to nothing, or the surface
    /// showing it closed.
    Stop {
        /// Which one.
        id: RequestId,
    },
    /// Replace matches a search found, each file rewritten whole; answered with
    /// [`SearchEvent::Replaced`]. It runs beside a search rather than stopping it.
    Replace(Replace),
}

/// Matches to replace, as a search reported them.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Replace {
    /// The client's number for it, which the answer carries.
    pub id: RequestId,
    /// The search's root: an absolute directory on the worker, or `~/…` in its home.
    pub root: String,
    /// The search's query, which finds the matches again in each file.
    pub query: SearchQuery,
    /// What each match becomes. With [`SearchQuery::regex`], `$1` and `${name}` stand for the
    /// match's groups and `$$` for a dollar sign; otherwise it is the text as it is.
    pub with: String,
    /// The files, each with the matches in it to replace.
    pub files: Vec<FileReplace>,
}

/// One file's matches to replace.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileReplace {
    /// Relative to the root, as [`FileHits::path`] said it.
    pub path: String,
    /// The file as it was searched: a file whose stamp differs is left alone.
    pub stamp: FileStamp,
    /// Which matches, each once.
    pub matches: Vec<MatchAt>,
}

/// A match by where a search reported it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct MatchAt {
    /// Its line, from 1.
    pub line: u32,
    /// Which match on the line, from 0: the same as its place in [`LineHit::spans`].
    pub index: u32,
}

/// Worker → client, about one search.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SearchEvent {
    /// Files with matches found since the last page, in the order they were found. A file
    /// comes whole in one page.
    Hits {
        /// The search.
        id: RequestId,
        /// The files.
        files: Vec<FileHits>,
    },
    /// The search went through everything, or stopped at [`MAX_LINES`].
    Done {
        /// The search.
        id: RequestId,
        /// What it found.
        summary: SearchSummary,
    },
    /// The search could not start: a pattern or a glob that does not parse, a root that is
    /// not a directory. For a replace: its query does not parse.
    Failed {
        /// The search, or the replace.
        id: RequestId,
        /// For a person to read.
        error: String,
    },
    /// A replace went through its files.
    Replaced {
        /// The replace.
        id: RequestId,
        /// The files rewritten, in the order they were asked for.
        files: Vec<FileReplaced>,
        /// The files left alone, and why.
        skipped: Vec<Skipped>,
    },
}

impl SearchEvent {
    /// The search the event is about.
    #[must_use]
    pub const fn id(&self) -> RequestId {
        match self {
            Self::Hits { id, .. }
            | Self::Done { id, .. }
            | Self::Failed { id, .. }
            | Self::Replaced { id, .. } => *id,
        }
    }
}

/// A file rewritten by a replace.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileReplaced {
    /// Relative to the root.
    pub path: String,
    /// How many matches it replaced.
    pub matches: u32,
    /// The file as it is now, for a later replace of its other matches.
    pub stamp: FileStamp,
}

/// A file a replace left alone.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Skipped {
    /// Relative to the root.
    pub path: String,
    /// Why.
    pub why: SkipReason,
}

/// Why a replace left a file alone.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SkipReason {
    /// It changed since it was searched, or is gone: its matches may be elsewhere now.
    Changed,
    /// It could not be read or written: the OS's word, for a person.
    Failed(String),
}

/// What a file was when it was searched: its size and its modification time, which any write
/// moves.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct FileStamp {
    /// Bytes.
    pub size: u64,
    /// Nanoseconds since the Unix epoch, as the file system keeps it.
    pub modified_ns: u64,
}

/// One file's matching lines.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileHits {
    /// Relative to the search's root, `/` between its parts.
    pub path: String,
    /// The file as it was searched, for a replace to check against.
    pub stamp: FileStamp,
    /// Its matching lines, top down.
    pub lines: Vec<LineHit>,
    /// The lines round them ([`SearchQuery::context`]), top down: each line once, never one
    /// of [`Self::lines`], so the context of two matches close together runs into one.
    pub context: Vec<ContextLine>,
}

/// A line near a match, shown round it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ContextLine {
    /// Its number in the file, from 1.
    pub line: u32,
    /// The line, without its end and its indentation, cut to [`LINE_BYTES`]; bytes that are
    /// not UTF-8 replaced.
    pub text: String,
    /// Text of the line after [`Self::text`] was cut off.
    pub cut_after: bool,
}

/// One matching line.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LineHit {
    /// Its number in the file, from 1.
    pub line: u32,
    /// The line, without its end and its indentation, cut to [`LINE_BYTES`] round its first
    /// match; bytes that are not UTF-8 replaced.
    pub text: String,
    /// Where the matches are in [`Self::text`], in order, never overlapping.
    pub spans: Vec<Span>,
    /// Text of the line before [`Self::text`] was cut off (indentation aside).
    pub cut_before: bool,
    /// Text of the line after [`Self::text`] was cut off.
    pub cut_after: bool,
}

/// A match in a [`LineHit`]'s text: byte offsets, each on a character's boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Span {
    /// Where it starts.
    pub start: u32,
    /// Where it ends, past its last byte.
    pub end: u32,
}

/// How a search ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SearchSummary {
    /// Files with a match.
    pub files: u32,
    /// Matching lines reported.
    pub lines: u32,
    /// Files searched: what the walk reached and `.gitignore` let through, binary ones included.
    pub searched: u32,
    /// It stopped at [`MAX_LINES`]; there were more.
    pub capped: bool,
    /// How long it took on the worker, in milliseconds.
    pub elapsed_ms: u32,
}
