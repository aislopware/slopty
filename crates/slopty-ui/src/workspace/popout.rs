//! A remote window or display tile shown in a native window of its own on the Mac.
//!
//! The stream does not move: the tile's [`ScreenView`] stays the workspace's, keeps its stream,
//! its input, its pasteboard hook and its pointer, and is drawn in the new window instead of
//! in its tile, which says where it went. The window opens at the remote window's size (one
//! stream pixel a device pixel, fitted to the screen) and its aspect. Resizing it asks the
//! remote window to take the new size, as widening a tile does, and a remote window resized on
//! the worker resizes it back. Closing it, or the same command again, puts the picture back in
//! its tile (`docs/decisions/ui.md`, "A remote tile pops out into a window of its own").
//!
//! iPadOS would need a second `UIWindowScene`, which the gpui iOS fork does not make; the
//! command is the Mac's only.

use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Size, Styled as _,
    Subscription, Task, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowOptions, div, px, size,
};
use slopty_client::layout::WindowFrame;
use slopty_core::ItemId;
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::screen::{CaptureTarget, ScreenRequest};
use slopty_theme::Theme;

use super::WorkspaceView;
use super::actions::ToggleOwnWindow;
use crate::colors::hsla;
use crate::screen::ScreenView;

/// The key context of a tile's own window.
pub(crate) const CTX: &str = "PopOut";
/// The palette's name for [`ToggleOwnWindow`] on a tile in the workspace.
pub(crate) const OPEN_OWN_WINDOW: &str = "Open in its own window";
/// The palette's name for [`ToggleOwnWindow`] on a tile shown in a window of its own.
pub(crate) const BACK_TO_WORKSPACE: &str = "Back to the workspace";
/// What a tile says while its picture is in a window of its own.
pub(crate) const IN_OWN_WINDOW: &str = "In its own window";
/// What a click on such a tile does, said to a screen reader.
pub(crate) const SHOW_OWN_WINDOW: &str = "Show its window";
/// How long the window holds a size before the remote window is asked to take it: a drag of
/// its corner asks once, at the end.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);
/// The most of the screen a new window takes, on either side.
const SCREEN_SHARE: f32 = 0.9;

/// The tiles shown in windows of their own.
#[derive(Default)]
pub(super) struct PopOuts {
    windows: Vec<(ItemId, WindowHandle<PopOutView>)>,
    /// Where each window stands, as it last moved: saved with the layout, so a relaunch puts
    /// the tile back out there.
    frames: Vec<(ItemId, WindowFrame)>,
    /// The tile whose window has the keyboard.
    active: Option<ItemId>,
}

impl std::fmt::Debug for PopOuts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopOuts")
            .field("items", &self.windows.iter().map(|(item, _)| *item).collect::<Vec<_>>())
            .field("frames", &self.frames)
            .field("active", &self.active)
            .finish()
    }
}

impl PopOuts {
    /// Whether `item` is shown in a window of its own.
    pub(super) fn holds(&self, item: ItemId) -> bool {
        self.windows.iter().any(|(held, _)| *held == item)
    }

    /// The tile whose window has the keyboard, if one does.
    pub(super) const fn active(&self) -> Option<ItemId> {
        self.active
    }

    /// The window `item` is shown in.
    pub(super) fn window(&self, item: ItemId) -> Option<AnyWindowHandle> {
        self.windows.iter().find(|(held, _)| *held == item).map(|(_, handle)| (*handle).into())
    }

    /// Where each window stands, by its tile's item.
    pub(super) fn frames(&self) -> impl Iterator<Item = (ItemId, &WindowFrame)> {
        self.frames.iter().map(|(item, frame)| (*item, frame))
    }

    /// `item`'s window stands at `frame` now; whether that moved it.
    fn moved_to(&mut self, item: ItemId, frame: WindowFrame) -> bool {
        match self.frames.iter_mut().find(|(held, _)| *held == item) {
            Some((_, was)) if *was == frame => false,
            Some((_, was)) => {
                *was = frame;
                true
            }
            None => {
                self.frames.push((item, frame));
                true
            }
        }
    }

    /// `item` is no longer shown in a window of its own; its window, if it was.
    fn forget(&mut self, item: ItemId) -> Option<WindowHandle<PopOutView>> {
        if self.active == Some(item) {
            self.active = None;
        }
        self.frames.retain(|(held, _)| *held != item);
        let at = self.windows.iter().position(|(held, _)| *held == item)?;
        Some(self.windows.remove(at).1)
    }
}

/// Stream pixels a remote window's size may differ by before it counts as another size: the
/// rounding of a size through points and back.
const SAME_SIZE_PX: f32 = 4.0;

/// Whether `now` is another size than `was`, past rounding.
fn moved(now: (f32, f32), was: (f32, f32)) -> bool {
    (now.0 - was.0).abs() > SAME_SIZE_PX || (now.1 - was.1).abs() > SAME_SIZE_PX
}

/// The size a window showing a target `native` stream pixels big opens at, in points: a stream
/// pixel a device pixel at `scale`, scaled down whole to fit `SCREEN_SHARE` of `screen`.
fn opening_size(native: (f32, f32), scale: f32, screen: Size<f32>) -> Size<f32> {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (w, h) = (native.0.max(1.0) / scale, native.1.max(1.0) / scale);
    let fit =
        (screen.width * SCREEN_SHARE / w).min(screen.height * SCREEN_SHARE / h).clamp(0.0, 1.0);
    let fit = if fit > 0.0 { fit } else { 1.0 };
    Size { width: (w * fit).floor(), height: (h * fit).floor() }
}

impl WorkspaceView {
    /// "Open in its own window" on the focused remote tile, or "Back to the workspace" on one
    /// already out.
    pub fn toggle_own_window(
        &mut self,
        _: &ToggleOwnWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.focused() else { return };
        if self.popouts.holds(tile.item) {
            self.return_tile(tile.item, cx);
        } else {
            self.pop_out(tile.item, window, cx);
        }
    }

    /// Whether the palette offers [`ToggleOwnWindow`] for `item`: a streaming remote tile, on
    /// the Mac.
    pub(super) fn can_pop_out(&self, item: ItemId) -> bool {
        cfg!(not(target_os = "ios")) && self.screens.contains_key(&item)
    }

    /// Show `item`'s picture in a window of its own, sized to the remote window.
    pub(super) fn pop_out(&mut self, item: ItemId, window: &Window, cx: &mut Context<Self>) {
        if !self.can_pop_out(item) || self.popouts.holds(item) {
            return;
        }
        let Some(view) = self.screens.get(&item).cloned() else { return };
        let native = view.read(cx).native();
        let display = window.display(cx);
        let screen = display.as_ref().map_or(Size { width: 1280.0, height: 800.0 }, |d| {
            let bounds = d.visible_bounds();
            Size { width: f32::from(bounds.size.width), height: f32::from(bounds.size.height) }
        });
        let opened = opening_size(native, window.scale_factor(), screen);
        let display = display.map(|d| d.id());
        let bounds = Bounds::centered(display, size(px(opened.width), px(opened.height)), cx);
        self.pop_out_at(item, (WindowBounds::Windowed(bounds), display), cx);
    }

    /// Show `item`'s picture in a window of its own at `placed`: its bounds, on its display.
    pub(super) fn pop_out_at(
        &mut self,
        item: ItemId,
        (bounds, display_id): (WindowBounds, Option<gpui::DisplayId>),
        cx: &mut Context<Self>,
    ) {
        if !self.can_pop_out(item) || self.popouts.holds(item) {
            return;
        }
        let Some(view) = self.screens.get(&item).cloned() else { return };
        let native = view.read(cx).native();
        let opened = bounds.get_bounds().size;
        let title = self.item_by_id(item).map(|i| self.tile_title(&i)).unwrap_or_default();
        let options = WindowOptions {
            window_bounds: Some(bounds),
            display_id,
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            // A stream is watched while another window has the keyboard.
            inactive_frame_interval: None,
            ..WindowOptions::default()
        };
        let workspace = cx.entity();
        let theme = self.theme.clone();
        let points_per_px = f32::from(opened.width) / native.0.max(1.0);
        let opened = cx.open_window(options, |window, cx| {
            let workspace = workspace.clone();
            window.on_window_should_close(cx, {
                let workspace = workspace.downgrade();
                move |_window, cx| {
                    let _gone = workspace.update(cx, |ws, cx| ws.popped_closed(item, cx));
                    true
                }
            });
            window.focus(&view.read(cx).focus_handle(cx), cx);
            let opening = Opening { item, screen: view, theme, points_per_px };
            cx.new(|cx| PopOutView::new(&workspace, opening, window, cx))
        });
        match opened {
            Ok(handle) => {
                self.popouts.windows.push((item, handle));
                self.popouts.active = Some(item);
                let frame =
                    handle.update(cx, |_view, window, cx| crate::window_frame::of(window, cx));
                if let Ok(Some(frame)) = frame {
                    self.popouts.moved_to(item, frame);
                }
                self.layout_touched(cx);
                cx.notify();
            }
            Err(e) => tracing::warn!(error = %e, "a tile's own window"),
        }
    }

    /// Put `item`'s picture back in its tile and close its window.
    pub(super) fn return_tile(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let Some(handle) = self.popouts.forget(item) else { return };
        let _gone = handle.update(cx, |_view, window, _cx| window.remove_window());
        self.go_to(item, cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// `item`'s window was closed: its picture goes back to its tile.
    fn popped_closed(&mut self, item: ItemId, cx: &mut Context<Self>) {
        if self.popouts.forget(item).is_some() {
            self.layout_touched(cx);
            cx.notify();
        }
    }

    /// `item`'s window moved or took another size: where it stands is saved.
    fn popped_moved(&mut self, item: ItemId, frame: Option<WindowFrame>, cx: &Context<Self>) {
        if let Some(frame) = frame
            && self.popouts.holds(item)
            && self.popouts.moved_to(item, frame)
        {
            self.layout_touched(cx);
        }
    }

    /// `item`'s window took the keyboard, or gave it up. While it has it, its tile is the
    /// focused one: the worker's clipboard is watched for it and the palette acts on it.
    fn popped_active(&mut self, item: ItemId, active: bool, cx: &mut Context<Self>) {
        if active {
            self.popouts.active = Some(item);
            if let Some(tile) = self.tile_of(item)
                && self.focused() != Some(tile)
            {
                self.focus_tile(tile, cx);
            }
        } else if self.popouts.active == Some(item) {
            self.popouts.active = None;
        }
        // The system's shortcuts follow the keyboard into the window and out of it now: the
        // workspace's own window, inactive, may draw no frame to arm them from.
        self.rearm_system_keys(cx);
        cx.notify();
    }

    /// Bring `item`'s own window to the front.
    pub(super) fn raise_popped(&self, item: ItemId, cx: &mut App) {
        if let Some(handle) = self.popouts.window(item) {
            let _gone = handle.update(cx, |_view, window, _cx| window.activate_window());
        }
    }

    /// Ask the remote window `item` streams to take `pixels` (stream pixels at scale 1).
    fn resize_popped(&self, item: ItemId, pixels: (u32, u32), cx: &App) {
        let Some(view) = self.screens.get(&item).map(|v| v.read(cx)) else { return };
        if !matches!(view.target(), CaptureTarget::Window(_)) {
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let asked = (pixels.0 as f32, pixels.1 as f32);
        if !moved(asked, view.native()) {
            return;
        }
        if let Some(tile) = self.tile_of(item) {
            let (width, height) = pixels;
            let stream = view.stream();
            let resize = ScreenRequest::Resize { stream, width, height, scale: None };
            self.send(tile.worker, ClientMsg::Screen(resize));
        }
    }

    /// The item `id`, from whichever worker holds it.
    fn item_by_id(&self, id: ItemId) -> Option<slopty_proto::items::Item> {
        self.items().find(|(_, item)| item.id == id).map(|(_, item)| item.clone())
    }
}

/// What a tile's own window opens with. The theme comes from the workspace, which is mid-update
/// while the window opens and cannot be read then.
struct Opening {
    item: ItemId,
    screen: Entity<ScreenView>,
    theme: Theme,
    points_per_px: f32,
}

/// The root of a tile's own window: the tile's stream, full size.
pub(crate) struct PopOutView {
    workspace: WeakEntity<WorkspaceView>,
    item: ItemId,
    /// The stream drawn, as the workspace has it: after a reconnect, the new one.
    screen: Option<Entity<ScreenView>>,
    theme: Theme,
    /// Points of this window a stream pixel at scale 1 takes, fixed when it opened.
    points_per_px: f32,
    /// The target's size (stream pixels at scale 1) the window was last fitted to.
    fitted: (f32, f32),
    /// The remote window is asked to take the window's size once it has held it.
    settle: Option<Task<()>>,
    _watch: [Subscription; 3],
}

impl std::fmt::Debug for PopOutView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopOutView").field("item", &self.item).finish_non_exhaustive()
    }
}

impl PopOutView {
    fn new(
        workspace: &Entity<WorkspaceView>,
        opening: Opening,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Opening { item, screen, theme, points_per_px } = opening;
        let fitted = screen.read(cx).native();
        let follow = cx.observe_in(workspace, window, |this, ws, window, cx| {
            this.workspace_changed(&ws, window, cx);
        });
        let activation = cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            let (item, active) = (this.item, window.is_window_active());
            let _gone = this.workspace.update(cx, |ws, cx| ws.popped_active(item, active, cx));
        });
        let resized = cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
            this.window_resized(window, cx);
        });
        Self {
            workspace: workspace.downgrade(),
            item,
            screen: Some(screen),
            theme,
            points_per_px,
            fitted,
            settle: None,
            _watch: [follow, activation, resized],
        }
    }

    /// The tile's stream, as this window draws it.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn screen(&self) -> Option<&Entity<ScreenView>> {
        self.screen.as_ref()
    }

    /// The workspace changed: a new stream for the tile (after a reconnect), a new theme, or
    /// the tile gone, which closes this window.
    fn workspace_changed(
        &mut self,
        workspace: &Entity<WorkspaceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = self.item;
        let ws = workspace.read(cx);
        if ws.tile_of(item).is_none() || !ws.popouts.holds(item) {
            workspace.update(cx, |ws, _cx| ws.popouts.forget(item));
            window.remove_window();
            return;
        }
        let screen = ws.screens.get(&item).cloned();
        let theme_changed = ws.theme != self.theme;
        if theme_changed {
            self.theme = ws.theme.clone();
        }
        if screen.as_ref().map(Entity::entity_id) != self.screen.as_ref().map(Entity::entity_id) {
            if let Some(view) = &screen {
                window.focus(&view.read(cx).focus_handle(cx), cx);
                self.fitted = view.read(cx).native();
            }
            self.screen = screen;
            cx.notify();
        } else if theme_changed {
            cx.notify();
        }
    }

    /// The window moved or took a new size: where it stands is saved, and once it holds a
    /// new size the remote window is asked for it.
    fn window_resized(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (item, frame) = (self.item, crate::window_frame::of(window, cx));
        let _ws = self.workspace.update(cx, |ws, cx| ws.popped_moved(item, frame, cx));
        let viewport = window.viewport_size();
        let per_point = 1.0 / self.points_per_px.max(f32::EPSILON);
        let px = |points: f32| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 2")]
            let v = ((points * per_point).round().max(2.0) as u32).next_multiple_of(2);
            v
        };
        let pixels = (px(f32::from(viewport.width)), px(f32::from(viewport.height)));
        self.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_SETTLE).await;
            let _gone = this.update(cx, |this, cx| {
                this.settle = None;
                #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
                let asked = (pixels.0 as f32, pixels.1 as f32);
                this.fitted = asked;
                let _ws = this.workspace.update(cx, |ws, cx| ws.resize_popped(item, pixels, cx));
            });
        }));
    }

    /// The remote window took a size of its own on the worker: the window follows it, once
    /// this frame is drawn.
    fn follow_target(&mut self, native: (f32, f32), window: &Window, cx: &mut Context<Self>) {
        if !moved(native, self.fitted) || self.settle.is_some() {
            return;
        }
        self.fitted = native;
        let wanted = size(px(native.0 * self.points_per_px), px(native.1 * self.points_per_px));
        if wanted != window.viewport_size() {
            cx.defer_in(window, move |_this, window, _cx| window.resize(wanted));
        }
    }
}

impl Render for PopOutView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.screen.clone().map(|view| {
            let (native, kind) = {
                let v = view.read(cx);
                (v.native(), v.target())
            };
            if matches!(kind, CaptureTarget::Window(_)) {
                self.follow_target(native, window, cx);
            }
            let painted = f32::from(window.viewport_size().width) * window.scale_factor();
            view.update(cx, |v, cx| v.set_painted_width(painted, cx));
            view.into_any_element()
        });
        div()
            .id("pop-out")
            .key_context(CTX)
            .size_full()
            .flex()
            .bg(hsla(self.theme.surfaces.canvas))
            .on_action(cx.listener(|this, _: &ToggleOwnWindow, window, cx| {
                let item = this.item;
                let _gone = this.workspace.update(cx, |ws, cx| ws.return_tile(item, cx));
                // The workspace cannot reach into the window whose update this is.
                window.remove_window();
            }))
            .children(body)
    }
}

impl WorkspaceView {
    /// The palette's line for [`ToggleOwnWindow`] on the focused tile, when it has one.
    pub(super) fn own_window_line(
        &self,
        item: &slopty_proto::items::Item,
        bindings: &[gpui::KeyBinding],
    ) -> Option<crate::palette::PaletteItem> {
        if !matches!(item.kind, ItemKind::Window { .. } | ItemKind::Display { .. }) {
            return None;
        }
        let label = if self.popouts.holds(item.id) {
            BACK_TO_WORKSPACE
        } else if self.can_pop_out(item.id) {
            OPEN_OWN_WINDOW
        } else {
            return None;
        };
        Some(crate::palette::PaletteItem::new(label, Box::new(ToggleOwnWindow), bindings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window opens a stream pixel a device pixel, and a target larger than the screen is
    /// scaled down whole, keeping its aspect.
    #[test]
    fn a_window_opens_at_the_remote_size_fitted_to_the_screen() {
        let screen = Size { width: 1512.0, height: 945.0 };
        let small = opening_size((1600.0, 1000.0), 2.0, screen);
        assert_eq!((small.width, small.height), (800.0, 500.0), "one pixel a device pixel");
        let big = opening_size((6016.0, 3384.0), 2.0, screen);
        assert!(big.width <= 1512.0 * SCREEN_SHARE && big.height <= 945.0 * SCREEN_SHARE);
        let aspect = big.width / big.height;
        assert!((aspect - 6016.0 / 3384.0).abs() < 0.01, "the aspect kept: {big:?}");
        let flat = opening_size((1920.0, 1080.0), 1.0, screen);
        assert!(flat.width <= 1512.0 * SCREEN_SHARE, "a 1× screen: a pixel a point");
    }
}
