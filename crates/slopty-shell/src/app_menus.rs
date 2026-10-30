// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Native menu bar + app-level window actions (macOS-first).
//!
//! slopty never called `cx.set_menus`, so on macOS `NSApp.mainMenu` stayed nil:
//! no app menu, no ⌘Q quit, and nothing for the auto-hidden system menu bar to
//! reveal on hover (gpui only calls `setMainMenu_` from `set_menus` —
//! `gpui_macos/src/platform.rs` `fn set_menus`). Structure ported from zed's
//! `crates/zed/src/zed/app_menus.rs` and the gpui `set_menus.rs` example at the
//! pinned rev (f14fea9bf3c9).
//!
//! Wiring: [`init`] registers the global action handlers (run once at boot),
//! `window_actions` the window verbs on each shell window's root, [`bind_keys`] installs the
//! fixed application shortcuts (re-run by `shell::apply_keymap`, which clears every binding
//! first), and [`app_menus`] builds the menu bar handed to `cx.set_menus` in `run_app`.

use gpui::{
    Action, App, Context, InteractiveElement, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType,
    Window,
};

use crate::appearance::{self, AppearanceMode};
use crate::{composer, shell};

mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        slopty,
        [
            /// Show the About panel.
            About,
            /// Quit once every open file is saved.
            Quit,
            /// Hide the app.
            Hide,
            /// Hide every other app.
            HideOthers,
            /// Show every hidden app again.
            ShowAll,
            /// Minimize the active window to the Dock.
            Minimize,
            /// Zoom the active window, or restore it.
            Zoom,
            /// Close the active window.
            CloseWindow,
            /// Close the focused pane, or the window when only the session's is left.
            // ⌘W closes the tab, ⇧⌘W the window (`docs/decisions/ui.md`, "⌘W closes the tab").
            CloseTab,
            /// Follow the system's light or dark appearance.
            AppearanceSystem,
            /// Always use the light appearance.
            AppearanceLight,
            /// Always use the dark appearance.
            AppearanceDark,
        ]
    );
}

pub use actions::{
    About, AppearanceDark, AppearanceLight, AppearanceSystem, CloseTab, CloseWindow, Hide,
    HideOthers, Minimize, Quit, ShowAll, Zoom,
};

/// Register the global handlers backing the menu bar and its shortcuts.
///
/// Call once at boot, before `cx.set_menus`.
pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    native_quit::init(cx);
    cx.on_action(quit);
    #[cfg(target_os = "macos")]
    cx.on_action(|_: &About, _| about_panel::show());
    // Application-menu verbs — gpui wraps NSApp `hide` / `hideOtherApplications`
    // / `unhideAllApplications` (zed registers the same trio in
    // crates/zed/src/zed.rs `init`).
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    // A shell window takes its window verbs itself (`window_actions`); these are for a verb no
    // window took, chosen in the menu bar over a window that is not a shell's, or over none.
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.on_action(close_window);
    cx.on_action(close_tab);
    // Appearance. Each verb persists and repaints every window; see
    // `appearance::set_mode`.
    cx.on_action(|_: &AppearanceSystem, cx| appearance::set_mode(AppearanceMode::System, cx));
    cx.on_action(|_: &AppearanceLight, cx| appearance::set_mode(AppearanceMode::Light, cx));
    cx.on_action(|_: &AppearanceDark, cx| appearance::set_mode(AppearanceMode::Dark, cx));
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut Window)) {
    if let Some(window) = cx.active_window() {
        // A window closed meanwhile has nothing left to minimize or zoom.
        let _gone = window.update(cx, |_, window, _| f(window));
    }
}

/// All app-owned quit paths must flush file buffers before GPUI destroys windows.
fn quit(_: &Quit, cx: &mut App) {
    request_quit(cx);
}

pub(crate) fn request_quit(cx: &mut App) {
    // Actions may arrive while GPUI has the active window borrowed. Inspect
    // roots only after that dispatch completes.
    cx.defer(prepare_quit);
}

fn prepare_quit(cx: &mut App) {
    let mut ready = true;
    for window in cx.windows() {
        if let Some(window) = window.downcast::<shell::Shell>() {
            ready &= window.update(cx, |shell, _, cx| shell.prepare_quit(cx)).unwrap_or(false);
        }
    }
    if ready {
        quit_after_save(cx);
    }
}

pub(crate) fn quit_after_save(cx: &App) {
    #[cfg(target_os = "macos")]
    native_quit::allow();
    cx.quit();
}

fn close_window(_: &CloseWindow, cx: &mut App) {
    cx.defer(|cx| close_active_window(false, cx));
}

// ⌘W closes the focused pane of the session's view (a split shell, a view summoned beside
// it), and the window only when the session's own pane is all there is.
fn close_tab(_: &CloseTab, cx: &mut App) {
    cx.defer(|cx| close_active_window(true, cx));
}

/// Close the active window, or with `pane_first` the focused pane when it is not the session's.
fn close_active_window(pane_first: bool, cx: &mut App) {
    if let Some(window) = cx.active_window() {
        if let Some(window) = window.downcast::<shell::Shell>() {
            // A window closed meanwhile has nothing left to close.
            let _gone = window.update(cx, |shell, window, cx| close(shell, pane_first, window, cx));
        } else {
            let _gone = window.update(cx, |_, window, _| window.remove_window());
        }
    }
}

/// Close `shell`'s focused pane with `pane_first` when it is not the session's, else its window,
/// the unsaved-file gate and all. On a touch screen the window is the app's scene, which the
/// system shows and puts away: there only the panes beside the session close.
fn close(
    shell: &mut shell::Shell,
    pane_first: bool,
    window: &mut Window,
    cx: &mut Context<shell::Shell>,
) {
    if pane_first && shell.close_focused_pane(cx) {
        return;
    }
    if !shell::touch_screen() && shell.prepare_window_close(cx) {
        window.remove_window();
    }
}

/// `root`, a shell window's, taking the window verbs for the window they were pressed in.
///
/// A key reaches the window that has the keyboard whether or not the app is the key app, so
/// the window is not looked up: the active window is the key app's key window, none while the
/// app is in the background (as the e2e guest's always is), and ⌘W there would close nothing.
/// The global handlers ([`init`]) stay for the menu bar with no window to take it.
pub(crate) fn window_actions<E: InteractiveElement>(root: E, cx: &Context<shell::Shell>) -> E {
    root.on_action(cx.listener(|shell, _: &CloseTab, window, cx| close(shell, true, window, cx)))
        .on_action(
            cx.listener(|shell, _: &CloseWindow, window, cx| close(shell, false, window, cx)),
        )
        .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
        .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
}

/// The keyboard the fixed application shortcuts are bound for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Keyboard {
    /// A Mac's: ⌘ keys, the window's and the app's verbs among them.
    Mac,
    /// An iPad's or iPhone's hardware keyboard: ⌘ keys, ⌘W closing the focused pane. The app and
    /// its window are the system's there: ⌘H and ⌘Q are iPadOS's own, and nothing minimizes,
    /// zooms or closes a scene's window.
    Ios,
    /// Any other: Ctrl keys.
    Other,
}

impl Keyboard {
    /// This build's.
    const HERE: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(target_os = "ios") {
        Self::Ios
    } else {
        Self::Other
    };
}

/// Fixed app-level shortcuts backing the menu key equivalents.
///
/// These live outside the customizable keymap; `shell::apply_keymap` calls this after its
/// `clear_key_bindings` so they survive keymap re-application. Settings follows the platform
/// convention everywhere (⌘, on every Apple keyboard, Ctrl+, elsewhere), and ⌘W closes the
/// focused pane on an iPad's keyboard as on a Mac's; the app's and its windows' own verbs are a
/// Mac's alone.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(app_key_bindings(Keyboard::HERE));
}

/// The binding table behind [`bind_keys`] — `KeyBinding` construction is pure
/// (no `App`), so unit tests can inspect it directly.
fn app_key_bindings(keyboard: Keyboard) -> Vec<KeyBinding> {
    let settings = if keyboard == Keyboard::Other { "ctrl-," } else { "cmd-," };
    let mut bindings = vec![KeyBinding::new(settings, shell::OpenSettings, None)];
    if keyboard != Keyboard::Other {
        bindings.push(KeyBinding::new("cmd-w", CloseTab, None));
    }
    if keyboard == Keyboard::Mac {
        bindings.extend([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-shift-w", CloseWindow, None),
        ]);
    }
    bindings
}

/// A window command the menu bar and the command palette both offer, named once in
/// sentence case; the menu bar writes it in Title Case ([`title_case`], the macOS rule) and macOS
/// writes its key beside it, the palette its key from the keymap.
#[derive(Clone, Copy)]
pub(crate) struct WindowCommand {
    pub label: &'static str,
    pub icon: &'static str,
    pub action: fn() -> Box<dyn Action>,
}

impl PartialEq for WindowCommand {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label
    }
}

impl std::fmt::Debug for WindowCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

impl WindowCommand {
    /// Its row in a menu-bar menu.
    fn menu_item(&self) -> MenuItem {
        MenuItem::Action {
            name: title_case(self.label).into(),
            action: (self.action)(),
            os_action: None,
            checked: false,
            disabled: false,
        }
    }
}

macro_rules! command {
    ($label:literal, $icon:expr, $action:expr) => {
        WindowCommand { label: $label, icon: $icon, action: || Box::new($action) }
    };
}

/// The File menu's view commands.
pub(crate) const TAB_COMMANDS: [WindowCommand; 1] =
    [command!("Reopen closed view", crate::icons::WINDOW_RESTORE, shell::ReopenClosedTab)];

/// The View menu: the sidebar, a session's panes and the views summoned beside it
/// (`docs/decisions/layout.md`, "One session per view, splits on demand").
pub(crate) const VIEW_COMMANDS: [WindowCommand; 9] = [
    command!("Toggle sidebar", crate::icons::SPLIT_COLUMNS, shell::ToggleSidebar),
    command!("Split right", crate::icons::SPLIT_COLUMNS, shell::SplitRight),
    command!("Split down", crate::icons::SPLIT_COLUMNS, shell::SplitDown),
    command!("Zoom pane", crate::icons::WINDOW_MAXIMIZE, shell::ZoomPane),
    command!("Changes", crate::icons::FILE_DIFF, shell::ToggleChanges),
    command!("Files", crate::icons::FOLDER_TREE, shell::ToggleFiles),
    // Steps in time, as a trace reads; the eye is the inbox's (`layout.md`).
    command!("Inspector", crate::icons::SORT_BY_TIME, shell::ToggleInspector),
    command!("Browser", crate::icons::GLOBE, shell::OpenBrowser),
    command!("Remote window", crate::icons::MONITOR, shell::OpenRemoteWindow),
];

/// The Go menu's moves.
pub(crate) const GO_COMMANDS: [WindowCommand; 10] = [
    command!("Back", crate::icons::ARROW_LEFT, shell::NavigateBack),
    command!("Forward", crate::icons::ARROW_RIGHT, shell::NavigateForward),
    // The session picker and Go to Directory (`shell::picker`).
    command!("Go to session\u{2026}", crate::icons::MAGNIFER, shell::OpenPicker),
    command!("Go to directory\u{2026}", crate::icons::FOLDER, shell::GoToDirectory),
    command!("Next pane", crate::icons::ALT_ARROW_RIGHT, shell::NextPane),
    command!("Previous pane", crate::icons::ALT_ARROW_LEFT, shell::PreviousPane),
    command!("Next session", crate::icons::ALT_ARROW_DOWN, shell::NextSession),
    command!("Previous session", crate::icons::ALT_ARROW_UP, shell::PrevSession),
    command!("Next session that needs you", crate::icons::BELL, shell::NextNeedsYou),
    command!("Inbox", crate::icons::EYE, shell::Inbox),
];

/// Every command the palette offers from the menu bar's table.
pub(crate) fn window_commands() -> impl Iterator<Item = WindowCommand> {
    TAB_COMMANDS.into_iter().chain(VIEW_COMMANDS).chain(GO_COMMANDS)
}

/// A sentence-case name in Title Case: each word capitalised but the short joining
/// ones after the first.
pub(crate) fn title_case(label: &str) -> String {
    const MINOR: [&str; 9] = ["a", "an", "and", "in", "of", "on", "or", "the", "to"];
    label
        .split(' ')
        .enumerate()
        .map(|(ix, word)| {
            if ix > 0 && MINOR.contains(&word) {
                return word.to_owned();
            }
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// The Go menu's rows for the sidebar's first nine chats (⌘1…⌘9).
const JUMP_TITLES: [&str; crate::settings::JUMP_SLOTS] = [
    "Session 1",
    "Session 2",
    "Session 3",
    "Session 4",
    "Session 5",
    "Session 6",
    "Session 7",
    "Session 8",
    "Session 9",
];

/// The slopty menu bar. macOS renders this natively; mac-only entries are gated
/// at runtime (`cfg!`) so the whole module compiles and tests on every target.
pub fn app_menus() -> Vec<Menu> {
    let macos = cfg!(target_os = "macos");

    // macOS titles the first menu with the bundle/process name regardless of
    // what we pass, but gpui still wants a name.
    let mut app_items = vec![
        // The native AppKit about panel; no equivalent elsewhere yet.
        MenuItem::action("About Slopty", About).disabled(!macos),
        MenuItem::separator(),
        // It opens a window of its own, so its name ends in "…" (the macOS rule).
        MenuItem::action("Settings…", shell::OpenSettings),
        MenuItem::separator(),
    ];
    if macos {
        app_items.extend([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Slopty", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
        ]);
    }
    app_items.push(MenuItem::action("Quit Slopty", Quit));

    // File, View and Go list the window's commands; macOS writes each one's key from the
    // keymap beside it, so a rebind shows in the menu too.
    let mut menus = vec![
        Menu::new("Slopty").items(app_items),
        Menu::new("File").items(
            [
                MenuItem::action("New Session", shell::NewSession),
                MenuItem::action("New Project…", shell::AddSpacePalette),
            ]
            .into_iter()
            .chain(TAB_COMMANDS.iter().map(WindowCommand::menu_item))
            .chain([
                MenuItem::separator(),
                MenuItem::action("Close", CloseTab),
                MenuItem::action("Save", shell::SaveFile),
            ]),
        ),
        // Standard clipboard verbs tied to the composer's existing actions via
        // their native selectors (`OsAction` → cut:/copy:/paste:/selectAll:),
        // so the OS Edit menu routes through the responder chain to the focused
        // input — zed wires its editor actions identically
        // (crates/zed/src/zed/app_menus.rs, Edit/Selection menus).
        Menu::new("Edit").items([
            // Undo/Redo have no `OsAction` counterpart — they dispatch as plain
            // actions to the focused input, same as the composer keymap.
            MenuItem::action("Undo", composer::Undo),
            MenuItem::action("Redo", composer::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", composer::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", composer::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", composer::Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", composer::SelectAll, OsAction::SelectAll),
        ]),
    ];
    // Appearance lives under View on every platform, after the panes.
    menus.push(Menu::new("View").items(VIEW_COMMANDS.iter().map(WindowCommand::menu_item).chain(
        [
            MenuItem::separator(),
            MenuItem::action("Command Palette", shell::ToggleCommandPalette),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Appearance").items([
                MenuItem::action("System", AppearanceSystem),
                MenuItem::action("Light", AppearanceLight),
                MenuItem::action("Dark", AppearanceDark),
            ])),
        ],
    )));
    let [
        back,
        forward,
        sessions,
        directory,
        next_pane,
        previous_pane,
        next,
        previous,
        needs_you,
        inbox,
    ] = GO_COMMANDS.map(|command| command.menu_item());
    let mut go = vec![
        back,
        forward,
        MenuItem::separator(),
        sessions,
        directory,
        MenuItem::separator(),
        next_pane,
        previous_pane,
        MenuItem::separator(),
        next,
        previous,
        needs_you,
        inbox,
        MenuItem::separator(),
    ];
    go.extend(
        JUMP_TITLES
            .iter()
            .enumerate()
            .map(|(slot, title)| MenuItem::action(*title, shell::JumpSession(slot))),
    );
    menus.push(Menu::new("Go").items(go));
    if macos {
        // Standard Window menu; macOS appends the open-window list itself.
        menus.push(Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Close", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
        ]));
    }
    menus
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;

    use super::*;

    fn action_names(menu: &Menu) -> Vec<&'static str> {
        menu.items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(action.name()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn app_menu_ends_with_quit() {
        let menus = app_menus();
        assert_eq!(menus[0].name.as_ref(), "Slopty");
        let Some(MenuItem::Action { name, action, .. }) = menus[0].items.last() else {
            panic!("last app-menu item must be an action");
        };
        assert_eq!(name.as_ref(), "Quit Slopty");
        assert_eq!(action.name(), Quit.name());
    }

    #[test]
    fn app_menu_offers_settings() {
        let menus = app_menus();
        assert!(
            menus[0].items.iter().any(|item| matches!(
                item,
                MenuItem::Action { name, action, .. }
                    if name.as_ref() == "Settings…" && action.name() == shell::OpenSettings.name()
            )),
            "the application menu should expose Settings"
        );
    }

    #[test]
    fn about_enabled_only_on_macos() {
        let menus = app_menus();
        let Some(first @ MenuItem::Action { action, .. }) = menus[0].items.first() else {
            panic!("first app-menu item must be an action");
        };
        assert_eq!(action.name(), About.name());
        assert_eq!(first.is_disabled(), !cfg!(target_os = "macos"));
    }

    #[test]
    fn edit_menu_uses_composer_clipboard_os_actions() {
        let menus = app_menus();
        let edit = menus.iter().find(|m| m.name.as_ref() == "Edit").expect("Edit menu present");
        // `OsAction` has no `Debug` impl at the pinned rev, so compare
        // per-field.
        let expect = [
            (composer::Cut.name(), OsAction::Cut),
            (composer::Copy.name(), OsAction::Copy),
            (composer::Paste.name(), OsAction::Paste),
            (composer::SelectAll.name(), OsAction::SelectAll),
        ];
        let got: Vec<(&str, OsAction)> = edit
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, os_action: Some(os_action), .. } => {
                    Some((action.name(), *os_action))
                }
                _ => None,
            })
            .collect();
        assert_eq!(got.len(), expect.len());
        for ((got_name, got_os), (want_name, want_os)) in got.iter().zip(expect.iter()) {
            assert_eq!(got_name, want_name);
            assert!(got_os == want_os, "OsAction mismatch for {want_name}");
        }
    }

    #[test]
    fn view_menu_offers_all_three_appearance_modes() {
        let menus = app_menus();
        let view = menus.iter().find(|m| m.name.as_ref() == "View").expect("View menu present");
        let appearance = view
            .items
            .iter()
            .find_map(|item| match item {
                MenuItem::Submenu(menu) if menu.name.as_ref() == "Appearance" => Some(menu),
                _ => None,
            })
            .expect("Appearance under View");
        assert_eq!(
            action_names(appearance),
            vec![AppearanceSystem.name(), AppearanceLight.name(), AppearanceDark.name()]
        );
    }

    #[test]
    fn the_menu_bar_names_the_palette_commands_in_title_case() {
        assert_eq!(title_case("Reopen closed view"), "Reopen Closed View");
        assert_eq!(title_case("Go to directory\u{2026}"), "Go to Directory\u{2026}");
        let menus = app_menus();
        let view = menus.iter().find(|m| m.name.as_ref() == "View").expect("View menu");
        let names: Vec<&str> = view
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.as_ref()),
                _ => None,
            })
            .collect();
        for command in VIEW_COMMANDS {
            assert!(names.contains(&title_case(command.label).as_str()), "{}", command.label);
        }
        let go = menus.iter().find(|m| m.name.as_ref() == "Go").expect("Go menu");
        assert_eq!(
            action_names(go)[..2],
            [shell::NavigateBack.name(), shell::NavigateForward.name()]
        );
    }

    #[test]
    fn app_bindings_use_platform_settings_convention() {
        // `KeyBinding::new` panics on unparsable combos, so constructing the
        // table is itself the parse check.
        let find = |bindings: &[KeyBinding], name: &str| {
            bindings.iter().find(|binding| binding.action().name() == name).map(|binding| {
                binding.keystrokes().iter().map(|ks| ks.inner().clone()).collect::<Vec<_>>()
            })
        };
        let combo = |source: &'static str| vec![Keystroke::parse(source).unwrap()];
        let macos = app_key_bindings(Keyboard::Mac);
        assert_eq!(find(&macos, shell::OpenSettings.name()), Some(combo("cmd-,")));
        assert_eq!(find(&macos, Quit.name()), Some(combo("cmd-q")));
        // ⌘W closes the focused pane, ⇧⌘W the window.
        assert_eq!(find(&macos, CloseTab.name()), Some(combo("cmd-w")));
        assert_eq!(find(&macos, CloseWindow.name()), Some(combo("cmd-shift-w")));
        assert_eq!(find(&macos, Minimize.name()), Some(combo("cmd-m")));

        // An iPad's keyboard opens Settings with ⌘, and ⌘W closes its focused pane; the app and
        // its window are the system's.
        let ipad = app_key_bindings(Keyboard::Ios);
        assert_eq!(find(&ipad, shell::OpenSettings.name()), Some(combo("cmd-,")));
        assert_eq!(find(&ipad, CloseTab.name()), Some(combo("cmd-w")));
        for system in [Quit.name(), Hide.name(), HideOthers.name(), Minimize.name()] {
            assert_eq!(find(&ipad, system), None, "{system} is iPadOS's");
        }
        assert_eq!(find(&ipad, CloseWindow.name()), None, "a scene's window is the system's");

        let other = app_key_bindings(Keyboard::Other);
        assert_eq!(find(&other, shell::OpenSettings.name()), Some(combo("ctrl-,")));
        assert_eq!(find(&other, Quit.name()), None);
        assert_eq!(find(&other, CloseTab.name()), None);
    }
}

// Standard AppKit about panel. Icon and copyright still come from the bundle's
// Info.plist; name and version are passed explicitly so unbundled dev builds
// (`cargo run`) show "Slopty" and the crate version instead of the process name.
#[cfg(target_os = "macos")]
mod about_panel {
    use objc2::MainThreadMarker;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{
        NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion,
        NSAboutPanelOptionVersion, NSApplication,
    };
    use objc2_foundation::{NSDictionary, NSString};

    pub(super) fn show() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let name = NSString::from_str("Slopty");
        let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
        // Empty build version: CFBundleVersion equals the marketing version, and
        // AppKit would otherwise render "Version 0.2.61 (0.2.61)".
        let build = NSString::from_str("");
        let values: [&AnyObject; 3] = [&name, &version, &build];
        // SAFETY: AppKit's about-panel option keys are immutable string
        // constants that live as long as the process.
        let keys = unsafe {
            [
                NSAboutPanelOptionApplicationName,
                NSAboutPanelOptionApplicationVersion,
                NSAboutPanelOptionVersion,
            ]
        };
        let options = NSDictionary::from_slices(&keys, &values);
        // SAFETY: each of these options takes a string, and every value is an NSString.
        unsafe {
            NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanelWithOptions(&options);
        }
    }
}

// GPUI's on_app_quit runs after AppKit has committed to termination. Catch
// Dock / system Quit at the earlier delegate decision, just like our menu
// action, so failed saves can still cancel termination. The pinned GPUI
// delegate does not implement applicationShouldTerminate:.
#[cfg(target_os = "macos")]
mod native_quit {
    use std::cell::Cell;

    thread_local! {
        static ALLOWED: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn allow() {
        ALLOWED.set(true);
    }

    pub(super) use hook::init;

    mod hook {
        use std::cell::RefCell;

        // objc2; objc 0.2 is banned in the workspace (deny.toml).
        use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
        use objc2::{msg_send, sel};

        use super::super::{App, request_quit};
        use super::ALLOWED;

        thread_local! {
            static REQUEST: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
        }

        extern "C" fn should_terminate(_: &AnyObject, _: Sel, _: *mut AnyObject) -> usize {
            if ALLOWED.get() {
                return 1; // NSTerminateNow
            }
            REQUEST.with_borrow(|request| {
                if let Some(request) = request {
                    request();
                }
            });
            0 // NSTerminateCancel; re-request only after buffers are safe.
        }

        unsafe extern "C" {
            /// AppKit's shared application, nil until something starts it.
            static NSApp: *mut AnyObject;
        }

        /// Catch AppKit's quit decision in GPUI's application delegate.
        ///
        /// Tests run on GPUI's test platform, which never starts AppKit: with
        /// no shared application there is no delegate to extend, so they keep out.
        pub(in crate::app_menus) fn init(cx: &App) {
            // SAFETY: AppKit writes its global once, on the main thread, as the
            // shared application starts; this main-thread read races nothing.
            let ns_app = unsafe { NSApp };
            if ns_app.is_null() {
                return;
            }
            let executor = cx.foreground_executor().clone();
            let app = cx.to_async();
            REQUEST.with_borrow_mut(|request| {
                *request = Some(Box::new(move || {
                    let app = app.clone(); // gpui-fast updates through `&AsyncApp`
                    executor
                        .spawn(async move {
                            let () = app.update(request_quit);
                        })
                        .detach();
                }));
            });
            // AppKit invokes delegate methods on the main thread. Add only the
            // missing selector: never replace GPUI's existing quit observer.
            // SAFETY: `ns_app` is the live NSApplication, whose `delegate` takes no
            // arguments and returns an object or nil.
            let delegate: *mut AnyObject = unsafe { msg_send![ns_app, delegate] };
            assert!(!delegate.is_null(), "GPUI application delegate must exist");
            // SAFETY: the delegate is non-nil, an object the application keeps
            // alive for as long as it runs.
            let delegate = unsafe { &*delegate };
            let class = std::ptr::from_ref::<AnyClass>(delegate.class()).cast_mut();
            // SAFETY: `Imp` is the untyped C function pointer the runtime stores;
            // AppKit calls it back with the (self, _cmd, sender) -> NSUInteger
            // shape `should_terminate` has and "Q@:@" declares.
            let imp = unsafe {
                std::mem::transmute::<extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> usize, Imp>(
                    should_terminate,
                )
            };
            // SAFETY: `class` is the delegate's live class, and the selector, the
            // type encoding and `imp` agree on the method's signature.
            let added = unsafe {
                objc2::ffi::class_addMethod(
                    class,
                    sel!(applicationShouldTerminate:),
                    imp,
                    c"Q@:@".as_ptr(),
                )
            };
            assert!(added.as_bool(), "GPUI termination delegate contract changed");
        }
    }
}
