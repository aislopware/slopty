//! The terminal's answers about itself: its name and version (XTVERSION, `CSI > q`), its size
//! in cells and pixels (XTWINOPS `CSI 14 t`, `16 t` and `18 t`, and the in-band reports of mode
//! 2048), and what it supports (the device attributes, `CSI c`). libghostty answers none of
//! these the way a terminal should without its embedder: XTVERSION names libghostty, the size
//! queries go unanswered, and the attributes never list the clipboard.

use std::rc::Rc;

use libghostty_vt::Terminal;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType,
    PrimaryDeviceAttributes, SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
};

use super::clipboard;
use crate::EngineError;

/// What XTVERSION answers.
pub(super) const XTVERSION: &str = concat!("Slopty ", env!("CARGO_PKG_VERSION"));

/// The level a VT220 claims, with ANSI colour.
const PRIMARY: PrimaryDeviceAttributes =
    PrimaryDeviceAttributes::new(ConformanceLevel::VT220, &[DeviceAttributeFeature::ANSI_COLOR]);

/// [`PRIMARY`] with clipboard access, while a viewer shares its clipboard.
const PRIMARY_CLIPBOARD: PrimaryDeviceAttributes = PrimaryDeviceAttributes::new(
    ConformanceLevel::VT220,
    &[DeviceAttributeFeature::ANSI_COLOR, DeviceAttributeFeature::CLIPBOARD],
);

/// Answer the three queries. The size is the terminal's own, in the cell pixels the driver
/// gave it, so it stays right through every resize.
pub(super) fn install(
    term: &mut Terminal<'static, 'static>,
    reads: &clipboard::Shared,
) -> Result<(), EngineError> {
    term.on_xtversion(|_| Some(XTVERSION))?;
    term.on_size(size)?;
    let reads = Rc::clone(reads);
    term.on_device_attributes(move |_| {
        let primary = if reads.borrow().shared() { PRIMARY_CLIPBOARD } else { PRIMARY };
        Some(DeviceAttributes {
            primary,
            // libghostty's own answers, which vim reads to judge what the terminal can do.
            secondary: SecondaryDeviceAttributes {
                device_type: DeviceType::VT220,
                firmware_version: 0,
                rom_cartridge: 0,
            },
            tertiary: TertiaryDeviceAttributes::default(),
        })
    })?;
    Ok(())
}

/// The terminal's size in cells and in the pixels of one cell.
fn size(term: &Terminal<'_, '_>) -> Option<SizeReportSize> {
    let (columns, rows) = (term.cols().ok()?, term.rows().ok()?);
    let cell_width = term.width_px().ok()?.checked_div(u32::from(columns))?;
    let cell_height = term.height_px().ok()?.checked_div(u32::from(rows))?;
    Some(SizeReportSize { rows, columns, cell_width, cell_height })
}

#[cfg(test)]
mod tests;
