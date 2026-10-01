//! The in-app settings: `settings.toml` as a form of sections and rows that applies each change
//! as it is made, with the file itself one click away, saved on ⌘↩.
//!
//! The phone has no editor to hand the file to, and on the Mac a small change should not
//! need one either. The dialog opens on the form ([`crate::settings_form`]): a sidebar of
//! sections and a page of rows, each a label, a line on what it does and its control. "Edit as
//! TOML" swaps the page for the file's text (the commented defaults when there is none) in a
//! monospace field, and "Edit with controls" swaps it back. Both edit one text: a row writes its
//! key into it a line at a time, and the form reads the field's text when it comes back, so
//! neither can hold a value the other does not.
//!
//! The form applies each change as it is made ([`SettingsEditorEvent::Apply`]): the app writes
//! the file and reloads it, and the dialog stays open, as System Settings, Zed's settings and
//! Linear's do. Done, Escape or a click outside closes it, writing first what was still
//! waiting for a pause. The file's face keeps an explicit save: ⌘↩ or the Save button hands the
//! text back ([`SettingsEditorEvent::Save`]), which the app parses and either writes, applies
//! and closes, or puts the parse error above the foot and keeps the dialog open so the typo can
//! be fixed. There, Escape, Cancel or a click outside discards what was typed. On the Mac an "Open
//! in editor" link keeps the old path to the default `.toml` editor. The field is the file tile's
//! editor with its TOML colours and line numbers, so a parse error's line is one glance away.
//!
//! The field is as tall as the file (between [`MIN_ROWS`] and [`MAX_ROWS`] lines), not a
//! fixed share of the window: two lines of TOML in a window-high box was mostly empty field.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::Size;
use gpui_kit::component::input::{Editor, EditorState, Enter, Escape, InputEvent};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::kit::ButtonKind;
use crate::settings_form::{SettingsForm, SettingsFormEvent};

/// Key context of the dialog.
pub const CTX: &str = "SettingsEditor";

/// The fewest lines the field shows: room to add a table to a short file.
pub const MIN_ROWS: u16 = 8;

/// The most lines the field shows before it scrolls.
pub const MAX_ROWS: u16 = 28;

/// How many lines the field shows for `text`: its lines and one to type on, within
/// [`MIN_ROWS`] and [`MAX_ROWS`].
#[must_use]
pub fn rows_for(text: &str) -> u16 {
    u16::try_from(text.lines().count().saturating_add(1))
        .unwrap_or(u16::MAX)
        .clamp(MIN_ROWS, MAX_ROWS)
}

/// What the user asked of the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsEditorEvent {
    /// The form changed the file: write this text and apply it, and keep the dialog open.
    Apply(String),
    /// The file's face was saved: parse this text, and when it holds, write it, apply it and
    /// close.
    Save(String),
    /// Hand the file to the system's editor instead.
    OpenExternally,
    /// Closed: the form's changes were already applied, the file's face unsaved is dropped.
    Dismiss,
}

/// Which face of the file the dialog shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Sections of rows with their controls.
    Form,
    /// The file's text.
    Toml,
}

/// The dialog, its form and its field.
#[derive(Debug)]
pub struct SettingsEditor {
    text: Entity<EditorState>,
    form: Entity<SettingsForm>,
    mode: Mode,
    /// The file's name, shown beside the title on the file's face; the whole path is its
    /// accessible name. A temp or sandbox path is seventy characters of noise to read and wraps
    /// on a phone. The form's title is "Settings" alone: its foot already offers the file.
    file_name: SharedString,
    /// Where the file lives.
    path: SharedString,
    /// Offer "Open in editor" (the Mac; the phone has nothing to open it with).
    external: bool,
    /// Why the last save was refused.
    error: Option<String>,
    /// How many lines the field shows: [`rows_for`] the text, kept as it is typed.
    rows: u16,
    theme: Theme,
    _subscriptions: [Subscription; 2],
}

impl SettingsEditor {
    /// An editor over `text`, the file at `path`, open on the form.
    pub fn new(
        text: &str,
        path: &str,
        external: bool,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .folding(false)
                .line_number_gap(px(theme.spacing.md))
                .default_value(text.to_owned());
            state.set_searchable(false, cx);
            state
        });
        install_highlighter(&state, &theme, cx);
        let typed = cx.subscribe(&state, |this, _state, event, cx| match event {
            // Typing past an error is the fix in progress; the line goes on the next save. A
            // line added or taken away grows or shrinks the dialog with the file.
            InputEvent::Change => this.text_changed(&this.text(cx), cx),
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        });
        let form = cx.new(|cx| SettingsForm::new(text, theme.clone(), window, cx));
        let set = cx.subscribe_in(&form, window, |this, _form, event, window, cx| match event {
            SettingsFormEvent::Apply(text) => this.apply(text, window, cx),
            SettingsFormEvent::Done => this.close(cx),
        });
        Self {
            text: state,
            form,
            mode: Mode::Form,
            file_name: SharedString::from(
                std::path::Path::new(path)
                    .file_name()
                    .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned()),
            ),
            path: SharedString::from(path.to_owned()),
            external,
            error: None,
            rows: rows_for(text),
            theme,
            _subscriptions: [typed, set],
        }
    }

    /// The file's text, as the field and the form both hold it.
    #[must_use]
    pub fn text(&self, cx: &gpui::App) -> String {
        self.text.read(cx).value().to_string()
    }

    /// Which face the dialog shows.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Give the keyboard to what the dialog shows: the form's search, or the end of the field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Form => self.form.update(cx, |form, cx| form.focus(window, cx)),
            Mode::Toml => self.text.update(cx, |state, cx| {
                let end = state.text().len();
                state.set_selected_range(end..end, cx);
                state.focus(window, cx);
            }),
        }
    }

    /// Show the file's text, with the keyboard in it, once the form's last change is applied.
    pub fn show_toml(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.form.update(cx, SettingsForm::flush) {
            self.apply(&text, window, cx);
        }
        self.mode = Mode::Toml;
        self.focus(window, cx);
        cx.notify();
    }

    /// Show the form over what the field holds now, with the keyboard in its search.
    pub fn show_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        self.form.update(cx, |form, cx| form.set_text(&text, window, cx));
        self.mode = Mode::Form;
        self.focus(window, cx);
        cx.notify();
    }

    /// Show the form on `section`'s page.
    pub fn show_section(
        &mut self,
        section: crate::settings_form::schema::Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Form {
            self.show_form(window, cx);
        }
        self.form.update(cx, |form, cx| form.show(section, window, cx));
        cx.notify();
    }

    /// The app refused the text: show why, keep editing.
    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    /// The error above the foot, if any.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// New tokens.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            install_highlighter(&self.text, &theme, cx);
            self.form.update(cx, |form, cx| form.set_theme(theme.clone(), cx));
            self.theme = theme;
            cx.notify();
        }
    }

    /// The text changed under the field or the form: an edit is the fix in progress, and the
    /// field follows the file's length.
    fn text_changed(&mut self, text: &str, cx: &mut Context<Self>) {
        let rows = rows_for(text);
        if self.error.is_some() || rows != self.rows {
            self.error = None;
            self.rows = rows;
            cx.notify();
        }
    }

    /// The form's `text`: the field holds it too, and the app writes and applies it.
    fn apply(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.text.update(cx, |state, cx| state.set_value(text, window, cx));
        self.text_changed(text, cx);
        cx.emit(SettingsEditorEvent::Apply(text.to_owned()));
    }

    fn save(&self, cx: &mut Context<Self>) {
        cx.emit(SettingsEditorEvent::Save(self.text(cx)));
    }

    /// Close: on the form, after applying what was still waiting for a pause; on the file's
    /// face, dropping what was not saved.
    fn close(&self, cx: &mut Context<Self>) {
        if self.mode == Mode::Form
            && let Some(text) = self.form.update(cx, SettingsForm::flush)
        {
            cx.emit(SettingsEditorEvent::Apply(text));
        }
        cx.emit(SettingsEditorEvent::Dismiss);
    }

    /// Escape from a control or a button closes: a text field's own Escape action covers it
    /// while it has the keyboard, captured below.
    fn key_down(this: &mut Self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key != "escape" || this.composing(cx) {
            return;
        }
        let typing = match this.mode {
            Mode::Form => this.form.read(cx).typing(window, cx),
            Mode::Toml => this.text.read(cx).focus_handle(cx).is_focused(window),
        };
        if !typing {
            this.close(cx);
            cx.stop_propagation();
        }
    }
}

impl SettingsEditor {
    /// Whether an input method holds uncommitted text in the TOML or in a field of the form.
    fn composing(&self, cx: &gpui::App) -> bool {
        self.text.read(cx).is_composing() || self.form.read(cx).composing(cx)
    }
}

/// TOML's colours in `theme`.
fn install_highlighter<T>(editor: &Entity<EditorState>, theme: &Theme, cx: &mut Context<T>) {
    let toml = crate::highlight::Syntax::for_token("toml");
    let factory = crate::highlight::editor::factory(toml, theme.clone());
    editor.update(cx, |e, cx| {
        e.set_highlighter_factory(factory, cx);
        e.set_highlighter("toml", cx);
    });
}

impl EventEmitter<SettingsEditorEvent> for SettingsEditor {}

impl Focusable for SettingsEditor {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        match self.mode {
            Mode::Form => self.form.read(cx).focus_handle(cx),
            Mode::Toml => self.text.read(cx).focus_handle(cx),
        }
    }
}

impl Render for SettingsEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let (s, spacing) = (theme.surfaces, theme.spacing);
        // A button says what it does; the keys (Esc, ⌘↩) are the palette's to list.
        let button = |id, label, kind| crate::kit::button(&theme, id, label, kind);
        let error = self.error.clone().map(|error| {
            crate::kit::inset_x(div(), &theme)
                .id("settings-error")
                .debug_selector(|| "settings-error".to_owned())
                .role(gpui::accesskit::Role::Alert)
                .aria_label(error.clone())
                .py(px(spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.error))
                .child(SharedString::from(error))
        });
        let body = match self.mode {
            // The form asks for its page's height and gives some up to a short window; the
            // page scrolls inside.
            Mode::Form => div()
                .flex_initial()
                .min_h_0()
                .h(px(SettingsForm::height(&theme)))
                .flex()
                .child(self.form.clone()),
            // The file's text at the document size and line: the field is as tall as its
            // lines, and gives up height to a short window (a phone's keyboard), scrolling
            // inside.
            Mode::Toml => {
                let line = theme.typography.mono_size * theme.typography.markdown_line_height;
                let field = px(f32::from(self.rows) * line) + Size::Medium.input_py() * 2.0;
                div()
                    .flex_initial()
                    .min_h_0()
                    .h(field + px(spacing.sm * 2.0))
                    // The text starts on the edge grid: the field pads itself by its size's
                    // inset, and this makes up the rest.
                    .px(px(spacing.inset() - crate::kit::FIELD_INSET))
                    .py(px(spacing.sm))
                    .font_family(crate::palette::mono_family(&theme))
                    .child(
                        Editor::new(&self.text)
                            .appearance(false)
                            .bordered(false)
                            .aria_label("settings.toml")
                            .text_size(px(theme.typography.mono_size))
                            .line_height(px(line))
                            .h(gpui::relative(1.0)),
                    )
            }
        };
        let swap = match self.mode {
            Mode::Form => button("settings-edit-toml", "Edit as TOML", ButtonKind::Link)
                .on_click(cx.listener(|this, _ev, window, cx| this.show_toml(window, cx))),
            Mode::Toml => button("settings-edit-form", "Edit with controls", ButtonKind::Link)
                .on_click(cx.listener(|this, _ev, window, cx| this.show_form(window, cx))),
        };
        let external = self.external && self.mode == Mode::Toml;
        // The form has nothing to save, since each change is applied as it is made; the file's
        // face has, and Cancel drops it.
        let actions = div().flex().items_center().gap(px(spacing.xs)).map(|el| match self.mode {
            Mode::Form => el.child(
                button("settings-done", "Done", ButtonKind::Primary)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.close(cx))),
            ),
            Mode::Toml => el
                .child(button("settings-cancel", "Cancel", ButtonKind::Ghost).on_click(
                    cx.listener(|_this, _ev, _w, cx| cx.emit(SettingsEditorEvent::Dismiss)),
                ))
                .child(
                    button("settings-save", "Save", ButtonKind::Primary)
                        .on_click(cx.listener(|this, _ev, _w, cx| this.save(cx))),
                ),
        });
        let dialog = crate::kit::dialog(&theme, crate::kit::Overlay::Editor)
            .id("settings-editor")
            .debug_selector(|| "settings-editor".to_owned())
            .role(gpui::accesskit::Role::Dialog)
            .aria_label("Settings")
            .child(
                crate::kit::inset_x(div(), &theme)
                    .flex_none()
                    .flex()
                    .items_baseline()
                    .gap(px(spacing.sm))
                    .py(px(spacing.sm))
                    .border_b_1()
                    .border_color(hsla(s.border))
                    .child(crate::kit::title(&theme, "Settings"))
                    .when(self.mode == Mode::Toml, |el| {
                        el.child(
                            div()
                                .id("settings-path")
                                .debug_selector(|| "settings-path".to_owned())
                                .aria_label(self.path.clone())
                                .text_size(px(theme.typography.small()))
                                .text_color(hsla(s.text_muted))
                                .child(self.file_name.clone()),
                        )
                    }),
            )
            .child(body)
            .children(error)
            .child(
                crate::kit::inset_x(div(), &theme)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(spacing.md))
                    .py(px(spacing.sm))
                    .border_t_1()
                    .border_color(hsla(s.border))
                    .child(swap)
                    .when(external, |el| {
                        el.child(
                            button("settings-open-external", "Open in editor", ButtonKind::Link)
                                .on_click(cx.listener(|_this, _ev, _w, cx| {
                                    cx.emit(SettingsEditorEvent::OpenExternally);
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(actions),
            );
        // The dialog rises the base unit's half into place as it fades in, as the palette does.
        let dialog =
            crate::kit::slide_fade(dialog, "settings-rise", spacing.xs, crate::kit::Pace::Fade, cx);
        let root = crate::kit::backdrop(&theme, window)
            .id("settings-backdrop")
            .key_context(CTX)
            .on_key_down(cx.listener(Self::key_down))
            // While an input method composes in a field, Esc and Enter are its own.
            .capture_action(cx.listener(|this, _: &Escape, _window, cx| {
                if !this.composing(cx) {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            // ⌘↩ (a field's `secondary-enter`: ⌘ on macOS and iOS alike) saves the file's face and
            // closes the form; captured so the field does not also break the line. A control that
            // is not a field asks through the form.
            .capture_action(cx.listener(|this, enter: &Enter, _window, cx| {
                if enter.secondary && !this.composing(cx) {
                    match this.mode {
                        Mode::Form => this.close(cx),
                        Mode::Toml => this.save(cx),
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _w, cx| {
                    this.close(cx);
                    cx.stop_propagation();
                }),
            )
            .child(dialog);
        gpui::deferred(root).with_priority(crate::palette::Layer::Dialog.priority())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{KeyUpEvent, Keystroke, TestAppContext, VisualTestContext};

    use super::*;
    use crate::settings_form::PRESS_KEYS;
    use crate::settings_form::schema::{self, Section, rows};

    /// A window over `text` showing `mode`, the events it raised, and its context. gpui-kit is
    /// set up by the first one a test opens.
    fn editor<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
        external: bool,
        mode: Mode,
    ) -> (Entity<SettingsEditor>, Rc<RefCell<Vec<SettingsEditorEvent>>>, &'a mut VisualTestContext)
    {
        if !cx.update(|cx| cx.has_global::<gpui_kit::component::Theme>()) {
            cx.update(gpui_kit::init);
        }
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&events);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = SettingsEditor::new(
                text,
                "~/settings.toml",
                external,
                Theme::default(),
                window,
                cx,
            );
            match mode {
                Mode::Form => view.focus(window, cx),
                Mode::Toml => view.show_toml(window, cx),
            }
            view
        });
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event: &SettingsEditorEvent, _cx| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        (view, events, cx)
    }

    /// Row `key` of `table` in the form's list.
    fn row(table: &str, key: &str) -> usize {
        rows().iter().position(|r| r.table() == table && r.key() == key).expect("a row for the key")
    }

    /// A selector built at run time, for a row's index.
    fn leak(selector: String) -> &'static str {
        Box::leak(selector.into_boxed_str())
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("no {selector}"));
        cx.simulate_click(at.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    }

    /// A key pressed and let go: a keyboard click needs the release.
    fn press(cx: &mut VisualTestContext, key: &str) {
        cx.simulate_keystrokes(key);
        let keystroke = Keystroke::parse(key).expect("a keystroke");
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
    }

    fn focused(cx: &mut VisualTestContext) -> (String, Option<String>) {
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let node = tree.into_iter().find(|n| n.focused).expect("a focused node");
        (node.role, node.label)
    }

    fn value_of(cx: &mut VisualTestContext, role: &str, label: &str) -> Option<String> {
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter().find(|n| n.is(role, Some(label))).and_then(|n| n.value)
    }

    /// The dialog opens on the form: sections in a sidebar, the first section's rows, the
    /// search with the keyboard, and the file one link away. The form has nothing to save, so
    /// its foot is Done.
    #[gpui::test]
    fn the_dialog_opens_on_the_form(cx: &mut TestAppContext) {
        let (view, _events, cx) = editor(cx, "", true, Mode::Form);
        assert_eq!(view.read_with(cx, |v, _| v.mode()), Mode::Form);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Dialog", Some("Settings"))), "{tree:#?}");
        for section in Section::ALL.map(Section::label) {
            assert!(tree.iter().any(|n| n.is("Tab", Some(section))), "{section}: {tree:#?}");
        }
        assert!(tree.iter().any(|n| n.is("RadioGroup", Some("Theme"))), "{tree:#?}");
        assert!(!tree.iter().any(|n| n.is("ComboBox", Some("Family"))), "another section's row");
        for label in ["Edit as TOML", "Done"] {
            assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
        }
        for label in ["Open in editor", "Cancel", "Save"] {
            assert!(!tree.iter().any(|n| n.is("Button", Some(label))), "{label} is the file's");
        }
        let (role, label) = focused(cx);
        assert!(role.ends_with("TextInput"), "{role}");
        assert_eq!(label.as_deref(), Some(crate::settings_form::SEARCH_PLACEHOLDER));
        assert_eq!(value_of(cx, "RadioGroup", "Theme").as_deref(), Some("System"));
        assert_eq!(value_of(cx, "SpinButton", "Text size").as_deref(), Some("13 pt"));
    }

    /// A change applies as it is made, and the dialog stays open: a switch hands the app the
    /// file's new text at once, every other line as it was; a run of stepper clicks hands it
    /// over once, when the clicks pause. The file written from it opens on the same values,
    /// and a line typed in the field shows in the form when it comes back.
    #[gpui::test]
    fn a_change_applies_as_it_is_made_and_the_dialog_stays_open(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("settings.toml");
        let file = "# mine\n[font]\nmono_size = 13.0 # the size I like\n";
        std::fs::write(&path, file).expect("the file");

        {
            let (view, events, cx) = editor(cx, file, true, Mode::Form);
            // The app's half of an apply: the text goes to the file, as `settings::save` does.
            let applied = |events: &Rc<RefCell<Vec<SettingsEditorEvent>>>| {
                let events = events.borrow();
                assert!(!events.contains(&SettingsEditorEvent::Dismiss), "still open");
                match events.last() {
                    Some(SettingsEditorEvent::Apply(text)) => {
                        std::fs::write(&path, text).expect("written");
                        text.clone()
                    }
                    other => panic!("{other:?}"),
                }
            };
            click(cx, "settings-section-1");
            assert!(value_of(cx, "ComboBox", "Family").is_some(), "the terminal's rows");
            click(cx, leak(format!("settings-switch-{}", row("font", "ligatures"))));
            assert_eq!(
                applied(&events),
                "# mine\n[font]\nmono_size = 13.0 # the size I like\nligatures = false\n",
                "at once, the comments kept"
            );
            let increase = leak(format!("settings-increase-{}", row("font", "mono_size")));
            click(cx, increase);
            click(cx, increase);
            assert_eq!(value_of(cx, "SpinButton", "Size").as_deref(), Some("15 pt"));
            assert_eq!(events.borrow().len(), 1, "the clicks wait for a pause");
            cx.executor().advance_clock(crate::settings_form::SETTLE);
            cx.run_until_parked();
            assert_eq!(events.borrow().len(), 2, "one apply for the run");
            assert_eq!(
                applied(&events),
                "# mine\n[font]\nmono_size = 15.0 # the size I like\nligatures = false\n"
            );
            assert_eq!(
                view.read_with(cx, SettingsEditor::text),
                std::fs::read_to_string(&path).expect("the file")
            );
            assert!(cx.debug_bounds("settings-editor").is_some(), "the dialog is still up");
        }

        let reread = std::fs::read_to_string(&path).expect("the file again");
        let (view, _events, cx) = editor(cx, &reread, true, Mode::Form);
        click(cx, "settings-section-1");
        assert_eq!(value_of(cx, "SpinButton", "Size").as_deref(), Some("15 pt"));
        let form = view.read_with(cx, |v, _| v.form.clone());
        let off =
            form.read_with(cx, |f, _| slopty_settings::edit::read(f.text(), "font", "ligatures"));
        assert_eq!(off, Some(slopty_settings::edit::Value::Bool(false)));

        click(cx, "settings-edit-toml");
        assert_eq!(view.read_with(cx, SettingsEditor::text), reread, "one text behind both");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("[theme]");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("appearance = \"dark\"");
        cx.run_until_parked();
        click(cx, "settings-edit-form");
        click(cx, "settings-section-0");
        assert_eq!(value_of(cx, "RadioGroup", "Theme").as_deref(), Some("Dark"));
    }

    /// A value its key does not take is never handed over: the row says why in place of its
    /// line, until it is typed again, and the fixed value applies once the typing pauses.
    #[gpui::test]
    fn an_invalid_value_is_not_written_and_its_row_says_why(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "[colors]\ncursor = \"\"\n", false, Mode::Form);
        let ix = row("colors", "cursor");
        click(cx, leak(format!("settings-field-{ix}")));
        cx.simulate_input("#12");
        cx.executor().advance_clock(crate::settings_form::SETTLE);
        cx.run_until_parked();
        assert!(events.borrow().is_empty(), "nothing to apply: {:?}", events.borrow());
        let form = view.read_with(cx, |v, _| v.form.clone());
        assert_eq!(form.read_with(cx, |f, _| f.text().to_owned()), "[colors]\ncursor = \"\"\n");
        let error = form.read_with(cx, |f, _| f.error(ix).map(str::to_owned)).expect("a reason");
        assert!(error.contains("#rrggbb"), "{error}");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Alert", Some(&error))), "{tree:#?}");

        cx.simulate_input("3456");
        cx.run_until_parked();
        assert_eq!(form.read_with(cx, |f, _| f.error(ix).map(str::to_owned)), None, "typing on");
        cx.executor().advance_clock(crate::settings_form::SETTLE);
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [SettingsEditorEvent::Apply("[colors]\ncursor = \"#123456\"\n".to_owned())]
        );
    }

    /// The keyboard walks the form: ↓ from the search to the rows and ↑ back, a query narrows
    /// the rows across sections, Space turns a switch, ← and → move a choice and a stepper, ↓
    /// in the sidebar moves to the next section and → goes into it, and ⌘↩ on a control is
    /// Done, applying the stepper that was still waiting first.
    #[gpui::test]
    fn the_keyboard_walks_the_form(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "", false, Mode::Form);
        cx.simulate_input("ligat");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Switch", Some("Ligatures"))), "{tree:#?}");
        assert!(!tree.iter().any(|n| n.is("RadioGroup", Some("Theme"))), "{tree:#?}");
        press(cx, "down");
        assert_eq!(focused(cx), ("Switch".to_owned(), Some("Ligatures".to_owned())));
        press(cx, "space");
        let text = |cx: &mut VisualTestContext| view.read_with(cx, SettingsEditor::text);
        assert_eq!(text(cx), "[font]\nligatures = false\n");
        press(cx, "up");
        assert_eq!(focused(cx).1.as_deref(), Some(crate::settings_form::SEARCH_PLACEHOLDER));

        cx.simulate_keystrokes("cmd-a backspace");
        cx.run_until_parked();
        press(cx, "down");
        assert_eq!(focused(cx), ("RadioGroup".to_owned(), Some("Theme".to_owned())));
        press(cx, "right");
        assert_eq!(value_of(cx, "RadioGroup", "Theme").as_deref(), Some("Light"));
        press(cx, "down");
        assert_eq!(focused(cx), ("SpinButton".to_owned(), Some("Text size".to_owned())));
        press(cx, "right");
        press(cx, "right");
        press(cx, "left");
        assert_eq!(value_of(cx, "SpinButton", "Text size").as_deref(), Some("14 pt"));

        let form = view.read_with(cx, |v, _| v.form.clone());
        cx.update(|window, cx| {
            let tab = form.read(cx).tab_handle(Section::Appearance).expect("a tab");
            window.focus(&tab, cx);
        });
        press(cx, "down");
        assert_eq!(form.read_with(cx, |f, _| f.section()), Section::Terminal);
        assert_eq!(focused(cx), ("Tab".to_owned(), Some("Terminal".to_owned())));
        press(cx, "right");
        assert_eq!(focused(cx), ("ComboBox".to_owned(), Some("Family".to_owned())));
        assert_eq!(events.borrow().len(), 2, "the switch and the choice, the stepper waiting");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        let events = events.borrow();
        let [.., SettingsEditorEvent::Apply(last), SettingsEditorEvent::Dismiss] =
            events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert!(last.contains("appearance = \"light\""), "{last}");
        assert!(last.contains("ui_size = 14.0"), "{last}");
        assert!(last.contains("ligatures = false"), "{last}");
    }

    /// A query that matches nothing says so, and one that matches a key's file name finds it.
    #[gpui::test]
    fn a_search_finds_rows_by_their_words_and_their_key(cx: &mut TestAppContext) {
        let (_view, _events, cx) = editor(cx, "", false, Mode::Form);
        cx.simulate_input("zzz");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let quiet = crate::settings_form::NO_MATCHES;
        assert!(tree.iter().any(|n| n.is("Status", Some(quiet))), "{tree:#?}");
        cx.simulate_keystrokes("cmd-a backspace");
        cx.simulate_input("max_bitrate");
        cx.run_until_parked();
        assert_eq!(value_of(cx, "SpinButton", "Bitrate ceiling").as_deref(), Some("30 Mb/s"));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Heading", Some("Streams"))), "{tree:#?}");
    }

    /// Every row on a page stands one height, its line on what it does cut to one line, and
    /// the page's first label stands on the search field's line, so the columns start together.
    #[gpui::test]
    fn rows_hold_one_line_and_the_columns_start_together(cx: &mut TestAppContext) {
        let (_view, _events, cx) = editor(cx, "", false, Mode::Form);
        let search = cx.debug_bounds("settings-search").expect("the search");
        let label = cx.debug_bounds("settings-heading-0").expect("the first label");
        assert!(f32::from((search.top() - label.top()).abs()) < 0.5, "{search:?} {label:?}");
        assert!(f32::from((search.size.height - label.size.height).abs()) < 0.5);
        let heights: Vec<f32> = rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.section == Section::Appearance)
            .map(|(ix, _)| {
                let row = cx.debug_bounds(leak(format!("settings-row-{ix}"))).expect("a row");
                f32::from(row.size.height)
            })
            .collect();
        assert!(heights.len() > 4, "{heights:?}");
        assert!(heights.iter().all(|h| (h - heights[0]).abs() < 0.5), "{heights:?}");
    }

    /// The pages beside the sections. Keyboard lists every command of the keymap, in the
    /// palette's words on key caps, and a query finds one among the rows; About names the
    /// version and the build and links out. The form's title is "Settings" alone, and the
    /// file's face names the file.
    #[gpui::test]
    fn keyboard_and_about_read_what_is_bound_and_built(cx: &mut TestAppContext) {
        let (view, _events, cx) = editor(cx, "", true, Mode::Form);
        assert!(cx.debug_bounds("settings-path").is_none(), "the form's title is Settings alone");

        click(cx, leak(format!("settings-section-{}", Section::Keyboard.index())));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let keys = |label: &str| {
            tree.iter().find(|n| n.is("ListItem", Some(label))).and_then(|n| n.value.clone())
        };
        assert_eq!(keys("New terminal").as_deref(), Some("⌘T, ⌘N"), "{tree:#?}");
        assert_eq!(keys("Focus column 3").as_deref(), Some("⌘3"));
        assert_eq!(keys("Open URL…").as_deref(), Some(""), "a command with no chord, listed");
        assert_eq!(keys("Copy last output").as_deref(), Some("⇧⌘C"));
        assert!(tree.iter().any(|n| n.is("Heading", Some("Layout"))), "{tree:#?}");
        assert!(!tree.iter().any(|n| n.is("RadioGroup", Some("Theme"))), "no rows here");

        click(cx, leak(format!("settings-section-{}", Section::About.index())));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let about = tree.iter().find(|n| n.is("Group", Some("About"))).expect("the about block");
        let said = about.description.as_deref().unwrap_or_default();
        assert!(said.contains(env!("CARGO_PKG_VERSION")) && said.contains("build"), "{said}");
        for link in schema::LINKS.map(|(words, _)| words) {
            assert!(tree.iter().any(|n| n.is("Link", Some(link))), "{link}: {tree:#?}");
        }

        view.update_in(cx, |view, window, cx| view.focus(window, cx));
        cx.simulate_input("last output");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("ListItem", Some("Copy last output"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Heading", Some("Keyboard"))), "{tree:#?}");

        view.update_in(cx, SettingsEditor::show_toml);
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-path").is_some(), "the file's face names the file");
    }

    /// A press on a command's chords records the next chord typed into `[keys]`, before any
    /// binding runs it, and applies it at once: the line shows it, and a chord taken from
    /// another command is said over the page. Esc keeps what it had, ⌫ leaves it none, and the
    /// way back to the default takes the line out of the file.
    #[gpui::test]
    fn a_chord_is_recorded_into_the_file(cx: &mut TestAppContext) {
        use crate::keymap::Scope;
        let ran = Rc::new(RefCell::new(false));
        let counted = Rc::clone(&ran);
        cx.update(|cx| {
            cx.bind_keys([gpui::KeyBinding::new("cmd-t", crate::workspace::NewNote, None)]);
            cx.on_action(move |_: &crate::workspace::NewNote, _cx| *counted.borrow_mut() = true);
        });
        let (_view, events, cx) = editor(cx, "", true, Mode::Form);
        click(cx, leak(format!("settings-section-{}", Section::Keyboard.index())));
        let keymap = crate::keymap::current();
        let note = keymap.find(Scope::Workspace, "new_note").expect("the command");
        let chords = leak(format!("settings-chord-{note}"));
        let applied = |events: &Rc<RefCell<Vec<SettingsEditorEvent>>>| match events.borrow().last()
        {
            Some(SettingsEditorEvent::Apply(text)) => text.clone(),
            other => panic!("{other:?}"),
        };

        click(cx, chords);
        assert_eq!(value_of(cx, "Button", "Keys for New note").as_deref(), Some(PRESS_KEYS));
        cx.simulate_keystrokes("cmd-t");
        assert!(!*ran.borrow(), "recorded, not run");
        assert_eq!(applied(&events), "[keys.workspace]\nnew_note = \"cmd-t\"\n");
        assert_eq!(value_of(cx, "ListItem", "New note").as_deref(), Some("⌘T"));
        assert_eq!(value_of(cx, "ListItem", "New terminal").as_deref(), Some("⌘N"), "⌘T went");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let said = tree.iter().find(|n| n.role == "Alert").and_then(|n| n.label.clone());
        assert_eq!(
            said.as_deref(),
            Some("⌘T runs `workspace.new_note` now, no longer `workspace.new_terminal`"),
            "{tree:#?}"
        );

        let count = events.borrow().len();
        click(cx, chords);
        cx.simulate_keystrokes("shift");
        cx.simulate_keystrokes("escape");
        assert_eq!(events.borrow().len(), count, "a modifier alone, then Esc: nothing written");
        assert_eq!(value_of(cx, "ListItem", "New note").as_deref(), Some("⌘T"));

        click(cx, chords);
        cx.simulate_keystrokes("backspace");
        assert_eq!(applied(&events), "[keys.workspace]\nnew_note = \"\"\n");
        assert_eq!(value_of(cx, "Button", "Keys for New note").as_deref(), Some(""));

        click(cx, leak(format!("settings-key-reset-{note}")));
        assert_eq!(applied(&events), "[keys.workspace]\n", "the line is gone");
        assert_eq!(value_of(cx, "ListItem", "New note").as_deref(), Some("⇧⌘N"));
        assert!(cx.debug_bounds(leak(format!("settings-key-reset-{note}"))).is_none());
    }

    /// Recording takes a chord a terminal or a field would not type. A key alone (a letter)
    /// says it wants ⌘ or ⌃ and keeps recording, as does a key with no name in a chord; Tab
    /// stops it and goes along the ring; a keystroke in another window is that window's. An F
    /// key alone is a chord.
    #[gpui::test]
    fn recording_takes_only_a_chord_the_app_can_own(cx: &mut TestAppContext) {
        use crate::keymap::Scope;
        use crate::settings_form::{CANT_BIND, NEEDS_MODIFIER};
        let (_view, events, cx) = editor(cx, "", true, Mode::Form);
        click(cx, leak(format!("settings-section-{}", Section::Keyboard.index())));
        let keymap = crate::keymap::current();
        let note = keymap.find(Scope::Workspace, "new_note").expect("the command");
        let chords = leak(format!("settings-chord-{note}"));
        let label = "Keys for New note";

        click(cx, chords);
        for (keys, said) in
            [("a", NEEDS_MODIFIER), ("shift-down", NEEDS_MODIFIER), ("cmd-§", CANT_BIND)]
        {
            cx.simulate_keystrokes(keys);
            assert_eq!(value_of(cx, "Button", label).as_deref(), Some(said), "{keys}");
        }
        let other = cx.update(|_window, cx| {
            cx.open_window(gpui::WindowOptions::default(), |_window, cx| cx.new(|_cx| gpui::Empty))
        });
        let other: gpui::AnyWindowHandle = other.expect("a second window").into();
        VisualTestContext::from_window(other, cx).simulate_keystrokes("cmd-y");
        cx.run_until_parked();
        assert!(events.borrow().is_empty(), "nothing written: {:?}", events.borrow());
        assert_eq!(value_of(cx, "Button", label).as_deref(), Some(CANT_BIND), "still recording");

        cx.simulate_keystrokes("tab");
        assert!(events.borrow().is_empty(), "Tab is no chord: {:?}", events.borrow());
        assert_eq!(value_of(cx, "Button", label).as_deref(), Some("⇧⌘N"), "recording stopped");
        assert_ne!(focused(cx).1.as_deref(), Some(label), "the keyboard went along the ring");

        click(cx, chords);
        cx.simulate_keystrokes("f5");
        assert!(
            matches!(events.borrow().last(), Some(SettingsEditorEvent::Apply(t)) if t.contains("new_note = \"f5\"")),
            "an F key alone is a chord: {:?}",
            events.borrow()
        );
    }

    /// A folder tile's rows are walked with bare keys, so its commands take a key alone.
    #[gpui::test]
    fn a_folders_command_takes_a_key_alone(cx: &mut TestAppContext) {
        use crate::keymap::Scope;
        let (_view, events, cx) = editor(cx, "", true, Mode::Form);
        cx.simulate_input("folder.open");
        cx.run_until_parked();
        let open = crate::keymap::current().find(Scope::Folder, "open").expect("the command");
        click(cx, leak(format!("settings-chord-{open}")));
        cx.simulate_keystrokes("right");
        assert_eq!(
            events.borrow().last(),
            Some(&SettingsEditorEvent::Apply("[keys.folder]\nopen = \"right\"\n".to_owned())),
        );
    }

    /// Where the ligatures switch's knob sits from its track's left, on the first frame after
    /// turning it off.
    fn knob_after_turning(cx: &mut TestAppContext) -> f32 {
        let (view, _events, cx) = editor(cx, "", false, Mode::Form);
        click(cx, "settings-section-1");
        let ix = row("font", "ligatures");
        click(cx, leak(format!("settings-switch-{ix}")));
        assert!(view.read_with(cx, SettingsEditor::text).contains("ligatures = false"));
        let track = cx.debug_bounds(leak(format!("settings-switch-{ix}"))).expect("the track");
        let knob = cx.debug_bounds(leak(format!("settings-knob-{ix}"))).expect("the knob");
        f32::from(knob.left() - track.left())
    }

    /// A turned switch's knob slides from where it was; under Reduce Motion it is at its new
    /// end on the first frame.
    #[gpui::test]
    fn a_switch_slides_unless_motion_is_reduced(cx: &mut TestAppContext) {
        let spacing = Theme::default().spacing;
        let off = spacing.xxs;
        assert!(knob_after_turning(cx) > off + 1.0, "it leaves from the on end");
        cx.update(|cx| cx.set_reduce_motion(true));
        assert!((knob_after_turning(cx) - off).abs() < 0.5, "at the off end at once");
    }

    /// The dialog reads as one to a screen reader, the field holds the file, ⌘↩ hands the
    /// edited text back, and a refused save shows its reason until the next keystroke.
    #[gpui::test]
    fn the_editor_saves_on_command_enter_and_shows_a_refusal(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "[font]\nmono_size = 13\n", true, Mode::Toml);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Dialog", Some("Settings"))), "{tree:#?}");
        for label in ["Edit with controls", "Open in editor", "Cancel", "Save"] {
            assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
        }
        cx.simulate_keystrokes("enter [ t e r m i n a l ]");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [SettingsEditorEvent::Save("[font]\nmono_size = 13\n\n[terminal]".to_owned())],
            "⌘↩ saves without breaking the line"
        );
        view.update(cx, |v, cx| v.set_error("bad key".to_owned(), cx));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Alert", Some("bad key"))), "{tree:#?}");
        cx.simulate_keystrokes("x");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.error().map(str::to_owned)), None);
        assert!(view.read_with(cx, |v, cx| v.text(cx).ends_with("[terminal]x")));
    }

    /// A screen reader hears the focused field, not the window: gpui-kit keeps the focus handle
    /// on a role-less element inside the labelled field, and the fork's gpui reports that
    /// element's nearest labelled ancestor as focused.
    #[gpui::test]
    fn the_focused_field_is_what_a_screen_reader_hears(cx: &mut TestAppContext) {
        let (_view, _events, cx) = editor(cx, "", true, Mode::Toml);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let focused: Vec<_> = tree.iter().filter(|n| n.focused).collect();
        assert!(
            matches!(focused.as_slice(), [n] if n.role.ends_with("TextInput")
                && n.label.as_deref() == Some("settings.toml")),
            "{focused:#?}"
        );
    }

    /// The field is the code editor with TOML's colours (`highlight`'s grammar test holds what
    /// they paint), and keeps them across a theme change.
    #[gpui::test]
    fn the_field_colours_the_file_as_toml(cx: &mut TestAppContext) {
        let (view, _events, cx) = editor(cx, "[theme]\nappearance = \"light\"\n", true, Mode::Toml);
        let language = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, cx| v.text.read(cx).language_name().to_string())
        };
        assert_eq!(language(cx), "toml");
        let light = Theme::new(slopty_theme::Variant::Light);
        view.update(cx, |v, cx| v.set_theme(light, cx));
        cx.run_until_parked();
        assert_eq!(language(cx), "toml");
    }

    /// Escape and the Cancel button discard, from the form's search and from a control alike;
    /// the phone's dialog has no external editor.
    #[gpui::test]
    fn escape_and_cancel_dismiss_and_the_phone_has_no_external_editor(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "", false, Mode::Toml);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(!tree.iter().any(|n| n.is("Button", Some("Open in editor"))), "{tree:#?}");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        click(cx, "settings-cancel");
        cx.update(|window, cx| view.update(cx, |v, cx| v.show_form(window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        press(cx, "down");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec![SettingsEditorEvent::Dismiss; 4]);
    }

    /// The dialog opens at the one modal anchor, where the palette does, and the file's face
    /// is as tall as the file: a short one leaves room for a few lines to type, each line
    /// typed grows it by one, and past [`MAX_ROWS`] it stops and the field scrolls.
    #[gpui::test]
    fn the_dialog_is_as_tall_as_the_file(cx: &mut TestAppContext) {
        // Measured where it lands, not on the frame it starts rising from.
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, _events, cx) = editor(cx, "[theme]\nappearance = \"light\"\n", true, Mode::Toml);
        let height = |cx: &mut VisualTestContext| {
            f32::from(cx.debug_bounds("settings-editor").expect("the dialog").size.height)
        };
        let rows = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.rows);
        let typography = Theme::default().typography;
        let line = typography.mono_size * typography.markdown_line_height;
        let short = height(cx);
        assert_eq!(rows(cx), MIN_ROWS, "two lines get the fewest");
        let viewport = cx.update(|window, _cx| f32::from(window.viewport_size().height));
        assert!(short < viewport * 0.5, "not a share of the window: {short} of {viewport}");
        let top = f32::from(cx.debug_bounds("settings-editor").expect("the dialog").top());
        let anchor = viewport * crate::kit::MODAL_ANCHOR;
        assert!((top - anchor).abs() < 0.5, "where the palette opens: {top} for {anchor}");

        for _ in 0..MIN_ROWS {
            cx.simulate_keystrokes("enter");
        }
        cx.run_until_parked();
        let grown = rows(cx);
        assert!(grown > MIN_ROWS && grown < MAX_ROWS, "{grown}");
        let taller = height(cx);
        let expected = f32::from(grown.saturating_sub(MIN_ROWS)) * line;
        assert!((taller - short - expected).abs() < 0.5, "{short} → {taller}, {expected}");

        for _ in 0..MAX_ROWS {
            cx.simulate_keystrokes("enter");
        }
        cx.run_until_parked();
        assert_eq!(rows(cx), MAX_ROWS, "a long file stops at the most");
        assert!(height(cx) <= viewport, "and fits the window");
    }
}
