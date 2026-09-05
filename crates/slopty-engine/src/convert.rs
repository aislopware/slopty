//! Conversions between libghostty-vt types and the wire model.

use libghostty_vt::key::{self, Key};
use libghostty_vt::mouse;
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::screen::{CellSemanticContent, CellWide, RowSemanticPrompt};
use libghostty_vt::style::{self, StyleColor, Underline as VtUnderline};
use slopty_grid::{CellWidth, Color, CursorShape, SemanticMark, Style, StyleFlags, Underline};
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};

macro_rules! key_code_map {
    ($($name:ident),* $(,)?) => {
        /// Physical key → engine key. Exhaustive by construction: both enums come from the same
        /// list.
        #[must_use]
        pub const fn key(code: KeyCode) -> Key {
            match code {
                $(KeyCode::$name => Key::$name,)*
            }
        }
    };
}
slopty_proto::for_each_key_code!(key_code_map);

/// Key action.
#[must_use]
pub const fn key_action(action: KeyAction) -> key::Action {
    match action {
        KeyAction::Press => key::Action::Press,
        KeyAction::Repeat => key::Action::Repeat,
        KeyAction::Release => key::Action::Release,
    }
}

/// Modifier bits. Same layout on both sides; converted bit by bit so a reorder on either side
/// stays a compile error rather than a silent swap.
#[must_use]
pub fn mods(m: Mods) -> key::Mods {
    let mut out = key::Mods::empty();
    let pairs = [
        (Mods::SHIFT, key::Mods::SHIFT),
        (Mods::ALT, key::Mods::ALT),
        (Mods::CTRL, key::Mods::CTRL),
        (Mods::SUPER, key::Mods::SUPER),
        (Mods::CAPS_LOCK, key::Mods::CAPS_LOCK),
        (Mods::NUM_LOCK, key::Mods::NUM_LOCK),
        (Mods::SHIFT_RIGHT, key::Mods::SHIFT_SIDE),
        (Mods::ALT_RIGHT, key::Mods::ALT_SIDE),
        (Mods::CTRL_RIGHT, key::Mods::CTRL_SIDE),
        (Mods::SUPER_RIGHT, key::Mods::SUPER_SIDE),
    ];
    for (ours, theirs) in pairs {
        if m.contains(ours) {
            out |= theirs;
        }
    }
    out
}

/// Mouse button.
#[must_use]
pub const fn mouse_button(b: MouseButton) -> mouse::Button {
    match b {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
        MouseButton::Back => mouse::Button::Eight,
        MouseButton::Forward => mouse::Button::Nine,
    }
}

/// Cell colour.
#[must_use]
pub const fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Default,
        StyleColor::Palette(i) => Color::Palette(i.0),
        StyleColor::Rgb(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// Underline style.
#[must_use]
pub const fn underline(u: VtUnderline) -> Underline {
    match u {
        VtUnderline::Single => Underline::Single,
        VtUnderline::Double => Underline::Double,
        VtUnderline::Curly => Underline::Curly,
        VtUnderline::Dotted => Underline::Dotted,
        VtUnderline::Dashed => Underline::Dashed,
        _ => Underline::None,
    }
}

/// Full cell style.
#[must_use]
pub fn style(s: &style::Style) -> Style {
    let mut flags = StyleFlags::empty();
    flags.set(StyleFlags::BOLD, s.bold);
    flags.set(StyleFlags::FAINT, s.faint);
    flags.set(StyleFlags::ITALIC, s.italic);
    flags.set(StyleFlags::BLINK, s.blink);
    flags.set(StyleFlags::INVERSE, s.inverse);
    flags.set(StyleFlags::INVISIBLE, s.invisible);
    flags.set(StyleFlags::STRIKETHROUGH, s.strikethrough);
    flags.set(StyleFlags::OVERLINE, s.overline);
    Style {
        fg: color(s.fg_color),
        bg: color(s.bg_color),
        underline_color: color(s.underline_color),
        underline: underline(s.underline),
        flags,
    }
}

/// Cell width.
#[must_use]
pub const fn cell_width(w: CellWide) -> CellWidth {
    match w {
        CellWide::Narrow => CellWidth::Narrow,
        CellWide::Wide => CellWidth::Wide,
        CellWide::SpacerTail => CellWidth::SpacerTail,
        CellWide::SpacerHead => CellWidth::SpacerHead,
    }
}

/// Semantic mark for a row, from its prompt state and its first cell's content class.
///
/// `start` says a `133;A` was written on this row (libghostty's flag cannot tell two prompts
/// on adjacent rows apart); `exit` is the status the shell reported for the command before it.
#[must_use]
pub const fn semantic_mark(
    prompt: RowSemanticPrompt,
    first: CellSemanticContent,
    start: bool,
    exit: Option<u8>,
) -> SemanticMark {
    let prompt_row = match prompt {
        RowSemanticPrompt::Prompt | RowSemanticPrompt::Continuation => true,
        RowSemanticPrompt::None => matches!(first, CellSemanticContent::Prompt),
    };
    if prompt_row {
        if start { SemanticMark::Prompt { exit } } else { SemanticMark::PromptContinuation }
    } else {
        match first {
            CellSemanticContent::Input => SemanticMark::Input,
            CellSemanticContent::Output | CellSemanticContent::Prompt => SemanticMark::Output,
        }
    }
}

/// Cursor shape.
#[must_use]
pub const fn cursor_shape(s: CursorVisualStyle) -> CursorShape {
    match s {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
        _ => CursorShape::Block,
    }
}
