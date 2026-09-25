//! gpui-kit's theme on Slopty's tokens.
//!
//! The widgets borrowed from gpui-kit (inputs, the composer's textarea, Markdown `TextView`)
//! colour themselves from gpui-kit's own global [`gpui_kit::component::Theme`]. Rather than
//! keep two palettes, [`sync`] switches that theme to the matching mode and then writes the
//! Slopty surface tokens over the colours those widgets read, so a text field, a code block
//! or a link looks like the chrome around it in both variants.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_kit::base::text::TextViewDefaults;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use slopty_theme::{Theme, Variant, alpha};

use crate::colors::{hsla, hsla_alpha};

/// What a find bar says before anything is typed. The terminal and the file card share it: the
/// same bar, the same word.
pub const FIND_PLACEHOLDER: &str = "Find";

/// How large an overlay grows on a desktop.
///
/// A phone gets whatever the margins leave. Two sizes, because a list of commands and a file
/// being edited want different room, and a third would be a size nobody could name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    /// A list typed at: the command palette, the pickers.
    List,
    /// A document edited: the settings file.
    Editor,
}

impl Overlay {
    /// Width and height ceilings, in points.
    #[must_use]
    pub const fn bounds(self) -> (f32, f32) {
        match self {
            Self::List => (560.0, 520.0),
            Self::Editor => (640.0, 720.0),
        }
    }
}

/// The backdrop an overlay sits on: the canvas dimmed, the dialog near the top.
///
/// Near the top so a phone's keyboard, which rises from the bottom, covers fewer of its rows;
/// under the window's safe area, so a phone's status bar and Dynamic Island never sit on it.
///
/// The caller adds the identity, the key handling and the dismiss on a click through.
#[must_use]
pub fn backdrop(theme: &Theme, window: &Window) -> Div {
    let safe = window.insets().effective();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(theme.spacing.xl * 2.0) + safe.top)
        .bg(hsla_alpha(theme.surfaces.canvas, alpha::SCRIM))
}

/// The shell every overlay wears: one radius, one hairline border, one elevation, the UI
/// font. `min_w_0` so an unwrapped title cannot hold the box wider than a phone.
///
/// The caller adds the identity, the accessibility role and label, and the children.
#[must_use]
pub fn dialog(theme: &Theme, size: Overlay) -> Div {
    let (w, h) = size.bounds();
    let s = &theme.surfaces;
    div()
        .w_full()
        .min_w_0()
        .max_w(px(w))
        .max_h(px(h))
        .mx(px(theme.spacing.md))
        .flex()
        .flex_col()
        .rounded(px(theme.radii.md))
        .border_1()
        .border_color(hsla(s.border))
        .bg(hsla(s.panel))
        .shadow_sm()
        .text_size(px(theme.typography.ui_size))
        .font_family(theme.typography.ui_family.clone())
        .text_color(hsla(s.text))
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
}

/// How loud a [`button`] is. One primary per surface; the rest are secondary, or ghost where
/// a frame would crowd a bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// The one action the surface is for: an accent fill.
    Primary,
    /// Another way on: the panel with a hairline, so it holds its edge on any surface.
    Secondary,
    /// A way out (Cancel): text only until the pointer is on it.
    Ghost,
    /// A way aside (the other way in, Open in editor): accent text with no pad, so its words
    /// start on the same edge as the text above them.
    Link,
}

/// A text button, the one every dialog, panel and empty state draws.
///
/// Four had been written by hand, and the secondary among them filled itself with `raised`,
/// which on the light canvas is one step from the canvas itself: "Add a window" and the phone's
/// "Paste" were words floating on a smudge. Every kind wears a 1 pt border (clear on a ghost or
/// a link) so they all stand the same height side by side and the keyboard's focus hairline
/// moves nothing.
#[must_use]
pub fn button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    kind: ButtonKind,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::accesskit::Role::Button)
        .aria_label(label)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(kind != ButtonKind::Link, |el| el.px(px(theme.spacing.md)).border_1())
        .when(kind == ButtonKind::Link, |el| el.border_t_1().border_b_1())
        .py(px(theme.spacing.xs))
        .rounded(px(theme.radii.sm))
        .text_size(px(theme.typography.ui_size))
        .cursor_pointer()
        .child(label);
    let el = match kind {
        ButtonKind::Primary => {
            el.border_color(hsla(s.accent)).bg(hsla(s.accent)).text_color(hsla(s.accent_fg))
        }
        ButtonKind::Secondary => el
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .text_color(hsla(s.text))
            .hover(move |el| el.bg(hsla(s.raised)))
            .active(move |el| el.bg(hsla(s.overlay))),
        ButtonKind::Ghost => el
            .border_color(gpui::transparent_black())
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
            .active(move |el| el.bg(hsla(s.overlay))),
        ButtonKind::Link => el
            .border_color(gpui::transparent_black())
            .text_color(hsla(s.accent))
            .hover(gpui::Styled::underline),
    };
    crate::a11y::tab_stop(el, s.accent)
}

/// A square button around one icon: a bar's actions, a tile's close and split.
///
/// Ghost like a bar's text buttons, with `label` as its accessible name and its hint, since the
/// icon alone names nothing to a screen reader.
#[must_use]
pub fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::IconName,
    label: &'static str,
) -> gpui::Stateful<Div> {
    icon_button_at(theme, id, icon, label, 1.0)
}

/// [`icon_button`] at the chrome's zoom `k`: a tile's header shrinks with the overview, and a
/// button drawn at full size there would outgrow the bar it sits in.
#[must_use]
pub fn icon_button_at(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::IconName,
    label: &'static str,
    k: f32,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let id: SharedString = id.into();
    let selector = id.to_string();
    let side = 2.0_f32.mul_add(theme.spacing.xs, theme.typography.icon_large()) * k;
    let el = div()
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector)
        .role(gpui::accesskit::Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(side))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm * k))
        .cursor_pointer()
        .text_color(hsla(s.text_secondary))
        .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
        .active(move |el| el.bg(hsla(s.overlay)))
        .child(
            crate::icons::icon(theme, icon, crate::icons::IconSize::Inline, hsla(s.text_secondary))
                .size(px(theme.typography.icon() * k)),
        );
    crate::a11y::tab_stop(el, s.accent)
}

/// A key cap: the keys on a small raised plate, the way the empty workspace teaches its chords
/// and the palette's foot names its keys. `keys` comes from the key tables, or is a lone key.
#[must_use]
pub fn key_cap(theme: &Theme, keys: impl Into<SharedString>) -> Div {
    let s = &theme.surfaces;
    div()
        .flex_none()
        .px(px(theme.spacing.xs))
        .rounded(px(theme.radii.xs))
        .border_1()
        .border_color(hsla(s.border))
        .bg(hsla(s.raised))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(s.text_secondary))
        .child(keys.into())
}

/// What a bar button does, and the key that does it, shown after a pause on the pointer.
///
/// The bar prints no keys of its own. Six ⌘ chords across one strip was most of the text up
/// there and none of it answered what a button does; zed and warp print none and keep the index
/// in the command palette, which Slopty already has. A hint needs a pointer to hover, so this is
/// the Mac's affordance — on a phone the palette is the only one, as it always was.
#[derive(Debug)]
pub struct Hint {
    what: SharedString,
    key: SharedString,
    theme: Rc<Theme>,
}

impl Hint {
    /// `what` the button does ("New shell"), and the `key` that does it ("⌘T").
    #[must_use]
    pub fn new(
        what: impl Into<SharedString>,
        key: impl Into<SharedString>,
        theme: Rc<Theme>,
    ) -> Self {
        Self { what: what.into(), key: key.into(), theme }
    }
}

impl Render for Hint {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        div()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xxs))
            .rounded(px(theme.radii.sm))
            .border_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.raised))
            .shadow_sm()
            .text_size(px(theme.typography.small()))
            .font_family(theme.typography.ui_family.clone())
            .child(div().text_color(hsla(s.text)).child(self.what.clone()))
            .child(div().text_color(hsla(s.text_muted)).child(self.key.clone()))
    }
}

/// Point gpui-kit's theme at `theme`: mode, then the colours its widgets read.
pub fn sync(theme: &Theme, cx: &mut App) {
    let mode = match theme.variant() {
        Variant::Dark => ThemeMode::Dark,
        Variant::Light => ThemeMode::Light,
    };
    KitTheme::change(mode, None, cx);
    let s = &theme.surfaces;
    let kit = KitTheme::global_mut(cx);
    let c = &mut kit.colors;
    c.background = hsla(s.panel);
    c.foreground = hsla(s.text);
    c.border = hsla(s.border);
    c.input = hsla(s.border);
    c.ring = hsla(s.accent);
    c.caret = hsla(s.accent);
    c.selection = hsla_alpha(s.accent, alpha::TINT);
    c.accent = hsla(s.raised);
    c.accent_foreground = hsla(s.text);
    c.muted = hsla(s.raised);
    c.muted_foreground = hsla(s.text_muted);
    c.secondary = hsla(s.raised);
    c.secondary_foreground = hsla(s.text);
    c.secondary_hover = hsla(s.overlay);
    c.secondary_active = hsla(s.overlay);
    c.primary = hsla(s.accent);
    c.primary_foreground = hsla(s.accent_fg);
    c.link = hsla(s.accent);
    c.link_hover = hsla(s.accent);
    c.link_active = hsla(s.accent);
    c.popover = hsla(s.panel);
    c.popover_foreground = hsla(s.text);
    c.list = hsla(s.panel);
    c.list_hover = hsla(s.raised);
    c.list_active = hsla(s.overlay);
    c.danger = hsla(s.error);
    c.success = hsla(s.success);
    c.warning = hsla(s.warn);
    c.scrollbar_thumb = hsla_alpha(s.text_muted, alpha::PRESSED);
    // Focus is one accent hairline. gpui-kit's ring is a second, wider halo painted outside
    // the field's border, and the first-run field wore both.
    kit.focus_ring = false;
    KitTheme::sync_base(cx);
    // `sync_base` reinstalls the Markdown defaults, so the code colouring goes on after it.
    TextViewDefaults::global(cx)
        .with_code_block_highlighter(crate::highlight::code_block(theme.clone()))
        .install(cx);
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    /// Chrome text is sentence case, whatever it is doing: a label, a button's accessible name,
    /// a field's placeholder and an empty state are all written the same way. The stream HUD and
    /// the agent pill are readouts of a number or a state, not chrome, and stay lowercase.
    #[test]
    fn chrome_text_is_sentence_case() {
        let chrome = [
            FIND_PLACEHOLDER,
            crate::workspace::INSTALL_HOOKS,
            crate::workspace::TAKE_OVER,
            crate::note::WRITE_PLACEHOLDER,
            crate::file::CHANGED_ON_DISK,
            crate::file::RELOAD,
            crate::file::OVERWRITE,
            crate::palette::NO_COMMAND_MATCHES,
            crate::picker::FILTER_PLACEHOLDER,
            crate::picker::NOTHING_MATCHES,
            crate::picker::NOTHING_TO_JUMP_TO,
            crate::picker::LOADING_WINDOWS,
            crate::terminal::BACK_TO_LIVE,
            crate::workspace::RECONNECTING,
            crate::workspace::SESSION_ENDED,
            crate::workspace::CLOSE_TILE,
            crate::workspace::FULLSCREEN_TILE,
            crate::workspace::EMPTY_WORKSPACE,
            crate::workspace::NO_WORKERS,
            crate::workspace::NO_WORKERS_NEXT,
            crate::workspace::NEW_WORKSPACE,
            crate::screen::waiting_text(slopty_proto::screen::SourceState::Idle),
            crate::screen::waiting_text(slopty_proto::screen::SourceState::Live),
        ];
        for text in chrome {
            let first = text.chars().next().unwrap_or(' ');
            assert!(first.is_uppercase(), "chrome text starts lowercase: {text:?}");
            let rest: String = text.chars().skip(1).collect();
            assert!(!rest.contains(char::is_uppercase), "chrome text is title case: {text:?}");
        }
    }

    /// Both variants land in gpui-kit's theme: mode and the token colours.
    #[gpui::test]
    fn the_kit_theme_follows_the_tokens(cx: &TestAppContext) {
        cx.update(gpui_kit::init);
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            cx.update(|cx| sync(&theme, cx));
            cx.update(|cx| {
                let kit = KitTheme::global(cx);
                assert_eq!(kit.mode.is_dark(), variant == Variant::Dark);
                assert_eq!(kit.colors.background, hsla(theme.surfaces.panel));
                assert_eq!(kit.colors.foreground, hsla(theme.surfaces.text));
                assert_eq!(kit.colors.primary, hsla(theme.surfaces.accent));
                assert_eq!(kit.colors.primary_foreground, hsla(theme.surfaces.accent_fg));
                assert_eq!(kit.colors.border, hsla(theme.surfaces.border));
                assert_eq!(kit.colors.ring, hsla(theme.surfaces.accent));
                assert!(!kit.focus_ring, "a focused field is one hairline, not a halo");
                assert!(
                    TextViewDefaults::global(cx).has_code_block_highlighter(),
                    "fenced code is coloured after the sync"
                );
            });
        }
    }

    /// The chrome under `dir`, without its test modules: every line of Rust up to the first
    /// `#[cfg(test)]`, with its file and one-based line number.
    fn chrome_lines(dir: &str) -> Vec<(String, usize, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, usize, String)>) {
            let entries = std::fs::read_dir(dir).expect("the crate's sources are readable");
            for entry in entries.flatten() {
                let path = entry.path();
                // Test modules in files of their own are not chrome either.
                let is_tests = path.file_stem().is_some_and(|n| n == "tests");
                if is_tests {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("a source file reads");
                    let name = path.display().to_string();
                    for (ix, line) in text.lines().enumerate() {
                        if line.contains("#[cfg(test)]") {
                            break;
                        }
                        out.push((name.clone(), ix.saturating_add(1), line.to_owned()));
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits in the workspace")
            .join(dir);
        let mut out = Vec::new();
        walk(&root, &mut out);
        assert!(!out.is_empty(), "found no sources under {dir}");
        out
    }

    /// A padding, margin or gap written as a literal rather than taken from `theme.spacing`.
    ///
    /// Only these: a literal `w`/`h`/`size` is a measurement of something real (a hairline, an
    /// icon box, a panel that has to be some width), while a literal pad is a rhythm chosen in
    /// one place and nowhere else, which is how a scale of 2/4/8/12/16/24 quietly becomes a
    /// scale of every number. Returns the offending call for the message.
    fn literal_spacing(line: &str) -> Option<String> {
        const PAD: [&str; 13] = [
            ".p(", ".px(", ".py(", ".pt(", ".pb(", ".pl(", ".pr(", ".m(", ".mx(", ".my(", ".gap(",
            ".gap_x(", ".gap_y(",
        ];
        PAD.iter()
            .find(|call| {
                line.split(*call).skip(1).any(|rest| {
                    rest.strip_prefix("px(")
                        .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
                })
            })
            .map(|call| format!("`{call}px(N)`"))
    }

    /// The ruling in `docs/decisions/ui.md`, as a check rather than a paragraph: chrome takes
    /// its transparencies from `alpha` and wears one elevation. A raw opacity or a second
    /// shadow is how a design system becomes a pile of one-offs, so neither compiles.
    #[test]
    fn chrome_paints_from_the_tokens() {
        // Every step is written `0.` or `1.0`, so a digit after the comma is a raw opacity.
        let raw_alpha = |line: &str| {
            line.contains("hsla_alpha(")
                && (line.contains(", 0.") || line.contains(", 1.0") || line.contains("{ 1.0 }"))
        };
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                if raw_alpha(&line) {
                    wrong.push(format!("{file}:{line_no}: a raw opacity, not an `alpha` step"));
                }
                for other in ["shadow_md()", "shadow_lg()", "shadow_xl()", "shadow_2xl()"] {
                    if line.contains(other) {
                        wrong.push(format!("{file}:{line_no}: a second elevation ({other})"));
                    }
                }
                if let Some(call) = literal_spacing(&line) {
                    wrong.push(format!("{file}:{line_no}: {call} off the spacing scale"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The string literals on a line of Rust, escapes left as written.
    fn literals(line: &str) -> Vec<String> {
        let code = line.trim_start();
        if code.starts_with("//") {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut current: Option<String> = None;
        let mut escaped = false;
        for c in code.chars() {
            match current.as_mut() {
                None if c == '"' => current = Some(String::new()),
                None => {}
                Some(text) if escaped => {
                    text.push(c);
                    escaped = false;
                }
                Some(text) if c == '\\' => {
                    text.push(c);
                    escaped = true;
                }
                Some(_) if c == '"' => out.extend(current.take()),
                Some(text) => text.push(c),
            }
        }
        out
    }

    /// A chord written into chrome text: a literal holding a modifier glyph and anything
    /// else. A lone glyph is a key cap on the phone's bar, not a chord.
    fn literal_chord(line: &str) -> Option<String> {
        literals(line)
            .into_iter()
            .find(|text| text.contains(['⌘', '⌥', '⌃', '⇧']) && text.chars().count() > 1)
    }

    /// The ruling in `docs/decisions/ui.md`: keys live in the palette, the menus and the
    /// hints, all of which read them from the binding tables through `palette::keys_for`.
    /// A chord typed into a button, an empty state or a menu row by hand drifts from the
    /// binding (the "+" menu said `⌘⇧T` while the palette said `⇧⌘T`) and puts keys where
    /// the palette should be, so none compiles.
    #[test]
    fn a_chord_is_spelled_only_by_the_key_tables() {
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                if let Some(text) = literal_chord(&line) {
                    wrong.push(format!("{file}:{line_no}: a chord written by hand: {text:?}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_chord_check_knows_a_chord_from_a_key_cap() {
        assert!(literal_chord(r#"button("Save", "⌘↩", true)"#).is_some());
        assert!(literal_chord(r#".child("⌘T opens a shell")"#).is_some());
        assert!(literal_chord(r#"("⌘", "cmd", None),"#).is_none(), "a key cap");
        assert!(literal_chord("/// ⌘⌥← walks the columns").is_none(), "a comment");
        assert!(literal_chord(r#"out.push('⌘'); label("a")"#).is_none(), "a char, not text");
        assert!(literal_chord(r#"f("a\"b", "⇧x")"#).is_some(), "past an escaped quote");
    }

    /// The spacing check catches what it is for and leaves alone what it is not.
    ///
    /// A lint that cannot fail is worse than no lint, because it reads like cover.
    #[test]
    fn the_spacing_check_knows_a_pad_from_a_measurement() {
        assert!(literal_spacing(".p(px(7.0))").is_some());
        assert!(literal_spacing("div().gap(px(10.0)).child(x)").is_some());
        assert!(literal_spacing(".py(px(3.))").is_some());
        // Taken from the scale: the whole point.
        assert!(literal_spacing(".p(px(theme.spacing.md))").is_none());
        assert!(literal_spacing(".gap(px(s.xs))").is_none());
        // A measurement of something real, not a rhythm.
        assert!(literal_spacing(".w(px(300.0))").is_none());
        assert!(literal_spacing(".border_b(px(1.0))").is_none());
        assert!(literal_spacing(".size(px(16.0))").is_none());
        // `.pr(` must not be found inside `.appear(` or any other word ending in those letters.
        assert!(literal_spacing("something.expr(px(4.0))").is_none());
    }

    /// The two overlay sizes differ in both directions, and a list is the smaller of them: a
    /// size that is nearly another size is a size nobody chose.
    #[test]
    fn the_overlay_sizes_are_two_and_they_differ() {
        let (lw, lh) = Overlay::List.bounds();
        let (ew, eh) = Overlay::Editor.bounds();
        assert!(lw < ew && lh < eh, "a list is the smaller overlay");
        assert!(ew - lw >= 40.0 && eh - lh >= 40.0, "far enough apart to tell apart");
    }
}
