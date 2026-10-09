//! The title bar's strip for a tab's one tile: the rest of its header.
//!
//! A tab that holds one tile shows it whole under the bar, and the bar's tab already says its
//! title, so its pane draws no header: a row of one tab repeated the bar's tab 32 pt lower and
//! cost the body that height. What else a header says moves up into the bar, at its trailing
//! end before the notices: a page's address, an agent's place, pull request and worktree, an
//! upload, the kind's states, how the tile is doing (its tab's mark, which the tab then leaves
//! to it), and its readouts, then its controls. The breadcrumb already names a shell's checkout,
//! its branch and its machine, and the tab its title, "Edited" and its close, so the strip says
//! none of them. A second tile joining the tab brings the pane's header back, and the strip goes.
//!
//! Unlike a pane's header, the controls stand at rest: the bar keeps its buttons in view, and a
//! strip that showed them only under the pointer would have nothing to point at while it held
//! no facts.
//!
//! It is a view of its own ([`super::Region::TileStrip`]), told what its tile's header is told
//! ([`WorkspaceView::panes_news`]), so a header's news never builds the bar around it.

use gpui::accesskit::Role;
use gpui::{
    Empty, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_client::layout::TileRef;
use slopty_proto::items::ItemKind;

use super::WorkspaceView;
use super::context_menus::Pressed;
use super::tile::{HeaderBits, Seat, spoken_heading};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::kit;

impl WorkspaceView {
    /// Where a tab's one tile is, for its tab to say after its title, as its pane's header did:
    /// a file's folder, a review's or a desktop's place. None for a shell, whose breadcrumb names
    /// its checkout and whose prompt says the rest; a page, whose address the strip shows as its
    /// control; a folder, whose path bar says it; or an agent's thread, whose place the strip's
    /// chips say.
    pub(super) fn bar_place(&self, tile: TileRef) -> Option<String> {
        let item = self.item(tile)?;
        match item.kind {
            ItemKind::Terminal { .. }
            | ItemKind::Thread { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. } => None,
            _ => self.tile_place(item),
        }
    }

    /// The strip of the tab's one tile ([`Self::lone_tile`]); nothing while the tab holds more,
    /// its one tile has no item yet, or the settings page stands where the panes were.
    pub(super) fn render_tile_strip(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let placed = self.placed_tiles().into_iter().find(|p| p.lone);
        let Some(placed) = placed.filter(|_| self.settings.is_none()) else {
            return Empty.into_any_element();
        };
        let tile = placed.tile;
        let Some(item) = self.item(tile) else { return Empty.into_any_element() };
        let theme = &self.theme;
        let id = item.id;
        let title = self.tile_title(item);
        let heading = SharedString::from(spoken_heading(&self.spoken_kind(item), &title));
        let HeaderBits {
            place,
            unsaved,
            branch,
            upload,
            readouts,
            states,
            actions,
            silenced,
            face,
            ..
        } = self.header_bits(&placed, item, &title, Seat::Bar, cx);
        let state = self.header_state(tile, item, cx);
        let mut row = kit::priority_row(SharedString::from(format!("strip-row-{}", id.as_uuid())))
            .fit_content()
            .flex_initial()
            .min_w_0()
            .h_full()
            .gap(px(theme.spacing.sm));
        if let Some(place) = place {
            row = row.item("place", kit::Priority::MEDIUM, place);
        }
        if let Some(unsaved) = unsaved {
            row = row.item("unsaved", kit::Priority::HIGH, unsaved);
        }
        for (key, priority, chip) in branch {
            row = row.item(key, priority, chip);
        }
        if let Some(upload) = upload {
            row = row.item("upload", kit::Priority::HIGH, upload);
        }
        if !states.is_empty() {
            let states =
                div().flex().flex_none().items_center().gap(px(theme.spacing.xs)).children(states);
            row = row.item("states", kit::Priority::HIGH, states);
        }
        if let Some(silenced) = silenced {
            row = row.item("silenced", kit::Priority::MEDIUM, silenced);
        }
        if let Some(state) = state {
            row = row.item("state", kit::Priority::HIGH, state);
        }
        for (key, priority, readout) in readouts {
            row = row.item(key, priority, readout);
        }
        let buttons: Vec<gpui::AnyElement> = actions.into_iter().chain(face).collect();
        if !buttons.is_empty() {
            let buttons = div()
                .debug_selector(move || format!("controls-{}", id.as_uuid()))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(theme.spacing.xs))
                .children(buttons);
            row = row.item("controls", kit::Priority::ESSENTIAL, buttons);
        }
        Self::tile_menu_press(div().id("tile-strip"), tile, Pressed::Header, cx)
            .debug_selector(move || format!("tile-strip-{}", id.as_uuid()))
            .role(Role::Heading)
            .aria_label(heading)
            .flex_initial()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(theme.surfaces.text_secondary))
            .child(row)
            .into_any_element()
    }
}
