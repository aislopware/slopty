//! Syntax colouring for code the canvas shows: a file card's lines and a fenced block in an
//! answer.
//!
//! [`syntect`] parses with Sublime Text grammars (its bundled set, pure Rust through
//! `fancy-regex`) and this module reduces its scopes to a handful of [`Token`]s, so the
//! colours come from the app's own theme at draw time rather than from a `TextMate` theme:
//! a theme swap or a settings reload recolours without parsing again, and every surface
//! that shows code agrees with the terminal's palette. Parsing is the slow part (tens of
//! milliseconds for a full card, see `docs/MEASUREMENTS.md`), so the file card does it off
//! the UI thread and draws plain text until the spans arrive.

use std::ops::Range;
use std::str::FromStr as _;
use std::sync::LazyLock;

use gpui::{Font, FontStyle, FontWeight, HighlightStyle, Hsla, TextRun};
use gpui_kit::base::text::CodeBlock;
use slopty_theme::Theme;
use syntect::highlighting::{
    Color, FontStyle as SyntectStyle, ScopeSelectors, StyleModifier, Theme as ScopeTheme, ThemeItem,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::colors::hsla;

/// Lines longer than this are left plain: a minified bundle or a data line would cost
/// seconds and read as noise anyway.
pub const LINE_MAX: usize = 4_000;

/// The kinds of text a theme colours apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Token {
    /// Everything the grammar did not single out.
    Plain,
    /// A comment.
    Comment,
    /// A string or character literal.
    String,
    /// A number, or a language constant (`true`, `None`, `nil`).
    Constant,
    /// A keyword, or a storage word (`fn`, `let`, `pub`, `struct`).
    Keyword,
    /// A type, class, struct or enum name, or a markup tag.
    Type,
    /// A function or method name, or a markup attribute.
    Function,
    /// Punctuation and operators.
    Punctuation,
    /// What the grammar could not parse.
    Invalid,
}

impl Token {
    /// The token a syntect colour encodes: [`scope_theme`] paints each token as a distinct
    /// stand-in colour, and this reads it back.
    const fn from_color(color: Color) -> Self {
        match color.r {
            1 => Self::Comment,
            2 => Self::String,
            3 => Self::Constant,
            4 => Self::Keyword,
            5 => Self::Type,
            6 => Self::Function,
            7 => Self::Punctuation,
            8 => Self::Invalid,
            _ => Self::Plain,
        }
    }

    const fn as_color(self) -> Color {
        let r = match self {
            Self::Plain => 0,
            Self::Comment => 1,
            Self::String => 2,
            Self::Constant => 3,
            Self::Keyword => 4,
            Self::Type => 5,
            Self::Function => 6,
            Self::Punctuation => 7,
            Self::Invalid => 8,
        };
        Color { r, g: 0, b: 0, a: 255 }
    }

    /// The token's colour in `theme`: the terminal's ANSI palette for the code colours (so
    /// code reads as it does in the shell) and the chrome's greys for the rest.
    #[must_use]
    pub fn color(self, theme: &Theme) -> Hsla {
        let ansi = &theme.terminal.ansi;
        let s = &theme.surfaces;
        hsla(match self {
            Self::Plain => s.text,
            Self::Comment => s.text_muted,
            Self::String => ansi[2],
            Self::Constant => ansi[3],
            Self::Keyword => ansi[5],
            Self::Type => ansi[6],
            Self::Function => ansi[4],
            Self::Punctuation => s.text_secondary,
            Self::Invalid => s.error,
        })
    }
}

/// One run of a line, `len` bytes of one [`Token`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    /// Bytes of the line.
    pub len: usize,
    /// What they are.
    pub token: Token,
    /// The grammar's theme asked for italics (comments).
    pub italic: bool,
    /// The grammar's theme asked for bold (headings).
    pub bold: bool,
}

/// Which grammar to parse with.
#[derive(Clone, Copy)]
pub struct Syntax(&'static SyntaxReference);

impl PartialEq for Syntax {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Syntax {}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Syntax").field(&self.0.name).finish()
    }
}

impl Syntax {
    /// The grammar for a file at `path`, by its extension or name (`Makefile`), else by its
    /// first line (`#!/bin/sh`); none for a file the bundle knows nothing about.
    #[must_use]
    pub fn for_path(path: &str, first_line: &str) -> Option<Self> {
        let set = syntaxes();
        let name = path.rsplit('/').next().unwrap_or(path);
        let by_ext = name
            .rsplit_once('.')
            .and_then(|(_, ext)| set.find_syntax_by_extension(ext))
            .or_else(|| set.find_syntax_by_extension(name));
        by_ext.or_else(|| set.find_syntax_by_first_line(first_line)).and_then(Self::coloured)
    }

    /// `syntax` unless it is the bundle's plain text (`.txt`), which colours nothing and would
    /// only add a name to the card's summary.
    fn coloured(syntax: &'static SyntaxReference) -> Option<Self> {
        (!std::ptr::eq(syntax, syntaxes().find_syntax_plain_text())).then_some(Self(syntax))
    }

    /// The grammar a fence names (```` ```rust ````, `sh`, `json`); none for an unknown or
    /// empty tag.
    #[must_use]
    pub fn for_token(lang: &str) -> Option<Self> {
        let lang = lang.trim();
        if lang.is_empty() {
            return None;
        }
        let set = syntaxes();
        set.find_syntax_by_token(lang)
            .or_else(|| set.find_syntax_by_extension(lang))
            .and_then(Self::coloured)
    }

    /// The grammar's name ("Rust").
    #[must_use]
    pub fn name(self) -> &'static str {
        &self.0.name
    }
}

/// The bundled grammars, loaded once on first use (a few tens of milliseconds, paid on a
/// background thread by the first card).
fn syntaxes() -> &'static SyntaxSet {
    static SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    &SET
}

/// The stand-in theme: each scope family paints as a colour whose red channel is the
/// [`Token`], read back by [`Token::from_color`]. Scope selectors follow the `TextMate`
/// conventions the bundled grammars share.
fn scope_theme() -> &'static ScopeTheme {
    static THEME: LazyLock<ScopeTheme> = LazyLock::new(|| {
        let rules: [(&str, Token, SyntectStyle); 9] = [
            ("comment, punctuation.definition.comment", Token::Comment, SyntectStyle::ITALIC),
            ("string, punctuation.definition.string", Token::String, SyntectStyle::empty()),
            (
                "constant.numeric, constant.language, constant.character, constant.other",
                Token::Constant,
                SyntectStyle::empty(),
            ),
            (
                "keyword, storage.type, storage.modifier, keyword.operator.new",
                Token::Keyword,
                SyntectStyle::empty(),
            ),
            (
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
                 entity.name.trait, entity.name.union, entity.name.tag, support.type, \
                 support.class, entity.other.inherited-class",
                Token::Type,
                SyntectStyle::empty(),
            ),
            (
                "entity.name.function, support.function, variable.function, \
                 entity.other.attribute-name, meta.function-call.generic",
                Token::Function,
                SyntectStyle::empty(),
            ),
            (
                "punctuation, keyword.operator, keyword.other.unit",
                Token::Punctuation,
                SyntectStyle::empty(),
            ),
            ("markup.heading", Token::Keyword, SyntectStyle::BOLD),
            ("invalid", Token::Invalid, SyntectStyle::empty()),
        ];
        let scopes = rules
            .into_iter()
            .filter_map(|(selector, token, style)| {
                let scope = ScopeSelectors::from_str(selector).ok()?;
                Some(ThemeItem {
                    scope,
                    style: StyleModifier {
                        foreground: Some(token.as_color()),
                        background: None,
                        font_style: Some(style),
                    },
                })
            })
            .collect();
        let mut theme = ScopeTheme::default();
        theme.settings.foreground = Some(Token::Plain.as_color());
        theme.scopes = scopes;
        theme
    });
    &THEME
}

/// The spans of every line of `text` under `syntax`, one `Vec` per line, in the order of
/// `text.split('\n')` (so a trailing newline yields a last, empty line).
///
/// Borrows the text: each line goes to the parser in place, newline included, as the
/// grammars expect. A line past [`LINE_MAX`] bytes is one plain span. Parsing state carries
/// from line to line (a block comment stays a comment), so the text must be whole.
#[must_use]
pub fn spans(text: &str, syntax: Syntax) -> Vec<Vec<Span>> {
    let set = syntaxes();
    let mut highlighter = syntect::easy::HighlightLines::new(syntax.0, scope_theme());
    let plain = |len: usize| vec![Span { len, token: Token::Plain, italic: false, bold: false }];
    let mut out: Vec<Vec<Span>> = text
        .split_inclusive('\n')
        .map(|line| {
            let body = line.strip_suffix('\n').unwrap_or(line);
            if body.len() > LINE_MAX {
                return plain(body.len());
            }
            let Ok(ranges) = highlighter.highlight_line(line, set) else {
                return plain(body.len());
            };
            let mut spans: Vec<Span> = Vec::with_capacity(ranges.len());
            let mut seen = 0_usize;
            for (style, piece) in ranges {
                // The last range carries the newline; keep the line's own bytes.
                let len = piece.len().min(body.len().saturating_sub(seen));
                if len == 0 {
                    continue;
                }
                seen = seen.saturating_add(len);
                let span = Span {
                    len,
                    token: Token::from_color(style.foreground),
                    italic: style.font_style.contains(SyntectStyle::ITALIC),
                    bold: style.font_style.contains(SyntectStyle::BOLD),
                };
                match spans.last_mut() {
                    Some(last)
                        if last.token == span.token
                            && last.italic == span.italic
                            && last.bold == span.bold =>
                    {
                        last.len = last.len.saturating_add(len);
                    }
                    _ => spans.push(span),
                }
            }
            spans
        })
        .collect();
    // `split('\n')` has one more line than `split_inclusive` when the text ends with a
    // newline (and one line for an empty text).
    if text.is_empty() || text.ends_with('\n') {
        out.push(Vec::new());
    }
    out
}

/// GPUI runs for one line's spans: the theme's colour per token, `font` in the italic or
/// bold face where the spans ask. A line with no spans (not parsed yet) is one plain run.
#[must_use]
pub fn runs(line_len: usize, spans: Option<&[Span]>, font: &Font, theme: &Theme) -> Vec<TextRun> {
    let plain = |len: usize| TextRun {
        len,
        font: font.clone(),
        color: Token::Plain.color(theme),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let Some(spans) = spans else { return vec![plain(line_len)] };
    let mut out: Vec<TextRun> = spans
        .iter()
        .map(|span| {
            let mut run_font = font.clone();
            if span.italic {
                run_font.style = FontStyle::Italic;
            }
            if span.bold {
                run_font.weight = FontWeight::BOLD;
            }
            TextRun {
                len: span.len,
                font: run_font,
                color: span.token.color(theme),
                background_color: None,
                underline: None,
                strikethrough: None,
            }
        })
        .collect();
    // The runs must cover the text exactly: pad or trim to the line's length.
    let covered: usize = out.iter().map(|r| r.len).sum();
    if covered < line_len {
        out.push(plain(line_len.saturating_sub(covered)));
    } else if covered > line_len {
        let mut left = line_len;
        out.retain_mut(|r| {
            if left == 0 {
                return false;
            }
            r.len = r.len.min(left);
            left = left.saturating_sub(r.len);
            true
        });
    }
    out
}

/// The coloured byte ranges of a fenced block's `code` under the grammar `lang` names.
///
/// For gpui-kit's Markdown view: plain runs are left out (the block's own colour shows), so
/// an unknown language yields nothing.
#[must_use]
pub fn code_ranges(code: &str, lang: &str, theme: &Theme) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(syntax) = Syntax::for_token(lang) else { return Vec::new() };
    let mut out = Vec::new();
    let mut at = 0_usize;
    for (line, spans) in code.split('\n').zip(spans(code, syntax)) {
        let mut from = at;
        for span in spans {
            let to = from.saturating_add(span.len);
            if span.token != Token::Plain || span.italic || span.bold {
                out.push((
                    from..to,
                    HighlightStyle {
                        color: Some(span.token.color(theme)),
                        font_style: span.italic.then_some(FontStyle::Italic),
                        font_weight: span.bold.then_some(FontWeight::BOLD),
                        ..HighlightStyle::default()
                    },
                ));
            }
            from = to;
        }
        // The line and the newline the split took.
        at = at.saturating_add(line.len()).saturating_add(1);
    }
    out
}

/// The code-block highlighter for gpui-kit's Markdown view, on `theme`'s colours: installed
/// by [`crate::kit::sync`], so an answer's fenced code reads like a file card.
pub fn code_block(
    theme: Theme,
) -> impl Fn(&CodeBlock) -> Vec<(Range<usize>, HighlightStyle)> + Send + Sync + 'static {
    move |block| {
        let Some(lang) = block.lang() else { return Vec::new() };
        code_ranges(block.code().as_ref(), lang.as_ref(), &theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grammar by fence tag, as a test result rather than a panic.
    fn grammar(tag: &str) -> Result<Syntax, String> {
        Syntax::for_token(tag).ok_or_else(|| format!("no grammar for {tag:?}"))
    }

    /// One line's spans as (text, token) pairs.
    fn tokens(line: &str, syntax: Syntax) -> Vec<(&str, Token)> {
        let mut at = 0_usize;
        spans(line, syntax)
            .into_iter()
            .next()
            .unwrap_or_default()
            .iter()
            .filter_map(|s| {
                let end = at.saturating_add(s.len);
                let text = line.get(at..end)?;
                at = end;
                Some((text, s.token))
            })
            .collect()
    }

    /// The token of the first span holding `word`.
    fn token_of(line: &str, syntax: Syntax, word: &str) -> Option<Token> {
        tokens(line, syntax).into_iter().find(|(t, _)| t.contains(word)).map(|(_, tok)| tok)
    }

    #[test]
    fn rust_by_extension_colours_keywords_strings_comments_and_numbers() -> Result<(), String> {
        assert_eq!(Syntax::for_path("/w/src/main.rs", "").map(Syntax::name), Some("Rust"));
        let rust = grammar("rust")?;
        let line = r#"fn main() { let x = "hi"; // said"#;
        assert_eq!(token_of(line, rust, "fn"), Some(Token::Keyword));
        assert_eq!(token_of(line, rust, "let"), Some(Token::Keyword));
        assert_eq!(token_of(line, rust, "main"), Some(Token::Function));
        assert_eq!(token_of(line, rust, "hi"), Some(Token::String));
        assert_eq!(token_of(line, rust, "said"), Some(Token::Comment));
        assert_eq!(token_of("let n = 42;", rust, "42"), Some(Token::Constant));
        assert_eq!(token_of("struct Foo;", rust, "Foo"), Some(Token::Type));
        // Spans cover the line exactly and merge equal neighbours.
        let first = spans(line, rust).into_iter().next().unwrap_or_default();
        assert_eq!(first.iter().map(|s| s.len).sum::<usize>(), line.len());
        assert!(first.windows(2).all(|w| {
            w.first().zip(w.get(1)).is_none_or(|(a, b)| a.token != b.token || a.italic != b.italic)
        }));
        Ok(())
    }

    #[test]
    fn a_block_comment_carries_across_lines_and_lines_match_the_split() -> Result<(), String> {
        let rust = grammar("rust")?;
        let text = "/* one\ntwo */ let\n";
        let all = spans(text, rust);
        assert_eq!(all.len(), text.split('\n').count(), "one span list per split line");
        assert_eq!(all.get(1).and_then(|l| l.first()).map(|s| s.token), Some(Token::Comment));
        assert!(all.get(1).is_some_and(|l| l.iter().any(|s| s.token == Token::Keyword)), "{all:?}");
        assert_eq!(all.get(2).map(Vec::len), Some(0), "the empty last line has no spans");
        assert_eq!(spans("", rust).len(), 1, "an empty text is one empty line");
        assert_eq!(spans("a\nb", rust).len(), 2);
        Ok(())
    }

    #[test]
    fn syntaxes_come_by_name_first_line_or_fence_and_a_long_line_stays_plain() -> Result<(), String>
    {
        assert_eq!(Syntax::for_path("Makefile", "").map(Syntax::name), Some("Makefile"));
        assert_eq!(
            Syntax::for_path("run", "#!/bin/bash").map(Syntax::name),
            Some("Bourne Again Shell (bash)")
        );
        assert!(Syntax::for_path("notes.unknownext", "plain words").is_none());
        assert!(
            Syntax::for_path("notes.txt", "plain words").is_none(),
            "plain text is not a colouring"
        );
        assert!(Syntax::for_token("txt").is_none());
        assert_eq!(Syntax::for_token("rust").map(Syntax::name), Some("Rust"));
        assert_eq!(Syntax::for_token("sh").map(Syntax::name), Some("Bourne Again Shell (bash)"));
        assert_eq!(Syntax::for_token("json").map(Syntax::name), Some("JSON"));
        assert!(Syntax::for_token("").is_none());
        let rust = grammar("rs")?;
        let long = "x".repeat(LINE_MAX.saturating_add(1));
        assert_eq!(
            spans(&long, rust).first().map(Vec::as_slice),
            Some(&[Span { len: long.len(), token: Token::Plain, italic: false, bold: false }][..])
        );
        Ok(())
    }

    #[test]
    fn a_fenced_block_yields_byte_ranges_across_its_lines_and_skips_plain_text() {
        let theme = Theme::default();
        let code = "let a = 1;\n// two\nfn b() {}";
        let ranges = code_ranges(code, "rust", &theme);
        let at = |word: &str| code.find(word);
        let find = |word: &str| {
            ranges.iter().find(|(r, _)| Some(r.start) == at(word)).map(|(r, s)| (r.clone(), *s))
        };
        let colour = |word: &str| find(word).and_then(|(_, s)| s.color);
        assert_eq!(colour("let"), Some(Token::Keyword.color(&theme)), "{ranges:?}");
        assert_eq!(colour("1"), Some(Token::Constant.color(&theme)));
        assert_eq!(find("// two").and_then(|(_, s)| s.font_style), Some(FontStyle::Italic));
        assert_eq!(colour("fn"), Some(Token::Keyword.color(&theme)));
        assert_eq!(find("b").map(|(r, _)| r.len()), Some(1));
        assert!(ranges.iter().all(|(r, _)| r.end <= code.len()));
        assert!(
            ranges.iter().all(|(_, s)| s.color != Some(Token::Plain.color(&theme))),
            "plain runs are left out"
        );
        assert!(code_ranges(code, "", &theme).is_empty(), "no language, no colours");
        assert!(code_ranges(code, "nosuchlang", &theme).is_empty());
    }

    /// The numbers behind the background parse (`docs/MEASUREMENTS.md`): the grammar load
    /// on first use, then a full card of Rust, then the same text again.
    #[test]
    #[ignore = "timing, run by hand with --ignored --nocapture"]
    fn timing_of_a_full_card() -> Result<(), String> {
        let text = include_str!("canvas.rs");
        let t0 = std::time::Instant::now();
        let rust = grammar("rust")?;
        let load = t0.elapsed();
        let t1 = std::time::Instant::now();
        let first = spans(text, rust);
        let parse = t1.elapsed();
        let t2 = std::time::Instant::now();
        let _again = spans(text, rust);
        let again = t2.elapsed();
        let code = text.lines().take(120).collect::<Vec<_>>().join("\n");
        let t3 = std::time::Instant::now();
        let _ranges = code_ranges(&code, "rust", &Theme::default());
        let block = t3.elapsed();
        println!(
            "grammars {load:?}; {} lines parsed {parse:?}, again {again:?}; a {}-byte fenced block {block:?}; {} spans",
            first.len(),
            code.len(),
            first.iter().map(Vec::len).sum::<usize>()
        );
        Ok(())
    }

    #[test]
    fn runs_cover_the_line_exactly_with_the_theme_colours() {
        let theme = Theme::default();
        let font = gpui::font("JetBrains Mono");
        let spans = [
            Span { len: 2, token: Token::Keyword, italic: false, bold: false },
            Span { len: 3, token: Token::Comment, italic: true, bold: false },
        ];
        let padded = runs(7, Some(&spans), &font, &theme);
        assert_eq!(padded.iter().map(|r| r.len).sum::<usize>(), 7, "padded to the line");
        assert_eq!(padded.first().map(|r| r.color), Some(Token::Keyword.color(&theme)));
        assert_eq!(padded.get(1).map(|r| r.font.style), Some(FontStyle::Italic));
        assert_eq!(padded.get(2).map(|r| r.color), Some(Token::Plain.color(&theme)));
        let trimmed = runs(3, Some(&spans), &font, &theme);
        assert_eq!(trimmed.iter().map(|r| r.len).sum::<usize>(), 3, "trimmed to the line");
        assert_eq!(runs(4, None, &font, &theme).len(), 1, "unparsed: one plain run");
    }
}
