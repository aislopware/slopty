//! Bundled fonts.
//!
//! The terminal face and the symbol fallback ship inside the binary so every client (macOS,
//! iOS) renders the same cell geometry and the same powerline/devicon glyphs, whatever the
//! system has installed.
//!
//! * `JetBrains Mono` 2.304 — SIL OFL 1.1 (`assets/fonts/LICENSE-JetBrainsMono.txt`).
//! * `Symbols Nerd Font Mono` 3.x — MIT + per-glyph-set licences
//!   (`assets/fonts/LICENSE-SymbolsNerdFont.txt`).

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Pixels, px};

/// The bundled monospace family.
pub const MONO_FAMILY: &str = "JetBrains Mono";
/// The bundled symbol family (powerline, devicons, box drawing extras).
pub const SYMBOLS_FAMILY: &str = "Symbols Nerd Font Mono";

#[expect(
    clippy::large_include_file,
    reason = "the symbol font is 2.6 MB by nature; bundled on purpose"
)]
const FILES: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf"),
];

/// Register the bundled fonts with the text system. Call once at startup, before any window
/// opens.
pub fn install(cx: &App) -> anyhow::Result<()> {
    cx.text_system().add_fonts(FILES.iter().map(|f| Cow::Borrowed(*f)).collect())
}

/// Fallback chain used by every terminal run.
///
/// Symbols first (they never collide with text glyphs), then the platform monospace (CJK, misc)
/// and colour emoji. The platform appends its own cascade list after these.
#[must_use]
pub fn terminal_fallbacks() -> FontFallbacks {
    FontFallbacks(Arc::new(vec![
        SYMBOLS_FAMILY.to_owned(),
        "Menlo".to_owned(),
        "Apple Color Emoji".to_owned(),
    ]))
}

/// A terminal font for `family` with the given weight and style, carrying the fallback chain.
/// Without `ligatures` the font's `calt` is off, so `=>` stays two glyphs.
#[must_use]
pub fn terminal_font(family: &str, bold: bool, italic: bool, ligatures: bool) -> Font {
    Font {
        family: family.to_owned().into(),
        features: if ligatures {
            FontFeatures::default()
        } else {
            FontFeatures::disable_ligatures()
        },
        fallbacks: Some(terminal_fallbacks()),
        weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
        style: if italic { FontStyle::Italic } else { FontStyle::Normal },
    }
}

/// The rung of the raster ladder nearest to `size`: the sizes text in motion is drawn from.
///
/// Eight rungs per octave. A zoom step lands within ±4.5 % of a rung, so a pinch from 30 % to
/// 200 % rasterises a glyph at ~22 sizes instead of one per step, and the atlas stops growing
/// with the number of steps.
#[must_use]
pub fn raster_rung(size: Pixels) -> Pixels {
    let size = f32::from(size).max(1.0);
    px(((size.log2() * 8.0).round() / 8.0).exp2())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_has_eight_rungs_an_octave_and_sizes_snap_to_the_nearest() {
        let rung = |s: f32| f32::from(raster_rung(px(s)));
        assert!((rung(16.0) - 16.0).abs() < 1e-4, "{}", rung(16.0));
        assert!((rung(32.0) - 32.0).abs() < 1e-4);
        // Between rungs a size moves to the nearest, never further than half a rung (4.4 %).
        for s in [13.0_f32, 13.4, 5.3, 27.9, 41.0, 100.0] {
            let r = rung(s);
            assert!((r / s).ln().abs() <= (2.0_f32).ln() / 16.0 + 1e-6, "{s} → {r}");
        }
        assert_eq!(rung(13.0).to_bits(), rung(13.4).to_bits(), "a 3 % zoom step keeps the rung");
        assert_eq!(rung(1.0).to_bits(), rung(0.2).to_bits(), "nothing below one pixel");
    }
}
