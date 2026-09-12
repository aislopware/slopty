//! A single grid cell.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::Style;

/// Inline storage for cell text. Almost every cell is one BMP scalar (≤ 3 bytes); the inline
/// buffer takes any cluster up to this many bytes, and the rare longer one (a ZWJ emoji
/// sequence) goes to the heap. Sized so the whole type stays at 24 bytes.
const INLINE_BYTES: usize = 22;

/// The text of one cell: a single grapheme cluster as segmented by the engine.
///
/// Invariant: always valid UTF-8. Empty text means a blank cell. A clone is a copy unless the
/// cluster is on the heap: the client copies every row it receives into its line cache and a
/// flood of output made that copy a visible share of the frame.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CellText(Repr);

/// Canonical: `Inline` for anything that fits (unused bytes zero), `Heap` only past the inline
/// size, so the derived equality and hash are by content.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Repr {
    Inline { len: u8, bytes: [u8; INLINE_BYTES] },
    Heap(Box<str>),
}

impl Default for CellText {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Serialize for CellText {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CellText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = CellText;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a grapheme cluster string")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(CellText::from_cluster(v))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

impl CellText {
    /// A blank cell.
    pub const EMPTY: Self = Self(Repr::Inline { len: 0, bytes: [0; INLINE_BYTES] });

    /// From a single scalar.
    #[must_use]
    pub fn from_char(c: char) -> Self {
        let mut buf = [0_u8; 4];
        Self::from_cluster(c.encode_utf8(&mut buf))
    }

    /// From a grapheme cluster. The caller guarantees `s` is one cluster; this type does not
    /// segment.
    #[must_use]
    pub fn from_cluster(s: &str) -> Self {
        let src = s.as_bytes();
        if let Ok(len) = u8::try_from(src.len())
            && src.len() <= INLINE_BYTES
        {
            let mut bytes = [0_u8; INLINE_BYTES];
            if let Some(dst) = bytes.get_mut(..src.len()) {
                dst.copy_from_slice(src);
            }
            return Self(Repr::Inline { len, bytes });
        }
        Self(Repr::Heap(Box::from(s)))
    }

    fn bytes(&self) -> &[u8] {
        match &self.0 {
            Repr::Inline { len, bytes } => bytes.get(..usize::from(*len)).unwrap_or(&[]),
            Repr::Heap(s) => s.as_bytes(),
        }
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            // Every constructor takes `&str`/`char` and deserialisation visits a `str`, so the
            // bytes are always valid UTF-8; the fallback exists so this can never be a source
            // of UB.
            Repr::Inline { .. } => core::str::from_utf8(self.bytes()).unwrap_or("\u{FFFD}"),
            Repr::Heap(s) => s,
        }
    }

    /// True for a blank cell.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes().len()
    }

    /// True when the text is a single ASCII byte (the overwhelmingly common case, and the one
    /// renderers fast-path).
    #[must_use]
    pub fn as_ascii(&self) -> Option<u8> {
        match self.bytes() {
            [b] if b.is_ascii() => Some(*b),
            _ => None,
        }
    }
}

impl fmt::Debug for CellText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl From<char> for CellText {
    fn from(c: char) -> Self {
        Self::from_char(c)
    }
}

impl From<&str> for CellText {
    fn from(s: &str) -> Self {
        Self::from_cluster(s)
    }
}

/// How many columns a cell occupies, as decided by the engine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum CellWidth {
    /// One column.
    #[default]
    Narrow,
    /// Two columns; the following cell is a [`CellWidth::SpacerTail`].
    Wide,
    /// The second column of a wide character. Carries no text of its own.
    SpacerTail,
    /// A blank cell inserted at the end of a line when a wide character did not fit and wrapped.
    SpacerHead,
}

impl CellWidth {
    /// Number of columns this cell contributes when advancing.
    #[must_use]
    pub const fn columns(self) -> u16 {
        match self {
            Self::Narrow | Self::SpacerHead | Self::SpacerTail => 1,
            Self::Wide => 2,
        }
    }

    /// True when the cell should draw its own text.
    #[must_use]
    pub const fn draws_text(self) -> bool {
        matches!(self, Self::Narrow | Self::Wide)
    }
}

/// One grid cell.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Cell {
    /// The grapheme cluster shown, or empty.
    pub text: CellText,
    /// Styling.
    pub style: Style,
    /// Column span.
    pub width: CellWidth,
}

impl Cell {
    /// A blank, unstyled cell.
    pub const BLANK: Self =
        Self { text: CellText::EMPTY, style: Style::DEFAULT, width: CellWidth::Narrow };

    /// A narrow cell with the given scalar and style.
    #[must_use]
    pub fn narrow(c: char, style: Style) -> Self {
        Self { text: CellText::from_char(c), style, width: CellWidth::Narrow }
    }

    /// A wide cell (the caller appends the spacer tail).
    #[must_use]
    pub fn wide(text: &str, style: Style) -> Self {
        Self { text: CellText::from_cluster(text), style, width: CellWidth::Wide }
    }

    /// The spacer that follows a wide cell.
    #[must_use]
    pub const fn spacer_tail(style: Style) -> Self {
        Self { text: CellText::EMPTY, style, width: CellWidth::SpacerTail }
    }

    /// True when the cell is blank with default style: nothing to draw.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text.is_empty() && self.style.is_default() && self.width == CellWidth::Narrow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_fast_path() {
        assert_eq!(CellText::from_char('a').as_ascii(), Some(b'a'));
        assert_eq!(CellText::from_char('é').as_ascii(), None);
        assert_eq!(CellText::from_cluster("👨‍👩‍👧").as_ascii(), None);
        assert_eq!(CellText::EMPTY.as_ascii(), None);
    }

    #[test]
    fn a_cell_text_converts_debugs_and_rejects_a_non_string() {
        assert_eq!(CellText::from('a'), CellText::from_char('a'));
        assert_eq!(CellText::from("é"), CellText::from_cluster("é"));
        assert_eq!(CellText::from("é").as_str(), "é");
        assert_eq!(format!("{:?}", CellText::from("é")), "\"é\"");
        assert_eq!(CellText::from("é").as_ascii(), None, "a two-byte cluster is not ascii");
        let err = serde_json::from_str::<CellText>("5").unwrap_err().to_string();
        assert!(err.contains("a grapheme cluster string"), "{err}");
    }

    #[test]
    fn a_width_spans_its_columns() {
        assert_eq!(CellWidth::Narrow.columns(), 1);
        assert_eq!(CellWidth::SpacerHead.columns(), 1);
        assert_eq!(CellWidth::SpacerTail.columns(), 1);
        assert_eq!(CellWidth::Wide.columns(), 2);
    }

    #[test]
    fn long_clusters_spill_without_corruption() {
        let family = "👨‍👩‍👧‍👦"; // 25 bytes, past the inline budget
        let t = CellText::from_cluster(family);
        assert_eq!(t.as_str(), family);
        assert_eq!(t.len(), family.len());
    }

    #[test]
    fn blank_detection() {
        assert!(Cell::BLANK.is_blank());
        assert!(!Cell::narrow(' ', Style::DEFAULT).is_blank(), "a space is text, not blank");
        let mut styled = Cell::BLANK;
        styled.style.bg = Color::Palette(1);
        assert!(!styled.is_blank(), "a coloured background must be painted");
    }

    #[test]
    fn text_survives_serde() {
        let t = CellText::from_cluster("ﬁ");
        let json = serde_json::to_string(&t).unwrap();
        let back: CellText = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    use crate::Color;
}

#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn a_cell_text_is_three_words_and_round_trips_any_cluster() {
        assert_eq!(size_of::<CellText>(), 24);
        for s in [
            "",
            "a",
            "é",
            "日",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦",
            &"x".repeat(INLINE_BYTES),
            &"y".repeat(INLINE_BYTES + 1),
        ] {
            let t = CellText::from_cluster(s);
            assert_eq!(t.as_str(), s);
            assert_eq!(t.len(), s.len());
            assert_eq!(t.is_empty(), s.is_empty());
            assert_eq!(t.clone(), t);
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(serde_json::from_str::<CellText>(&json).unwrap(), t);
        }
        assert_eq!(CellText::from_char('a').as_ascii(), Some(b'a'));
        assert_eq!(CellText::from_cluster("ab").as_ascii(), None);
        assert_eq!(CellText::from_cluster("é").as_ascii(), None);
        assert_eq!(CellText::EMPTY, CellText::default());
    }
}
