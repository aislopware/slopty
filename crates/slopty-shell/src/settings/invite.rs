//! The code a phone or iPad scans to connect to the server.
//!
//! In Settings → Devices' "Connect a phone", the link the app gives (`ServerInvite`) is drawn as
//! a QR code, with the address it carries and how to use it, or why there is none.

use gpui::prelude::*;
use gpui::{AnyElement, Hsla, div, px};
use slopty_view_proto::ServerInvite;

use crate::popover;
use crate::theme::Theme;

/// The code's side at most, in points: whole points per module under it, so every module edge
/// falls on a pixel.
pub const CODE_SIDE: f32 = 208.0;
/// Light modules round the code on every side, as the QR standard (ISO/IEC 18004) asks: a
/// scanner finds the code's edge by them.
pub const QUIET: u16 = 4;

/// The field the code is drawn on and its modules' colour, whatever the appearance: a scanner
/// needs dark modules on a light field, and an inverted code is one many scanners do not read.
pub const fn inks() -> (Hsla, Hsla) {
    (gpui::hsla(0.0, 0.0, 1.0, 1.0), gpui::hsla(0.0, 0.0, 0.08, 1.0))
}

/// A code's modules with its quiet zone, as the runs of dark modules each row holds.
#[derive(Debug, PartialEq, Eq)]
pub struct Matrix {
    /// Modules a side, the quiet zone's included.
    pub side: u16,
    /// Each run of dark modules: its row, its first column and its length.
    pub runs: Vec<(u16, u16, u16)>,
}

impl Matrix {
    /// The code for `link`, at the medium error correction (15 % of it may be lost); `None`
    /// past what a code holds.
    pub fn of(link: &str) -> Option<Self> {
        let code = qrcode::QrCode::with_error_correction_level(link, qrcode::EcLevel::M).ok()?;
        let width = code.width();
        // A code is at most 177 modules a side (version 40).
        let side = u16::try_from(width).ok()?.checked_add(QUIET)?.checked_add(QUIET)?;
        let mut runs = Vec::new();
        for (row, modules) in (0_u16..).zip(code.to_colors().chunks(width.max(1))) {
            let mut from = None;
            let dark = modules.iter().map(|&c| c == qrcode::Color::Dark).chain([false]);
            for (col, dark) in (0_u16..).zip(dark) {
                match (dark, from) {
                    (true, None) => from = Some(col),
                    (false, Some(first)) => {
                        runs.push((
                            row.saturating_add(QUIET),
                            first.saturating_add(QUIET),
                            col.saturating_sub(first),
                        ));
                        from = None;
                    }
                    _ => {}
                }
            }
        }
        Some(Self { side, runs })
    }

    /// Whole points a module, as large as [`CODE_SIDE`] allows.
    pub fn module(&self) -> f32 {
        (CODE_SIDE / f32::from(self.side.max(1))).floor().max(1.0)
    }

    /// The code drawn on its light field.
    fn element(self) -> gpui::Canvas<()> {
        let module = self.module();
        let side = module * f32::from(self.side);
        let (paper, ink) = inks();
        gpui::canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                window.paint_quad(gpui::fill(bounds, paper).corner_radii(px(8.0)));
                for &(row, col, len) in &self.runs {
                    let at = gpui::point(px(f32::from(col) * module), px(f32::from(row) * module));
                    let run = gpui::size(px(f32::from(len) * module), px(module));
                    window.paint_quad(gpui::fill(gpui::Bounds::new(bounds.origin + at, run), ink));
                }
            },
        )
        .size(px(side))
    }
}

/// The dialog's body: the code, the address under it and how to use it; with no link, why
/// there is no code. Its title and buttons are the page's.
pub fn body(theme: &Theme, invite: &ServerInvite) -> AnyElement {
    let code = invite.link.as_deref().and_then(Matrix::of);
    let note = match (&invite.link, &code) {
        (Some(_), None) => {
            "The server's address is too long for a code: type it on your device.".to_owned()
        }
        _ => invite.note.clone(),
    };
    div()
        .mt(px(12.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.0))
        .children(code.map(|code| {
            div()
                .id("invite-code")
                .role(gpui::Role::Image)
                .aria_label("The server's code")
                .child(code.element())
        }))
        .children(invite.address.clone().filter(|_| invite.link.is_some()).map(|address| {
            div()
                .id("invite-address")
                .role(gpui::Role::Label)
                .aria_label(address.clone())
                .text_size(crate::typography::ui_rems(12.5))
                .font_family(theme.font_mono.clone())
                .text_color(theme.text)
                .child(address)
        }))
        .child(popover::dialog_body(theme, note).text_center())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code keeps four light modules on every side, opens on a finder pattern's dark top
    /// edge, and fits the side it is drawn at in whole points a module.
    #[test]
    fn the_code_keeps_its_quiet_zone() {
        let code = Matrix::of("slopty://connect?server=mac-studio.tail1234.ts.net:45560")
            .expect("a link fits a code");
        let far = code.side.saturating_sub(QUIET);
        for &(row, col, len) in &code.runs {
            assert!((QUIET..far).contains(&row), "row {row} in the quiet zone");
            assert!(
                col >= QUIET && col.saturating_add(len) <= far,
                "run {col}+{len} in the quiet zone"
            );
        }
        assert!(code.runs.contains(&(QUIET, QUIET, 7)), "the top-left finder's edge");
        let module = code.module();
        assert!(module >= 4.0 && module.fract() == 0.0, "{module}");
        assert!(module * f32::from(code.side) <= CODE_SIDE, "{module} × {}", code.side);
    }

    /// Dark modules on a light field, whichever the appearance.
    #[test]
    fn the_code_is_dark_on_light() {
        let (paper, ink) = inks();
        assert!(paper.l - ink.l > 0.8, "{paper:?} over {ink:?}");
    }
}
