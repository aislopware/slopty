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

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight};

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
#[must_use]
pub fn terminal_font(family: &str, bold: bool, italic: bool) -> Font {
    Font {
        family: family.to_owned().into(),
        features: FontFeatures::default(),
        fallbacks: Some(terminal_fallbacks()),
        weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
        style: if italic { FontStyle::Italic } else { FontStyle::Normal },
    }
}
