//! An agent's visual of two GPU shaders, as the Mac's and the simulator's cases show it, and what
//! a test reads of it: the surface each shader is painted on, and the colours a render holds
//! there.
//!
//! The upper shader animates and the lower one stands still. Each draws into an `IOSurface`-backed
//! BGRA buffer the window paints as a surface (`docs/decisions/ui.md`, "An animating visual renders
//! into the surface GPUI shows"). The self-test holds Reduce Motion, under which an animating
//! shader draws its first frame at time 0 and stands, so a render taken held is the same in every
//! run; [`crate::Command::Motion`] lets it run between two held renders.
//!
//! What the cases hold the app to is rules, never waits: a frame shown was drawn for the place it
//! is painted at ([`SurfaceInfo::fits`]) once the visual has settled; a still shader keeps its one
//! frame whatever happens around it; the animating one draws while motion runs and, held, lands at
//! most the frame it had in flight.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use image::RgbaImage;

use crate::snapshot::Frame;
use crate::{Command, Driver, Dump, SurfaceInfo};

/// Between two dumps that wait on the app.
const POLL: Duration = Duration::from_millis(150);

/// A rect, in points, that covers any window whole.
const WHOLE: [f32; 4] = [0.0, 0.0, f32::MAX / 4.0, f32::MAX / 4.0];

/// The visual's title, which names its group in the accessibility tree.
pub const TITLE: &str = "Shaders";

/// The animating shader's height, points.
pub const MOVING_HEIGHT: f32 = 160.0;

/// The still shader's height, points.
pub const STILL_HEIGHT: f32 = 80.0;

/// The script: one shader that moves, one that stands.
///
/// The moving shader's stripes, blue from its left edge then orange, sweep right once every
/// 1.4 s and dim from full to half over its first 30 s, so every pixel of a frame past its first
/// quarter second differs from its first frame. The still one runs from blue at its left to
/// orange at its right.
pub const SCRIPT: &str = r#"
return function()
  return ui.column { gap = 12,
    ui.shader { height = 160, animate = true, blend = "opaque", code = [[
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
  vec2 uv = fragCoord / iResolution.xy;
  float stripe = step(0.5, fract(uv.x * 6.0 - iTime * 0.7));
  vec3 colour = mix(vec3(0.12, 0.42, 0.92), vec3(0.96, 0.60, 0.16), stripe);
  fragColor = vec4(colour * (1.0 - min(iTime, 30.0) / 60.0), 1.0);
}
]] },
    ui.shader { height = 80, blend = "opaque", code = [[
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
  vec2 uv = fragCoord / iResolution.xy;
  fragColor = vec4(uv.x, 0.55, 1.0 - uv.x, 1.0);
}
]] },
  }
end
"#;

/// The two shaders as the window last painted them, the animating one first.
///
/// They are the two surfaces whose middles lie inside the visual's group, told apart by height.
/// `None` until both are painted, or when anything else is painted there.
#[must_use]
pub fn shaders(dump: &Dump) -> Option<(SurfaceInfo, SurfaceInfo)> {
    let [gx, gy, gw, gh] = dump.a11y_node("Group", Some(TITLE))?.bounds;
    let inside = |s: &&SurfaceInfo| {
        let [x, y, w, h] = s.bounds;
        let (mx, my) = (x + w / 2.0, y + h / 2.0);
        mx >= gx && mx <= gx + gw && my >= gy && my <= gy + gh
    };
    let [one, other] =
        <[&SurfaceInfo; 2]>::try_from(dump.surfaces.iter().filter(inside).collect::<Vec<_>>())
            .ok()?;
    let (moving, still) = if one.bounds[3] > other.bounds[3] { (one, other) } else { (other, one) };
    Some((moving.clone(), still.clone()))
}

/// Every surface the window last painted, in words, for a failure to say what was there.
#[must_use]
pub fn said(dump: &Dump) -> String {
    dump.surfaces
        .iter()
        .map(|s| {
            let ([x, y, w, h], [pw, ph]) = (s.bounds, s.pixels);
            format!(
                "{} {w:.0}x{h:.0} pt at {x:.0},{y:.0} drawn at {pw}x{ph} px on surface {:?}",
                s.format, s.surface
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The pixel span `rect` (points, at `scale`) covers in `image`, clamped to it.
fn span(image: &RgbaImage, rect: [f32; 4], scale: f32) -> (u32, u32, u32, u32) {
    let [left, top, width, height] = rect;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0) as u32;
    (
        px(left).min(image.width()),
        px(top).min(image.height()),
        px(left + width).min(image.width()),
        px(top + height).min(image.height()),
    )
}

/// How many of `before`'s pixels inside `rect` (window points, at `scale`) differ from
/// `after`'s; the whole picture for a `rect` past its edges.
#[must_use]
pub fn changed_inside(before: &RgbaImage, after: &RgbaImage, rect: [f32; 4], scale: f32) -> usize {
    let (x0, y0, x1, y1) = span(before, rect, scale);
    (y0..y1)
        .flat_map(|row| (x0..x1).map(move |col| (col, row)))
        .filter(|&(col, row)| before.get_pixel(col, row) != after.get_pixel(col, row))
        .count()
}

/// How many pixels `rect` (window points) covers at `scale` in `image`.
#[must_use]
pub fn area(image: &RgbaImage, rect: [f32; 4], scale: f32) -> usize {
    let (x0, y0, x1, y1) = span(image, rect, scale);
    let (width, height) = (u64::from(x1.saturating_sub(x0)), u64::from(y1.saturating_sub(y0)));
    usize::try_from(width.saturating_mul(height)).unwrap_or(usize::MAX)
}

/// The colour `image` holds `across` `rect` (window points, at `scale`) and halfway down: 0 is
/// its left edge, 1 its right.
#[must_use]
pub fn colour_across(image: &RgbaImage, rect: [f32; 4], scale: f32, across: f32) -> [u8; 3] {
    let [left, top, width, height] = rect;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |at: f32, most: u32| ((at * scale).max(0.0) as u32).min(most.saturating_sub(1));
    let column = px(width.mul_add(across, left), image.width());
    let row = px(height.mul_add(0.5, top), image.height());
    let [red, green, blue, _] = image.get_pixel(column, row).0;
    [red, green, blue]
}

/// Blue (the stripes' first colour, the still one's left): blue well over red.
#[must_use]
pub const fn blue(colour: [u8; 3]) -> bool {
    colour[2] > colour[0].saturating_add(60)
}

/// Orange (the stripes' second colour, the still one's right): red well over blue.
#[must_use]
pub const fn orange(colour: [u8; 3]) -> bool {
    colour[0] > colour[2].saturating_add(60)
}

/// The visual once it has settled ([`settled`]).
#[derive(Debug)]
pub struct Shown {
    /// The animating shader's surface.
    pub moving: SurfaceInfo,
    /// The still shader's surface.
    pub still: SurfaceInfo,
    /// The window's scale, device pixels a point.
    pub scale: f32,
    /// The surfaces painted in the first dump that showed both shaders, in words: a frame drawn
    /// for a provisional place says so there.
    pub first: String,
}

/// The visual once both shaders are painted, each frame drawn for the place it is painted at,
/// BGRA in an `IOSurface` at the script's height.
///
/// # Errors
///
/// When they are not, within `within`, or a shader is painted otherwise.
pub async fn settled(drv: &mut Driver, within: Duration) -> Result<Shown> {
    let since = Instant::now();
    let mut first = None;
    loop {
        let dump = drv.dump().await?;
        let scale = dump.window.scale;
        if let Some((moving, still)) = shaders(&dump) {
            first.get_or_insert_with(|| said(&dump));
            if moving.fits(scale) && still.fits(scale) {
                for (shader, height) in [(&moving, MOVING_HEIGHT), (&still, STILL_HEIGHT)] {
                    ensure!(shader.format == "BGRA", "a shader's frame is BGRA: {shader:?}");
                    ensure!(
                        shader.surface.is_some(),
                        "a shader draws into an IOSurface: {shader:?}"
                    );
                    ensure!((shader.bounds[3] - height).abs() < 1.0, "the height: {shader:?}");
                }
                let first = first.unwrap_or_default();
                return Ok(Shown { moving, still, scale, first });
            }
        }
        ensure!(
            since.elapsed() < within,
            "the shaders never painted with frames drawn for their places: [{}]\n{}",
            said(&dump),
            crate::frame_text(&dump.a11y)
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The surfaces painted at each step of a case, a step kept when they differ from the step
/// before's: which step drew a frame no rule let it.
#[derive(Default)]
struct Trail(Vec<(&'static str, String)>);

impl Trail {
    /// What `dump` shows painted, at `step`.
    fn mark(&mut self, step: &'static str, dump: &Dump) {
        let now = said(dump);
        if self.0.last().is_none_or(|(_, last)| *last != now) {
            self.0.push((step, now));
        }
    }
}

impl std::fmt::Display for Trail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (nth, (step, painted)) in self.0.iter().enumerate() {
            let then = if nth == 0 { "" } else { "; then " };
            write!(f, "{then}{step}: [{painted}]")?;
        }
        Ok(())
    }
}

/// The last of three renders taken after motion was held again, how many times the window
/// changed from one of them to the next, and the app's state after the last.
async fn held_renders(
    drv: &mut Driver,
    dir: &Path,
    name: &str,
    trail: &mut Trail,
) -> Result<(Frame, usize, Dump)> {
    let mut last = drv.render(&dir.join(format!("{name}-held-0.png"))).await?;
    let mut dump = drv.dump().await?;
    trail.mark("rendered held", &dump);
    let mut changes = 0_usize;
    for nth in 1..3 {
        let next = drv.render(&dir.join(format!("{name}-held-{nth}.png"))).await?;
        dump = drv.dump().await?;
        trail.mark("rendered held", &dump);
        if changed_inside(&last.image, &next.image, WHOLE, next.scale) > 0 {
            changes = changes.saturating_add(1);
        }
        last = next;
    }
    Ok((last, changes, dump))
}

/// Hold the visual `shown` to its rules; what it measured, in words, with the surfaces painted
/// at each step.
///
/// - The still shader has kept its one frame since it settled, whatever happened between.
/// - Held at time 0, the render holds each shader's colours where the script put them.
/// - With motion let run, the animating shader draws: a frame it draws never lies in the surface
///   the frame shown holds, so its surface changes. Held again, it may land the one frame it had in
///   flight and then stands: of three renders, at most one differs from the one before.
/// - Then more than half its pixels differ from time 0's, the still shader's pixels and surface are
///   as they were, and fewer than 400 of the Mac's 1x pixels moved outside the visual.
///
/// Renders go to `dir` as `<name>-*.png`.
///
/// # Errors
///
/// When a rule is broken, or the app does not answer.
pub async fn hold_to_its_rules(
    drv: &mut Driver,
    shown: &Shown,
    dir: &Path,
    name: &str,
    within: Duration,
) -> Result<String> {
    let Shown { moving, still, scale, .. } = shown;
    let scale = *scale;
    let mut trail = Trail::default();
    let before = drv.dump().await?;
    trail.mark("settled", &before);
    let Some((_, still_before)) = shaders(&before) else {
        bail!("the shaders are gone: [{}]", said(&before));
    };
    ensure!(
        &still_before == still,
        "the still shader kept its one frame: {still_before:?}, was {still:?}"
    );

    let first = drv.render(&dir.join(format!("{name}-first.png"))).await?;
    trail.mark("rendered", &drv.dump().await?);
    // A render's pixels are at its own scale, the window's unless it was asked for another.
    let pixel = first.scale;
    let at = |frame: &Frame, rect, across| colour_across(&frame.image, rect, pixel, across);
    let (left, right) = (at(&first, moving.bounds, 0.05), at(&first, moving.bounds, 0.12));
    ensure!(blue(left) && orange(right), "the stripes at time 0: {left:?}, {right:?}");
    let (left, right) = (at(&first, still.bounds, 0.1), at(&first, still.bounds, 0.9));
    ensure!(blue(left) && orange(right), "the still gradient: {left:?}, {right:?}");

    drv.ok(&Command::Motion { reduced: false }).await?;
    let since = Instant::now();
    let drew = loop {
        let dump = drv.dump_moving().await?;
        trail.mark("motion running", &dump);
        if let Some((now, _)) = shaders(&dump).filter(|(now, _)| now.surface != moving.surface) {
            break now;
        }
        if since.elapsed() >= within {
            drv.ok(&Command::Motion { reduced: true }).await?;
            bail!("the animating shader drew no frame while motion ran: [{}]", said(&dump));
        }
        tokio::time::sleep(POLL).await;
    };
    drv.ok(&Command::Motion { reduced: true }).await?;
    trail.mark("held again", &drv.dump().await?);
    let (rest, changes, after) = held_renders(drv, dir, name, &mut trail).await?;
    let Some((moved, kept)) = shaders(&after) else {
        bail!("the shaders after motion ran: [{}]", said(&after));
    };
    let group = after.a11y_node("Group", Some(TITLE)).map(|n| n.bounds).unwrap_or_default();
    let changed = |rect| changed_inside(&first.image, &rest.image, rect, pixel);
    let (in_moving, in_still) = (changed(moving.bounds), changed(still.bounds));
    let outside = changed(WHOLE).saturating_sub(changed(group));
    let area = area(&first.image, moving.bounds, pixel);
    let said = format!(
        "the still shader on surface {:?} settled, {:?} after motion ran; the animating one on \
         {:?}, {:?} while motion ran, {:?} held; {in_moving} of {area} pixels of the animating \
         shader changed from time 0, {in_still} of the still one's, {outside} outside the \
         visual; {changes} of 2 held renders in a row differed. Painted: {trail}",
        still.surface, kept.surface, moving.surface, drew.surface, moved.surface
    );
    ensure!(changes <= 1, "held, the animating shader went on drawing: {said}");
    ensure!(kept == *still, "the still shader kept its one frame while motion ran: {said}");
    ensure!(moved.format == "BGRA" && moved.surface.is_some(), "still a surface: {moved:?}");
    let stayed = moved.bounds.iter().zip(moving.bounds).all(|(now, was)| (now - was).abs() < 0.5);
    ensure!(stayed && moved.fits(scale), "where it was: {moved:?}, was {moving:?}");
    ensure!(in_moving.saturating_mul(2) > area, "the animating shader moved on: {said}");
    ensure!(in_still == 0, "the still shader's pixels stayed: {said}");
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let tolerated = (400.0 * pixel * pixel) as usize;
    ensure!(outside < tolerated, "nothing outside the visual moved: {said}");
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Painted surface `bounds` with an IOSurface `id`, in BGRA.
    fn surface(bounds: [f32; 4], id: u32) -> SurfaceInfo {
        SurfaceInfo { bounds, format: "BGRA".into(), pixels: [0, 0], surface: Some(id) }
    }

    /// The two surfaces inside the visual's group are its shaders, the taller the animating one;
    /// a surface outside it is not one of them, and a third inside makes them unknown.
    #[test]
    fn the_shaders_are_the_two_surfaces_inside_the_visual() {
        let group = crate::A11yNode {
            role: "Group".into(),
            label: Some(TITLE.into()),
            bounds: [100.0, 100.0, 400.0, 300.0],
            ..crate::A11yNode::default()
        };
        let still = surface([110.0, 290.0, 380.0, STILL_HEIGHT], 7);
        let moving = surface([110.0, 110.0, 380.0, MOVING_HEIGHT], 3);
        let away = surface([600.0, 110.0, 100.0, 100.0], 9);
        let mut dump = Dump {
            a11y: vec![group],
            surfaces: vec![still.clone(), away, moving.clone()],
            ..Dump::default()
        };
        assert_eq!(shaders(&dump), Some((moving, still)));
        dump.surfaces.push(surface([110.0, 200.0, 10.0, 10.0], 11));
        assert_eq!(shaders(&dump), None, "a third surface in the visual");
        dump.a11y.clear();
        assert_eq!(shaders(&dump), None, "no visual");
    }

    /// A rect reads the pixels it covers at the render's scale, and one past the picture's edge
    /// stops there.
    #[test]
    fn a_rect_reads_the_pixels_it_covers_at_the_scale() {
        let before = RgbaImage::from_pixel(20, 10, image::Rgba([10, 20, 200, 255]));
        let mut after = before.clone();
        after.put_pixel(5, 5, image::Rgba([200, 20, 10, 255]));
        after.put_pixel(15, 5, image::Rgba([200, 20, 10, 255]));
        assert_eq!(changed_inside(&before, &after, [0.0, 0.0, 4.0, 5.0], 2.0), 1);
        assert_eq!(changed_inside(&before, &after, [0.0, 0.0, 1e9, 1e9], 2.0), 2);
        assert_eq!(area(&before, [0.0, 0.0, 4.0, 5.0], 2.0), 80);
        assert_eq!(area(&before, [8.0, 0.0, 10.0, 10.0], 2.0), 40);
        let rect = [0.0, 0.0, 10.0, 5.0];
        assert!(blue(colour_across(&before, rect, 2.0, 0.0)));
        assert!(orange(colour_across(&after, rect, 2.0, 0.75)), "(15, 5), halfway down");
        assert!(blue(colour_across(&after, rect, 2.0, 1.0)), "its right edge, in the picture");
    }
}
