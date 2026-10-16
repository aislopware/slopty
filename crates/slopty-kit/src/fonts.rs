//! The symbol face, and the terminal's fallback chain.
//!
//! The shell bundles and registers the interface and code faces, Geist and Geist Mono
//! (`slopty_shell::typography`). This crate bundles the one face a terminal needs beside them:
//! `Symbols Nerd Font Mono` 3.x, the powerline, devicon and box-drawing glyphs prompts draw, under
//! MIT and per-glyph-set licences (`assets/fonts/LICENSE-SymbolsNerdFont.txt`). The app registers
//! it at startup ([`install`]), so a prompt's glyphs resolve to it on every client, whatever the
//! system has installed.

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight};

/// The monospace family a terminal falls back to when the theme names none installed: the
/// shell's code face.
pub const MONO_FAMILY: &str = "Geist Mono";
/// The bundled symbol family (powerline, devicons, box drawing extras).
pub const SYMBOLS_FAMILY: &str = "Symbols Nerd Font Mono";

#[expect(
    clippy::large_include_file,
    reason = "the symbol font is 2.6 MB by nature; bundled on purpose"
)]
const SYMBOLS: &[u8] = include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// Register the symbol face with the text system, and on Apple's platforms with CoreText for
/// the process. Call once at startup, before any window opens.
///
/// # Errors
/// When the text system or CoreText refuses the face.
pub fn install(cx: &App) -> anyhow::Result<()> {
    cx.text_system().add_fonts(vec![Cow::Borrowed(SYMBOLS)])?;
    register_for_fallback()
}

/// Register the symbol face with CoreText for the process, so a run's fallback list finds it by
/// its family name. The text system's own registration serves families asked for by name, and
/// it loads no family without an `m`, which the symbol face lacks; CoreText's fallback cascade,
/// which names its faces, sees only fonts registered here.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn register_for_fallback() -> anyhow::Result<()> {
    use std::ffi::c_void;
    use std::ptr;

    /// A data provider's release callback (none here: the bytes are static).
    type Release = unsafe extern "C" fn(*mut c_void, *const c_void, usize);
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        /// A provider over `size` bytes at `data`, owned by the caller.
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const c_void,
            size: usize,
            release: Option<Release>,
        ) -> *mut c_void;
        /// The font `provider` holds, owned by the caller.
        fn CGFontCreateWithDataProvider(provider: *mut c_void) -> *mut c_void;
    }
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        /// Make `font` available to the process by name.
        fn CTFontManagerRegisterGraphicsFont(font: *mut c_void, error: *mut *mut c_void) -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        /// Drop one reference to `object`.
        fn CFRelease(object: *const c_void);
    }
    // SAFETY: the bytes are 'static, so the provider may read them for the process's life
    // without a release callback.
    let provider = unsafe {
        CGDataProviderCreateWithData(ptr::null_mut(), SYMBOLS.as_ptr().cast(), SYMBOLS.len(), None)
    };
    anyhow::ensure!(!provider.is_null(), "CoreGraphics took no provider for the symbol face");
    // SAFETY: `provider` is the live provider made above.
    let font = unsafe { CGFontCreateWithDataProvider(provider) };
    // SAFETY: the provider is ours by the create rule; the font holds its own reference.
    unsafe {
        CFRelease(provider);
    }
    anyhow::ensure!(!font.is_null(), "CoreGraphics read no font from the symbol face");
    // SAFETY: `font` is the live font made above; a null error pointer asks for no error.
    let registered = unsafe { CTFontManagerRegisterGraphicsFont(font, ptr::null_mut()) };
    // SAFETY: the font is ours by the create rule; CoreText keeps its own reference.
    unsafe {
        CFRelease(font);
    }
    anyhow::ensure!(registered, "CoreText refused the symbol face");
    Ok(())
}

/// Elsewhere the text system's own registration serves fallbacks too.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[expect(clippy::unnecessary_wraps, reason = "the Apple registration can fail")]
const fn register_for_fallback() -> anyhow::Result<()> {
    Ok(())
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

/// A prompt's glyphs, set in a face without them, are drawn from the registered symbol face: the
/// run they shape into is that face, by its metrics (`docs/decisions/ui.md`, "The symbol face is
/// registered at startup"). Registered with the text system alone, they fell to a system face
/// (an em of 1000): CoreText's fallback cascade sees only what is registered with it.
#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use gpui::{FontRun, px};

    use super::{SYMBOLS, register_for_fallback, terminal_font};

    /// Powerline's right arrow and branch, and Nerd Fonts' Rust devicon.
    const PROMPT: &str = "\u{e0b0}\u{e0a0}\u{e7a8}";
    /// The symbol face's em, ascent and descent, in its units (its `head` and `hhea`).
    const SYMBOL_METRICS: (u32, f32, f32) = (2048, 1638.0, 410.0);

    #[test]
    fn the_prompt_glyphs_resolve_to_the_symbol_face() {
        let system = gpui_platform::text_system();
        system.add_fonts(vec![Cow::Borrowed(SYMBOLS)]).unwrap_or_else(|e| panic!("{e}"));
        register_for_fallback().unwrap_or_else(|e| panic!("{e}"));
        let menlo = system
            .font_id(&terminal_font("Menlo", false, false, true))
            .unwrap_or_else(|e| panic!("Menlo: {e}"));
        let runs = [FontRun { len: PROMPT.len(), font_id: menlo }];
        let layout = system.layout_line(PROMPT, px(13.0), &runs);
        let glyphs: usize = layout.runs.iter().map(|run| run.glyphs.len()).sum();
        assert_eq!(glyphs, 3, "{layout:?}");
        for run in &layout.runs {
            let m = system.font_metrics(run.font_id);
            let (em, ascent, descent) = SYMBOL_METRICS;
            assert!(
                run.font_id != menlo
                    && m.units_per_em == em
                    && (m.ascent - ascent).abs() < 1.0
                    && (m.descent.abs() - descent).abs() < 1.0,
                "a prompt glyph fell to another face: {m:?}"
            );
        }
    }
}
