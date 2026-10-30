// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Terminal paint metrics and the panel fill.
//!
//! The grid is Slopty's own terminal view ([`crate::live`]); what lives here is the look the
//! panel hands that view: the font metrics, the padding and the fill behind the grid.

use gpui::Hsla;

use crate::theme::Theme;

/// Default terminal font metrics; the rendered values come from the theme.
pub const TERM_FONT_SIZE: f32 = 13.0;
pub const TERM_LINE_HEIGHT: f32 = 18.0;
/// Inner padding of the grid area.
pub const TERM_PADDING: f32 = 12.0;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// The panel fill behind the grid. On glass the opaque tone thins to a
/// translucent wash so the blurred desktop reads through like the rest of the
/// chrome (same move as [`Theme::card_glass_bg`]); opaque platforms keep the
/// true tone. Explicit cell backgrounds (vim colorschemes etc.) still paint
/// their own opaque quads on top.
pub fn terminal_panel_bg(theme: &Theme) -> Hsla {
    theme.session_ground() // one ground per session
}
