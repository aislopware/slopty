// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Syntax highlighting as zeron's surfaces ask for it: byte ranges of one line, each a
//! [`HighlightKind`] the theme colours ([`crate::theme::SyntaxPalette`]).
//!
//! Zeron parses with tree-sitter (`crates/syntax`); this keeps its contract (the kinds, their
//! precedence, line-relative spans, the request) and answers it with Slopty's syntect grammars
//! ([`crate::highlight`]), mapping their `TextMate` scopes onto the same kinds.

use std::collections::BTreeSet;
use std::ops::Range;
use std::str::FromStr as _;
use std::sync::LazyLock;

use syntect::highlighting::{
    Color, FontStyle, HighlightIterator, HighlightState, Highlighter, ScopeSelectors,
    StyleModifier, Theme as ScopeTheme, ThemeItem,
};
use syntect::parsing::{ParseState, ScopeStack};

pub use crate::highlight::Syntax as LanguageId;

/// The largest source highlighted by default.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// The most spans a document keeps by default.
pub const DEFAULT_MAX_SPANS: usize = 200_000;

/// How much a highlight may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightLimits {
    /// Bytes of source past which nothing is highlighted.
    pub max_source_bytes: usize,
    /// Spans past which the highlight gives up.
    pub max_spans: usize,
}

impl Default for HighlightLimits {
    fn default() -> Self {
        Self { max_source_bytes: DEFAULT_MAX_SOURCE_BYTES, max_spans: DEFAULT_MAX_SPANS }
    }
}

/// The kinds of text the syntax palette colours apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[expect(missing_docs, reason = "each kind is named for what it colours")]
pub enum HighlightKind {
    Comment,
    Keyword,
    String,
    StringSpecial,
    Escape,
    Number,
    Boolean,
    Type,
    TypeBuiltin,
    Constructor,
    Function,
    FunctionBuiltin,
    Macro,
    Property,
    Constant,
    Variable,
    VariableSpecial,
    Parameter,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Label,
    MarkupHeading,
    MarkupRaw,
    MarkupLink,
    MarkupReference,
    MarkupEmphasis,
    MarkupStrong,
    Embedded,
    Invalid,
}

impl HighlightKind {
    /// Every kind, in the order the scope theme encodes them.
    pub const ALL: [Self; 31] = [
        Self::Comment,
        Self::Keyword,
        Self::String,
        Self::StringSpecial,
        Self::Escape,
        Self::Number,
        Self::Boolean,
        Self::Type,
        Self::TypeBuiltin,
        Self::Constructor,
        Self::Function,
        Self::FunctionBuiltin,
        Self::Macro,
        Self::Property,
        Self::Constant,
        Self::Variable,
        Self::VariableSpecial,
        Self::Parameter,
        Self::Operator,
        Self::Punctuation,
        Self::Tag,
        Self::Attribute,
        Self::Label,
        Self::MarkupHeading,
        Self::MarkupRaw,
        Self::MarkupLink,
        Self::MarkupReference,
        Self::MarkupEmphasis,
        Self::MarkupStrong,
        Self::Embedded,
        Self::Invalid,
    ];

    /// Which of two overlapping kinds shows.
    #[must_use]
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
            Self::MarkupHeading => 30,
            Self::MarkupEmphasis => 31,
            Self::MarkupStrong => 32,
            Self::MarkupLink | Self::MarkupReference => 33,
            Self::MarkupRaw => 34,
        }
    }

    /// The stand-in colour the scope theme paints this kind in: its index plus one in red.
    fn encoded(self) -> Color {
        let index = Self::ALL.iter().position(|kind| *kind == self).unwrap_or_default();
        let r = u8::try_from(index.saturating_add(1)).unwrap_or(u8::MAX);
        Color { r, g: 0, b: 0, a: 255 }
    }

    /// The kind a stand-in colour encodes; none for plain text.
    fn decoded(color: Color) -> Option<Self> {
        usize::from(color.r).checked_sub(1).and_then(|index| Self::ALL.get(index).copied())
    }
}

/// One run of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Bytes of the line.
    pub range: Range<usize>,
    /// What they are.
    pub kind: HighlightKind,
}

/// A whole source's spans, one list a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedDocument {
    /// The grammar it was parsed with.
    pub language: LanguageId,
    /// Each line's spans, line-relative.
    pub lines: Vec<Vec<HighlightSpan>>,
}

/// What to highlight and how to find its language.
#[derive(Debug, Clone, Copy)]
pub struct HighlightRequest<'a> {
    /// The source.
    pub source: &'a str,
    /// Its path, for the language by extension or name.
    pub path: Option<&'a str>,
    /// A fence's tag (```` ```rust ````).
    pub fence_tag: Option<&'a str>,
}

/// Why nothing was highlighted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HighlightError {
    /// No grammar for the source.
    UnknownLanguage,
    /// The source is past [`HighlightLimits::max_source_bytes`].
    SourceTooLarge,
    /// The spans went past [`HighlightLimits::max_spans`].
    TooManySpans,
    /// The grammar failed on a line.
    Parser(String),
}

impl std::fmt::Display for HighlightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownLanguage => f.write_str("the source language is not registered"),
            Self::SourceTooLarge => f.write_str("source exceeds the configured highlighting limit"),
            Self::TooManySpans => f.write_str("highlight output exceeds the configured span limit"),
            Self::Parser(why) => write!(f, "parser failed: {why}"),
        }
    }
}

impl std::error::Error for HighlightError {}

/// The scope theme that paints each kind as its stand-in colour; the selectors follow the
/// `TextMate` conventions the bundled grammars share, more specific selectors winning.
fn scope_theme() -> &'static ScopeTheme {
    static THEME: LazyLock<ScopeTheme> = LazyLock::new(|| {
        use HighlightKind as K;
        let rules: [(&str, HighlightKind); 31] = [
            ("comment, punctuation.definition.comment", K::Comment),
            ("keyword, storage.type, storage.modifier, keyword.declaration", K::Keyword),
            ("string, punctuation.definition.string", K::String),
            ("string.regexp, string.other.link, string.interpolated", K::StringSpecial),
            ("constant.character.escape", K::Escape),
            ("constant.numeric", K::Number),
            ("constant.language.boolean, constant.language.bool", K::Boolean),
            (
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
                 entity.name.trait, entity.name.union, entity.name.interface, \
                 entity.other.inherited-class, support.class",
                K::Type,
            ),
            ("support.type, storage.type.primitive, storage.type.numeric", K::TypeBuiltin),
            ("entity.name.constructor, meta.struct.constructor", K::Constructor),
            (
                "entity.name.function, variable.function, meta.function-call.identifier, \
                 support.function.user",
                K::Function,
            ),
            ("support.function", K::FunctionBuiltin),
            ("support.macro, entity.name.macro, variable.function.macro, meta.macro", K::Macro),
            (
                "variable.other.member, variable.other.property, variable.other.object.property, \
                 meta.property-name, support.type.property-name, meta.mapping.key string, \
                 entity.name.tag.yaml",
                K::Property,
            ),
            ("constant, variable.other.constant, constant.other", K::Constant),
            ("variable", K::Variable),
            ("variable.language, keyword.other.self", K::VariableSpecial),
            ("variable.parameter", K::Parameter),
            ("keyword.operator", K::Operator),
            ("punctuation", K::Punctuation),
            ("entity.name.tag", K::Tag),
            ("entity.other.attribute-name, meta.attribute, meta.annotation", K::Attribute),
            ("entity.name.label, entity.name.lifetime, storage.modifier.lifetime", K::Label),
            ("markup.heading, entity.name.section.markdown", K::MarkupHeading),
            ("markup.raw, markup.inline.raw", K::MarkupRaw),
            ("markup.underline.link, meta.link", K::MarkupLink),
            ("meta.link.reference, constant.other.reference.link", K::MarkupReference),
            ("markup.italic", K::MarkupEmphasis),
            ("markup.bold", K::MarkupStrong),
            ("source.embedded, meta.embedded", K::Embedded),
            ("invalid", K::Invalid),
        ];
        let mut theme = ScopeTheme::default();
        theme.settings.foreground = Some(Color { r: 0, g: 0, b: 0, a: 255 });
        theme.scopes = rules
            .into_iter()
            .filter_map(|(selector, kind)| {
                Some(ThemeItem {
                    scope: ScopeSelectors::from_str(selector).ok()?,
                    style: StyleModifier {
                        foreground: Some(kind.encoded()),
                        background: None,
                        font_style: Some(FontStyle::empty()),
                    },
                })
            })
            .collect();
        theme
    });
    &THEME
}

/// Whether this build can highlight `language`: every bundled grammar can.
#[must_use]
pub const fn supports_language(_language: LanguageId) -> bool {
    true
}

/// A whole document with the default limits.
///
/// # Errors
///
/// When no grammar fits, the source is too large, or the grammar fails.
pub fn highlight(request: HighlightRequest<'_>) -> Result<HighlightedDocument, HighlightError> {
    highlight_with_limits(request, HighlightLimits::default())
}

/// A whole document with explicit limits.
///
/// # Errors
///
/// When no grammar fits, the source is past the limit, the spans run past theirs, or the grammar
/// fails.
pub fn highlight_with_limits(
    request: HighlightRequest<'_>,
    limits: HighlightLimits,
) -> Result<HighlightedDocument, HighlightError> {
    if request.source.len() > limits.max_source_bytes {
        return Err(HighlightError::SourceTooLarge);
    }
    let language = detect_language(request.path, request.fence_tag, request.source.lines().next())
        .ok_or(HighlightError::UnknownLanguage)?;
    let highlighter = Highlighter::new(scope_theme());
    let mut parse = ParseState::new(language.reference());
    let mut state = HighlightState::new(&highlighter, ScopeStack::new());
    let mut lines = Vec::new();
    let mut total = 0_usize;
    for line in request.source.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        if body.len() > crate::highlight::LINE_MAX {
            lines.push(Vec::new());
            continue;
        }
        let ops = parse
            .parse_line(line, crate::highlight::syntaxes())
            .map_err(|error| HighlightError::Parser(error.to_string()))?;
        let mut spans: Vec<HighlightSpan> = Vec::new();
        let mut at = 0_usize;
        for (style, piece) in HighlightIterator::new(&mut state, &ops, line, &highlighter) {
            let start = at;
            at = at.saturating_add(piece.len()).min(body.len());
            let Some(kind) = HighlightKind::decoded(style.foreground) else { continue };
            if start >= at {
                continue;
            }
            match spans.last_mut() {
                Some(last) if last.kind == kind && last.range.end == start => last.range.end = at,
                _ => spans.push(HighlightSpan { range: start..at, kind }),
            }
        }
        total = total.saturating_add(spans.len());
        if total > limits.max_spans {
            return Err(HighlightError::TooManySpans);
        }
        lines.push(spans);
    }
    if request.source.is_empty() || request.source.ends_with('\n') {
        lines.push(Vec::new());
    }
    Ok(HighlightedDocument { language, lines })
}

/// Overlapping spans resolved to one kind per byte by [`HighlightKind::precedence`], the later
/// span winning a tie, and neighbours of one kind merged.
#[must_use]
pub fn normalize_line(spans: &[HighlightSpan]) -> Vec<HighlightSpan> {
    let mut edges: Vec<usize> =
        spans.iter().flat_map(|span| [span.range.start, span.range.end]).collect();
    edges.sort_unstable();
    edges.dedup();
    let mut normalized: Vec<HighlightSpan> = Vec::new();
    for pair in edges.windows(2) {
        let [start, end] =
            [pair.first().copied().unwrap_or_default(), pair.get(1).copied().unwrap_or_default()];
        let active: BTreeSet<(u8, usize)> = spans
            .iter()
            .enumerate()
            .filter(|(_, span)| span.range.start <= start && span.range.end >= end)
            .map(|(index, span)| (span.kind.precedence(), index))
            .collect();
        let Some(kind) = active.last().and_then(|(_, index)| spans.get(*index)).map(|s| s.kind)
        else {
            continue;
        };
        match normalized.last_mut() {
            Some(previous) if previous.kind == kind && previous.range.end == start => {
                previous.range.end = end;
            }
            _ => normalized.push(HighlightSpan { range: start..end, kind }),
        }
    }
    normalized
}

/// The language by a fence's tag, else the path, else a shebang.
#[must_use]
pub fn detect_language(
    path: Option<&str>,
    fence_tag: Option<&str>,
    first_line: Option<&str>,
) -> Option<LanguageId> {
    fence_tag.and_then(language_for_alias).or_else(|| {
        path.and_then(|path| LanguageId::for_path(path, first_line.unwrap_or_default()))
    })
}

/// The language a fence or an alias names (`rust`, `sh`, `tsx`).
#[must_use]
pub fn language_for_alias(alias: &str) -> Option<LanguageId> {
    let alias = alias.split_ascii_whitespace().next()?;
    LanguageId::for_token(alias)
}

/// The language of a file at `path`.
#[must_use]
pub fn language_for_path(path: &str) -> Option<LanguageId> {
    LanguageId::for_path(path, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str, tag: &str) -> Vec<(String, HighlightKind)> {
        let document =
            highlight(HighlightRequest { source, path: None, fence_tag: Some(tag) }).expect("rust");
        document
            .lines
            .iter()
            .zip(source.split('\n'))
            .flat_map(|(spans, line)| {
                spans.iter().map(move |span| {
                    (line.get(span.range.clone()).unwrap_or_default().to_owned(), span.kind)
                })
            })
            .collect()
    }

    #[test]
    fn rust_reads_as_its_kinds() {
        let got = kinds("// note\nfn main() { let x = \"hi\"; 42 }\n", "rust");
        assert!(got.contains(&("// note".to_owned(), HighlightKind::Comment)), "{got:?}");
        assert!(got.contains(&("fn".to_owned(), HighlightKind::Keyword)), "{got:?}");
        assert!(got.contains(&("main".to_owned(), HighlightKind::Function)), "{got:?}");
        assert!(
            got.iter().any(|(text, kind)| text.contains("hi") && *kind == HighlightKind::String),
            "{got:?}"
        );
        assert!(got.contains(&("42".to_owned(), HighlightKind::Number)), "{got:?}");
    }

    #[test]
    fn an_unknown_fence_has_no_language() {
        let request = HighlightRequest { source: "x", path: None, fence_tag: Some("nonsense") };
        assert_eq!(highlight(request), Err(HighlightError::UnknownLanguage), "unknown");
    }

    #[test]
    fn overlaps_resolve_by_precedence() {
        let spans = [
            HighlightSpan { range: 0..10, kind: HighlightKind::String },
            HighlightSpan { range: 3..5, kind: HighlightKind::Escape },
        ];
        let got = normalize_line(&spans);
        assert_eq!(
            got,
            [
                HighlightSpan { range: 0..3, kind: HighlightKind::String },
                HighlightSpan { range: 3..5, kind: HighlightKind::Escape },
                HighlightSpan { range: 5..10, kind: HighlightKind::String },
            ],
            "the escape shows inside its string"
        );
    }
}
