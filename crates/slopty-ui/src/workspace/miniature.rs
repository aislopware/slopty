//! A tile in the overview's small zoom: its miniature, and the label that names it.
//!
//! The miniature is the tile's own body drawn at the zoom, not a picture rebuilt from it. A
//! shell's grid, a file's editor, a note's Markdown and a conversation are each laid out at their
//! resting size and painted scaled; a remote window or display is its last decoded frame, a
//! texture already; a page is the snapshot it leaves when the overview covers it. What they
//! show is what the tile shows, colours and cursor included, and nothing is built for it:
//! an unfocused body is an `Entity::cached` view, replayed from the last frame until its own
//! content changes, so a still miniature costs a replay and the overview's springs cost what
//! the bodies already cost under the old word covers. With the overview closed there is no
//! miniature at all.
//!
//! Text scaled a few points high is read by its shape, so the label says what the tile is at
//! chrome size, the size it has whatever the zoom: the state (else the kind) in the glyph slot,
//! the title, then one muted line of where it is and the worker where there are several.

use gpui::{
    FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use slopty_client::layout::Placed;
use slopty_proto::items::Item;
use slopty_theme::Typography;

use super::WorkspaceView;
use crate::colors::hsla;
use crate::draw::Draw;

impl WorkspaceView {
    /// The body `content` as its tile's miniature, with its label at its foot.
    pub(super) fn render_miniature(
        &self,
        placed: &Placed,
        item: &Item,
        content: gpui::AnyElement,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let id = item.id;
        let label = self.miniature_label(placed, item, cx);
        div()
            .debug_selector(move || format!("miniature-{}", id.as_uuid()))
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(content)
            .children(label)
            .into_any_element()
    }

    /// The thin label under a miniature: glyph slot and title where the workspace's name above
    /// the block starts, then the tile's facts in the meta size. Drawn once the zoom has all
    /// but landed, and not while the overview closes, as the overview's other words are.
    fn miniature_label(
        &self,
        placed: &Placed,
        item: &Item,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let t = &theme.typography;
        let id = item.id;
        let muted = hsla(s.text_muted);
        let (mark, _) = self.tile_marks(placed.tile, item);
        let lead = crate::palette::lead_slot(theme, self.kind_glyph(item), muted, 1.0);
        let state = mark
            .filter(|m| *m != crate::icons::Status::Idle)
            .map(|m| crate::icons::status_mark(theme, Some(m), 1.0));
        let name = div()
            .debug_selector(move || format!("shapes-label-{}", id.as_uuid()))
            .h(px(t.icon_large()))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .overflow_hidden()
            .child(lead)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(self.tile_title(item))),
            )
            .children(state);
        let worker = (self.workers.len() > 1).then(|| self.worker_name(placed.tile.worker));
        let (meta, _) = self.tile_meta(item, std::time::SystemTime::now(), cx);
        let meta = super::rollup::meta_line([Some(meta.as_str()), worker.as_deref()]);
        let meta = (!meta.is_empty()).then(|| {
            div()
                .debug_selector(move || format!("shapes-meta-{}", id.as_uuid()))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(t.small()))
                .text_color(muted)
                .child(SharedString::from(meta))
        });
        let label = div()
            .debug_selector(move || format!("miniature-label-{}", id.as_uuid()))
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(2.0_f32.mul_add(theme.spacing.xs, t.icon_large())))
            .px(px(theme.spacing.sm))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .overflow_hidden()
            .whitespace_nowrap()
            .font_family(t.ui_family.clone())
            .text_size(px(t.small()))
            .bg(hsla(theme.content()))
            .border_t(crate::kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .child(name)
            .children(meta);
        let words = SharedString::from(format!("miniature-in-{}", id.as_uuid()));
        super::strip::overview_words(
            label,
            words,
            self.layout.overview_open(),
            self.chrome_moves(cx),
        )
    }
}
