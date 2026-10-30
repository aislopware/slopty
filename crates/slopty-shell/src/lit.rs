//! The person's message as a faintly lit surface, after Amp's "alchemy glow"
//! (`.research/amp-study-2026-10-19.md`, item 4). Over the bubble's wash lie three soft
//! glows: the theme's accent high on the left, and a cool blue and a warm clay rising from
//! beyond its right edge. In dark, a hairline of the text's own ink rings it. A high-contrast
//! theme, or a system asked for more contrast or less transparency, keeps the flat wash.
#![allow(
    clippy::cfg_not_test,
    clippy::float_cmp,
    clippy::multiple_unsafe_ops_per_block,
    reason = "shell lint migration: slice B"
)]

use gpui::{
    App, Background, Bounds, Hsla, IntoElement, Pixels, Styled as _, Window, canvas, fill, point,
    px, size,
};

use crate::theme::{Appearance, Theme};

/// A glow: where its ellipse is centred, as fractions of the bubble, and which light it is.
struct Glow {
    centre: (f32, f32),
    hue: Light,
}

/// The three lights: the accent itself, and its cool and warm neighbours.
#[derive(Clone, Copy)]
enum Light {
    Accent,
    Cool,
    Warm,
}

/// Amp's three: the accent at a quarter across and a fifth down, the cool and warm lights off
/// the right edge, a third and nine tenths down.
const GLOWS: [Glow; 3] = [
    Glow { centre: (0.25, 0.20), hue: Light::Accent },
    Glow { centre: (1.50, 0.35), hue: Light::Cool },
    Glow { centre: (1.50, 0.90), hue: Light::Warm },
];

/// Each ellipse reaches 70 % of the bubble's width across and 400 px down, so a tall message is
/// lit as evenly as a short one.
const REACH_ACROSS: f32 = 0.7;
const REACH_DOWN: f32 = 400.0;

/// A glow's alpha at its centre and at its ellipse's edge, before [`strength`].
const CENTRE_ALPHA: f32 = 0.07;
const EDGE_ALPHA: f32 = 0.007;

/// The cool and warm lights' hues: a blue and a clay, at the accent's saturation and lightness.
const COOL_HUE: f32 = 0.6;
const WARM_HUE: f32 = 0.05;

/// The dark ring's ink.
const RING_ALPHA: f32 = 0.07;

/// How strongly the glows show on a display of this scale: a dense display carries a faint hue
/// a coarse one would band, so as Amp does, the light is a third as strong at 1x.
fn strength(scale: f32) -> f32 {
    if scale >= 2.0 { 1.0 } else { 0.3 }
}

const fn light(light: Light, accent: Hsla) -> Hsla {
    match light {
        Light::Accent => accent,
        Light::Cool => Hsla { h: COOL_HUE, ..accent },
        Light::Warm => Hsla { h: WARM_HUE, ..accent },
    }
}

/// One glow as painted: its ellipse's centre and radii, as fractions of the bubble, and its
/// colour at the centre and at the edge.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lit {
    centre: (f32, f32),
    radii: (f32, f32),
    from: Hsla,
    to: Hsla,
}

/// The three glows lighting a bubble `height` tall on a display of `scale`.
fn glows(accent: Hsla, height: f32, scale: f32) -> [Lit; 3] {
    let strength = strength(scale);
    let down = REACH_DOWN / height.max(1.0);
    GLOWS.map(|glow| {
        let color = light(glow.hue, accent);
        Lit {
            centre: glow.centre,
            radii: (REACH_ACROSS, down),
            from: Hsla { a: CENTRE_ALPHA * strength, ..color },
            to: Hsla { a: EDGE_ALPHA * strength, ..color },
        }
    })
}

impl Lit {
    fn background(self) -> Background {
        gpui::radial_gradient(
            point(self.centre.0, self.centre.1),
            size(self.radii.0, self.radii.1),
            self.from,
            self.to,
        )
    }
}

/// Whether the bubble is lit: not under a high-contrast theme, nor when the system asks for more
/// contrast or less transparency.
fn lit_under(theme: &Theme, plain: bool) -> bool {
    !plain && !theme.variant_id.starts_with("high-contrast")
}

/// The light over a bubble with corners `radius`, laid over its whole box and under its words,
/// or nothing when the bubble stays flat.
pub fn light_over(radius: Pixels, theme: &Theme) -> Option<impl IntoElement + use<>> {
    if !lit_under(theme, plain_display()) {
        return None;
    }
    let accent = theme.accent;
    let ring = (crate::theme::current_appearance() == Appearance::Dark)
        .then_some(Hsla { a: RING_ALPHA, ..theme.text });
    Some(
        canvas(
            |_, _, _| (),
            move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
                let height = f32::from(bounds.size.height);
                for glow in glows(accent, height, window.scale_factor()) {
                    window.paint_quad(fill(bounds, glow.background()).corner_radii(radius));
                }
                if let Some(ring) = ring {
                    window.paint_quad(
                        fill(bounds, gpui::transparent_black())
                            .corner_radii(radius)
                            .border_widths(px(1.0))
                            .border_color(ring),
                    );
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
}

/// Whether the person asked the system for more contrast or less transparency.
#[cfg(all(target_os = "macos", not(test)))]
fn plain_display() -> bool {
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    // SAFETY: `NSWorkspace.sharedWorkspace` and its accessibility display flags are plain
    // getters, called on the main thread like every view's render.
    let (contrast, transparency): (Bool, Bool) = unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        (
            msg_send![workspace, accessibilityDisplayShouldIncreaseContrast],
            msg_send![workspace, accessibilityDisplayShouldReduceTransparency],
        )
    };
    contrast.as_bool() || transparency.as_bool()
}

/// Whether the person asked the system for more contrast or less transparency.
#[cfg(all(target_os = "ios", not(test)))]
fn plain_display() -> bool {
    #[link(name = "UIKit", kind = "framework")]
    unsafe extern "C" {
        fn UIAccessibilityDarkerSystemColorsEnabled() -> bool;
        fn UIAccessibilityIsReduceTransparencyEnabled() -> bool;
    }
    // SAFETY: UIKit accessibility getters with no arguments, called on the main thread.
    unsafe {
        UIAccessibilityDarkerSystemColorsEnabled() || UIAccessibilityIsReduceTransparencyEnabled()
    }
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
const fn plain_display() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{CENTRE_ALPHA, EDGE_ALPHA, glows, lit_under};
    use crate::theme::Theme;

    #[test]
    fn the_persons_message_is_lit_in_the_themes_hues_and_flat_when_asked() {
        let theme = Theme::dark();
        let accent = theme.accent;
        let [first, cool, warm] = glows(accent, 80.0, 2.0);
        let (from, to) = (first.from, first.to);
        assert_eq!(
            (from.h, from.s, from.l),
            (accent.h, accent.s, accent.l),
            "the accent's own hue"
        );
        assert!((from.a - CENTRE_ALPHA).abs() < 1e-6 && (to.a - EDGE_ALPHA).abs() < 1e-6);
        assert!(cool.from.h != warm.from.h, "a cool and a warm light");
        assert_eq!(cool.from.s, accent.s, "at the accent's saturation, never grey");
        assert_eq!(first.radii.1, 5.0, "400 px down, whatever the bubble's height");
        assert_eq!(glows(accent, 800.0, 2.0)[0].radii.1, 0.5);

        let coarse = glows(accent, 80.0, 1.0)[0].from.a;
        assert!(coarse < from.a * 0.5, "fainter on a coarse display");

        assert!(lit_under(&theme, false));
        assert!(!lit_under(&theme, true), "the system asked for a plain display");
        let mut contrast = Theme::dark();
        contrast.variant_id = "high-contrast-dark".into();
        assert!(!lit_under(&contrast, false), "a high-contrast theme stays flat");
    }
}
