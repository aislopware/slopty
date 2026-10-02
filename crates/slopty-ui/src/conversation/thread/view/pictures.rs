//! Pictures in a thread: those sent with a message and those a call returned, drawn small at
//! the size their header gives, so a row keeps its height while the bytes come.
//!
//! The bytes are fetched the first time a picture is drawn (`ThreadRequest::Expand`, held in
//! the client's blob cache) and decoded once per digest here.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ObjectFit, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, div, img,
};
use slopty_proto::thread::Image;
use slopty_proto::thread::wire::Expanded;

use super::ThreadView;
use crate::colors::hsla;

/// The tallest a picture is drawn, in points at zoom 1.
const PICTURE_HEIGHT: f32 = 120.0;

/// The widest, as a multiple of its height: a panorama is cut, not shrunk to a sliver.
const PICTURE_ASPECT: f32 = 3.0;

/// The gpui format of `media_type`, for the ones it decodes.
fn format_of(media_type: &str) -> Option<gpui::ImageFormat> {
    match media_type {
        "image/png" => Some(gpui::ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(gpui::ImageFormat::Jpeg),
        "image/gif" => Some(gpui::ImageFormat::Gif),
        "image/webp" => Some(gpui::ImageFormat::Webp),
        _ => None,
    }
}

/// What a picture says to a screen reader.
pub(super) fn picture_label(image: &Image) -> String {
    if image.width > 0 && image.height > 0 {
        format!("Picture, {} \u{d7} {}", image.width, image.height)
    } else {
        "Picture".to_owned()
    }
}

impl ThreadView {
    /// `image` decoded, once its bytes came; the first ask fetches them.
    fn picture(&self, image: &Image, cx: &mut Context<Self>) -> Option<Arc<gpui::Image>> {
        if let Some(held) = self.pictures.borrow().get(&image.digest) {
            return Some(Arc::clone(held));
        }
        let format = format_of(&image.media_type)?;
        let thread = self.thread;
        let came = self.hub.update(cx, |hub, cx| hub.expanded(thread, &image.at, cx))?;
        let Expanded::Bytes(bytes) = came.as_ref() else { return None };
        let picture = Arc::new(gpui::Image::from_bytes(format, bytes.clone()));
        self.pictures.borrow_mut().insert(image.digest.clone(), Arc::clone(&picture));
        Some(picture)
    }

    /// The pictures in a row, each at the size its header gives; `None` for none.
    pub(super) fn pictures_row(
        &self,
        images: &[Image],
        end: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if images.is_empty() {
            return None;
        }
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let tiles: Vec<AnyElement> = images
            .iter()
            .map(|image| {
                #[expect(clippy::cast_precision_loss, reason = "a picture's shape on screen")]
                let aspect = if image.width > 0 && image.height > 0 {
                    (image.width as f32 / image.height as f32).min(PICTURE_ASPECT)
                } else {
                    1.0
                };
                let shown = self.picture(image, cx);
                div()
                    .id(SharedString::from(format!("picture-{}", image.digest)))
                    .role(Role::Image)
                    .aria_label(SharedString::from(picture_label(image)))
                    .flex_none()
                    .h(self.z(PICTURE_HEIGHT))
                    .w(self.z(PICTURE_HEIGHT * aspect))
                    .rounded(self.z(theme.radii.md))
                    .overflow_hidden()
                    .border_1()
                    .border_color(hsla(s.border_subtle))
                    .bg(hsla(s.raised))
                    .children(shown.map(|p| img(p).size_full().object_fit(ObjectFit::Cover)))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(self.z(theme.spacing.xs))
                .when(end, gpui::Styled::justify_end)
                .children(tiles)
                .into_any_element(),
        )
    }
}
