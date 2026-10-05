//! The main window, opened again after it was closed and opened where it last stood.
//!
//! It opens on the workspace at launch, and again after it was closed: a click on the Dock
//! icon, ⌘N with no window, the Window menu. Where it stands is kept with the layout, so a
//! relaunch opens it there.
//!
//! The workspace outlives its window: closing the window leaves every link, tile and stream
//! running, as a Mac app's document outlives its window, and the next window shows them as
//! they are.

use gpui::{App, AppContext as _, Entity, Focusable as _, WeakEntity, WindowOptions};
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
    // each names where it stands aside: ⌘N in the workspace is a new shell, and only with no
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
    let options = WindowOptions {
        window_bounds,
        display_id,
        inactive_frame_interval: None,
        focus: options.focus && !self_test(),
        ..options
    };
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
        });
    })?;
    Ok(window)
}
