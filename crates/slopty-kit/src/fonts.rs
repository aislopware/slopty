//! Bundled fonts.
//!
//! The interface face, the code face and the symbol fallback ship inside the binary, so every
//! client (macOS, iOS) sets the same words and the same cell geometry, with the same
//! powerline and devicon glyphs, whatever the system has installed. They are Zed's two faces.
//!
//! * `IBM Plex Sans` 3.005, Regular, Italic, `SemiBold` and `SemiBold` Italic, the four weights Zed
//!   bundles — SIL OFL 1.1 (`assets/fonts/LICENSE-IBMPlexSans.txt`).
//! * `Lilex` 2.630 (IBM Plex Mono with ligatures), Regular, Bold and their italics — SIL OFL 1.1
//!   (`assets/fonts/LICENSE-Lilex.txt`).
//! * `Symbols Nerd Font Mono` 3.x — MIT + per-glyph-set licences
//!   (`assets/fonts/LICENSE-SymbolsNerdFont.txt`).

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight};

/// The bundled interface family.
pub const UI_FAMILY: &str = "IBM Plex Sans";
/// The bundled monospace family, for terminals and code.
pub const MONO_FAMILY: &str = "Lilex";
/// The bundled symbol family (powerline, devicons, box drawing extras).
pub const SYMBOLS_FAMILY: &str = "Symbols Nerd Font Mono";

#[expect(
    clippy::large_include_file,
    reason = "the symbol font is 2.6 MB by nature; bundled on purpose"
)]
const FILES: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Italic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/Lilex-Regular.ttf"),
    include_bytes!("../assets/fonts/Lilex-Bold.ttf"),
    include_bytes!("../assets/fonts/Lilex-Italic.ttf"),
    include_bytes!("../assets/fonts/Lilex-BoldItalic.ttf"),
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

/// Every bundled face is the one a run asks for: the family resolves to our file at each weight
/// and slant, not to a system face or a synthesised one (`docs/decisions/ui.md`, "Fonts are
/// bundled, never system-resolved").
#[cfg(test)]
mod bundled {
    use std::borrow::Cow;

    use gpui::{Font, FontFeatures, FontStyle, FontWeight};

    use super::{FILES, MONO_FAMILY, UI_FAMILY};

    /// One face as its font tables say (per 1000 units): `M`'s advance and its ink's left and
    /// right edges, which tell the weights and slants apart, then the `hhea` ascent and
    /// descent and the `post` underline position and thickness.
    struct Tables {
        family: &'static str,
        weight: FontWeight,
        italic: bool,
        advance: f32,
        ink: (f32, f32),
        ascent: f32,
        descent: f32,
        underline: (f32, f32),
    }

    /// Read from the bundled files with fontTools (`hmtx`, `glyf`, `hhea`, `post`).
    const FACES: [Tables; 8] = [
        Tables {
            family: UI_FAMILY,
            weight: FontWeight::NORMAL,
            italic: false,
            advance: 812.0,
            ink: (93.0, 719.0),
            ascent: 1025.0,
            descent: -275.0,
            underline: (-95.0, 60.0),
        },
        Tables {
            family: UI_FAMILY,
            weight: FontWeight::NORMAL,
            italic: true,
            advance: 784.0,
            ink: (39.0, 781.0),
            ascent: 1025.0,
            descent: -275.0,
            underline: (-95.0, 60.0),
        },
        Tables {
            family: UI_FAMILY,
            weight: FontWeight::SEMIBOLD,
            italic: false,
            advance: 816.0,
            ink: (82.0, 734.0),
            ascent: 1025.0,
            descent: -275.0,
            underline: (-75.0, 88.0),
        },
        Tables {
            family: UI_FAMILY,
            weight: FontWeight::SEMIBOLD,
            italic: true,
            advance: 796.0,
            ink: (27.0, 804.0),
            ascent: 1025.0,
            descent: -275.0,
            underline: (-74.0, 88.0),
        },
        Tables {
            family: MONO_FAMILY,
            weight: FontWeight::NORMAL,
            italic: false,
            advance: 600.0,
            ink: (68.0, 532.0),
            ascent: 999.0,
            descent: -301.0,
            underline: (-140.0, 60.0),
        },
        Tables {
            family: MONO_FAMILY,
            weight: FontWeight::NORMAL,
            italic: true,
            advance: 600.0,
            ink: (26.0, 605.0),
            ascent: 999.0,
            descent: -301.0,
            underline: (-140.0, 60.0),
        },
        Tables {
            family: MONO_FAMILY,
            weight: FontWeight::BOLD,
            italic: false,
            advance: 600.0,
            ink: (39.0, 561.0),
            ascent: 999.0,
            descent: -301.0,
            underline: (-140.0, 60.0),
        },
        Tables {
            family: MONO_FAMILY,
            weight: FontWeight::BOLD,
            italic: true,
            advance: 600.0,
            ink: (-2.0, 635.0),
            ascent: 999.0,
            descent: -301.0,
            underline: (-140.0, 60.0),
        },
    ];

    #[test]
    fn each_family_weight_and_slant_is_the_bundled_face() {
        let system = gpui_platform::text_system();
        system
            .add_fonts(FILES.iter().map(|f| Cow::Borrowed(*f)).collect())
            .unwrap_or_else(|e| panic!("{e}"));
        let near = |a: f32, b: f32| (a - b).abs() < 1.0;
        for face in &FACES {
            let font = Font {
                family: face.family.into(),
                features: FontFeatures::default(),
                fallbacks: None,
                weight: face.weight,
                style: if face.italic { FontStyle::Italic } else { FontStyle::Normal },
            };
            let name = format!("{} {:?} italic {}", face.family, face.weight, face.italic);
            let id = system.font_id(&font).unwrap_or_else(|e| panic!("{name}: {e}"));
            let m = system.font_metrics(id);
            assert_eq!(m.units_per_em, 1000, "{name}");
            assert!(near(m.ascent, face.ascent) && near(m.descent, face.descent), "{name}: {m:?}");
            assert!(
                near(m.underline_position, face.underline.0)
                    && near(m.underline_thickness, face.underline.1),
                "{name}: {m:?}"
            );
            let glyph = system.glyph_for_char(id, 'M').unwrap_or_else(|| panic!("{name}: no M"));
            let advance = system.advance(id, glyph).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(near(advance.width, face.advance), "{name}: advance {advance:?}");
            let ink =
                system.typographic_bounds(id, glyph).unwrap_or_else(|e| panic!("{name}: {e}"));
            let (left, right) = (ink.origin.x, ink.origin.x + ink.size.width);
            assert!(near(left, face.ink.0) && near(right, face.ink.1), "{name}: ink {ink:?}");
        }
    }
}

/// What the terminal face costs where it meets the frame path, on the platform's own text
/// system (Core Text).
#[cfg(test)]
mod cost {
    use std::borrow::Cow;
    use std::time::{Duration, Instant};

    use gpui::{FontRun, Point, RenderGlyphParams, SUBPIXEL_VARIANTS_X, px};

    use super::{FILES, MONO_FAMILY, terminal_font};

    /// Dense code with the operators a ligature face joins.
    const CODE: &str = "    let frame = view.update(cx, |v, cx| v.apply(&batch, cx)) => Ok(()); \
                        // != <= >= -> :: www 0x1f ";

    /// A 200-column line of code shaped with ligatures on, and every printable ASCII glyph
    /// rasterised at 13 pt on a 2× display in each subpixel offset, with the bytes those
    /// rasters take in the atlas and the face's cell metrics per em. Prints the numbers
    /// MEASUREMENTS records; run by hand in release, naming another face to set beside it:
    /// `SLOPTY_OTHER_FACE=<ttf> SLOPTY_OTHER_FAMILY=<family> cargo test -p slopty-kit --release
    /// --lib fonts::cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn the_terminal_face_shaped_and_rasterised() {
        const SHAPES: u32 = 500;
        const ROUNDS: u32 = 20;
        let system = gpui_platform::text_system();
        system.add_fonts(FILES.iter().map(|f| Cow::Borrowed(*f)).collect()).unwrap();
        let mut families = vec![MONO_FAMILY.to_owned()];
        if let (Ok(path), Ok(family)) =
            (std::env::var("SLOPTY_OTHER_FACE"), std::env::var("SLOPTY_OTHER_FAMILY"))
        {
            system.add_fonts(vec![Cow::Owned(std::fs::read(&path).unwrap())]).unwrap();
            families.push(family);
        }
        let mut line = String::new();
        while line.len() < 200 {
            line.push_str(CODE);
        }
        line.truncate(200);
        for family in &families {
            let id = system.font_id(&terminal_font(family, false, false, true)).unwrap();
            let runs = [FontRun { len: line.len(), font_id: id }];
            let mut glyphs = 0;
            let mut shape = Vec::new();
            for _ in 0..SHAPES {
                let started = Instant::now();
                let layout = system.layout_line(&line, px(13.0), &runs);
                shape.push(started.elapsed());
                glyphs = layout.runs.iter().map(|r| r.glyphs.len()).sum::<usize>();
            }
            shape.sort_unstable();
            let ascii: Vec<_> = (' '..='~').filter_map(|c| system.glyph_for_char(id, c)).collect();
            let (mut rasters, mut bytes) = (0_usize, 0_usize);
            let mut raster = Vec::new();
            for round in 0..ROUNDS {
                let started = Instant::now();
                for glyph in &ascii {
                    for x in 0..SUBPIXEL_VARIANTS_X {
                        let params = RenderGlyphParams {
                            font_id: id,
                            glyph_id: *glyph,
                            font_size: px(13.0),
                            subpixel_variant: Point { x, y: 0 },
                            scale_factor: 2.0,
                            is_emoji: false,
                            subpixel_rendering: false,
                            dilation: 0,
                        };
                        let bounds = system.glyph_raster_bounds(&params).unwrap();
                        if bounds.size.width.0 > 0 && bounds.size.height.0 > 0 {
                            let (_, data) = system.rasterize_glyph(&params, bounds).unwrap();
                            if round == 0 {
                                rasters += 1;
                                bytes += data.len();
                            }
                        }
                    }
                }
                raster.push(started.elapsed());
            }
            raster.sort_unstable();
            let m = system.font_metrics(id);
            let em = |v: f32| f64::from(v) / f64::from(m.units_per_em);
            let advance = system.advance(id, ascii[33]).unwrap().width;
            let at = |all: &[Duration], q: usize| all[all.len() * q / 100];
            println!(
                "MEASURE {family} 13 pt at 2×: a 200-column line shaped p50 {:?} p95 {:?} \
                 ({glyphs} glyphs); {rasters} ASCII rasters in {SUBPIXEL_VARIANTS_X} offsets, \
                 all of them p50 {:?} p95 {:?}, {bytes} atlas bytes; per em advance {} \
                 ascent {} descent {} gap {}",
                at(&shape, 50),
                at(&shape, 95),
                at(&raster, 50),
                at(&raster, 95),
                em(advance),
                em(m.ascent),
                em(m.descent),
                em(m.line_gap),
            );
        }
    }
}
