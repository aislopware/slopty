//! The images a reply's markdown names, drawn in the transcript.
//!
//! A path resolves in the chat's folder, as T3 Code does: relative to it, or absolute inside
//! it; one that leaves the folder is refused and never read. The file is read through the
//! chat's machine like a tool's images (`tool_images`), and kept while its row is in the
//! transcript. A web address is not fetched: the transcript names it. Two or more images with
//! only space between them make a gallery of tiles (`markdown/render.rs`).
//!
//! Each image is an a11y image node labelled with its alt text; its value says when it is
//! "loading", "missing" (the file could not be read), "refused" (outside the folder) or
//! "remote".

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, div, img, px};

use super::Transcript;
use crate::markdown::parser::InlineImage;
use crate::markdown::render;
use crate::theme::Theme;
use crate::tool_images::Thumb;

/// A lone image's tallest.
const MAX_HEIGHT: f32 = 360.0;
/// A lone image's widest.
const MAX_WIDTH: f32 = 560.0;
/// The room an image takes while it loads.
const PLACEHOLDER_HEIGHT: f32 = 120.0;

/// What the transcript's image handler and the transcript share.
#[derive(Default)]
pub(super) struct MdImages {
    /// What each path read gave, by its resolved path.
    thumbs: HashMap<String, Thumb>,
    /// The resolved paths the row drawn last showed, until the transcript holds them.
    wanted: Vec<String>,
    /// The chat's folder, where a path resolves.
    folder: Option<String>,
}

impl MdImages {
    /// Resolve paths in `folder`, the chat's.
    pub(super) fn set_folder(&mut self, folder: Option<String>) {
        self.folder = folder;
    }

    /// Another chat's images.
    pub(super) fn clear(&mut self) {
        self.thumbs.clear();
        self.wanted.clear();
    }
}

/// Where an image's source points.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Resolved {
    /// A file in the chat's folder, its absolute path.
    Path(String),
    /// A web address or data: not fetched.
    Remote,
    /// A path outside the chat's folder, or no folder to resolve in.
    Refused,
}

/// `source` resolved in `folder`.
pub(super) fn resolve(source: &str, folder: Option<&str>) -> Resolved {
    let source = source.trim().trim_start_matches('<').trim_end_matches('>');
    let source = source.strip_prefix("file://").unwrap_or(source);
    if source.contains("://") || source.starts_with("data:") || source.starts_with("mailto:") {
        return Resolved::Remote;
    }
    let Some(folder) = folder.filter(|folder| !folder.is_empty()) else {
        return Resolved::Refused;
    };
    let windows = folder.contains('\\');
    let separator = if windows { '\\' } else { '/' };
    let joined = crate::tool_images::resolve_path(source, Some(folder));
    let Some(path) = normalize(&joined, windows) else {
        return Resolved::Refused;
    };
    let Some(root) = normalize(folder, windows) else {
        return Resolved::Refused;
    };
    let root = root.trim_end_matches(separator);
    let inside =
        path.strip_prefix(root).is_some_and(|rest| rest.starts_with(separator) && rest.len() > 1);
    if inside { Resolved::Path(path) } else { Resolved::Refused }
}

/// `path` with `.` and `..` worked out; `None` when `..` climbs past its root.
fn normalize(path: &str, windows: bool) -> Option<String> {
    let separator = if windows { '\\' } else { '/' };
    let mut parts: Vec<&str> = Vec::new();
    let mut prefix = String::new();
    for (ix, part) in path.split(['/', '\\']).enumerate() {
        match part {
            "" if ix == 0 => {}
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            drive if ix == 0 && windows && drive.ends_with(':') => drive.clone_into(&mut prefix),
            part => parts.push(part),
        }
    }
    let lead = if windows { prefix } else { String::new() };
    Some(format!("{lead}{separator}{}", parts.join(&separator.to_string())))
}

/// The image `image` names, as the transcript draws it.
pub(super) fn element(
    shared: &Rc<RefCell<MdImages>>,
    image: &InlineImage,
    id: SharedString,
    theme: &Theme,
) -> AnyElement {
    let label: SharedString = if image.alt.trim().is_empty() {
        image.source.rsplit(['/', '\\']).next().unwrap_or(&image.source).to_owned().into()
    } else {
        image.alt.clone().into()
    };
    let tile = render::in_gallery(&id);
    let folder = shared.borrow().folder.clone();
    let (value, words): (&str, String) = match resolve(&image.source, folder.as_deref()) {
        Resolved::Remote => ("remote", format!("{label} · {}", image.source)),
        Resolved::Refused => ("refused", format!("{label} · outside this session's folder")),
        Resolved::Path(path) => {
            let mut shared = shared.borrow_mut();
            let thumb = shared.thumbs.get(&path).cloned();
            shared.wanted.push(path);
            match thumb {
                Some(Thumb::Ready { image, aspect }) => {
                    let aspect = aspect.max(0.05);
                    let widest = if tile {
                        render::GALLERY_TILE
                    } else {
                        (MAX_HEIGHT * aspect).min(MAX_WIDTH)
                    };
                    return div()
                        .id(id)
                        .role(gpui::Role::Image)
                        .aria_label(label)
                        .w_full()
                        .max_w(px(widest))
                        .aspect_ratio(aspect)
                        // The transcript's one card corner and hairline.
                        .rounded(px(crate::transcript::TRANSCRIPT_CARD_RADIUS))
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme.border)
                        .child(img(image).size_full().object_fit(gpui::ObjectFit::Contain))
                        .into_any_element();
                }
                Some(Thumb::Failed) => ("missing", format!("{label} · not found")),
                Some(Thumb::Loading { aspect }) => {
                    return placeholder(id, label, tile, aspect, theme);
                }
                None => return placeholder(id, label, tile, None, theme),
            }
        }
    };
    div()
        .id(id)
        .role(gpui::Role::Image)
        .aria_label(label)
        .aria_value(value)
        .text_color(theme.text_muted)
        .child(words)
        .into_any_element()
}

/// An image's room while it loads: its final box once its first bytes said its shape
/// (so nothing under it moves when it arrives), else 4:3.
fn placeholder(
    id: SharedString,
    label: SharedString,
    tile: bool,
    aspect: Option<f32>,
    theme: &Theme,
) -> AnyElement {
    let id_key = id.clone();
    let base = div().id(id).role(gpui::Role::Image).aria_label(label).aria_value("loading");
    let sized = match aspect {
        Some(aspect) => {
            let aspect = aspect.max(0.05);
            let widest =
                if tile { render::GALLERY_TILE } else { (MAX_HEIGHT * aspect).min(MAX_WIDTH) };
            base.w_full().max_w(px(widest)).aspect_ratio(aspect)
        }
        None => base
            .when(tile, Styled::w_full)
            .when(!tile, |el| el.w(px(PLACEHOLDER_HEIGHT * 4.0 / 3.0)))
            .h(px(PLACEHOLDER_HEIGHT)),
    };
    sized
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(crate::transcript::TRANSCRIPT_CARD_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(crate::theme::ink(0.035))
        .child(crate::loaders::pending(format!("{id_key}-load"), "Loading image…"))
        .into_any_element()
}

impl Transcript {
    /// Hold the images the row just drawn showed, reading any not read yet; draw again when one
    /// the row showed loading is already here.
    pub(super) fn hold_md_images(
        &mut self,
        row_id: &SharedString,
        markdown: bool,
        cx: &mut Context<Self>,
    ) {
        let wanted = std::mem::take(&mut self.md_images.borrow_mut().wanted);
        let chip = SharedString::from(format!("{row_id}#dmd"));
        if wanted.is_empty() {
            if markdown {
                self.tool_images.release(&chip, Self::tool_images_of, cx);
            }
            return;
        }
        let mut paths = wanted;
        paths.dedup();
        let (device, _) = self.image_host(cx);
        let loader = crate::tool_images::Loader {
            engine: device.as_ref().and_then(|_| self.state.read(cx).engine().cloned()),
            local_device: self.state.read(cx).local_device_id.clone(),
        };
        let keys = paths
            .iter()
            .map(|path| crate::tool_images::ImageKey {
                device: device.clone().unwrap_or_default(),
                path: path.clone(),
            })
            .collect();
        let thumbs = self.tool_images.hold(&chip, keys, &loader, Self::tool_images_of, cx);
        let mut shared = self.md_images.borrow_mut();
        let mut changed = false;
        for (path, thumb) in paths.into_iter().zip(thumbs) {
            let was_ready = matches!(shared.thumbs.get(&path), Some(Thumb::Ready { .. }));
            let was_failed = matches!(shared.thumbs.get(&path), Some(Thumb::Failed));
            changed |= match &thumb {
                Thumb::Ready { .. } => !was_ready,
                Thumb::Failed => !was_failed,
                Thumb::Loading { .. } => false,
            };
            shared.thumbs.insert(path, thumb);
        }
        drop(shared);
        if changed {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path resolves inside the chat's folder; one that leaves it, or a folder unknown, is
    /// refused; a web address is left alone.
    #[test]
    fn an_image_path_stays_in_the_chats_folder() {
        let folder = Some("/work/app");
        assert_eq!(resolve("img/x.png", folder), Resolved::Path("/work/app/img/x.png".into()));
        assert_eq!(resolve("./img/../x.png", folder), Resolved::Path("/work/app/x.png".into()));
        assert_eq!(resolve("/work/app/a.png", folder), Resolved::Path("/work/app/a.png".into()));
        assert_eq!(resolve("../secret.png", folder), Resolved::Refused);
        assert_eq!(resolve("/etc/passwd", folder), Resolved::Refused);
        assert_eq!(resolve("/work/application/a.png", folder), Resolved::Refused);
        assert_eq!(resolve("../../../../x.png", folder), Resolved::Refused);
        assert_eq!(resolve("img/x.png", None), Resolved::Refused);
        assert_eq!(resolve("https://example.com/x.png", folder), Resolved::Remote);
        assert_eq!(
            resolve(r"img\x.png", Some(r"C:\work\app")),
            Resolved::Path(r"C:\work\app\img\x.png".into())
        );
    }
}
