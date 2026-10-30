//! Terminal output in a code fence, in its colours: a fence that carries ANSI SGR escapes (an
//! ` ```ansi ` fence, or console output pasted with its escape bytes) draws each run in the
//! colour its escapes chose, from the terminal theme, the way the terminal showed it. The escapes
//! themselves never show and never reach the clipboard: copy takes the plain text.
//!
//! Only SGR (`ESC [ … m`) sets a style; every other control sequence (cursor moves, erases, OSC
//! titles and links) is dropped. A real escape byte is read in any fence; an ` ```ansi ` fence
//! also reads the spelled-out forms an agent writes (`\x1b[`, `\033[`, `\e[`, `\u001b[`), which
//! in any other fence are code and stay as written.

use std::ops::Range;

use gpui::{FontStyle, FontWeight, Hsla, TextRun, UnderlineStyle, px};

use crate::theme::Theme;

/// A colour an escape names: one of the 256 (the first 16 the theme's), or a true colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Index(u8),
    Rgb(u8, u8, u8),
}

/// The style a run of text was printed in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnsiStyle {
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// One line of output: its plain text, and its styled runs as byte ranges into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnsiLine {
    pub text: String,
    pub spans: Vec<(Range<usize>, AnsiStyle)>,
}

/// Whether a fence of `language` holding `code` is terminal output to draw in its colours.
pub fn is_ansi_fence(language: Option<&str>, code: &str) -> bool {
    declared(language) || code.contains('\u{1b}')
}

/// The fence says it is ANSI output.
fn declared(language: Option<&str>) -> bool {
    language.is_some_and(|l| l.trim().eq_ignore_ascii_case("ansi"))
}

/// The spelled-out escapes an ` ```ansi ` fence may hold, read as the byte itself.
const SPELLED: [&str; 5] = ["\\x1b", "\\x1B", "\\033", "\\e", "\\u001b"];

/// `code` with an ` ```ansi ` fence's spelled-out escapes made real.
fn unspelled(language: Option<&str>, code: &str) -> String {
    if !declared(language) {
        return code.to_owned();
    }
    let mut out = code.to_owned();
    for spelled in SPELLED {
        out = out.replace(&format!("{spelled}["), "\u{1b}[");
        out = out.replace(&format!("{spelled}]"), "\u{1b}]");
    }
    out
}

/// The fence's lines with their styles. The style carries across line breaks, as a terminal's
/// does.
pub fn parse(language: Option<&str>, code: &str) -> Vec<AnsiLine> {
    let code = unspelled(language, code);
    let mut style = AnsiStyle::default();
    let mut lines = Vec::new();
    for raw in code.split('\n') {
        let mut line = AnsiLine::default();
        let mut chars = raw.chars().peekable();
        let mut run_start = 0usize;
        let push = |line: &mut AnsiLine, start: usize, style: AnsiStyle| {
            let end = line.text.len();
            if end > start {
                line.spans.push((start..end, style));
            }
        };
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                if c != '\r' {
                    line.text.push(c);
                }
                continue;
            }
            match chars.peek().copied() {
                // CSI: parameters, then a final byte in @..~; only `m` styles.
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut last = None;
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            last = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if last == Some('m') {
                        let next = apply_sgr(style, &params);
                        if next != style {
                            push(&mut line, run_start, style);
                            run_start = line.text.len();
                            style = next;
                        }
                    }
                }
                // OSC: up to BEL or ESC \.
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other escape: the escape and the byte after it.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        }
        push(&mut line, run_start, style);
        lines.push(line);
    }
    lines
}

/// `style` after the SGR parameters `params` ("1;31", "38;5;208", "" = reset).
fn apply_sgr(mut style: AnsiStyle, params: &str) -> AnsiStyle {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect()
    };
    let mut at = 0;
    while at < codes.len() {
        let code = codes[at];
        match code {
            0 => style = AnsiStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            7 => style.inverse = true,
            21 | 22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            27 => style.inverse = false,
            30..=37 => style.fg = Some(index(code - 30)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(index(code - 40)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(index(code - 90 + 8)),
            100..=107 => style.bg = Some(index(code - 100 + 8)),
            38 | 48 => {
                let color = match codes.get(at + 1) {
                    Some(5) => {
                        let color = codes.get(at + 2).map(|n| index(*n));
                        at += 2;
                        color
                    }
                    Some(2) => {
                        let channel =
                            |ix: usize| codes.get(at + ix).map_or(0, |n| u8::try_from(*n).unwrap_or(u8::MAX));
                        let color = AnsiColor::Rgb(channel(2), channel(3), channel(4));
                        at += 4;
                        Some(color)
                    }
                    _ => None,
                };
                if code == 38 {
                    style.fg = color;
                } else {
                    style.bg = color;
                }
            }
            _ => {}
        }
        at += 1;
    }
    style
}

/// Colour `n` of the 256.
fn index(n: u16) -> AnsiColor {
    AnsiColor::Index(u8::try_from(n).unwrap_or(u8::MAX))
}

/// An escape's colour from the terminal theme.
fn color(color: AnsiColor, theme: &Theme) -> Hsla {
    match color {
        AnsiColor::Index(n) => crate::composer::ansi_color(n, theme),
        AnsiColor::Rgb(r, g, b) => {
            gpui::rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).into()
        }
    }
}

/// `line`'s text runs in `mono`, each in the colours and face its escapes chose; text with no
/// colour of its own is the code's plain colour.
pub fn runs(line: &AnsiLine, mono: &gpui::Font, theme: &Theme) -> Vec<TextRun> {
    line.spans
        .iter()
        .map(|(range, style)| {
            let (mut fg, mut bg) = (
                style.fg.map_or(theme.text, |c| color(c, theme)),
                style.bg.map(|c| color(c, theme)),
            );
            if style.inverse {
                let back = bg.unwrap_or(theme.bg);
                bg = Some(fg);
                fg = back;
            }
            if style.dim {
                fg = fg.opacity(0.6);
            }
            let mut font = mono.clone();
            if style.bold {
                font.weight = FontWeight::BOLD;
            }
            if style.italic {
                font.style = FontStyle::Italic;
            }
            TextRun {
                len: range.len(),
                font,
                color: fg,
                background_color: bg,
                underline: style.underline.then(|| UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                }),
                strikethrough: None,
            }
        })
        .filter(|run| run.len > 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `code` with every escape taken out, as the code block shows and copies it.
    fn strip(language: Option<&str>, code: &str) -> String {
        parse(language, code).into_iter().map(|line| line.text).collect::<Vec<_>>().join("\n")
    }

    /// A coloured prompt and a reset read as two runs over the plain text.
    #[test]
    fn escapes_become_styled_runs_over_plain_text() {
        let lines = parse(None, "\u{1b}[1;32m✓\u{1b}[0m passed\n\u{1b}[31merror\u{1b}[m: no");
        assert_eq!(lines[0].text, "✓ passed");
        assert_eq!(lines[0].spans.len(), 2);
        let (first, style) = &lines[0].spans[0];
        assert_eq!(&lines[0].text[first.clone()], "✓");
        assert!(style.bold);
        assert_eq!(style.fg, Some(AnsiColor::Index(2)));
        assert_eq!(lines[0].spans[1].1, AnsiStyle::default());
        assert_eq!(lines[1].text, "error: no");
        assert_eq!(lines[1].spans[0].1.fg, Some(AnsiColor::Index(1)));
    }

    /// 256 and true colours, bright colours, and a style that runs on past a line break.
    #[test]
    fn extended_colours_and_styles_across_lines() {
        let lines = parse(None, "\u{1b}[38;5;208ma\u{1b}[48;2;1;2;3mb\nc\u{1b}[95md");
        assert_eq!(lines[0].spans[0].1.fg, Some(AnsiColor::Index(208)));
        assert_eq!(lines[0].spans[1].1.bg, Some(AnsiColor::Rgb(1, 2, 3)));
        assert_eq!(lines[1].spans[0].1.fg, Some(AnsiColor::Index(208)), "it carries on");
        assert_eq!(lines[1].spans[1].1.fg, Some(AnsiColor::Index(13)));
    }

    /// Cursor moves, erases and OSC links leave nothing behind; copy takes the plain text.
    #[test]
    fn other_sequences_drop_and_copy_is_plain() {
        let code = "\u{1b}[2K\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{1b}\\ \u{1b}[1Gok\r";
        assert_eq!(strip(None, code), "link ok");
    }

    /// Spelled-out escapes are read only where the fence says it is ANSI.
    #[test]
    fn spelled_escapes_only_in_an_ansi_fence() {
        let code = r"\033[31mred\x1b[0m";
        assert!(!is_ansi_fence(Some("bash"), code));
        assert_eq!(strip(Some("bash"), code), code);
        assert!(is_ansi_fence(Some("ansi"), code));
        assert_eq!(strip(Some("ansi"), code), "red");
        assert!(is_ansi_fence(Some("console"), "\u{1b}[31mx"));
    }

    /// Each run takes its colour from the terminal theme, bold its face; inverse swaps.
    #[test]
    fn runs_take_the_terminal_theme() {
        let theme = Theme::dark();
        let mono = gpui::font("Geist Mono");
        let lines = parse(None, "\u{1b}[1;31mred\u{1b}[0m \u{1b}[7mrev");
        let runs = runs(&lines[0], &mono, &theme);
        assert_eq!(runs[0].color, theme.terminal.ansi[1]);
        assert_eq!(runs[0].font.weight, FontWeight::BOLD);
        assert_eq!(runs[1].color, theme.text);
        assert_eq!(runs[2].background_color, Some(theme.text));
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), "red rev".len());
    }
}
