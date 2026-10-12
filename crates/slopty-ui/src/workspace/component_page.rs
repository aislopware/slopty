//! The component preview as a page of the workspace ([`crate::components`]): from the palette,
//! in the panes' place under the window's own top line, as the settings are. It leaves by its
//! Done and the way the settings do, when the person turns to the work.

use gpui::{AppContext as _, Context, Entity, Window};

use super::WorkspaceView;
use super::actions::OpenComponentPreview;
use crate::components::{ComponentPreview, ComponentPreviewEvent};

impl WorkspaceView {
    /// Show the component preview in the panes' place.
    pub(super) fn open_component_preview(
        &mut self,
        _: &OpenComponentPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.components.is_some() {
            return;
        }
        if let Some(editor) = &self.settings {
            editor.update(cx, |editor, cx| editor.close(cx));
        }
        let theme = self.theme.clone();
        let page: Entity<ComponentPreview> = cx.new(|cx| ComponentPreview::new(theme, window, cx));
        cx.subscribe(&page, |this, _page, event: &ComponentPreviewEvent, cx| match event {
            ComponentPreviewEvent::Close => this.leave_component_preview(cx),
        })
        .detach();
        self.components = Some(page);
        cx.notify();
    }

    /// The panes again.
    pub(super) fn leave_component_preview(&mut self, cx: &mut Context<Self>) {
        if self.components.take().is_some() {
            cx.notify();
        }
    }

    /// Whether a page stands in the panes' place: the settings or the component preview.
    pub(super) const fn paged(&self) -> bool {
        self.settings.is_some() || self.components.is_some()
    }

    /// The preview in the panes' place, while it is up.
    pub(super) fn render_component_page(&self) -> Option<gpui::AnyElement> {
        use gpui::{IntoElement as _, ParentElement as _, Styled as _};
        self.components.clone().map(|page| {
            gpui::div().flex_1().min_w_0().min_h_0().flex().child(page).into_any_element()
        })
    }
}
