//! The Slopty macOS app: logging, tokio runtime, latency-critical activity, then the shared
//! workspace from `slopty-app`.

use anyhow::Result;
use gpui::{
    Bounds, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, WindowBounds, WindowOptions, px,
    size,
};

mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        slopty,
        [
            /// Quit.
            Quit,
            /// Hide the app.
            Hide,
            /// Hide every other app.
            HideOthers,
            /// Show every hidden app.
            ShowAll,
        ]
    );
}
use actions::{Hide, HideOthers, Quit, ShowAll};

/// The application menu. Items name the same actions the key bindings do, so the shortcuts
/// shown next to them come from the keymap and the two can never disagree.
fn menus() -> Vec<Menu> {
    use slopty_ui::canvas::{
        AddWindow, CloseItem, FitAll, NewAgent, NewNote, NewTerminal, NextAttention, ToggleMute,
        ToggleStats, ZoomIn, ZoomOut, ZoomReset,
    };
    use slopty_ui::terminal::{Copy, Find, FindNext, FindPrev, Paste};
    vec![
        Menu::new("Slopty").items([
            MenuItem::action("Settings…", slopty_app::OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Slopty", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Slopty", Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrev),
        ]),
        Menu::new("Canvas").items([
            MenuItem::action("New Shell", NewTerminal),
            MenuItem::action("New Agent", NewAgent),
            MenuItem::action("New Note", NewNote),
            MenuItem::action("Add Window…", AddWindow),
            MenuItem::separator(),
            MenuItem::action("Next Agent Needing You", NextAttention),
            MenuItem::action("Mute Window", ToggleMute),
            MenuItem::action("Stream Stats", ToggleStats),
            MenuItem::separator(),
            MenuItem::action("Close Item", CloseItem),
        ]),
        Menu::new("Host").items([
            MenuItem::action("Next Host", slopty_app::NextHost),
            MenuItem::action("Previous Host", slopty_app::PrevHost),
            MenuItem::separator(),
            MenuItem::action("Add Host…", slopty_app::AddHost),
            MenuItem::action("Forget Host", slopty_app::ForgetHost),
        ]),
        Menu::new("View").items([
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ZoomReset),
            MenuItem::action("Fit All", FitAll),
        ]),
    ]
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| {
                // iroh's path events carry the abandon reason; always keep them.
                tracing_subscriber::EnvFilter::new("info,iroh::_events::path=debug")
            },
        ))
        .with_writer(std::io::stderr)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = runtime.handle().clone();
    // Held for the whole run: App Nap would otherwise throttle the link's heartbeats while the
    // window is covered and the direct path would be abandoned.
    let _activity = slopty_platform::Activity::latency_critical("Slopty remote session");

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("cmd-alt-h", HideOthers, None),
        ]);
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Slopty".into()),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
            }),
            ..Default::default()
        };
        if let Err(e) = slopty_app::open_workspace(cx, handle, options) {
            tracing::error!(error = %e, "open window");
            cx.quit();
            return;
        }
        // After `open_workspace`: the menu reads its shortcut labels from the keymap, which the
        // workspace fills in.
        cx.set_menus(menus());
        cx.activate(true);
    });
    Ok(())
}
