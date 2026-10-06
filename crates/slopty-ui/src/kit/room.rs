//! How much room a surface has, as one of three classes every surface lays out for: a tile by
//! the width the workspace placed it at, anything else by its own container
//! ([`room_query`]), never by the window's (`docs/decisions/ui.md`, "How surfaces adapt to
//! their room").
//!
//! The classes are set by text, so they are written at the default 13 pt chrome and grow and
//! shrink with the chrome size setting: a larger chrome needs more room for the same words.

use gpui::{App, IntoElement, Pixels, Size, Window, container_query};
use slopty_theme::Theme;

/// The chrome size the rooms are written at.
const WRITTEN_AT: f32 = 13.0;

/// Under this, written at a 13 pt chrome, a surface is [`Room::Narrow`]: a column beside a
/// board or a review (312 pt), a phone's.
const NARROW_BELOW: f32 = 420.0;

/// From this, written at a 13 pt chrome, a surface is [`Room::Wide`]: wide work's own column
/// (`WIDE_LEAST` in the layout), where a review shows its file list.
const WIDE_FROM: f32 = 720.0;

/// A surface's room.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Room {
    /// Under 420 pt: one thing to a line, facts left to the menu or the hint.
    Narrow,
    /// 420 pt to 720 pt: the ordinary column.
    Regular,
    /// From 720 pt: room for a side list, a split, a second column.
    Wide,
}

impl Room {
    /// The room of a surface `width` points wide for `theme`'s chrome size.
    #[must_use]
    pub fn of(width: f32, theme: &Theme) -> Self {
        if width < Self::narrow_below(theme) {
            Self::Narrow
        } else if width < Self::wide_from(theme) {
            Self::Regular
        } else {
            Self::Wide
        }
    }

    /// The width, in points, under which a surface is [`Room::Narrow`].
    #[must_use]
    pub fn narrow_below(theme: &Theme) -> f32 {
        NARROW_BELOW * scale(theme)
    }

    /// The width, in points, from which a surface is [`Room::Wide`].
    #[must_use]
    pub fn wide_from(theme: &Theme) -> f32 {
        WIDE_FROM * scale(theme)
    }

    /// Whether it is narrow.
    #[must_use]
    pub fn is_narrow(self) -> bool {
        self == Self::Narrow
    }

    /// Whether it is wide.
    #[must_use]
    pub fn is_wide(self) -> bool {
        self == Self::Wide
    }
}

/// How far `theme`'s chrome is from the size the rooms are written at.
fn scale(theme: &Theme) -> f32 {
    theme.typography.ui_size / WRITTEN_AT
}

/// What `render` draws for the room its container gives it.
///
/// For a surface that is not a tile (a popover, an overlay, a sheet), which the workspace does
/// not place. It fills its parent unless styled otherwise; what it draws cannot size it.
#[must_use]
pub fn room_query<E: IntoElement>(
    theme: &Theme,
    render: impl 'static + FnOnce(Room, Size<Pixels>, &mut Window, &mut App) -> E,
) -> gpui::ContainerQuery {
    let theme = theme.clone();
    container_query(move |size, window, cx| {
        render(Room::of(f32::from(size.width), &theme), size, window, cx)
    })
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, InteractiveElement as _, ParentElement as _, Render, Styled as _, TestAppContext,
        div, px,
    };

    use super::*;

    /// The three rooms at the default chrome, and their edges moving with the chrome size.
    #[test]
    fn a_room_is_read_from_its_width_at_the_chromes_size() {
        let theme = Theme::default();
        assert_eq!(Room::of(312.0, &theme), Room::Narrow, "a column beside a board");
        assert_eq!(Room::of(419.0, &theme), Room::Narrow);
        assert_eq!(Room::of(420.0, &theme), Room::Regular);
        assert_eq!(Room::of(719.0, &theme), Room::Regular);
        assert_eq!(Room::of(720.0, &theme), Room::Wide);
        let mut larger = Theme::default();
        larger.typography.ui_size = 15.0;
        assert_eq!(Room::of(440.0, &larger), Room::Narrow, "larger words need more room");
        assert!(Room::wide_from(&larger) > Room::wide_from(&theme));
    }

    struct Sheet {
        width: f32,
    }

    impl Render for Sheet {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::default();
            div().w(px(self.width)).h(px(40.0)).child(room_query(&theme, |room, _, _, _| {
                let id = match room {
                    Room::Narrow => "narrow",
                    Room::Regular => "regular",
                    Room::Wide => "wide",
                };
                div().debug_selector(move || id.to_owned()).size_full()
            }))
        }
    }

    /// A surface that is not a tile reads its room from its own container.
    #[gpui::test]
    fn a_sheet_reads_its_room_from_its_container(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| Sheet { width: 300.0 });
        cx.run_until_parked();
        assert!(cx.debug_bounds("narrow").is_some());
        view.update(cx, |sheet, cx| {
            sheet.width = 800.0;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("wide").is_some() && cx.debug_bounds("narrow").is_none());
    }
}
