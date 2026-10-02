//! The Slopty macOS app: logging, tokio runtime, latency-critical activity, then the shared
//! workspace from `slopty-app`.

#![forbid(unsafe_code)]

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
/// shown next to them come from the keymap in effect whenever the menu is built.
///
/// The Edit menu names the text fields' own actions (gpui-kit's), which every field answers:
/// the editors, the composer and the settings' search, and the terminal as its own. Cut, Copy,
/// Paste and Select All also go down the responder chain as AppKit's selectors, so a web
/// page's view answers them too. An item nothing focused answers is greyed, as macOS greys it.
fn menus() -> Vec<Menu> {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    use slopty_app::{Minimize, OpenHelp, ShowWindow, Zoom};
    use slopty_ui::terminal::{Find, FindNext, FindPrev};
    use slopty_ui::workspace::{
        AddWindow, CenterColumn, CloseItem, ConsumeOrExpelLeft, ConsumeOrExpelRight, CycleWidth,
        FocusColumnLeft, FocusColumnRight, FocusDown, FocusUp, FontLarger, FontReset, FontSmaller,
        FullscreenTile, MaximizeColumn, MoveColumnLeft, MoveColumnRight, MoveDown, MoveUp,
        NewAgent, NewNote, NewTerminal, NextAttention, OpenPalette, ToggleMute, ToggleOverview,
        ToggleStats, ToggleTabbed, UndoClose,
    };
    vec![
        Menu::new("Slopty").items([
            MenuItem::action("Settings…", slopty_app::OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Add Worker…", slopty_app::AddWorker),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Slopty", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Slopty", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Shell", NewTerminal),
            MenuItem::action("New Agent", NewAgent),
            MenuItem::action("New Note", NewNote),
            MenuItem::action("Add Window…", AddWindow),
            MenuItem::separator(),
            MenuItem::action("Close Tile", CloseItem),
            MenuItem::action("Undo Close", UndoClose),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrev),
        ]),
        Menu::new("View").items([
            MenuItem::action("Commands…", OpenPalette),
            MenuItem::action("Overview", ToggleOverview),
            MenuItem::separator(),
            MenuItem::action("Bigger Text", FontLarger),
            MenuItem::action("Smaller Text", FontSmaller),
            MenuItem::action("Actual Text Size", FontReset),
            MenuItem::separator(),
            MenuItem::action("Stream Stats", ToggleStats),
            MenuItem::action("Mute Sound", ToggleMute),
        ]),
        Menu::new("Layout").items([
            MenuItem::action("Focus Column Left", FocusColumnLeft),
            MenuItem::action("Focus Column Right", FocusColumnRight),
            MenuItem::action("Focus Up", FocusUp),
            MenuItem::action("Focus Down", FocusDown),
            MenuItem::separator(),
            MenuItem::action("Move Column Left", MoveColumnLeft),
            MenuItem::action("Move Column Right", MoveColumnRight),
            MenuItem::action("Move Up", MoveUp),
            MenuItem::action("Move Down", MoveDown),
            MenuItem::action("Consume or Expel Left", ConsumeOrExpelLeft),
            MenuItem::action("Consume or Expel Right", ConsumeOrExpelRight),
            MenuItem::separator(),
            MenuItem::action("Cycle Column Width", CycleWidth),
            MenuItem::action("Maximize Column", MaximizeColumn),
            MenuItem::action("Fullscreen Tile", FullscreenTile),
            MenuItem::action("Center Column", CenterColumn),
            MenuItem::action("Tabbed Column", ToggleTabbed),
            MenuItem::separator(),
            MenuItem::action("Next Thing Needing You", NextAttention),
        ]),
        // Named "Window", AppKit lists the open windows under these and adds its own tiling.
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Slopty", ShowWindow),
        ]),
        // Named "Help", AppKit puts its search of the menus at the top.
        Menu::new("Help").items([
            MenuItem::action("Slopty Help", OpenHelp),
            MenuItem::action("Keyboard Shortcuts", slopty_app::OpenKeyboardShortcuts),
        ]),
    ]
}

/// The main window's options at launch and whenever it opens again: 1280 × 800 in the middle
/// of the main screen, unless the layout kept where it last stood.
fn window_options(cx: &gpui::App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("Slopty".into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
        }),
        ..Default::default()
    }
}

/// Run the app: the crash reporter, logging, the runtime, then the workspace window.
pub fn main() -> Result<()> {
    slopty_crash::install(slopty_crash::Process::App, &slopty_platform::dirs::data_dir());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // The link's writer, the session pumps and noq's drivers carry every keystroke and echo.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(slopty_platform::user_interactive_thread)
        .build()?;
    let handle = runtime.handle().clone();
    // Held for the whole run: App Nap would otherwise throttle the link's heartbeats while the
    // window is covered and the direct path would be abandoned.
    let _activity = slopty_platform::Activity::latency_critical("Slopty remote session");

    let app = gpui_kit::application().with_assets(slopty_ui::icons::Assets);
    // A click on the Dock icon brings the window back after it was closed.
    app.on_reopen(slopty_app::show_main_window);
    app.run(move |cx| {
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
        if let Err(e) = slopty_app::open_workspace(cx, handle, window_options) {
            tracing::error!(error = %e, "open window");
            cx.quit();
            return;
        }
        // After `open_workspace`: the menu reads its shortcut labels from the keymap, which the
        // workspace fills in, and it is built again whenever the keys are rebound.
        slopty_app::set_app_menus(cx, menus);
        if !slopty_app::self_test() {
            cx.activate(true);
        }
    });
    Ok(())
}
