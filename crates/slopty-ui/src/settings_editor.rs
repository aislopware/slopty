//! The in-app settings: `settings.toml` as a form of sections and rows that applies each change
//! as it is made.
//!
//! The phone has no editor to hand the file to, and on the Mac a small change should not
//! need one either. The settings are a page, not a dialog: the workspace draws it where the
//! panes are, and its navigator's body becomes the form's section list while it is up
//! ([`crate::settings_form`]), as `MonoCode`'s settings fill its main area and turn its rail into
//! their sections. With no navigator beside it the page carries the list as its own sidebar.
//!
//! The form applies each change as it is made ([`SettingsEditorEvent::Apply`]): the app writes
//! the file and reloads it, and the page stays, as System Settings, Zed's settings and Linear's
//! do. Back, Escape or ⌘↩ leaves it, writing first what was still waiting for a pause.
//!
//! The file's own text is not a second face of the page. Where this Mac has a worker, "Open the
//! file" at the list's foot (in the page's head when the form is one column) leaves the page for
//! `settings.toml` in a file tile ([`SettingsEditorEvent::OpenFile`]), the way another machine's
//! settings open; the app follows the file as it is saved there.

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Enter, Escape};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::settings_form::{SettingsForm, SettingsFormEvent};

/// Key context of the page.
pub const CTX: &str = "SettingsEditor";

/// What the user asked of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsEditorEvent {
    /// The form changed the file: write this text and apply it, and keep the page up.
    Apply(String),
    /// Leave the page for the file itself, in a file tile, once the form's last change is
    /// applied.
    OpenFile,
    /// Closed: the form's changes were already applied.
    Dismiss,
}

/// The page and its form.
#[derive(Debug)]
pub struct SettingsEditor {
    form: Entity<SettingsForm>,
    /// Why the last apply was refused.
    error: Option<String>,
    /// The file's text as the page last knew it: given, applied or followed.
    file: String,
    /// The file's text as it changed outside the page, taken in at the next frame, which
    /// has the window the fields are set in.
    changed_file: Option<String>,
    theme: Theme,
    _subscription: Subscription,
}

impl SettingsEditor {
    /// The app's own palette lines, so the Keyboard page names its commands in the palette's
    /// words ([`SettingsForm::set_palette_words`]).
    pub fn set_palette_words(
        &self,
        words: Vec<crate::palette::PaletteItem>,
        cx: &mut Context<Self>,
    ) {
        self.form.update(cx, |form, cx| form.set_palette_words(words, cx));
    }

    /// A page over `text`, a `settings.toml`.
    pub fn new(text: &str, theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form = cx.new(|cx| SettingsForm::new(text, theme.clone(), window, cx));
        let set = cx.subscribe_in(&form, window, |this, _form, event, _window, cx| match event {
            SettingsFormEvent::Apply(text) => this.apply(text, cx),
            SettingsFormEvent::Done => this.close(cx),
            SettingsFormEvent::OpenFile => this.open_file(cx),
        });
        Self {
            form,
            error: None,
            file: text.to_owned(),
            changed_file: None,
            theme,
            _subscription: set,
        }
    }

    /// Whether the form offers the file itself: only where there is a file tile to open it in
    /// ([`SettingsForm::set_file_opens`]).
    pub fn set_file_opens(&self, opens: bool, cx: &mut Context<Self>) {
        self.form.update(cx, |form, cx| form.set_file_opens(opens, cx));
    }

    /// Whether the form offers the file itself ([`Self::set_file_opens`]).
    #[must_use]
    pub fn file_opens(&self, cx: &gpui::App) -> bool {
        self.form.read(cx).file_opens()
    }

    /// Whether the host draws the form's section list beside the page: the workspace's
    /// navigator while it is docked ([`SettingsForm::set_aside`]).
    pub fn set_aside(&self, aside: bool, cx: &mut Context<Self>) {
        self.form.update(cx, |form, cx| form.set_aside(aside, cx));
    }

    /// The form, whose section list the workspace's navigator draws.
    pub(crate) const fn form(&self) -> &Entity<SettingsForm> {
        &self.form
    }

    /// The file's text, as the form holds it.
    #[must_use]
    pub fn text(&self, cx: &gpui::App) -> String {
        self.form.read(cx).text().to_owned()
    }

    /// Give the keyboard to the form's search.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.form.update(cx, |form, cx| form.focus(window, cx));
    }

    /// Show the form on `section`'s page.
    pub fn show_section(
        &self,
        section: crate::settings_form::schema::Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form.update(cx, |form, cx| form.show(section, window, cx));
    }

    /// Show the form on the row for `key` in `table`, with the keyboard on it
    /// ([`SettingsForm::show_setting`]).
    pub fn show_setting(
        &self,
        table: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form.update(cx, |form, cx| form.show_setting(table, key, window, cx));
    }

    /// The app refused the text: show why, keep the page.
    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    /// The error at the page's foot, if any.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// New tokens.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.form.update(cx, |form, cx| form.set_theme(theme.clone(), cx));
            self.theme = theme;
            cx.notify();
        }
    }

    /// The form's `text`: the app writes and applies it. A new change is the fix in progress
    /// for a refused one.
    fn apply(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
        text.clone_into(&mut self.file);
        cx.emit(SettingsEditorEvent::Apply(text.to_owned()));
    }

    /// The file changed outside the page (a file tile, another editor, the appearance written
    /// to it): the form follows it, so it neither shows a value the file no longer holds nor
    /// writes it back over the file. The form's change on its way is kept.
    pub fn follow_file(&mut self, text: String, cx: &mut Context<Self>) {
        if text != self.file {
            self.changed_file = Some(text);
            cx.notify();
        }
    }

    /// Take in the file's text that changed outside, now that there is a window.
    fn take_changed_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.changed_file.take() else { return };
        if self.form.update(cx, |form, cx| form.follow_file(&text, window, cx)) {
            self.file = text;
        }
    }

    /// The form's last change applied, then the file in a file tile.
    fn open_file(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.form.update(cx, SettingsForm::flush) {
            self.apply(&text, cx);
        }
        cx.emit(SettingsEditorEvent::OpenFile);
    }

    /// Close, after applying what was still waiting for a pause.
    pub fn close(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.form.update(cx, SettingsForm::flush) {
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
        if !this.form.read(cx).typing(window, cx) {
            this.close(cx);
            cx.stop_propagation();
        }
    }

    /// Whether an input method holds uncommitted text in a field of the form.
    fn composing(&self, cx: &gpui::App) -> bool {
        self.form.read(cx).composing(cx)
    }
}

impl EventEmitter<SettingsEditorEvent> for SettingsEditor {}

impl Focusable for SettingsEditor {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.form.read(cx).focus_handle(cx)
    }
}

impl Render for SettingsEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.take_changed_file(window, cx);
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let error = self.error.clone().map(|error| {
            crate::kit::inset_x(div(), theme)
                .id("settings-error")
                .debug_selector(|| "settings-error".to_owned())
                .role(gpui::accesskit::Role::Alert)
                .aria_label(error.clone())
                .py(px(spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.error))
                .child(SharedString::from(error))
        });
        div()
            .id("settings-editor")
            .debug_selector(|| "settings-editor".to_owned())
            .role(gpui::accesskit::Role::Group)
            .aria_label("Settings")
            .key_context(CTX)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(hsla(theme.content()))
            .font_family(theme.typography.ui_family.clone())
            .on_key_down(cx.listener(Self::key_down))
            // While an input method composes in a field, Esc and Enter are its own.
            .capture_action(cx.listener(|this, _: &Escape, _window, cx| {
                if !this.composing(cx) {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            // ⌘↩ (a field's `secondary-enter`: ⌘ on macOS and iOS alike) leaves the form;
            // captured so a field does not also take it. A control that is not a field asks
            // through the form.
            .capture_action(cx.listener(|this, enter: &Enter, _window, cx| {
                if enter.secondary && !this.composing(cx) {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            // The form fills the page, which scrolls inside.
            .child(div().flex_1().min_h_0().flex().child(self.form.clone()))
            .children(error)
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

    /// A window over `text`, the events it raised, and its context. gpui-kit is
    /// set up by the first one a test opens.
    fn editor<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (Entity<SettingsEditor>, Rc<RefCell<Vec<SettingsEditorEvent>>>, &'a mut VisualTestContext)
    {
        if !cx.update(|cx| cx.has_global::<gpui_kit::component::Theme>()) {
            cx.update(gpui_kit::init);
        }
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&events);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let view = SettingsEditor::new(text, Theme::default(), window, cx);
            view.focus(window, cx);
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

    /// The page opens on the form: the sections listed under their groups, the first
    /// section's rows, the search with the keyboard, and the file and the way back at the
    /// list's foot. The form has nothing to save.
    #[gpui::test]
    fn the_page_opens_on_the_form(cx: &mut TestAppContext) {
        let (_view, _events, cx) = editor(cx, "");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Group", Some("Settings"))), "{tree:#?}");
        for section in Section::ALL.map(Section::label) {
            assert!(tree.iter().any(|n| n.is("Tab", Some(section))), "{section}: {tree:#?}");
        }
        for group in schema::Group::ALL.map(schema::Group::label) {
            assert!(tree.iter().any(|n| n.is("Heading", Some(group))), "{group}: {tree:#?}");
        }
        assert!(tree.iter().any(|n| n.is("RadioGroup", Some("Theme"))), "{tree:#?}");
        assert!(!tree.iter().any(|n| n.is("ComboBox", Some("Family"))), "another section's row");
        assert!(!tree.iter().any(|n| n.is("Button", Some("Done"))), "the list's Back is the way");
        assert!(tree.iter().any(|n| n.is("Button", Some("Back"))), "{tree:#?}");
        let file = crate::settings_form::OPEN_FILE;
        assert!(!tree.iter().any(|n| n.is("Button", Some(file))), "no tile to open it in");
        let (role, label) = focused(cx);
        assert!(role.ends_with("TextInput"), "{role}");
        assert_eq!(label.as_deref(), Some(crate::settings_form::SEARCH_PLACEHOLDER));
        assert_eq!(value_of(cx, "RadioGroup", "Theme").as_deref(), Some("System"));
        assert_eq!(value_of(cx, "SpinButton", "Text size").as_deref(), Some("13 pt"));
        // The section shown wears the selected wash, as the navigator's chosen row does
        // where the keyboard is not: here it is in the search.
        let theme = Theme::default();
        // Past the wash's ease in.
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let shown = cx.debug_bounds("settings-section-0").expect("the first section");
        let fills: Vec<gpui::Background> = cx.update(|window, _| {
            let scale = window.scale_factor();
            let centre = shown.center().scale(scale);
            window
                .painted_quads()
                .into_iter()
                .filter(|q| {
                    q.bounds.contains(&centre) && q.bounds.size.height.0 < 3.0 * 28.0 * scale
                })
                .map(|q| q.background)
                .collect()
        });
        let plate = hsla(theme.surfaces.selected);
        assert!(fills.contains(&gpui::Background::from(plate)), "{plate:?} in {fills:?}");
    }

    /// Where the window has room the list is a sidebar beside a page of at least 480 pt, and
    /// the file, once there is a tile to open it in, is its advanced path at its foot, over the
    /// way back, apart from the sections; it asks for the file's tile after applying the change
    /// still waiting for a pause, and Back leaves. Stacked in one column, with no list, the head
    /// offers the file beside Done instead.
    #[gpui::test]
    fn the_file_is_the_sidebars_advanced_path(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "");
        cx.simulate_resize(gpui::size(px(1280.0), px(800.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-open-file").is_none(), "no tile to open it in");
        view.update(cx, |v, cx| v.set_file_opens(true, cx));
        cx.run_until_parked();
        let page = cx.debug_bounds("settings-page").expect("the page");
        assert!(f32::from(page.size.width) >= 480.0, "{page:?}");
        let file = cx.debug_bounds("settings-open-file").expect("the file's way");
        let back = cx.debug_bounds("settings-back").expect("the way back");
        let last = cx.debug_bounds(leak(format!("settings-section-{}", Section::About.index())));
        let last = last.expect("the last section");
        assert!(file.right() <= page.left(), "in the sidebar: {file:?} {page:?}");
        assert!(file.top() > last.bottom() && back.top() >= file.bottom(), "at its foot");
        click(cx, "settings-section-1");
        click(cx, leak(format!("settings-increase-{}", row("font", "mono_size"))));
        click(cx, "settings-open-file");
        assert!(
            matches!(
                events.borrow().as_slice(),
                [SettingsEditorEvent::Apply(text), SettingsEditorEvent::OpenFile]
                    if text.contains("mono_size = 14")
            ),
            "the waiting change first: {:?}",
            events.borrow()
        );
        click(cx, "settings-back");
        assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss), "Back leaves");
        let narrow = crate::settings_form::sidebar_from(&Theme::default()) - 1.0;
        cx.simulate_resize(gpui::size(px(narrow), px(800.0)));
        cx.run_until_parked();
        let file = cx.debug_bounds("settings-open-file").expect("the head's link");
        let done = cx.debug_bounds("settings-done").expect("Done");
        assert!((f32::from(file.center().y - done.center().y)).abs() < 1.0, "on Done's line");
        click(cx, "settings-open-file");
        assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::OpenFile), "the head's");
        view.update(cx, |v, cx| v.set_file_opens(false, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-open-file").is_none(), "gone with the tile's way");
        assert!(cx.debug_bounds("settings-done").is_some(), "Done stays");
    }

    /// A change applies as it is made, and the dialog stays open: a switch hands the app the
    /// file's new text at once, every other line as it was; a run of stepper clicks hands it
    /// over once, when the clicks pause. The file written from it opens on the same values.
    #[gpui::test]
    fn a_change_applies_as_it_is_made_and_the_dialog_stays_open(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("settings.toml");
        let file = "# mine\n[font]\nmono_size = 13.0 # the size I like\n";
        std::fs::write(&path, file).expect("the file");

        {
            let (view, events, cx) = editor(cx, file);
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
        let (view, _events, cx) = editor(cx, &reread);
        click(cx, "settings-section-1");
        assert_eq!(value_of(cx, "SpinButton", "Size").as_deref(), Some("15 pt"));
        let form = view.read_with(cx, |v, _| v.form.clone());
        let off =
            form.read_with(cx, |f, _| slopty_settings::edit::read(f.text(), "font", "ligatures"));
        assert_eq!(off, Some(slopty_settings::edit::Value::Bool(false)));
    }

    /// A value its key does not take is never handed over: the row says why in place of its
    /// line, until it is typed again, and the fixed value applies once the typing pauses.
    #[gpui::test]
    fn an_invalid_value_is_not_written_and_its_row_says_why(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "[colors.light]\ncursor = \"\"\n");
        let ix = row("colors.light", "cursor");
        // Narrowed to it first: the Appearance section runs past the test window's foot.
        cx.simulate_input("cursor");
        cx.run_until_parked();
        click(cx, leak(format!("settings-field-{ix}")));
        cx.simulate_input("#12");
        cx.executor().advance_clock(crate::settings_form::SETTLE);
        cx.run_until_parked();
        assert!(events.borrow().is_empty(), "nothing to apply: {:?}", events.borrow());
        let form = view.read_with(cx, |v, _| v.form.clone());
        assert_eq!(
            form.read_with(cx, |f, _| f.text().to_owned()),
            "[colors.light]\ncursor = \"\"\n"
        );
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
            [SettingsEditorEvent::Apply("[colors.light]\ncursor = \"#123456\"\n".to_owned())]
        );
    }

    /// The keyboard walks the form: ↓ from the search to the rows and ↑ back, a query narrows
    /// the rows across sections, Space turns a switch, ← and → move a choice and a stepper, ↓
    /// in the sidebar moves to the next section and → goes into it, and ⌘↩ on a control is
    /// Done, applying the stepper that was still waiting first.
    #[gpui::test]
    fn the_keyboard_walks_the_form(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "");
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
        let (_view, _events, cx) = editor(cx, "");
        cx.simulate_input("zzz");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let quiet = crate::settings_form::NO_MATCHES;
        assert!(tree.iter().any(|n| n.is("Status", Some(quiet))), "{tree:#?}");
        cx.simulate_keystrokes("cmd-a backspace");
        cx.simulate_input("live_agents");
        cx.run_until_parked();
        assert_eq!(value_of(cx, "SpinButton", "Live agents").as_deref(), Some("24"));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Heading", Some("Agents"))), "{tree:#?}");
    }

    /// The page's head stands on the search field's line, so the columns start together: the
    /// page's name on it, as a macOS 26 pane is titled, with no title bar over both.
    #[gpui::test]
    fn the_columns_start_together(cx: &mut TestAppContext) {
        let (_view, _events, cx) = editor(cx, "");
        let search = cx.debug_bounds("settings-search").expect("the search");
        let title = cx.debug_bounds("settings-title").expect("the page's name");
        assert!((search.center().y - title.center().y).abs() < px(0.5), "{search:?} {title:?}");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let general = Section::ALL[0].label();
        assert!(tree.iter().any(|n| n.is("Heading", Some(general))), "{tree:#?}");
    }

    /// A sheet as narrow as a phone's heads the form with what it needs at its end, all inside
    /// the page: Done, and the way to the file where there is a tile to open it in. Where the
    /// words of the way do not fit, its glyph stands for them.
    #[gpui::test]
    fn a_narrow_sheet_keeps_its_head_whole(cx: &mut TestAppContext) {
        let (view, _events, cx) = editor(cx, "");
        view.update(cx, |v, cx| v.set_file_opens(true, cx));
        for width in [320.0, 260.0] {
            cx.simulate_resize(gpui::size(px(width), px(700.0)));
            cx.run_until_parked();
            let dialog = cx.debug_bounds("settings-editor").expect("the dialog");
            let inside = |cx: &mut VisualTestContext, s: &'static str| {
                cx.debug_bounds(s)
                    .is_some_and(|b| b.left() >= dialog.left() && b.right() <= dialog.right())
            };
            assert!(inside(cx, "settings-done"), "{width}: Done");
            let file = inside(cx, "settings-open-file") || inside(cx, "settings-open-file-glyph");
            assert!(file, "{width}: the way to the file, in words or its glyph");
        }
        assert!(cx.debug_bounds("settings-open-file-glyph").is_some(), "260 pt: the glyph alone");
    }

    /// The widths a sheet is written for: a phone's, or an iPad's in Slide Over, and the
    /// narrowest that still has the sidebar ([`crate::settings_form::sidebar_from`]).
    fn narrowest() -> [f32; 2] {
        [320.0, crate::settings_form::sidebar_from(&Theme::default())]
    }

    /// Each row shown at `width`, by section: its row and the height it stands. A sheet as
    /// wide as a phone's lists every section in one column.
    fn row_heights(cx: &mut VisualTestContext, width: f32) -> Vec<(usize, f32)> {
        cx.simulate_resize(gpui::size(px(width), px(900.0)));
        cx.run_until_parked();
        let mut seen: Vec<(usize, f32)> = Vec::new();
        for section in Section::ALL {
            if let Some(tab) =
                cx.debug_bounds(leak(format!("settings-section-{}", section.index())))
            {
                cx.simulate_click(tab.center(), gpui::Modifiers::default());
                cx.run_until_parked();
            }
            for (ix, _) in rows().iter().enumerate().filter(|(_, r)| r.section == section) {
                if seen.iter().any(|&(at, _)| at == ix) {
                    continue;
                }
                if let Some(row) = cx.debug_bounds(leak(format!("settings-row-{ix}"))) {
                    seen.push((ix, f32::from(row.size.height)));
                }
            }
        }
        seen
    }

    /// Every row is one line at the narrowest sheets, as System Settings draws its rows: its
    /// title and its control, with no description under them. What the file says of it is
    /// still its control's description to a screen reader, and a search still finds it.
    #[test]
    fn every_row_is_one_line_at_the_narrowest_sheet() {
        let mut cx = real_text();
        let (_view, _events, cx) = editor(&mut cx, "");
        let theme = Theme::default();
        let one = theme.density.row + theme.spacing.sm;
        for width in narrowest() {
            let all = row_heights(cx, width);
            assert!(all.len() > 20, "{width}: {} rows", all.len());
            let tall: Vec<String> = all
                .iter()
                .filter(|&&(_, h)| (h - one).abs() > 0.5)
                .map(|&(ix, h)| format!("{} ({h} pt)", rows()[ix].label()))
                .collect();
            assert!(tall.is_empty(), "at {width} pt, rows not one line:\n{}", tall.join("\n"));
        }
        cx.simulate_resize(gpui::size(px(900.0), px(900.0)));
        cx.run_until_parked();
        click(cx, leak(format!("settings-section-{}", Section::Appearance.index())));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let row = rows().first().expect("a row");
        assert!(
            tree.iter().any(|n| n.label.as_deref() == Some(row.label())
                && n.description.as_deref() == Some(row.meta())),
            "{}: its words, to a screen reader",
            row.label()
        );
    }

    /// A test app shaping with the platform's own text system, so a line wraps where it would.
    fn real_text() -> TestAppContext {
        TestAppContext::build_with_text_system(
            gpui::TestDispatcher::new(0),
            None,
            gpui_platform::text_system(),
        )
    }

    /// The pages beside the sections. Keyboard lists every command of the keymap, in the
    /// palette's words on key caps, and a query finds one among the rows; About names the
    /// version and the build and links out. The form's title is "Settings" alone.
    #[gpui::test]
    fn keyboard_and_about_read_what_is_bound_and_built(cx: &mut TestAppContext) {
        let (view, _events, cx) = editor(cx, "");
        assert!(cx.debug_bounds("settings-path").is_none(), "the form's title is Settings alone");

        click(cx, leak(format!("settings-section-{}", Section::Keyboard.index())));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let keys = |label: &str| {
            tree.iter().find(|n| n.is("ListItem", Some(label))).and_then(|n| n.value.clone())
        };
        assert_eq!(keys("New terminal").as_deref(), Some("⇧⌘T"), "{tree:#?}");
        assert_eq!(keys("Select tab 3").as_deref(), Some("⌘3"));
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
        let (_view, events, cx) = editor(cx, "");
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
        assert_eq!(value_of(cx, "ListItem", "New agent").as_deref(), Some(""), "⌘T went");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let said = tree.iter().find(|n| n.role == "Alert").and_then(|n| n.label.clone());
        assert_eq!(
            said.as_deref(),
            Some("⌘T runs `workspace.new_note` now, no longer `workspace.start_agent`"),
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
        // One line: what it does and what it was, and where its caps would be the offer.
        let (add, was) =
            (leak(format!("settings-add-keys-{note}")), leak(format!("settings-key-was-{note}")));
        let row = cx.debug_bounds(leak(format!("settings-key-{note}"))).expect("the row");
        let one = Theme::default().density.row;
        assert!((f32::from(row.size.height) - one).abs() < 0.5, "one line: {row:?}");
        assert!(cx.debug_bounds(add).is_some(), "no keys: the offer of some");
        assert!(cx.debug_bounds(was).is_some(), "set by the file: what it was");

        click(cx, leak(format!("settings-key-reset-{note}")));
        assert_eq!(applied(&events), "[keys.workspace]\n", "the line is gone");
        assert_eq!(value_of(cx, "ListItem", "New note").as_deref(), Some("⇧⌘N"));
        assert!(cx.debug_bounds(leak(format!("settings-key-reset-{note}"))).is_none());
        assert!(cx.debug_bounds(add).is_none() && cx.debug_bounds(was).is_none(), "its caps");
    }

    /// Recording takes a chord a terminal or a field would not type. A key alone (a letter)
    /// says it wants ⌘ or ⌃ and keeps recording, as does a key with no name in a chord; Tab
    /// stops it and goes along the ring; a keystroke in another window is that window's. An F
    /// key alone is a chord.
    #[gpui::test]
    fn recording_takes_only_a_chord_the_app_can_own(cx: &mut TestAppContext) {
        use crate::keymap::Scope;
        use crate::settings_form::{CANT_BIND, NEEDS_MODIFIER};
        let (_view, events, cx) = editor(cx, "");
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
        let (_view, events, cx) = editor(cx, "");
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
        let (view, _events, cx) = editor(cx, "");
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

    /// A refused change shows its reason at the page's foot, to a screen reader too, until the
    /// next change goes; ⌘↩ from the search leaves.
    #[gpui::test]
    fn a_refusal_shows_until_the_next_change(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "");
        view.update(cx, |v, cx| v.set_error("could not write it".to_owned(), cx));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Alert", Some("could not write it"))), "{tree:#?}");
        click(cx, "settings-section-1");
        click(cx, leak(format!("settings-switch-{}", row("font", "ligatures"))));
        assert_eq!(view.read_with(cx, |v, _| v.error().map(str::to_owned)), None, "a new try");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert_eq!(events.borrow().last(), Some(&SettingsEditorEvent::Dismiss), "⌘↩ leaves");
    }

    /// Escape leaves, from the form's search and from a control alike.
    #[gpui::test]
    fn escape_dismisses(cx: &mut TestAppContext) {
        let (_view, events, cx) = editor(cx, "");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        press(cx, "down");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec![SettingsEditorEvent::Dismiss; 2]);
    }

    /// The page fills what it is given, on the content's plane, with no backdrop.
    #[gpui::test]
    fn the_page_fills_what_it_is_given(cx: &mut TestAppContext) {
        let (_view, _events, cx) = editor(cx, "[theme]\nappearance = \"light\"\n");
        let viewport = cx.update(|window, _cx| window.viewport_size());
        let page = cx.debug_bounds("settings-editor").expect("the page");
        assert_eq!(page.size, viewport, "the whole window it is given");
        assert!(cx.debug_bounds("settings-backdrop").is_none(), "no dim under it");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(!tree.iter().any(|n| n.role == "Dialog"), "not a dialog: {tree:#?}");
    }

    /// The file written outside while the page is up (the appearance switched to dark, the
    /// file saved in its tile): the form shows what it holds now, and closing writes nothing
    /// back over it.
    #[gpui::test]
    fn the_page_follows_the_file_and_writes_nothing_back(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, LIGHT_FILE);
        view.update(cx, |v, cx| v.follow_file(DARK_FILE.to_owned(), cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, SettingsEditor::text), DARK_FILE, "the form follows");
        assert_eq!(value_of(cx, "RadioGroup", "Theme").as_deref(), Some("Dark"));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let applied: Vec<_> = events
            .borrow()
            .iter()
            .filter(|e| matches!(e, SettingsEditorEvent::Apply(_)))
            .cloned()
            .collect();
        assert!(applied.is_empty(), "nothing written back: {applied:?}");
    }

    /// A change on its way when the file changes outside is kept, and lands next.
    #[gpui::test]
    fn a_change_on_its_way_outlives_the_file_changing(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, LIGHT_FILE);
        click(cx, "settings-section-1");
        click(cx, leak(format!("settings-increase-{}", row("font", "mono_size"))));
        view.update(cx, |v, cx| v.follow_file(DARK_FILE.to_owned(), cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, SettingsEditor::text).contains("mono_size = 14"), "kept");
        cx.executor().advance_clock(crate::settings_form::SETTLE);
        cx.run_until_parked();
        assert!(
            matches!(events.borrow().last(), Some(SettingsEditorEvent::Apply(t)) if t.contains("mono_size = 14")),
            "{:?}",
            events.borrow()
        );
    }

    const LIGHT_FILE: &str = "[theme]\nappearance = \"light\"\n";
    const DARK_FILE: &str = "[theme]\nappearance = \"dark\"\n";
}
