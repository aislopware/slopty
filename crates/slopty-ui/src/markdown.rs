//! One markdown look for every surface that draws it with gpui-kit's `TextView`.
//!
//! An assistant's turn in a conversation ([`crate::terminal::conversation`]) and a note on the
//! canvas ([`crate::note`]) read the same, because both take their sizes, colours and corners
//! from the theme through [`style`].

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_kit::component::text::TextViewStyle;
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};

/// A piece of a Markdown text: prose for gpui-kit's `TextView`, or a fenced block drawn as
/// its own element so it can carry buttons.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Segment {
    /// Markdown between fenced blocks.
    Prose(String),
    /// One fenced block: the info string after the fence, and the lines inside.
    Code {
        /// The language after the opening fence (may be empty).
        lang: String,
        /// The block's lines, joined with `\n`.
        body: String,
    },
}

/// Split `markdown` into prose and fenced blocks (a fence is a line starting with ```` ``` ````,
/// indentation ignored; an unclosed fence runs to the end). Blank prose is dropped.
#[must_use]
pub fn segments(markdown: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut code: Option<(String, Vec<&str>)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        match code.take() {
            Some((lang, body)) if trimmed.starts_with("```") => {
                out.push(Segment::Code { lang, body: body.join("\n") });
            }
            Some((lang, mut body)) => {
                body.push(line);
                code = Some((lang, body));
            }
            None if trimmed.starts_with("```") => {
                if prose.iter().any(|l| !l.trim().is_empty()) {
                    out.push(Segment::Prose(prose.join("\n")));
                }
                prose.clear();
                code = Some((trimmed.trim_start_matches('`').trim().to_owned(), Vec::new()));
            }
            None => prose.push(line),
        }
    }
    if let Some((lang, body)) = code {
        out.push(Segment::Code { lang, body: body.join("\n") });
    }
    if prose.iter().any(|l| !l.trim().is_empty()) {
        out.push(Segment::Prose(prose.join("\n")));
    }
    out
}

/// What a "run" button does with the block's code (the surface's owner knows the shells).
pub type Run = Rc<dyn Fn(String, &mut App)>;

/// A fenced block as its own element, with buttons.
///
/// Its language, a "copy" button and — with `run` — a "run" button, over the code in the
/// mono face on the raised surface. `ids` are the copy and run buttons' element ids and
/// debug selectors; `scale` as for [`style`].
#[must_use]
pub fn code_block(
    ids: (String, String),
    lang: &str,
    body: &str,
    theme: &Theme,
    scale: f32,
    run: Option<Run>,
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    let button = |id: String, label: &'static str, text: &'static str| {
        div()
            .id(ElementId::Name(id.clone().into()))
            .debug_selector(move || id)
            .role(Role::Button)
            .aria_label(label)
            .px(px(spacing.xs * scale))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_muted))
            .hover(move |st| st.bg(hsla_alpha(s.text, alpha::HOVER)))
            // The surface under it may take a press as "edit me": the button keeps its own.
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .child(text)
    };
    let (copy_id, run_id) = ids;
    let text = body.to_owned();
    let copy = button(copy_id, "Copy code", "copy").on_click(move |_ev, _window, cx| {
        cx.stop_propagation();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
    });
    let run = run.map(|run| {
        let code = body.to_owned();
        button(run_id, "Run in shell", "run").on_click(move |_ev, _window, cx| {
            cx.stop_propagation();
            run(code.clone(), cx);
        })
    });
    div()
        .flex()
        .flex_col()
        .rounded(px(theme.radii.xs))
        .bg(hsla(s.raised))
        .px(px(spacing.sm * scale))
        .py(px(spacing.xs * scale))
        .font_family(mono)
        .text_size(px(theme.typography.small() * scale))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(lang.to_owned()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(spacing.xs * scale))
                        .children(run)
                        .child(copy),
                ),
        )
        .child(
            div()
                .whitespace_normal()
                .text_color(hsla(s.text))
                .child(SharedString::from(body.to_owned())),
        )
        .into_any_element()
}

/// Markdown on the theme's tokens.
///
/// Paragraphs one base unit apart, headings stepping down from the title size to the base,
/// code in the terminal mono at `small()` on the raised surface with `radii.xs` corners.
/// Colours come from the gpui-kit theme, which [`crate::kit::sync`] keeps on the same tokens.
///
/// `scale` multiplies every size, for a surface that is drawn at a zoom of its own (a canvas
/// note); the chrome passes `1.0`.
#[must_use]
pub fn style(theme: &Theme, mono: &str, scale: f32) -> TextViewStyle {
    let small = theme.typography.small() * scale;
    let code_block = gpui::StyleRefinement::default()
        .font_family(mono.to_owned())
        .text_size(px(small))
        .bg(hsla(theme.surfaces.raised))
        .rounded(px(theme.radii.xs))
        .px(px(theme.spacing.sm * scale))
        .py(px(theme.spacing.xs * scale));
    let inline_code = gpui::HighlightStyle {
        background_color: Some(hsla(theme.surfaces.raised)),
        color: Some(hsla(theme.surfaces.text)),
        ..gpui::HighlightStyle::default()
    };
    let (title, base) = (theme.typography.title(), theme.typography.ui_size);
    TextViewStyle {
        paragraph_gap: gpui::rems(theme.spacing.sm / base),
        heading_base_font_size: px(base * scale),
        heading_font_size: Some(std::sync::Arc::new(move |level: u8, _base| {
            px((title - f32::from(level.saturating_sub(1))).max(base) * scale)
        })),
        code_block,
        inline_code,
        ..TextViewStyle::default()
    }
}
