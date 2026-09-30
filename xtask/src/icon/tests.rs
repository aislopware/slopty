#![expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "pixel indices within a 1024 px image, and colour formulas written as published"
)]

use super::*;

mod colour;

fn art() -> Art {
    let root = crate::tools::repo_root().expect("root");
    Art::parse(&std::fs::read_to_string(root.join(SOURCE)).expect("icon.svg")).expect("parses")
}

/// A fresh scratch directory for one test.
fn scratch(test: &str) -> Utf8PathBuf {
    let dir = Utf8PathBuf::from_path_buf(std::env::temp_dir())
        .expect("utf-8 temp dir")
        .join(format!("slopty-icon-{test}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear scratch");
    }
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// OKLCH to 8-bit sRGB (Björn Ottosson's matrices), for checking the family formula.
fn oklch(l: f64, c: f64, hue: f64) -> [u8; 3] {
    let (a, b) = (c * hue.to_radians().cos(), c * hue.to_radians().sin());
    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    let linear = [
        4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_,
        -1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701_0 * s_,
    ];
    linear.map(|v| {
        assert!((0.0..=1.0).contains(&v), "OKLCH {l} {c} {hue} is outside sRGB");
        let encoded = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (encoded * 255.0).round() as u8
    })
}

/// WCAG relative luminance of an sRGB colour.
fn luminance([r, g, b]: [u8; 3]) -> f64 {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// A square image, straight (not premultiplied) RGBA in 8-bit sRGB, whatever space it was
/// stored in.
struct Raster {
    size: u32,
    pixels: Vec<[u8; 4]>,
    /// Where its colour space came from, for failure messages.
    source: String,
}

impl Raster {
    fn png(path: &Utf8Path) -> Self {
        Self::png_bytes(&std::fs::read(path).expect("png"))
    }

    fn png_bytes(bytes: &[u8]) -> Self {
        let decoded = colour::decode_png(bytes);
        Self { size: decoded.size, pixels: decoded.pixels, source: decoded.profile.to_string() }
    }

    /// One `.icns` slot: its embedded PNG, colour-managed, or the raw ARGB of the small slots.
    fn icns(element: &icns::IconElement) -> Self {
        if element.data.starts_with(b"\x89PNG") {
            return Self::png_bytes(&element.data);
        }
        let image = element.decode_image().expect("slot decodes");
        let rgba = image.convert_to(icns::PixelFormat::RGBA);
        assert_eq!(rgba.width(), rgba.height());
        let (pixels, _) = rgba.data().as_chunks::<4>();
        Self {
            size: rgba.width(),
            pixels: pixels.to_vec(),
            source: "icns ARGB (untagged)".to_owned(),
        }
    }

    fn at(&self, x: f32, y: f32) -> [u8; 3] {
        let clamp = |v: f32| (v.max(0.0) as u32).min(self.size - 1);
        let [red, green, blue, _] = self.pixels[(clamp(y) * self.size + clamp(x)) as usize];
        [red, green, blue]
    }

    /// The opaque span of the middle row: where the shape is drawn inside any margin.
    fn shape(&self) -> (f32, f32) {
        let row = self.size / 2;
        let opaque: Vec<u32> = (0..self.size)
            .filter(|&x| self.pixels[(row * self.size + x) as usize][3] >= 128)
            .collect();
        let (first, last) = (opaque.first().expect("not blank"), opaque.last().expect("not blank"));
        (*first as f32, (*last + 1) as f32)
    }

    /// Share of opaque pixels within `tolerance` (Oklab ΔE) of `colour`.
    fn share(&self, colour: [u8; 3], tolerance: f64) -> f64 {
        let near = self
            .pixels
            .iter()
            .filter(|p| p[3] == 255 && colour::delta_e([p[0], p[1], p[2]], colour) <= tolerance)
            .count();
        near as f64 / self.pixels.len() as f64
    }

    /// The centre colour of every lit dot, and the worst Oklab distance from `brand`.
    fn lit_centres(&self, art: &Art, brand: [u8; 3]) -> (Vec<String>, f64) {
        let (lo, hi) = self.shape();
        let map = |v: f32| lo + v / art.canvas * (hi - lo);
        let centres: Vec<[u8; 3]> = art
            .dots
            .iter()
            .filter(|d| d.lit())
            .map(|d| self.at(map(d.centre.0), map(d.centre.1)))
            .collect();
        let worst = centres.iter().map(|c| colour::delta_e(*c, brand)).fold(0.0, f64::max);
        (centres.into_iter().map(colour::hex).collect(), worst)
    }
}
/// How far (Oklab ΔE) a lit dot's face may sit from the brand green: one just-noticeable
/// difference. Xcode 27's glass shading puts the faces at 0.006 to 0.012; the specular wash
/// (0.032) and the hexes read as Display P3 (0.030) both fall outside
/// (`colour::tests::the_brand_tolerance_keeps_renders_and_catches_regressions`).
const BRAND_TOLERANCE: f64 = 0.02;

/// How the mark reads in one picture, sampled at each dot's centre (WCAG contrast).
#[derive(Debug)]
struct Reading {
    /// The dimmest lit dot over the brightest unlit one: the prompt stands out.
    mark: f64,
    /// The dimmest unlit dot over the plate: the grid stays.
    unlit: f64,
    /// The cursor over its unlit neighbour on the baseline: the cursor reads on its own.
    cursor: f64,
}

fn reading(art: &Art, raster: &Raster) -> Reading {
    let (lo, hi) = raster.shape();
    let map = |v: f32| lo + v / art.canvas * (hi - lo);
    let sample = |d: &Dot| raster.at(map(d.centre.0), map(d.centre.1));
    let by_luminance = |a: &[u8; 3], b: &[u8; 3]| luminance(*a).total_cmp(&luminance(*b));
    let lit: Vec<[u8; 3]> = art.dots.iter().filter(|d| d.lit()).map(sample).collect();
    let unlit: Vec<[u8; 3]> = art.dots.iter().filter(|d| !d.lit()).map(sample).collect();
    let cursor = art.dots.iter().find(|d| d.cursor()).expect("cursor");
    let beside = art.dots.iter().find(|d| d.cell == (1, 2)).expect("the cursor's neighbour");
    let plate = raster.at(map(art.canvas * 0.5), map(art.canvas * 0.9));
    Reading {
        mark: contrast(
            *lit.iter().min_by(|a, b| by_luminance(a, b)).expect("lit"),
            *unlit.iter().max_by(|a, b| by_luminance(a, b)).expect("unlit"),
        ),
        unlit: contrast(*unlit.iter().min_by(|a, b| by_luminance(a, b)).expect("unlit"), plate),
        cursor: contrast(sample(cursor), sample(beside)),
    }
}

fn source() -> String {
    std::fs::read_to_string(crate::tools::repo_root().expect("root").join(SOURCE))
        .expect("icon.svg")
}

#[test]
fn the_art_is_a_prompt_with_its_cursor_in_slopty_green() {
    let art = art();
    assert_eq!(art.plate, Rgb([0x1c, 0x1f, 0x26]), "the family's ink");
    let green = Rgb(oklch(0.72, 0.16, 150.0));
    assert_eq!(green.hex(), "#4ac06c");
    let mut mark = String::new();
    for row in 0..3 {
        for column in 0..3 {
            let dot = art.dots.iter().find(|d| d.cell == (column, row)).expect("every cell");
            assert_eq!(dot.colour, green, "{}", dot.id);
            assert!((dot.radius - 88.0).abs() < 0.01, "{}: one radius, 0.38 of the pitch", dot.id);
            mark.push(match (dot.cursor(), dot.lit()) {
                (true, true) => '_',
                (false, true) => '#',
                (_, false) => '.',
            });
        }
        mark.push('/');
    }
    assert_eq!(mark, "#../.#./#._/", "`>` with its cursor dot on the baseline");
    for dot in art.dots.iter().filter(|d| !d.lit()) {
        assert!((dot.opacity - 0.2).abs() < 1e-3, "unlit on ink is 0.2: {}", dot.id);
    }
}

#[test]
fn the_art_is_refused_when_it_is_not_the_prompt() {
    let svg = source();
    assert_eq!(Art::parse(&svg).expect("the real art parses"), art());
    let refused = [
        (svg.replace(r#" fill-opacity="0.2""#, ""), "all nine lit"),
        (
            svg.replace(
                r##"id="dot-2-0" cx="744" cy="280" r="88" fill="#4ac06c" fill-opacity="0.2""##,
                r##"id="dot-2-0" cx="744" cy="280" r="88" fill="#4ac06c""##,
            ),
            "a dot lit outside the prompt",
        ),
        (
            svg.replace(
                r##"id="cursor" cx="744" cy="744" r="88" fill="#4ac06c""##,
                r##"id="cursor" cx="744" cy="744" r="88" fill="#4ac06c" fill-opacity="0.2""##,
            ),
            "the cursor unlit",
        ),
        (svg.replace(r#"id="cursor""#, r#"id="dot-2-2""#), "the cursor not its own element"),
        (
            svg.replace(r#"<circle id="cursor""#, r#"<circle id="cursor" display="none""#),
            "no cursor",
        ),
        (
            svg.replace(
                r#"<circle id="cursor" cx="744" cy="744" r="88""#,
                r#"<rect id="cursor" x="656" y="770.4" width="176" height="61.6" rx="30.8""#,
            ),
            "a capsule cursor: every dot is a circle",
        ),
        (
            svg.replace(
                r#"<circle id="dot-0-0" cx="280" cy="280" r="88""#,
                r#"<rect id="dot-0-0" x="192" y="192" width="176" height="176" rx="40""#,
            ),
            "a rounded square for a dot",
        ),
        (
            svg.replace(
                r#"id="dot-1-1" cx="512" cy="512" r="88""#,
                r#"id="dot-1-1" cx="530" cy="512" r="88""#,
            ),
            "a dot off the grid",
        ),
        (
            svg.replace(
                r#"id="cursor" cx="744" cy="744" r="88""#,
                r#"id="cursor" cx="744" cy="744" r="96""#,
            ),
            "a cursor of another size",
        ),
        (
            svg.replace(
                r#"<rect id="plate" width="1024""#,
                r#"<rect id="plate" x="100" width="824""#,
            ),
            "an inset plate: the platform masks a full-bleed one",
        ),
    ];
    for (art, why) in refused {
        assert_ne!(art, svg, "the fixture for {why} changes the art");
        assert!(Art::parse(&art).is_err(), "refused: {why}");
    }
}

#[test]
fn the_document_has_a_glass_layer_per_lit_dot_and_the_cursor_its_own_group() {
    let art = art();
    let json = art.icon_json();
    let groups = json["groups"].as_array().expect("groups");
    assert!(groups.len() <= 4, "Icon Composer takes at most four groups");
    let layers: Vec<&Value> =
        groups.iter().flat_map(|g| g["layers"].as_array().expect("layers")).collect();
    assert_eq!(layers.len(), art.dots.len());
    for dot in &art.dots {
        let layer = layers.iter().find(|l| l["name"] == dot.id.as_str()).expect("a layer per dot");
        assert_eq!(layer["glass"], dot.lit(), "{}", dot.id);
        assert_eq!(layer["image-name"], format!("{}.svg", dot.id));
    }
    let cursor = groups.iter().find(|g| g["name"] == "cursor").expect("a cursor group");
    assert_eq!(cursor["layers"].as_array().map(Vec::len), Some(1));
    assert_eq!(cursor["layers"][0]["name"], CURSOR_ID);
    assert_eq!(json["fill"]["solid"], art.plate.icon_composer());
    assert!(json.get("color-space-for-untagged-svg-colors").is_none(), "untagged SVG stays sRGB");
}

/// What ships: actool's `Assets.car` and fallback `.icns`, and the system renderer's picture
/// at every `.icns` size and in every appearance. The prompt reads everywhere, the cursor on
/// its own from 32 px up, the unlit dots stay visible, and the green is the brand's.
#[test]
fn compiles_and_the_prompt_reads_at_every_size_and_appearance() {
    let sh = Shell::new().expect("shell");
    let dir = scratch("compile");
    let art = art();
    let document = art.write_document(&sh, &dir).expect("document");
    let resources = dir.join("Resources");
    compile_macos(&sh, &document, &resources).expect("actool compiles the document");

    let icns = icns::IconFamily::read(
        std::fs::File::open(resources.join(format!("{NAME}.icns"))).expect("icns"),
    )
    .expect("readable icns");
    assert!(!icns.elements.is_empty(), "the fallback .icns has images");
    let brand = art.dots[0].colour.0;
    for element in &icns.elements {
        let slot = element.ostype;
        let raster = Raster::icns(element);
        let r = reading(&art, &raster);
        let (centres, worst) = raster.lit_centres(&art, brand);
        eprintln!(
            "icns {slot} ({} px, {}): {r:?}, lit centres {centres:?}",
            raster.size, raster.source
        );
        assert!(r.mark >= 3.0, "{slot}: the prompt stands {:.2}:1 over the unlit dots", r.mark);
        assert!(r.unlit >= 1.2, "{slot}: unlit dots {:.2}:1 over the plate", r.unlit);
        if raster.size >= 32 {
            assert!(r.cursor >= 3.0, "{slot}: the cursor {:.2}:1 over its neighbour", r.cursor);
        }
        if raster.size >= 128 {
            assert!(
                worst <= BRAND_TOLERANCE,
                "{slot}: lit dots {centres:?} are ΔE {worst:.3} from {} ({})",
                colour::hex(brand),
                raster.source
            );
        }
    }

    for px in [16_u32, 32, 64, 128, 256, 512, 1024] {
        let out = dir.join(format!("default-{px}.png"));
        render(&sh, &document, Rendition::Default, px, &out).expect("ictool");
        let raster = Raster::png(&out);
        assert_eq!(raster.size, px);
        let r = reading(&art, &raster);
        eprintln!("ictool {px} px ({}): {r:?}", raster.source);
        assert!(r.mark >= 3.0, "{px} px: the prompt stands {:.2}:1 over the unlit dots", r.mark);
        assert!(r.unlit >= 1.2, "{px} px: unlit dots {:.2}:1 over the plate", r.unlit);
        if px >= 32 {
            assert!(r.cursor >= 3.0, "{px} px: the cursor {:.2}:1 over its neighbour", r.cursor);
        }
        if px >= 128 {
            let (centres, worst) = raster.lit_centres(&art, brand);
            eprintln!("  lit centres {centres:?}, worst ΔE {worst:.4}");
            assert!(
                worst <= BRAND_TOLERANCE,
                "{px} px: lit dots {centres:?} are ΔE {worst:.3} from {} ({})",
                colour::hex(brand),
                raster.source
            );
        }
        if px == 1024 {
            // Four lit dots of r 88 on the 1024 canvas cover 9.28 %; their faces keep the
            // brand green (no specular wash), less the anti-aliased rims.
            let share = raster.share(brand, BRAND_TOLERANCE);
            eprintln!("  brand green share {share:.4}");
            assert!(
                (0.08..=0.10).contains(&share),
                "brand green covers {share:.3} ({}, lit centres {:?})",
                raster.source,
                raster.lit_centres(&art, brand).0
            );
        }
    }
    for rendition in Rendition::ALL {
        let out = dir.join(format!("{}-128.png", rendition.name()));
        render(&sh, &document, rendition, 128, &out).expect("ictool");
        let r = reading(&art, &Raster::png(&out));
        eprintln!("{rendition:?} 128 px: {r:?}");
        assert!(
            r.mark >= 1.8,
            "{rendition:?}: the prompt stands {:.2}:1 over the unlit dots",
            r.mark
        );
        assert!(r.cursor >= 1.8, "{rendition:?}: the cursor {:.2}:1 over its neighbour", r.cursor);
    }
    std::fs::remove_dir_all(&dir).expect("clean scratch");
}
