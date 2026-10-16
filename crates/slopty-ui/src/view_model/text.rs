//! Words from an agent or a machine made safe to show: what would hide or reorder text shown as a
//! visible escape (`[U+202E]`), and a line cut to what a row holds.
//!
//! An approval shows what it allows exactly as it runs: a right-to-left override or a zero-width
//! character in a command could make it read as something else, so each one shows as its code
//! (Claude desktop does the same).

use std::fmt::Write as _;

/// Whether `c` reorders the text around it: the bidirectional marks, embeddings, overrides and
/// isolates.
const fn bidi(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Whether `c` draws nothing where it stands: Unicode's default-ignorable characters, the
/// bidirectional ones left to [`bidi`].
const fn ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'..='\u{2065}'
            | '\u{206A}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// Whether `c` is hidden or moves text: a control other than a line break or a tab, a bidi
/// character, or a default-ignorable one.
const fn hidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || bidi(c) || ignorable(c)
}

/// `text` with every hidden character (a control, a bidi or a default-ignorable one) shown as
/// its code, `[U+202E]`.
#[must_use]
pub fn escaped(text: &str) -> String {
    if !text.chars().any(hidden) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len().saturating_add(16));
    for c in text.chars() {
        if hidden(c) {
            let _written = write!(out, "[U+{:04X}]", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `c` joins the character before it into one that is seen as one: a combining mark,
/// a variation selector, a skin tone, a zero-width joiner.
const fn extends(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{200D}'
            | '\u{1F3FB}'..='\u{1F3FF}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// `text` as one line of at most `max` characters as seen.
///
/// A letter with its accents and an emoji with its joins count once; its whitespace runs one
/// space each, its controls and bidi characters are taken out, and an ellipsis ends it where it
/// was cut.
#[must_use]
pub fn line(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut seen = 0_usize;
    let mut space = false;
    let mut joined = false;
    for c in text.trim().chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control() || bidi(c) {
            continue;
        }
        let starts = out.is_empty() || !(extends(c) || joined);
        let needed = usize::from(starts).saturating_add(usize::from(space));
        if seen.saturating_add(needed) > max {
            out.push('…');
            return out;
        }
        seen = seen.saturating_add(needed);
        if space {
            out.push(' ');
            space = false;
        }
        joined = c == '\u{200D}';
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A right-to-left override, a zero-width space and a terminal escape show as their codes;
    /// line breaks, tabs and plain letters stay.
    #[test]
    fn hidden_characters_show_as_their_codes() {
        assert_eq!(escaped("rm -rf /tmp/\u{202E}gpj.sh"), "rm -rf /tmp/[U+202E]gpj.sh");
        assert_eq!(escaped("a\u{200B}b\u{1b}[2J"), "a[U+200B]b[U+001B][2J");
        assert_eq!(escaped("cargo test\n\tok"), "cargo test\n\tok");
        assert_eq!(escaped("héllo"), "héllo");
    }

    /// A line keeps what is seen, one space between words, and is cut by what the eye counts.
    #[test]
    fn a_line_is_cut_by_what_is_seen() {
        assert_eq!(line("  run\n  cargo\ttest  ", 60), "run cargo test");
        assert_eq!(line("abc\u{202E}def", 60), "abcdef", "bidi taken out");
        assert_eq!(line("abcdef", 3), "abc…");
        assert_eq!(line("e\u{301}e\u{301}e\u{301}", 2), "e\u{301}e\u{301}…", "accents count once");
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(line(&format!("{family}{family}"), 1), format!("{family}…"));
        assert_eq!(line("ab cd", 4), "ab c…");
        assert_eq!(line("", 3), "");
    }
}
