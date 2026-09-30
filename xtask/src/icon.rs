//! The app icon: `assets/icon.svg` turned into an Icon Composer document at build time.
//!
//! The SVG is the full-bleed artwork on a 1024 canvas: the ink plate and the family's nine
//! dots, lit as a prompt, `>` with its cursor dot on the baseline (`docs/decisions/brand.md`).
//! macOS 26 and iOS 26 draw Liquid Glass only from an Icon Composer `.icon` document, so the
//! SVG is parsed (usvg) into its plate and dots, and each dot becomes a layer of
//! `AppIcon.icon`: the chevron and the cursor in glass groups of their own, the unlit dots in a
//! flat one beneath, the plate as the document's fill. The system masks the shape, and derives
//! the dark, tinted and clear appearances from those layers.
//!
//! `actool` compiles the document (it ships with Xcode, like the linker): on macOS
//! `cargo xtask bundle` runs it into `Assets.car` plus the `.icns` older readers fall back
//! to; on iOS the document sits in the generated Xcode project and `xcodebuild` runs it.

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use resvg::usvg;
use serde_json::{Value, json};
use xshell::{Shell, cmd};

use crate::tools::step;

/// The source drawing, relative to the repo root.
pub const SOURCE: &str = "assets/icon.svg";
/// The icon's name: the `.icon` document, `CFBundleIconName`, and the fallback `.icns`.
pub const NAME: &str = "AppIcon";
/// Floor the icon is compiled for (the project floor).
const MINIMUM_OS: &str = "26.5";
/// Id prefix of the dots in [`SOURCE`], followed by `<column>-<row>`.
const DOT_ID: &str = "dot-";
/// Id of the cursor dot, the one the app blinks.
const CURSOR_ID: &str = "cursor";
/// The mark, row by row: `#` lit, `.` unlit, `_` the cursor (a lit dot of its own).
const PROMPT: [&str; 3] = ["#..", ".#.", "#._"];

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb([u8; 3]);

impl Rgb {
    /// `#rrggbb`.
    fn hex(self) -> String {
        let [r, g, b] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    /// Icon Composer's colour notation.
    fn icon_composer(self) -> String {
        let [r, g, b] = self.0.map(|c| f32::from(c) / 255.0);
        format!("srgb:{r:.5},{g:.5},{b:.5},1.00000")
    }
}

/// One dot of the grid, in canvas units.
#[derive(Debug, Clone, PartialEq)]
struct Dot {
    id: String,
    /// (column, row).
    cell: (usize, usize),
    centre: (f32, f32),
    radius: f32,
    colour: Rgb,
    /// The fill opacity: 1 for a lit dot, the family's unlit level otherwise.
    opacity: f32,
}

impl Dot {
    fn lit(&self) -> bool {
        self.opacity >= 1.0
    }

    fn cursor(&self) -> bool {
        self.id == CURSOR_ID
    }

    /// What [`PROMPT`] says this cell is.
    fn expected(&self) -> Option<char> {
        let (column, row) = self.cell;
        PROMPT.get(row).and_then(|r| r.chars().nth(column))
    }

    /// The layer image: the dot alone at full strength on the canvas, so it needs no
    /// positioning (opacity is the layer's, where the glass can see it).
    fn layer_svg(&self, canvas: f32) -> String {
        let (cx, cy) = self.centre;
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{canvas}" height="{canvas}" viewBox="0 0 {canvas} {canvas}"><circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"/></svg>
"#,
            r = self.radius,
            fill = self.colour.hex(),
        )
    }
}

/// The cell an element id names: `dot-<column>-<row>`, or the cursor's.
fn cell(id: &str) -> Option<(usize, usize)> {
    if id == CURSOR_ID {
        return PROMPT.iter().enumerate().find_map(|(row, r)| r.find('_').map(|c| (c, row)));
    }
    let (column, row) = id.strip_prefix(DOT_ID)?.split_once('-')?;
    Some((column.parse().ok()?, row.parse().ok()?))
}

/// The drawing, as the plate and its dots.
#[derive(Debug, Clone, PartialEq)]
pub struct Art {
    /// Side of the square canvas.
    canvas: f32,
    plate: Rgb,
    dots: Vec<Dot>,
}

impl Art {
    /// Parse `assets/icon.svg`.
    pub fn load(sh: &Shell) -> Result<Self> {
        let svg = sh.read_file(SOURCE).with_context(|| format!("reading {SOURCE}"))?;
        Self::parse(&svg)
    }

    /// Parse the drawing and hold it to the mark: nine circles of one size on a square grid,
    /// lit exactly as [`PROMPT`], on a full-bleed plate.
    fn parse(svg: &str) -> Result<Self> {
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
            .map_err(|e| anyhow!("{SOURCE}: {e}"))?;
        let size = tree.size();
        ensure!((size.width() - size.height()).abs() < 0.01, "{SOURCE} must be square");
        let mut plate = None;
        let mut dots = Vec::new();
        for node in tree.root().children() {
            let usvg::Node::Path(path) = node else {
                bail!("{SOURCE}: only the plate and the dots are drawn, found {:?}", node.id());
            };
            let id = path.id();
            let fill = path.fill().ok_or_else(|| anyhow!("{SOURCE}: {id} has no fill"))?;
            let usvg::Paint::Color(c) = fill.paint() else {
                bail!("{SOURCE}: {id} is not a flat colour");
            };
            let colour = Rgb([c.red, c.green, c.blue]);
            let bounds = path.abs_bounding_box();
            if id == "plate" {
                ensure!(fill.opacity().get() >= 1.0, "{SOURCE}: the plate is opaque");
                ensure!(
                    bounds.width() >= size.width() && bounds.height() >= size.height(),
                    "{SOURCE}: the plate is full bleed; the platform masks its shape"
                );
                plate = Some(colour);
                continue;
            }
            let cell = cell(id).ok_or_else(|| anyhow!("{SOURCE}: unexpected element {id:?}"))?;
            ensure!(
                (bounds.width() - bounds.height()).abs() < 0.01 && is_round(path.data()),
                "{SOURCE}: {id} is not a circle; the family's dots are circles"
            );
            dots.push(Dot {
                id: id.to_owned(),
                cell,
                centre: (
                    bounds.left() + bounds.width() / 2.0,
                    bounds.top() + bounds.height() / 2.0,
                ),
                radius: bounds.width() / 2.0,
                colour,
                opacity: fill.opacity().get(),
            });
        }
        let plate = plate.ok_or_else(|| anyhow!("{SOURCE}: no #plate"))?;
        let art = Self { canvas: size.width(), plate, dots };
        art.check_grid()?;
        Ok(art)
    }

    /// Every cell once, on one pitch, one radius and one colour, lit as [`PROMPT`] says.
    fn check_grid(&self) -> Result<()> {
        ensure!(
            self.dots.len() == 9,
            "{SOURCE}: the grid has nine dots, found {}",
            self.dots.len()
        );
        let first = self.dots.iter().find(|d| d.cell == (0, 0));
        let last = self.dots.iter().find(|d| d.cell == (2, 2));
        let (Some(first), Some(last)) = (first, last) else {
            bail!("{SOURCE}: the grid's corners are missing");
        };
        let pitch = (last.centre.0 - first.centre.0) / 2.0;
        ensure!(pitch > 2.0 * first.radius, "{SOURCE}: the dots overlap");
        for dot in &self.dots {
            let (column, row) = dot.cell;
            #[expect(clippy::cast_precision_loss, reason = "cells are 0..3")]
            let at = (
                (column as f32).mul_add(pitch, first.centre.0),
                (row as f32).mul_add(pitch, first.centre.1),
            );
            ensure!(
                (dot.centre.0 - at.0).abs() < 0.5 && (dot.centre.1 - at.1).abs() < 0.5,
                "{SOURCE}: {} is off the grid",
                dot.id
            );
            ensure!(
                (dot.radius - first.radius).abs() < 0.01,
                "{SOURCE}: {} is another size",
                dot.id
            );
            ensure!(dot.colour == first.colour, "{SOURCE}: {} is another colour", dot.id);
            let lit = match dot.expected() {
                Some('#' | '_') => true,
                Some(_) => false,
                None => bail!("{SOURCE}: {} is outside the grid", dot.id),
            };
            ensure!(
                dot.cursor() == (dot.expected() == Some('_')),
                "{SOURCE}: the cursor is its own element, #{CURSOR_ID}, at the `_` cell"
            );
            ensure!(
                dot.lit() == lit,
                "{SOURCE}: {} is {}; the mark is {PROMPT:?}",
                dot.id,
                if dot.lit() { "lit" } else { "unlit" }
            );
        }
        let cells: std::collections::BTreeSet<_> = self.dots.iter().map(|d| d.cell).collect();
        ensure!(cells.len() == 9, "{SOURCE}: a cell is drawn twice");
        Ok(())
    }

    /// `icon.json` of the Icon Composer document.
    fn icon_json(&self) -> Value {
        let layer = |dot: &Dot| {
            json!({
                "name": dot.id,
                "image-name": format!("{}.svg", dot.id),
                "glass": dot.lit(),
                "opacity": (f64::from(dot.opacity) * 100.0).round() / 100.0,
                // The colour is the layer's, tagged sRGB: an SVG colour is untagged, and Xcode
                // versions disagree on what untagged means (26.6 on CI rendered the green away
                // from #4ac06c, 27 renders it exact). Tinting keeps the luminance of what it
                // tints: left green, the lit dots turn a dim purple on the dark tinted plate
                // (#47337c on #241f2f), so they tint white.
                "fill-specializations": [
                    { "value": { "solid": dot.colour.icon_composer() } },
                    { "appearance": "tinted", "value": { "solid": "extended-gray:1.00000,1.00000" } },
                ],
            })
        };
        let layers = |pick: fn(&Dot) -> bool| {
            self.dots.iter().filter(|d| pick(d)).map(layer).collect::<Vec<_>>()
        };
        // The specular highlight lifts the whole face to a pastel (#6cbd74 for #4ac06c) and
        // translucency muddies it with the plate: the lit dots stay the brand green and get
        // their depth from the glass edge and their shadow.
        let glass = |name: &str, layers: Vec<Value>| {
            json!({
                "name": name,
                "layers": layers,
                "lighting": "individual",
                "shadow": { "kind": "layer-color", "opacity": 0.5 },
                "specular": false,
                "translucency": { "enabled": false, "value": 0 },
            })
        };
        json!({
            "fill": { "solid": self.plate.icon_composer() },
            "groups": [
                glass("cursor", layers(Dot::cursor)),
                glass("prompt", layers(|d| d.lit() && !d.cursor())),
                {
                    "name": "grid",
                    "layers": layers(|d| !d.lit()),
                    "shadow": { "kind": "neutral", "opacity": 0 },
                    "specular": false,
                    "translucency": { "enabled": false, "value": 0 },
                },
            ],
            "supported-platforms": { "squares": "shared" },
        })
    }

    /// Write `<dir>/AppIcon.icon` and return its path.
    pub fn write_document(&self, sh: &Shell, dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let document = dir.join(format!("{NAME}.icon"));
        if document.exists() {
            sh.remove_path(&document)?;
        }
        let assets = document.join("Assets");
        sh.create_dir(&assets)?;
        for dot in &self.dots {
            sh.write_file(assets.join(format!("{}.svg", dot.id)), dot.layer_svg(self.canvas))?;
        }
        let mut json = serde_json::to_string_pretty(&self.icon_json())?;
        json.push('\n');
        sh.write_file(document.join("icon.json"), json)?;
        Ok(document)
    }
}

/// A circle as usvg draws it: curves only, no straight edge (a rounded square has four).
fn is_round(path: &usvg::tiny_skia_path::Path) -> bool {
    use usvg::tiny_skia_path::PathSegment;
    let segments = || path.segments();
    segments().any(|s| matches!(s, PathSegment::CubicTo(..) | PathSegment::QuadTo(..)))
        && !segments().any(|s| matches!(s, PathSegment::LineTo(..)))
}

/// Compile `document` for macOS into `resources`: `Assets.car` and `AppIcon.icns`. The bundle's
/// `Info.plist` names both (`CFBundleIconName`, `CFBundleIconFile`).
pub fn compile_macos(sh: &Shell, document: &Utf8Path, resources: &Utf8Path) -> Result<()> {
    sh.create_dir(resources)?;
    let plist = resources.join("icon-partial.plist");
    step(
        "actool",
        &cmd!(
            sh,
            "xcrun actool {document} --compile {resources} --platform macosx --minimum-deployment-target {MINIMUM_OS} --app-icon {NAME} --output-partial-info-plist {plist} --output-format human-readable-text --errors --warnings"
        )
        .quiet(),
    )?;
    sh.remove_path(&plist)?;
    for made in ["Assets.car", &format!("{NAME}.icns")] {
        ensure!(resources.join(made).exists(), "actool wrote no {made}");
    }
    Ok(())
}

/// How the system shows the icon; `ictool`'s rendition names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendition {
    Default,
    Dark,
    TintedLight,
    TintedDark,
    ClearLight,
    ClearDark,
}

impl Rendition {
    pub const ALL: [Self; 6] = [
        Self::Default,
        Self::Dark,
        Self::TintedLight,
        Self::TintedDark,
        Self::ClearLight,
        Self::ClearDark,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Dark => "Dark",
            Self::TintedLight => "TintedLight",
            Self::TintedDark => "TintedDark",
            Self::ClearLight => "ClearLight",
            Self::ClearDark => "ClearDark",
        }
    }
}

/// Icon Composer's renderer (`ictool`), the one the system uses, inside the active Xcode.
fn ictool(sh: &Shell) -> Result<Utf8PathBuf> {
    let developer = cmd!(sh, "xcode-select --print-path").quiet().read()?;
    let tool = Utf8Path::new(developer.trim())
        .join("../Applications/Icon Composer.app/Contents/Executables/ictool");
    ensure!(tool.exists(), "no ictool at {tool}: install Xcode 26 or later");
    Ok(tool)
}

/// PNG of `document` as macOS draws it at `px` pixels (scale 1), in `rendition`.
pub fn render(
    sh: &Shell,
    document: &Utf8Path,
    rendition: Rendition,
    px: u32,
    out: &Utf8Path,
) -> Result<()> {
    let tool = ictool(sh)?;
    let name = rendition.name();
    let px = px.to_string();
    cmd!(
        sh,
        "{tool} {document} --export-image --output-file {out} --platform macOS --rendition {name} --width {px} --height {px} --scale 1"
    )
    .quiet()
    .ignore_stdout()
    .run()
    .with_context(|| format!("ictool {name} {px} px"))?;
    ensure!(out.exists(), "ictool wrote no {out}");
    Ok(())
}

/// `cargo xtask icon <dir>`: the document, the compiled `Assets.car` and `.icns`, and the
/// system's renders at the sizes and in the appearances worth a look.
pub fn run(sh: &Shell, out: &Utf8Path) -> Result<()> {
    let art = Art::load(sh)?;
    sh.create_dir(out)?;
    let document = art.write_document(sh, out)?;
    compile_macos(sh, &document, &out.join("compiled"))?;
    for px in [16_u32, 32, 128, 512, 1024] {
        let file = out.join(format!("icon-{px}.png"));
        render(sh, &document, Rendition::Default, px, &file)?;
    }
    for rendition in Rendition::ALL.into_iter().skip(1) {
        let file = out.join(format!("icon-{}-512.png", rendition.name().to_lowercase()));
        render(sh, &document, rendition, 512, &file)?;
    }
    println!(
        "✔ {out}: icon-{{16,32,128,512,1024}}.png, the other appearances at 512 px, {NAME}.icon, compiled/"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
