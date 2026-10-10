//! A reviewed file that is more than its lines, or has none to read: a rename, a mode change,
//! a picture, a file too large to cut into hunks.
//!
//! - **A rename** names where the file was, in its head: "Renamed from old.rs" when it kept its
//!   folder, "Moved from src/old.rs" when it did not.
//! - **A mode change** says what it means in its head ("Made executable", "Now a symbolic link").
//! - **Either with no change to its lines** says only that where its lines would be: the head has
//!   said the rest.
//! - **A picture** shows its two sides beside each other, each side's bytes asked of the worker by
//!   its blob the first time the row is drawn (`GitOp::Blob`) and decoded once.
//! - **A text file too large to cut** says its size, with a press that opens it whole in a tile of
//!   its own.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, div, img, px,
};
use slopty_proto::git::GitOp;
use slopty_proto::thread::wire::{FileDiff, FileKind, Modes};

use super::{ReviewEvent, ReviewView};
use crate::colors::hsla;
use crate::conversation::thread::git::Blob;
use crate::icons::{IconSize, Symbol};
use crate::kit;

/// A symbolic link's mode, as git writes it.
const LINK: u32 = 0o120_000;

/// The execute bits.
const EXECUTE: u32 = 0o111;

/// How tall a picture's side is drawn, in points.
const PICTURE_HEIGHT: f32 = 160.0;

/// The press that opens a file too large to show in a tile of its own.
pub(in crate::review) const OPEN_WHOLE: &str = "Open the whole file";

/// What a file's change of mode means, as its head says it.
#[must_use]
pub(in crate::review) fn mode_words(modes: Modes) -> String {
    let executable = |mode: u32| mode != LINK && mode & EXECUTE != 0;
    match (modes.from, modes.to) {
        (_, LINK) => "Now a symbolic link".to_owned(),
        (LINK, _) => "No longer a symbolic link".to_owned(),
        (from, to) if !executable(from) && executable(to) => "Made executable".to_owned(),
        (from, to) if executable(from) && !executable(to) => "No longer executable".to_owned(),
        (from, to) => format!("Mode {from:o} \u{2192} {to:o}"),
    }
}

/// Where a renamed file was, as its head says it: its old name when it kept its folder, else
/// its old path.
#[must_use]
pub(in crate::review) fn renamed_words(file: &FileDiff) -> Option<String> {
    let old = file.old_path.as_deref()?;
    let folder = |path: &str| path.rsplit_once('/').map_or("", |(dir, _)| dir).to_owned();
    if folder(old) == folder(&file.path) {
        let name = old.rsplit_once('/').map_or(old, |(_, name)| name);
        Some(format!("Renamed from {name}"))
    } else {
        Some(format!("Moved from {old}"))
    }
}

/// What a file's head says of it besides its name: added, removed, renamed, its mode.
#[must_use]
pub(in crate::review) fn head_words(file: &FileDiff) -> Option<String> {
    let status = match (&file.from, &file.to) {
        (None, Some(_)) => Some("Added".to_owned()),
        (Some(_), None) => Some("Removed".to_owned()),
        _ => renamed_words(file),
    };
    let mode = file.modes.map(mode_words);
    match (status, mode) {
        (Some(status), Some(mode)) => Some(format!("{status} \u{b7} {mode}")),
        (status, mode) => status.or(mode),
    }
}

/// What the row of a file with no hunks says, where it is not a picture or a file too large.
#[must_use]
pub(in crate::review) fn bare_words(file: &FileDiff) -> String {
    match file.kind {
        FileKind::Binary => "Binary file".to_owned(),
        FileKind::TooLarge { bytes } => too_large(bytes),
        FileKind::Image { bytes } => format!("Picture, {}", kit::size_label(bytes)),
        // The head says the rename or the mode already; the row says only what is not there.
        FileKind::Text if file.modes.is_some() || file.old_path.is_some() => {
            "No change to its lines".to_owned()
        }
        FileKind::Text => "No lines to show".to_owned(),
    }
}

/// What the row of a text file too large to cut into hunks says.
#[must_use]
pub(in crate::review) fn too_large(bytes: u64) -> String {
    format!("{}, too large to show its changes here", kit::size_label(bytes))
}

/// The format gpui decodes a picture at `path` in, by its extension.
fn picture_format(path: &str) -> Option<gpui::ImageFormat> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => gpui::ImageFormat::Png,
        "jpg" | "jpeg" => gpui::ImageFormat::Jpeg,
        "gif" => gpui::ImageFormat::Gif,
        "webp" => gpui::ImageFormat::Webp,
        "bmp" => gpui::ImageFormat::Bmp,
        "tif" | "tiff" => gpui::ImageFormat::Tiff,
        "ico" => gpui::ImageFormat::Ico,
        "svg" => gpui::ImageFormat::Svg,
        _ => return None,
    })
}

/// How a picture's side stands.
enum Shown {
    /// Its bytes are on their way, or not asked yet.
    Coming,
    /// Decoded.
    Drawn(Arc<gpui::Image>),
    /// It could not be read, or gpui does not decode its kind.
    Failed(String),
}

impl ReviewView {
    /// The pictures' sides decoded so far, by blob, in order.
    #[cfg(test)]
    pub(in crate::review) fn pictures_held(&self) -> Vec<String> {
        let mut held: Vec<String> = self.pictures.borrow().keys().cloned().collect();
        held.sort();
        held
    }

    /// The row of a file with no hunks to show that is more than words: a picture's two sides,
    /// or a file too large with its press; `None` for one that is words alone.
    pub(super) fn sides_row(&self, at: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let file = self.model.file(at)?;
        match file.kind {
            FileKind::Image { bytes } => Some(self.picture_row(at, file, bytes, cx)),
            FileKind::TooLarge { bytes } => Some(self.too_large_row(at, file, bytes, cx)),
            FileKind::Text | FileKind::Binary => None,
        }
    }

    /// A picture's sides, before and after, beside each other, and its size under them.
    fn picture_row(
        &self,
        at: usize,
        file: &FileDiff,
        bytes: u64,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let sides = [("Before", file.from.as_deref()), ("After", file.to.as_deref())];
        let panes =
            sides.into_iter().filter_map(|(side, blob)| Some((side, blob?))).map(|(side, blob)| {
                let shown = self.picture_side(&file.path, blob, cx);
                let label = format!("{side}: {}", file.path);
                let slug = side.to_ascii_lowercase();
                let body = match shown {
                    Shown::Drawn(picture) => {
                        img(picture).size_full().object_fit(ObjectFit::Contain).into_any_element()
                    }
                    Shown::Coming => div().into_any_element(),
                    Shown::Failed(why) => div()
                        .p(px(theme.spacing.sm))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(why))
                        .into_any_element(),
                };
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.xxs))
                    .child(div().text_color(hsla(s.text_muted)).child(side))
                    .child(
                        div()
                            .id(ElementId::Name(format!("review-picture-{slug}-{at}").into()))
                            .debug_selector(move || format!("review-picture-{slug}-{at}"))
                            .role(Role::Image)
                            .aria_label(SharedString::from(label))
                            .h(px(PICTURE_HEIGHT))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .rounded(px(theme.radii.sm))
                            .border(kit::HAIR)
                            .border_color(hsla(s.stroke))
                            .map(|el| kit::inset(el, theme))
                            .child(body),
                    )
            });
        div()
            .debug_selector(move || format!("review-pictures-{at}"))
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.sm))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .text_size(px(theme.typography.small()))
            .child(div().flex().gap(px(theme.spacing.md)).children(panes))
            .child(
                div()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(format!("Picture, {}", kit::size_label(bytes)))),
            )
            .into_any_element()
    }

    /// One side of a picture: decoded once its bytes came; asked for the first time it is
    /// drawn.
    fn picture_side(&self, path: &str, blob: &str, cx: &Context<Self>) -> Shown {
        if let Some(held) = self.pictures.borrow().get(blob) {
            return Shown::Drawn(Arc::clone(held));
        }
        let Some(repo) = self.repo(cx) else { return Shown::Coming };
        let came = self.hub.read(cx).git().repo(&repo).and_then(|r| r.blobs.get(blob).cloned());
        match came {
            Some(Blob::Came(bytes)) => {
                let Some(format) = picture_format(path) else {
                    return Shown::Failed("This kind of picture is not drawn here".to_owned());
                };
                let picture = Arc::new(gpui::Image::from_bytes(format, bytes.to_vec()));
                self.pictures.borrow_mut().insert(blob.to_owned(), Arc::clone(&picture));
                Shown::Drawn(picture)
            }
            Some(Blob::Failed(why)) => Shown::Failed(format!("It could not be read: {why}")),
            None => {
                if self.blobs_asked.borrow_mut().insert(blob.to_owned()) {
                    let op = GitOp::Blob { blob: blob.to_owned() };
                    cx.spawn(async move |this, cx| {
                        let _gone = this.update(cx, |view, cx| {
                            let Some(repo) = view.repo(cx) else { return };
                            let _asked = view.hub.update(cx, |hub, cx| hub.git_op(&repo, op, cx));
                        });
                    })
                    .detach();
                }
                Shown::Coming
            }
        }
    }

    /// A text file too large to cut: its size, and the press that opens it whole, while it has
    /// a side to open and the repository's root is known.
    fn too_large_row(
        &self,
        at: usize,
        file: &FileDiff,
        bytes: u64,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let opens = file.to.is_some() && self.root(cx).is_some();
        let id = format!("review-open-whole-{at}");
        div()
            .debug_selector(move || format!("review-too-large-{at}"))
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.sm))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(too_large(bytes)))
            .when(opens, |el| {
                let selector = id.clone();
                // A link's look, as the settings' links have: the words in the text's tone,
                // underlined under the pointer, then the glyph.
                let link = div()
                    .id(ElementId::Name(id.into()))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label(OPEN_WHOLE)
                    .self_start()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .cursor_pointer()
                    .text_color(hsla(s.text))
                    .hover(gpui::Styled::underline)
                    .child(OPEN_WHOLE)
                    .child(
                        crate::icons::icon(
                            theme,
                            Symbol::ArrowUpRight,
                            IconSize::Inline,
                            hsla(s.text_muted),
                        )
                        .size(px(theme.typography.icon())),
                    )
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        if let Some(path) = this.whole_path(at, cx) {
                            cx.emit(ReviewEvent::OpenFile { path });
                        }
                    }));
                el.child(crate::a11y::tab_stop(link, s.focus))
            })
            .into_any_element()
    }
}
