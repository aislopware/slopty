//! A single grid cell.

use core::fmt;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::Style;

/// Inline storage for cell text. Almost every cell is one BMP scalar (≤ 3 bytes); a grapheme
/// cluster with ZWJ sequences can be much longer and spills to the heap.
const INLINE_BYTES: usize = 8;

/// The text of one cell: a single grapheme cluster as segmented by the engine.
///
/// Invariant: always valid UTF-8. Empty text means a blank cell.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct CellText(SmallVec<[u8; INLINE_BYTES]>);

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
    pub const EMPTY: Self = Self(SmallVec::new_const());

    /// From a single scalar.
    #[must_use]
    pub fn from_char(c: char) -> Self {
        let mut buf = [0_u8; 4];
        Self(SmallVec::from_slice(c.encode_utf8(&mut buf).as_bytes()))
    }

    /// From a grapheme cluster. The caller guarantees `s` is one cluster; this type does not
    /// segment.
    #[must_use]
    pub fn from_cluster(s: &str) -> Self {
        Self(SmallVec::from_slice(s.as_bytes()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every constructor takes `&str`/`char` and deserialisation visits a `str`, so the bytes
        // are always valid UTF-8; the fallback exists so this can never be a source of UB.
        core::str::from_utf8(&self.0).unwrap_or("\u{FFFD}")
    }

    /// True for a blank cell.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when the text is a single ASCII byte (the overwhelmingly common case, and the one
    /// renderers fast-path).
    #[must_use]
    pub fn as_ascii(&self) -> Option<u8> {
        match self.0.as_slice() {
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
