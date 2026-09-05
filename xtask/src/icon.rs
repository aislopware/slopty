//! The app icon, rendered from `assets/icon.svg` at build time.
//!
//! One SVG is the source of truth for every platform: `cargo xtask bundle` rasterises it into
//! `Slopty.icns` (every size the Dock, Finder and Spotlight ask for) and `cargo xtask ios`
//! into the 1024 px PNG the asset catalog wants. Rendering is pure Rust (`resvg` +
//! `tiny-skia`), so no `iconutil`, `sips` or design tool sits in the build path.

use anyhow::{Context as _, Result, anyhow, bail};
use camino::Utf8Path;
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;
use xshell::Shell;

/// The source drawing, relative to the repo root.
pub const SOURCE: &str = "assets/icon.svg";

/// Every 32-bit slot of a modern `.icns`: 16…512 pt at 1× and 2×.
const ICNS_SLOTS: [icns::IconType; 10] = [
    icns::IconType::RGBA32_16x16,
    icns::IconType::RGBA32_16x16_2x,
    icns::IconType::RGBA32_32x32,
    icns::IconType::RGBA32_32x32_2x,
    icns::IconType::RGBA32_128x128,
    icns::IconType::RGBA32_128x128_2x,
    icns::IconType::RGBA32_256x256,
    icns::IconType::RGBA32_256x256_2x,
    icns::IconType::RGBA32_512x512,
    icns::IconType::RGBA32_512x512_2x,
];

/// The parsed drawing.
pub struct Icon {
    tree: usvg::Tree,
}

impl Icon {
    /// Parse `assets/icon.svg`.
    pub fn load(sh: &Shell) -> Result<Self> {
        let svg = sh.read_file(SOURCE).with_context(|| format!("reading {SOURCE}"))?;
        let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())
            .map_err(|e| anyhow!("{SOURCE}: {e}"))?;
        Ok(Self { tree })
    }

    /// Rasterise at `px` square pixels.
    pub fn render(&self, px: u32) -> Result<Pixmap> {
        let mut pixmap = Pixmap::new(px, px).ok_or_else(|| anyhow!("icon size {px} is empty"))?;
        let source = self.tree.size();
        let scale_x = f64::from(px) / f64::from(source.width());
        let scale_y = f64::from(px) / f64::from(source.height());
        // `f32` is all tiny-skia takes; icon scales are small numbers.
        #[expect(clippy::cast_possible_truncation, reason = "scale factors are < 1000")]
        let transform = Transform::from_scale(scale_x as f32, scale_y as f32);
        resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        Ok(pixmap)
    }

    /// PNG bytes at `px` square pixels.
    pub fn png(&self, px: u32) -> Result<Vec<u8>> {
        self.render(px)?.encode_png().map_err(|e| anyhow!("encoding {px} px icon: {e}"))
    }

    /// The macOS icon family with every standard size.
    pub fn icns(&self) -> Result<Vec<u8>> {
        let mut family = icns::IconFamily::new();
        for kind in ICNS_SLOTS {
            let px = kind.pixel_width();
            let pixmap = self.render(px)?;
            let mut rgba = Vec::with_capacity(pixmap.data().len());
            for p in pixmap.pixels() {
                let c = p.demultiply();
                rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
            }
            let image = icns::Image::from_data(icns::PixelFormat::RGBA, px, px, rgba)?;
            family.add_icon_with_type(&image, kind)?;
        }
        if family.is_empty() {
            bail!("no icon sizes rendered");
        }
        let mut out = Vec::new();
        family.write(&mut out)?;
        Ok(out)
    }
}

/// `cargo xtask icon <dir>`: write a few PNG sizes and the `.icns` for a look.
pub fn run(sh: &Shell, out: &Utf8Path) -> Result<()> {
    let icon = Icon::load(sh)?;
    sh.create_dir(out)?;
    for px in [64_u32, 256, 1024] {
        sh.write_file(out.join(format!("icon-{px}.png")), icon.png(px)?)?;
    }
    sh.write_file(out.join("Slopty.icns"), icon.icns()?)?;
    println!("✔ {out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_every_icns_slot() {
        let sh = Shell::new().expect("shell");
        sh.change_dir(crate::tools::repo_root().expect("root"));
        let icon = Icon::load(&sh).expect("icon.svg parses");
        let bytes = icon.icns().expect("icns");
        let family = icns::IconFamily::read(bytes.as_slice()).expect("readable icns");
        assert!(family.has_icon_with_type(icns::IconType::RGBA32_512x512_2x));
        assert!(family.has_icon_with_type(icns::IconType::RGBA32_16x16));
        let png = icon.png(1024).expect("png");
        assert_eq!(&png[1..4], b"PNG");
    }
}
