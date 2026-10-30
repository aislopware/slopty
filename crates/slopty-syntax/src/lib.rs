// Derived from zeron (MIT, github.com/zeronsh/zeron @ 187b402), see THIRD_PARTY_NOTICES.md
//! Syntax-highlighting contracts shared by Slopty's desktop surfaces.
//!
//! This crate intentionally has no UI, RPC, or engine dependencies. Public
//! ranges are byte offsets relative to one UTF-8 source line.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::AtomicUsize;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// The largest source highlighted by default: 1 MiB.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// The most spans one document yields by default.
pub const DEFAULT_MAX_SPANS: usize = 200_000;

/// What one highlight may cost before it gives up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightLimits {
    /// A longer source is refused with [`HighlightError::SourceTooLarge`].
    pub max_source_bytes: usize,
    /// Past this many spans the highlight stops with [`HighlightError::TooManySpans`].
    pub max_spans: usize,
}

impl Default for HighlightLimits {
    fn default() -> Self {
        Self { max_source_bytes: DEFAULT_MAX_SOURCE_BYTES, max_spans: DEFAULT_MAX_SPANS }
    }
}

/// A language this crate highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    /// Rust (`.rs`).
    Rust,
    /// JavaScript (`.js`, `.mjs`, `.cjs`).
    JavaScript,
    /// JavaScript with JSX (`.jsx`).
    Jsx,
    /// TypeScript (`.ts`, `.mts`, `.cts`).
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    /// Python (`.py`).
    Python,
    /// Go (`.go`).
    Go,
    /// JSON (`.json`).
    Json,
    /// JSON with comments (`.jsonc`).
    Jsonc,
    /// Bash and the POSIX shells (`.sh`, `.bash`, `.zsh`).
    Bash,
    /// TOML (`.toml`).
    Toml,
    /// Markdown (`.md`), its fenced code highlighted in the fence's language.
    Markdown,
    /// HTML (`.html`), its scripts and styles highlighted too.
    Html,
    /// CSS (`.css`).
    Css,
    /// YAML (`.yaml`, `.yml`).
    Yaml,
    /// C (`.c`).
    C,
    /// C++ (`.cpp`, `.cc`, `.hpp`).
    Cpp,
    /// C# (`.cs`).
    CSharp,
    /// Java (`.java`).
    Java,
    /// Kotlin (`.kt`, `.kts`).
    Kotlin,
    /// Swift (`.swift`).
    Swift,
    /// Ruby (`.rb`).
    Ruby,
    /// PHP (`.php`).
    Php,
    /// SQL (`.sql`).
    Sql,
    /// Lua (`.lua`).
    Lua,
    /// Luau (`.luau`), the Lua dialect agent visuals are written in.
    Luau,
    /// Dockerfile and Containerfile.
    Dockerfile,
    /// Nix (`.nix`).
    Nix,
    /// Makefiles.
    Make,
}

/// What a highlighted span is, as the theme colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    /// Comments.
    Comment,
    /// Keywords.
    Keyword,
    /// String literals.
    String,
    /// Special strings: regexes, symbols, paths.
    StringSpecial,
    /// Escape sequences inside strings.
    Escape,
    /// Numeric literals.
    Number,
    /// Boolean literals.
    Boolean,
    /// Type names.
    Type,
    /// The language's built-in types.
    TypeBuiltin,
    /// Constructors and enum variants.
    Constructor,
    /// Function names.
    Function,
    /// The language's built-in functions.
    FunctionBuiltin,
    /// Macros.
    Macro,
    /// Fields and properties.
    Property,
    /// Constants.
    Constant,
    /// Variables.
    Variable,
    /// Built-in variables: `self`, `this`, `super`.
    VariableSpecial,
    /// Parameters.
    Parameter,
    /// Operators.
    Operator,
    /// Brackets, delimiters and other punctuation.
    Punctuation,
    /// Markup tags.
    Tag,
    /// Attributes and annotations.
    Attribute,
    /// Labels.
    Label,
    /// Markdown headings.
    MarkupHeading,
    /// Markdown code spans and blocks.
    MarkupRaw,
    /// Markdown link destinations.
    MarkupLink,
    /// Markdown link text and references.
    MarkupReference,
    /// Markdown emphasis.
    MarkupEmphasis,
    /// Markdown strong emphasis.
    MarkupStrong,
    /// Code embedded in another language.
    Embedded,
    /// Text the grammar could not parse.
    Invalid,
}

impl HighlightKind {
    /// Stable precedence used to resolve overlapping parser captures.
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Invalid => 100,
            Self::Escape => 95,
            Self::Macro => 90,
            Self::Property | Self::Attribute => 85,
            Self::FunctionBuiltin | Self::TypeBuiltin | Self::VariableSpecial => 80,
            Self::StringSpecial | Self::Constructor | Self::Parameter => 75,
            Self::Function | Self::Type | Self::Constant | Self::Tag | Self::Label => 70,
            Self::Comment | Self::Keyword | Self::String | Self::Number | Self::Boolean => 60,
            Self::Variable | Self::Operator => 50,
            Self::Punctuation | Self::Embedded => 40,
            // Markdown block captures can wrap more specific inline captures,
            // and fenced-code captures can wrap an injected language. Keep
            // markup below programming-language tokens while preserving the
            // nesting order among Markdown roles.
            Self::MarkupHeading => 30,
            Self::MarkupEmphasis => 31,
            Self::MarkupStrong => 32,
            Self::MarkupLink | Self::MarkupReference => 33,
            Self::MarkupRaw => 34,
        }
    }
}

/// One highlighted run of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Byte offsets within the line (absolute in the source before
    /// [`HighlightedDocument::from_absolute_spans`] splits it).
    pub range: Range<usize>,
    /// What the run is.
    pub kind: HighlightKind,
}

/// A highlighted source, line by line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedDocument {
    /// The language it was highlighted as.
    pub language: LanguageId,
    /// Each source line's spans, ordered and disjoint; a line with nothing highlighted is empty.
    pub lines: Vec<Vec<HighlightSpan>>,
}

/// A source to highlight and what names its language.
#[derive(Debug, Clone, Copy)]
pub struct HighlightRequest<'a> {
    /// The whole source.
    pub source: &'a str,
    /// The file's path, whose name or extension names the language.
    pub path: Option<&'a str>,
    /// A Markdown fence's info string, which names the language ahead of the path.
    pub fence_tag: Option<&'a str>,
}

/// Why a source was not highlighted.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum HighlightError {
    /// Neither the fence, the path nor a shebang names a language this crate knows.
    #[error("the source language is not registered")]
    UnknownLanguage,
    /// A span runs backwards or past the source's end.
    #[error("highlight range {start}..{end} is invalid for a {len}-byte source")]
    InvalidRange {
        /// The span's first byte.
        start: usize,
        /// The byte past the span's last.
        end: usize,
        /// The source's length in bytes.
        len: usize,
    },
    /// A span starts or ends inside a UTF-8 character.
    #[error("highlight range {start}..{end} is not on UTF-8 boundaries")]
    InvalidUtf8Boundary {
        /// The span's first byte.
        start: usize,
        /// The byte past the span's last.
        end: usize,
    },
    /// The source is longer than [`HighlightLimits::max_source_bytes`].
    #[error("source exceeds the configured highlighting limit")]
    SourceTooLarge,
    /// The source yields more than [`HighlightLimits::max_spans`] spans.
    #[error("highlight output exceeds the configured span limit")]
    TooManySpans,
    /// tree-sitter refused the grammar's queries or stopped mid-parse (cancelled, or timed out).
    #[error("parser failed: {0}")]
    Parser(String),
    /// The language is known but its grammar is not in this build.
    #[error("the {0:?} grammar is not bundled")]
    GrammarUnavailable(LanguageId),
}

impl HighlightedDocument {
    /// Validate, split, and normalize absolute source spans into line-relative spans.
    pub fn from_absolute_spans(
        language: LanguageId,
        source: &str,
        spans: impl IntoIterator<Item = HighlightSpan>,
    ) -> Result<Self, HighlightError> {
        let starts = line_starts(source);
        let mut lines = vec![Vec::new(); starts.len()];
        for span in spans {
            validate_span(source, &span.range)?;
            if span.range.is_empty() {
                continue;
            }
            // `starts[0]` is 0, so the line holding the span's start is the last one at or
            // before it.
            let first_line =
                starts.partition_point(|&start| start <= span.range.start).saturating_sub(1);
            for (line_ix, &start) in starts.iter().enumerate().skip(first_line) {
                let raw_end =
                    starts.get(line_ix.saturating_add(1)).copied().unwrap_or(source.len());
                let mut end = raw_end;
                if source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\n') {
                    end = end.saturating_sub(1);
                    if source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
                        end = end.saturating_sub(1);
                    }
                }
                let segment_start = span.range.start.max(start);
                let segment_end = span.range.end.min(end);
                if segment_start < segment_end
                    && let Some(line) = lines.get_mut(line_ix)
                {
                    line.push(HighlightSpan {
                        range: segment_start.saturating_sub(start)
                            ..segment_end.saturating_sub(start),
                        kind: span.kind,
                    });
                }
                if raw_end >= span.range.end {
                    break;
                }
            }
        }
        for line in &mut lines {
            *line = normalize_line(line);
        }
        Ok(Self { language, lines })
    }
}

const fn validate_span(source: &str, range: &Range<usize>) -> Result<(), HighlightError> {
    if range.start > range.end || range.end > source.len() {
        return Err(HighlightError::InvalidRange {
            start: range.start,
            end: range.end,
            len: source.len(),
        });
    }
    if !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return Err(HighlightError::InvalidUtf8Boundary { start: range.start, end: range.end });
    }
    Ok(())
}

fn normalize_line(spans: &[HighlightSpan]) -> Vec<HighlightSpan> {
    /// A span's start or end: where it is, which span (by precedence, then index), and its kind.
    struct Edge {
        offset: usize,
        start: bool,
        rank: (u8, usize),
        kind: HighlightKind,
    }

    let mut edges = spans
        .iter()
        .enumerate()
        .flat_map(|(index, span)| {
            let rank = (span.kind.precedence(), index);
            [
                Edge { offset: span.range.start, start: true, rank, kind: span.kind },
                Edge { offset: span.range.end, start: false, rank, kind: span.kind },
            ]
        })
        .collect::<Vec<_>>();
    edges.sort_unstable_by_key(|edge| edge.offset);

    // The span index is the tie-breaker so equal-precedence overlaps retain
    // the old `Iterator::max_by_key` behavior (the later span wins).
    let mut active = BTreeMap::new();
    let mut normalized: Vec<HighlightSpan> = Vec::new();
    let mut groups = edges.chunk_by(|a, b| a.offset == b.offset).peekable();
    while let Some(group) = groups.next() {
        for edge in group.iter().filter(|edge| !edge.start) {
            active.remove(&edge.rank);
        }
        for edge in group.iter().filter(|edge| edge.start) {
            active.insert(edge.rank, edge.kind);
        }
        let (Some(here), Some(next)) = (group.first(), groups.peek().and_then(|g| g.first()))
        else {
            break;
        };
        let (offset, next_offset) = (here.offset, next.offset);
        if let Some((_, &kind)) = active.last_key_value() {
            if let Some(previous) = normalized.last_mut()
                && previous.kind == kind
                && previous.range.end == offset
            {
                previous.range.end = next_offset;
            } else {
                normalized.push(HighlightSpan { range: offset..next_offset, kind });
            }
        }
    }
    normalized
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .match_indices('\n')
            .map(|(index, _)| index.saturating_add(1))
            .filter(|start| *start < source.len()),
    );
    starts
}

/// Whether this build contains a parser and compatible highlight queries.
pub const fn supports_language(_language: LanguageId) -> bool {
    true
}

/// Highlight a complete document with the default resource limits.
pub fn highlight(request: HighlightRequest<'_>) -> Result<HighlightedDocument, HighlightError> {
    highlight_with_limits(request, HighlightLimits::default(), None)
}

/// Highlight a complete document with explicit limits and cooperative cancellation.
pub fn highlight_with_limits(
    request: HighlightRequest<'_>,
    limits: HighlightLimits,
    cancellation_flag: Option<&AtomicUsize>,
) -> Result<HighlightedDocument, HighlightError> {
    if request.source.len() > limits.max_source_bytes {
        return Err(HighlightError::SourceTooLarge);
    }
    let language = detect_language(request.path, request.fence_tag, request.source.lines().next())
        .ok_or(HighlightError::UnknownLanguage)?;
    if !supports_language(language) {
        return Err(HighlightError::GrammarUnavailable(language));
    }

    let primary_configuration = cached_configuration(language)?;
    let injected = injected_languages(language);
    let markdown_inline = if language == LanguageId::Markdown {
        Some(cached_markdown_inline_configuration()?)
    } else {
        None
    };
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(
            primary_configuration,
            request.source.as_bytes(),
            None,
            cancellation_flag,
            |name| {
                if name == "markdown_inline" {
                    return markdown_inline;
                }
                let language = language_for_alias(name)?;
                if !injected.contains(&language) {
                    return None;
                }
                cached_configuration(language).ok()
            },
        )
        .map_err(|error| HighlightError::Parser(error.to_string()))?;

    // Each open capture's kind, `None` for one outside `CAPTURE_KINDS` (it still closes).
    let mut active: Vec<Option<HighlightKind>> = Vec::new();
    let mut spans = Vec::new();
    for event in events {
        match event.map_err(|error| HighlightError::Parser(error.to_string()))? {
            HighlightEvent::HighlightStart(highlight) => {
                active.push(CAPTURE_KINDS.get(highlight.0).copied());
            }
            HighlightEvent::HighlightEnd => {
                active.pop();
            }
            HighlightEvent::Source { start, end } => {
                if let Some(kind) =
                    active.iter().flatten().copied().max_by_key(|kind| kind.precedence())
                {
                    spans.push(HighlightSpan { range: start..end, kind });
                    if spans.len() > limits.max_spans {
                        return Err(HighlightError::TooManySpans);
                    }
                }
            }
        }
    }
    HighlightedDocument::from_absolute_spans(language, request.source, spans)
}

fn injected_languages(parent: LanguageId) -> Vec<LanguageId> {
    use LanguageId as L;
    match parent {
        L::Html => vec![L::JavaScript, L::Css, L::Json],
        L::Markdown => vec![
            L::Rust,
            L::JavaScript,
            L::Jsx,
            L::TypeScript,
            L::Tsx,
            L::Python,
            L::Go,
            L::Json,
            L::Jsonc,
            L::Bash,
            L::Toml,
            L::Html,
            L::Css,
            L::Yaml,
            L::C,
            L::Cpp,
            L::CSharp,
            L::Java,
            L::Kotlin,
            L::Swift,
            L::Ruby,
            L::Php,
            L::Sql,
            L::Lua,
            L::Luau,
            L::Dockerfile,
            L::Nix,
            L::Make,
        ],
        L::Dockerfile => vec![L::Bash, L::Json, L::Yaml, L::Toml],
        _ => Vec::new(),
    }
}

/// Compiled queries contain no document/theme state and are immutable after
/// capture configuration. Keep one per used grammar instead of recompiling
/// for every fence or eagerly compiling all 28 Markdown injection targets.
/// Each grammar has its own cell: concurrent requests compile it only once,
/// while unrelated languages never wait on a global compilation lock.
fn cached_configuration(
    language: LanguageId,
) -> Result<&'static HighlightConfiguration, HighlightError> {
    macro_rules! registry {
        ($($variant:ident),+ $(,)?) => {
            match language {
                $(LanguageId::$variant => {
                    static CONFIG: std::sync::OnceLock<Result<HighlightConfiguration, HighlightError>> =
                        std::sync::OnceLock::new();
                    CONFIG.get_or_init(|| {
                        let mut config = configuration(language)?;
                        config.configure(CAPTURE_NAMES);
                        Ok(config)
                    }).as_ref().map_err(Clone::clone)
                }),+
            }
        };
    }
    registry!(
        Rust, JavaScript, Jsx, TypeScript, Tsx, Python, Go, Json, Jsonc, Bash, Toml, Markdown,
        Html, Css, Yaml, C, Cpp, CSharp, Java, Kotlin, Swift, Ruby, Php, Sql, Lua, Luau,
        Dockerfile, Nix, Make,
    )
}

fn cached_markdown_inline_configuration() -> Result<&'static HighlightConfiguration, HighlightError>
{
    static CONFIG: std::sync::OnceLock<Result<HighlightConfiguration, HighlightError>> =
        std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut config = markdown_inline_configuration()?;
            config.configure(CAPTURE_NAMES);
            Ok(config)
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn rust_configuration() -> Result<HighlightConfiguration, HighlightError> {
    // The upstream Rust query groups numbers and booleans as
    // `constant.builtin`. Slopty preserves those structural roles separately.
    let highlights = tree_sitter_rust::HIGHLIGHTS_QUERY
        .replace("(boolean_literal) @constant.builtin", "(boolean_literal) @boolean")
        .replace("(integer_literal) @constant.builtin", "(integer_literal) @number")
        .replace("(float_literal) @constant.builtin", "(float_literal) @number");
    HighlightConfiguration::new(
        tree_sitter_rust::LANGUAGE.into(),
        "rust",
        &highlights,
        tree_sitter_rust::INJECTIONS_QUERY,
        "",
    )
    .map_err(|error| HighlightError::Parser(error.to_string()))
}

/// The upstream Luau query is written for Neovim, and several of its patterns
/// misfire under tree-sitter-highlight. Each rewrite names the exact upstream
/// text, so a grammar bump that changes it fails the Luau tests instead of
/// silently highlighting differently.
const LUAU_QUERY_REWRITES: &[(&str, &str)] = &[
    // `#lua-match?` takes Lua patterns and the Rust runtime doesn't evaluate
    // it: every identifier would match the type and constant patterns. The
    // patterns are regexes already, but for Lua's `%s` class.
    ("#lua-match?", "#match?"),
    (r#""^[-][-](%s?)@""#, r#""^[-][-](\\s?)@""#),
    // A second capture Slopty doesn't map takes the comment's highlight.
    ("(comment) @comment @spell", "(comment) @comment"),
    // `type` is a supertype whose subtypes include `identifier`, so this
    // matches every identifier. A bare type name sits after an annotation's
    // `:`, a cast's `::` or an alias's `=`, or inside a composite type.
    (
        "(type\n  (identifier) @type)",
        "(parameter \":\" . (identifier) @type)\n\
         (variable_list \":\" . (identifier) @type)\n\
         (function_declaration \":\" . (identifier) @type)\n\
         (function_definition \":\" . (identifier) @type)\n\
         (object_type \":\" . (identifier) @type)\n\
         (cast_expression \"::\" . (identifier) @type)\n\
         (type_definition \"=\" . (identifier) @type)\n\
         ([(generic_type) (generic_type_list) (union_type) (intersection_type) (optional_type)\n\
           (variadic_type) (tuple_type) (field_type) (function_type)] (identifier) @type)",
    ),
    // Only a name before a `:` is a table type's key; the rest are types.
    (
        "(object_type\n  (identifier) @variable.member)",
        "(object_type (identifier) @variable.member . \":\")",
    ),
    // The grammar doesn't parse named parameters in function types, so
    // every identifier there is a type, not a parameter.
    ("(function_type\n  (identifier) @variable.parameter)", ""),
];

fn luau_highlights() -> String {
    LUAU_QUERY_REWRITES
        .iter()
        .fold(tree_sitter_luau::HIGHLIGHTS_QUERY.to_owned(), |query, (from, to)| {
            query.replace(from, to)
        })
}

fn markdown_configuration() -> Result<HighlightConfiguration, HighlightError> {
    // tree-sitter-highlight excludes child ranges from injections by default.
    // The Markdown block grammar's `inline` node owns anonymous children that
    // cover its source, so the upstream query otherwise injects an empty range.
    let injections = tree_sitter_md::INJECTION_QUERY_BLOCK.replace(
        "((inline) @injection.content\n  (#set! injection.language \"markdown_inline\"))",
        "((inline) @injection.content\n  (#set! injection.language \"markdown_inline\")\n  (#set! injection.include-children))",
    );
    make_configuration(
        tree_sitter_md::LANGUAGE.into(),
        "markdown",
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        &injections,
        "",
    )
}

fn markdown_inline_configuration() -> Result<HighlightConfiguration, HighlightError> {
    make_configuration(
        tree_sitter_md::INLINE_LANGUAGE.into(),
        "markdown_inline",
        tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
        tree_sitter_md::INJECTION_QUERY_INLINE,
        "",
    )
}

fn make_configuration(
    language: tree_sitter::Language,
    name: &str,
    highlights: &str,
    injections: &str,
    locals: &str,
) -> Result<HighlightConfiguration, HighlightError> {
    HighlightConfiguration::new(language, name, highlights, injections, locals)
        .map_err(|error| HighlightError::Parser(error.to_string()))
}

/// The JavaScript family's configuration: JavaScript, TypeScript and their JSX dialects share
/// JavaScript's highlights, each adding its own on top.
fn javascript_family_configuration(
    language: LanguageId,
) -> Result<HighlightConfiguration, HighlightError> {
    use LanguageId as L;
    use tree_sitter_javascript as js;
    use tree_sitter_typescript as ts;

    let (grammar, name, highlights, injections, locals): (tree_sitter::Language, _, &[&str], _, _) =
        match language {
            L::JavaScript => (
                js::LANGUAGE.into(),
                "javascript",
                &[js::HIGHLIGHT_QUERY],
                js::INJECTIONS_QUERY,
                js::LOCALS_QUERY,
            ),
            L::Jsx => (
                js::LANGUAGE.into(),
                "jsx",
                &[js::HIGHLIGHT_QUERY, js::JSX_HIGHLIGHT_QUERY],
                js::INJECTIONS_QUERY,
                js::LOCALS_QUERY,
            ),
            L::TypeScript => (
                ts::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                &[js::HIGHLIGHT_QUERY, ts::HIGHLIGHTS_QUERY],
                "",
                ts::LOCALS_QUERY,
            ),
            L::Tsx => (
                ts::LANGUAGE_TSX.into(),
                "tsx",
                &[js::HIGHLIGHT_QUERY, js::JSX_HIGHLIGHT_QUERY, ts::HIGHLIGHTS_QUERY],
                "",
                ts::LOCALS_QUERY,
            ),
            _ => return Err(HighlightError::GrammarUnavailable(language)),
        };
    make_configuration(grammar, name, &highlights.join("\n"), injections, locals)
}

fn configuration(language: LanguageId) -> Result<HighlightConfiguration, HighlightError> {
    use LanguageId as L;
    match language {
        L::Rust => rust_configuration(),
        L::JavaScript | L::Jsx | L::TypeScript | L::Tsx => {
            javascript_family_configuration(language)
        }
        L::Python => make_configuration(
            tree_sitter_python::LANGUAGE.into(),
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Go => make_configuration(
            tree_sitter_go::LANGUAGE.into(),
            "go",
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Json | L::Jsonc => make_configuration(
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Bash => make_configuration(
            tree_sitter_bash::LANGUAGE.into(),
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        L::Toml => make_configuration(
            tree_sitter_toml_ng::LANGUAGE.into(),
            "toml",
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Markdown => markdown_configuration(),
        L::Html => make_configuration(
            tree_sitter_html::LANGUAGE.into(),
            "html",
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        ),
        L::Css => make_configuration(
            tree_sitter_css::LANGUAGE.into(),
            "css",
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Yaml => make_configuration(
            tree_sitter_yaml::LANGUAGE.into(),
            "yaml",
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::C => make_configuration(
            tree_sitter_c::LANGUAGE.into(),
            "c",
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        L::Cpp => make_configuration(
            tree_sitter_cpp::LANGUAGE.into(),
            "cpp",
            &format!("{}\n{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY),
            "",
            "",
        ),
        L::CSharp => make_configuration(
            tree_sitter_c_sharp::LANGUAGE.into(),
            "csharp",
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Java => make_configuration(
            tree_sitter_java::LANGUAGE.into(),
            "java",
            tree_sitter_java::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Kotlin => make_configuration(
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            "kotlin",
            include_str!("../queries/kotlin/highlights.scm"),
            "",
            "",
        ),
        L::Swift => make_configuration(
            tree_sitter_swift::LANGUAGE.into(),
            "swift",
            tree_sitter_swift::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_swift::LOCALS_QUERY,
        ),
        L::Ruby => make_configuration(
            tree_sitter_ruby::LANGUAGE.into(),
            "ruby",
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY,
        ),
        L::Php => make_configuration(
            tree_sitter_php::LANGUAGE_PHP.into(),
            "php",
            tree_sitter_php::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Sql => make_configuration(
            tree_sitter_sequel::LANGUAGE.into(),
            "sql",
            tree_sitter_sequel::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Lua => make_configuration(
            tree_sitter_lua::LANGUAGE.into(),
            "lua",
            tree_sitter_lua::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_lua::LOCALS_QUERY,
        ),
        // Its locals query names definitions `local.definition.var` and the
        // like, which tree-sitter-highlight doesn't resolve: no locals.
        L::Luau => make_configuration(
            tree_sitter_luau::LANGUAGE.into(),
            "luau",
            &luau_highlights(),
            "",
            "",
        ),
        L::Nix => make_configuration(
            tree_sitter_nix::LANGUAGE.into(),
            "nix",
            tree_sitter_nix::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Make => make_configuration(
            tree_sitter_make::LANGUAGE.into(),
            "make",
            tree_sitter_make::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        L::Dockerfile => make_configuration(
            tree_sitter_containerfile::LANGUAGE.into(),
            "dockerfile",
            tree_sitter_containerfile::HIGHLIGHTS_QUERY,
            tree_sitter_containerfile::INJECTIONS_QUERY,
            "",
        ),
    }
}

// Ordered from generic to specific. `HighlightConfiguration::configure`
// resolves dotted captures to the best recognized name in this table.
const CAPTURE_NAMES: &[&str] = &[
    "comment",
    "keyword",
    "string",
    "string.special",
    "string.escape",
    "number",
    "boolean",
    "type",
    "type.builtin",
    "constructor",
    "function",
    "function.builtin",
    "function.macro",
    "property",
    "constant",
    "variable",
    "variable.builtin",
    "variable.parameter",
    "operator",
    "punctuation",
    "tag",
    "attribute",
    "label",
    "text.title",
    "text.literal",
    "text.uri",
    "text.reference",
    "text.emphasis",
    "text.strong",
    "embedded",
    "error",
];

const CAPTURE_KINDS: &[HighlightKind] = &[
    HighlightKind::Comment,
    HighlightKind::Keyword,
    HighlightKind::String,
    HighlightKind::StringSpecial,
    HighlightKind::Escape,
    HighlightKind::Number,
    HighlightKind::Boolean,
    HighlightKind::Type,
    HighlightKind::TypeBuiltin,
    HighlightKind::Constructor,
    HighlightKind::Function,
    HighlightKind::FunctionBuiltin,
    HighlightKind::Macro,
    HighlightKind::Property,
    HighlightKind::Constant,
    HighlightKind::Variable,
    HighlightKind::VariableSpecial,
    HighlightKind::Parameter,
    HighlightKind::Operator,
    HighlightKind::Punctuation,
    HighlightKind::Tag,
    HighlightKind::Attribute,
    HighlightKind::Label,
    HighlightKind::MarkupHeading,
    HighlightKind::MarkupRaw,
    HighlightKind::MarkupLink,
    HighlightKind::MarkupReference,
    HighlightKind::MarkupEmphasis,
    HighlightKind::MarkupStrong,
    HighlightKind::Embedded,
    HighlightKind::Invalid,
];

/// The language a source is in, named by its Markdown fence tag first, then its path, then a
/// shebang on its first line.
pub fn detect_language(
    path: Option<&str>,
    fence_tag: Option<&str>,
    first_line: Option<&str>,
) -> Option<LanguageId> {
    fence_tag
        .and_then(language_for_alias)
        .or_else(|| path.and_then(language_for_path))
        .or_else(|| first_line.and_then(language_for_shebang))
}

/// The language a name or extension stands for (`rs`, `typescript`, `c++`), case-insensitive;
/// a fence's info string is read up to its first space.
pub fn language_for_alias(alias: &str) -> Option<LanguageId> {
    let alias = alias.trim().split_ascii_whitespace().next()?.to_ascii_lowercase();
    Some(match alias.as_str() {
        "rust" | "rs" => LanguageId::Rust,
        "javascript" | "js" | "mjs" | "cjs" => LanguageId::JavaScript,
        "jsx" => LanguageId::Jsx,
        "typescript" | "ts" | "mts" | "cts" => LanguageId::TypeScript,
        "tsx" => LanguageId::Tsx,
        "python" | "py" | "python3" => LanguageId::Python,
        "go" | "golang" => LanguageId::Go,
        "json" => LanguageId::Json,
        "jsonc" => LanguageId::Jsonc,
        "bash" | "sh" | "shell" | "zsh" | "console" => LanguageId::Bash,
        "toml" => LanguageId::Toml,
        "markdown" | "md" => LanguageId::Markdown,
        "html" | "htm" => LanguageId::Html,
        "css" => LanguageId::Css,
        "yaml" | "yml" => LanguageId::Yaml,
        "c" => LanguageId::C,
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => LanguageId::Cpp,
        "csharp" | "c#" | "cs" => LanguageId::CSharp,
        "java" => LanguageId::Java,
        "kotlin" | "kt" | "kts" => LanguageId::Kotlin,
        "swift" => LanguageId::Swift,
        "ruby" | "rb" => LanguageId::Ruby,
        "php" => LanguageId::Php,
        "sql" => LanguageId::Sql,
        "lua" => LanguageId::Lua,
        "luau" => LanguageId::Luau,
        "dockerfile" | "docker" => LanguageId::Dockerfile,
        "nix" => LanguageId::Nix,
        "make" | "makefile" => LanguageId::Make,
        _ => return None,
    })
}

/// The language a file is in, by its name (`Dockerfile`, `Cargo.lock`) or else its extension.
pub fn language_for_path(path: &str) -> Option<LanguageId> {
    let path = Path::new(path);
    let name = path.file_name()?.to_str()?;
    match name.to_ascii_lowercase().as_str() {
        "dockerfile" | "containerfile" => return Some(LanguageId::Dockerfile),
        "makefile" | "gnumakefile" => return Some(LanguageId::Make),
        "cargo.lock" | "cargo.toml" | "pyproject.toml" => return Some(LanguageId::Toml),
        _ => {}
    }
    language_for_alias(path.extension()?.to_str()?)
}

fn language_for_shebang(line: &str) -> Option<LanguageId> {
    let line = line.strip_prefix("#!")?.to_ascii_lowercase();
    if line.contains("python") {
        Some(LanguageId::Python)
    } else if line.contains("node") {
        Some(LanguageId::JavaScript)
    } else if line.contains("ruby") {
        Some(LanguageId::Ruby)
    } else if ["bash", "zsh", "/sh", " sh"].iter().any(|name| line.contains(name)) {
        Some(LanguageId::Bash)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_queries_are_shared_across_concurrent_documents() {
        let config = cached_configuration(LanguageId::Rust).unwrap();
        std::thread::scope(|scope| {
            for i in 0..8 {
                scope.spawn(move || {
                    let shared = cached_configuration(LanguageId::Rust).unwrap();
                    assert!(std::ptr::eq(config, shared));
                    let source = format!("fn example_{i}() {{ let value = {i}; }}");
                    let document = highlight(HighlightRequest {
                        source: &source,
                        path: None,
                        fence_tag: Some("rust"),
                    })
                    .unwrap();
                    assert_eq!(document.language, LanguageId::Rust);
                    assert!(document.lines.iter().flatten().next().is_some());
                });
            }
        });
    }

    #[test]
    fn aliases_keep_language_variants_distinct() {
        let cases = [
            ("js", LanguageId::JavaScript),
            ("jsx", LanguageId::Jsx),
            ("ts", LanguageId::TypeScript),
            ("tsx", LanguageId::Tsx),
            ("RS", LanguageId::Rust),
            ("shell", LanguageId::Bash),
        ];
        for (alias, expected) in cases {
            assert_eq!(language_for_alias(alias), Some(expected), "{alias}");
        }
        assert_eq!(language_for_alias("unknown-lang"), None);
    }

    #[test]
    fn paths_and_exact_names_are_table_driven() {
        let cases = [
            ("src/main.rs", LanguageId::Rust),
            ("web/app.tsx", LanguageId::Tsx),
            ("Cargo.toml", LanguageId::Toml),
            ("Dockerfile", LanguageId::Dockerfile),
            ("GNUmakefile", LanguageId::Make),
            ("config.jsonc", LanguageId::Jsonc),
        ];
        for (path, expected) in cases {
            assert_eq!(language_for_path(path), Some(expected), "{path}");
        }
        assert_eq!(language_for_path("README"), None);
        assert_eq!(language_for_path("image.png"), None);
    }

    #[test]
    fn shebang_is_only_used_after_explicit_hints() {
        assert_eq!(
            detect_language(None, None, Some("#!/usr/bin/env python3")),
            Some(LanguageId::Python)
        );
        assert_eq!(detect_language(None, None, Some("let x = 1")), None);
    }

    #[test]
    fn spans_are_valid_sorted_non_overlapping_and_line_relative() {
        let source = "let café = \"x\";\nnext";
        let document = HighlightedDocument::from_absolute_spans(
            LanguageId::Rust,
            source,
            [
                HighlightSpan { range: 0..9, kind: HighlightKind::Variable },
                HighlightSpan { range: 0..3, kind: HighlightKind::Keyword },
                HighlightSpan { range: 12..15, kind: HighlightKind::String },
                HighlightSpan { range: 17..21, kind: HighlightKind::Function },
            ],
        )
        .unwrap();
        assert_eq!(document.lines.len(), 2);
        assert_eq!(
            document.lines[0][0],
            HighlightSpan { range: 0..3, kind: HighlightKind::Keyword }
        );
        for line in document.lines {
            assert!(line.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start));
        }
        assert_eq!(
            HighlightedDocument::from_absolute_spans(
                LanguageId::Rust,
                source,
                [HighlightSpan { range: 8..9, kind: HighlightKind::Type }]
            ),
            Err(HighlightError::InvalidUtf8Boundary { start: 8, end: 9 })
        );
    }

    #[test]
    fn normalization_preserves_overlap_precedence_and_tie_order() {
        let normalized = normalize_line(&[
            HighlightSpan { range: 0..10, kind: HighlightKind::Variable },
            HighlightSpan { range: 2..8, kind: HighlightKind::Keyword },
            HighlightSpan { range: 4..6, kind: HighlightKind::String },
        ]);
        assert_eq!(
            normalized,
            vec![
                HighlightSpan { range: 0..2, kind: HighlightKind::Variable },
                HighlightSpan { range: 2..4, kind: HighlightKind::Keyword },
                HighlightSpan { range: 4..6, kind: HighlightKind::String },
                HighlightSpan { range: 6..8, kind: HighlightKind::Keyword },
                HighlightSpan { range: 8..10, kind: HighlightKind::Variable },
            ]
        );
    }

    #[test]
    fn minified_lines_normalize_without_quadratic_rescans() {
        let source = "let value=1;".repeat(8_000);
        let document = highlight(HighlightRequest {
            source: &source,
            path: Some("bundle.js"),
            fence_tag: None,
        })
        .unwrap();
        assert!(document.lines[0].len() > 20_000);
    }

    fn highlighted_fragments(source: &str) -> Vec<(&str, HighlightKind)> {
        highlighted_fragments_at("src/lib.rs", source)
    }

    fn highlighted_fragments_at<'a>(path: &str, source: &'a str) -> Vec<(&'a str, HighlightKind)> {
        let document =
            highlight(HighlightRequest { source, path: Some(path), fence_tag: None }).unwrap();
        source
            .lines()
            .zip(document.lines)
            .flat_map(|(line, spans)| {
                spans.into_iter().map(move |span| (line.get(span.range).unwrap(), span.kind))
            })
            .collect()
    }

    #[test]
    fn rust_highlighting_distinguishes_structural_categories() {
        let source = r#"pub struct Widget { field: usize }
fn build(value: usize) -> Widget {
    let name = format!("item-{value}");
    Widget { field: 42 }
}"#;
        let fragments = highlighted_fragments(source);
        for (text, expected) in [
            ("pub", HighlightKind::Keyword),
            ("Widget", HighlightKind::Type),
            ("build", HighlightKind::Function),
            ("format!", HighlightKind::Macro),
            ("42", HighlightKind::Number),
        ] {
            assert!(
                fragments.contains(&(text, expected)),
                "missing {text:?} as {expected:?}: {fragments:?}"
            );
        }
    }

    #[test]
    fn luau_highlighting_covers_its_syntax_beyond_lua() {
        let source = r#"--!strict
export type Point<T> = { x: T, y: T? }
local MAX_COUNT = 10
local function add(a: Point<number>, b: Point<number>): Point<number>
    return { x = a.x + b.x, y = a.y + b.y }
end
local total = 0
for i = 1, MAX_COUNT do
    if i % 2 == 0 then continue end
    total += i
end
local label = if total > 10 then `big {total}` else "small"
local Players = game:GetService("Players")
local item = value :: Item
print(typeof(label), self)"#;
        let fragments = highlighted_fragments_at("app.luau", source);
        for (text, expected) in [
            ("--!strict", HighlightKind::Comment),
            ("export", HighlightKind::Keyword),
            ("type", HighlightKind::Keyword),
            ("number", HighlightKind::TypeBuiltin),
            ("Item", HighlightKind::Type),
            ("x", HighlightKind::Variable),
            ("a", HighlightKind::Parameter),
            ("MAX_COUNT", HighlightKind::Constant),
            ("add", HighlightKind::Function),
            ("GetService", HighlightKind::Function),
            ("continue", HighlightKind::Keyword),
            ("+=", HighlightKind::Operator),
            ("`big {total}`", HighlightKind::String),
            ("typeof", HighlightKind::FunctionBuiltin),
            ("self", HighlightKind::VariableSpecial),
        ] {
            assert!(
                fragments.contains(&(text, expected)),
                "missing {text:?} as {expected:?}: {fragments:?}"
            );
        }
        // Upstream, every identifier matched the type patterns.
        for text in ["total", "i", "label", "item", "value", "a"] {
            assert!(
                !fragments.contains(&(text, HighlightKind::Type)),
                "{text:?} highlighted as a type: {fragments:?}"
            );
        }
        assert!(fragments.contains(&("Point", HighlightKind::Type)));
        assert!(fragments.contains(&("T", HighlightKind::Type)));
    }

    #[test]
    fn luau_query_rewrites_still_apply_to_the_pinned_grammar() {
        for (from, _) in LUAU_QUERY_REWRITES {
            assert!(
                tree_sitter_luau::HIGHLIGHTS_QUERY.contains(from),
                "the upstream Luau query no longer contains {from:?}"
            );
        }
        let query = luau_highlights();
        assert!(!query.contains("lua-match") && !query.contains("@spell"));
        assert_eq!(language_for_path("src/Main.client.luau"), Some(LanguageId::Luau));
        assert_eq!(language_for_alias("luau"), Some(LanguageId::Luau));
    }

    #[test]
    fn rust_multiline_raw_unicode_and_incomplete_code_remain_valid() {
        let source = "/* café\ncomment */\nlet raw = r#\"héllo\nworld\"#;\nlet before = 7;\nfn incomplete( {";
        let document =
            highlight(HighlightRequest { source, path: None, fence_tag: Some("rust") }).unwrap();
        assert!(document.lines[1].iter().any(|span| span.kind == HighlightKind::Comment));
        assert!(document.lines[3].iter().any(|span| span.kind == HighlightKind::String));
        assert!(document.lines.iter().flatten().any(|span| span.kind == HighlightKind::Number));
        for (line, spans) in source.lines().zip(&document.lines) {
            for span in spans {
                assert!(line.is_char_boundary(span.range.start));
                assert!(line.is_char_boundary(span.range.end));
            }
        }
    }

    #[test]
    fn limits_and_unbundled_languages_degrade_with_typed_errors() {
        assert_eq!(
            highlight_with_limits(
                HighlightRequest { source: "fn main() {}", path: Some("main.rs"), fence_tag: None },
                HighlightLimits { max_source_bytes: 2, max_spans: 10 },
                None,
            ),
            Err(HighlightError::SourceTooLarge)
        );
        assert_eq!(
            highlight(HighlightRequest {
                source: "plain",
                path: Some("unknown.extension"),
                fence_tag: None,
            }),
            Err(HighlightError::UnknownLanguage)
        );
    }

    #[test]
    fn rust_queries_load_for_the_bundled_abi() {
        rust_configuration().unwrap();
        let language_version = std::hint::black_box(tree_sitter::LANGUAGE_VERSION);
        assert!(language_version >= tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION);
    }

    #[test]
    fn every_registered_grammar_and_query_loads_for_the_bundled_abi() {
        let fixtures = [
            (LanguageId::JavaScript, "app.js", "const value = call(42);"),
            (LanguageId::Jsx, "app.jsx", "const view = <main id=\"x\" />;"),
            (LanguageId::TypeScript, "app.ts", "const value: number = 42;"),
            (LanguageId::Tsx, "app.tsx", "const view: JSX.Element = <main />;"),
            (LanguageId::Python, "app.py", "def call(value):\n    return value"),
            (LanguageId::Go, "main.go", "package main\nfunc main() {}"),
            (LanguageId::Json, "a.json", "{\"value\": 42}"),
            (LanguageId::Jsonc, "a.jsonc", "{\"value\": 42}"),
            (LanguageId::Bash, "run.sh", "echo \"hello\""),
            (LanguageId::Toml, "Cargo.toml", "name = \"slopty\""),
            (LanguageId::Markdown, "README.md", "# Heading\n\n`code`"),
            (LanguageId::Html, "index.html", "<main id=\"app\"></main>"),
            (LanguageId::Css, "app.css", ".app { color: red; }"),
            (LanguageId::Yaml, "app.yml", "name: slopty"),
            (LanguageId::C, "main.c", "int main(void) { return 0; }"),
            (LanguageId::Cpp, "main.cpp", "int main() { return 0; }"),
            (LanguageId::CSharp, "App.cs", "class App { int Value = 1; }"),
            (LanguageId::Java, "App.java", "class App { int value = 1; }"),
            (LanguageId::Kotlin, "App.kt", "val value = 1"),
            (LanguageId::Swift, "App.swift", "let value: Int = 1"),
            (LanguageId::Ruby, "app.rb", "def call(value)\n value\nend"),
            (LanguageId::Php, "app.php", "<?php function call() { return 1; }"),
            (LanguageId::Sql, "query.sql", "SELECT name FROM users;"),
            (LanguageId::Lua, "app.lua", "local value = 1"),
            (LanguageId::Luau, "app.luau", "local value: number = 1"),
            (LanguageId::Nix, "flake.nix", "{ pkgs }: pkgs.hello"),
            (LanguageId::Make, "Makefile", "all:\n\techo hello"),
            (LanguageId::Dockerfile, "Dockerfile", "FROM alpine\nRUN echo hello"),
        ];
        for (language, path, source) in fixtures {
            let config =
                configuration(language).unwrap_or_else(|error| panic!("{language:?}: {error}"));
            assert!(!config.names().is_empty(), "{language:?} query has no captures");
            let document =
                highlight(HighlightRequest { source, path: Some(path), fence_tag: None })
                    .unwrap_or_else(|error| panic!("{language:?}: {error}"));
            assert_eq!(document.language, language);
            assert!(
                document.lines.iter().flatten().next().is_some(),
                "{language:?} fixture has no structural spans"
            );
        }
    }

    #[test]
    fn affected_composed_and_project_queries_load_for_pinned_grammars() {
        for language in
            [LanguageId::TypeScript, LanguageId::Tsx, LanguageId::Kotlin, LanguageId::Dockerfile]
        {
            let configuration = configuration(language)
                .unwrap_or_else(|error| panic!("{language:?} query failed to load: {error}"));
            assert!(!configuration.names().is_empty(), "{language:?} query has no captures");
        }
    }

    #[test]
    fn html_injects_javascript_and_css_with_a_bounded_registry() {
        let source = r#"<main id="app">
<style>.item { color: red; }</style>
<script>const answer = call(42);</script>
</main>"#;
        let document =
            highlight(HighlightRequest { source, path: Some("index.html"), fence_tag: None })
                .unwrap();
        let kinds = document.lines.iter().flatten().map(|span| span.kind).collect::<Vec<_>>();
        assert!(kinds.contains(&HighlightKind::Tag));
        assert!(kinds.contains(&HighlightKind::Attribute));
        assert!(kinds.contains(&HighlightKind::Keyword));
        assert!(kinds.contains(&HighlightKind::Number));
    }

    #[test]
    fn jsonc_accepts_and_highlights_comments() {
        let source = "{\n  // explanation\n  \"enabled\": true\n}\n";
        let document =
            highlight(HighlightRequest { source, path: Some("settings.jsonc"), fence_tag: None })
                .unwrap();
        assert_eq!(document.language, LanguageId::Jsonc);
        assert!(document.lines[1].iter().any(|span| span.kind == HighlightKind::Comment));
    }

    #[test]
    fn unknown_markdown_fence_does_not_break_parent_highlighting() {
        let source = "# Title\n\n```unknown-language\nopaque\n```\n";
        let document =
            highlight(HighlightRequest { source, path: Some("README.md"), fence_tag: None })
                .unwrap();
        assert_eq!(document.language, LanguageId::Markdown);
    }

    #[test]
    fn markdown_highlights_block_and_inline_semantics() {
        let source = "# Heading with *emphasis* and **strong**\n\nUse `inline code` and [reference](https://example.com).\n";
        let document =
            highlight(HighlightRequest { source, path: Some("README.md"), fence_tag: None })
                .unwrap();

        let assert_kind = |line_index: usize, needle: &str, expected: HighlightKind| {
            let line = source.lines().nth(line_index).unwrap();
            let start = line.find(needle).unwrap();
            let end = start + needle.len();
            assert!(
                document.lines[line_index].iter().any(|span| {
                    span.kind == expected && span.range.start <= start && span.range.end >= end
                }),
                "missing {needle:?} as {expected:?}: {:?}",
                document.lines[line_index]
            );
        };

        assert_kind(0, "Heading", HighlightKind::MarkupHeading);
        assert_kind(0, "emphasis", HighlightKind::MarkupEmphasis);
        assert_kind(0, "strong", HighlightKind::MarkupStrong);
        assert_kind(2, "inline code", HighlightKind::MarkupRaw);
        assert_kind(2, "reference", HighlightKind::MarkupReference);
        assert_kind(2, "https://example.com", HighlightKind::MarkupLink);
    }

    #[test]
    fn markdown_fences_use_all_bundled_child_grammars() {
        let source = "```rust\nfn main() { let value = 42; }\n```\n\n```yaml\nenabled: true\n```\n";
        let document =
            highlight(HighlightRequest { source, path: Some("README.md"), fence_tag: None })
                .unwrap();
        assert!(
            document.lines[1].iter().any(|span| span.kind == HighlightKind::Keyword),
            "Rust fence was not injected: {:?}",
            document.lines[1]
        );
        assert!(
            document.lines[5].iter().any(|span| matches!(
                span.kind,
                HighlightKind::Property | HighlightKind::Boolean | HighlightKind::String
            )),
            "YAML fence was not injected: {:?}",
            document.lines[5]
        );
    }
}
