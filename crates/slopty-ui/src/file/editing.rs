//! A file tile's own editing commands over gpui-kit's editor: comment toggling in the file's
//! language, lines moved and copied, the bracket pair at the caret, "go to line", soft wrap,
//! and the indentation read from the file. The text work is [`super::edit`]'s; this applies it.

use std::ops::Range;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement as _,
    KeyBinding, MouseButton, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{
    Input, InputEvent, InputState, RangeDecoration, RangeDecorationStyle, Rope, RopeExt as _,
    TabSize,
};
use slopty_theme::alpha;

use super::edit::{self, Indent, LineEdit};
use super::{
    CloseGoToLine, DuplicateLine, FileView, GoToLine, JumpToBracket, MoveLineDown, MoveLineUp,
    ToggleComment, TogglePreview, ToggleReplace, ToggleSoftWrap,
};
use crate::colors::{hsla, hsla_alpha};
use crate::highlight::Syntax;
use crate::palette::PaletteItem;

/// The "go to line" field's placeholder.
pub(crate) const GO_TO_LINE: &str = "Line, or line:column";
/// The key context of the "go to line" field.
pub const GO_TO_CTX: &str = "FileGoTo";
/// The key context round the editor itself, apart from the tile's fields (find, go to line):
/// the editing commands bind in `FileText > Input`.
pub const TEXT_CTX: &str = "FileText";

/// The open "go to line" field.
pub(super) struct GoTo {
    input: Entity<InputState>,
    /// The selection when it opened, put back by Esc.
    origin: Range<usize>,
    _subscription: Subscription,
}

/// The palette's lines for the editor's own commands, `bindings` giving their chords.
#[must_use]
pub fn palette_items(bindings: &[KeyBinding]) -> Vec<PaletteItem> {
    let line =
        |label: &str, action: Box<dyn gpui::Action>| PaletteItem::new(label, action, bindings);
    vec![
        line("Toggle comment", Box::new(ToggleComment)),
        line("Jump to line", Box::new(GoToLine)),
        line("Move line up", Box::new(MoveLineUp)),
        line("Move line down", Box::new(MoveLineDown)),
        line("Duplicate line", Box::new(DuplicateLine)),
        line("Jump to matching bracket", Box::new(JumpToBracket)),
        line("Wrap long lines", Box::new(ToggleSoftWrap)),
        line("Show preview or source", Box::new(TogglePreview)),
        line("Find and replace", Box::new(ToggleReplace)),
    ]
}

impl FileView {
    /// The tile's handlers for the editor's own commands, while it shows text.
    pub(super) fn editing_keys(el: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        el.on_action(cx.listener(|this, _: &ToggleComment, window, cx| {
            this.toggle_comment(window, cx);
        }))
        .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.go_to_line(window, cx)))
        .on_action(
            cx.listener(|this, _: &MoveLineUp, window, cx| this.move_lines(true, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &MoveLineDown, window, cx| this.move_lines(false, window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &DuplicateLine, window, cx| this.duplicate_lines(window, cx)),
        )
        .on_action(cx.listener(|this, _: &JumpToBracket, _, cx| this.jump_to_bracket(cx)))
        .on_action(cx.listener(|this, _: &ToggleSoftWrap, _, cx| this.toggle_soft_wrap(cx)))
        .on_action(
            cx.listener(|this, _: &ToggleReplace, window, cx| this.toggle_replace(window, cx)),
        )
    }

    /// How the file indents, as read from it: what Tab puts in.
    #[must_use]
    pub const fn indent(&self) -> Indent {
        self.indent
    }

    /// How the file differs from UTF-8 with `\n` line ends ("CRLF", "UTF-8 with BOM"), kept on
    /// save; none for the usual.
    #[must_use]
    pub fn format_label(&self) -> Option<String> {
        self.base.as_ref().and_then(|b| b.format.label())
    }

    /// What the foot line says of the file's layout: its indentation ("Spaces: 4", "Tabs"),
    /// then its line ends and BOM when they are not the usual.
    #[must_use]
    pub fn layout_facts(&self) -> Vec<String> {
        std::iter::once(self.indent.label()).chain(self.format_label()).collect()
    }

    /// Whether long lines wrap at the tile's width.
    #[must_use]
    pub const fn soft_wrap(&self) -> bool {
        self.wrap
    }

    /// The bracket pair the caret is at, as byte offsets, opening first.
    #[must_use]
    pub const fn bracket_pair(&self) -> Option<(usize, usize)> {
        self.bracket
    }

    /// Whether the editor holds the text and takes edits: drawn, not still waiting for a read
    /// to land in it.
    pub(super) const fn editable(&self) -> bool {
        self.shows_text() && self.pending_text.is_none() && self.read_only.is_none()
    }

    /// Replace a block of lines as one undo step, then place the selection.
    pub(super) fn apply(&mut self, edit: LineEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            e.set_selected_range(edit.range, cx);
            e.replace(edit.text, window, cx);
            e.set_selected_range(edit.selection, cx);
        });
        // The editor says nothing of a change made through `replace`.
        self.edited(cx);
    }

    /// An edit worked out from the text and the selection, applied when there is one.
    fn edit_with(
        &mut self,
        make: impl FnOnce(&Rope, &Range<usize>) -> Option<LineEdit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let edit = {
            let editor = self.editor.read(cx);
            make(editor.text(), &editor.selected_range())
        };
        if let Some(edit) = edit {
            self.apply(edit, window, cx);
        }
    }

    /// ⌘/: comment the selected lines in the file's language, or uncomment them when they all
    /// are. A file whose grammar has no comment is left alone.
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self.editor.read(cx).text().slice_line(0).to_string();
        // Past the colouring cap the tile has no grammar for colours; the comment still has one.
        let Some(comment) =
            self.syntax.or_else(|| Syntax::for_path(&self.path, &first)).and_then(Syntax::comment)
        else {
            return;
        };
        self.edit_with(
            |text, selection| edit::toggle_comment(text, selection, comment),
            window,
            cx,
        );
    }

    /// ⌥↑ / ⌥↓: swap the selected lines with their neighbour.
    pub fn move_lines(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_with(|text, selection| edit::move_lines(text, selection, up), window, cx);
    }

    /// ⌥⇧↓: the selected lines again below them.
    pub fn duplicate_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_with(|text, selection| Some(edit::duplicate_lines(text, selection)), window, cx);
    }

    /// ⌘⇧\: from one bracket of the pair at the caret to the other.
    pub fn jump_to_bracket(&self, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        self.editor.update(cx, |e, cx| {
            let caret = e.cursor();
            let Some((open, close)) = edit::matching_bracket(e.text(), caret) else { return };
            let to = if caret <= open.saturating_add(1) { close } else { open };
            e.set_selected_range(to..to, cx);
        });
        cx.notify();
    }

    /// Wrap long lines at the tile's width, or stop.
    pub fn toggle_soft_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap = !self.wrap;
        cx.notify();
    }

    /// The editor takes the file's indentation (after a read put new text in).
    pub(super) fn apply_indent(&self, cx: &mut Context<Self>) {
        let Indent { hard_tabs, width } = self.indent;
        self.editor.update(cx, |e, cx| e.set_tab_size(TabSize { tab_size: width, hard_tabs }, cx));
    }

    /// Find the bracket pair at the caret again when the caret or the text moved; the tint
    /// follows it. Cheap when nothing moved, and bounded by [`edit::BRACKET_SCAN_BYTES`] when
    /// it did.
    pub(super) fn refresh_bracket(&mut self, cx: &mut Context<Self>) {
        let at = {
            let e = self.editor.read(cx);
            let selection = e.selected_range();
            (self.editable() && selection.is_empty()).then_some((selection.start, self.edit))
        };
        if at == self.bracket_at {
            return;
        }
        self.bracket_at = at;
        let pair =
            at.and_then(|(caret, _)| edit::matching_bracket(self.editor.read(cx).text(), caret));
        if pair != self.bracket {
            self.bracket = pair;
            self.remark(cx);
        }
    }

    /// ⌃G: open the "go to line" field (the find bar closes); the caret follows what is typed.
    pub fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        // Quietly: the find bar's close hands the keyboard to the editor, and this field wants it.
        if self.search.take().is_some() {
            self.remark(cx);
        }
        if self.goto.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(GO_TO_LINE));
            let subscription =
                cx.subscribe_in(&input, window, |this, input, event, window, cx| match event {
                    InputEvent::Change => {
                        let typed = input.read(cx).value().to_string();
                        this.preview_line(&typed, cx);
                    }
                    InputEvent::PressEnter { .. } => this.close_go_to(false, window, cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                });
            let origin = self.editor.read(cx).selected_range();
            self.goto = Some(GoTo { input, origin, _subscription: subscription });
        }
        if let Some(goto) = &self.goto {
            goto.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Whether the "go to line" field is open.
    #[must_use]
    pub const fn going_to_line(&self) -> bool {
        self.goto.is_some()
    }

    /// The caret to the place `typed` names, if it names one.
    fn preview_line(&self, typed: &str, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            if let Some(at) = edit::line_target(typed, e.text()) {
                e.set_selected_range(at..at, cx);
            }
        });
    }

    /// Close the field: ↩ keeps the caret where it went, Esc (`restore`) puts it back. The
    /// editor takes the keyboard again.
    pub fn close_go_to(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(goto) = self.goto.take() else { return };
        self.editor.update(cx, |e, cx| {
            if restore {
                let end = e.text().len();
                e.set_selected_range(goto.origin.start.min(end)..goto.origin.end.min(end), cx);
            }
            e.focus(window, cx);
        });
        cx.notify();
    }

    /// The "go to line" field over the top-right corner, where the find bar sits, with the
    /// file's line count after it.
    pub(super) fn render_goto(&self, goto: &GoTo, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let lines = self.editor.read(cx).text().lines_len();
        let count: SharedString = format!("of {lines}").into();
        div()
            .id("file-goto")
            .debug_selector(|| "file-goto".to_owned())
            .key_context(GO_TO_CTX)
            .absolute()
            .top(px(spacing.sm))
            .right(px(spacing.sm))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(radii.sm))
            .map(|el| crate::kit::elevate(el, theme))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .on_action(cx.listener(|this, _: &CloseGoToLine, window, cx| {
                this.close_go_to(true, window, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .role(Role::Group)
            .aria_label("Go to line")
            .child(div().w(px(180.0)).child(Input::new(&goto.input).aria_label("Go to line")))
            .child(
                div()
                    .id("file-goto-count")
                    .min_w(px(40.0))
                    .text_color(hsla(s.text_secondary))
                    .role(Role::Label)
                    .aria_label("Lines")
                    .aria_value(count.clone())
                    .child(count),
            )
            .into_any_element()
    }

    /// The tint of the bracket pair at the caret: a hairline frame round each, in the text's
    /// muted tone, so it reads without a colour of its own. Brackets are one byte each.
    pub(super) fn bracket_marks(&self) -> Vec<RangeDecoration> {
        let Some(pair) = self.bracket else { return Vec::new() };
        let tone = hsla_alpha(self.theme.surfaces.text_muted, alpha::STRONG);
        <[usize; 2]>::from(pair)
            .map(|at| {
                RangeDecoration::new(at..at.saturating_add(1))
                    .with_style(RangeDecorationStyle::Frame)
                    .with_color(tone)
            })
            .into()
    }
}

/// The indentation and wrap a file opens with: read from its text, and wrapped when it is
/// prose (Markdown), where a paragraph is one long line.
pub(super) fn opening_layout(text: &str, syntax: Option<Syntax>) -> (Indent, bool) {
    (edit::detect_indent(text), syntax.is_some_and(is_prose))
}

/// Whether the grammar is prose's (Markdown): wrapped, and offered no words.
pub(super) fn is_prose(syntax: Syntax) -> bool {
    syntax.name() == "Markdown"
}
