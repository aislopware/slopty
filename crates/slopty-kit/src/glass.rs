// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Glass plates for controls.
//!
//! Each is a vertical gradient lit from above, a hairline rim, an inner top highlight and a soft
//! drop. [`light_plate`] is the neutral plate (a track), [`accent_plate`]
//! the active one (Stop, a switch that is on, a slider's fill) and [`thumb_plate`] the opaque
//! handle that rides on either. The only gradients the chrome draws are these.

use gpui::{
    Background, Bounds, BoxShadow, Corners, Hsla, Pixels, Styled, Window, hsla, linear_color_stop,
    linear_gradient, point, px,
};

use crate::theme::Theme;

const fn shadow(color: Hsla, y: f32, blur: f32, spread: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.0), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset,
    }
}

const fn white(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}

const fn black(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, alpha)
}

fn vertical(top: Hsla, bottom: Hsla) -> Background {
    linear_gradient(180.0, linear_color_stop(top, 0.0), linear_color_stop(bottom, 1.0))
}

/// A lighter tint of `color` for a gradient's lit top.
fn lift(color: Hsla, amount: f32) -> Hsla {
    crate::motion::mix(color, white(color.a), amount)
}

/// One glass surface, for an element or a canvas.
#[derive(Debug, Clone)]
pub struct Plate {
    background: Background,
    rim: Hsla,
    shadows: Vec<BoxShadow>,
}

impl Plate {
    /// The plate on an element: fill, rim and shadows.
    pub fn apply<E: Styled>(self, el: E) -> E {
        el.bg(self.background).border_1().border_color(self.rim).shadow(self.shadows)
    }

    /// The plate painted as a styled element would: drops under the fill, the rim on it, the
    /// inset light over it.
    pub fn paint(&self, window: &mut Window, bounds: Bounds<Pixels>, radius: f32) {
        let corners = Corners::all(px(radius));
        let (inset, drop): (Vec<_>, Vec<_>) = self.shadows.iter().cloned().partition(|s| s.inset);
        window.paint_drop_shadows(bounds, corners, &drop);
        window.paint_quad(gpui::quad(
            bounds,
            corners,
            self.background,
            px(1.0),
            self.rim,
            gpui::BorderStyle::default(),
        ));
        window.paint_inset_shadows(bounds, corners, &inset);
    }
}

/// Neutral glass faded in by `t`: dark lifts translucent white off the surface, light sinks a
/// translucent tint into it, since a near-white plate on a near-white surface washes out.
#[must_use]
pub fn light_plate(theme: &Theme, t: f32) -> Plate {
    if theme.appearance.is_dark() {
        return Plate {
            background: vertical(white(0.08 * t), white(0.05 * t)),
            rim: white(0.09 * t),
            shadows: vec![
                shadow(white(0.07 * t), 1.0, 0.0, 0.0, true),
                shadow(black(0.16 * t), 1.0, 2.0, 0.0, false),
            ],
        };
    }
    Plate {
        background: vertical(black(0.075 * t), black(0.04 * t)),
        rim: black(0.08 * t),
        // Inset only: a drop under a translucent fill would whiten it.
        shadows: vec![
            shadow(black(0.08 * t), 1.0, 2.0, 0.0, true),
            shadow(white(0.55 * t), -1.0, 0.0, 0.0, true),
        ],
    }
}

/// Accent glass faded in by `t`, its coloured shadow spread by `glow` (0–1).
#[must_use]
pub fn accent_plate(theme: &Theme, t: f32, glow: f32) -> Plate {
    let dark = theme.appearance.is_dark();
    let base = theme.accent_strong;
    let top = lift(base, if dark { 0.06 } else { 0.22 });
    let rim = if dark {
        lift(base, 0.35).opacity(0.35)
    } else {
        crate::motion::mix(base, black(1.0), 0.14)
    };
    let highlight = white(if dark { 0.12 } else { 0.32 });
    let ring = white(if dark { 0.0 } else { 0.22 });
    let halo = base.opacity((0.3_f32.mul_add(glow, if dark { 0.0 } else { 0.14 })) * t);
    Plate {
        background: vertical(top.opacity(t), base.opacity(t)),
        rim: rim.opacity(t),
        shadows: vec![
            shadow(highlight.opacity(t), 1.0, 0.0, 0.0, true),
            shadow(ring.opacity(t), 0.0, 0.0, 1.0, true),
            shadow(halo, 2.0_f32.mul_add(glow, 2.0), 14.0_f32.mul_add(glow, 6.0), 0.0, false),
        ],
    }
}

/// The opaque handle: light's neutral plate in both appearances, so it stays solid over
/// translucent dark glass, with the appearance's own drop under it.
#[must_use]
pub fn thumb_plate(theme: &Theme) -> Plate {
    let dark = theme.appearance.is_dark();
    let mut shadows = vec![
        shadow(white(0.8), 1.0, 0.0, 0.0, true),
        shadow(black(if dark { 0.22 } else { 0.14 }), 1.0, 2.0, 0.0, false),
    ];
    if !dark {
        shadows.push(shadow(black(0.06), 2.0, 6.0, 0.0, false));
    }
    Plate {
        background: vertical(hsla(0.0, 0.0, 1.0, 1.0), hsla(0.0, 0.0, 0.965, 1.0)),
        rim: black(if dark { 0.12 } else { 0.11 }),
        shadows,
    }
}

/// `el` on neutral glass faded in by `t`.
pub fn light<E: Styled>(el: E, theme: &Theme, t: f32) -> E {
    light_plate(theme, t).apply(el)
}

/// `el` as the opaque handle.
pub fn thumb<E: Styled>(el: E, theme: &Theme) -> E {
    thumb_plate(theme).apply(el)
}
