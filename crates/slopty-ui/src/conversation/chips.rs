//! The chips of what is attached to a composer's draft, for the conversation face and the
//! thread view alike.
//!
//! A pasted picture shows as the picture, a file by its name, each with a way to take it off
//! the draft and, while it uploads, how far it got. The chip is the one place the upload is
//! said: the tile's header leaves an attachment's out.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement as _, ObjectFit,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, px,
};
use slopty_theme::Theme;

use super::attach::Attachment;
use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};
use crate::kit;

/// The widest an attachment's chip grows, in points at zoom 1; a longer name is cut short.
const ATTACHMENT_WIDTH: f32 = 240.0;

/// The side of a pasted picture's chip, in points at zoom 1: a two-line row's height.
const THUMBNAIL: f32 = 40.0;

/// What a click on a chip's ✕ does: takes that attachment off the draft.
pub type Remove = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The chips, in a wrapping row over the field; `None` while nothing is attached.
///
/// `picture` gives a pasted picture's image by attachment, `remove` the click that takes one
/// off the draft.
pub fn row(
    theme: &Theme,
    zoom: f32,
    chips: &[Attachment],
    picture: &dyn Fn(u64) -> Option<Arc<gpui::Image>>,
    remove: &dyn Fn(u64) -> Remove,
) -> Option<AnyElement> {
    if chips.is_empty() {
        return None;
    }
    let z = |v: f32| px(v * zoom);
    let row = div()
        .id("composer-attachments")
        .debug_selector(|| "composer-attachments".to_owned())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(z(theme.spacing.xs))
        .pl(z(kit::FIELD_INSET))
        .children(chips.iter().map(|chip| match picture(chip.id) {
            Some(image) => picture_chip(theme, zoom, chip, image, remove(chip.id)),
            None => file_chip(theme, zoom, chip, remove(chip.id)),
        }));
    Some(row.into_any_element())
}

/// What a chip says to a screen reader: its name, and how far it got while it uploads.
fn label(chip: &Attachment) -> SharedString {
    if chip.landed() {
        SharedString::from(format!("Attached {}", chip.name))
    } else {
        SharedString::from(format!("Attaching {}, {}", chip.name, chip.progress()))
    }
}

/// The way to take `chip` off the draft, its ✕ `side` square.
fn remove_button(
    theme: &Theme,
    chip: &Attachment,
    side: Pixels,
    zoom: f32,
    remove: Remove,
) -> AnyElement {
    let s = theme.surfaces;
    let id = chip.id;
    let button = div()
        .id(SharedString::from(format!("attachment-remove-{id}")))
        .debug_selector(|| "composer-attachment-remove".to_owned())
        .role(Role::Button)
        .aria_label(SharedString::from(format!("Remove {}", chip.name)))
        .flex_none()
        .size(side)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
        .on_click(remove)
        .child(
            crate::icons::icon(theme, Symbol::Xmark, IconSize::Inline, hsla(s.text_muted))
                .size(px(theme.typography.icon() * zoom)),
        );
    crate::a11y::tab_stop(button, s.focus).into_any_element()
}

/// A pasted picture's chip: the picture itself in a small square on the hairline, its ✕ on a
/// lifted disc in the top corner, and while it uploads a progress line along its foot.
fn picture_chip(
    theme: &Theme,
    zoom: f32,
    chip: &Attachment,
    picture: Arc<gpui::Image>,
    remove: Remove,
) -> AnyElement {
    let s = theme.surfaces;
    let z = |v: f32| px(v * zoom);
    let disc = kit::elevate(div(), theme)
        .absolute()
        .top(z(theme.spacing.xxs))
        .right(z(theme.spacing.xxs))
        .rounded_full()
        .overflow_hidden()
        .hover(move |el| el.bg(hsla(s.hover)))
        .child(remove_button(theme, chip, z(theme.typography.icon_large()), zoom, remove));
    // How far it got, as the kit's capsule along the picture's foot, held in from its edges.
    let progress = (!chip.landed()).then(|| {
        div()
            .debug_selector(|| "composer-attachment-progress".to_owned())
            .absolute()
            .left(z(theme.spacing.xs))
            .right(z(theme.spacing.xs))
            .bottom(z(theme.spacing.xs))
            .child(
                kit::progress::Bar::new(
                    theme,
                    format!("attachment-bar-{}", chip.id),
                    kit::progress::Progress::Share(chip.fraction),
                )
                .height(z(theme.spacing.xs))
                .label(format!("Uploading {}", chip.name)),
            )
    });
    div()
        .id(SharedString::from(format!("attachment-{}", chip.id)))
        .debug_selector(|| "composer-attachment".to_owned())
        .role(Role::Image)
        .aria_label(label(chip))
        .relative()
        .flex_none()
        .size(z(THUMBNAIL))
        .rounded(z(theme.radii.sm))
        .overflow_hidden()
        .border(kit::HAIR)
        .border_color(hsla(s.border_subtle))
        .map(|el| kit::inset(el, theme))
        .child(img(picture).size_full().object_fit(ObjectFit::Cover))
        .children(progress)
        .child(disc)
        .into_any_element()
}

/// A file's chip: how far it got as a ring while it uploads (the file's glyph once it landed),
/// its name on the pill, and its ✕.
fn file_chip(theme: &Theme, zoom: f32, chip: &Attachment, remove: Remove) -> AnyElement {
    let s = theme.surfaces;
    let z = |v: f32| px(v * zoom);
    let side = (-2.0_f32).mul_add(theme.spacing.xxs, kit::PILL_HEIGHT);
    // While it uploads, its mark is a ring of how far it got; landed, the file's glyph.
    let mark = if chip.landed() {
        crate::icons::icon(theme, Symbol::Doc, IconSize::Inline, hsla(s.text_muted))
            .size(z(theme.typography.icon()))
            .into_any_element()
    } else {
        div()
            .debug_selector(|| "composer-attachment-progress".to_owned())
            .child(kit::progress::ring(
                theme,
                SharedString::from(format!("attachment-ring-{}", chip.id)),
                chip.fraction,
                s.accent_fill,
                z(theme.typography.icon()),
            ))
            .into_any_element()
    };
    kit::pill_frame(theme, zoom)
        .id(SharedString::from(format!("attachment-{}", chip.id)))
        .debug_selector(|| "composer-attachment".to_owned())
        .role(Role::Status)
        .aria_label(label(chip))
        .max_w(z(ATTACHMENT_WIDTH))
        // The way off sits in the pill's own end, a pad's width from its edge.
        .pr(z(theme.spacing.xxs))
        .map(|el| kit::inset(el, theme))
        .text_color(hsla(s.text_secondary))
        .child(mark)
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(SharedString::from(chip.name.clone())),
        )
        .child(
            div()
                .rounded(z(theme.radii.xs))
                .hover(move |el| el.bg(hsla(s.hover)))
                .child(remove_button(theme, chip, z(side), zoom, remove)),
        )
        .into_any_element()
}
