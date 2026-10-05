//! The Slopty macOS app: logging, tokio runtime, latency-critical activity, then the shared
//! workspace from `slopty-app`.

#![forbid(unsafe_code)]

use anyhow::Result;
use gpui::{Bounds, Menu, MenuItem, SystemMenuType, WindowBounds, WindowOptions, px, size};

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

/// The menu bar: the shared menus (`slopty_app::menus`), whose application menu ends with the
/// Mac's own items.
fn menus() -> Vec<Menu> {
    slopty_app::menus::menus(
        vec![
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Slopty", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Slopty", Quit),
        ],
        false,
    )
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

    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    // A click on the Dock icon brings the window back after it was closed.
    app.on_reopen(slopty_app::show_main_window);
    app.run(move |cx| {
        gpui_kit::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.bind_keys(slopty_ui::keymap::app_chords(Quit, Hide, HideOthers));
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

#[cfg(test)]
mod tests {
    use slopty_ui::palette::PaletteRun;

    use super::*;

    /// The File menu opens a file, a folder and a page, uploads and downloads, and saves, by the
    /// very actions the palette's lines run, so the menu and the palette never drift apart.
    #[test]
    fn the_file_menu_opens_and_saves_as_the_palette_does() {
        let menus = menus();
        let file = menus.iter().find(|m| m.name == "File").expect("a File menu");
        let actions: Vec<&dyn gpui::Action> = file
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(action.as_ref()),
                _ => None,
            })
            .collect();
        let palette = slopty_ui::workspace::palette_items();
        let labels = [
            "Open file…",
            "Open folder…",
            "Open URL…",
            slopty_ui::folder::UPLOAD,
            slopty_ui::folder::DOWNLOAD,
            "Save file",
            slopty_ui::workspace::SAVE_A_COPY,
        ];
        for label in labels {
            let line = palette.iter().find(|l| l.label == label).expect(label);
            let PaletteRun::Action(wanted) = &line.run else { panic!("{label} runs an action") };
            assert!(
                actions.iter().any(|a| a.partial_eq(wanted.as_ref())),
                "the File menu runs {label}"
            );
        }
    }
}
