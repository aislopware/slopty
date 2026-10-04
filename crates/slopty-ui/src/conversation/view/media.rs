//! Pictures in the conversation: a prompt's pasted image, a tool's screenshot, a picture a
//! `Read` opened.
//!
//! Each shows as a thumbnail at the medium radius, sized to its own shape inside a fixed box,
//! so the row's height is known before the bytes come and nothing moves when they do. Until
//! then the thumbnail is a blank plate (a load that resolves in a frame never flashes a mark).
//! A click opens it large over the face, on the scrim, with what it is under it; a click
//! anywhere or Esc closes it.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, Context, ElementId, ImageFormat, InteractiveElement as _, IntoElement as _,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img,
};
use slopty_proto::conversation::{IMAGE_BYTES, Image};

use super::ConversationView;
use crate::colors::hsla;
use crate::conversation::model::Picture;
use crate::conversation::thread::view::picture_words;
use crate::kit;

/// The box a thumbnail fits in, in points at zoom 1: a screenshot reads at this height, and a
/// wide one stops at the width.
const THUMB: (f32, f32) = (200.0, 120.0);

/// The narrowest a thumbnail gets: a tall picture stays a thing to click.
const THUMB_LEAST: f32 = 48.0;

/// A thumbnail's size for `image`: its own shape inside [`THUMB`], a square when its header
/// gave no size.
#[must_use]
pub(super) fn thumb_size(image: &Image) -> (f32, f32) {
    let (box_w, box_h) = THUMB;
    if image.width == 0 || image.height == 0 {
        return (box_h, box_h);
    }
    #[expect(clippy::cast_precision_loss, reason = "a picture's shape on screen")]
    let ratio = image.width as f32 / image.height as f32;
    let (w, h) =
        if ratio * box_h > box_w { (box_w, box_w / ratio) } else { (ratio * box_h, box_h) };
    (w.max(THUMB_LEAST), h.max(THUMB_LEAST.min(box_h)))
}

/// What a picture is, in words: "1600 × 1200 · PNG · 240 KB".
#[must_use]
pub(super) fn describe(image: &Image) -> String {
    picture_words(image.width, image.height, &image.media_type, image.bytes)
}

fn format_of(media_type: &str) -> ImageFormat {
    match media_type {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    }
}

impl ConversationView {
    /// The picture `image` ready to draw, once its bytes are here: made once per digest, and
    /// decoded by GPUI on its background executor the first time it is painted.
    fn drawable(&self, image: &Image) -> Option<Arc<gpui::Image>> {
        let Some(Picture::Here(bytes)) = self.model.picture(&image.digest) else { return None };
        let mut made = self.pictures.borrow_mut();
        let drawable = made.entry(image.digest.clone()).or_insert_with(|| {
            Arc::new(gpui::Image::from_bytes(format_of(&image.media_type), bytes.to_vec()))
        });
        Some(Arc::clone(drawable))
    }

    /// Pictures as a row of thumbnails that wraps; `key` names the row they are in.
    pub(super) fn thumbnails(&self, key: &str, images: &[Image], cx: &Context<Self>) -> AnyElement {
        div()
            .debug_selector({
                let key = key.to_owned();
                move || format!("pictures-{key}")
            })
            .flex()
            .flex_wrap()
            .gap(self.z(self.theme.spacing.xs))
            .children(images.iter().enumerate().map(|(n, image)| self.thumbnail(key, n, image, cx)))
            .into_any_element()
    }

    fn thumbnail(&self, key: &str, n: usize, image: &Image, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (w, h) = thumb_size(image);
        let radius = self.z(theme.radii.md);
        let picture = self.model.picture(&image.digest);
        let body = match (self.drawable(image), picture) {
            (Some(drawable), _) => img(drawable)
                .size_full()
                .object_fit(ObjectFit::Cover)
                .rounded(radius)
                .into_any_element(),
            (None, Some(Picture::Missing)) => {
                let why =
                    if image.bytes > IMAGE_BYTES as u64 { "Too large" } else { "Unavailable" };
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(why)
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };
        let label = SharedString::from(format!("Picture, {}", describe(image)));
        let selector = format!("picture-{key}-{n}");
        let opened = image.clone();
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(selector.clone().into()))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .w(self.z(w))
                .h(self.z(h))
                .rounded(radius)
                .overflow_hidden()
                .border(kit::hair(theme))
                .border_color(hsla(s.border_subtle))
                .bg(hsla(s.hover))
                .cursor_pointer()
                .child(body),
            s.accent,
        )
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            cx.stop_propagation();
            this.view_picture(Some(opened.clone()), cx);
        }))
        .into_any_element()
    }

    /// The picture open large: fitted to the face on the scrim, with what it is and the way
    /// out under it. A click anywhere closes it.
    pub(super) fn picture_viewer(
        &self,
        _window: &mut Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let image = self.viewing.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let picture = self.drawable(image).map(|drawable| {
            img(drawable).size_full().object_fit(ObjectFit::Contain).rounded(self.z(theme.radii.md))
        });
        let close = kit::icon_button_at(
            theme,
            "picture-close",
            crate::icons::IconName::X,
            "Close",
            self.zoom,
        )
        .on_click(cx.listener(|this, _ev, _w, cx| {
            cx.stop_propagation();
            this.view_picture(None, cx);
        }));
        let caption = kit::elevate(div(), theme)
            .flex_none()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .pl(self.z(theme.spacing.sm))
            .pr(self.z(theme.spacing.xxs))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.md))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child(kit::tabular(div()).child(SharedString::from(describe(image))))
            .child(close);
        let layer = div()
            .id("picture-viewer")
            .debug_selector(|| "picture-viewer".to_owned())
            .role(Role::Dialog)
            .aria_label("Picture")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .p(self.z(theme.spacing.lg))
            .bg(kit::scrim(theme))
            .cursor_pointer()
            .on_click(cx.listener(|this, _ev, _w, cx| this.view_picture(None, cx)))
            .child(div().flex_1().min_h_0().w_full().children(picture))
            .child(caption);
        Some(kit::fade_in(layer, "picture-viewer", cx))
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::conversation::{Part, TextRef};

    use super::*;

    fn image(width: u32, height: u32, bytes: u64) -> Image {
        Image {
            digest: "d".to_owned(),
            media_type: "image/png".to_owned(),
            bytes,
            width,
            height,
            at: TextRef {
                record: "r".to_owned(),
                part: Part::Image { tool_use_id: None, index: 0 },
            },
        }
    }

    /// A thumbnail keeps its picture's shape inside its box; a picture with no size is square.
    #[test]
    fn a_thumbnail_keeps_its_pictures_shape() {
        assert_eq!(thumb_size(&image(1_600, 1_200, 1)), (160.0, 120.0));
        assert_eq!(thumb_size(&image(3_000, 600, 1)), (200.0, 48.0));
        assert_eq!(thumb_size(&image(100, 2_000, 1)), (48.0, 120.0));
        assert_eq!(thumb_size(&image(0, 0, 1)), (120.0, 120.0));
    }

    /// What a picture is reads in words, its size as a person says it.
    #[test]
    fn a_picture_says_what_it_is() {
        assert_eq!(
            describe(&image(1_600, 1_200, 245_760)),
            "1600 \u{d7} 1200 \u{b7} PNG \u{b7} 240 KB"
        );
    }
}
