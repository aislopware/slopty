// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Math fonts compiled into the binary (feature `bundled`).
//!
//! Two faces, each falling back to STIX Two Math for what it lacks. A face loads on first use
//! and is shared, so every host and thread uses the same parsed font and
//! caches; faces nobody picks are never parsed (their bytes stay untouched
//! pages of the binary).
//!
//! [`layout`] numbers glyph fonts against one fixed table ([`FONTS`],
//! [`font_at`]) rather than the chosen face's fallback chain, so a host can
//! resolve a glyph's font without knowing which face laid it out.

use std::sync::{Arc, OnceLock};

use crate::{Formula, Item, LayoutError, MathFont, Options};

/// The bundled faces apps can pick between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Face {
    /// Fira Math: a sans face that sits beside UI sans-serifs.
    #[default]
    Fira,
    /// STIX Two Math: Times-like; covers everything, so it backs every face.
    StixTwo,
    // New Computer Modern, Libertinus, Pagella and Euler are not bundled: nothing
    // picks a face but Fira, and STIX Two backs it.
}

impl Face {
    /// Every face, in table order.
    pub const ALL: [Self; 2] = [Self::Fira, Self::StixTwo];

    /// The face's own font in the [`font_at`] table. Append-only, so an
    /// index a host stored or sent across FFI never changes meaning.
    pub const fn index(self) -> u8 {
        self as u8
    }

    const fn data(self) -> &'static [u8] {
        match self {
            Self::Fira => include_bytes!("../fonts/FiraMath-Regular.otf"),
            Self::StixTwo => include_bytes!("../fonts/STIXTwoMath-Regular.otf"),
        }
    }

    /// The face's font, without fallback.
    #[expect(
        clippy::expect_used,
        reason = "the bundled fonts are bytes fixed at build time that every test run parses, \
                  so one that fails is a broken build rather than a case to handle"
    )]
    fn parse(self) -> MathFont {
        MathFont::new(self.data()).expect("a bundled math font parses")
    }
}

/// How many fonts [`layout`] can name: one per [`Face`], by [`Face::index`].
#[expect(clippy::cast_possible_truncation, reason = "a handful of faces")]
pub const FONTS: u8 = Face::ALL.len() as u8;

fn stix_arc() -> &'static Arc<MathFont> {
    static FONT: OnceLock<Arc<MathFont>> = OnceLock::new();
    FONT.get_or_init(|| Arc::new(Face::StixTwo.parse()))
}

/// The font for `face` (with its fallback chain), loaded on first use.
pub fn font(face: Face) -> &'static MathFont {
    static FONTS: [OnceLock<MathFont>; Face::ALL.len()] =
        [const { OnceLock::new() }; Face::ALL.len()];
    if face == Face::StixTwo {
        return stix_arc();
    }
    let Some(slot) = FONTS.get(usize::from(face.index())) else {
        return stix_arc();
    };
    slot.get_or_init(|| face.parse().with_fallback(Arc::clone(stix_arc())))
}

/// The font a [`layout`]ed glyph's `font` index names (see [`FONTS`]): that
/// face's own font, whose [`MathFont::data`] and [`MathFont::outline`] are
/// its own. Out-of-range indices name STIX Two.
pub fn font_at(index: u8) -> &'static MathFont {
    match Face::ALL.get(index as usize) {
        Some(&face) => font(face),
        None => stix_arc(),
    }
}

/// [`crate::layout()`] in a bundled face, with glyph font indices into the
/// fixed table [`font_at`] resolves — the same for every face.
pub fn layout(source: &str, face: Face, options: &Options) -> Result<Formula, LayoutError> {
    let mut formula = crate::layout(source, font(face), options)?;
    // A chain is the face then STIX Two (STIX Two's is just itself).
    let table = |chain: u8| match (chain, face) {
        (0, face) => face.index(),
        _ => Face::StixTwo.index(),
    };
    for item in &mut formula.items {
        if let Item::Glyph { font, .. } = item {
            *font = table(*font);
        }
    }
    Ok(formula)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_resolves_through_one_table() {
        let names: Vec<String> =
            Face::ALL.iter().map(|&f| font_at(f.index()).name().unwrap()).collect();
        for (name, part) in names.iter().zip(["Fira", "STIX"]) {
            assert!(name.contains(part), "{names:?}");
        }
        let src = r"\mathcal{L} = \frac{a+b}{c-d} + \mathfrak{g}";
        let opts = Options::inline(16.0);
        for face in Face::ALL {
            let f = layout(src, face, &opts).unwrap();
            let raw = crate::layout(src, font(face), &opts).unwrap();
            assert_eq!(f.items.len(), raw.items.len());
            for (item, raw) in f.items.iter().zip(&raw.items) {
                if let (Item::Glyph { font: i, glyph, .. }, Item::Glyph { font: r, glyph: g, .. }) =
                    (item, raw)
                {
                    assert!(*i < FONTS);
                    assert_eq!(glyph, g);
                    assert_eq!(
                        font_at(*i).data().as_ptr(),
                        font(face).at(*r).data().as_ptr(),
                        "{face:?}"
                    );
                }
            }
        }
        // Fira has no script letters: they come from STIX Two.
        let f = layout(r"\mathcal{L}", Face::Fira, &opts).unwrap();
        assert!(matches!(f.items[0], Item::Glyph { font: 1, .. }));
        // Its own letters stay its own.
        let f = layout("x", Face::Fira, &opts).unwrap();
        assert!(matches!(f.items[0], Item::Glyph { font, .. } if font == Face::Fira.index()));
    }

    #[test]
    fn corpus_lays_out_in_every_bundled_face() {
        for face in Face::ALL {
            for src in crate::tests::CORPUS {
                for opts in [Options::inline(16.0), Options::display(16.0)] {
                    let f =
                        layout(src, face, &opts).unwrap_or_else(|e| panic!("{face:?}: {src}: {e}"));
                    assert!(f.width > 0.0 && f.height() > 0.0, "{face:?}: {src}");
                }
            }
        }
    }
}
