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

/// Whether GPUI sets the system face as the system sets it: its optical size (Text under 20 pt,
/// Display from 20) and its size-specific tracking. Measured against Core Text's own UI font,
/// which applies both (`docs/decisions/ui.md`, "SF's optical size and tracking, measured").
#[cfg(test)]
mod optical_size {
    use std::ffi::c_void;

    use core_foundation::attributed_string::CFMutableAttributedString;
    use core_foundation::base::{CFRange, CFRelease, CFType, TCFType as _};
    use core_foundation::string::{CFString, CFStringRef};
    use gpui::{FontRun, px};

    /// A Core Text font or line, by its opaque reference.
    type CtRef = *const c_void;

    /// `kCTFontUIFontSystem` in `CTFontUIFontType` (`CoreText/CTFont.h`).
    const UI_FONT_SYSTEM: u32 = 2;

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        static kCTFontAttributeName: CFStringRef;
        fn CTFontCreateUIFontForLanguage(kind: u32, size: f64, language: CFStringRef) -> CtRef;
        fn CTFontCreateCopyWithAttributes(
            font: CtRef,
            size: f64,
            matrix: *const c_void,
            attributes: *const c_void,
        ) -> CtRef;
        fn CTFontCopyPostScriptName(font: CtRef) -> CFStringRef;
        fn CTLineCreateWithAttributedString(string: *const c_void) -> CtRef;
        fn CTLineGetTypographicBounds(
            line: CtRef,
            ascent: *mut f64,
            descent: *mut f64,
            leading: *mut f64,
        ) -> f64;
    }

    /// A Core Text font's PostScript name and the width it sets `text` at.
    fn core_text(font: CtRef, text: &str) -> (String, f64) {
        // SAFETY: `font` is a live CTFont (released by the caller); the name comes back under
        // the Create Rule.
        let name = unsafe { CTFontCopyPostScriptName(font) };
        // SAFETY: `name` was returned under the Create Rule and is wrapped once, which releases
        // it (Core Foundation's ownership rules).
        let name = unsafe { CFString::wrap_under_create_rule(name) };
        let mut string = CFMutableAttributedString::new();
        string.replace_str(&CFString::new(text), CFRange::init(0, 0));
        // SAFETY: `font` is a live CF object; the Get Rule wrap retains it for the string.
        let font_value = unsafe { CFType::wrap_under_get_rule(font.cast()) };
        // SAFETY: a CFStringRef static exported by Core Text, valid for the process.
        let key = unsafe { kCTFontAttributeName };
        string.set_attribute(CFRange::init(0, string.char_len()), key, &font_value);
        // SAFETY: the attributed string is live for the call; the line comes back under the
        // Create Rule and is released below.
        let line = unsafe { CTLineCreateWithAttributedString(string.as_concrete_TypeRef().cast()) };
        let none = std::ptr::null_mut();
        // SAFETY: `line` is a live CTLine; the three out-pointers may be null (CTLine.h).
        let width = unsafe { CTLineGetTypographicBounds(line, none, none, none) };
        // SAFETY: `line` was made under the Create Rule above and is released once.
        unsafe {
            CFRelease(line);
        }
        (name.to_string(), width)
    }

    /// The UI font at `size` as the system makes it, or, with `from`, made at `from` and
    /// copied to `size`, as GPUI keeps one font and copies it to each size it draws.
    fn ui_font(size: f64, from: Option<f64>) -> CtRef {
        // SAFETY: Core Text's UI font constructor takes no ownership of its null language and
        // returns under the Create Rule.
        let made = unsafe {
            CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, from.unwrap_or(size), std::ptr::null())
        };
        if from.is_none() {
            return made;
        }
        // SAFETY: `made` is live; the copy takes no ownership of it or of its null matrix and
        // attributes, and returns under the Create Rule.
        let copy = unsafe {
            CTFontCreateCopyWithAttributes(made, size, std::ptr::null(), std::ptr::null())
        };
        // SAFETY: `made` was created under the Create Rule above and is released once.
        unsafe {
            CFRelease(made);
        }
        copy
    }

    /// GPUI's width for `text` in the system face at `size`, shaped by its Core Text backend.
    fn gpui_width(text: &str, size: f32) -> f32 {
        let system = gpui_platform::text_system();
        let font_id =
            system.font_id(&gpui::font(".SystemUIFont")).unwrap_or_else(|e| panic!("{e}"));
        let runs = [FontRun { len: text.len(), font_id }];
        f32::from(system.layout_line(text, px(size), &runs).width)
    }

    /// Measured 2026-10-03 on macOS 27 ("Connect to a server"): Core Text's UI font sets it
    /// 119.13 / 171.43 / 217.56 pt wide at 13 / 20 / 26 pt, all `.SFNS-Regular`, the one
    /// variable face whose optical size and tracking Core Text applies at each size; set in
    /// proportion to 13 pt the 26 pt line would be 238.26. A copy of a font made at another size
    /// carries the new size's tracking, so GPUI keeping one font and copying it to each size
    /// loses nothing: it sets the same widths. It set 171.61 at 20 pt until gpui-fast#25, from
    /// shaping alternate runs a float step over the size, which crossed SF's step there. So
    /// GPUI needs no tracking of its own (study §3 #12).
    #[test]
    fn gpui_sets_the_system_face_at_its_optical_size_and_tracking() {
        const TEXT: &str = "Connect to a server";
        let mut per_point = Vec::new();
        for size in [13.0_f32, 20.0, 26.0] {
            let measure = |at: f64, from: Option<f64>| {
                let font = ui_font(at, from);
                let measured = core_text(font, TEXT);
                // SAFETY: made under the Create Rule by `ui_font` and measured; released once.
                unsafe {
                    CFRelease(font);
                }
                measured
            };
            let (name, system) = measure(f64::from(size), None);
            assert!(name.starts_with(".SFNS"), "{size} pt: the system face, not {name}");
            let (_, copied) = measure(f64::from(size), Some(13.0));
            assert!((copied - system).abs() < 0.01, "{size} pt: a copy keeps the size's tracking");
            let gpui = f64::from(gpui_width(TEXT, size));
            assert!(
                (gpui - system).abs() < 0.01,
                "{size} pt: GPUI {gpui:.2}, Core Text {system:.2}"
            );
            per_point.push(system / f64::from(size));
        }
        assert!(
            per_point.windows(2).all(|w| w[1] < w[0] * 0.98),
            "SF tightens as it grows: {per_point:?}"
        );
    }
}
