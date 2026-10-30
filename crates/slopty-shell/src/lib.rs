// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3d2f1e6), see THIRD_PARTY_NOTICES.md
//! slopty-shell — the window, drawn on gpui (`docs/ARCHITECTURE.md` §4,
//! `docs/decisions/layout.md`): the titlebar, the session list, each session's terminal and the
//! views summoned beside it.
//!
//! - [`theme`] — the theme's colours, light and dark, a gpui Global;
//! - [`motion`] — the animation catalog over gpui `Animation` + cubic-bezier;
//! - [`state`] — `AppState` entity + `EngineHandle`, the view model's client;
//! - [`settings`] — the persisted settings, the pane widths among them;
//! - [`shell`] — the window: titlebar, sidebar, terminal, the views beside it and the gate;
//! - [`loaders`] — the pulse loader, the gradient spinner and the boot splash.

// Tests read the wall clock as they please; the shell itself only through `clock`.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

// Voice, dictation, the orb, appshots, the updater, sign-in, sync, agent accounts,
// the pull-request pages and agent visuals are not taken (docs/decisions/ui.md).
pub mod app_menus;
pub mod appearance;
pub mod attachments;
pub mod browser;
pub mod catalog;
pub mod changes;
pub mod clock; // the frame's moment, the shell's one clock
pub mod composer;
pub mod edge_fade;
pub mod file_icons;
pub mod files;
pub mod frost;
mod glass;
mod haptics;
pub mod icons;
pub mod inspector; // the read-only inspector of an agent session, in the right pane
pub mod keys; // how a key reads in tooltips, menus, the palette and the a11y tree
pub(crate) mod image_media;
pub(crate) mod image_viewer;
pub mod links;
mod lit; // the person's message as a faintly lit surface
pub mod live; // Slopty's terminals and remote screens in the shell's chrome
pub mod loaders;
pub mod markdown;
pub mod motion;
mod notice;
pub mod invariants; // what the shell's state must hold, checked as it runs
pub mod overscroll;
pub mod page; // a page an agent shows inline, as its card
pub mod pickers;
pub mod popover;
pub mod presence; // whether the person looks at the window
pub mod project_actions;
pub mod rail;
mod roll_text;
pub mod running_pill;
pub mod settings;
pub mod shell;
pub mod smooth_scroll;
pub mod state;
pub(crate) mod surface_chrome;
pub mod syntax_cache;
pub mod terminal;
pub(crate) mod tool_images;
pub mod theme;
pub mod toast;
pub mod transcript;
pub mod typography;
pub mod visuals;
pub mod window_appearance; // a window wears its terminal's ground's appearance
mod workspace_links;

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

pub use state::EngineBootConfig;
pub use slopty_view_proto::HarnessId;

/// Everything the headed binary passes in (config/env resolution lives in
/// `apps/slopty`, not here).
// No `Debug`; the service it carries has none.
#[derive(Clone)]
pub struct UiConfig {
    /// Data directory — engine stores + `ui-settings.json`.
    pub data_dir: PathBuf,
    /// Harness for doc-command runs until per-chat config lands (M4).
    pub default_harness: HarnessId,
    /// Conversation URL passed by the OS on a cold launch.
    pub initial_url: Option<String>,
    /// The engine: Slopty's server through the `RpcService` in `slopty-ui`.
    pub service: std::sync::Arc<dyn slopty_view_rpc::RpcService>,
    /// Whether the app comes forward once its window is open; a self-test's never does.
    pub activate: bool,
}

impl UiConfig {
    fn boot(&self) -> EngineBootConfig {
        EngineBootConfig {
            data_dir: self.data_dir.clone(),
            default_harness: self.default_harness,
        }
    }
}

/// What a dock-icon reopen needs to rebuild the main window after ⌘W closed it
/// (macOS keeps the process alive with just the menu bar, like zed).
struct ReopenState {
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
}

impl gpui::Global for ReopenState {}

// One name for the device the app runs on, wherever the app speaks of it.
/// This device, as the app's words name it: "this Mac", "this device" on an iPhone or iPad,
/// "this computer" elsewhere.
pub const THIS_DEVICE: &str = if cfg!(target_os = "macos") {
    "this Mac"
} else if cfg!(target_os = "ios") {
    "this device"
} else {
    "this computer"
};

/// Run the headed app: tokio bridge up, engine bootstrap kicked off (probe →
/// connect-or-embed), 1320×880 window (min 900×600) with [`shell::Shell`] as the
/// root view, boot splash overlaid until the engine reports ready.
pub fn run_app(config: UiConfig) {
    // Retain ownership for the whole application lifetime. The bridge's
    // default runtime has only two workers, insufficient for a desktop engine.
    let runtime = tokio::runtime::Runtime::new().expect("desktop Tokio runtime");
    let runtime_handle = runtime.handle().clone();
    let app = gpui_platform::application().with_assets(icons::Assets);
    let (url_tx, url_rx) = futures::channel::mpsc::unbounded::<String>();
    let callback_tx = url_tx.clone();
    app.on_open_urls(move |urls| {
        for url in urls {
            let _ = callback_tx.unbounded_send(url);
        }
    });
    if let Some(url) = config.initial_url.clone() {
        let _ = url_tx.unbounded_send(url);
    }
    // Dock-icon click with no window (⌘W closed it): rebuild the main window
    // around the still-running engine — zed does the same via `on_reopen`
    // (crates/zed/src/main.rs `app.on_reopen`).
    app.on_reopen(|cx| {
        if cx.windows().is_empty()
            && let Some(reopen) = cx.try_global::<ReopenState>()
        {
            let (state, boot) = (reopen.state.clone(), reopen.boot.clone());
            open_main_window(state, boot, cx);
        }
    });
    app.run(move |cx: &mut App| {
        gpui_tokio::init_from_handle(cx, runtime_handle);
        start(config, url_rx, cx);
    });
}

/// The application [`run_app`] makes, with the shell's assets and its Dock-icon reopen,
/// for a host that runs it itself and then calls [`start`].
pub fn application(runtime_handle: tokio::runtime::Handle) -> gpui::Application {
    // Nothing in the shell runs on the handle: it has no HTTP client of its own.
    let _ = runtime_handle;
    let app = gpui_platform::application().with_assets(icons::Assets);
    app.on_reopen(|cx| {
        if cx.windows().is_empty()
            && let Some(reopen) = cx.try_global::<ReopenState>()
        {
            let (state, boot) = (reopen.state.clone(), reopen.boot.clone());
            open_main_window(state, boot, cx);
        }
    });
    app
}

/// What [`run_app`] does once the app runs, apart from the Tokio bridge, so a host app
/// that made its own application and runtime starts the interface in it: settings, typography,
/// theme, motion, the engine bootstrap, the main window and the menus.
pub fn start(
    config: UiConfig,
    mut url_rx: futures::channel::mpsc::UnboundedReceiver<String>,
    cx: &mut App,
) -> gpui::WindowHandle<shell::Shell> {
    {
        gpui_base::init(cx);
        let data_dir = config.boot().data_dir.clone();
        let ui_settings = settings::UiSettings::load(&data_dir);
        settings::init(ui_settings.clone(), data_dir.clone(), cx);
        let font_availability = typography::register_fonts(cx);
        // Typography first: theme installation reads the effective family, so
        // the first frame has the final font and palette without a flash.
        typography::init(
            ui_settings.ui_font_family.clone(),
            ui_settings.ui_font_size,
            ui_settings.terminal_font_family.clone(),
            ui_settings.terminal_font_size,
            ui_settings.code_font_family.clone(),
            ui_settings.code_font_size,
            font_availability,
            cx,
        );
        appearance::init(
            ui_settings.appearance,
            ui_settings.theme_selection,
            ui_settings.accent,
            ui_settings.surface,
            cx,
        );
        motion::init(
            // The OS's reduced motion alone (P12).
            motion::ReduceMotion::System,
            ui_settings.pause_animations_in_background,
            cx,
        );
        composer::init(cx);
        app_menus::init(cx);
        cx.register_url_scheme("slopty").detach();

        let state = cx.new(|cx| {
            let mut state = state::AppState::new();
            state.watch_clock_transitions(cx);
            state
        });
        let url_state = state.clone();
        cx.spawn(async move |cx| {
            while let Some(url) = url_rx.next().await {
                url_state.update(cx, |state, cx| state.open_deep_link(&url, cx));
            }
        })
        .detach();
        cx.set_global(state::EngineService(config.service.clone()));
        state::AppState::bootstrap(state.clone(), config.boot(), cx);
        invariants::watch(&state, cx);

        // Graceful teardown: an in-process engine drains live runs and flushes
        // doc snapshots before the process exits (remote engines outlive us).
        let quit_state = state.clone();
        cx.on_app_quit(move |cx| {
            settings::flush(cx);
            let shutdown =
                quit_state.read(cx).engine().cloned().map(|handle| {
                    gpui_tokio::Tokio::spawn(cx, async move { handle.shutdown().await })
                });
            async move {
                if let Some(task) = shutdown {
                    let _ = task.await;
                }
            }
        })
        .detach();

        cx.set_global(ReopenState {
            state: state.clone(),
            boot: config.boot(),
        });
        let window = open_main_window(state, config.boot(), cx);
        // Native menu bar — macOS gets the standard app menu (About/Services/
        // Hide/Quit ⌘Q), Edit clipboard verbs routed to the focused input, and
        // a Window menu (⌘M/⌘W). Without this, `NSApp.mainMenu` stays nil: no
        // Cmd+Q, and nothing for the system menu bar to show. Set after
        // `open_main_window` because `Shell::new` ran `apply_keymap`
        // synchronously, so `set_menus` reads the final bindings for the ⌘-key
        // equivalents (gpui snapshots the keymap at set time).
        cx.set_menus(app_menus::app_menus());
        // Switching keyboard layout moves the keys behind ⌘-shortcuts (AZERTY
        // reaches ⌘1 as ⌘&): rebind against the new layout, then refresh the
        // menu bar's snapshot of the keymap and redraw the hints that name
        // those keys.
        cx.on_keyboard_layout_change(|cx| {
            let settings = settings::current(cx);
            shell::apply_keymap(cx, &settings.keymap);
            cx.set_menus(app_menus::app_menus());
            cx.refresh_windows();
        })
        .detach();
        if config.activate {
            cx.activate(true); // as the host asks
        }
        window
    }
}

/// Bring Slopty forward, reopening the main window first if ⌘W closed it.
// Public, for the host's Dock click and notes.
pub fn activate_main_window(cx: &mut App) {
    cx.activate(true);
    if cx.windows().is_empty()
        && let Some(reopen) = cx.try_global::<ReopenState>()
    {
        let (state, boot) = (reopen.state.clone(), reopen.boot.clone());
        open_main_window(state, boot, cx);
    }
}

/// Bring Slopty forward on the chat `chat_id`, as a clicked banner does: the host's
/// own notes open their chats this way.
pub fn open_chat(chat_id: String, cx: &mut App) {
    let Some(state) = cx.try_global::<ReopenState>().map(|reopen| reopen.state.clone()) else {
        return;
    };
    open_notification_target(chat_id, &state, cx);
}

/// A clicked banner: bring Slopty forward on its chat, reopening the main window first if ⌘W
/// closed it. Every banner's target is a chat.
fn open_notification_target(target: String, state: &gpui::Entity<state::AppState>, cx: &mut App) {
    activate_main_window(cx);
    // A note of a chat archived or deleted since brings Slopty forward and opens nothing,
    // rather than an empty page in that chat's name.
    if !state.read(cx).chat_listed(&target) {
        return;
    }
    let shell = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<shell::Shell>());
    match shell {
        Some(shell) => {
            let _ = shell.update(cx, |shell, window, cx| {
                window.activate_window();
                shell.open_chat(target.clone(), cx);
                shell.present_landing(target, cx); // one accent fade where it lands
            });
        }
        None => state.update(cx, |state, cx| state.select_chat(Some(target), cx)),
    }
}

fn restored_main_window_bounds(cx: &App) -> (Bounds<gpui::Pixels>, Option<gpui::DisplayId>) {
    let fallback = (Bounds::centered(None, size(px(1320.), px(880.)), cx), None);
    let Some(saved) = settings::current(cx).window_geometry else {
        return fallback;
    };
    let displays = cx.displays();
    let primary = cx
        .primary_display()
        .and_then(|primary| {
            displays
                .iter()
                .position(|display| display.id() == primary.id())
        })
        .unwrap_or(0);
    let geometries: Vec<_> = displays
        .iter()
        .map(|display| {
            let mut geometry = settings::WindowGeometry::from_bounds(display.visible_bounds());
            geometry.display_uuid = display.uuid().ok();
            geometry
        })
        .collect();
    saved
        .restore(&geometries, primary)
        .map_or(fallback, |(index, geometry)| {
            (geometry.bounds(), Some(displays[index].id()))
        })
}

fn save_main_window_geometry(window: &gpui::Window, cx: &mut App) {
    save_window_geometry(window, true, cx);
}

/// `query_display: false` keeps the display recorded by the last bounds
/// change. The close path must not query displays: on X11 the should-close
/// callback runs while the platform client is mutably borrowed, and the
/// display lookup panicked — killing the app before its quit hooks (engine
/// drain, install-on-quit) could run.
fn save_window_geometry(window: &gpui::Window, query_display: bool, cx: &mut App) {
    if window.is_fullscreen() {
        return;
    }
    // macos infers maximization from screen-sized bounds, including ordinary
    // windows; other desktop backends report a distinct maximized variant.
    let WindowBounds::Windowed(bounds) = window.window_bounds() else {
        return;
    };
    let mut geometry = settings::WindowGeometry::from_bounds(bounds);
    geometry.display_uuid = if query_display {
        window.display(cx).and_then(|display| display.uuid().ok())
    } else {
        settings::current(cx)
            .window_geometry
            .and_then(|saved| saved.display_uuid)
    };
    if geometry.is_valid() {
        settings::update(settings::SavePolicy::Debounced, cx, |settings| {
            settings.window_geometry = Some(geometry);
        });
    }
}

fn observe_main_window_geometry<T: 'static>(window: &mut gpui::Window, cx: &gpui::Context<T>) {
    cx.observe_window_bounds(window, |_, window, cx| {
        save_main_window_geometry(window, cx);
    })
    .detach();
}

fn open_main_window(
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
    cx: &mut App,
) -> gpui::WindowHandle<shell::Shell> {
    let (bounds, display_id) = restored_main_window_bounds(cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                display_id,
                window_min_size: Some(size(px(900.), px(600.))),
                // `kind` is deliberately left at its default `WindowKind::Normal`
                // (gpui platform.rs WindowOptions::default), which on macOS maps
                // to `NSNormalWindowLevel` (gpui_macos window.rs) — same as zed's
                // main window. Nothing here raises the window level or touches
                // presentation options; the "menu bar never appears" symptom came
                // from the missing `set_menus` call (nil `NSApp.mainMenu`), not
                // from window kind/level, and `appears_transparent` only affects
                // the titlebar, not the menu bar.
                // macOS: frameless-inset chrome, the traffic lights at 14,15. The strip is
                // custom-drawn. Windows
                // still needs a native title for taskbar previews and Alt+Tab. On
                // Linux/Windows `appears_transparent` hides the system titlebar
                // for our custom-drawn chrome; harmless where unsupported.
                titlebar: Some(TitlebarOptions {
                    title: cfg!(target_os = "windows").then(|| "Slopty".into()),
                    appears_transparent: true,
                    // Native lights are 14px tall: top 14 → center 21, matching
                    // the 38px titlebar row with 4px top-only content padding.
                    traffic_light_position: Some(gpui::point(px(14.), px(14.))),
                }),
                // Our own titlebar strip drags the window (WindowControlArea::
                // Drag + start_window_move) — mark the content view app-owned
                // so AppKit neither dead-zones the strip nor delays clicks.
                app_owns_titlebar_drag: true,
                // Linux: request client-side decorations — Slopty draws its own
                // unified titlebar and (under CSD) its own caption buttons
                // (shell.rs `render_linux_caption_controls`). Leaving this unset
                // requests SERVER decorations, which stacked a compositor
                // titlebar on top of the app's chrome under sway/KDE, while
                // compositors without SSD support (GNOME) went client-side
                // anyway — frameless, and before the shell drew caption buttons,
                // with no window controls at all. The compositor can still
                // override via xdg-decoration negotiation; the shell re-resolves
                // what to draw every frame.
                window_decorations: cfg!(target_os = "linux")
                    .then_some(gpui::WindowDecorations::Client),
                // Frosted shell (macOS): blur the desktop behind the window; the
                // shell paints its frost surface translucent so the sidebar reads
                // as glass (shell.rs root). Elsewhere blur support is compositor
                // roulette — stay opaque.
                // One source of truth with the re-apply loop in `appearance::apply`
                // — if these two ever disagree, vibrancy dies on the first theme
                // change and never comes back.
                window_background: theme::Theme::of(cx).window_background_appearance(),
                app_id: Some("slopty".into()),
                ..Default::default()
            },
            move |window, cx| {
                window.set_rem_size(px(typography::font_size(cx).pixels()));
                // A tooltip sits centred under the control it names, never under the
                // pointer (`docs/decisions/ui.md`, "Tooltips name the action and its key").
                window.set_tooltip_placement(gpui::TooltipPlacement::Below);
                // The theme takes the app's appearance from the window as it opens, which
                // has it even early in launch; from `appearance::init` on it follows the app,
                // never a window, which wears its ground's (`window_appearance`).
                appearance::reconcile(window, cx);
                let shell = cx.new(|cx| {
                    observe_main_window_geometry(window, cx);
                    shell::Shell::new(state, boot, cx)
                });
                save_main_window_geometry(window, cx);
                // A file link Slopty's views ask for (a path ⌘-clicked in a shell) opens
                // as a transcript's does.
                let (opener, here) = (shell.downgrade(), window.window_handle());
                live::serve_files(cx, move |chat, target, cx| {
                    here.update(cx, |_, window, cx| {
                        opener.update(cx, |shell, cx| {
                            shell.open_workspace_file_link(chat, target, window, cx)
                        })
                    })
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false)
                });
                // A web link one hands over (quick select) opens as a transcript's does.
                let (linker, here) = (shell.downgrade(), window.window_handle());
                live::serve_links(cx, move |chat, url, cx| {
                    here.update(cx, |_, window, cx| {
                        linker.update(cx, |shell, cx| {
                            shell.open_session_web_link(chat, url, window, cx)
                        })
                    })
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false)
                });
                let weak_shell = shell.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    let should_close = weak_shell
                        .update(cx, |shell, cx| shell.prepare_window_close(cx))
                        .unwrap_or(true);
                    if should_close {
                        save_window_geometry(window, false, cx);
                        settings::flush(cx);
                    }
                    should_close
                });
                shell
            },
        )
        .expect("failed to open window");
    // Belt and braces: assert the blur once the window actually exists. The
    // `WindowOptions` value is applied during creation, before the view is
    // attached; re-pushing it here means a window is never left opaque.
    appearance::reapply_window_background(cx);
    handle
}

