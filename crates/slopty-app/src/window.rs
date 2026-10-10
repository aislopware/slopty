//! The main window, opened again after it was closed and opened where it last stood.
//!
//! It opens on the workspace at launch, and again after it was closed: a click on the Dock
//! icon, ⌘N with no window, the Window menu. Where it stands is kept with the layout, so a
//! relaunch opens it there.
//!
//! The workspace outlives its window: closing the window leaves every link, tile and stream
//! running, as a Mac app's document outlives its window, and the next window shows them as
//! they are.

use gpui::{
    App, AppContext as _, Bounds, Entity, Focusable as _, Pixels, Size, WeakEntity, WindowBounds,
    WindowOptions, px,
};
use gpui_kit::component::Root;
use slopty_settings::Loaded;
use slopty_ui::workspace::WorkspaceView;

use crate::{Workspace, self_test, settings};

/// The window commands of the app's menus.
pub mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        slopty,
        [
            /// Bring the main window to the front, opening it again if it was closed.
            ShowWindow,
            /// Minimise the window in front to the Dock.
            Minimize,
            /// Zoom the window in front, or put it back.
            Zoom,
            /// Open the project's page in the browser.
            OpenHelp,
        ]
    );
}

/// Where the project's documentation lives.
pub const HELP_URL: &str = "https://github.com/aislopware/slopty#readme";

/// The smallest the main window can be made: the narrowest phone the layout is drawn for.
///
/// It is 375 pt wide, and tall enough for its bar, a tile's header and a composer. Every size
/// a Mac window can take is then a size the layout was drawn for; below it the phone bar's
/// lights, title, bell and "…" ran into each other (`.research/responsive-2026-10-06.md`,
/// finding 13).
pub const MIN_SIZE: Size<Pixels> = Size { width: px(375.0), height: px(480.0) };

/// `options` held to [`MIN_SIZE`]: the window can't be made smaller, and a frame kept from
/// before the floor opens at least that large, where it stood.
fn floored(options: WindowOptions) -> WindowOptions {
    let floor = |b: Bounds<Pixels>| Bounds { origin: b.origin, size: b.size.max(&MIN_SIZE) };
    let window_bounds = options.window_bounds.map(|bounds| match bounds {
        WindowBounds::Windowed(b) => WindowBounds::Windowed(floor(b)),
        WindowBounds::Maximized(b) => WindowBounds::Maximized(floor(b)),
        WindowBounds::Fullscreen(b) => WindowBounds::Fullscreen(floor(b)),
    });
    WindowOptions { window_bounds, window_min_size: Some(MIN_SIZE), ..options }
}

/// The main window's options, made afresh for each window ([`WindowOptions`] is made once).
pub type MakeOptions = std::rc::Rc<dyn Fn(&App) -> WindowOptions>;

/// The workspace the main window shows and how that window is made, kept to open it again.
struct Main {
    workspace: WeakEntity<Workspace>,
    options: MakeOptions,
}

impl gpui::Global for Main {}

/// Keep `workspace` and `options` for opening the main window again, and answer the window
/// actions.
pub(crate) fn install(workspace: &Entity<Workspace>, options: MakeOptions, cx: &mut App) {
    cx.set_global(Main { workspace: workspace.downgrade(), options });
    cx.on_action(|_: &actions::ShowWindow, cx| show(cx));
    cx.on_action(|_: &actions::Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _gone = window.update(cx, |_root, window, _cx| window.minimize_window());
        }
    });
    cx.on_action(|_: &actions::Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _gone = window.update(cx, |_root, window, _cx| window.zoom_window());
        }
    });
    cx.on_action(|_: &actions::OpenHelp, cx| cx.open_url(HELP_URL));
    // A binding with no context outranks every other (GPUI ranks it as deep as the focus), so
    // each names where it stands aside: ⌘N is the workspace's to bind, and only with no
    // workspace window (none at all, through the menu bar, or a popped-out tile's) does it
    // bring the workspace back; ⌘M in a remote picture is the worker's.
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-n", actions::ShowWindow, Some("!Workspace")),
        gpui::KeyBinding::new("cmd-m", actions::Minimize, Some("!Screen")),
    ]);
}

/// Bring the main window to the front, or open it again where it stood if it was closed.
pub fn show(cx: &mut App) {
    let Some((workspace, options)) = cx
        .try_global::<Main>()
        .and_then(|main| Some((main.workspace.upgrade()?, std::rc::Rc::clone(&main.options))))
    else {
        return;
    };
    let shown = workspace.read(cx).window.filter(|w| cx.windows().contains(w));
    if let Some(window) = shown {
        let _gone = window.update(cx, |_root, window, _cx| window.activate_window());
    } else if let Err(e) = open(&workspace, options(cx), None, cx) {
        tracing::error!(error = %e, "open the window again");
        return;
    }
    cx.activate(true);
}

/// Open the main window on `workspace`, at the frame the layout kept when there is one: the
/// window's appearance and activation followed, where it stands kept, the frame probe and the
/// root over the workspace, files dropped on it taken, and the keyboard in the workspace.
/// `loaded` is the launch's settings, applied once the window's appearance is known.
pub(crate) fn open(
    workspace: &Entity<Workspace>,
    options: WindowOptions,
    loaded: Option<Loaded>,
    cx: &mut App,
) -> anyhow::Result<gpui::WindowHandle<Root>> {
    let saved = workspace.read(cx).view.read(cx).window_frame().cloned();
    let (window_bounds, display_id) = match saved {
        Some(frame) => {
            let (bounds, display) = slopty_ui::window_frame::bounds(&frame, cx);
            (Some(bounds), display)
        }
        None => (options.window_bounds, options.display_id),
    };
    // Terminals and remote desktops stay at full rate while another app has the keyboard: a
    // second display is watched while typing elsewhere.
    // The self-test's window comes up in front but takes no keyboard: its keys arrive over the
    // socket, and the machine's keyboard belongs to whoever is using it.
    let options = floored(WindowOptions {
        window_bounds,
        display_id,
        inactive_frame_interval: None,
        focus: options.focus && !self_test(),
        ..options
    });
    let root_view = workspace.clone();
    let window = cx.open_window(options, move |window, cx| {
        // The theme follows the window's appearance while `theme.appearance = "system"`.
        let observed = root_view.clone();
        let appearance = window.observe_window_appearance(move |window, cx| {
            let dark = settings::is_dark(window.appearance());
            observed.update(cx, |ws, cx| ws.set_window_dark(dark, cx));
        });
        let dark = settings::is_dark(window.appearance());
        root_view.update(cx, |ws, cx| {
            // A worker's clipboard is watched only while this app is frontmost, and this Mac's
            // checklist is read again on the way back from System Settings.
            let activation = cx.observe_window_activation(window, |ws, window, cx| {
                let active = window.is_window_active();
                ws.view.update(cx, |v, cx| v.set_app_active(active, cx));
                ws.set_active(active, cx);
                ws.tell_presence_in(window, cx);
                if active {
                    ws.this_mac_activated(window, cx);
                }
            });
            let moved = cx.observe_window_bounds(window, |ws, window, cx| {
                let frame = slopty_ui::window_frame::of(window, cx);
                ws.view.update(cx, |v, cx| v.set_window_frame(frame, cx));
            });
            // A window opened again replaces the closed one's watches.
            ws.window_subscriptions = vec![appearance, activation, moved];
            ws.window_dark = dark;
            if let Some(loaded) = loaded {
                ws.apply_loaded(loaded, cx);
            }
        });
        // The frame probe times every frame from the root down.
        let framed = cx.new(|_| slopty_ui::frames::Framed::new(root_view));
        cx.new(|cx| Root::new(framed, window, cx))
    })?;
    window.update(cx, |_root, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.window = Some(window.window_handle());
            ws.view.update(cx, |_v, cx| WorkspaceView::accept_dropped_files(window, cx));
            let handle = ws.view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            // A phone opens again on its home: what waits on the person, before any tile. The
            // first run opens on the first shell, which is what the person just asked for.
            ws.view.update(cx, |v, cx| {
                if v.relaunched() {
                    v.show_home(window, cx);
                }
            });
        });
    })?;
    Ok(window)
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, WindowBounds, WindowOptions, point, px, size};

    use super::{MIN_SIZE, floored};

    /// The main window can't be made narrower than the narrowest phone, and a frame kept from
    /// a smaller window opens at the floor where it stood; a larger one is left as it was.
    #[test]
    fn the_window_is_never_narrower_than_a_phone() {
        let at =
            |w: f32, h: f32| Bounds { origin: point(px(40.0), px(60.0)), size: size(px(w), px(h)) };
        let open = |bounds| {
            floored(WindowOptions { window_bounds: Some(bounds), ..WindowOptions::default() })
        };
        let small = open(WindowBounds::Windowed(at(300.0, 900.0)));
        assert_eq!(small.window_min_size, Some(MIN_SIZE));
        assert_eq!(
            small.window_bounds,
            Some(WindowBounds::Windowed(at(375.0, 900.0))),
            "where it stood"
        );
        let short = open(WindowBounds::Maximized(at(800.0, 300.0)));
        assert_eq!(short.window_bounds, Some(WindowBounds::Maximized(at(800.0, 480.0))));
        let roomy = open(WindowBounds::Windowed(at(1280.0, 800.0)));
        assert_eq!(
            roomy.window_bounds,
            Some(WindowBounds::Windowed(at(1280.0, 800.0))),
            "left alone"
        );
        assert_eq!(
            floored(WindowOptions::default()).window_min_size,
            Some(MIN_SIZE),
            "a new window too"
        );
        assert_eq!(MIN_SIZE.width, px(375.0), "the narrowest phone");
    }
}
