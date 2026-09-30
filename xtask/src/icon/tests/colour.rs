//! Colour-managed reading of the renderer's PNG files.
//!
//! `ictool` writes a PNG in whatever space the render needed: 8-bit sRGB for most
//! appearances on Xcode 27, 16-bit Display P3 with an embedded ICC profile for others, and
//! Xcode 26.6 on CI differs again. Raw sample values are therefore not colours. This module
//! decodes at full depth and converts through the embedded profile to sRGB, so a colour check
//! means the same thing on every Xcode.
//!
//! The profile is read as an ICC matrix/TRC ("matrix-shaper") RGB profile, the kind every
//! Apple display profile is: per-channel tone curves (`rTRC`…, `para` or `curv`), then the
//! colourant matrix (`rXYZ`…) into the D50 profile connection space, then Bradford-adapted
//! D50 XYZ to linear sRGB.

use std::fmt;

/// A decoded image: straight RGBA, converted to 8-bit sRGB.
pub(super) struct Decoded {
    pub size: u32,
    pub pixels: Vec<[u8; 4]>,
    pub profile: Source,
}

/// Where the samples' colour space came from.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Source {
    /// An `sRGB` chunk.
    Srgb,
    /// No colour information: read as sRGB, as the web and the PNG spec's default do.
    Untagged,
    /// An embedded ICC profile, named by its description.
    Icc(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Srgb => f.write_str("sRGB chunk"),
            Self::Untagged => f.write_str("untagged (read as sRGB)"),
            Self::Icc(name) => write!(f, "ICC profile {name:?}"),
        }
    }
}

/// D50 XYZ to linear sRGB (sRGB primaries, Bradford-adapted to the ICC connection space).
const SRGB_FROM_XYZ_D50: [[f64; 3]; 3] = [
    [3.133_856_1, -1.616_866_7, -0.490_614_6],
    [-0.978_768_4, 1.916_141_5, 0.033_454_0],
    [0.071_945_3, -0.228_991_4, 1.405_242_7],
];

/// A tone curve: encoded sample (0…1) to linear light.
#[derive(Debug, Clone, PartialEq)]
enum Curve {
    Gamma(f64),
    /// ICC `para` function type 0…4 with its parameters g, a, b, c, d, e, f.
    Parametric(u16, [f64; 7]),
    Table(Vec<f64>),
}

impl Curve {
    #[expect(clippy::many_single_char_names, reason = "the ICC specification's parameter names")]
    fn linear(&self, x: f64) -> f64 {
        match self {
            Self::Gamma(g) => x.powf(*g),
            Self::Parametric(kind, [g, a, b, c, d, e, f]) => match kind {
                0 => x.powf(*g),
                1 => {
                    if x >= -b / a {
                        (a * x + b).powf(*g)
                    } else {
                        0.0
                    }
                }
                2 => {
                    if x >= -b / a {
                        (a * x + b).powf(*g) + c
                    } else {
                        *c
                    }
                }
                3 => {
                    if x >= *d {
                        (a * x + b).powf(*g)
                    } else {
                        c * x
                    }
                }
                _ => {
                    if x >= *d {
                        (a * x + b).powf(*g) + e
                    } else {
                        c * x + f
                    }
                }
            },
            Self::Table(table) => {
                let at = x.clamp(0.0, 1.0) * (table.len() - 1) as f64;
                let (low, t) = (at.floor() as usize, at.fract());
                let high = (low + 1).min(table.len() - 1);
                table[low] * (1.0 - t) + table[high] * t
            }
        }
    }
}

/// A matrix-shaper RGB profile.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Profile {
    name: String,
    curves: [Curve; 3],
    /// Columns: the red, green and blue colourants in D50 XYZ.
    colourants: [[f64; 3]; 3],
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// ICC `s15Fixed16Number`.
fn fixed(d: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(be32(d, at)?.cast_signed()) / 65536.0)
}

impl Profile {
    /// Parse an ICC profile; `Err` names what it lacks to be a matrix-shaper RGB profile.
    pub fn parse(icc: &[u8]) -> Result<Self, String> {
        if icc.get(16..20) != Some(b"RGB ") || icc.get(20..24) != Some(b"XYZ ") {
            return Err("not an RGB profile over an XYZ connection space".to_owned());
        }
        let count = be32(icc, 128).ok_or("no tag table")? as usize;
        let tag = |signature: &[u8; 4]| -> Option<&[u8]> {
            (0..count).find_map(|i| {
                let entry = 132 + i * 12;
                (icc.get(entry..entry + 4)? == signature).then_some(())?;
                let (offset, length) =
                    (be32(icc, entry + 4)? as usize, be32(icc, entry + 8)? as usize);
                icc.get(offset..offset + length)
            })
        };
        let xyz = |signature: &[u8; 4]| -> Result<[f64; 3], String> {
            let data = tag(signature)
                .filter(|d| d.starts_with(b"XYZ "))
                .ok_or_else(|| format!("no {} colourant", String::from_utf8_lossy(signature)))?;
            Ok([fixed(data, 8), fixed(data, 12), fixed(data, 16)].map(|v| v.unwrap_or(f64::NAN)))
        };
        let curve = |signature: &[u8; 4]| -> Result<Curve, String> {
            let missing = || format!("no {} tone curve", String::from_utf8_lossy(signature));
            let data = tag(signature).ok_or_else(missing)?;
            match data.get(0..4) {
                Some(b"para") => {
                    let kind = be16(data, 8).ok_or_else(missing)?;
                    let used = [1, 3, 4, 5, 7].get(usize::from(kind)).ok_or("unknown para type")?;
                    let mut params = [0.0; 7];
                    for (i, p) in params.iter_mut().take(*used).enumerate() {
                        *p = fixed(data, 12 + 4 * i).ok_or_else(missing)?;
                    }
                    Ok(Curve::Parametric(kind, params))
                }
                Some(b"curv") => match be32(data, 8).ok_or_else(missing)? {
                    0 => Ok(Curve::Gamma(1.0)),
                    1 => Ok(Curve::Gamma(f64::from(be16(data, 12).ok_or_else(missing)?) / 256.0)),
                    n => (0..n as usize)
                        .map(|i| be16(data, 12 + 2 * i).map(|v| f64::from(v) / 65535.0))
                        .collect::<Option<Vec<_>>>()
                        .map(Curve::Table)
                        .ok_or_else(missing),
                },
                _ => Err(format!(
                    "{} is not a para or curv curve",
                    String::from_utf8_lossy(signature)
                )),
            }
        };
        let [r, g, b] = [xyz(b"rXYZ")?, xyz(b"gXYZ")?, xyz(b"bXYZ")?];
        let name = tag(b"desc").and_then(description).unwrap_or_else(|| "unnamed".to_owned());
        Ok(Self {
            name,
            curves: [curve(b"rTRC")?, curve(b"gTRC")?, curve(b"bTRC")?],
            colourants: [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]],
        })
    }

    /// Encoded samples (0…1) to linear sRGB, unclipped.
    fn to_linear_srgb(&self, rgb: [f64; 3]) -> [f64; 3] {
        let linear = [0, 1, 2].map(|i| self.curves[i].linear(rgb[i]));
        let xyz =
            self.colourants.map(|row| row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]);
        SRGB_FROM_XYZ_D50.map(|row| row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2])
    }
}

/// The text of a `desc` tag (`mluc`, first record, UTF-16BE; or v2 `desc`, ASCII).
fn description(data: &[u8]) -> Option<String> {
    match data.get(0..4)? {
        b"mluc" => {
            let (length, offset) = (be32(data, 20)? as usize, be32(data, 24)? as usize);
            let (pairs, _) = data.get(offset..offset + length)?.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect();
            String::from_utf16(&units).ok()
        }
        b"desc" => {
            let length = be32(data, 8)? as usize;
            Some(
                String::from_utf8_lossy(data.get(12..12 + length)?)
                    .trim_end_matches('\0')
                    .to_owned(),
            )
        }
        _ => None,
    }
}

/// Linear light to an 8-bit sRGB sample, clipped to the gamut.
fn encode(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let encoded = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (encoded * 255.0).round() as u8
}

/// Decode `bytes` and convert every pixel to sRGB through the image's own profile.
pub(super) fn decode_png(bytes: &[u8]) -> Decoded {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().expect("a PNG");
    let (profile, source) = match (&reader.info().srgb, &reader.info().icc_profile) {
        (Some(_), _) => (None, Source::Srgb),
        (None, Some(icc)) => {
            let profile =
                Profile::parse(icc).unwrap_or_else(|e| panic!("unsupported ICC profile: {e}"));
            let name = profile.name.clone();
            (Some(profile), Source::Icc(name))
        }
        (None, None) => (None, Source::Untagged),
    };
    let mut buffer = vec![0; reader.output_buffer_size().expect("image size")];
    let frame = reader.next_frame(&mut buffer).expect("decodes");
    assert_eq!(frame.width, frame.height, "icon renders are square");
    let wide = frame.bit_depth == png::BitDepth::Sixteen;
    let channels = frame.color_type.samples();
    let sample = |bytes: &[u8], i: usize| -> f64 {
        if wide {
            f64::from(u16::from_be_bytes([bytes[2 * i], bytes[2 * i + 1]])) / 65535.0
        } else {
            f64::from(bytes[i]) / 255.0
        }
    };
    let stride = channels * if wide { 2 } else { 1 };
    let pixels = buffer[..frame.buffer_size()]
        .chunks_exact(stride)
        .map(|px| {
            let (rgb, alpha) = match frame.color_type {
                png::ColorType::Rgba => {
                    ([sample(px, 0), sample(px, 1), sample(px, 2)], sample(px, 3))
                }
                png::ColorType::Rgb => ([sample(px, 0), sample(px, 1), sample(px, 2)], 1.0),
                png::ColorType::GrayscaleAlpha => ([sample(px, 0); 3], sample(px, 1)),
                png::ColorType::Grayscale => ([sample(px, 0); 3], 1.0),
                png::ColorType::Indexed => panic!("EXPAND turns palettes into RGB"),
            };
            let [r, g, b] = match &profile {
                Some(profile) => profile.to_linear_srgb(rgb).map(encode),
                None => rgb.map(|v| (v * 255.0).round() as u8),
            };
            [r, g, b, (alpha * 255.0).round() as u8]
        })
        .collect();
    Decoded { size: frame.width, pixels, profile: source }
}

/// Oklab of an 8-bit sRGB colour (Björn Ottosson).
#[expect(clippy::many_single_char_names, reason = "Ottosson's names for the cone responses")]
pub(super) fn oklab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| {
        let c = f64::from(c) / 255.0;
        if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    });
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

/// Perceptual distance in Oklab; 0.02 is about a just-noticeable difference.
pub(super) fn delta_e(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (a, b) = (oklab(a), oklab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// `#rrggbb`.
pub(super) fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display P3 as Apple's profile states it (the colourants ictool embeds, D50).
    fn display_p3() -> Profile {
        let srgb_curve = Curve::Parametric(3, [2.4, 0.947_9, 0.052_1, 0.077_4, 0.040_5, 0.0, 0.0]);
        Profile {
            name: "Display P3".to_owned(),
            curves: [srgb_curve.clone(), srgb_curve.clone(), srgb_curve],
            colourants: [
                [0.515_1, 0.292_0, 0.157_1],
                [0.241_2, 0.692_2, 0.066_6],
                [-0.001_1, 0.041_9, 0.784_1],
            ],
        }
    }

    #[test]
    fn display_p3_converts_to_srgb_by_the_published_matrix() {
        let p3 = display_p3();
        // Linear Display P3 primaries in linear sRGB (the CSS Color 4 matrix).
        let expected =
            [[1.224_9, -0.042_0, -0.019_6], [-0.224_9, 1.042_0, -0.078_6], [0.0, 0.0, 1.098_2]];
        for (i, want) in expected.iter().enumerate() {
            let mut primary = [0.0; 3];
            primary[i] = 1.0;
            let got = p3.to_linear_srgb(primary);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() < 0.003,
                    "primary {i} channel {c}: {got:?} vs {want:?}"
                );
            }
        }
        // White stays white.
        assert_eq!(p3.to_linear_srgb([1.0; 3]).map(encode), [255, 255, 255]);
    }

    #[test]
    fn the_brand_green_survives_a_p3_encoding() {
        // #4ac06c written in Display P3 numbers (as a P3-tagged render stores it) reads back as
        // #4ac06c once converted, where the raw numbers are far from it.
        let p3_numbers = [0x6c_u8, 0xbd, 0x75];
        let converted =
            display_p3().to_linear_srgb(p3_numbers.map(|c| f64::from(c) / 255.0)).map(encode);
        assert!(delta_e(converted, [0x4a, 0xc0, 0x6c]) < 0.01, "{}", hex(converted));
        assert!(delta_e(p3_numbers, [0x4a, 0xc0, 0x6c]) > 0.03, "the raw numbers differ");
    }

    /// The brand tolerance keeps real renders and catches the two ways the green went wrong.
    #[test]
    fn the_brand_tolerance_keeps_renders_and_catches_regressions() {
        let brand = [0x4a_u8, 0xc0, 0x6c];
        let tolerance = super::super::BRAND_TOLERANCE;
        let face = [0x47, 0xbc, 0x6a];
        eprintln!("Xcode 27 glass face ΔE {:.4}", delta_e(face, brand));
        assert!(delta_e(face, brand) < tolerance, "Xcode 27's glass face at 128 px");
        let wash = [0x6c, 0xbd, 0x74];
        eprintln!("specular wash ΔE {:.4}", delta_e(wash, brand));
        assert!(delta_e(wash, brand) > tolerance, "the specular wash");
        let misread = display_p3().to_linear_srgb(brand.map(|c| f64::from(c) / 255.0)).map(encode);
        eprintln!("hexes read as P3: {} ΔE {:.4}", hex(misread), delta_e(misread, brand));
        assert!(delta_e(misread, brand) > tolerance, "the hexes read as Display P3");
    }

    #[test]
    fn a_profile_without_colourants_is_refused() {
        let mut icc = vec![0_u8; 132];
        icc[16..20].copy_from_slice(b"RGB ");
        icc[20..24].copy_from_slice(b"XYZ ");
        Profile::parse(&icc).expect_err("no colourants or curves");
        icc[16..20].copy_from_slice(b"CMYK");
        Profile::parse(&icc).expect_err("not RGB");
    }
}
