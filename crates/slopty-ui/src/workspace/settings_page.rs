//! The settings as a page of the workspace (audit row 9, `MonoCode`'s settings view).
//!
//! The app hands the workspace its settings editor ([`WorkspaceView::show_settings`]) and the
//! page takes the panes' place under the title bar; the foot bar goes with the panes, as
//! `MonoCode`'s usage footer does. While the navigator is docked its body becomes the form's
//! section list (its lights row stays), so the sections sit where the projects were; anywhere
//! else (hidden, over the panes, a phone's drawer) the navigator stays the navigator and the
//! page carries the list itself, or one column of every section on a narrow window.
//!
//! The page leaves by its own ways (Back, Esc, ⌘↩) and when the person turns to the work: an
//! action on the tiling (a title tab pressed, a pane focused by key) or the bell. Leaving asks
//! the editor to close, so what was waiting for a pause is written first and the app takes the
//! page back ([`WorkspaceView::show_settings`] with nothing).

use gpui::{Context, Entity, IntoElement as _, ParentElement as _, Styled as _, Window};

use super::WorkspaceView;
use crate::settings_editor::SettingsEditor;

impl WorkspaceView {
    /// Show `editor` as a page in the panes' place, or, with `None`, the panes again.
    pub fn show_settings(
        &mut self,
        editor: Option<Entity<SettingsEditor>>,
        cx: &mut Context<Self>,
    ) {
        if self.settings == editor {
            return;
        }
        self.settings = editor;
        cx.notify();
    }

    /// The settings page, while it is up.
    #[must_use]
    pub const fn settings(&self) -> Option<&Entity<SettingsEditor>> {
        self.settings.as_ref()
    }

    /// The person turned to the work: the settings page, if up, closes the way its Back does.
    pub(super) fn leave_settings(&self, cx: &mut Context<Self>) {
        if let Some(editor) = &self.settings {
            editor.update(cx, |editor, cx| editor.close(cx));
        }
    }

    /// Tell the page whether the navigator draws its section list: only a docked one does.
    /// Read before it is written, so a frame that changes nothing writes nothing.
    pub(super) fn sync_settings_aside(&self, cx: &mut Context<Self>) {
        let Some(editor) = &self.settings else { return };
        let aside = self.settings_listed();
        let form = editor.read(cx).form().clone();
        if form.read(cx).aside() != aside {
            form.update(cx, |form, cx| form.set_aside(aside, cx));
        }
    }

    /// Whether the navigator's body is the settings' section list: the page is up and the
    /// navigator docked.
    pub(super) fn settings_listed(&self) -> bool {
        self.settings.is_some() && self.nav.drawn == Some(super::navigator::Mode::Docked)
    }

    /// The form's section list as the navigator's body, built from the form as it is now:
    /// its rows answer the form, which the navigator's view reads, so a change of the form's
    /// draws the navigator again.
    pub(super) fn render_settings_sections(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<gpui::AnyElement> {
        if !self.settings_listed() {
            return None;
        }
        let form = self.settings.as_ref()?.read(cx).form().clone();
        let draw = crate::draw::Draw::new(cx, form.downgrade());
        Some(form.read(cx).sections(window, &draw).into_any_element())
    }

    /// The page in the panes' place, while it is up.
    pub(super) fn render_settings_page(&self) -> Option<gpui::AnyElement> {
        self.settings.clone().map(|editor| {
            gpui::div().flex_1().min_w_0().min_h_0().flex().child(editor).into_any_element()
        })
    }
}
