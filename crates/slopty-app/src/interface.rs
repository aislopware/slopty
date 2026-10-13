//! The app's window: the shell (`slopty-shell`), over this workspace's workers and threads.
//!
//! The workspace runs as before, with no window of its own: its links, its threads and its
//! terminals are the shell's data, served by Slopty's view model ([`slopty_ui::view_model`]).
//! The shell keeps its own settings, typography, theme and window; the workspace hears the window
//! come forward and go back, as it heard its own.

use gpui::{AnyWindowHandle, App, Entity};
use slopty_settings::Loaded;
use slopty_shell::live::{Hosted, TerminalLook};
use slopty_ui::view_model::live::{LiveLook, LiveView};
use slopty_ui::workspace::WorkspaceView;

use crate::{Workspace, self_test, settings};

/// The id the shell knows this app by: a client, never one of the devices it lists, so
/// every chat names the worker it runs on.
const CLIENT: &str = "slopty-client";

/// Open the shell's window on `workspace`, its view model served from the workspace, and watch
/// the window for the workspace. `loaded` is the settings read at launch.
pub(crate) fn open(
    workspace: &Entity<Workspace>,
    runtime: tokio::runtime::Handle,
    loaded: Loaded,
    cx: &mut App,
) -> AnyWindowHandle {
    gpui_tokio::init_from_handle(cx, runtime);
    let (view_model, feed, asks) = slopty_ui::view_model::ViewModel::new(CLIENT.to_owned());
    let view = workspace.read(cx).view.clone();
    workspace.update(cx, |ws, cx| {
        ws.interface_feed = Some(std::sync::Arc::clone(&feed));
        ws.tell_server_presence(cx);
        ws.tell_local_device();
    });
    view.update(cx, |v, cx| v.serve_interface(feed, asks, cx));
    host_live_views(&view, cx);
    // The shell's own deep links are not Slopty's: nothing sends on this.
    let (_links, links) = futures::channel::mpsc::unbounded();
    let config = slopty_shell::UiConfig {
        data_dir: slopty_platform::dirs::data_dir().join("interface"),
        // slopty: Slopty's server is the only backend.
        default_harness: slopty_view_proto::HarnessId::CLAUDE_CODE,
        initial_url: None,
        service: view_model,
        activate: !self_test(),
    };
    let shell = slopty_shell::start(config, links, cx);
    // The shell's and the kit's bindings are in: Tab and the rest go back to the views that take
    // every key.
    slopty_ui::key_takers::release(cx);
    let watched = workspace.clone();
    let _open = shell.update(cx, move |_shell, window, cx| {
        let observed = watched.clone();
        let appearance = window.observe_window_appearance(move |window, cx| {
            let dark = settings::is_dark(window.appearance());
            observed.update(cx, |ws, cx| ws.set_window_dark(dark, cx));
        });
        let dark = settings::is_dark(window.appearance());
        let handle = window.window_handle();
        watched.update(cx, |ws, cx| {
            let activation = cx.observe_window_activation(window, |ws, window, cx| {
                let active = window.is_window_active();
                ws.view.update(cx, |v, cx| v.set_app_active(active, cx));
                ws.set_active(active, cx);
                ws.tell_presence_in(window, cx);
                if active {
                    ws.wake_this_mac_run();
                }
            });
            ws.window_subscriptions = vec![appearance, activation];
            ws.window_dark = dark;
            ws.window = Some(handle);
            // The clipboard follows the keyboard of this window's live views.
            ws.view.update(cx, |v, cx| v.set_pasteboard(crate::pasteboard(), Some(handle), cx));
            ws.apply_loaded(loaded, cx);
        });
    });
    shell.into()
}

/// Hand the shell the workspace's own views by the ids its view model answers: a shell's
/// terminal and a remote window or desktop (`slopty_shell::live`).
fn host_live_views(view: &Entity<WorkspaceView>, cx: &mut App) {
    let terminals = view.downgrade();
    let screens = view.downgrade();
    let watched = view.downgrade();
    slopty_shell::live::install(
        cx,
        move |id, look, cx| {
            let look = live_look(look);
            terminals.upgrade()?.update(cx, |v, cx| v.hosted_terminal(id, &look, cx)).map(hosted)
        },
        move |id, cx| screens.upgrade()?.update(cx, |v, cx| v.hosted_screen(id, cx)).map(hosted),
        move |id, changed, cx| {
            watched.upgrade()?.update(cx, |v, cx| v.hosted_watch(id, changed, cx))
        },
    );
}

/// The shell's terminal look, as the workspace takes it.
fn live_look(look: &TerminalLook) -> LiveLook {
    LiveLook {
        family: look.family.clone(),
        font_size: look.font_size,
        line_height: look.line_height,
        padding: look.padding,
        background: look.background,
        foreground: look.foreground,
        cursor: look.cursor,
        selection: look.selection,
        ansi: look.ansi,
    }
}

/// A workspace view, as the shell places it.
fn hosted(view: LiveView) -> Hosted {
    Hosted { view: view.view, focus: view.focus, title: view.title, ended: view.ended }
}
