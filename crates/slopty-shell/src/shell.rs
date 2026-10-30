// Derived from zeron (MIT, github.com/zeronsh/zeron @ 77a3c0f), see THIRD_PARTY_NOTICES.md
//! The shell's window (`docs/decisions/layout.md`): one row of chrome, the session list beside
//! the session's terminal, and the views summoned beside the terminal ([`RightSurface`]: changes,
//! files, a file, the browser, a diff, the inspector, a remote screen, a page), plus the boot
//! splash and the connection gate.
//!
//! The sidebar drags between `settings::SIDEBAR_MIN` and `settings::SIDEBAR_MAX` wide
//! (`settings::SIDEBAR_DEFAULT` at first) and puts away to its rail; a view beside the terminal
//! is at least `settings::RIGHT_PANE_MIN` wide (`settings::RIGHT_PANE_DEFAULT` at first). The
//! widths and whether the sidebar is put away persist to `ui-settings.json`, debounced.
//!
//! Resize handles use gpui's drag-and-drop pattern (an `on_drag` with an empty
//! ghost view + `on_drag_move::<Marker>` on the root), the same idiom as Zed's
//! dock. Double-clicking a handle resets that pane to its default width.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, Empty, Entity, FocusHandle, Focusable as _,
    IntoElement, Keystroke, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseUpEvent, NavigationDirection, Pixels, Point, Render, SharedString, Subscription, Task,
    Window, WindowControlArea, actions, div, prelude::*, px,
};
use crate::motion::HoverFade as _;
#[cfg(test)] // the shell's tests set a workspace scope through `use super::*`
use slopty_view_proto::WorkspaceScope;

use slopty_view_rpc::methods;

use crate::changes::{Changes, ChangesEvent};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::files::{FileColumn, FilesCloseDisposition, FilesEvent, FilesSurface};
use crate::icons::{self, icon};
use crate::loaders;
use crate::motion::{self, AnimationExt as _, MotionSpec, PANEL_RESIZE, RESIZE, SPLASH_OUT};
use crate::popover::{self, Loadable};
use crate::settings::appearance::{AppearancePage, AppearanceSettingsEvent};
use crate::settings::archived::ArchivedPage;
use crate::settings::devices::DevicesPage;
use crate::settings::files::{FilesSettingsEvent, FilesSettingsPage};
use crate::settings::harnesses::HarnessesPage;
use crate::settings::notifications::{NotificationsEvent, NotificationsPage};
use crate::settings::shortcuts::{ShortcutsEvent, ShortcutsPage};
use crate::settings::{
    self, CHAT_PANEL_MIN, JUMP_SLOTS, KeymapConfig, SIDEBAR_DEFAULT, SIDEBAR_MAX,
    SIDEBAR_MIN, SavePolicy, ShortcutId, UiSettings, jump_hints_visible,
    platform_combo, workspace_profile_key,
};
use crate::state::{
    AppState, ConnectionStatus, EngineBootConfig, GatePhase, Indicator,
    format_time_ago,
};
use crate::terminal::panel::TerminalPanel;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::theme::Theme;
use crate::transcript::{self, Transcript};

mod actions_ui;
#[cfg(test)]
mod chat_rename_tests;
mod command_palette;
mod file_mutations;
mod files_panel;
mod navigation_focus;
#[cfg(test)]
mod navigation_tests;
mod panel_layout;
use panel_layout::HorizontalPanelFit;
mod panes; // a session's view as a tree of panes
mod pane_area; // the panes drawn, with their dividers
mod pane_frame; // the panes on the shell's side: splits, summoned views, keys and headers
mod views; // the views summoned beside a session: what each shows, names, focuses and lets go
#[cfg(test)]
mod panel_interaction_tests;
#[cfg(test)]
mod retention_tests; // the whole shell under retention; the tests' shell window and tree
#[cfg(test)]
mod fuzz_tests; // seeded action sequences over the whole shell, shrunk on a failure
pub(crate) mod project_icon;
mod session_hover_card;
mod inbox; // the inbox behind the titlebar's eye
pub(crate) mod marks; // the five states, their mark and hues, on every surface
mod sidebar; // the sidebar column, its list, rows and the server foot
mod sidebar_status; // the list's groups, its rows' hints and the hover card's facts
mod search_warm;
pub(crate) mod spaces;
mod thread_search;
mod session_search; // finding sessions by title and by what was said in them
mod tabs;
mod live_surfaces; // remote window and desktop views
mod kept_tabs; // the views beside each session kept across launches
mod page_panes; // a page an agent showed, as a view (M)
mod compact;
pub(crate) use compact::touch_screen;
#[cfg(test)]
pub(crate) use compact::force_touch; // tests lay a view out for a finger
mod chrome_keys; // find, history, pane and view keys, and undoing an archive
// The window's own commands, for the menu bar (`app_menus`).
pub use chrome_keys::{
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, Inbox, NavigateBack, NavigateForward,
    NextNeedsYou, NextPane, OpenBrowser, OpenIn, OpenRemoteWindow, PreviousPane, ReopenClosedTab,
    SplitAuto, SplitDown, SplitRight, ToggleInspector, ZoomPane,
};
mod closed_tabs; // the views closed last, opened again
mod open_in; // a chat's folder in the person's editor
mod terminal_runs; // a transcript's Run in the chat's own shell terminal
mod new_session; // New session…: an agent's TUI, or a terminal, in a project at once
mod past_sessions; // Claude Code's past sessions, found in the picker and taken up again
mod picker; // the session picker (⌘K) and Go to Directory (⇧⌘G)
pub use picker::{GoToDirectory, OpenPicker}; // for the menu bar's Go menu
#[cfg(test)]
mod chrome_tests; // the chrome as a person and assistive tech meet it
pub mod agent_ui; // an agent drives this window (`slopty ui`)

use spaces::{AddSpaceFlow, RenameSpaceDialog};

actions!(
    shell,
    [
        SaveFile,
        ToggleSidebar,
        ToggleChanges,
        ToggleFiles,
        AddSpacePalette,
        ToggleCommandPalette,
        NewSession,
        OpenSettings,
        NextSession,
        PrevSession,
        ArchiveSession
    ]
);

/// Restore a default focus only after an in-flight handoff has had a frame to
/// claim the window. A synchronous focus-lost fallback can otherwise steal
/// focus from controls that are mounting in response to the same input event.
pub(crate) fn restore_focus_if_empty_on_next_frame<T: 'static>(
    focus: FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    window.on_next_frame(move |window, cx| {
        if window.focused(cx).is_none() {
            window.focus(&focus, cx);
        }
    });
    cx.notify();
}

/// Run [`restore_mounted_focus`] after this frame, judged against it only while the focus is
/// where the frame found it. A frame drawn while an input is dispatched (a key draws a dirty
/// window first) runs its deferred work after the input's handlers; a handler that focused a
/// field this frame never built (⌘F's find) leaves the judgement to the next frame, which
/// builds it.
pub(crate) fn defer_mounted_focus_restore(
    root: FocusHandle,
    preferred: FocusHandle,
    unfocused: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let focused = window.focused(cx);
    window.defer(cx, move |window, cx| {
        if window.focused(cx) == focused {
            restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
        }
    });
}

/// Check the completed dispatch tree, not just the lifetime of the focused
/// handle: a hidden editor can stay alive after its element has unmounted.
pub(crate) fn restore_mounted_focus(
    root: &FocusHandle,
    preferred: &FocusHandle,
    unfocused: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let preferred_mounted = root.contains(preferred, window);
    if !root.contains_focused(window, cx) || (root.is_focused(window) && preferred_mounted) {
        // Explicit blur keeps shortcuts active without returning the caret to
        // an input. The root remains the temporary fallback for stale handles.
        let target = if window.focused(cx).is_none() {
            unfocused
        } else if preferred_mounted {
            preferred
        } else {
            root
        };
        window.focus(target, cx);
    }
}

/// Tab groups order their descendants but do not trap focus. The trailing
/// non-tab-stop handle gives reverse traversal a stable end boundary even
/// when a page adds or removes controls.
fn move_settings_focus(
    start: &FocusHandle,
    end: &FocusHandle,
    reverse: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if reverse {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
    if !start.contains_focused(window, cx) {
        if reverse {
            window.focus(end, cx);
            window.focus_prev(cx);
        } else {
            window.focus(start, cx);
            window.focus_next(cx);
        }
    }
}

/// Whether the chat menu's Copy submenu is open beside it.
#[derive(Clone, Copy, PartialEq)]
enum ChatMenuPage {
    Root,
    Copy,
}

/// Hover-intent keys for the chat menu's rows; only Copy owns a submenu.
const CHAT_MENU_ROW_RENAME: u8 = 0;
const CHAT_MENU_ROW_ARCHIVE: u8 = 2;
const CHAT_MENU_ROW_COPY: u8 = 3;
const CHAT_MENU_ROW_OPEN_IN: u8 = 4;

#[derive(Clone)]
struct ChatMenuState {
    chat_id: String,
    position: Point<Pixels>,
    page: ChatMenuPage,
}

impl ChatMenuState {
    /// The menu of chat `chat_id`'s row, opened at `position` on its first page.
    pub(super) fn for_row(chat_id: impl Into<String>, position: Point<Pixels>) -> Self {
        Self { chat_id: chat_id.into(), position, page: ChatMenuPage::Root }
    }
}

pub(super) use crate::motion::DisclosureMotion as SidebarDisclosureMotion;

/// Vertical pane resize hitboxes yield the global titlebar. Keeping this in
/// the shared constructor makes left/right seams mirror each other and avoids
/// relying on paint order when chrome crosses an animated pane boundary.
const PANE_RESIZE_HITBOX_HALF_WIDTH: f32 = 10.0;
const PANE_RESIZE_HITBOX_TOP: f32 = Theme::TITLEBAR_HEIGHT;
/// Corner radius of the floating Linux CSD window (macOS gets its native
/// curve from the platform; maximized/tiled Linux windows go square).
/// Chrome layers that paint full-bleed at a window edge round their own
/// backgrounds with it — see [`Shell::window_corner_radius`].
pub(crate) const LINUX_WINDOW_CORNER_RADIUS: f32 = 10.0;

/// The chat's working directory as the host device spells it — the folder the
/// harness runs in (a worktree chat's worktree). Projectless `~` chats have
/// none. Deliberately not `source_context.repo_root`: that is canonicalized
/// (symlinks resolved, `\\?\` verbatim prefix on Windows hosts).
fn chat_copy_path(chat: &slopty_view_proto::Chat) -> Option<&str> {
    chat.cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| is_host_absolute_path(cwd))
}

/// Absolute on the HOST, whatever the viewer's OS: a remote engine may hand a
/// POSIX path to a Windows viewport (no drive, so `Path::is_absolute` says
/// no) or a drive path to a POSIX one.
fn is_host_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

/// How long a started session's main column says "Starting…" while no terminal shows.
const STARTS_WITHIN: std::time::Duration = std::time::Duration::from_secs(15);

/// Open the session at `slot` (zero-based) of the sidebar's active list. One
/// action carrying the slot, rather than nine near-identical action types.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = shell, no_json)]
pub struct JumpSession(pub usize);

// ---------------------------------------------------------------------------
// Traffic-light-aware titlebar layout
// ---------------------------------------------------------------------------

/// Where the top-left window-control cluster starts, in px from the window's
/// left edge: 88, or 12 in fullscreen. The
/// frameless hiddenInset chrome puts the macOS traffic lights at {14,15};
/// fullscreen hides them and the cluster reclaims the inset.
pub fn titlebar_cluster_start(fullscreen: bool) -> f32 {
    if fullscreen { 12.0 } else { 88.0 }
}

/// Within-group rhythm for the row's trailing actions.
pub const TITLEBAR_CONTROL_GAP: f32 = 2.0;
/// Structural separation between titlebar groups: the sidebar's, the session's identity and
/// the trailing actions.
pub const TITLEBAR_GROUP_GAP: f32 = Theme::SPACE_SM;
/// Breathing room between the sidebar toggle and the session's identity.
pub const TITLEBAR_IDENTITY_GAP: f32 = Theme::SPACE_MD;
/// A 28px action centered in the 38px titlebar with its 2px downward optical
/// shift lands 6px from the top; use the same inset at the trailing edge.
pub const TITLEBAR_ACTION_EDGE_INSET: f32 = 6.0;
/// Width of the persistent top-left button cluster itself: the 24px sidebar toggle.
pub const CLUSTER_BUTTONS_WIDTH: f32 = 24.0;
/// Horizontal inset owned by the titlebar control row itself. Keep this value
/// paired with [`Self::titlebar_spacer`]: using a different number for the
/// spacer shifts every control while leaving the declared cluster geometry
/// unchanged.
const TITLEBAR_CLUSTER_PAD: f32 = 10.0;

/// Width of a row of `count` Linux caption buttons, drawn at the cluster's
/// own 24px-button / 2px-gap rhythm.
pub fn caption_buttons_width(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    count as f32 * 24.0 + (count as f32 - 1.0) * 2.0
}

/// Where the cluster's first button starts, from the window's left edge.
/// `linux_left_captions` is the number of caption buttons Slopty draws at the
/// top-left on Linux (GNOME `close:…` layouts) — the app cluster follows them
/// at the shared 2px rhythm.
pub fn cluster_buttons_start(is_macos: bool, fullscreen: bool, linux_left_captions: usize) -> f32 {
    if is_macos {
        titlebar_cluster_start(fullscreen)
    } else if linux_left_captions > 0 {
        10.0 + caption_buttons_width(linux_left_captions) + 2.0
    } else {
        10.0
    }
}

/// (Re-)apply the whole app keymap: clears every binding, restores the text fields'
/// map, then binds the customizable shortcuts from `keymap`. Invalid persisted combos fall
/// back to that shortcut's default.
/// Everything is written in US keys and bound where the active keyboard layout
/// types them (see [`localize_key_bindings`]); re-run on a layout switch.
pub fn apply_keymap(cx: &mut App, keymap: &KeymapConfig) {
    fn valid_or_default(combo: &str, fallback: &str) -> String {
        let candidate = platform_combo(combo);
        if Keystroke::parse_with_os(&candidate, crate::keys::OS).is_ok() {
            candidate
        } else {
            tracing::warn!(%combo, "unparseable shortcut combo; using default");
            platform_combo(fallback)
        }
    }
    let equivalents = crate::settings::key_equivalents(cx);
    cx.clear_key_bindings();
    // `clear_key_bindings` also removes the contextual editing actions that
    // gpui-base installed at startup. Reinitialize the component layer before
    // rebuilding Slopty's bindings so the file editor keymap remains active.
    gpui_base::init(cx);
    crate::composer::init(cx);
    // Fixed app-level shortcuts (Settings on every platform; ⌘Q quit, ⌘W
    // close, ⌘M minimize, ⌘H hide on macOS) — these back the native menu
    // key equivalents and must survive keymap re-application.
    crate::app_menus::bind_keys(cx);
    cx.bind_keys([
        crate::keys::binding(
            &valid_or_default(&keymap.save_file, "mod-s"),
            SaveFile,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(&keymap.toggle_sidebar, "mod-b"),
            ToggleSidebar,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(&keymap.toggle_changes, "mod-r"),
            ToggleChanges,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(&keymap.toggle_files, "mod-e"),
            ToggleFiles,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(&keymap.new_session, "mod-n"),
            NewSession,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(
                &keymap.next_session,
                crate::settings::ShortcutId::NextSession.default_combo(),
            ),
            NextSession,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(
                &keymap.prev_session,
                crate::settings::ShortcutId::PrevSession.default_combo(),
            ),
            PrevSession,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(&keymap.archive_session, "mod-shift-a"),
            ArchiveSession,
            None,
            cx,
        ),
        crate::keys::binding(
            &valid_or_default(&keymap.new_project, ShortcutId::NewProject.default_combo()),
            AddSpacePalette,
            None,
            cx,
        ),
        // Fixed: ⇧⌘P summons the command palette (Ghostty's and VS Code's key), and pressing it
        // again dismisses it. ⌘K is the session picker in every session (`picker`).
        crate::keys::binding(&platform_combo("mod-shift-p"), ToggleCommandPalette, None, cx),
        crate::keys::binding(&platform_combo("mod-k"), picker::OpenPicker, None, cx),
        // ⇧⌘G is Go to Directory, bound before `chrome_keys`' Find Previous on the same key, so
        // that one goes first and passes the key on unless a find is open where the keyboard is.
        // A terminal's own Find Previous goes before both and passes it on with its card shut.
        crate::keys::binding(&platform_combo("mod-shift-g"), picker::GoToDirectory, None, cx),
    ]);
    crate::browser::bind_keys(cx, keymap);
    // The files surface's and the diff's fixed keys, in their own contexts.
    crate::files::bind_keys(cx);
    crate::changes::bind_keys(cx);
    chrome_keys::bind_keys(cx, keymap); // find, history, tab, picker and undo keys
    // ⌘1..⌘9 open the sidebar's first nine rows. A slot left unbound (an empty
    // combo in a hand-edited file) binds nothing rather than falling back —
    // the user cleared it on purpose.
    let jumps: Vec<_> = (0..JUMP_SLOTS).filter_map(|slot| { // built before binding
        let id = ShortcutId::JumpSession(slot);
        let combo = keymap.get(id);
        if combo.is_empty() {
            return None;
        }
        Some(crate::keys::binding(
            &valid_or_default(combo, id.default_combo()),
            JumpSession(slot),
            None,
            cx,
        ))
    }).collect();
    cx.bind_keys(jumps);
    // Slopty's views' own keys, last, so they hold in their contexts.
    crate::live::bind_keys(cx);
    localize_key_bindings(cx, &equivalents);
    crate::settings::set_bound_key_equivalents(equivalents);
}

/// Move every binding to the key the active layout types for it. Combos are
/// written in US keys, like the macOS menus they mirror; Apple relocates
/// those per layout (AZERTY ⌘1 is ⌘&, ⌘/ is ⌘:, ⌘[ is ⌘^) and gpui ships
/// the same table, but `KeyBinding::new` never applies it — to Slopty's
/// bindings or gpui-base's editor map. Order is kept: gpui breaks ties
/// between equally deep matches by binding order.
fn localize_key_bindings(cx: &mut App, equivalents: &crate::settings::KeyEquivalents) {
    if equivalents.is_empty() {
        return;
    }
    let keymap = cx.key_bindings();
    let bindings: Vec<gpui::KeyBinding> = keymap
        .borrow()
        .bindings()
        .map(|binding| localize_binding(binding, equivalents, cx))
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

// gpui-fast parses and spells a keystroke for the OS the app runs on.
fn localize_binding(
    binding: &gpui::KeyBinding,
    equivalents: &crate::settings::KeyEquivalents,
    cx: &App,
) -> gpui::KeyBinding {
    let mut moved = false;
    let source = binding
        .keystrokes()
        .iter()
        .map(|keystroke| {
            let keystroke = keystroke.inner();
            let key = crate::settings::localize_key(&keystroke.key, equivalents);
            moved |= key != keystroke.key.as_str();
            gpui::Keystroke {
                key: key.into_owned(),
                ..keystroke.clone()
            }
            .unparse(cx.platform_info().os)
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !moved {
        return binding.clone();
    }
    match gpui::KeyBinding::load(
        &source,
        binding.action().boxed_clone(),
        binding.predicate(),
        false,
        binding.action_input(),
        cx,
    ) {
        Ok(mut localized) => {
            if let Some(meta) = binding.meta() {
                localized.set_meta(meta);
            }
            localized
        }
        Err(error) => {
            tracing::warn!(%error, %source, "unbindable localized shortcut; keeping US key");
            binding.clone()
        }
    }
}

/// The settings sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
    Devices,
    /// Which harnesses the composer offers (enable/disable toggles) —
    /// labeled "Providers".
    Harnesses,
    #[default]
    Appearance,
    Files,
    Notifications,
    Shortcuts,
    Archived,
}

impl SettingsSection {
    /// Sections shown in Settings.
    pub const ALL: [SettingsSection; 7] = [
        SettingsSection::Appearance,
        SettingsSection::Notifications,
        SettingsSection::Shortcuts,
        SettingsSection::Harnesses,
        SettingsSection::Devices,
        SettingsSection::Files,
        SettingsSection::Archived,
    ];

    /// Stable name shared by `ui-settings.json` and `SLOPTY_OPEN_ROUTE`.
    fn slug(self) -> &'static str {
        match self {
            SettingsSection::Devices => "devices",
            SettingsSection::Harnesses => "providers",
            SettingsSection::Appearance => "appearance",
            SettingsSection::Files => "files",
            SettingsSection::Notifications => "notifications",
            SettingsSection::Shortcuts => "shortcuts",
            SettingsSection::Archived => "archived",
        }
    }

    /// Inverse of [`Self::slug`], plus the pages' former names.
    fn from_slug(slug: &str) -> Option<Self> {
        Some(match slug {
            "devices" => SettingsSection::Devices,
            "providers" | "harnesses" => SettingsSection::Harnesses,
            "appearance" => SettingsSection::Appearance,
            "files" => SettingsSection::Files,
            "notifications" => SettingsSection::Notifications,
            "shortcuts" => SettingsSection::Shortcuts,
            "archived" => SettingsSection::Archived,
            _ => return None,
        })
    }

    /// Nav groups are separated by spacing alone: preferences, providers and
    /// devices, then workspace data.
    fn starts_nav_group(self) -> bool {
        matches!(self, Self::Harnesses | Self::Files)
    }

    /// The section's label, in the settings sidebar and the header alike.
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::Devices => "Devices",
            SettingsSection::Harnesses => "Providers",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Files => "Files",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Shortcuts => "Shortcuts",
            SettingsSection::Archived => "Archived sessions",
        }
    }
}

/// Persisted as [`crate::settings::UiSettings::settings_section`].
impl serde::Serialize for SettingsSection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.slug())
    }
}

/// Lenient: an unknown or malformed value reads as Appearance instead of
/// failing — and so defaulting — the whole settings file.
impl<'de> serde::Deserialize<'de> for SettingsSection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value.as_str().and_then(Self::from_slug).unwrap_or_default())
    }
}

/// The section a `SLOPTY_OPEN_ROUTE` value opens: bare `settings` reopens the
/// remembered section, `settings/<slug>` names one (and so becomes the
/// remembered one). `None` for anything else, including unknown slugs.
fn settings_open_route(route: &str, remembered: SettingsSection) -> Option<SettingsSection> {
    if route == "settings" {
        return Some(remembered);
    }
    route
        .strip_prefix("settings/")
        .and_then(SettingsSection::from_slug)
}

/// What the main outlet shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Chat,
    Settings(SettingsSection),
}

/// A view summoned beside a session's terminal as a pane of its own (`docs/decisions/layout.md`,
/// "No permanent right column"): the session's files, a file of them, its changes, a web page, its
/// inspector, a remote window or desktop, or a page an agent showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RightSurface {
    /// The session's files, the explorer of its folder.
    Files,
    File(u64),
    Browser(u64),
    Diff(u64),
    // The session's inspector, one per chat, following it (`crate::inspector`).
    Inspector,
    Screen(u64), // a remote window or desktop (`live_surfaces`)
    Page(u64), // a page an agent showed (`page_panes`)
}

// What a remote window tab's chip wears: the window glyph for a window (its choice's id
// is `window:<n>`), the monitor for a desktop or before anything is chosen.
fn screen_chip_glyph(chosen: Option<&str>) -> &'static str {
    match chosen {
        Some(id) if id.starts_with("window:") => icons::WINDOW_RESTORE,
        _ => icons::MONITOR,
    }
}

fn workspace_file_title(path: &str) -> SharedString {
    path.rsplit('/').next().unwrap_or(path).to_string().into()
}

/// One route-history entry: a route the person visited, kept browser-style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEntry {
    /// A chat route; the id of the selected chat ("" = the new-chat canvas).
    Chat(String),
    Settings(SettingsSection),
}

/// Browser-style navigation history for the titlebar back/forward buttons:
/// every route change pushes an entry;
/// Back/Forward walk the stack without changing it; pushing while behind the
/// tip truncates the entries ahead (a new branch, exactly like a browser).
#[derive(Debug)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
}

impl NavHistory {
    pub fn new(initial: NavEntry) -> Self {
        Self {
            entries: vec![initial],
            index: 0,
        }
    }

    pub fn current(&self) -> &NavEntry {
        &self.entries[self.index]
    }

    /// Record a route change. Re-navigating to the current route is a no-op
    /// (selecting the already-selected chat never happened as a navigation);
    /// otherwise any forward branch is truncated and the entry appended.
    pub fn push(&mut self, entry: NavEntry) {
        if *self.current() == entry {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(entry);
        self.index += 1;
    }

    /// Swap the current entry in place without growing the stack, so a redirect (the boot
    /// canvas into the last-used chat) leaves no dead Back target behind.
    pub fn replace(&mut self, entry: NavEntry) {
        self.entries[self.index] = entry;
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    /// The entry Back would land on.
    pub fn previous(&self) -> Option<&NavEntry> {
        self.index.checked_sub(1).map(|index| &self.entries[index])
    }

    /// Memory history keeps every entry, so "behind the last entry" is exactly
    /// "can go forward".
    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<NavEntry> {
        if !self.can_back() {
            return None;
        }
        self.index -= 1;
        Some(self.current().clone())
    }

    pub fn forward(&mut self) -> Option<NavEntry> {
        if !self.can_forward() {
            return None;
        }
        self.index += 1;
        Some(self.current().clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The transcript's fade at the bottom stack: `(inset, band)` for a stack `stack_h` tall
/// over the transcript whose composer footer reaches `floor` under the pill. Opaque from the
/// pill's top (the status strip above it is empty air), zero by the pill's bottom, and nothing
/// under the footer.
fn transcript_floor_fade(stack_h: f32, floor: f32) -> (f32, f32) {
    let floor = floor.clamp(0.0, (stack_h - Theme::STATUS_STRIP_HEIGHT).max(0.0));
    (floor, (stack_h - Theme::STATUS_STRIP_HEIGHT - floor).max(1.0))
}

/// Flex gap between sidebar list items.
const SIDEBAR_LIST_GAP: f32 = 2.0;
/// Fixed vertical slot occupied by one active sidebar card.
#[cfg(test)]
const SIDEBAR_SESSION_SLOT: f32 = 61.0 + SIDEBAR_LIST_GAP;
const SIDEBAR_DRAG_SCROLL_BAND: f32 = 48.0;
const SIDEBAR_DRAG_SCROLL_MAX: f32 = 12.0;
const SIDEBAR_DRAG_SCROLL_FRAME_MS: u64 = 16;

/// The sidebar footer's profile and settings buttons share one hit target.
const SIDEBAR_FOOTER_BUTTON_SIZE: f32 = 28.0;

/// The window's cursor while a drag runs, wherever the pointer is (Zed's
/// `pane_group.rs`): a resize that outruns its handle keeps its cursor.
fn drag_cursor(style: gpui::CursorStyle) -> impl IntoElement {
    gpui::canvas(|_, _, _| {}, move |_, _, window, _| window.set_window_cursor_style(style))
        .absolute()
        .size_0()
}

/// Ramp height of the sidebar's scroll-edge fade (the gpui
/// [`gpui::EdgeFade`] scope — per-primitive, so text fades per glyph).
const SIDEBAR_GLASS_FADE_BAND: f32 = 24.0;


/// Drag marker for the sidebar resize handle.
struct SidebarResize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneResizeKind {
    Sidebar,
}

/// Resolve one pointer sample while keeping the persisted width legal. The
/// edge is latched by the caller, so a held pointer produces one nudge rather
/// than restarting the animation for every drag event.
fn sidebar_drag_sample(
    pointer_x: f32,
    latched_edge: Option<motion::ResizeEdge>,
    reduced_motion: bool,
) -> motion::ResizeDragSample {
    motion::resize_drag_sample(
        pointer_x,
        SIDEBAR_MIN,
        SIDEBAR_MAX,
        latched_edge,
        reduced_motion,
    )
}

type SidebarKeyedRow = (String, f32, AnyElement);

struct SidebarSessionRows {
    rows: Vec<SidebarKeyedRow>,
}

// A tab's tooltip is the house chip (`settings::widgets::text_tooltip`).
/// Invisible drag ghost — resize drags render nothing at the cursor.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A oneshot width tween, driven MANUALLY from render via
/// [`Shell::eval_tween`] — never through a `with_animation` wrapper. gpui keys
/// an animation element's start time by its full global element-id path, so a
/// wrapper that mounts/remounts (route swap, or an ancestor animation keyed by
/// a fresh epoch) silently REPLAYS the tween from t=0. Manual evaluation keeps
/// the element tree's shape constant: a finished or stale tween is exactly the
/// steady state, no matter how the tree around it remounts (round-6 §1–3).
#[derive(Debug, Clone, Copy)]
struct WidthTween {
    from: f32,
    to: f32,
    started: std::time::Instant,
    duration: Duration,
    curve: motion::CubicBezier,
}

impl WidthTween {
    fn new(from: f32, to: f32, now: std::time::Instant) -> Self {
        Self {
            from,
            to,
            started: now,
            duration: RESIZE.total().mul_f32(motion::speed_scale()),
            curve: RESIZE.curve,
        }
    }

    /// A panel column moving from `from` to `to` over [`PANEL_RESIZE`]: one 240 ms
    /// whatever the distance, as opencode's panel; the quint ease-out covers a wide move early
    /// and lands soft, so it needs no stretching by distance (item 12).
    fn panel(from: f32, to: f32, now: std::time::Instant) -> Self {
        Self {
            duration: PANEL_RESIZE.total().mul_f32(motion::speed_scale()),
            curve: PANEL_RESIZE.curve,
            ..Self::new(from, to, now)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplashPhase {
    Visible,
    FadingOut,
    Gone,
}

/// How long past a revealing disclosure's motion the sidebar keeps scrolling
/// a renamed row into view.
const CHAT_RENAME_REVEAL_GRACE: std::time::Duration = std::time::Duration::from_millis(150);

/// A chat title edited in place on its row (double-click, or Rename in the
/// chat menu). Enter or blur commits, Escape cancels.
struct ChatRename {
    chat_id: String,
    input: Entity<ComposerInput>,
    /// Focus the input on its first paint (opened without window access).
    focus_pending: bool,
    /// Until when the sidebar scrolls to keep the row in view — long
    /// enough to follow a disclosure that opened to reveal it.
    reveal_until: Option<std::time::Instant>,
    _events: Subscription,
    /// Commit-on-blur, armed once the input has taken focus.
    _blur: Option<Subscription>,
}


/// When the shell next has render-only clock pixels to repaint, as of `now`:
/// every second while the selected chat is Working (the trailer's elapsed
/// timer and its 7s flavour word), else the next wall-clock minute for
/// relative "5m"/"2h" labels. State transitions (staleness, device presence,
/// send grace) arrive as AppState notifications instead — see
/// [`AppState::watch_clock_transitions`]. Nothing else ticks: AwaitingInput
/// and Errored show no clock, and the connection pill has no countdown (its
/// spinner animates itself).
fn shell_clock_wake(state: &AppState, now: DateTime<Utc>) -> DateTime<Utc> {
    let working = state
        .selected_chat
        .as_deref()
        .is_some_and(|id| state.indicator_for(id, now) == Indicator::Working);
    if working {
        return now + chrono::TimeDelta::seconds(1);
    }
    DateTime::from_timestamp((now.timestamp().div_euclid(60) + 1) * 60, 0)
        .unwrap_or(now + chrono::TimeDelta::minutes(1))
}

/// A one-shot redraw timer, re-armed on every render for the view's next
/// clock-driven change. Idle views sleep until that deadline instead of
/// polling.
#[derive(Default)]
struct ClockRedraw {
    wake_at: Option<DateTime<Utc>>,
    task: Option<Task<()>>,
}

impl ClockRedraw {
    /// Notify the view at `wake` unless a redraw is already due no later.
    /// `slot` finds this timer on the view so the firing task can clear it.
    fn arm<V: 'static>(
        &mut self,
        wake: DateTime<Utc>,
        now: DateTime<Utc>,
        slot: fn(&mut V) -> &mut ClockRedraw,
        cx: &mut Context<V>,
    ) {
        if self.task.is_some() && self.wake_at.is_some_and(|at| at <= wake) {
            return;
        }
        let delay = (wake - now).to_std().unwrap_or_default();
        self.wake_at = Some(wake);
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |view, cx| {
                let clock = slot(view);
                clock.wake_at = None;
                clock.task = None;
                cx.notify();
            })
            .ok();
        }));
    }
}

// Escape over a turn under way stops it on its second press (Amp); the first arms it and
// the working line says so (`composer::escape_stops`). Anything that takes Escape first (a menu,
// the find bar) still does.




/// Sidebar render identity lets transcript/caret frames reuse its GPUI scene.
/// State and event handlers stay on Shell. Explicit Shell notifications still
/// invalidate the sidebar, including selection, menus, theme and navigation.
struct SidebarPane {
    shell: gpui::WeakEntity<Shell>,
    _observation: Subscription,
}

impl Render for SidebarPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::transcript::record_view_frame("sidebar");
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let inner = shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            shell.render_chat_sidebar(&theme, cx)
        });
        div().size_full().child(inner).into_any_element()
    }
}


#[derive(Debug, Clone)]
enum PendingExit {
    CloseWindow,
    Quit,
}

pub struct Shell {
    state: Entity<AppState>,
    sidebar_pane: Entity<SidebarPane>,
    transcript: Entity<Transcript>,
    // The main column's session terminal and the right pane's inspector of it.
    session_terminal: Entity<TerminalPanel>,
    /// The appearance the window was last given for the ground it shows
    /// (`docs/decisions/layout.md`, "The window wears its ground's appearance").
    window_ground: crate::window_appearance::WindowGround,
    /// The session a note's tap last landed on, and when: its terminal wears one accent
    /// border fade ([`landing_opacity`]).
    landed: Option<(String, std::time::Instant)>,
    inspector: Entity<crate::inspector::Inspector>,
    // The selected session and the terminal it was last seen in, which the selection
    // follows to the session that terminal is next (`follow_session_terminal`).
    followed_terminal: Option<(String, Option<String>)>,
    // The New session… picker, while it is up.
    new_session: Option<new_session::NewSessionPicker>,
    // A session just started or resumed, until its terminal shows, and since when.
    starting: Option<(String, std::time::Instant)>,
    /// The redraw that ends "Starting…" once its time is up.
    _starting_redraw: Option<Task<()>>,
    /// The sidebar's archived accordion (t3code Sidebar): OPEN by default
    /// (user request), session-transient. `archived_shown` pages the
    /// expanded list ("Show more" reveals another page).
    pub(super) archived_open: bool,
    pub(super) archived_shown: usize,
    // How many rows a paged sidebar group shows before "Show more", by its collapse key
    // (`sidebar_status`); a group not named shows its first page.
    pub(super) sidebar_group_shown: std::collections::HashMap<String, usize>,
    /// Ephemeral shut sidebar groups, by collapse key (`sidebar_status`).
    pub(super) sidebar_collapsed_groups: std::collections::HashSet<String>,
    /// In-flight disclosure tweens of the sidebar's groups and Archived.
    pub(super) sidebar_disclosure_motion:
        std::collections::HashMap<String, SidebarDisclosureMotion>,
    /// Disclosures a reveal (an inline rename of a hidden row) just opened;
    /// each starts its motion on the render that knows its height.
    pub(super) sidebar_reveal_motions: std::collections::HashSet<String>,
    /// The jump-hint overlay: true while the held modifiers exactly match a
    /// jump shortcut, which swaps the first nine rows' time-ago for their
    /// key-cap chip (t3code's `showJumpHints`). Frame-transient — window
    /// deactivation clears it, so a chip cannot stick after an app switch
    /// swallows the key-up.
    pub(super) jump_hints: bool,
    /// Each session view's panes, by its panel key (`panes.rs`).
    panes: std::collections::HashMap<String, panes::Panes>,
    /// The shells opened beside a session (⌘D), by their number.
    pane_shells: std::collections::HashMap<u64, Entity<TerminalPanel>>,
    pane_seq: u64,
    /// Where the panes were laid out last, for walking focus by direction.
    pane_area_at: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<Pixels>>>>,
    /// The grid the terminal panes snap to, as last drawn.
    pane_grid: Option<panes::Grid>,
    /// The focus each view's pane holds around what it shows (`views::view_handle`).
    view_handles: std::collections::HashMap<RightSurface, FocusHandle>,
    /// The focused pane changed (a switch, a summon, a close) and takes the keyboard on the next
    /// frame, once it is laid out.
    pane_keyboard_pending: bool,
    /// The views each session had beside it when the app last ran, until it is shown
    /// again (`kept_tabs`).
    unrestored_tabs: std::collections::BTreeMap<String, crate::settings::SavedTabs>,
    // The views closed last, newest last, each with its session (`closed_tabs`).
    closed_tabs: Vec<(String, crate::settings::SavedSurface)>,
    // The chat archived last and its Undo toast (`chrome_keys`).
    last_archive: Option<(String, u64)>,
    // The Open in menu (`open_in`).
    open_in: popover::Popup<open_in::OpenInMenu>,
    // Claude Code's past sessions (`past_sessions`).
    past_sessions: past_sessions::PastSessions,
    // The inbox (`inbox`).
    inbox: inbox::Inbox,
    /// Host-owned project Actions cached per (device, space).
    project_actions: crate::project_actions::ProjectActionsController,
    /// Diff surfaces by id — each tab its own [`Changes`] viewer with its own
    /// scope/base pick and diff watch (multiple diff panels, user request).
    diffs: std::collections::HashMap<u64, Entity<Changes>>,
    /// One workspace browser per chat/panel key. Dropping the entity closes
    /// its file watcher and every in-flight workspace request.
    files: std::collections::HashMap<String, Entity<FilesSurface>>,
    files_subs: std::collections::HashMap<String, Subscription>,
    /// One independent editor per opened workspace file. IDs are global
    /// while the lookup key keeps a file tab scoped to its chat panel.
    file_surfaces: std::collections::HashMap<u64, Entity<FilesSurface>>,
    file_surface_paths: std::collections::HashMap<u64, String>,
    /// Open editors by (pane, owning chat, path): a side chat's file links
    /// open editors bound to the side chat, beside the main chat's own.
    file_surface_keys: std::collections::HashMap<(String, String, String), u64>,
    file_surface_subs: std::collections::HashMap<u64, Subscription>,
    file_surface_seq: u64,
    pending_file_closes: std::collections::HashSet<RightSurface>,
    pending_exit: Option<PendingExit>,
    /// Event hookups for [`Self::diffs`] (History rows opening commit tabs).
    diff_subs: std::collections::HashMap<u64, Subscription>,
    diff_seq: u64,
    browsers: std::collections::HashMap<u64, Entity<crate::browser::BrowserSurface>>,
    browser_subs: std::collections::HashMap<u64, Subscription>,
    browser_seq: u64,
    browser_context: crate::browser::BrowserContext,
    browser_profile: Option<crate::browser::profile::BrowserProfile>,
    /// Chat outlet, pull request dashboard, or settings pages.
    route: Route,
    settings_focus: FocusHandle,
    settings_end_focus: FocusHandle,
    settings_nav_focus: Vec<FocusHandle>,
    settings_return_focus: Option<FocusHandle>,
    settings_focus_pending: bool,
    settings_restore_pending: bool,
    settings_return_route: Route,
    /// Route history behind the titlebar back/forward buttons (§ nav history).
    nav: NavHistory,
    devices_page: Option<Entity<DevicesPage>>,
    archived_page: Option<Entity<ArchivedPage>>,
    appearance_page: Option<Entity<AppearancePage>>,
    files_settings_page: Option<Entity<FilesSettingsPage>>,
    notifications_page: Option<Entity<NotificationsPage>>,
    shortcuts_page: Option<Entity<ShortcutsPage>>,
    harnesses_page: Option<Entity<HarnessesPage>>,
    shortcuts_sub: Option<Subscription>,
    notifications_sub: Option<Subscription>,
    files_settings_sub: Option<Subscription>,
    appearance_settings_sub: Option<Subscription>,
    /// Session-row context menu, including the Copy submenu.
    chat_menu: popover::Popup<ChatMenuState>,
    /// The Copy submenu's hover grace, its painted bounds, and which side
    /// of the menu it opens on (the sidebar options menu's recipe).
    chat_copy_intent: popover::HoverIntent<u8>,
    chat_copy_bounds: Option<gpui::Bounds<Pixels>>,
    chat_copy_left: bool,
    /// The chat title being edited in place on its row.
    chat_rename: Option<ChatRename>,
    // The keyboard of the remove-project dialog; sessions are archived, never deleted.
    delete_space_focus: crate::surface_chrome::ModalFocus,
    /// Keyboard of the New/Edit section dialog, given back to what had it on close.
    section_focus: crate::surface_chrome::ModalFocus,
    /// Keyboard of the project action editor and its Delete confirmation.
    project_action_focus: crate::surface_chrome::ModalFocus,
    /// Global confirmation/error dialog for the Changes-pane trash action. The
    /// RPC task is retained separately so rerenders do not cancel it.
    /// Space-row context menu (dropdown rows): (space id, window position).
    space_menu: popover::Popup<(String, Point<Pixels>)>,
    rename_space_dialog: Option<RenameSpaceDialog>,
    /// Space id awaiting delete confirmation (hard delete + session cascade).
    delete_space_confirm: Option<String>,
    /// The add-space palette (device tabs + folder search), `Some`
    /// while open.
    add_space: Option<AddSpaceFlow>,
    /// The New project palette's collapsed-breadcrumbs (`…`) menu.
    project_crumb_menu: popover::Popup<()>,
    command_palette: Option<command_palette::CommandPalette>,
    /// The session picker or Go to Directory, while up (`picker`).
    picker: Option<picker::Picker>,
    /// The focus whose workspace search index was last warmed (see `search_warm`).
    search_warm: Option<search_warm::SearchWarm>,
    /// Per chat, the files opened in editor tabs, most recent first.
    recent_files: std::collections::HashMap<String, std::collections::VecDeque<String>>,
    // The changed file the inspector asked for, shown on the next frame (it needs the
    // window).
    pending_change: Option<String>,
    /// The sidebar's space-filter dropdown.
    spaces_menu: popover::Popup<spaces::SpacesMenu>,
    /// Hover/drag + scroll-linger state of the dropdown's floating rail.
    spaces_menu_bar: popover::MenuScrollbarState,
    project_icons: project_icon::ProjectIcons,
    /// Hovered row whose status is replaced by the archive control.
    chat_status_hover: Option<String>,
    /// The time the sidebar's rows say how long ago they moved by: taken when the state
    /// moves and on [`Self::sidebar_clock_tick`], never as a row is built, so a sidebar built
    /// again (from scratch, or for a hover) says what the one shown says.
    sidebar_clock: chrono::DateTime<Utc>,
    /// When the list next reads differently of time alone (a row folding into Inactive,
    /// a session going quiet).
    sidebar_clock_next: std::cell::Cell<Option<chrono::DateTime<Utc>>>,
    /// The clock's next tick, at that time.
    sidebar_clock_tick: Option<(chrono::DateTime<Utc>, Task<()>)>,
    /// Set by an archive-pill click until the pointer next moves. gpui only
    /// re-evaluates `on_hover` on mouse movement, so the row that slides up
    /// under a still pointer would otherwise never show its archive pill;
    /// while set, rows adopt the hover from a paint-time hit test instead.
    chat_hover_resync: bool,
    /// The sidebar row whose hover card shows (or waits out its dwell).
    session_hover_card: session_hover_card::SessionHoverCard,
    /// Scroll position of the sidebar lists region (drives its edge fades).
    sidebar_scroll: gpui::ScrollHandle,
    /// `settings.last_space_id` applied once after the first spaces frame.
    space_boot_applied: bool,
    user_menu: popover::Popup<()>,
    /// Inline sidebar error strip (mutation failures); click dismisses.
    sidebar_notice: Option<SharedString>,
    mutate_task: Option<Task<()>>,
    /// Kept for the failed-gate "Retry" action.
    boot: EngineBootConfig,
    data_dir: PathBuf,
    settings: UiSettings,
    /// The store's settings when `settings` last synced with it. Fields that
    /// differ from this are the Shell's edits; all others follow the store.
    settings_base: UiSettings,
    /// The panel key of the chat currently shown ("" = new-chat canvas).
    active_chat: String,
    /// The chat whose panes the active one shows (`AppState::panel_owner`), with the
    /// chat it was found for: its agent, for a subagent of a call ([`Self::active_owner`]).
    active_owner: Option<(String, String)>,
    /// Last rendered sidebar order (key + estimated height) — the FLIP baseline
    /// for the §1.6 resort glide.
    sidebar_prev_order: Vec<(String, f32)>,
    /// The sidebar's rows as keyboard stops (`spaces::SidebarKeys`).
    sidebar_keys: spaces::SidebarKeys,
    /// Whether the outlet last drew Settings, and how often it has changed between
    /// Settings and the chats since launch (`route_outlet`).
    route_shown: Option<(bool, u64)>,
    /// Per-key paint offsets of the resort in flight, keyed elements restart on
    /// `resort_epoch` bumps.
    sidebar_resort: std::collections::HashMap<String, f32>,
    /// Keys that just appeared in a live list (fade in, no glide).
    sidebar_new_keys: std::collections::HashSet<String>,
    resort_epoch: usize,
    /// Last observed `window.is_window_active()` — rising edge fires a
    /// ProbeSync so a broadcast-deaf room heals as the user looks at the app.
    was_window_active: bool,
    /// Dev/testing knobs (`SLOPTY_OPEN_DIALOG`, `SLOPTY_FORCE_GATE`) — see [`Shell::new`].
    debug_dialog: Option<String>,
    debug_gate: Option<GatePhase>,
    sidebar_tween: Option<WidthTween>,
    /// The last tracked frame: the window fit, the sidebar's painted width and the width its
    /// content laid out at.
    painted_sidebar: Option<(HorizontalPanelFit, (f32, f32))>,
    /// The sidebar easing out from the width it last painted after the window grew too narrow
    /// for it, its content held at its layout width.
    fit_exit: Option<(WidthTween, f32)>,
    sidebar_edge_bounce: Option<motion::ResizeEdgeBounce>,
    /// Boundary currently held during a sidebar drag. Cleared on re-entry or
    /// release so the next genuine edge crossing can acknowledge the limit.
    sidebar_resize_edge: Option<motion::ResizeEdge>,
    /// Gesture-owned resize feedback. Unlike hover, this stays active while
    /// the seam moves away from the pointer and clears only on release or when
    /// a constrained edge takes over with its bounce cue.
    pane_resize_active: Option<PaneResizeKind>,
    pane_resize_dragging: Option<PaneResizeKind>,
    /// The window changed width since the previous frame. Live window
    /// resizing moves layout directly instead of through motion springs.
    viewport_resized: bool,
    /// Viewport width stamped each frame at render, for the fit and the splits.
    viewport_width: f32,
    /// Last observed `window.is_fullscreen()` (`None` before first paint) —
    /// flips key the traffic-light inset tween.
    fullscreen: Option<bool>,
    /// 200ms ease-out tween of the cluster start on fullscreen toggles.
    titlebar_tween: Option<WidthTween>,
    /// Armed by mouse-down on a titlebar strip; the next mouse-move hands the
    /// drag to the compositor (zed's platform-titlebar pattern).
    titlebar_should_move: bool,
    /// The caption buttons Slopty itself draws on Linux under client-side
    /// decorations, per side, already filtered to what the compositor
    /// supports — `None` off Linux or under server decorations (where the WM
    /// draws real buttons). Re-resolved every frame at the top of `render`.
    linux_captions: Option<gpui::WindowButtonLayout>,
    /// Re-renders when the desktop's button layout changes (GNOME
    /// `button-layout` gsetting). Registered on first paint — [`Shell::new`]
    /// has no window.
    button_layout_sub: Option<Subscription>,
    /// `motion::reduced_motion` snapshot, refreshed at the top of each render
    /// pass so [`Shell::eval_tween`] (called from `&self` render helpers) can
    /// snap without a `cx`. Only the global flag makes one.
    reduced_motion: motion::ReducedMotion,
    /// Set by [`Shell::eval_tween`] when any tween is mid-flight this frame;
    /// render schedules the next animation frame off it.
    motion_active: std::cell::Cell<bool>,
    /// All pane masks and chrome evaluate animation at the same frame time.
    /// A slow render must not give the native page and its titlebar different widths.
    render_time: Option<std::time::Instant>,
    splash: SplashPhase,
    splash_task: Option<Task<()>>,
    /// Focus fallback (registered on first paint — [`Shell::new`] has no
    /// window): keyboard shortcuts dispatch through the window focus chain, so
    /// with missing or unmounted focus they go dead. Recover after handoffs
    /// settle, preserving focus on mounted controls.
    focus_sub: Option<Subscription>,
    shortcut_focus: FocusHandle,
    /// The main column's focus: a press in it keeps the keyboard there (`navigation_focus`).
    main_focus: FocusHandle,
    /// Neutral shortcut target after clicking away from an input.
    unfocused: FocusHandle,
    /// Clears the jump hints when the window deactivates: a Cmd+Tab away
    /// swallows the key-up, so without this the chips stay on screen for good.
    activation_sub: Option<Subscription>,
    /// Redraw scheduled for the next clock-driven change (see [`shell_clock_wake`]).
    clock: ClockRedraw,
    /// The dispatcher, whose clock the shell's motion reads between frames.
    executor: gpui::BackgroundExecutor,
    _state_observation: Subscription,
    _inspector_events: Subscription,
    /// Hears the session terminal's ground change, which the titlebar row wears.
    _session_terminal_events: Subscription,
    _transcript_invalidation: Subscription,
}

impl Shell {
    pub fn new(state: Entity<AppState>, boot: EngineBootConfig, cx: &mut Context<Self>) -> Self {
        motion::run_on(cx.background_executor().clone()); // hover flips on the dispatcher's clock
        let observation = cx.observe(&state, |this: &mut Shell, state, cx| {
            this.sidebar_clock = crate::clock::utc(cx); // the list's clock moves with the state
            this.on_state_changed(&state, cx);
            // A result arriving in the chat being looked at is seen as it arrives, here
            // where every view watching the state hears it, before the frame that shows it.
            if this.was_window_active {
                this.mark_open_chat_seen(cx);
            }
            cx.notify();
        });
        // A settings file that could not be read was moved aside; say so once.
        cx.defer(|cx| {
            settings::announce_set_aside(cx);
        });
        // A page a card moves into the pane becomes a tab (M).
        cx.observe_global::<crate::page::PagePaneAsk>(Self::on_page_pane_ask).detach();
        // A reopened window reuses AppState, so the engine may already be
        // ready. Only show the boot splash while it is actually connecting.
        let splash = match &state.read(cx).connection {
            ConnectionStatus::Connecting => SplashPhase::Visible,
            ConnectionStatus::Ready | ConnectionStatus::Failed(_) => SplashPhase::Gone,
        };
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, _| transcript.retain_for_route_exit());
        let links = Self::session_links(None, cx);
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        // The session's terminal in the main column, its inspector in the right pane.
        let session_terminal = cx.new(|cx| TerminalPanel::new_session(state.clone(), cx));
        // The titlebar row and the column wear the shown terminal's ground.
        let session_terminal_events =
            cx.subscribe(&session_terminal, |_, _, _: &crate::terminal::panel::ShownChanged, cx| {
                cx.notify();
            });
        let inspector = cx.new(|cx| crate::inspector::Inspector::new(state.clone(), transcript.clone(), cx));
        let inspector_events = cx.subscribe(&inspector, Self::on_inspector_event);
        let data_dir = boot.data_dir.clone();
        let mut settings = settings::current(cx);
        state.update(cx, |state, cx| state.watch_clock_transitions(cx));
        // Bind the customizable shortcuts from the persisted keymap.
        apply_keymap(cx, &settings.keymap);
        // Dev/testing knob: `SLOPTY_OPEN_ROUTE=settings[/<section>]` boots
        // straight into a settings section — these pages have no deep link and
        // synthetic input can't reach them on headless compositors. Bare
        // `settings` reopens the remembered section; a named one is
        // remembered like any other link to a section.
        let open_route = std::env::var("SLOPTY_OPEN_ROUTE").ok();
        let route = match open_route.as_deref() {
            Some(route) if route == "settings" || route.starts_with("settings/") => {
                match settings_open_route(route, settings.settings_section) {
                    Some(section) => {
                        if settings.settings_section != section {
                            settings.settings_section = section;
                            settings::update(settings::SavePolicy::Debounced, cx, |s| {
                                s.settings_section = section;
                            });
                        }
                        Route::Settings(section)
                    }
                    None => Route::Chat,
                }
            }
            // `new` pins the new-chat canvas (suppresses boot auto-select).
            Some("new") => {
                state.update(cx, |s, _| s.auto_selected = true);
                Route::Chat
            }
            _ => Route::Chat,
        };
        // More capture knobs of the same kind: `SLOPTY_OPEN_DIALOG=rename`
        // opens that dialog for the first chat once chats land (`add-space` the
        // add-project palette);
        // `SLOPTY_FORCE_GATE=signin|org|failed` renders that gate regardless of
        // real auth state (display-only — for styling passes).
        let debug_dialog = std::env::var("SLOPTY_OPEN_DIALOG").ok();
        let debug_gate = match std::env::var("SLOPTY_FORCE_GATE").ok().as_deref() {
            Some("failed") => Some(GatePhase::Failed(
                "Couldn\u{2019}t reach the Slopty engine on port 27901. Restart Slopty.".into(),
            )),
            _ => None,
        };
        let nav = NavHistory::new(match route {
            Route::Chat => NavEntry::Chat(String::new()),
            Route::Settings(section) => NavEntry::Settings(section),
        });
        // Parent notifications carry presentation changes (session status,
        // elapsed labels, menus); sibling animation/caret ticks do not.
        let transcript_invalidation = cx.observe_self(|shell, cx| {
            shell.transcript.update(cx, |_, cx| cx.notify());
        });
        let shell = cx.entity();
        let sidebar_pane = cx.new(|cx| SidebarPane {
            shell: shell.downgrade(),
            _observation: cx.observe(&shell, |_, _, cx| cx.notify()),
        });
        Self {
            state,
            sidebar_pane,
            transcript,
            // Seed with the compact composer stack's rough height so the
            // first frame's clearance isn't zero (the measure corrects it).
            archived_open: true,
            archived_shown: 0,
            sidebar_group_shown: std::collections::HashMap::new(),
            sidebar_collapsed_groups: sidebar_status::shut_at_first(),
            sidebar_reveal_motions: std::collections::HashSet::new(),
            sidebar_disclosure_motion: std::collections::HashMap::new(),
            jump_hints: false,
            panes: std::collections::HashMap::new(),
            pane_shells: std::collections::HashMap::new(),
            pane_seq: 0,
            pane_area_at: std::rc::Rc::default(),
            pane_grid: None,
            view_handles: std::collections::HashMap::new(),
            pane_keyboard_pending: false,
            unrestored_tabs: settings.session_tabs.clone(),
            closed_tabs: Vec::new(),
            last_archive: None,
            open_in: popover::Popup::default(),
            past_sessions: past_sessions::PastSessions::default(),
            inbox: inbox::Inbox::default(),
            project_actions: crate::project_actions::ProjectActionsController::default(),
            diffs: std::collections::HashMap::new(),
            files: std::collections::HashMap::new(),
            files_subs: std::collections::HashMap::new(),
            file_surfaces: std::collections::HashMap::new(),
            file_surface_paths: std::collections::HashMap::new(),
            file_surface_keys: std::collections::HashMap::new(),
            file_surface_subs: std::collections::HashMap::new(),
            file_surface_seq: 0,
            pending_file_closes: std::collections::HashSet::new(),
            pending_exit: None,
            diff_subs: std::collections::HashMap::new(),
            diff_seq: 0,
            browsers: std::collections::HashMap::new(),
            browser_subs: std::collections::HashMap::new(),
            browser_seq: 0,
            browser_context: crate::browser::BrowserContext::default(),
            browser_profile: None,
            route,
            settings_focus: cx.focus_handle(),
            settings_end_focus: cx.focus_handle(),
            settings_nav_focus: SettingsSection::ALL
                .iter()
                .map(|_| cx.focus_handle())
                .collect(),
            settings_return_focus: None,
            settings_focus_pending: matches!(route, Route::Settings(_)),
            settings_restore_pending: false,
            settings_return_route: Route::Chat,
            nav,
            devices_page: None,
            archived_page: None,
            appearance_page: None,
            files_settings_page: None,
            notifications_page: None,
            shortcuts_page: None,
            harnesses_page: None,
            shortcuts_sub: None,
            notifications_sub: None,
            files_settings_sub: None,
            appearance_settings_sub: None,
            chat_menu: popover::Popup::default(),
            chat_copy_intent: Default::default(),
            chat_copy_bounds: None,
            chat_copy_left: false,
            chat_rename: None,
            delete_space_focus: crate::surface_chrome::ModalFocus::new(cx),
            section_focus: crate::surface_chrome::ModalFocus::new(cx),
            project_action_focus: crate::surface_chrome::ModalFocus::new(cx),
            space_menu: popover::Popup::default(),
            rename_space_dialog: None,
            delete_space_confirm: None,
            add_space: None,
            project_crumb_menu: popover::Popup::default(),
            command_palette: None,
            picker: None,
            search_warm: None,
            recent_files: std::collections::HashMap::new(),
            pending_change: None,
            spaces_menu: popover::Popup::default(),
            spaces_menu_bar: popover::MenuScrollbarState::default(),
            project_icons: Default::default(),
            chat_status_hover: None,
            sidebar_clock: crate::clock::utc(cx),
            sidebar_clock_next: std::cell::Cell::new(None),
            sidebar_clock_tick: None,
            chat_hover_resync: false,
            session_hover_card: Default::default(),
            sidebar_scroll: gpui::ScrollHandle::new(),
            space_boot_applied: false,
            user_menu: popover::Popup::default(),
            sidebar_notice: None,
            mutate_task: None,
            boot,
            data_dir,
            settings_base: settings::current(cx),
            settings,
            active_chat: String::new(),
            active_owner: None,
            sidebar_prev_order: Vec::new(),
            sidebar_keys: spaces::SidebarKeys::default(),
            route_shown: None,
            sidebar_resort: std::collections::HashMap::new(),
            sidebar_new_keys: std::collections::HashSet::new(),
            resort_epoch: 0,
            was_window_active: false,
            debug_dialog,
            debug_gate,
            sidebar_tween: None,
            painted_sidebar: None,
            fit_exit: None,
            sidebar_edge_bounce: None,
            sidebar_resize_edge: None,
            pane_resize_active: None,
            pane_resize_dragging: None,
            viewport_resized: false,
            viewport_width: 1280.0,
            fullscreen: None,
            titlebar_tween: None,
            titlebar_should_move: false,
            linux_captions: None,
            button_layout_sub: None,
            reduced_motion: motion::ReducedMotion::default(),
            motion_active: std::cell::Cell::new(false),
            render_time: None,
            splash,
            splash_task: None,
            focus_sub: None,
            shortcut_focus: cx.focus_handle(),
            main_focus: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            activation_sub: None,
            clock: ClockRedraw::default(),
            executor: cx.background_executor().clone(),
            _state_observation: observation,
            _inspector_events: inspector_events,
            _session_terminal_events: session_terminal_events,
            session_terminal,
            window_ground: Default::default(),
            landed: None,
            inspector,
            followed_terminal: None,
            new_session: None,
            starting: None,
            _starting_redraw: None,
            _transcript_invalidation: transcript_invalidation,
        }
    }


    // ---- splash ----

    /// A session is its terminal: when the terminal the selected session was shown in
    /// becomes another session's (an agent started in a plain terminal, `/clear` or `/resume`
    /// in an agent's, the agent left it), the selection follows the terminal. A session chosen
    /// once it ended stays chosen.
    fn follow_session_terminal(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(selected) = state.selected_chat.clone() else {
            self.followed_terminal = None;
            return;
        };
        let owner = state.panel_owner(&selected);
        let now = state.session_terminals.get(&owner).cloned();
        if now.is_some() && self.starting.as_ref().is_some_and(|(chat, _)| *chat == owner) {
            self.starting = None;
        }
        let before = self.followed_terminal.replace((owner.clone(), now.clone()));
        let Some((was_chat, Some(was_terminal))) = before else { return };
        if was_chat != owner || now.as_ref() == Some(&was_terminal) {
            return;
        }
        let next = state
            .session_terminals
            .iter()
            .find(|(chat, terminal)| **terminal == was_terminal && **chat != owner)
            .map(|(chat, _)| chat.clone());
        if let Some(next) = next {
            self.followed_terminal = Some((next.clone(), Some(was_terminal)));
            self.open_chat(next, cx);
        }
    }

    fn on_state_changed(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        self.follow_session_terminal(cx);
        self.prune_file_explorers(cx);
        self.sync_search_warm(cx);
        if let Some(notice) = state.update(cx, |state, _| state.take_deep_link_notice()) {
            self.show_notice(crate::toast::ToastKind::Info, notice, cx);
        }
        // Capture knob: the add-space palette needs only the device registry.
        if self.debug_dialog.as_deref() == Some("add-space") && !state.read(cx).devices.is_empty() {
            self.debug_dialog = None;
            self.open_add_space(cx);
        }
        // Capture knob: pop the requested dialog once chats have landed.
        if let Some(which) = self.debug_dialog.clone()
            && let Some(first) = state.read(cx).chats.first().map(|c| c.id.clone())
        {
            self.debug_dialog = None;
            if which == "rename" {
                self.open_rename_chat(first, cx);
            }
        }
        // An explicit projectless canvas must be visible in the sidebar:
        // retaining a project filter would hide the session on its first send.
        if state.read(cx).no_project
            && state.read(cx).selected_chat.is_none()
            && self.settings.space_filter.take().is_some()
        {
            self.schedule_save(cx);
        }
        // Boot: restore the last selected space once the first spaces frame
        // lands (a still-existing row wins over the auto-selected first one;
        // the boot-auto-selected chat's own space wins over both — selecting a
        // chat implies its space, which `select_chat` already applied).
        if !self.space_boot_applied && !state.read(cx).spaces.is_empty() {
            self.space_boot_applied = true;
            if state.read(cx).selected_chat.is_none() {
                // A set sidebar filter is an explicit standing choice — the
                // canvas defaults (project AND its device) follow it, even
                // over a remembered "no project" opt-out. Otherwise the last
                // project stands (`last_space_id`, which follows opened chats),
                // unless opted out.
                let exists = |id: &String| state.read(cx).space_row(id).is_some();
                let filter = self.settings.space_filter.clone().filter(&exists);
                let target = match filter {
                    Some(filter) => Some(filter),
                    None if !state.read(cx).no_project => {
                        self.settings.last_space_id.clone().filter(&exists)
                    }
                    None => None,
                };
                if target.is_some() {
                    state.update(cx, |s, cx| s.select_space(target, cx));
                }
            }
        }
        // Persist the selected space (the new-tab fallback under "All").
        {
            let selected_space = state.read(cx).selected_space.clone();
            if selected_space != self.settings.last_space_id && selected_space.is_some() {
                self.settings.last_space_id = selected_space;
                self.schedule_save(cx);
            }
        }
        // Boot landing: the most recent session once the first chats frame
        // syncs (manual selection wins).
        self.boot_select_chat(cx);
        // Heal a dangling sidebar filter (space deleted, possibly elsewhere):
        // fall back to "All" rather than filtering everything out.
        if state.read(cx).spaces_synced
            && let Some(filter) = self.settings.space_filter.clone()
            && state.read(cx).space_row(&filter).is_none()
            // Not while a machine is out of reach: its projects are missing, not gone.
            && spaces::every_device_reached(state.read(cx), crate::clock::utc(cx))
        {
            self.settings.space_filter = None;
            self.schedule_save(cx);
        }
        // Heal a machine filter naming a machine no longer known (removed, possibly
        // elsewhere): every machine again, rather than an empty list. A machine out of reach is
        // still known, so its filter stands (`spaces::Shell::sidebar_filter`).
        if !state.read(cx).devices.is_empty()
            && let Some(machine) = self.settings.machine_filter.clone()
            && !state.read(cx).devices.iter().any(|device| device.id == machine)
        {
            self.settings.machine_filter = None;
            self.schedule_save(cx);
        }
        // Chat switch: the destination's panes show as it left them, at once.
        let selected = state.read(cx).selected_chat.clone().unwrap_or_default();
        // The panes a chat shows are its owner's: a subagent's are its agent's, and one
        // whose agent's row came only now moves to them.
        let owner = state.read(cx).panel_owner(&selected);
        if selected != self.active_chat || owner != self.active_owner() {
            self.suspend_file_images(cx);
            self.active_owner = Some((selected.clone(), owner));
            self.active_chat = selected;
            // Route history: a chat switch is a navigation. The very first
            // selection off the untouched boot canvas REPLACES that entry, leaving no
            // dead Back target. Walking history lands here too, but the
            // destination already equals `current()`, so the push dedups.
            if matches!(self.route, Route::Chat) {
                let entry = NavEntry::Chat(self.active_chat.clone());
                if self.nav.len() == 1 && *self.nav.current() == NavEntry::Chat(String::new()) {
                    self.nav.replace(entry);
                } else {
                    self.nav.push(entry);
                }
            }
            self.fit_exit = None;
            // Its keyboard goes back to the pane that had it, once that pane draws.
            self.pane_keyboard_pending = true;
            for view in self.panes_now(cx).views() {
                if let RightSurface::Diff(id) = view
                    && let Some(changes) = self.diffs.get(&id).cloned()
                {
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                }
            }
        }
        match state.read(cx).connection {
            ConnectionStatus::Ready => {
                if self.splash == SplashPhase::Visible {
                    self.splash = SplashPhase::FadingOut;
                    self.splash_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(SPLASH_OUT.total() + Duration::from_millis(30))
                            .await;
                        this.update(cx, |shell, cx| {
                            shell.splash = SplashPhase::Gone;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
            }
            // Reveal the gate card immediately; the splash never returns mid-session.
            ConnectionStatus::Failed(_) => self.splash = SplashPhase::Gone,
            ConnectionStatus::Connecting => {}
        }
    }

    // ---- layout state ----

    fn sidebar_target(&self) -> f32 {
        let fit = self.horizontal_fit();
        if !fit.sidebar {
            0.0
        } else if self.settings.sidebar_collapsed {
            // Put away, the sidebar leaves its rail of one mark per project
            // (`sidebar::Shell::render_sidebar_rail`).
            sidebar::SIDEBAR_RAIL_WIDTH
        } else {
            fit.sidebar_limit
        }
    }

    /// Does the selected space's folder have git? Owner-stamped and synced —
    /// gates the Changes pane, its toggle, and Cmd-B with zero RPCs.
    fn space_git_detected(&self, cx: &App) -> bool {
        self.state.read(cx).selected_space_git()
    }

    /// The current chat's changes-pane flag (per-session, in-memory), gated on
    /// the space having git at all: a stale per-chat open flag must not reopen
    /// the pane after switching into a non-git space.
    /// The per-session panel key. The new-chat canvas (no selection) keys per
    /// SPACE — one shared "" key made a canvas toggle read as global state
    /// (user report).
    fn panel_key(&self, cx: &App) -> String {
        if self.active_chat.is_empty() {
            crate::state::canvas_panel_key(self.state.read(cx).selected_space.as_deref())
        } else {
            // A subagent shows its agent's panes.
            self.active_owner().to_owned()
        }
    }

    /// The chat whose panes the active chat shows: its agent's, for a subagent of a call.
    fn active_owner(&self) -> &str {
        match &self.active_owner {
            Some((chat, owner)) if *chat == self.active_chat => owner,
            _ => &self.active_chat,
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        // On a phone the sidebar button goes back to Sessions.
        if self.compact_back(cx) {
            return;
        }
        let from = self.sidebar_now();
        self.session_hover_card.dismiss();
        self.sidebar_edge_bounce = None;
        self.sidebar_resize_edge = None;
        self.pane_resize_active = None;
        self.pane_resize_dragging = None;
        self.settings.sidebar_collapsed = !self.settings.sidebar_collapsed;
        self.sidebar_tween = Some(self.panel_tween(from, self.sidebar_target()));
        self.schedule_save(cx);
        cx.notify();
    }

    /// What a press in the inspector asks.
    fn on_inspector_event(
        &mut self,
        _: Entity<crate::inspector::Inspector>,
        event: &crate::inspector::InspectorEvent,
        cx: &mut Context<Self>,
    ) {
        // A subagent drills in inside the inspector itself (`inspector.md`, "A long or
        // nested run drills in, inside the inspector"): the selection, the terminal and the
        // keyboard stay where they are.
        let crate::inspector::InspectorEvent::OpenChange { path } = event;
        self.pending_change = Some(path.clone());
        cx.notify();
    }

    fn suspend_file_images(&mut self, cx: &mut Context<Self>) {
        for files in self.files.values().chain(self.file_surfaces.values()) {
            files.update(cx, |files, cx| files.suspend_images(cx));
        }
    }

    fn set_files_word_wrap(
        &mut self,
        word_wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.files_word_wrap = word_wrap;
        if let Some(page) = self.files_settings_page.clone() {
            page.update(cx, |page, cx| page.set_word_wrap(word_wrap, cx));
        }
        let surfaces = self.file_surfaces.values().cloned().collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_word_wrap(word_wrap, window, cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Push a new code size into every open file surface. Called by the
    /// Appearance settings page, which owns the control. The typography
    /// global is the canonical store and persists on its own; this only
    /// propagates the change to already-open surfaces.
    pub(crate) fn set_code_font_size(&mut self, code_font_size: f32, cx: &mut Context<Self>) {
        let surfaces = self
            .files
            .values()
            .chain(self.file_surfaces.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_editor_font_size(code_font_size, cx)
            });
        }
        cx.notify();
    }

    fn set_files_show_all(&mut self, show_all_files: bool, cx: &mut Context<Self>) {
        self.settings.files_show_all = show_all_files;
        if let Some(page) = self.files_settings_page.clone() {
            page.update(cx, |page, cx| page.set_show_all_files(show_all_files, cx));
        }
        let surfaces = self
            .file_surfaces
            .values()
            .chain(self.files.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_show_all_files(show_all_files, cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn session_links(
        source_session: Option<String>,
        cx: &Context<Self>,
    ) -> crate::markdown::render::LinkUi {
        let shell = cx.weak_entity();
        crate::markdown::render::LinkUi {
            source_session,
            source_local: false,
            file_roots: None,
            handler: std::rc::Rc::new(move |activation, window, cx| {
                shell
                    .update(cx, |shell, cx| {
                        shell.activate_session_link(activation, window, cx)
                    })
                    .unwrap_or(crate::markdown::render::LinkOutcome::Rejected)
            }),
        }
    }

    fn activate_session_link(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::markdown::render::LinkOutcome {
        use crate::markdown::render::{LinkAction, LinkOutcome};
        let Some(source) = activation.source_session.clone() else {
            return LinkOutcome::Rejected;
        };
        let from_main = !self.active_chat.is_empty()
            && source == self.active_chat
            && self.state.read(cx).selected_chat.as_deref() == Some(self.active_chat.as_str());
        if !from_main {
            return LinkOutcome::Rejected;
        }
        if activation.target.navigation.is_err() {
            return if matches!(
                activation.action,
                LinkAction::Primary | LinkAction::Internal
            ) && self.open_workspace_file_link(
                &source,
                &activation.target.original,
                window,
                cx,
            ) {
                LinkOutcome::Internal
            } else {
                LinkOutcome::Rejected
            };
        }
        let mut resolved = activation.clone();
        if resolved.action == LinkAction::Primary {
            resolved.action = if crate::settings::current(cx).open_web_links_in_slopty {
                LinkAction::Internal
            } else {
                LinkAction::External
            };
        }
        let outcome = resolved.web_outcome(cfg!(any(target_os = "macos", target_os = "linux", windows)));
        if outcome == LinkOutcome::Internal {
            self.add_browser_surface(activation.target.navigation.clone().ok(), window, cx);
        }
        outcome
    }

    fn sync_browser_profile(&mut self, cx: &mut Context<Self>) {
        let browser_profile = {
            let state = self.state.read(cx);
            crate::browser::profile::BrowserProfile::for_workspace(
                &self.data_dir,
                state.workspace_scope,
                state.auth.as_ref(),
                state.local_device_id.as_deref(),
            )
        };
        if browser_profile != self.browser_profile {
            for browser in self.browsers.values() {
                browser.update(cx, |browser, cx| browser.close(cx));
            }
            self.browsers.clear();
            self.browser_subs.clear();
            self.browser_context = browser_profile
                .clone()
                .map(crate::browser::BrowserContext::for_profile)
                .unwrap_or_default();
            self.browser_profile = browser_profile;
        }
    }

    /// Browser tabs are independent instances owned by the current session.
    fn add_browser_surface(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        self.sync_browser_profile(cx);
        let key = self.panel_key(cx);
        let remote = {
            let state = self.state.read(cx);
            state.selected_chat_row().is_some_and(|chat| {
                Some(chat.device_id.as_str()) != state.local_device_id.as_deref()
            })
        };
        self.browser_seq += 1;
        let id = self.browser_seq;
        let browser = cx.new(|cx| {
            crate::browser::BrowserSurface::new(self.browser_context.clone(), remote, window, cx)
        });
        if let Some(handle) = self.state.read(cx).engine().cloned() {
            let chat_id = self.active_chat.clone();
            browser.update(cx, |browser, cx| {
                browser.watch_previews(handle, chat_id, cx)
            });
        }
        let owner = key.clone();
        let sub = cx.subscribe_in(&browser, window, move |this, _, event, window, cx| {
            match event {
                crate::browser::BrowserEvent::Changed => cx.notify(),
                crate::browser::BrowserEvent::NewTab(url) => {
                    // A background page cannot open a tab in the wrong session.
                    if this.panel_key(cx) == owner
                        && this.front_view(cx) == Some(RightSurface::Browser(id))
                    {
                        this.add_browser_surface(url.clone(), window, cx);
                    }
                }
                crate::browser::BrowserEvent::Close => {
                    this.close_view(RightSurface::Browser(id), cx);
                }
            }
        });
        self.browsers.insert(id, browser.clone());
        self.browser_subs.insert(id, sub);
        self.open_view(RightSurface::Browser(id), cx);
        browser.update(cx, |browser, cx| {
            if let Some(url) = url {
                browser.navigate(&url, window, cx);
            } else {
                browser.focus_address(window, cx);
            }
        });
    }

    /// A fresh view of the session's changes beside it, with a scope and base of its own: one
    /// kept across launches comes back so, and several can stand at once.
    fn add_diff_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = cx.new(|cx| Changes::new(self.state.clone(), cx));
        self.register_diff_surface(changes, window, cx);
    }

    /// Open or focus a session-owned editor tab. The explorer is independent.
    fn add_file_surface(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let owner = (self.active_chat.clone(), self.state.clone());
        self.add_file_surface_at(owner, path, None, window, cx);
    }

    /// The chat and state a transcript link resolves in: the main chat's, or
    /// an open side chat's own (its chat row may not reach the main list yet).
    fn link_owner(&self, chat_id: &str, cx: &App) -> Option<(String, Entity<AppState>)> {
        let _ = cx;
        (chat_id == self.active_chat).then(|| (chat_id.to_owned(), self.state.clone()))
    }

    /// Open or focus the editor for `path` owned by `owner` (chat id + the
    /// state that resolves it): reads and saves go to that chat's checkout.
    fn add_file_surface_at(
        &mut self,
        owner: (String, Entity<AppState>),
        path: String,
        location: Option<(u32, Option<FileColumn>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() || owner.0.is_empty() {
            return;
        }
        let (owner_chat, owner_state) = owner;
        let panel_key = self.panel_key(cx);
        let lookup = (panel_key.clone(), owner_chat.clone(), path.clone());
        if let Some(id) = self.file_surface_keys.get(&lookup).copied() {
            self.open_view(RightSurface::File(id), cx);
            if let Some((line, column)) = location
                && let Some(file) = self.file_surfaces.get(&id).cloned()
            {
                file.update(cx, |file, cx| file.navigate_to_line(line, column, cx));
            }
            return;
        }

        self.file_surface_seq += 1;
        let id = self.file_surface_seq;
        let file = cx.new(|cx| {
            FilesSurface::new_editor(
                owner_state.clone(),
                owner_chat.clone(),
                path.clone(),
                self.settings.files_autosave_enabled,
                self.settings.files_autosave_delay_ms,
                crate::typography::code_font_size(cx),
                self.settings.files_word_wrap,
                self.settings.files_show_all,
                cx,
            )
        });
        let event_panel_key = panel_key.clone();
        let sub = cx.subscribe_in(
            &file,
            window,
            move |this: &mut Self, source, event, window, cx| {
                if matches!(event, FilesEvent::OpenFile(_) | FilesEvent::RevealFile(_))
                    && !this.accepts_file_navigation(&event_panel_key, &source, cx)
                {
                    return;
                }
                match event {
                    FilesEvent::HoldMutation { origin, paths } => {
                        let surfaces = this
                            .files
                            .values()
                            .chain(this.file_surfaces.values())
                            .filter(|s| s.read(cx).shares_workspace(origin))
                            .cloned()
                            .collect::<Vec<_>>();
                        for surface in surfaces {
                            surface.update(cx, |files, cx| files.hold_mutation(paths.clone(), cx));
                        }
                    }
                    FilesEvent::Mutate(intent) => {
                        this.start_file_mutation(source.clone(), intent.clone(), cx)
                    }
                    FilesEvent::MutateMany(intents) => {
                        this.start_file_mutations(source.clone(), intents.clone(), cx)
                    }
                    // Navigation from an editor stays in its own chat.
                    FilesEvent::OpenFile(path) => {
                        let owner = (source.read(cx).chat_id().to_owned(), owner_state.clone());
                        this.add_file_surface_at(owner, path.clone(), None, window, cx)
                    }
                    FilesEvent::RevealFile(path) => {
                        this.ensure_explorer(window, cx);
                        this.open_view(RightSurface::Files, cx);
                        if let Some(files) = this.files.get(&this.panel_key(cx)).cloned() {
                            files.update(cx, |files, cx| {
                                files.reveal_file_explicit(path.clone(), cx)
                            });
                        }
                    }
                    FilesEvent::OpenWebLink(activation) => {
                        if let crate::markdown::render::LinkOutcome::External(url) =
                            this.activate_session_link(activation, window, cx)
                        {
                            cx.open_url(&url);
                        }
                    }
                    FilesEvent::TitleChanged => cx.notify(),
                    FilesEvent::FileRenamed { old_path, new_path } => {
                        this.rename_file_surface(id, &event_panel_key, old_path, new_path, cx)
                    }
                    FilesEvent::WordWrapChanged(word_wrap) => {
                        this.set_files_word_wrap(*word_wrap, window, cx)
                    }
                    FilesEvent::ShowAllFilesChanged(show_all_files) => {
                        this.set_files_show_all(*show_all_files, cx)
                    }
                    FilesEvent::CloseReady => {
                        this.on_file_close_ready(RightSurface::File(id), &event_panel_key, cx)
                    }
                    FilesEvent::CloseCancelled => {
                        this.cancel_file_close(RightSurface::File(id), cx)
                    }
                }
            },
        );
        self.file_surfaces.insert(id, file);
        self.file_surface_paths.insert(id, path);
        self.file_surface_keys.insert(lookup, id);
        self.file_surface_subs.insert(id, sub);
        self.open_view(RightSurface::File(id), cx);
        if let Some((line, column)) = location
            && let Some(file) = self.file_surfaces.get(&id).cloned()
        {
            file.update(cx, |file, cx| file.navigate_to_line(line, column, cx));
        }
    }

    /// A web link a terminal of chat `chat_id` handed over (quick select): in the session's
    /// Browser when the person keeps web links in Slopty and the chat is the one shown, as a
    /// transcript's link opens; else in their browser.
    pub(crate) fn open_session_web_link(
        &mut self,
        chat_id: &str,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shown = !self.active_chat.is_empty() && self.active_chat == chat_id;
        if shown && crate::settings::current(cx).open_web_links_in_slopty {
            self.add_browser_surface(Some(url.to_owned()), window, cx);
        } else {
            cx.open_url(url);
        }
        true
    }

    /// Open a transcript's file link in the linking chat's own checkout: a
    /// side chat's link resolves against, and edits, the side chat's files.
    /// An absolute path the linking checkout cannot own falls through the
    /// chat's parent and this device's project roots before it is treated
    /// as a host file; the first known root that owns the path wins. A chat
    /// root opens the file in that chat's context; a project root and a host
    /// file open by absolute path through the linking chat.
    // Slopty's views' links come here too (`live::open_file`).
    pub(crate) fn open_workspace_file_link(
        &mut self,
        chat_id: &str,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let owner_state = self
            .link_owner(chat_id, cx)
            .map(|(_, state)| state)
            .unwrap_or_else(|| self.state.clone());
        let roots = owner_state.read(cx).file_link_roots(chat_id);
        let root_refs: Vec<&str> = roots.iter().map(|root| root.root.as_str()).collect();
        use crate::workspace_links::FileLinkResolution;
        let (owner_chat, link) = match crate::workspace_links::first_root_owning(target, root_refs)
        {
            Some(FileLinkResolution::Owned { root, link }) => match roots[root].chat.as_deref() {
                Some(owner) => (owner.to_owned(), link),
                // A linking chat without a checkout of its own resolves in
                // its own file context, as before.
                None if root == 0 => (chat_id.to_owned(), link),
                // A project root past the linking chat's own has no chat of
                // its own, and its relative path would name a different file
                // under the linking chat's checkout: read it by absolute path
                // through the linking chat, which keeps it editable only
                // inside that checkout.
                None => (
                    chat_id.to_owned(),
                    crate::workspace_links::WorkspaceFileLink {
                        path: roots[root].absolute(&link).to_string_lossy().into_owned(),
                        outside: true,
                        ..link
                    },
                ),
            },
            // An absolute path no known root owns is still a file link: it
            // opens read-only through the linking chat's own file context.
            Some(FileLinkResolution::Outside(link)) => (chat_id.to_owned(), link),
            None => return false,
        };
        let Some(owner) = self.link_owner(&owner_chat, cx) else {
            return false;
        };

        self.add_file_surface_at(
            owner,
            link.path,
            link.line
                .map(|line| (line, link.column.map(FileColumn::Character))),
            window,
            cx,
        );
        true
    }

    fn rename_file_surface(
        &mut self,
        id: u64,
        panel_key: &str,
        old_path: &str,
        new_path: &str,
        cx: &mut Context<Self>,
    ) {
        if self.file_surface_paths.get(&id).map(String::as_str) != Some(old_path) {
            return;
        }
        self.file_surface_paths.insert(id, new_path.to_string());
        let Some(owner) = self
            .file_surfaces
            .get(&id)
            .map(|file| file.read(cx).chat_id().to_owned())
        else {
            return;
        };
        self.file_surface_keys.remove(&(
            panel_key.to_string(),
            owner.clone(),
            old_path.to_string(),
        ));
        self.file_surface_keys
            .entry((panel_key.to_string(), owner, new_path.to_string()))
            .or_insert(id);
        cx.notify();
    }


    fn register_diff_surface(
        &mut self,
        changes: Entity<Changes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_seq += 1;
        let id = self.diff_seq;
        let sub =
            cx.subscribe_in(
                &changes,
                window,
                |this: &mut Self, _, event, window, cx| match event {
                    ChangesEvent::OpenFile(path) => {
                        this.add_file_surface(path.clone(), window, cx);
                    }
                },
            );
        self.diffs.insert(id, changes);
        self.diff_subs.insert(id, sub);
        self.open_view(RightSurface::Diff(id), cx);
    }

    fn on_file_close_ready(
        &mut self,
        surface: RightSurface,
        panel_key: &str,
        cx: &mut Context<Self>,
    ) {
        if self.pending_file_closes.contains(&surface) {
            self.complete_file_close(surface, panel_key, cx);
        } else {
            if self.pending_exit.is_some() {
                self.reveal_unsaved_file(cx);
            }
            cx.notify();
        }
    }

    fn cancel_file_close(&mut self, surface: RightSurface, cx: &mut Context<Self>) {
        self.pending_file_closes.remove(&surface);
        self.pending_exit = None;
        cx.notify();
    }

    pub fn prepare_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        self.prepare_exit(PendingExit::CloseWindow, cx)
    }

    pub fn prepare_quit(&mut self, cx: &mut Context<Self>) -> bool {
        self.prepare_exit(PendingExit::Quit, cx)
    }

    fn prepare_exit(&mut self, action: PendingExit, cx: &mut Context<Self>) -> bool {
        let surfaces = self.file_surfaces.values().cloned().collect::<Vec<_>>();
        if surfaces
            .iter()
            .all(|surface| !surface.read(cx).has_unsaved_changes())
        {
            self.pending_exit = None;
            return true;
        }
        self.pending_exit = Some(action);
        let mut all_ready = true;
        for surface in surfaces {
            let disposition = surface.update(cx, |surface, cx| surface.prepare_close(cx));
            all_ready &= disposition == FilesCloseDisposition::Allow;
        }
        if all_ready {
            self.pending_exit = None;
        } else {
            self.reveal_unsaved_file(cx);
        }
        cx.notify();
        all_ready
    }

    fn reveal_unsaved_file(&mut self, cx: &mut Context<Self>) {
        let editors = self
            .file_surface_keys
            .iter()
            .filter_map(|((key, _, _), id)| {
                self.file_surfaces
                    .get(id)
                    .filter(|files| files.read(cx).has_unsaved_changes())
                    .map(|_| (key.clone(), RightSurface::File(*id)))
            });
        let current = self.panel_key(cx);
        let mut dirty = editors.collect::<Vec<_>>();
        dirty.sort_by_key(|(key, _)| (key != &current, key.clone()));
        if let Some((key, surface)) = dirty.into_iter().next() {
            // Its session on screen, and its pane, kept since it opened, given the keyboard.
            self.panes.entry(key.clone()).or_default().focus(panes::Pane::View(surface));
            self.pane_keyboard_pending = true;
            self.apply_nav(NavEntry::Chat(key), cx);
        }
    }

    fn all_file_edits_flushed(&self, cx: &App) -> bool {
        self.file_surfaces
            .values()
            .all(|surface| !surface.read(cx).has_unsaved_changes())
    }

    fn on_sidebar_drag(
        &mut self,
        event: &gpui::DragMoveEvent<SidebarResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x = f32::from(event.event.position.x);
        let sample = sidebar_drag_sample(x, self.sidebar_resize_edge, self.reduced_motion.get());
        let was_collapsed = self.settings.sidebar_collapsed;
        self.settings.sidebar_width = sample.width;
        self.settings.sidebar_collapsed = false;
        self.pane_resize_dragging = Some(PaneResizeKind::Sidebar);
        self.sidebar_tween = None; // live drag tracks the pointer directly
        if sample.starts_bounce {
            self.sidebar_edge_bounce = sample.edge.map(|edge| motion::ResizeEdgeBounce::new(edge, self.now()));
        } else if sample.edge.is_none() {
            self.sidebar_edge_bounce = None;
        }
        self.pane_resize_active = sample.edge.is_none().then_some(PaneResizeKind::Sidebar);
        self.sidebar_resize_edge = sample.edge;
        self.schedule_save(cx);
        let _ = was_collapsed;
        cx.notify();
    }

    fn finish_pane_resize(&mut self, kind: PaneResizeKind) {
        if self.pane_resize_active == Some(kind) {
            self.pane_resize_active = None;
        }
        if self.pane_resize_dragging == Some(kind) {
            self.pane_resize_dragging = None;
        }
        match kind {
            PaneResizeKind::Sidebar => self.sidebar_resize_edge = None,
        }
    }

    /// Publish this view's working copy to the central settings store. The
    /// store owns the single debounce task and the only production writer.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance = crate::appearance::mode(cx);
        self.settings.theme_selection = crate::appearance::themes(cx);
        self.settings.accent = crate::appearance::accent(cx);
        self.settings.surface = crate::appearance::surface(cx);
        self.pull_settings(cx);
        settings::replace(self.settings.clone(), SavePolicy::Debounced, cx);
        self.settings_base = settings::current(cx);
    }

    /// Controls outside the Shell (Voice, typography, diff and transcript
    /// toggles, …) save to the central store directly. Fold those into the
    /// working copy while keeping the Shell's own edits, so a geometry save
    /// can never publish an older value over another surface's choice.
    fn pull_settings(&mut self, cx: &App) {
        let current = settings::current(cx);
        self.settings =
            UiSettings::merge_changes(&self.settings_base, &self.settings, current.clone());
        self.settings_base = current;
    }

    fn retry_engine(&mut self, cx: &mut Context<Self>) {
        AppState::bootstrap(self.state.clone(), self.boot.clone(), cx);
    }

    // ---- routes / settings ----

    // The app's first frame rises in; each move between Settings and the chats after it fades the
    // new page in, with no slide (a section within Settings does not).
    fn route_outlet(&mut self, frame: gpui::Div) -> AnyElement {
        let settings = matches!(self.route, Route::Settings(_));
        let (was, changes) = self.route_shown.unwrap_or((settings, 0));
        let changes = changes + u64::from(was != settings);
        self.route_shown = Some((settings, changes));
        if changes == 0 {
            motion::fade_in("phase-app", frame).into_any_element()
        } else {
            motion::fade_quick(gpui::ElementId::NamedInteger("route".into(), changes), frame)
                .into_any_element()
        }
    }

    fn set_route(&mut self, route: Route, _cx: &mut Context<Self>) {
        self.command_palette = None;
        self.route = route;
    }

    /// Close the user menu through the exit animation (no-op when closed).
    /// The open chat's unseen result marked seen. Never from a render: a change to
    /// the state while a frame draws reaches none of the views that watch it.
    fn mark_open_chat_seen(&mut self, cx: &mut Context<Self>) {
        let unseen = self.state.read(cx).selected_chat_row().filter(|chat| chat.unseen()).map(|chat| chat.id.clone());
        if let Some(chat_id) = unseen {
            self.state.update(cx, |state, cx| state.mark_chat_seen(&chat_id, cx));
        }
    }

    fn close_user_menu(&mut self, cx: &mut Context<Self>) {
        if self.user_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.user_menu);
            cx.notify();
        }
    }

    /// Close the session-row context menu through the exit animation.
    fn close_chat_menu(&mut self, cx: &mut Context<Self>) {
        self.chat_copy_intent.reset();
        if self.chat_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.chat_menu);
            cx.notify();
        }
    }

    /// A sidebar-level notice (a failed mutation, a copied link) as a toast.
    /// The latest message is kept so a pin-write failure can be matched later.
    pub(super) fn show_notice(
        &mut self,
        kind: crate::toast::ToastKind,
        message: impl Into<SharedString>,
        cx: &mut App,
    ) {
        let message = message.into();
        self.sidebar_notice = Some(message.clone());
        crate::toast::show(cx, kind, message);
    }

    // The words for the sidebar's notice and the toast; the raw reason behind Copy error.
    pub(super) fn show_error_detail(
        &mut self,
        message: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        cx: &mut App,
    ) {
        let message = message.into();
        self.sidebar_notice = Some(message.clone());
        crate::toast::error_with_detail(cx, message, detail);
    }

    fn set_chat_copy_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let page = if open {
            ChatMenuPage::Copy
        } else {
            ChatMenuPage::Root
        };
        if let Some(menu) = self.chat_menu.open_mut()
            && menu.page != page
        {
            menu.page = page;
            if !open {
                self.chat_copy_bounds = None;
            }
            cx.notify();
        }
    }

    /// Pointer intent over a chat-menu row: hovering Copy opens its submenu,
    /// any other row closes it — after a grace while the pointer is heading
    /// into the open submenu.
    fn hover_chat_menu_row(
        &mut self,
        key: u8,
        pointer: Point<Pixels>,
        moved: bool,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .chat_menu
            .as_open()
            .filter(|menu| menu.page == ChatMenuPage::Copy)
            .map(|_| CHAT_MENU_ROW_COPY);
        let (bounds, left) = (self.chat_copy_bounds, self.chat_copy_left);
        let action = if moved {
            self.chat_copy_intent
                .moved(current.as_ref(), &key, pointer, bounds, left)
        } else {
            self.chat_copy_intent
                .enter(current.as_ref(), &key, pointer, bounds, left)
        };
        match action {
            popover::HoverAction::None => {}
            popover::HoverAction::Open => {
                self.set_chat_copy_open(key == CHAT_MENU_ROW_COPY, cx);
                self.chat_copy_intent.record_origin(pointer);
            }
            popover::HoverAction::Defer => {
                self.chat_copy_intent.defer(cx, move |this, cx| {
                    if this.chat_menu.is_open()
                        && this.chat_copy_intent.pending() == Some(&key)
                    {
                        this.chat_copy_intent.cancel();
                        this.set_chat_copy_open(key == CHAT_MENU_ROW_COPY, cx);
                        this.chat_copy_intent.record_origin(pointer);
                    }
                });
            }
        }
    }

    /// Wrap a chat-menu row in the hover surface that feeds
    /// [`Self::hover_chat_menu_row`]. The row keeps its own hover fade.
    fn chat_menu_hover_row(
        &self,
        key: u8,
        row: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(("chat-menu-hover", key as usize))
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                if *hovered {
                    this.hover_chat_menu_row(key, window.mouse_position(), false, cx);
                } else {
                    this.chat_copy_intent.leave(&key);
                }
            }))
            .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                this.hover_chat_menu_row(key, event.position, true, cx);
            }))
            .child(row)
            .into_any_element()
    }

    /// The chat menu's Copy row: opens its submenu beside the menu on hover
    /// (or click), on whichever side has room.
    fn render_chat_copy_trigger(
        &mut self,
        chat_id: &str,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        let exit_entity = entity.clone();
        let mut row = popover::menu_row_nav(theme, open, false, format!("chat-menu-copy-{chat_id}"))
            .id("chat-menu-copy")
            // A named menu item that says whether its submenu is open.
            .role(gpui::Role::MenuItem)
            .aria_label("Copy")
            .aria_expanded(open)
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                this.chat_copy_intent.reset();
                this.set_chat_copy_open(true, cx);
                cx.stop_propagation();
            }))
            .child(icon(icons::COPY).size(px(16.0)).text_color(theme.text_muted))
            .child(div().flex_1().child(SharedString::from("Copy")))
            .child(
                icon(icons::ALT_ARROW_RIGHT)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        let left = bounds.right() + px(244.0) > window.viewport_size().width;
                        let _ = entity.update(cx, |this, cx| {
                            if this.chat_copy_left != left {
                                this.chat_copy_left = left;
                                cx.notify();
                            }
                        });
                    },
                    move |trigger, _, window, _| {
                        if !open {
                            return;
                        }
                        // Leaving the trigger anywhere but toward the submenu
                        // closes it, as in the sidebar options menu.
                        window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                            if phase != gpui::DispatchPhase::Bubble {
                                return;
                            }
                            let _ = exit_entity.update(cx, |this, cx| {
                                let (bounds, left) = (this.chat_copy_bounds, this.chat_copy_left);
                                if this
                                    .chat_copy_intent
                                    .contains_pointer(trigger, bounds, event.position, left)
                                {
                                    return;
                                }
                                this.chat_copy_intent.reset();
                                this.set_chat_copy_open(false, cx);
                            });
                        });
                    },
                )
                .absolute()
                .inset_0(),
            );
        if open {
            let submenu = self.render_chat_copy_submenu(chat_id, theme, cx);
            row = row.child(popover::nested_menu(
                format!("chat-copy-submenu-{chat_id}"),
                submenu,
                self.chat_copy_left,
            ));
        }
        row.into_any_element()
    }

    /// The Copy submenu's card: what can be copied for this chat.
    fn render_chat_copy_submenu(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .cloned();
        let harness_link = chat
            .as_ref()
            .and_then(crate::links::harness_conversation_link);
        let session_id = chat
            .as_ref()
            .and_then(|chat| chat.harness_session_id.as_deref())
            .is_some_and(|id| !id.trim().is_empty());
        let has_path = chat.as_ref().and_then(chat_copy_path).is_some();
        let slopty_id = chat_id.to_owned();
        let harness_id = chat_id.to_owned();
        let session_chat_id = chat_id.to_owned();
        let path_chat_id = chat_id.to_owned();
        let item = |id: &'static str, label: SharedString| {
            popover::menu_row(theme, false, format!("{id}-{chat_id}"))
                .id(id)
                .debug_selector(move || id.to_owned())
                .child(icon(icons::COPY).size(px(16.0)).text_color(theme.text_muted))
                .child(label)
        };
        let entity = cx.entity().downgrade();
        popover::popover_card(theme)
            .w(px(232.0))
            .relative()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .when(has_path, |menu| {
                menu.child(item("chat-copy-path", "Path".into()).on_click(cx.listener(
                    move |this, _, _, cx| this.copy_chat_path(&path_chat_id, cx),
                )))
            })
            .child(
                item("chat-copy-slopty", "Slopty conversation link".into()).on_click(cx.listener(
                    move |this, _, _, cx| this.copy_slopty_conversation_link(&slopty_id, cx),
                )),
            )
            .when_some(harness_link, |menu, link| {
                menu.child(item("chat-copy-harness", link.label.into()).on_click(cx.listener(
                    move |this, _, _, cx| this.copy_harness_conversation_link(&harness_id, cx),
                )))
            })
            .when(session_id, |menu| {
                menu.child(
                    item("chat-copy-session", "Agent session ID".into()).on_click(cx.listener(
                        move |this, _, _, cx| this.copy_harness_session_id(&session_chat_id, cx),
                    )),
                )
            })
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = entity.update(cx, |this, _| this.chat_copy_bounds = Some(bounds));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }

    fn copy_slopty_conversation_link(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let link = {
            let state = self.state.read(cx);
            crate::links::workspace_locator(
                state.workspace_scope,
                state.auth.as_ref(),
                state.local_device_id.as_deref(),
            )
            .map(|workspace| crate::links::slopty_conversation_link(chat_id, &workspace))
        };
        if let Some(link) = link {
            cx.write_to_clipboard(ClipboardItem::new_string(link));
            self.show_notice(crate::toast::ToastKind::Success, "Slopty conversation link copied", cx);
        } else {
            self.show_notice(crate::toast::ToastKind::Error, "Couldn\u{2019}t copy the chat\u{2019}s link yet. Try again in a moment.", cx);
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_harness_conversation_link(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let link = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(crate::links::harness_conversation_link);
        if let Some(link) = link {
            cx.write_to_clipboard(ClipboardItem::new_string(link.url));
            self.show_notice(crate::toast::ToastKind::Success, format!("{} copied", link.label), cx);
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_harness_session_id(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.harness_session_id.clone());
        if let Some(id) = id.filter(|id| !id.trim().is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(id));
            self.show_notice(crate::toast::ToastKind::Success, "Agent session ID copied", cx);
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_chat_path(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let path = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(chat_copy_path)
            .map(str::to_owned);
        if let Some(path) = path {
            cx.write_to_clipboard(ClipboardItem::new_string(path));
            self.show_notice(crate::toast::ToastKind::Success, "Path copied", cx);
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn ensure_appearance_page(&mut self, cx: &mut Context<Self>) {
        if self.appearance_page.is_none() {
            let page = cx.new(AppearancePage::new);
            self.appearance_settings_sub = Some(cx.subscribe(
                &page,
                |this: &mut Shell, _, event: &AppearanceSettingsEvent, cx| match *event {
                    AppearanceSettingsEvent::CodeFontSizeChanged(size) => {
                        this.set_code_font_size(size, cx);
                    }
                },
            ));
            self.appearance_page = Some(page);
        }
    }

    pub(crate) fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.command_palette = None;
        // Recreate per visit: the page's ListHarnesses load re-probes which
        // CLIs are installed, so installing one shows up on the next open.
        if section == SettingsSection::Harnesses {
            self.harnesses_page = None;
        }
        if !matches!(self.route, Route::Settings(_)) {
            self.settings_focus_pending = true;
            self.settings_return_route = self.route;
        }
        self.set_route(Route::Settings(section), cx);
        self.remember_settings_section(section, cx);
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    // The first run's way in: Settings → Devices with its Add device dialog open.
    fn open_add_device(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Devices, cx);
        let state = self.state.clone();
        let page = self.devices_page.get_or_insert_with(|| cx.new(|cx| DevicesPage::new(state, cx)));
        page.update(cx, |page, cx| page.open_add_device(cx));
    }

    // The app's menu (Add a machine, Connect to a server): Settings → Devices with Add
    // device open on that choice.
    pub fn open_add_device_on(
        &mut self,
        choice: crate::settings::add_device::Choice,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(SettingsSection::Devices, cx);
        let state = self.state.clone();
        let page = self.devices_page.get_or_insert_with(|| cx.new(|cx| DevicesPage::new(state, cx)));
        page.update(cx, |page, cx| page.open_add_device_on(choice, cx));
    }

    // A link that names a server: Add device on "A server", the address filled in.
    pub fn open_add_device_with_server(&mut self, address: String, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Devices, cx);
        let state = self.state.clone();
        let page = self.devices_page.get_or_insert_with(|| cx.new(|cx| DevicesPage::new(state, cx)));
        page.update(cx, |page, cx| page.open_add_device_with_server(address, cx));
    }

    // The app's menu (Settings…): Settings where they were left.
    pub fn open_settings_from_menu(&mut self, cx: &mut Context<Self>) {
        self.open_last_settings(cx);
    }

    /// Generic entry points (⌘, / Ctrl+,, the footer gear, the palette,
    /// `/settings`) reopen the section last viewed. Links that name a
    /// section go through [`Self::open_settings`] instead.
    fn open_last_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings(self.settings.settings_section, cx);
    }

    /// Every section shown is the one to reopen. Written through the shell's
    /// own settings copy: [`Self::schedule_save`] replaces the whole file
    /// from it, so a write that bypassed it would be undone by the next save.
    fn remember_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        if self.settings.settings_section != section {
            self.settings.settings_section = section;
            self.schedule_save(cx);
        }
    }

    /// Escape bubbling out of the settings page. Focused controls — open
    /// dropdowns, the shortcut recorder, a dialog's own input — consume it
    /// before it gets here. What remains open without holding focus closes
    /// first: the account menu, a sync prompt, then the routed page's dialog
    /// or login flow. Returns whether one did; only then does Settings stay.
    fn dismiss_settings_escape_surface(&mut self, cx: &mut Context<Self>) -> bool {
        if self.user_menu.is_open() {
            self.close_user_menu(cx);
            return true;
        }
        if self.user_menu.get().is_some() {
            return true;
        }
        let Route::Settings(section) = self.route else {
            return false;
        };
        // Appearance's one dialog (the background adjustment) takes its own Escape; Archived has
        // none.
        section == SettingsSection::Devices
            && self
                .devices_page
                .as_ref()
                .is_some_and(|page| page.update(cx, |page, cx| page.dismiss_on_escape(cx)))
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_focus_pending = false;
        self.settings_restore_pending = true;
        let route = self.settings_return_route;
        self.set_route(route, cx);
        cx.notify();
    }

    fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Settings(_)) {
            self.close_settings(cx);
        } else {
            self.open_last_settings(cx);
        }
    }

    // ---- back/forward (route history) ----

    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.back() {
            self.apply_nav(entry, cx);
        }
    }

    fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.forward() {
            self.apply_nav(entry, cx);
        }
    }

    /// Land on a history entry WITHOUT recording a new one: the stack already
    /// points at `entry` (back/forward moved the index); the selection change
    /// this triggers dedups against `current()` in [`Self::on_state_changed`].
    fn apply_nav(&mut self, entry: NavEntry, cx: &mut Context<Self>) {
        self.suspend_file_images(cx);
        match entry {
            NavEntry::Chat(chat_id) => {
                self.set_route(Route::Chat, cx);
                self.focus_composer_on_landing(cx); // not on touch
                let target = (!chat_id.is_empty()).then_some(chat_id);
                // A chat archived or deleted since it was left is not opened again as
                // an empty page: the way back lands on the new chat's canvas instead.
                let target = target.filter(|id| self.state.read(cx).chat_listed(id));
                if self.state.read(cx).selected_chat != target {
                    self.state.update(cx, |s, cx| s.select_chat(target, cx));
                }
            }
            NavEntry::Settings(section) => {
                if !matches!(self.route, Route::Settings(_)) {
                    self.settings_return_route = self.route;
                }
                self.set_route(Route::Settings(section), cx);
                self.remember_settings_section(section, cx);
            }
        }
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Lazily create the entity for a settings section and return it renderable.
    fn settings_outlet(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match section {
            SettingsSection::Devices => {
                if self.devices_page.is_none() {
                    let state = self.state.clone();
                    self.devices_page = Some(cx.new(|cx| DevicesPage::new(state, cx)));
                }
                match &self.devices_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Harnesses => {
                if self.harnesses_page.is_none() {
                    let state = self.state.clone();
                    self.harnesses_page = Some(cx.new(|cx| HarnessesPage::new(state, cx)));
                }
                match &self.harnesses_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Appearance => {
                self.ensure_appearance_page(cx);
                match &self.appearance_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Files => {
                if self.files_settings_page.is_none() {
                    let page = cx.new(|cx| {
                        FilesSettingsPage::new(
                            self.settings.files_autosave_enabled,
                            self.settings.files_autosave_delay_ms,
                            self.settings.files_word_wrap,
                            self.settings.files_show_all,
                            cx,
                        )
                    });
                    self.files_settings_sub = Some(cx.subscribe_in(
                        &page,
                        window,
                        |this: &mut Shell, _, event: &FilesSettingsEvent, window, cx| match *event {
                            FilesSettingsEvent::AutosaveChanged(autosave_enabled) => {
                                this.settings.files_autosave_enabled = autosave_enabled;
                                for surface in
                                    this.files.values().chain(this.file_surfaces.values())
                                {
                                    surface.update(cx, |surface, cx| {
                                        surface.set_autosave_enabled(autosave_enabled, cx)
                                    });
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            FilesSettingsEvent::AutosaveDelayChanged(autosave_delay_ms) => {
                                this.settings.files_autosave_delay_ms = autosave_delay_ms;
                                for surface in
                                    this.files.values().chain(this.file_surfaces.values())
                                {
                                    surface.update(cx, |surface, cx| {
                                        surface.set_autosave_delay_ms(autosave_delay_ms, cx)
                                    });
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            FilesSettingsEvent::WordWrapChanged(word_wrap) => {
                                this.set_files_word_wrap(word_wrap, window, cx);
                            }
                            FilesSettingsEvent::ShowAllFilesChanged(show_all_files) => {
                                this.set_files_show_all(show_all_files, cx);
                            }
                        },
                    ));
                    self.files_settings_page = Some(page);
                }
                match &self.files_settings_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Notifications => {
                if self.notifications_page.is_none() {
                    let page = cx.new(|cx| {
                        NotificationsPage::new(self.settings.notifications_enabled, cx)
                    });
                    // Keep the one switch whenever the page flips it.
                    self.notifications_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &NotificationsEvent, cx| {
                            let NotificationsEvent::Changed { desktop } = *event;
                            this.settings.notifications_enabled = desktop;
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.notifications_page = Some(page);
                }
                match &self.notifications_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Shortcuts => {
                if self.shortcuts_page.is_none() {
                    let state = self.state.clone();
                    let keymap = self.settings.keymap.clone();
                    let page = cx.new(|cx| ShortcutsPage::new(state, keymap, cx));
                    // Persist + re-apply shortcut preferences whenever the page changes them.
                    self.shortcuts_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &ShortcutsEvent, cx| {
                            let ShortcutsEvent::KeymapChanged(keymap) = event;
                            this.settings.keymap = keymap.clone();
                            apply_keymap(cx, &this.settings.keymap);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.shortcuts_page = Some(page);
                }
                match &self.shortcuts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Archived => {
                if self.archived_page.is_none() {
                    let state = self.state.clone();
                    self.archived_page = Some(cx.new(|cx| ArchivedPage::new(state, cx)));
                }
                match &self.archived_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
        }
    }

    // ---- sidebar mutations ----

    /// Fire a Mutate op; failures surface in the sidebar notice strip.
    fn mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.show_notice(crate::toast::ToastKind::Error, "Couldn\u{2019}t reach the Slopty engine. Wait for it to connect, then try again.", cx);
            cx.notify();
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                this.update(cx, |shell, cx| {
                    shell.show_notice(crate::toast::ToastKind::Error, format!("{err}"), cx);
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// Start editing `chat_id`'s title in place on its sidebar row.
    fn open_rename_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        if self.chat_rename.is_some() {
            self.finish_rename_chat(true, cx);
        }
        let Some(current) = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .map(|c| c.title.clone().unwrap_or_default())
        else {
            return;
        };
        // TUI-first: no side chats, so a title is edited only on its sidebar row, which
        // must be on screen first: a field on a hidden row would take keystrokes unseen. The
        // sidebar explains its own refusals.
        if !self.reveal_sidebar_chat(&chat_id, cx) {
            cx.notify();
            return;
        }
        let input = cx.new(|cx| {
            ComposerInput::new("Chat title", cx)
                .with_single_line()
                .with_text_metrics(13.0, 17.0)
                .with_accessibility_role(gpui::Role::TextInput)
        });
        input.update(cx, |input, cx| {
            input.set_text(current, cx);
            input.select_all_text(cx);
        });
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.finish_rename_chat(true, cx);
            }
        });
        self.chat_rename = Some(ChatRename {
            chat_id,
            input,
            focus_pending: true,
            reveal_until: Some(
                crate::clock::now(cx) + motion::COLLAPSE.total() + CHAT_RENAME_REVEAL_GRACE,
            ),
            _events: events,
            _blur: None,
        });
        cx.notify();
    }

    /// Focus a just-opened inline rename and arm its commit-on-blur.
    fn focus_rename_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let revealing = self.chat_rename_revealing();
        let Some(rename) = self.chat_rename.as_mut() else {
            return;
        };
        if !rename.focus_pending {
            return;
        }
        // The claim waits for a frame that drew the field, asking again after each frame
        // while the row is being revealed. A field a card is still opening onto (a side chat
        // paged out of a closed card) is in no frame yet: focused then, the frame's focus
        // recovery hands the keyboard to the composer without the field ever losing it, so the
        // rename stood open with no keyboard.
        if revealing && rename.input.read(cx).drawn_bounds().is_none() {
            let shell = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                shell.update(cx, |shell, cx| shell.focus_rename_chat(window, cx)).ok();
            });
            return;
        }
        rename.focus_pending = false;
        let focus = rename.input.focus_handle(cx);
        window.focus(&focus, cx);
        rename._blur = Some(cx.on_blur(&focus, window, |this, _, cx| {
            this.finish_rename_chat(true, cx);
        }));
    }

    /// Close the inline rename, saving a changed, non-empty title when
    /// `commit` (Enter or blur) and dropping it otherwise (Escape).
    fn finish_rename_chat(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.chat_rename.take() else {
            return;
        };
        let title = rename.input.read(cx).text().trim().to_string();
        let unchanged = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == rename.chat_id)
            .is_none_or(|c| c.title.as_deref() == Some(title.as_str()));
        if commit && !title.is_empty() && !unchanged {
            self.mutate(
                serde_json::json!({ "op": "renameChat", "chatId": rename.chat_id, "title": title }),
                cx,
            );
        }
        cx.notify();
    }

    fn chat_rename_revealing(&self) -> bool {
        self.chat_rename
            .as_ref()
            .and_then(|rename| rename.reveal_until)
            .is_some_and(|until| self.now() < until)
    }

    fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.set_chat_archived(chat_id, true, cx);
    }

    fn active_profile_key(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        workspace_profile_key(
            state.workspace_scope,
            state.auth.as_ref(),
            None,
        )
    }

    /// The Archive session shortcut. With no chat open, or with an already
    /// archived one, it does nothing — the shortcut archives, it never
    /// unarchives.
    fn archive_selected_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self
            .state
            .read(cx)
            .archivable_selected_chat()
            .map(str::to_string)
        else {
            return;
        };
        self.archive_chat(chat_id, cx);
    }

    pub(super) fn set_chat_archived(
        &mut self,
        chat_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatArchived", "chatId": chat_id, "archived": archived }),
            cx,
        );
        // An archive can be undone from its toast, or with ⌘Z while it is up.
        if archived {
            self.offer_undo_archive(chat_id, cx);
        }
        cx.notify();
    }

    /// A jump shortcut: open the sidebar row at `slot`. A slot past the end of
    /// a short list does nothing. Reads the DISPLAYED order — sort and
    /// grouping view options permute the list, and the chip on a row must
    /// name the key that opens it.
    fn jump_to_session(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(chat_id) = self.sidebar_visible_order(cx).into_iter().nth(slot) else {
            return;
        };
        // Same path a click on that row takes.
        self.open_chat(chat_id, cx);
    }

    /// Whether an overlay that owns the keyboard is up — the command palette, the add-space
    /// palette, the session picker or New session. Session-nav shortcuts (cycle/jump/archive)
    /// go quiet underneath one: gpui runs a matched binding before any `on_key_down`, so an
    /// unguarded jump would switch sessions UNDER the open overlay, stranding it over a session
    /// the user never picked. The Settings page is not one: navigating leaves it, and its shortcut
    /// recorder intercepts the keys it records before they can dispatch.
    pub(super) fn overlay_owns_keyboard(&self) -> bool {
        self.command_palette.is_some()
            || self.add_space.is_some()
            || self.picker_open() // the session picker or Go to Directory
            || self.inbox.is_open()
            || self.new_session.is_some()
    }

    /// Track held modifiers for sidebar jump hints and the queue's submit hint.
    /// Only a visibility change repaints; modifier traffic is otherwise constant.
    fn on_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        self.update_jump_hints(&event.modifiers, cx);
    }

    fn update_jump_hints(&mut self, mods: &gpui::Modifiers, cx: &mut Context<Self>) {
        let primary = if crate::keys::MOD_IS_COMMAND { // an iPad's ⌘ too
            mods.platform
        } else {
            mods.control
        };
        // No hints while an overlay owns the keyboard — the jumps they
        // advertise are suppressed there.
        let visible = matches!(self.route, Route::Chat)
            && !self.overlay_owns_keyboard()
            && jump_hints_visible(&self.settings.keymap, primary, mods.alt, mods.shift);
        self.set_jump_hints(visible, cx);
    }

    pub(super) fn set_jump_hints(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.jump_hints != visible {
            self.jump_hints = visible;
            cx.notify();
        }
    }


    // ---- org gate ----

    // ---- render pieces ----

    fn motion_time(&self) -> std::time::Instant {
        // The dispatcher's clock, which a frame's moment is read from and a test moves.
        self.executor.now()
    }

    /// The moment the shell's motion stands at: the frame's while one is drawn, the
    /// dispatcher's clock between frames.
    pub(super) fn now(&self) -> std::time::Instant {
        self.render_time.unwrap_or_else(|| self.motion_time())
    }

    /// [`Self::motion_time`] while a frame is drawn: the frame's moment.
    fn frame_motion_time(&self, cx: &App) -> std::time::Instant {
        cx.frame_time()
    }

    fn panel_tween(&self, from: f32, to: f32) -> WidthTween {
        WidthTween::panel(from, to, self.now())
    }

    fn tween_elapsed(&self, started: std::time::Instant) -> Duration {
        self.render_time
            .unwrap_or_else(|| self.motion_time())
            .saturating_duration_since(started)
    }

    /// Evaluate a width tween at the frame time (see [`WidthTween`]).
    /// Mid-flight: lerp on the tween's curve, flagging `motion_active` so
    /// render schedules the next frame. Finished, stale, absent, or under
    /// reduced motion: exactly `target`. Honors `SLOPTY_MOTION_SCALE`.
    fn eval_tween(&self, tween: Option<WidthTween>, target: f32) -> f32 {
        let Some(WidthTween {
            from,
            to,
            started,
            duration,
            curve,
        }) = tween
        else {
            return target;
        };
        if self.reduced_motion.get() {
            return target;
        }
        let raw = self.tween_elapsed(started).as_secs_f32() / duration.as_secs_f32();
        if raw >= 1.0 {
            return target;
        }
        self.motion_active.set(true);
        motion::lerp(from, to, curve.eval(raw))
    }

    /// [`Self::eval_tween`] heading for `target` as it is this frame, not
    /// the endpoint the tween saw when it started. A column's target follows
    /// its neighbours while they move; easing toward it lands there instead
    /// of snapping to it as the tween ends.
    fn ease_toward(&self, tween: Option<WidthTween>, target: f32) -> f32 {
        self.eval_tween(
            tween.map(|tween| WidthTween {
                to: target,
                ..tween
            }),
            target,
        )
    }

    fn eval_resize_edge_bounce(
        &self,
        bounce: Option<motion::ResizeEdgeBounce>,
        enabled: bool,
    ) -> f32 {
        let Some(bounce) = bounce else {
            return 0.0;
        };
        if self.reduced_motion.get() || !enabled {
            return 0.0;
        }
        let total =
            Duration::from_millis(motion::RESIZE_EDGE_BOUNCE_MS).mul_f32(motion::speed_scale());
        let raw = self.tween_elapsed(bounce.started).as_secs_f32() / total.as_secs_f32();
        if raw >= 1.0 {
            return 0.0;
        }
        self.motion_active.set(true);
        motion::resize_bounce_offset(bounce.edge, raw)
    }

    pub(super) fn sidebar_now(&self) -> f32 {
        let fit = self.horizontal_fit();
        if !fit.sidebar {
            return self.fit_exit_width().unwrap_or(0.0);
        }
        let limit = self.sidebar_tween_limit(fit.sidebar_limit);
        // The edge bounce applies past the limit so the drag cue at the
        // maximum width stays visible.
        (self
            .ease_toward(self.sidebar_tween, self.sidebar_target())
            .min(limit)
            + self.eval_resize_edge_bounce(
                self.sidebar_edge_bounce,
                !self.settings.sidebar_collapsed,
            ))
        .max(0.0)
    }

    fn tween_active(&self, tween: Option<WidthTween>) -> bool {
        tween.is_some_and(|tween| {
            !self.reduced_motion.get() && self.tween_elapsed(tween.started) < tween.duration
        })
    }

    fn active_tween_endpoints(&self, tween: Option<WidthTween>) -> Option<(f32, f32)> {
        tween
            .filter(|transition| {
                !self.reduced_motion.get() && self.tween_elapsed(transition.started) < transition.duration
            })
            .map(|transition| (transition.from, transition.to))
    }

    /// The animated spacer clearing the macOS traffic lights ahead of a
    /// titlebar control cluster. Fullscreen toggles tween the cluster start
    /// over 200ms ease-out ([`RESIZE`]; reduced motion snaps).
    /// `None` off macOS — no phantom flex child.
    fn titlebar_spacer(&self, container_pad: f32) -> Option<AnyElement> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let fullscreen = self.fullscreen.unwrap_or(false);
        // The tween runs in cluster-start coordinates; the spacer is that
        // minus the container's own padding.
        let start = self.eval_tween(self.titlebar_tween, titlebar_cluster_start(fullscreen));
        let width = (start - container_pad).max(0.0);
        Some(div().flex_none().h_full().w(px(width)).into_any_element())
    }

    /// The header's content row with the animated left inset: on sidebar toggles (and macOS
    /// fullscreen flips) the SAME element's padding tweens over 200 ms ease-out, so the title
    /// glides to its new x-position. Route changes SNAP: every route transition kills the tween
    /// (an instant swap, no horizontal motion).
    /// Where unified-titlebar content (tabs / the settings label) starts: past
    /// the traffic lights + control cluster, riding the fullscreen inset tween.
    pub(super) fn title_bar_content_start(&self) -> f32 {
        let fullscreen = self.fullscreen.unwrap_or(false);
        let is_macos = cfg!(target_os = "macos");
        let cluster = self.eval_tween(
            self.titlebar_tween,
            cluster_buttons_start(is_macos, fullscreen, self.linux_left_caption_count()),
        );
        cluster + CLUSTER_BUTTONS_WIDTH + TITLEBAR_IDENTITY_GAP
    }

    /// Make a titlebar strip drag the window — zed's platform-titlebar
    /// pattern: mark it a [`WindowControlArea::Drag`]
    /// (macOS app-owned titlebar), hand the drag to the compositor once the
    /// pointer moves with the button down, and double-click zooms.
    fn titlebar_drag_region(
        &self,
        id: &'static str,
        el: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.id(id)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar_should_move = false))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = false),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = true),
            )
            // Hand the drag to the compositor only while the button is
            // actually held (`pressed_button` guard): on macOS
            // `start_window_move` runs AppKit's NATIVE drag session
            // (`performWindowDragWithEvent:`), and AppKit resolves a quick
            // second click inside that session as a titlebar double-click —
            // system zoom — natively, beyond gpui's reach. Without the guard a
            // stale `titlebar_should_move` (armed by a down whose bubble was
            // later stopped) would start that session from a mere hover move
            // between the two clicks of a double-click.
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                    if this.titlebar_should_move && event.pressed_button == Some(MouseButton::Left)
                    {
                        this.titlebar_should_move = false;
                        window.start_window_move();
                    }
                }),
            )
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    if cfg!(target_os = "macos") {
                        // Native titlebar double-click action (zoom/minimize
                        // per system preference).
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                }
            })
    }

    /// The top-left window-control cluster: past the traffic lights, the sidebar's toggle, alone
    /// (`docs/decisions/layout.md`, "One row of chrome"). Rendered once, in a paint-only overlay
    /// layer pinned at the window's top-left, ABOVE the sidebar and the titlebar row, so the
    /// sidebar's width animates beneath it and the toggle never moves or remounts; only the
    /// fullscreen traffic-light inset tweens (the animated spacer). The container has no
    /// listeners: everything around the toggle falls through to the titlebar drag strip below.
    /// Over the sidebar it wears the window's ink; with the sidebar put away it stands on the
    /// session's ground and is inked for it, as the row is.
    fn render_titlebar_cluster(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let over_ground = self.sidebar_now() < self.title_bar_content_start()
            && self.state.read(cx).selected_chat_row().is_some();
        let ink = if over_ground { tabs::ink_for(self.shown_ground(cx), &theme) } else { theme };
        div()
            .absolute()
            .top_0()
            .left_0()
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .pt(px(titlebar_top_pad())) // a finger's presses inside the window
            .px(px(TITLEBAR_CLUSTER_PAD))
            .children(self.titlebar_spacer(TITLEBAR_CLUSTER_PAD))
            // Left-side Linux captions (GNOME `close:…` layouts): the
            // root-level caption overlay owns the buttons; the cluster row
            // just starts past them, at the shared 2px rhythm.
            .children((self.linux_left_caption_count() > 0).then(|| {
                div()
                    .flex_none()
                    .h_full()
                    .w(px(caption_buttons_width(self.linux_left_caption_count())))
            }))
            .child(window_control_button_with(
                "toggle-sidebar",
                icons::sidebar_glyph(
                    motion::state_t(
                        "toggle-sidebar",
                        !self.settings.sidebar_collapsed,
                        motion::GLYPH_STATE,
                        self.reduced_motion.get(),
                        cx.frame_time(),
                    ),
                    false,
                    14.0, // a 24 control holds a 14 glyph
                    ink.text_muted,
                ),
                &ink,
                cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)),
            )
            // What it does now, and its key.
            .action_hint(
                if self.settings.sidebar_collapsed { "Show sidebar" } else { "Hide sidebar" },
                Keys::Action(Box::new(ToggleSidebar)),
                cx,
            ))
            .into_any_element()
    }

    /// Native Windows caption controls integrated into Slopty's unified
    /// titlebar. `WindowControlArea` maps these hit targets to HTMINBUTTON,
    /// HTMAXBUTTON, and HTCLOSE, so Windows owns their behavior (including
    /// Snap Layouts) while GPUI renders the system Segoe caption glyphs.
    fn render_windows_caption_controls(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        if !cfg!(target_os = "windows") {
            return None;
        }

        let theme = Theme::of(cx);
        let (maximize_id, maximize_glyph) = if window.is_maximized() {
            ("window-restore", "\u{e923}")
        } else {
            ("window-maximize", "\u{e922}")
        };
        Some(
            div()
                .id("windows-window-controls")
                .absolute()
                .top_0()
                .right_0()
                .h(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_row()
                .font_family("Segoe Fluent Icons")
                .child(windows_caption_button(
                    "window-minimize",
                    "\u{e921}",
                    WindowControlArea::Min,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    maximize_id,
                    maximize_glyph,
                    WindowControlArea::Max,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    "window-close",
                    "\u{e8bb}",
                    WindowControlArea::Close,
                    theme,
                    true,
                ))
                .into_any_element(),
        )
    }

    /// Which caption buttons Slopty itself must draw on Linux: under
    /// client-side decorations (the Wayland default) nobody else will —
    /// without these the window has NO minimize/maximize/close at all.
    /// Server-side decorations (X11 WMs, KDE with SSD) already draw real
    /// buttons, so `None` there. The desktop's layout (GNOME's
    /// `button-layout` gsetting via `cx.button_layout()`) decides side and
    /// order — min/max/close on the right by default; controls the
    /// compositor can't do (e.g. minimize on some Wayland compositors) drop
    /// out, close always stays.
    #[cfg(target_os = "linux")]
    fn resolve_linux_captions(window: &Window, cx: &App) -> Option<gpui::WindowButtonLayout> {
        use gpui::{MAX_BUTTONS_PER_SIDE, WindowButton, WindowButtonLayout};
        if !matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ) {
            return None;
        }
        let layout = cx
            .button_layout()
            .unwrap_or_else(WindowButtonLayout::linux_default);
        let supported = window.window_controls();
        let filter_side = |side: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE]| {
            let mut out = [None; MAX_BUTTONS_PER_SIDE];
            let mut i = 0;
            for button in side.into_iter().flatten() {
                let keep = match button {
                    WindowButton::Minimize => supported.minimize,
                    WindowButton::Maximize => supported.maximize,
                    WindowButton::Close => true,
                };
                if keep {
                    out[i] = Some(button);
                    i += 1;
                }
            }
            out
        };
        let layout = WindowButtonLayout {
            left: filter_side(layout.left),
            right: filter_side(layout.right),
        };
        (layout.left[0].is_some() || layout.right[0].is_some()).then_some(layout)
    }

    #[cfg(not(target_os = "linux"))]
    fn resolve_linux_captions(_window: &Window, _cx: &App) -> Option<gpui::WindowButtonLayout> {
        None
    }

    pub(super) fn linux_left_caption_count(&self) -> usize {
        self.linux_captions
            .map_or(0, |l| l.left.iter().flatten().count())
    }

    pub(super) fn linux_right_caption_count(&self) -> usize {
        self.linux_captions
            .map_or(0, |l| l.right.iter().flatten().count())
    }

    /// Right padding titlebar content needs to clear the platform's caption
    /// controls (native Windows cluster / Slopty-drawn Linux buttons).
    pub(super) fn titlebar_right_pad(&self, base: f32) -> f32 {
        titlebar_right_padding(
            cfg!(target_os = "windows"),
            self.linux_right_caption_count(),
            base,
        )
    }

    /// Slopty-drawn Linux caption controls, one overlay per populated side.
    /// Shell-level chrome like the Windows cluster: mounted at the root so
    /// they stay above the splash and every auth/org/error gate.
    fn render_linux_caption_controls(&self, window: &Window, cx: &App) -> Vec<AnyElement> {
        let Some(layout) = self.linux_captions else {
            return Vec::new();
        };
        let theme = Theme::of(cx);
        let is_maximized = window.is_maximized();
        // Ids can be per-button (not per-side): the layout parser dedups, so
        // a button never appears on both sides at once.
        let strip = |buttons: &[Option<gpui::WindowButton>]| {
            div()
                .absolute()
                .top_0()
                .h(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .pt(px(Theme::TITLEBAR_TOP_PAD))
                .gap(px(2.0))
                .px(px(10.0))
                .children(buttons.iter().flatten().map(|button| {
                    match button {
                        gpui::WindowButton::Minimize => linux_caption_button(
                            "window-minimize",
                            "Minimize",
                            icons::WINDOW_MINIMIZE,
                            false,
                            theme,
                            |_, window, _| window.minimize_window(),
                        )
                        .into_any_element(),
                        gpui::WindowButton::Maximize => {
                            let (id, label, icon_path) = if is_maximized {
                                ("window-restore", "Restore", icons::WINDOW_RESTORE)
                            } else {
                                ("window-maximize", "Maximize", icons::WINDOW_MAXIMIZE)
                            };
                            linux_caption_button(
                                id,
                                label,
                                icon_path,
                                false,
                                theme,
                                |_, window, _| window.zoom_window(),
                            )
                            .into_any_element()
                        }
                        gpui::WindowButton::Close => linux_caption_button(
                            "window-close",
                            "Close",
                            icons::CLOSE,
                            true,
                            theme,
                            |_, window, _| window.remove_window(),
                        )
                        .into_any_element(),
                    }
                }))
        };
        let mut out = Vec::new();
        if layout.left[0].is_some() {
            out.push(strip(&layout.left).left_0().into_any_element());
        }
        if layout.right[0].is_some() {
            out.push(strip(&layout.right).right_0().into_any_element());
        }
        out
    }

    /// Linux CSD chrome state: which edges the compositor has taken (tiled or
    /// maximized). A free edge can carry a resize strip; a fully floating
    /// window (nothing tiled) also gets rounded corners. Server decorations
    /// (e.g. KDE SSD) hand the frame back to the compositor — no CSD chrome.
    #[cfg(target_os = "linux")]
    fn linux_csd_edges(window: &Window) -> (bool, bool, bool, bool) {
        match window.window_decorations() {
            gpui::Decorations::Client { tiling } => {
                (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right)
            }
            gpui::Decorations::Server => (false, false, false, false),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn linux_csd_edges(_window: &Window) -> (bool, bool, bool, bool) {
        (false, false, false, false)
    }

    /// True when the Linux window floats (CSD, nothing tiled or maximized) —
    /// the state that gets macOS-style rounded corners.
    fn linux_window_floating(window: &Window) -> bool {
        let (top, bottom, left, right) = Self::linux_csd_edges(window);
        top && bottom && left && right && cfg!(target_os = "linux")
    }

    /// The corner radius chrome layers must PAINT for this frame. gpui's
    /// content masks are rectangles — `.rounded()` never clips children, so
    /// every full-bleed layer that can occupy a window corner rounds its own
    /// background with this radius (the layer beneath shows through the
    /// curve, down to the transparent window corners). Zero everywhere the
    /// window must be square: macOS rounds via the window server, and a
    /// tiled/maximized Linux window sits flush with the screen.
    pub(crate) fn window_corner_radius(window: &Window) -> f32 {
        if Self::linux_window_floating(window) {
            LINUX_WINDOW_CORNER_RADIUS
        } else {
            0.0
        }
    }

    /// Invisible edge strips that turn pointer presses into compositor
    /// resizes — the CSD contract on X11 and Wayland, where the platform
    /// draws no frame for us. Each strip mounts only along a free edge (a
    /// maximized or snapped window exposes just its untiled edges, like
    /// GTK). The strips paint last so they sit above all content chrome.
    fn render_linux_resize_borders(window: &Window) -> Vec<AnyElement> {
        let (top, bottom, left, right) = Self::linux_csd_edges(window);
        const EDGE: f32 = 6.0;
        const CORNER: f32 = 14.0;
        let mut out = Vec::new();
        macro_rules! strip {
            ($position:expr, $cursor:ident, $edge:expr) => {
                out.push(
                    $position
                        .$cursor()
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            cx.stop_propagation();
                            window.start_window_resize($edge);
                        })
                        .into_any_element(),
                );
            };
        }
        // Cardinal edges: full-length strips inset by the corner squares.
        if top {
            strip!(
                div()
                    .absolute()
                    .top_0()
                    .left(px(CORNER))
                    .right(px(CORNER))
                    .h(px(EDGE)),
                cursor_ns_resize,
                gpui::ResizeEdge::Top
            );
        }
        if bottom {
            strip!(
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(CORNER))
                    .right(px(CORNER))
                    .h(px(EDGE)),
                cursor_ns_resize,
                gpui::ResizeEdge::Bottom
            );
        }
        if left {
            strip!(
                div()
                    .absolute()
                    .left_0()
                    .top(px(CORNER))
                    .bottom(px(CORNER))
                    .w(px(EDGE)),
                cursor_ew_resize,
                gpui::ResizeEdge::Left
            );
        }
        if right {
            strip!(
                div()
                    .absolute()
                    .right_0()
                    .top(px(CORNER))
                    .bottom(px(CORNER))
                    .w(px(EDGE)),
                cursor_ew_resize,
                gpui::ResizeEdge::Right
            );
        }
        // Corners: squares over both adjacent edges, diagonal cursors.
        if top && left {
            strip!(
                div().absolute().top_0().left_0().size(px(CORNER)),
                cursor_nwse_resize,
                gpui::ResizeEdge::TopLeft
            );
        }
        if top && right {
            strip!(
                div().absolute().top_0().right_0().size(px(CORNER)),
                cursor_nesw_resize,
                gpui::ResizeEdge::TopRight
            );
        }
        if bottom && left {
            strip!(
                div().absolute().bottom_0().left_0().size(px(CORNER)),
                cursor_nesw_resize,
                gpui::ResizeEdge::BottomLeft
            );
        }
        if bottom && right {
            strip!(
                div().absolute().bottom_0().right_0().size(px(CORNER)),
                cursor_nwse_resize,
                gpui::ResizeEdge::BottomRight
            );
        }
        out
    }

    /// The sidebar column's tone, spanning the FULL window height (under the
    /// traffic lights, through the titlebar, down to the bottom edge), with a
    /// hairline on its right edge so the column reads as its own surface.
    /// The tone carries the window's left corners when the CSD window floats
    /// — with one caveat: a corner radius is clamped to the element's own
    /// size, and the COLLAPSED sidebar is a ~1px border sliver (the grab
    /// affordance). macOS trims that hairline with the window server's native
    /// corner clip; we reproduce the same trim by insetting the sliver
    /// vertically to where the curve begins, so its tips never float over the
    /// transparent corner cutouts.
    fn sidebar_tone(width: f32, border_color: gpui::Hsla, window: &Window) -> gpui::Div {
        let window_corner = Self::window_corner_radius(window);
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(width))
            .when(window_corner > 0.0, |el| {
                if width >= 2.0 * window_corner {
                    el.rounded_tl(px(window_corner))
                        .rounded_bl(px(window_corner))
                } else {
                    el.top(px(window_corner)).bottom(px(window_corner))
                }
            })
            .bg(crate::theme::wash(0.05))
            .border_r_1()
            .border_color(border_color)
    }

    /// Settings takes over the window: the section list stands where the
    /// chat sidebar was, the page fills the rest. Opening a preference never
    /// adds a route-history entry or changes the selected conversation.
    fn render_settings_page(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).for_settings_surface();
        if self.settings_focus_pending {
            self.settings_focus_pending = false;
            self.settings_return_focus = window.focused(cx);
            // Keep keyboard dispatch in settings without selecting a control.
            // Tab remains the explicit way to enter its navigation or content.
            window.focus(&self.settings_focus, cx);
        }
        let sidebar_width = self.settings.sidebar_width;
        // Dropdowns contain themselves to the page pane beside this column.
        settings::widgets::set_sidebar_width(sidebar_width, cx);
        let nav = self.render_settings_nav(section, &theme, window, cx);
        // The footer uses the chat sidebar's theme so the account and
        // settings buttons render identically in both places.
        let footer_theme = Theme::of(cx).clone();
        let footer = self.render_sidebar_footer_row(&footer_theme, cx);
        let outlet = self.settings_outlet(section, window, cx);
        div()
            .id("settings-page")
            .role(gpui::Role::Group)
            .aria_label("Settings")
            .track_focus(&self.settings_focus)
            .tab_group()
            .tab_stop(false)
            .size_full()
            .flex()
            .flex_row()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke.key;
                if key == "escape" {
                    if !this.dismiss_settings_escape_surface(cx) {
                        this.close_settings(cx);
                    }
                    cx.stop_propagation();
                } else if key == "tab" {
                    move_settings_focus(
                        &this.settings_focus,
                        &this.settings_end_focus,
                        event.keystroke.modifiers.shift,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            }))
            .child(
                // Same width and footer wrapper as the chat sidebar, so the
                // account controls hold their position across the swap.
                div()
                    .w(px(sidebar_width))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .pt(px(Theme::TITLEBAR_HEIGHT))
                    .child(
                        div().flex_none().px(px(Theme::SPACE_SM)).child(
                            settings::widgets::section_tab(
                                &theme,
                                false,
                                0.0,
                                "settings-back",
                                "settings-back-hover",
                            )
                            .role(gpui::Role::Button)
                            .aria_label("Back")
                            .tab_index(0)
                            // The shell's ring, paint only (M8).
                            .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))
                            .child(icon(icons::ARROW_LEFT).size(px(16.0)).text_color(
                                motion::hover_blend(
                                    "settings-back-hover",
                                    theme.text_muted,
                                    theme.text,
                                ),
                            ))
                            .child("Back"),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(nav))
                    .child(div().p(px(Theme::SPACE_SM)).flex_none().child(footer)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    // Pages scroll to the window's top edge; `page_column`
                    // carries the titlebar clearance inside the scroll.
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(outlet)),
            )
            .child(
                div()
                    .id("settings-focus-end")
                    .track_focus(&self.settings_end_focus)
                    .tab_index(0)
                    .tab_stop(false),
            )
            .into_any_element()
    }

    /// Roving section tabs, grouped by spacing alone: preferences, agents and
    /// devices, then workspace data.
    fn render_settings_nav(
        &mut self,
        section: SettingsSection,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section_icon = |item: SettingsSection| match item {
            SettingsSection::Devices => icons::LAPTOP, // as the server menu's Devices
            SettingsSection::Harnesses => icons::WIDGET,
            SettingsSection::Appearance => icons::TUNING,
            SettingsSection::Files => icons::FOLDER,
            SettingsSection::Notifications => icons::BELL,
            SettingsSection::Shortcuts => icons::KEYBOARD,
            SettingsSection::Archived => icons::ARCHIVE_MINIMALISTIC,
        };
        settings::widgets::scroll_faded(
            "settings-nav-scroll",
            div()
                .id("settings-sections")
                .role(gpui::Role::TabList)
                .aria_label("Settings sections")
                .w_full()
                .overflow_y_scroll()
                .h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .px(px(Theme::SPACE_SM))
                        .pt(px(Theme::SPACE_MD))
                        .pb(px(Theme::SPACE_SM))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(
                            SettingsSection::ALL
                                .into_iter()
                                .map(|item| {
                                    let index = SettingsSection::ALL
                                        .iter()
                                        .position(|s| *s == item)
                                        .unwrap();
                                    let selected = item == section;
                                    let key = format!("settings-nav-{}", item.label());
                                    let hover_key = format!("{key}-hover");
                                    let selection_t = settings::widgets::tab_selection_t(
                                        window,
                                        format!("{key}-selection"),
                                        selected,
                                        motion::reduced_motion(cx),
                                    );
                                    let tab_text = motion::hover_blend(
                                        &hover_key,
                                        motion::mix(theme.text_muted, theme.text, selection_t),
                                        theme.text,
                                    );
                                    settings::widgets::section_tab(
                                        theme,
                                        selected,
                                        selection_t,
                                        key.clone(),
                                        hover_key,
                                    )
                                    .when(item.starts_nav_group(), |el| el.mt(px(Theme::SPACE_LG)))
                                    .role(gpui::Role::Tab)
                                    .aria_label(item.label())
                                    .aria_selected(selected)
                                    .track_focus(
                                        &self.settings_nav_focus[index].clone().tab_stop(selected),
                                    )
                                    .tab_index(0)
                                    .tab_stop(selected)
                                    .on_key_down(cx.listener(
                                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                                            let items: Vec<_> = SettingsSection::ALL.into_iter().collect();
                                            let current =
                                                items.iter().position(|s| *s == item).unwrap_or(0);
                                            let next = match event.keystroke.key.as_str() {
                                                "up" | "left" => {
                                                    (current + items.len() - 1) % items.len()
                                                }
                                                "down" | "right" => (current + 1) % items.len(),
                                                "home" => 0,
                                                "end" => items.len() - 1,
                                                _ => return,
                                            };
                                            let target = items[next];
                                            this.open_settings(target, cx);
                                            let index = SettingsSection::ALL
                                                .iter()
                                                .position(|s| *s == target)
                                                .unwrap();
                                            window.focus(&this.settings_nav_focus[index], cx);
                                            cx.stop_propagation();
                                        },
                                    ))
                                    // The shell's ring inside the row, paint only (M8).
                                    .map(|el| crate::surface_chrome::focus_ring_inset(el, &theme))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_settings(item, cx)
                                    }))
                                    .child(
                                        icon(section_icon(item))
                                            .size(px(16.0))
                                            .text_color(tab_text),
                                    )
                                    .child(SharedString::from(item.label()))
                                }),
                        ),
                ),
        )
        .into_any_element()
    }


    /// The changes with the keyboard.
    fn active_changes(&self, cx: &App) -> Option<Entity<Changes>> {
        let Some(RightSurface::Diff(id)) = self.front_view(cx) else {
            return None;
        };
        self.diffs.get(&id).cloned()
    }

    /// Resolve shell-owned Escape surfaces in capture phase, before focused
    /// descendants such as an integrated terminal can consume the key.
    fn capture_escape_surface(&mut self, cx: &mut Context<Self>) -> bool {
        // Modals and context menus sit above the rest of the shell. Preserve
        // their existing behavior: only surfaces that already have a Cancel
        // path close here; the others remain explicit blockers.
        if self.delete_space_confirm.is_some() {
            return true;
        }
        // Escape puts an open context menu away, as every system menu does, instead of
        // taking the key and leaving the menu up; one still fading out keeps the key.
        if self.chat_menu.is_open() {
            self.close_chat_menu(cx);
            return true;
        }
        if self.space_menu.is_open() {
            self.close_space_menu(cx);
            return true;
        }
        if self.user_menu.is_open() {
            self.close_user_menu(cx);
            return true;
        }
        if self.chat_menu.get().is_some()
            || self.space_menu.get().is_some()
            || self.user_menu.get().is_some()
        {
            return true;
        }
        if self.chat_rename.is_some() {
            self.finish_rename_chat(false, cx);
            return true;
        }
        if self.rename_space_dialog.is_some() {
            self.rename_space_dialog = None;
            cx.notify();
            return true;
        }
        // The folded-breadcrumbs menu floats over the palette; it closes first.
        if self.add_space.is_some() && self.project_crumb_menu.is_open() {
            self.close_project_crumb_menu(cx);
            return true;
        }
        // A typed path gives the crumbs back before the palette closes.
        if self.add_space_cancel_path_edit(cx) {
            return true;
        }
        if self.add_space.is_some() {
            self.close_add_space(cx);
            return true;
        }
        if self.spaces_menu.is_open() {
            self.close_spaces_menu(cx);
            return true;
        }
        if self.spaces_menu.get().is_some() {
            return true;
        }

        self.active_changes(cx)
            .is_some_and(|changes| changes.update(cx, |changes, cx| changes.handle_escape(cx)))
    }

    fn on_key_down_capture(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            cx.stop_propagation();
            return;
        }
        if matches!(self.route, Route::Settings(_)) {
            return;
        }
        if event.keystroke.key == "escape" && self.capture_escape_surface(cx) {
            cx.stop_propagation();
        }
    }

    fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Inputs and completion menus consume Tab first. Unhandled Tab walks
        // accessible controls, including individual transcript link ranges.
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key == "tab"
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
        {
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
            return;
        }
        // Esc a view took no use of puts the keyboard back in the session's terminal
        // (`docs/decisions/layout.md`, "No permanent right column"); the view stays.
        if event.keystroke.key == "escape"
            && modifiers == gpui::Modifiers::default()
            && matches!(self.route, Route::Chat)
            && !self.overlay_owns_keyboard()
            && matches!(self.panes_now(cx).focused(), panes::Pane::View(_))
        {
            self.focus_pane(panes::Pane::Session, cx);
            cx.stop_propagation();
        }
    }

    fn render_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some(menu_state) = self.chat_menu.get().cloned() {
            let chat_id = menu_state.chat_id;
            let position = menu_state.position;
            let chat_menu_closing = self.chat_menu.closing_at(crate::clock::now(cx));
            let is_side_chat = self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .any(|chat| chat.id == chat_id && chat.parent_chat_id.is_some());
            // A row names its key where the key would do the same, on the open chat
            // and the tab in front (M16).
            let is_open = matches!(self.route, Route::Chat)
                && self.state.read(cx).selected_chat.as_deref() == Some(chat_id.as_str());
            let archive_keys = is_open
                .then(|| crate::keys::for_action(&ArchiveSession, None, window, cx))
                .flatten();
            let rename_id = chat_id.clone();
            let archive_id = chat_id.clone();
            let menu = popover::popover_card(&theme)
                .w(px(216.0))
                .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    // The Copy submenu floats outside this card: a press on
                    // it is inside the menu.
                    if this
                        .chat_copy_bounds
                        .is_some_and(|bounds| bounds.contains(&event.position))
                    {
                        return;
                    }
                    this.close_chat_menu(cx);
                }))
                .flex()
                .flex_col();
            let copy_open = menu_state.page == ChatMenuPage::Copy;
            let menu = if chat_id.is_empty() {
                menu
            } else {
                let rename = popover::menu_row(&theme, false, format!("chat-menu-rename-{chat_id}"))
                    .id("chat-menu-rename")
                    .debug_selector(|| "chat-menu-rename".into())
                    // Every row of the menu is a named menu item, as the tab rows are.
                    .role(gpui::Role::MenuItem)
                    .aria_label("Rename")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_rename_chat(rename_id.clone(), cx)
                    }))
                    .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from("Rename"));
                let menu = menu.child(self.chat_menu_hover_row(CHAT_MENU_ROW_RENAME, rename, cx));
                if is_side_chat {
                    menu
                } else {
                    let archive =
                        popover::menu_row(&theme, false, format!("chat-menu-archive-{chat_id}"))
                            .id("chat-menu-archive")
                            .role(gpui::Role::MenuItem)
                            .aria_label("Archive")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.archive_chat(archive_id.clone(), cx)
                            }))
                            .child(
                                icon(icons::ARCHIVE_MINIMALISTIC)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(div().flex_1().child(SharedString::from("Archive")))
                            .children(archive_keys.map(|keys| popover::kbd_hint(&theme, &keys)));
                    let copy = self.render_chat_copy_trigger(&chat_id, copy_open, &theme, cx);
                    // The chat's folder in the person's editor (`open_in`).
                    let open_in = (Self::offers_open_in()
                        && !self.open_in_choices(&chat_id, cx).is_empty())
                    .then(|| {
                        let open_id = chat_id.clone();
                        let row = popover::menu_row(&theme, false, format!("chat-menu-open-in-{chat_id}"))
                            .id("chat-menu-open-in")
                            .role(gpui::Role::MenuItem)
                            .aria_label("Open in\u{2026}")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_open_in(open_id.clone(), position, cx)
                            }))
                            .child(icon(icons::FILE_CODE).size(px(16.0)).text_color(theme.text_muted))
                            .child(SharedString::from("Open in\u{2026}"));
                        self.chat_menu_hover_row(CHAT_MENU_ROW_OPEN_IN, row, cx)
                    });
                    menu.child(self.chat_menu_hover_row(CHAT_MENU_ROW_ARCHIVE, archive, cx))
                        .child(self.chat_menu_hover_row(CHAT_MENU_ROW_COPY, copy, cx))
                        .children(open_in)
                }
            };
            let menu = menu.into_any_element();
            overlays.push(popover::menu_at(
                "chat-context-menu",
                position,
                menu,
                chat_menu_closing,
            ));
        }
        overlays.extend(self.render_open_in_menu(cx));

        overlays.extend(self.render_space_overlays(viewport, window, cx));
        if let Some(overlay) = self.render_command_palette(viewport, window, cx) {
            overlays.push(overlay);
        }
        overlays.extend(self.render_picker(viewport, window, cx));
        overlays.extend(self.render_new_session_picker(viewport, window, cx));
        overlays.extend(self.render_inbox(window, cx));
        if let Some(overlay) = self.render_add_space_overlay(viewport, window, cx) {
            overlays.push(overlay);
        }
        if let Some(overlay) = self.render_project_action_overlay(viewport, window, cx) {
            overlays.push(overlay);
        }



        overlays
    }

    fn resize_handle<T>(
        &self,
        id: &'static str,
        kind: PaneResizeKind,
        marker: fn() -> T,
        reset: fn(&mut Shell, &mut Context<Shell>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div>
    where
        T: 'static,
    {
        let theme = Theme::of(cx);
        let fade_key = format!("pane-resize-{id}");
        let hover_highlight = motion::hover_blend(
            &fade_key,
            theme.border_strong.opacity(0.0),
            theme.border_strong,
        );
        let active = self.pane_resize_active == Some(kind);
        let constrained = self.pane_resize_dragging == Some(kind) && !active;
        let highlight = if constrained {
            theme.border_strong.opacity(0.0)
        } else if active {
            theme.border_strong
        } else {
            hover_highlight
        };
        let clear = highlight.opacity(0.0);
        let release_key = fade_key.clone();
        let release_out_key = fade_key.clone();
        // A splitter a screen reader names (T3's `role="separator"`).
        let name = match kind {
            PaneResizeKind::Sidebar => "Resize sidebar",
        };
        div()
            .id(id)
            .role(gpui::Role::Splitter)
            .aria_label(name)
            .aria_orientation(gpui::Orientation::Vertical)
            .absolute()
            .top(px(PANE_RESIZE_HITBOX_TOP))
            .bottom_0()
            .w(px(PANE_RESIZE_HITBOX_HALF_WIDTH * 2.0))
            .flex_none()
            .occlude()
            .cursor_col_resize()
            // The resize cursor holds for the whole drag, past the handle and the clamp.
            .when(self.pane_resize_dragging == Some(kind), |el| {
                el.child(drag_cursor(gpui::CursorStyle::ResizeColumn))
            })
            .hover_fade(fade_key)
            // Codex-style seam feedback: the existing 1px panel border stays
            // visible at rest; hover adds a stronger center highlight that
            // fades back into that border toward both ends.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(PANE_RESIZE_HITBOX_HALF_WIDTH))
                    .w(px(1.0))
                    .flex()
                    .flex_col()
                    .child(div().flex_1().bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(clear, 0.0),
                        gpui::linear_color_stop(highlight, 1.0),
                    )))
                    .child(div().flex_1().bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(highlight, 0.0),
                        gpui::linear_color_stop(clear, 1.0),
                    ))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.pane_resize_dragging = Some(kind);
                    this.pane_resize_active = Some(kind);
                    cx.notify();
                }),
            )
            .on_drag(marker(), |_, _point: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, window, cx| {
                    if event.click_count == 2 {
                        reset(this, cx);
                        this.schedule_save(cx);
                        cx.notify();
                    }
                    this.finish_pane_resize(kind);
                    motion::set_hover(&release_key, false, this.reduced_motion.get());
                    window.refresh();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, window, _| {
                    this.finish_pane_resize(kind);
                    motion::set_hover(&release_out_key, false, this.reduced_motion.get());
                    window.refresh();
                }),
            )
    }

    /// The main column is the selected session's view: its terminal, full height and
    /// width with nothing over it, and the panes split or summoned beside it
    /// (`docs/decisions/layout.md`, "One session per view, splits on demand"). With no session
    /// selected it offers to start one; with nowhere to work yet, the first step.
    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme_owned = Theme::of(cx).clone();
        let theme = &theme_owned;
        let faint = theme.text_faint;
        let selected = self.state.read(cx).selected_chat.clone();
        let has_terminal =
            selected.as_deref().is_some_and(|chat| self.state.read(cx).session_terminal(chat).is_some());
        let outlet: AnyElement = if has_terminal {
            let landing = self.landing_fade(selected.as_deref(), window, cx);
            div()
                .id("session-terminal")
                .debug_selector(|| "session-terminal".into())
                .relative()
                .flex_1()
                .min_h_0()
                .child(self.session_terminal.clone())
                .when_some(landing, |terminal, opacity| {
                    terminal.child(
                        div()
                            .debug_selector(|| "session-landing".into())
                            .absolute()
                            .inset_0()
                            .border_2()
                            .border_color(theme.accent.opacity(opacity)),
                    )
                })
                .into_any_element()
        } else if let Some(chat) = selected {
            self.render_session_ended(&chat, theme, cx)
        } else if let Some(step) = self.first_step(cx) {
            // Onboarding (first boot / after the destructive wipe): no folders
            // to work in yet — one clear affordance.
            let _ = faint;
            // With no device yet there is nowhere for a project: add one first.
            let no_devices = step == spaces::FirstStep::AddDevice;
            let action = step.action();
            let (headline, line) = if no_devices {
                ("Add a device to get started", "A device is a machine Slopty works on.")
            } else {
                ("Add a project to get started", "A project is a folder on one of your devices.")
            };
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(motion::fade_in(
                    "no-spaces-canvas",
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        // The glyph of what fills the canvas, as Archived's empty page
                        // says its own, where the logo's dots read as the app's spinner.
                        .child(
                            icon(step.glyph())
                                .size(px(28.0))
                                .text_color(theme.text_muted.opacity(0.2)),
                        )
                        .child(
                            div()
                                .id("onboarding-headline")
                                .role(gpui::Role::Heading)
                                .aria_label(headline)
                                .mt(px(16.0))
                                .text_size(crate::typography::ui_rems(16.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(headline)),
                        )
                        .child(
                            div()
                                .id("onboarding-line")
                                .role(gpui::Role::Label)
                                .aria_label(line)
                                .mt(px(6.0))
                                .text_size(crate::typography::ui_rems(13.0))
                                .text_color(theme.text_muted.opacity(0.7))
                                .child(SharedString::from(line)),
                        )
                        .child(
                            popover::named_button(
                                popover::btn_primary(&theme_owned, action)
                                    .id(if no_devices { "onboarding-add-device" } else { "onboarding-add-space" }),
                                action, // named, as the canvas's one action
                            )
                                .mt(px(20.0))
                                .on_click(cx.listener(move |this, _, _, cx| step.take(this, cx))),
                        ),
                ))
                .into_any_element()
        } else {
            self.render_no_session(theme, cx)
        };
        div()
            .id("main-column")
            .debug_selector(|| "main-column".into())
            .track_focus(&self.main_focus)
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                this.capture_navigation_focus(window, cx);
            }))
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            // A session's column stands on its terminal's own ground, the titlebar row over it
            // too (`docs/decisions/layout.md`, "One row of chrome").
            .when(has_terminal, |column| column.bg(self.shown_ground(cx)))
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .child({
                // The session's view, its terminal or that it ended, or the new-session canvas,
                // among its panes: a shell splits beside either.
                let session = div().size_full().flex().flex_col().child(outlet).into_any_element();
                div().flex_1().min_h_0().child(self.render_panes(session, window, cx)).into_any_element()
            })
            .into_any_element()
    }

    /// A note's tap landed on `chat`: its terminal wears one accent border fade.
    ///
    /// Ghostty presents the surface a notification lands on so, without its repeating glow
    /// (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a feed").
    pub(crate) fn present_landing(&mut self, chat: String, cx: &mut Context<Self>) {
        self.landed = Some((chat, crate::clock::now(cx)));
        cx.notify();
    }

    /// The landing fade's border opacity for the terminal of `shown` this frame, asking for the
    /// next while it fades; `None` once it has, or while another session is shown.
    fn landing_fade(&mut self, shown: Option<&str>, window: &mut Window, cx: &App) -> Option<f32> {
        let (chat, at) = self.landed.as_ref()?;
        let opacity = landing_opacity(crate::clock::now(cx).saturating_duration_since(*at))
            .filter(|_| shown == Some(chat.as_str()));
        if opacity.is_some() {
            window.request_animation_frame();
        } else {
            self.landed = None;
        }
        opacity
    }

    /// The main column for a session with no terminal: one starting shows that it
    /// starts; one that ended says so and offers to resume it in a new terminal (its record
    /// stays in the inspector).
    fn render_session_ended(&mut self, chat: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (row, resumable) = {
            let state = self.state.read(cx);
            let owner = state.panel_owner(chat);
            let row = state.chats.iter().find(|row| row.id == owner).cloned();
            let resumable = row.as_ref().is_some_and(|row| {
                row.harness_session_id.as_deref().is_some_and(|id| !id.is_empty())
                    && row.config.as_ref().is_none_or(|config| {
                        config.harness == slopty_view_proto::HarnessId::CLAUDE_CODE
                    })
            });
            (row, resumable)
        };
        let starting = self.session_starting(chat, cx)
            || row.as_ref().is_some_and(|row| self.resuming(row));
        let (glyph, headline, line): (&str, &str, &str) = if starting {
            (icons::TERMINAL, "Starting\u{2026}", "Its terminal opens here in a moment.")
        } else if resumable {
            (
                icons::CHECK,
                "This session has ended",
                "Its record stays in the inspector. Resume it to go on in a new terminal.",
            )
        } else {
            (icons::CHECK, "This session has ended", "Its record stays in the inspector.")
        };
        let resume = (resumable && !starting).then(|| {
            popover::named_button(popover::btn_primary(theme, "Resume").id("session-resume"), "Resume")
                .mt(px(20.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(row) = row.clone() {
                        this.resume_ended_session(&row, cx);
                    }
                }))
        });
        div()
            .id("session-ended")
            .debug_selector(|| "session-ended".into())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(motion::fade_in(
                "session-ended-canvas",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(icon(glyph).size(px(28.0)).text_color(theme.text_muted.opacity(0.2)))
                    .child(
                        div()
                            .id("session-ended-headline")
                            .role(gpui::Role::Heading)
                            .aria_label(headline)
                            .mt(px(16.0))
                            .text_size(crate::typography::ui_rems(16.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(headline),
                    )
                    .child(
                        div()
                            .id("session-ended-line")
                            .role(gpui::Role::Label)
                            .mt(px(6.0))
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(line),
                    )
                    .children(resume),
            ))
            .into_any_element()
    }

    /// Whether session `chat` was just started or resumed here and its terminal has
    /// not shown yet; the main column is drawn again as the wait runs out.
    fn session_starting(&self, chat: &str, cx: &App) -> bool {
        let Some((id, since)) = self.starting.as_ref() else { return false };
        id == chat && crate::clock::now(cx).saturating_duration_since(*since) < STARTS_WITHIN
    }

    /// Session `chat` was just started or resumed: its main column says "Starting…"
    /// until its terminal shows, for [`STARTS_WITHIN`] at most.
    pub(super) fn mark_starting(&mut self, chat: String, cx: &mut Context<Self>) {
        self.starting = Some((chat, crate::clock::now(cx)));
        self._starting_redraw = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STARTS_WITHIN).await;
            let _gone = this.update(cx, |_, cx| cx.notify());
        }));
    }

    /// The main column with no session selected: one quiet offer to start one, with
    /// its key, and the history to resume from.
    fn render_no_session(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(motion::fade_in(
                "no-session-canvas",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        icon(icons::TERMINAL)
                            .size(px(28.0))
                            .text_color(theme.text_muted.opacity(0.2)),
                    )
                    .child(
                        div()
                            .id("no-session-headline")
                            .role(gpui::Role::Heading)
                            .aria_label("No session open")
                            .mt(px(16.0))
                            .text_size(crate::typography::ui_rems(16.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("No session open"),
                    )
                    .child(
                        div()
                            .id("no-session-line")
                            .role(gpui::Role::Label)
                            .mt(px(6.0))
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child("Pick one in the sidebar, or start an agent in a folder."),
                    )
                    .child(
                        popover::named_button(
                            popover::btn_primary(theme, "New session").id("no-session-new"),
                            "New session",
                        )
                        .mt(px(20.0))
                        .action_hint("New session", Keys::Action(Box::new(NewSession)), cx)
                        .on_click(cx.listener(|this, _, _, cx| this.open_new_session(None, cx))),
                    ),
            ))
            .into_any_element()
    }




    fn render_gate_card(&mut self, error: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        // Backend unreachable: quiet centered copy,
        // plus a Retry affordance (the native engine doesn't self-redial).
        let content: AnyElement = div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(Theme::SPACE_MD))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(14.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(error.to_owned())),
                )
                .child(
                    div()
                        .id("retry-engine")
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()))
                        .on_click(cx.listener(|this, _, _, cx| this.retry_engine(cx)))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element();
        div()
            .size_full()
            .relative()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    // Keyed per phase: every gate swap replays the
                    // 0.5s entrance instead of mutating one animated element.
                    .child(motion::fade_in(
                        "gate-card-failed",
                        div().child(content),
                    )),
            )
            .into_any_element()
    }

}

/// The gate's faint grid backdrop:
/// 44px hairlines at white 3.5%, with the radial mask approximated by edge
/// gradients back into the page background (gpui has no mask-image).
fn grid_backdrop(theme: &Theme) -> AnyElement {
    let line = crate::theme::hairline(0.035);
    let bg = theme.bg;
    const STEP: f32 = 44.0;
    const SPAN: f32 = 2640.0;
    let verticals = (1..(SPAN / STEP) as usize).map(|i| {
        div()
            .absolute()
            .left(px(i as f32 * STEP))
            .top_0()
            .bottom_0()
            .w(px(1.0))
            .bg(line)
    });
    let horizontals = (1..((SPAN * 0.75) / STEP) as usize).map(|i| {
        div()
            .absolute()
            .top(px(i as f32 * STEP))
            .left_0()
            .right_0()
            .h(px(1.0))
            .bg(line)
    });
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .children(verticals)
        .children(horizontals)
        // Mask approximation: fade the grid back into the background toward
        // the window edges (the original masks to an ellipse at 50% / 40%).
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(120.0))
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(260.0))
                .bg(gpui::linear_gradient(
                    0.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    270.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .into_any_element()
}

/// A 24 px icon button for the titlebar strip, its glyph centred in the muted ink.
// The caller names it with `action_hint` (its tooltip, key and accessible name).
fn window_control_button(
    id: impl Into<SharedString>,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    // A 24 control holds a 14 glyph (M48).
    let glyph = icon(icon_path).size(px(14.0)).text_color(theme.text_muted);
    window_control_button_with(id, glyph, theme, on_click)
}

/// [`window_control_button`] around a caller-drawn glyph (the morphing
/// sidebar glyph).
/// The titlebar row's top padding: [`Theme::TITLEBAR_TOP_PAD`], and on a touch screen
/// enough more that its 24 pt controls' 44 pt presses start inside the window.
pub(crate) fn titlebar_top_pad() -> f32 {
    let center = (Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD) * 0.5;
    let over = (crate::surface_chrome::touch_reach(24.0) - (center - 12.0)).max(0.0);
    Theme::TITLEBAR_TOP_PAD + 2.0 * over
}

fn window_control_button_with(
    id: impl Into<SharedString>,
    glyph: impl IntoElement,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let fade_key = format!("window-control-{id}");
    // On a touch screen the control takes presses over a finger's 44 pt, centred on the
    // 24 it draws, which keeps its wash and glyph (#23).
    let reach = crate::surface_chrome::touch_reach(24.0);
    let group = SharedString::from(format!("window-control-press-{id}"));
    let drawn = div()
        .id(SharedString::from(format!("{id}-drawn")))
        .size(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        // The wash fades, the header buttons' wash, so the strip's controls agree (M24).
        .bg(motion::hover_blend(
            &fade_key,
            crate::theme::wash(0.0),
            crate::theme::wash(0.11),
        ))
        .group_active(group.clone(), |style| style.bg(theme_pressed()))
        .child(glyph);
    div()
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .group(group)
        .size(px(24.0 + 2.0 * reach))
        .m(px(-reach))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0 + reach))
        .cursor_pointer()
        .hover_fade(fade_key)
        // Buttons in/over a titlebar drag strip must be EXCLUDED from the
        // strip's event surface entirely. `.occlude()` (gpui
        // `HitboxBehavior::BlockMouse`) makes the window hit-test STOP at the
        // button, so every `is_hovered`-guarded strip listener — the
        // mouse-down that arms the drag, the mouse-move that hands AppKit a
        // native drag session (`performWindowDragWithEvent:`, whose second
        // quick click zooms NATIVELY on macOS), and the `click_count == 2`
        // zoom handler — never fires with the pointer over a button. It also
        // removes the button's rect from the native Drag control-area
        // hit-test on Windows/Linux. The click-level stop_propagation is
        // zed's ButtonLike belt on top. Double-click on EMPTY strip space
        // still zooms — nothing occludes it there.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(drawn)
        // A button for VoiceOver (and the simulator's e2e taps), reached by Tab, pressed
        // with a darker wash, its ring a hairline that never resizes it.
        .role(gpui::Role::Button)
        .tab_index(0)
        .map(|el| crate::surface_chrome::focus_ring_within(el, theme, reach))
}

// What a pressed chrome button shows under the pointer (Zed's `active_background`).
fn theme_pressed() -> gpui::Hsla {
    crate::theme::wash(0.16)
}

const WINDOWS_CAPTION_BUTTON_WIDTH: f32 = 36.0;
const WINDOWS_CAPTION_WIDTH: f32 = WINDOWS_CAPTION_BUTTON_WIDTH * 3.0;

/// Right padding for titlebar content: past the native Windows caption
/// cluster, or past Slopty's own Linux caption buttons (10px edge inset +
/// the button row) when the layout puts any on the right.
fn titlebar_right_padding(is_windows: bool, linux_right_captions: usize, base: f32) -> f32 {
    base + if is_windows {
        WINDOWS_CAPTION_WIDTH
    } else if linux_right_captions > 0 {
        10.0 + caption_buttons_width(linux_right_captions)
    } else {
        0.0
    }
}

/// A Windows-owned caption target using the same system glyphs and native
/// non-client hit-test areas as GPUI/Zed's platform titlebar.
fn windows_caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    theme: &Theme,
    close: bool,
) -> impl IntoElement {
    let (hover_bg, hover_fg, active_bg, active_fg) = if close {
        let red: gpui::Hsla = gpui::rgb(0xe81123).into();
        (
            red,
            gpui::white(),
            red.opacity(0.8),
            gpui::white().opacity(0.8),
        )
    } else {
        (
            theme.glass_hover(),
            theme.text,
            theme.glass_hover().opacity(0.7),
            theme.text,
        )
    };
    div()
        .id(id)
        .w(px(WINDOWS_CAPTION_BUTTON_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(active_bg).text_color(active_fg))
        .occlude()
        .window_control_area(area)
        .child(glyph)
}

/// A Linux caption button in Slopty's own cluster style (24px, rounded-6,
/// 16px linear icon). gpui's `WindowControlArea` hit-testing is inert on
/// Linux, so unlike the Windows cluster these carry explicit click handlers
/// (`minimize_window` / `zoom_window` / `remove_window`), the same calls
/// zed's Linux titlebar makes. `occlude` + `prevent_default` keep them out
/// of the drag strip's event surface (see [`window_control_button`]).
fn linux_caption_button(
    id: &'static str,
    label: &'static str,
    icon_path: &'static str,
    close: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (muted, hover_bg, hover_fg) = if close {
        let red: gpui::Hsla = gpui::rgb(0xe81123).into();
        (theme.text_muted, red, gpui::white())
    } else {
        (theme.text_muted, theme.glass_hover(), theme.text)
    };
    div()
        .id(id)
        // gpui svgs don't inherit the div's text color — recolor the glyph
        // on hover through the group instead (zed's WindowControl idiom).
        .group("linux-caption-button")
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .role(gpui::Role::Button)
        .aria_label(label)
        .tooltip(crate::settings::widgets::text_tooltip(label))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(
            icon(icon_path)
                .size(px(16.0))
                .text_color(muted)
                .group_hover("linux-caption-button", move |style| {
                    style.text_color(hover_fg)
                }),
        )
}

/// A 28 px icon button for the main-panel header, its glyph centred in the muted ink.
// The caller names it with `action_hint` (its tooltip, key and accessible name).
fn header_icon_button(
    id: &'static str,
    icon_path: &'static str,
    on: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let glyph = icon(icon_path).size(px(16.0)).text_color(theme.text_muted);
    header_icon_button_with(id, glyph, on, theme, on_click)
}

/// [`header_icon_button`] around a caller-drawn glyph (the morphing sidebar
/// glyph).
// `on` washes what it draws for a pane it shows, as an open menu's trigger is.
fn header_icon_button_with(
    id: &'static str,
    glyph: impl IntoElement,
    on: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let fade_key = format!("header-icon-{id}");
    // On a touch screen it takes presses over a finger's 44 pt around the 28 it draws.
    let reach = crate::surface_chrome::touch_reach(28.0);
    let group = SharedString::from(format!("header-icon-press-{id}"));
    let drawn = div()
        .id(SharedString::from(format!("{id}-drawn")))
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        // The wash fades, from the theme's one table of wash steps, as every icon toggle's
        // (`surface_chrome::icon_button_frame`): a latched button under the pointer still
        // answers, a step past its latch.
        .bg({
            let (rest, hover) = if on {
                (crate::theme::selected_wash(), crate::theme::selected_hover_wash())
            } else {
                (crate::theme::wash(0.0), crate::theme::control_hover())
            };
            motion::hover_blend(&fade_key, rest, hover)
        })
        .group_active(group.clone(), |style| style.bg(crate::theme::pressed_wash()))
        .child(glyph);
    div()
        .id(id)
        .debug_selector(|| id.into())
        .group(group)
        .size(px(28.0 + 2.0 * reach))
        .m(px(-reach))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0 + reach))
        .cursor_pointer()
        .hover_fade(fade_key)
        // Same occlusion + click-swallowing as [`window_control_button`]: this
        // button sits inside the chat header's titlebar drag region, so its
        // rect must be carved out of the strip's drag/double-click surface.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        // A button reached by Tab, its ring a hairline that never resizes it, pressed
        // with a darker wash.
        .role(gpui::Role::Button)
        .tab_index(0)
        .map(|el| crate::surface_chrome::focus_ring_within(el, theme, reach))
        .child(drawn)
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        motion::begin_hover_frame(cx.frame_time()); // the frame's fades at its moment
        popover::return_focus(window, cx); // a closed menu gives the keyboard back
        // Every repaint (state frame, input, or the clock firing) reschedules
        // the next clock-driven one; the sidebar and transcript follow this
        // view's notifications.
        let now = crate::clock::utc(cx);
        let wake = shell_clock_wake(self.state.read(cx), now);
        self.clock
            .arm(wake, now, |shell: &mut Shell| &mut shell.clock, cx);
        // The panes a chat has beside its terminal are kept across launches.
        self.restore_tabs(window, cx);
        self.keep_tabs(cx);
        let active_files_key = self.panel_key(cx);
        let hidden_explorers = self
            .files
            .iter()
            .filter(|(key, _)| {
                key.as_str() != active_files_key || !matches!(self.route, Route::Chat)
            })
            .map(|(_, files)| files.clone())
            .collect::<Vec<_>>();
        for files in hidden_explorers {
            files.update(cx, |files, cx| files.suspend_tree_interactions(cx));
        }
        if let Some(path) = self.pending_change.take() {
            self.show_change_at(&path, window, cx);
        }
        self.focus_rename_chat(window, cx);

        // The frame's moment, so a frame drawn again stands where this one did.
        self.render_time = Some(self.frame_motion_time(cx));
        if self.all_file_edits_flushed(cx)
            && let Some(action) = self.pending_exit.take()
        {
            let shell = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                if matches!(action, PendingExit::Quit) {
                    crate::app_menus::request_quit(cx);
                } else {
                    shell
                        .update(cx, |shell, cx| match action {
                            PendingExit::CloseWindow => {
                                if shell.prepare_window_close(cx) {
                                    window.remove_window();
                                }
                            }
                            PendingExit::Quit => unreachable!(),
                        })
                        .ok();
                }
            });
        }
        crate::transcript::record_view_frame("shell");
        self.observe_viewport_width(f32::from(window.viewport_size().width));
        // Appearance actions persist independently of the shell. Mirror the
        // globals before any later debounced settings save can overwrite them.
        self.settings.appearance = crate::appearance::mode(cx);
        self.settings.theme_selection = crate::appearance::themes(cx);
        self.settings.accent = crate::appearance::accent(cx);
        self.settings.surface = crate::appearance::surface(cx);
        self.pull_settings(cx);
        let theme = Theme::of(cx);
        // The shell frost sits over native desktop blur on macOS and Windows.
        // Content surfaces add their own backgrounds over this shared tint.
        let (frost, text, font) = (theme.glass(), theme.text, theme.font_sans.clone());
        let gate = self
            .debug_gate
            .clone()
            .unwrap_or_else(|| self.state.read(cx).gate());

        self.sync_browser_profile(cx);
        // Manual tween drive bookkeeping for this pass (see [`WidthTween`]).
        self.reduced_motion = motion::ReducedMotion::of(cx);
        self.motion_active.set(false);
        self.track_horizontal_fit();
        // The sidebar's render reconciles the card against its rows.
        if !matches!(gate, GatePhase::Ready)
            || matches!(self.route, Route::Settings(_))
            || self.sidebar_target() <= 0.0
            || self.settings.sidebar_collapsed // the rail has no rows
        {
            self.dismiss_session_card(cx);
        }
        // A page draws while its pane is laid out: in the shown session's view, through
        // a zoom only the pane zoomed, and on a phone only the page in front.
        let laid_views = self.laid_views(matches!(gate, GatePhase::Ready), cx);
        // Native clipping follows the GPUI layout. Drags only transfer pointer ownership; the
        // browser continues rendering and reflowing.
        let browser_dragging = cx.has_active_drag();
        for (id, browser) in &self.browsers {
            let presentation = crate::browser::model::presentation(
                laid_views.contains(&RightSurface::Browser(*id)),
                browser_dragging,
            );
            browser.update(cx, |browser, cx| {
                // The page starts clear of the grab of the divider at its left (`pane_area`).
                #[cfg(target_os = "macos")]
                browser.set_resize_inset(px((panes::DIVIDER_GRAB - panes::DIVIDER) / 2.0), cx);
                browser.set_shortcuts(&self.settings.keymap);
                browser.set_presentation(presentation, cx);
            });
        }
        // A file's images let go of their memory while its view is not laid out (another
        // session's, under a zoom, behind a phone's page); drawing it again brings them back.
        for (id, file) in &self.file_surfaces {
            if !laid_views.contains(&RightSurface::File(*id)) {
                file.update(cx, |file, cx| file.suspend_images(cx));
            }
        }

        // Fullscreen hides the macOS traffic lights — reflow the control
        // cluster with a 200ms ease-out tween (§1.1). A fullscreen transition
        // resizes the window, which re-renders us, so polling here is exact.
        let fullscreen = window.is_fullscreen();
        if self.fullscreen != Some(fullscreen) {
            if self.fullscreen.is_some() && cfg!(target_os = "macos") {
                self.titlebar_tween = Some(WidthTween::new(
                    titlebar_cluster_start(!fullscreen),
                    titlebar_cluster_start(fullscreen),
                    self.now(),
                ));
            }
            self.fullscreen = Some(fullscreen);
        }
        // Linux CSD: (re-)resolve which caption buttons we draw and on which
        // side — decorations can flip server↔client at runtime and the
        // desktop's button layout is user configuration.
        self.linux_captions = Self::resolve_linux_captions(window, cx);
        if cfg!(target_os = "linux") && self.button_layout_sub.is_none() {
            self.button_layout_sub =
                Some(cx.observe_button_layout_changed(window, |_, _, cx| cx.notify()));
        }
        agent_ui::serve(window, cx); // this window takes agents' asks (`agent_ui`)

        if self.activation_sub.is_none() {
            self.activation_sub = Some(cx.observe_window_activation(
                window,
                |this: &mut Shell, window, cx| {
                    motion::window_activation_changed(window.is_window_active(), cx);
                    // The titlebar's title dims while the window is not the key one.
                    cx.notify();
                    if !window.is_window_active() {
                        this.reset_command_palette_key_state();
                        this.set_jump_hints(false, cx);
                    }
                },
            ));
        }

        // A live handle can refer to an unmounted element. Recover against
        // the completed frame so newly mounted dialogs can claim focus first.
        if self.focus_sub.is_none() {
            self.focus_sub = Some(cx.on_focus_lost(window, |this: &mut Shell, window, cx| {
                let root = this.shortcut_focus.clone();
                let (preferred, unfocused) = this.navigation_focus_fallback(cx);
                window.on_next_frame(move |window, cx| {
                    restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
                });
                cx.notify();
            }));
        }
        if self.settings_restore_pending {
            self.settings_restore_pending = false;
            let target = self
                .settings_return_focus
                .take()
                .unwrap_or_else(|| self.session_terminal.read(cx).focus_handle());
            window.on_next_frame(move |window, cx| window.focus(&target, cx));
        }
        let shortcut_focus = self.shortcut_focus.clone();
        let (preferred_focus, unfocused) = self.navigation_focus_fallback(cx);
        defer_mounted_focus_restore(shortcut_focus, preferred_focus, unfocused, window, cx);

        // Modifier events follow focus too. Reconcile from the window's input
        // snapshot so a missed release (or pointer event after it) heals hints.
        if !window.is_window_active() {
            self.set_jump_hints(false, cx);
        } else if self.jump_hints {
            // Only a fresh modifier event may turn hints on: activation can
            // retain the snapshot from before Cmd+Tab.
            self.update_jump_hints(&window.modifiers(), cx);
        }

        let root = div()
            .id("shell-root")
            .track_focus(&self.shortcut_focus)
            .child(div().track_focus(&self.unfocused))
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(frost)
            // Linux CSD: a floating window rounds its corners against the
            // desktop (the window composites with alpha — see
            // `Theme::window_background_appearance`); tiled/maximized keeps
            // square edges flush with the screen. Everything, including the
            // splash and caption chrome, clips to the curve.
            .when(Self::linux_window_floating(window), |el| {
                el.rounded(px(LINUX_WINDOW_CORNER_RADIUS)).overflow_hidden()
            })
            .text_color(text)
            .font_family(font)
            // Every text under the window draws a prompt's symbols (`symbol_fallbacks`).
            .map(|mut el| {
                el.text_style().font_fallbacks = Some(crate::typography::symbol_fallbacks());
                el
            })
            .text_size(crate::typography::ui_rems(14.0))
            // Mouse side buttons walk the same history as the titlebar
            // back/forward arrows (the browser surface keeps them to itself).
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, _, cx| this.navigate_back(cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| this.navigate_forward(cx)),
            )
            .capture_key_down(cx.listener(Self::on_key_down_capture))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drag_move(cx.listener(Self::on_sidebar_drag))
            // The view keys are chat-scoped chrome: in Settings they are no-ops. The sidebar
            // toggle stays live everywhere.
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| {
                // The file with the keyboard saves.
                if matches!(this.route, Route::Chat)
                    && let Some(RightSurface::File(id)) = this.front_view(cx)
                    && let Some(file) = this.file_surfaces.get(&id).cloned()
                {
                    file.update(cx, |file, cx| file.save_active_document(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                if !matches!(this.route, Route::Settings(_)) {
                    this.toggle_sidebar(cx)
                }
            }))
            // New session works from anywhere — `open_new_session` routes back
            // to chat itself, so Settings is not a dead spot.
            .on_action(cx.listener(|this, _: &NewSession, _, cx| this.open_new_session(None, cx)))
            // Native Settings menu item and the platform convention (Cmd+, on
            // macOS, Ctrl+, elsewhere) toggle the modal from any section.
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| this.toggle_settings(cx)))
            .on_action(cx.listener(|this, _: &NextSession, _, cx| {
                this.cycle_navigation(true, cx)
            }))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| {
                this.cycle_navigation(false, cx)
            }))
            // Each view's key summons it beside the session, and again puts the keyboard
            // back in the terminal (`docs/decisions/layout.md`, "No permanent right column").
            .on_action(cx.listener(|this, _: &ToggleChanges, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.summon_changes(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleFiles, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.summon_files(window, cx);
                }
            }))
            // Chat-scoped like the panel toggles: Settings has no current
            // session to archive. Quiet under an open popover, like the other
            // session-nav shortcuts.
            .on_action(cx.listener(|this, _: &ArchiveSession, _, cx| {
                if matches!(this.route, Route::Chat) && !this.overlay_owns_keyboard() {
                    this.archive_selected_chat(cx)
                }
            }))
            // A jump routes back to chat itself, so Settings is not a dead
            // spot — the same call a click on that sidebar row makes. But an
            // open picker/palette owns the keyboard: no jumping underneath
            // it. The MODEL menu advertises these same slots on its rows and
            // this matched binding beats its key handler to the dispatch —
            // forward the slot instead of eating it.
            .on_action(cx.listener(|this, jump: &JumpSession, _, cx| {
                // The session picker marks its first nine with the same keys.
                if this.picker_open() {
                    this.pick_digit(jump.0, cx);
                } else if !this.overlay_owns_keyboard() {
                    this.jump_to_session(jump.0, cx)
                }
            }))
            .on_modifiers_changed(
                cx.listener(|this, event, _, cx| this.on_modifiers_changed(event, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddSpacePalette, _, cx| {
                if matches!(this.route, Route::Settings(_)) {
                    return;
                }
                if this.add_space.is_some() {
                    this.close_add_space(cx);
                } else {
                    this.open_add_space(cx);
                }
            }));
        let root = self.chrome_actions(root, cx);
        let root = self.picker_actions(root, cx);

        // The ground the window shows, a session's, for the window to wear its appearance.
        let mut shown_ground = None;
        let root = match &gate {
            GatePhase::Ready => 'ready: {
                // Focus is a sync signal: on the rising edge of window
                // activation, nudge every open room to verify liveness — a
                // broadcast-deaf socket (accepted writes, runtime pongs,
                // nothing delivered; 2026-08-04 incident) then heals within
                // seconds of the user looking at the app rather than waiting
                // out the background probe cadence.
                // Whether the person looks, as the OS says or the e2e socket holds.
                let window_active = crate::presence::looking(window, cx);
                if window_active && !self.was_window_active {
                    self.state.update(cx, |s, cx| s.probe_sync(cx));
                }
                if window_active != self.was_window_active {
                    // The invariants' watch reads whether anybody looks.
                    self.state.update(cx, |s, _| s.window_active = window_active);
                }
                self.was_window_active = window_active;
                // What the person looks at, which the app's notes keep quiet for
                // (`presence::OnScreen`).
                let on_screen = self.state.read(cx).selected_chat.clone();
                crate::presence::show(on_screen.filter(|_| window_active), cx);
                // A run finishing while you're LOOKING at the session must not
                // badge "completed" until you leave and return — mark it seen
                // live while the window is active (idempotent guard inside;
                // one extra frame settles it).
                // Marked after this frame, never while it draws: a change to the state
                // while a frame draws reaches none of the views watching it (the sidebar went
                // on drawing "Done" from its last build); a result arriving while the window
                // is active is marked as it arrives (`Shell::new`'s state observer), so this
                // is the window coming back to the front.
                if window_active
                    && self.state.read(cx).selected_chat_row().is_some_and(|chat| chat.unseen())
                {
                    cx.defer_in(window, |this, _, cx| this.mark_open_chat_seen(cx));
                }
                // Settings replaces the whole workspace, sidebar included. The
                // chat layout stays unmounted; its entities keep their state
                // for the return trip.
                if let Route::Settings(section) = self.route {
                    let settings_page = self.render_settings_page(section, window, cx);
                    let overlays = self.render_overlays(window.viewport_size(), window, cx);
                    let border_color = Theme::of(cx).border;
                    let sidebar_tone =
                        Self::sidebar_tone(self.settings.sidebar_width, border_color, window);
                    let drag = self.titlebar_drag_region(
                        "settings-titlebar",
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(Theme::TITLEBAR_HEIGHT)),
                        cx,
                    );
                    let page = div()
                        .size_full()
                        .relative()
                        .child(settings_page)
                        .child(drag)
                        .children(overlays);
                    let frame = self.compact_frame(page, false, window, cx);
                    break 'ready root
                        .child(sidebar_tone)
                        // Clear of a phone's or tablet's edges.
                        .child(self.route_outlet(frame));
                }
                // A phone shows its Sessions page rather than the columns.
                if let Some(page) = self.render_compact_sessions(window, cx) {
                    break 'ready root.child(page);
                }
                // A phone's page for a view summoned beside the session, over it.
                if let Some(page) = self.render_compact_side(window, cx) {
                    break 'ready root.child(page);
                }
                let sidebar = self.render_sidebar(cx);
                // A collapsed or fit-hidden sidebar has no seam: the window's
                // own left edge sits there, and a press must resize the window
                // rather than drag the sidebar open.
                // The rail is no seam to drag.
                let sidebar_handle = (!self.settings.sidebar_collapsed && self.sidebar_now() > 0.5).then(|| {
                    self.resize_handle(
                        "sidebar-resize",
                        PaneResizeKind::Sidebar,
                        || SidebarResize,
                        |shell, cx| shell.reset_sidebar_width(cx),
                        cx,
                    )
                });
                // The session's view: its terminal and every pane split or summoned
                // beside it (`pane_frame`).
                let main = self.render_main(window, cx);
                let overlays = self.render_overlays(window.viewport_size(), window, cx);
                // Copied out (not held) — `render_title_bar` needs `cx` mutable.
                let border_color = Theme::of(cx).border;
                let card: AnyElement = div()
                    .id("conversation-column")
                    .debug_selector(|| "conversation-column".into())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(main)
                    .into_any_element();
                // The sidebar resize handle FLOATS over the sidebar/card seam
                // (zero layout width) so the sidebar's right gutter stays exactly
                // as wide as its left one.
                let sidebar_seam = div()
                    .w(px(0.0))
                    .h_full()
                    .flex_none()
                    .relative()
                    .children(
                        sidebar_handle
                            .map(|handle| handle.left(px(-PANE_RESIZE_HITBOX_HALF_WIDTH))),
                    );
                let title_bar = self.render_session_title_bar(window, cx);
                let compact_nav = self.render_compact_nav_bar(cx);
                // A phone's inspector, a sheet over the session.
                let compact_inspector = self.render_compact_inspector(window, cx);
                // Sidebar tone: a slightly lighter column behind the sidebar.
                // Its width rides the same tween as the sidebar, so the tone
                // melts away with the collapse instead of vanishing in a frame.
                let sidebar_now = self.sidebar_now();
                let sidebar_tone = Self::sidebar_tone(sidebar_now, border_color, window);
                // The content row spans the FULL window height — the titlebar
                // overlays it (glass, no fill), so the transcript can scroll
                // under the header and fade out at its edge. Columns that
                // must NOT underlap (sidebar content, the changes panel,
                // settings) pad themselves down by the titlebar height.
                let page = div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .child(sidebar)
                            .child(sidebar_seam)
                            .child(card),
                    )
                    // A phone's navigation bar stands in for the titlebar, its title
                    // and its sidebar and history cluster.
                    .when(compact_nav.is_none(), |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .child(title_bar),
                        )
                    })
                    .child(match compact_nav {
                        Some(bar) => div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .child(bar)
                            .into_any_element(),
                        None => self.render_titlebar_cluster(cx),
                    })
                    .children(compact_inspector)
                    .children(overlays);
                let session = matches!(self.route, Route::Chat) && {
                    let state = self.state.read(cx);
                    let chat = state.selected_chat.as_deref();
                    chat.is_some_and(|chat| state.session_terminal(chat).is_some())
                };
                shown_ground = session.then(|| self.shown_ground(cx));
                let frame = self.compact_frame(page, session, window, cx);
                root.child(sidebar_tone)
                    // Clear of a phone's or tablet's edges.
                    .child(self.route_outlet(frame))
            }
            GatePhase::Loading => root, // splash overlay covers boot
            GatePhase::Failed(error) => {
                let card = self.render_gate_card(error, cx);
                root.child(card)
            }
        };
        self.window_ground.follow(shown_ground, window, cx);
        // An iPad's keyboard shortcut bar holds the session's keys while its terminal types.
        self.publish_key_bar(window, cx);
        // Toasts float over every phase and route.
        // Never over a session's terminal (`toast_place`).
        let place = self.toast_place(window, cx);
        let root = root.children(crate::toast::render_toaster(place, window, cx));

        // A manually-driven tween is mid-flight: keep frames coming (the same
        // scheduling `with_animation` would have requested). Hover color fades
        // ride the same clock; their once-per-frame tick lives here (this is
        // the window's root render — it runs exactly once per frame).
        // The sidebar is a cached view, drawn from its last frame when only the shell is
        // drawn again: its hovered row reads no fade then, and the tick would drop the hover of
        // the row the pointer rests on, so the next sidebar frame painted it unlit under the
        // pointer while the frame shown kept its wash. The shell holds that row's hover read.
        if let Some(row) = &self.chat_status_hover {
            motion::hover_t(&format!("{row}-hover"));
        }
        if self.motion_active.get() | motion::hover_fades_active(cx.frame_time()) | motion::state_morphs_active(cx.frame_time()) {
            window.request_animation_frame();
        }

        // Boot splash overlay: visible → crossfades out on Ready → removed.
        let root = match self.splash {
            SplashPhase::Visible => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, false, cx.entity_id(), cx))
            }
            SplashPhase::FadingOut => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, true, cx.entity_id(), cx))
            }
            SplashPhase::Gone => root,
        };

        // Caption controls are shell-level chrome, not Ready-page content:
        // keep them above the splash and every auth/org/error gate as well as
        // the full application. Gate pages also need a drag surface because
        // they do not render the unified tabs/settings titlebar — on Windows
        // the native `Drag` control area, on Linux the explicit
        // `start_window_move` strip (the control-area hit-test is inert
        // there); macOS drags gate windows natively.
        let root = if matches!(gate, GatePhase::Ready)
            || cfg!(target_os = "macos")
        {
            root
        } else {
            root.child(
                self.titlebar_drag_region(
                    "gate-titlebar-drag",
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(Theme::TITLEBAR_HEIGHT)),
                    cx,
                ),
            )
        };
        let root = root
            .children(self.render_windows_caption_controls(window, cx))
            .children(self.render_linux_caption_controls(window, cx))
            // Last so the invisible CSD resize strips sit above every other
            // element at the window edges.
            .children(Self::render_linux_resize_borders(window));
        self.render_time = None;
        // Wheel notches anywhere in the window glide (Appearance → Motion).
        crate::smooth_scroll::smooth_scroll(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn chat_with_path(
        cwd: Option<&str>,
        source: Option<(&str, &str)>,
    ) -> slopty_view_proto::Chat {
        slopty_view_proto::Chat {
            id: "chat".into(),
            device_id: "remote-device".into(),
            title: None,
            archived: false,
            cwd: cwd.map(str::to_owned),
            branch: None,
            checkout_id: None,
            source_context: source.map(|(source_cwd, repo_root)| {
                slopty_view_proto::ConversationSourceContext {
                    checkout_id: "checkout".into(),
                    repo_root: repo_root.into(),
                    cwd: source_cwd.into(),
                    branch: "main".into(),
                    head_sha: None,
                    observed_at: chrono::Utc::now(),
                }
            }),
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            parent_chat_id: None,
            subagent: false,
            in_parent: false,
            draft: None,
        }
    }

    #[test]
    fn copy_path_copies_the_chat_cwd_not_the_canonical_repo_root() {
        let chat = chat_with_path(
            Some("/remote/repo/packages/app"),
            Some(("/remote/repo/packages/app", "/remote/repo")),
        );
        assert_eq!(chat_copy_path(&chat), Some("/remote/repo/packages/app"));

        let windows = chat_with_path(
            Some(r"C:\Users\me\repo"),
            Some((r"C:\Users\me\repo", r"\\?\C:\Users\me\repo")),
        );
        assert_eq!(chat_copy_path(&windows), Some(r"C:\Users\me\repo"));
    }

    #[test]
    fn copy_path_accepts_host_absolute_paths_from_any_os() {
        for cwd in [
            "/home/me/repo",
            r"C:\Users\me\repo",
            "D:/work/repo",
            r"\\server\share\repo",
        ] {
            let chat = chat_with_path(Some(cwd), None);
            assert_eq!(chat_copy_path(&chat), Some(cwd));
        }
    }

    #[test]
    fn copy_path_is_unavailable_without_an_absolute_path() {
        for cwd in [
            None,
            Some(""),
            Some("  "),
            Some("~"),
            Some("~/repo"),
            Some("."),
            Some("C:"),
        ] {
            let chat = chat_with_path(cwd, None);
            assert_eq!(chat_copy_path(&chat), None);
        }
    }

    #[test]
    fn narrowest_sidebar_keeps_its_options_button_beside_the_titlebar_controls() {
        for is_macos in [true, false] {
            let cluster_end = cluster_buttons_start(is_macos, false, 0) + CLUSTER_BUTTONS_WIDTH;
            let options_right = cluster_end + TITLEBAR_GROUP_GAP + 24.0;
            assert!(
                options_right + Theme::SPACE_SM <= SIDEBAR_MIN,
                "options button ends at {options_right} in a {SIDEBAR_MIN} sidebar"
            );
        }
    }

    #[test]
    fn sidebar_drag_nudges_each_edge_once_until_rearmed() {
        let min = sidebar_drag_sample(SIDEBAR_MIN, None, false);
        assert_eq!(min.width, SIDEBAR_MIN);
        assert_eq!(min.edge, Some(motion::ResizeEdge::Min));
        assert!(min.starts_bounce);

        let held_min = sidebar_drag_sample(SIDEBAR_MIN - 80.0, min.edge, false);
        assert_eq!(held_min.width, SIDEBAR_MIN);
        assert_eq!(held_min.edge, min.edge);
        assert!(!held_min.starts_bounce);

        let inside = sidebar_drag_sample(SIDEBAR_MIN + 1.0, held_min.edge, false);
        assert_eq!(inside.edge, None);
        assert!(!inside.starts_bounce);

        let rearmed_min = sidebar_drag_sample(SIDEBAR_MIN - 1.0, inside.edge, false);
        assert!(rearmed_min.starts_bounce);

        let max = sidebar_drag_sample(SIDEBAR_MAX, rearmed_min.edge, false);
        assert_eq!(max.width, SIDEBAR_MAX);
        assert_eq!(max.edge, Some(motion::ResizeEdge::Max));
        assert!(max.starts_bounce);

        let held_max = sidebar_drag_sample(SIDEBAR_MAX + 80.0, max.edge, false);
        assert_eq!(held_max.width, SIDEBAR_MAX);
        assert!(!held_max.starts_bounce);
    }

    #[test]
    fn sidebar_drag_stays_exact_in_range_and_reduced_motion_never_nudges() {
        let middle = sidebar_drag_sample(312.0, None, false);
        assert_eq!(middle.width, 312.0);
        assert_eq!(middle.edge, None);
        assert!(!middle.starts_bounce);

        for pointer_x in [
            SIDEBAR_MIN - 100.0,
            SIDEBAR_MIN,
            SIDEBAR_MAX,
            SIDEBAR_MAX + 100.0,
        ] {
            let sample = sidebar_drag_sample(pointer_x, None, true);
            assert!((SIDEBAR_MIN..=SIDEBAR_MAX).contains(&sample.width));
            assert!(!sample.starts_bounce);
        }
    }

    #[test]
    fn sidebar_bounce_has_rounded_out_and_return_phases() {
        assert_eq!(
            motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.0),
            0.0
        );
        assert_eq!(
            motion::resize_bounce_offset(
                motion::ResizeEdge::Max,
                motion::RESIZE_EDGE_BOUNCE_OUT_FRACTION
            ),
            motion::RESIZE_EDGE_NUDGE
        );
        assert_eq!(
            motion::resize_bounce_offset(motion::ResizeEdge::Max, 1.0),
            0.0
        );

        let gentle_start = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.01);
        let outbound = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.2);
        let returning = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.7);
        assert!(gentle_start > 0.0 && gentle_start < 0.1);
        assert!(outbound > gentle_start && outbound < motion::RESIZE_EDGE_NUDGE);
        assert!(returning > 0.0 && returning < motion::RESIZE_EDGE_NUDGE);
        assert_eq!(
            motion::resize_bounce_offset(motion::ResizeEdge::Min, 0.2),
            -outbound
        );
    }

    #[test]
    fn every_default_shortcut_binds_on_this_platform() {
        // `apply_keymap` silently falls back on an unparseable combo, so a
        // default gpui cannot parse would ship as a dead shortcut.
        for id in crate::settings::ShortcutId::ALL {
            let combo = platform_combo(id.default_combo());
            assert!(
                Keystroke::parse_with_os(&combo, crate::keys::OS).is_ok(),
                "{} default {combo:?} does not parse",
                id.label()
            );
        }
    }

    /// A minute boundary.
    fn clock_epoch() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn clock_session(
        chat_id: &str,
        status: slopty_view_proto::SessionStatus,
        updated_at: DateTime<Utc>,
    ) -> slopty_view_proto::Session {
        slopty_view_proto::Session {
            last_completed_turn: None,
            chat_id: chat_id.into(),
            device_id: "remote".into(),
            status,
            started_at: Some(updated_at),
            updated_at,
            running_subagents: 0,
            status_since: None,
            waiting_on: None,
        }
    }

    struct ClockProbe {
        clock: ClockRedraw,
    }

    fn clock_probe(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::Entity<ClockProbe>,
        std::rc::Rc<std::cell::Cell<usize>>,
        gpui::Subscription,
    ) {
        use gpui::AppContext as _;
        let probe = cx.new(|_| ClockProbe {
            clock: ClockRedraw::default(),
        });
        let redraws = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = redraws.clone();
        let subscription =
            cx.update(|cx| cx.observe(&probe, move |_, _| counter.set(counter.get() + 1)));
        (probe, redraws, subscription)
    }

    fn arm_probe(
        cx: &mut gpui::TestAppContext,
        probe: &gpui::Entity<ClockProbe>,
        wake: DateTime<Utc>,
        now: DateTime<Utc>,
    ) {
        probe.update(cx, |probe, cx| {
            probe
                .clock
                .arm(wake, now, |probe: &mut ClockProbe| &mut probe.clock, cx)
        });
    }

    /// Schedules the shell clock for `state` at `now` and proves the redraw
    /// lands exactly at `expected` on the simulated clock — not a millisecond
    /// earlier, and without any intermediate polling.
    fn assert_redraw_at(
        cx: &mut gpui::TestAppContext,
        state: &AppState,
        now: DateTime<Utc>,
        expected: DateTime<Utc>,
    ) {
        let wake = shell_clock_wake(state, now);
        assert_eq!(wake, expected);
        let (probe, redraws, _subscription) = clock_probe(cx);
        arm_probe(cx, &probe, wake, now);
        let delay = (expected - now).to_std().unwrap();
        cx.executor()
            .advance_clock(delay - Duration::from_millis(1));
        assert_eq!(redraws.get(), 0, "no redraw before the deadline");
        cx.executor().advance_clock(Duration::from_millis(1));
        assert_eq!(redraws.get(), 1, "one redraw at the deadline");
    }

    #[gpui::test]
    fn idle_shell_sleeps_until_the_minute(cx: &mut gpui::TestAppContext) {
        let now = clock_epoch() + chrono::TimeDelta::seconds(20);
        let mut state = AppState::new();
        // A failed run, a pending question and a reconnecting pill show no
        // clock: only the relative-time minute remains.
        state.sessions = vec![
            clock_session("failed", slopty_view_proto::SessionStatus::Errored, now),
            clock_session("idle", slopty_view_proto::SessionStatus::Idle, now),
        ];
        state.selected_chat = Some("failed".into());
        state.connectivity.state = slopty_view_proto::ConnectivityState::Reconnecting;
        assert_redraw_at(
            cx,
            &state,
            now,
            clock_epoch() + chrono::TimeDelta::minutes(1),
        );
    }

    #[gpui::test]
    fn selected_working_chat_redraws_every_second(cx: &mut gpui::TestAppContext) {
        let now = clock_epoch();
        let mut state = AppState::new();
        state.sessions = vec![clock_session(
            "chat",
            slopty_view_proto::SessionStatus::Working,
            now,
        )];
        state.selected_chat = Some("chat".into());
        assert_redraw_at(cx, &state, now, now + chrono::TimeDelta::seconds(1));
        // Awaiting input shows no elapsed timer; its staleness cutoff is an
        // AppState notification, not a shell tick.
        state.sessions[0].status = slopty_view_proto::SessionStatus::AwaitingInput;
        assert_redraw_at(cx, &state, now, now + chrono::TimeDelta::minutes(1));
    }

    #[gpui::test]
    fn clock_redraw_keeps_the_earliest_deadline(cx: &mut gpui::TestAppContext) {
        let now = clock_epoch();
        let (probe, redraws, _subscription) = clock_probe(cx);
        arm_probe(cx, &probe, now + chrono::TimeDelta::seconds(10), now);
        // An earlier deadline replaces the pending wake; a later one does not.
        arm_probe(cx, &probe, now + chrono::TimeDelta::seconds(5), now);
        arm_probe(cx, &probe, now + chrono::TimeDelta::seconds(30), now);
        cx.executor().advance_clock(Duration::from_secs(5));
        assert_eq!(redraws.get(), 1);
        // Fired timers are one-shot: nothing else is pending.
        cx.executor().advance_clock(Duration::from_secs(60));
        assert_eq!(redraws.get(), 1);
        assert!(probe.read_with(cx, |probe, _| probe.clock.task.is_none()));
    }


    #[test]
    fn pane_resize_hitboxes_yield_the_titlebar_chrome() {
        assert_eq!(PANE_RESIZE_HITBOX_TOP, Theme::TITLEBAR_HEIGHT);
        assert_eq!(PANE_RESIZE_HITBOX_HALF_WIDTH * 2.0, 20.0);
    }






    // A remote window's chip matches its picker row's glyph.
    #[test]
    fn a_remote_window_chip_wears_its_kind() {
        assert_eq!(screen_chip_glyph(Some("window:42")), icons::WINDOW_RESTORE);
        assert_eq!(screen_chip_glyph(Some("display:1")), icons::MONITOR);
        assert_eq!(screen_chip_glyph(None), icons::MONITOR, "nothing chosen yet");
    }

    #[test]
    fn titlebar_cluster_matches_slopty_window_controls() {
        // 88, or 12 in fullscreen: the
        // cluster clears the {14,15} traffic lights, and reclaims the inset
        // when fullscreen hides them.
        assert_eq!(titlebar_cluster_start(false), 88.0);
        assert_eq!(titlebar_cluster_start(true), 12.0);
        assert_eq!(TITLEBAR_CONTROL_GAP, 2.0);
        assert_eq!(TITLEBAR_GROUP_GAP, Theme::SPACE_SM);
        assert_eq!(TITLEBAR_IDENTITY_GAP, Theme::SPACE_MD);
        assert_eq!(CLUSTER_BUTTONS_WIDTH, 24.0);
        assert_eq!(TITLEBAR_ACTION_EDGE_INSET, 6.0);
    }

    #[test]
    fn windows_caption_controls_reserve_titlebar_space() {
        assert_eq!(titlebar_right_padding(true, 0, 16.0), 124.0);
        assert_eq!(titlebar_right_padding(false, 0, 16.0), 16.0);
    }

    #[test]
    fn linux_caption_controls_reserve_titlebar_space() {
        // 24px buttons on the cluster's 2px rhythm.
        assert_eq!(caption_buttons_width(0), 0.0);
        assert_eq!(caption_buttons_width(1), 24.0);
        assert_eq!(caption_buttons_width(3), 76.0);
        // Right-side captions (the Linux default: minimize,maximize,close):
        // content pads past the 10px edge inset + the button row.
        assert_eq!(titlebar_right_padding(false, 3, 16.0), 16.0 + 10.0 + 76.0);
        // GNOME-vanilla ":close" — a single right button.
        assert_eq!(titlebar_right_padding(false, 1, 16.0), 16.0 + 10.0 + 24.0);
        // Left-side captions ("close:…" layouts) shift the app cluster right
        // by the button row + one 2px gap.
        assert_eq!(cluster_buttons_start(false, false, 0), 10.0);
        assert_eq!(cluster_buttons_start(false, false, 1), 10.0 + 24.0 + 2.0);
        assert_eq!(cluster_buttons_start(false, false, 3), 10.0 + 76.0 + 2.0);
        // macOS ignores the Linux caption count entirely.
        assert_eq!(cluster_buttons_start(true, false, 3), 88.0);
    }

    #[test]
    fn a_files_view_is_titled_by_its_name() {
        assert_eq!(workspace_file_title("src/árbol.rs"), "árbol.rs");
    }

    // ---- navigation history (titlebar back/forward) ----

    fn chat(id: &str) -> NavEntry {
        NavEntry::Chat(id.to_string())
    }

    #[test]
    fn nav_history_starts_with_nothing_to_walk() {
        let nav = NavHistory::new(chat(""));
        assert!(!nav.can_back());
        assert!(!nav.can_forward());
        assert_eq!(*nav.current(), chat(""));
    }

    #[test]
    fn nav_push_then_back_and_forward() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("b"));
        nav.push(NavEntry::Settings(SettingsSection::Devices));
        assert!(nav.can_back());
        assert!(!nav.can_forward());

        // Back walks toward the oldest entry without dropping anything.
        assert_eq!(
            nav.back(),
            Some(chat("b")),
            "back lands on the previous route"
        );
        assert_eq!(nav.back(), Some(chat("a")));
        assert!(!nav.can_back());
        assert!(nav.can_forward());
        assert_eq!(nav.back(), None, "past the oldest entry is a no-op");

        // Forward retraces the same path.
        assert_eq!(nav.forward(), Some(chat("b")));
        assert_eq!(
            nav.forward(),
            Some(NavEntry::Settings(SettingsSection::Devices))
        );
        assert!(!nav.can_forward());
        assert_eq!(nav.forward(), None);
    }

    #[test]
    fn nav_push_dedups_the_current_route() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("a"));
        nav.push(chat("a"));
        assert_eq!(nav.len(), 1, "re-selecting the current route never stacks");
        nav.push(NavEntry::Settings(SettingsSection::Harnesses));
        nav.push(NavEntry::Settings(SettingsSection::Harnesses));
        assert_eq!(nav.len(), 2);
    }

    #[test]
    fn nav_push_truncates_the_forward_branch() {
        // a → b → c, back to a, then push d: the b/c branch is gone (browser
        // semantics: a push truncates the entries ahead).
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("b"));
        nav.push(chat("c"));
        nav.back();
        nav.back();
        assert_eq!(*nav.current(), chat("a"));
        assert!(nav.can_forward());
        nav.push(chat("d"));
        assert!(!nav.can_forward(), "the old branch is unreachable");
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.back(), Some(chat("a")));
        assert_eq!(nav.forward(), Some(chat("d")));
    }

    #[test]
    fn nav_replace_swaps_in_place() {
        // The boot auto-select replaces the untouched canvas entry, so Back
        // stays disabled after landing in the last-used chat.
        let mut nav = NavHistory::new(chat(""));
        nav.replace(chat("boot"));
        assert_eq!(nav.len(), 1);
        assert_eq!(*nav.current(), chat("boot"));
        assert!(!nav.can_back());
    }

    #[test]
    fn nav_settings_sections_are_distinct_entries() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(NavEntry::Settings(SettingsSection::Devices));
        nav.push(NavEntry::Settings(SettingsSection::Shortcuts));
        assert_eq!(nav.len(), 3, "section changes are navigations");
        assert_eq!(
            nav.back(),
            Some(NavEntry::Settings(SettingsSection::Devices))
        );
        assert_eq!(nav.back(), Some(chat("a")));
    }

    #[test]
    fn sidebar_disclosure_motion_lands_exactly_on_its_target() {
        let now = std::time::Instant::now();
        let mut tween = SidebarDisclosureMotion::new(1, 240.0, 0.0, now);
        tween.started = now - motion::COLLAPSE.total().mul_f32(2.0);
        assert_eq!(tween.current(now), 0.0);
        assert!(!tween.animating(now));
    }
}

#[cfg(test)]
mod exit_regressions {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn new_shell_only_shows_boot_splash_while_connecting(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let boot = EngineBootConfig {
            data_dir: dir.path().into(),
            default_harness: slopty_view_proto::HarnessId::MOCK,
        };
        for (connection, expected) in [
            (ConnectionStatus::Connecting, SplashPhase::Visible),
            (ConnectionStatus::Ready, SplashPhase::Gone),
            (
                ConnectionStatus::Failed("offline".into()),
                SplashPhase::Gone,
            ),
        ] {
            let window = cx.add_window(|_, cx| {
                let state = cx.new(|_| {
                    let mut state = AppState::new();
                    state.connection = connection;
                    state
                });
                Shell::new(state, boot.clone(), cx)
            });
            window
                .update(cx, |shell, _, _| assert_eq!(shell.splash, expected))
                .unwrap();
        }
    }


    #[gpui::test]
    fn pane_geometry_uses_one_animation_time_per_frame(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, _, cx| {
                let duration = RESIZE.total().mul_f32(motion::speed_scale());
                let started = std::time::Instant::now() - duration.mul_f32(2.);
                let tween = Some(WidthTween {
                    from: 520.,
                    to: 0.,
                    started,
                    duration,
                    curve: RESIZE.curve,
                });
                // Motion on, read as a render would before the tween's frame.
                crate::motion::set_reduced_motion(cx, false);
                shell.reduced_motion = motion::ReducedMotion::of(cx);
                // The frame started halfway through the transition but rendering
                // crossed its deadline. Every region must still use that frame.
                shell.render_time = Some(started + duration.mul_f32(0.5));
                assert!(shell.tween_active(tween));
                assert_eq!(shell.active_tween_endpoints(tween), Some((520., 0.)));
                let width = shell.eval_tween(tween, 0.);
                assert!(width > 0. && width < 520.);
                assert_eq!(shell.eval_tween(tween, 0.), width);
                shell.settings.sidebar_collapsed = true;
                // Put away, the sidebar keeps its rail, so its closing tween ends there.
                let closing = tween.map(|tween| WidthTween {
                    to: sidebar::SIDEBAR_RAIL_WIDTH,
                    ..tween
                });
                shell.sidebar_tween = closing;
                shell.toggle_sidebar(cx);
                assert_eq!(
                    shell.sidebar_tween.unwrap().from,
                    shell.eval_tween(closing, 0.),
                    "reopening must not restart from the rail"
                );
                shell.render_time = None;
                assert!(!shell.tween_active(tween));
                assert_eq!(shell.active_tween_endpoints(tween), None);
                assert_eq!(shell.eval_tween(tween, 0.), 0.);
            })
            .unwrap();
    }

    #[gpui::test]
    fn panel_saves_preserve_settings_selected_outside_the_shell(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        for index in 0..5 {
            let pause_in_background = index % 2 == 1;
            let open_links_in_slopty = index % 2 == 0;
            let terminal_family = if open_links_in_slopty {
                crate::typography::UiFontFamily::System
            } else {
                crate::typography::UiFontFamily::Geist
            };
            let code_family = if open_links_in_slopty {
                crate::typography::UiFontFamily::Geist
            } else {
                crate::typography::UiFontFamily::System
            };
            let terminal_size = 15.0 + index as f32;
            let code_size = 11.0 + index as f32;
            let conversation_width = settings::ConversationWidth::ALL[index % 3];
            let geometry = Some(settings::WindowGeometry {
                display_uuid: Some(uuid::Uuid::from_u128(7)),
                x: 80.0 + index as f32,
                y: 60.0,
                width: 1100.0,
                height: 750.0,
            });
            window
                .update(cx, |shell, _, cx| {
                    // Selection changes in Appearance, independently of the shell's
                    // cached snapshot. Include a previously queued geometry save.
                    shell.settings.sidebar_width = 280.0;
                    shell.schedule_save(cx);
                    settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                        settings.pause_animations_in_background = pause_in_background;
                        settings.window_geometry = geometry;
                        settings.open_web_links_in_slopty = open_links_in_slopty;
                        settings.terminal_font_family = terminal_family.clone();
                        settings.terminal_font_size = terminal_size;
                        settings.code_font_family = code_family.clone();
                        settings.code_font_size = code_size;
                        settings.conversation_width = conversation_width;
                    });
                    for step in 0..3 {
                        shell.settings.sidebar_width = 290.0 + step as f32;
                        shell.settings.right_pane_width = 540.0 + step as f32;
                        shell.settings.terminal_height = 300.0 + step as f32;
                        shell.schedule_save(cx);
                        let current = settings::current(cx);
                        assert_eq!(current.pause_animations_in_background, pause_in_background);
                        assert_eq!(current.window_geometry, geometry);
                        assert_eq!(current.open_web_links_in_slopty, open_links_in_slopty);
                        assert_eq!(current.terminal_font_family, terminal_family);
                        assert_eq!(current.terminal_font_size, terminal_size);
                        assert_eq!(current.code_font_family, code_family);
                        assert_eq!(current.code_font_size, code_size);
                        assert_eq!(current.conversation_width, conversation_width);
                    }
                    settings::flush(cx);
                    let loaded = settings::UiSettings::load(dir.path());
                    assert_eq!(loaded.window_geometry, geometry);
                    assert_eq!(loaded.pause_animations_in_background, pause_in_background);
                    assert_eq!(loaded.open_web_links_in_slopty, open_links_in_slopty);
                    assert_eq!(loaded.terminal_font_family, terminal_family);
                    assert_eq!(loaded.terminal_font_size, terminal_size);
                    assert_eq!(loaded.code_font_family, code_family);
                    assert_eq!(loaded.code_font_size, code_size);
                    assert_eq!(loaded.conversation_width, conversation_width);
                    assert_eq!(loaded.sidebar_width, 292.0);
                    assert_eq!(loaded.right_pane_width, 542.0);
                    assert_eq!(loaded.terminal_height, 302.0);
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn shell_saves_never_revert_settings_written_outside_the_shell(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let before = cx.update(|cx| settings::current(cx));
        window
            .update(cx, |shell, _, cx| {
                // Toggles that write the store directly (the notification and diff switches,
                // ...).
                settings::update(SavePolicy::Immediate, cx, |s| {
                    s.notifications_enabled = !before.notifications_enabled;
                    s.diff_wrap = !before.diff_wrap;
                });
                // Navigating away saves the shell's own working copy.
                shell.remember_settings_section(SettingsSection::Notifications, cx);
                // The shell's own writes still land.
                shell.settings.sidebar_width += 10.0;
                shell.schedule_save(cx);
            })
            .unwrap();
        let after = cx.update(|cx| settings::current(cx));
        assert_eq!(after.notifications_enabled, !before.notifications_enabled);
        assert_eq!(after.diff_wrap, !before.diff_wrap);
        assert_eq!(
            after.settings_section,
            SettingsSection::Notifications
        );
        assert_eq!(after.sidebar_width, before.sidebar_width + 10.0);
    }

    #[gpui::test]
    fn shell_saves_keep_motion_settings_chosen_in_appearance(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, _, cx| {
                // The Appearance page writes these straight to the store,
                // outside the shell's working copy.
                settings::update(SavePolicy::Immediate, cx, |settings| {
                    settings.pause_animations_in_background = true;
                });
                settings::set_smooth_scrolling(false, cx);
                // Any shell-owned change (sidebar width, panels) republishes
                // the working copy; it must not revert the motion choices.
                shell.schedule_save(cx);
                let saved = settings::current(cx);
                assert!(saved.pause_animations_in_background);
                assert!(!saved.smooth_scrolling);
            })
            .unwrap();
    }

    #[gpui::test]
    fn shell_saves_keep_every_setting_chosen_outside_the_shell(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, _, cx| {
                // Voice, transcript and diff controls save straight to the
                // store. None of them is known to the Shell.
                settings::update(SavePolicy::Immediate, cx, |settings| {
                    settings.diff_split = true;
                    settings.diff_wrap = true;
                });
                // Leaving Settings and resizing both republish the Shell's copy.
                shell.settings.settings_section = SettingsSection::Appearance;
                shell.settings.sidebar_width = 300.0;
                shell.schedule_save(cx);
                let saved = settings::current(cx);
                assert!(saved.diff_split);
                assert!(saved.diff_wrap);
                assert_eq!(saved.settings_section, SettingsSection::Appearance);
                assert_eq!(saved.sidebar_width, 300.0);
                // An outside change to a field the Shell also holds still wins
                // once the Shell has nothing newer of its own.
                settings::update(SavePolicy::Immediate, cx, |settings| {
                    settings.diff_split = false;
                    settings.sidebar_width = 320.0;
                });
                shell.schedule_save(cx);
                let saved = settings::current(cx);
                assert!(!saved.diff_split);
                assert_eq!(saved.sidebar_width, 320.0);
                assert_eq!(shell.settings.sidebar_width, 320.0);
            })
            .unwrap();
    }

    #[gpui::test]
    fn sidebar_settings_trigger_toggles_the_settings_route(cx: &mut TestAppContext) {
        // Just the footer row: the full shell's settings modal would cover it.
        struct FooterHost(Entity<Shell>);
        impl Render for FooterHost {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.0.update(cx, |shell, cx| {
                    div()
                        .w(px(256.0))
                        .child(shell.render_sidebar_footer(&Theme::default(), cx))
                })
            }
        }

        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            FooterHost(cx.new(|cx| {
                let state = cx.new(|_| AppState::new());
                Shell::new(
                    state,
                    EngineBootConfig {
                        data_dir: dir.path().into(),
                        default_harness: slopty_view_proto::HarnessId::MOCK,
                    },
                    cx,
                )
            }))
        });
        let shell = host.read_with(cx, |host, _| host.0.clone());
        for expect_open in [true, false] {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert!(cx.debug_bounds("user-menu-label").is_some());
            let trigger = cx.debug_bounds("settings-trigger").unwrap().center();
            cx.simulate_click(trigger, gpui::Modifiers::default());
            shell.read_with(cx, |shell, _| {
                if expect_open {
                    assert!(matches!(
                        shell.route,
                        Route::Settings(SettingsSection::Appearance)
                    ));
                } else {
                    assert!(matches!(shell.route, Route::Chat));
                }
            });
        }
    }

    /// A shell split beside the session takes the keyboard once it draws; ordinary redraws after
    /// leave a focus elsewhere where it is.
    #[gpui::test]
    fn a_split_shell_takes_the_keyboard_once(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let panel = window
            .update(cx, |shell, _, cx| {
                shell.open_chat("terminal-session".into(), cx);
                shell.active_chat = "terminal-session".into();
                shell.split_pane(None, cx);
                let panes = shell.panes_now(cx);
                let panes::Pane::Shell(n) = panes.focused() else {
                    panic!("the shell is the pane focused: {panes:?}");
                };
                shell.pane_shells[&n].clone()
            })
            .unwrap();
        let terminal_window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |_, _| panel.clone()).unwrap()
        });
        cx.update_window(terminal_window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            assert!(panel.read(cx).focus_handle().is_focused(window));
            let other_input = cx.focus_handle();
            window.focus(&other_input, cx);
            window.draw(cx).clear(cx);
            assert!(other_input.is_focused(window), "a redraw steals nothing");
        })
        .unwrap();
    }

    /// Landing on a session (opening it, the way back) gives its terminal the keyboard
    /// once; a settings visit does not, and nothing reclaims it after a click away.
    #[gpui::test]
    fn session_navigation_focuses_the_terminal_once(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        // The session's terminal in its own window, to exercise mounting without the shell's
        // boot/connection gate hiding the destination.
        let terminal_window = cx.update(|cx| {
            let terminal = window.read(cx).unwrap().session_terminal.clone();
            cx.open_window(gpui::WindowOptions::default(), |_, _| terminal).unwrap()
        });
        for destination in ["chat", "chat", "back", "settings"] {
            window
                .update(cx, |shell, _, cx| match destination {
                    "chat" => shell.open_chat("existing-session".into(), cx),
                    "back" => shell.apply_nav(NavEntry::Chat("existing-session".into()), cx),
                    _ => {
                        shell.open_settings(SettingsSection::Devices, cx);
                        shell.close_settings(cx);
                    }
                })
                .unwrap();
            cx.update_window(terminal_window.into(), |terminal, window, cx| {
                window.draw(cx).clear(cx);
                let focused = terminal
                    .downcast::<TerminalPanel>()
                    .unwrap()
                    .read(cx)
                    .focus_handle()
                    .is_focused(window);
                assert_eq!(focused, destination != "settings", "{destination}");
                window.blur(cx);
                window.draw(cx).clear(cx);
                assert!(window.focused(cx).is_none(), "{destination}");
            })
            .unwrap();
        }
    }

    /// New session opens its picker on the project asked for, else the open session's.
    #[gpui::test]
    fn new_session_opens_its_picker_on_the_project(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let space = |id: &str| slopty_view_proto::Space {
            id: id.into(),
            device_id: "local".into(),
            path: format!("/{id}"),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            repository_id: None,
            created_at: Utc::now(),
        };
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_spaces(vec![space("one"), space("two")]);
                });
                let first = |shell: &Shell, cx: &App| {
                    shell.new_session_rows(cx).first().map(|space| space.id.clone())
                };
                shell.open_new_session(Some("two".into()), cx);
                assert!(shell.new_session.is_some(), "the picker is open");
                assert!(shell.overlay_owns_keyboard(), "and owns the keyboard");
                assert_eq!(first(shell, cx).as_deref(), Some("two"), "the project asked for first");
                shell.close_new_session_picker(cx);
                assert!(shell.new_session.is_none());
            })
            .unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[gpui::test]
    fn transcript_links_open_new_tabs_and_reject_stale_sessions(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let weak = window
            .update(cx, |shell, window, cx| {
                use crate::markdown::render::{
                    LinkAction, LinkActivation, LinkOutcome, LinkTarget,
                };
                shell.active_chat = "first-session".into();
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("first-session".into())
                });
                let mut activation = LinkActivation {
                    target: LinkTarget::new("Docs", "https://example.com/docs"),
                    action: LinkAction::Primary,
                    source_session: Some("first-session".into()),
                };
                assert_eq!(shell.front_view(cx), None);
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                let first = shell.browser_seq;
                assert_eq!(
                    shell.browsers[&first].read(cx).page.url.as_deref(),
                    Some("https://example.com/docs")
                );
                assert_eq!(
                    shell.front_view(cx),
                    Some(RightSurface::Browser(first)),
                    "the page opens as a pane beside the session, with the keyboard"
                );
                shell.activate_session_link(&activation, window, cx);
                assert_eq!(shell.browsers.len(), 2);
                settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                    settings.open_web_links_in_slopty = false;
                });
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::External("https://example.com/docs".into())
                );
                assert_eq!(shell.browsers.len(), 2);
                activation.action = LinkAction::Internal;
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal,
                    "the explicit internal action ignores the default preference"
                );
                assert_eq!(shell.browsers.len(), 3);
                activation.action = LinkAction::External;
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::External("https://example.com/docs".into())
                );
                assert_eq!(shell.browsers.len(), 3);
                activation.source_session = Some("other-session".into());
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Rejected
                );
                activation.source_session = Some("first-session".into());
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("switch-in-progress".into())
                });
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Rejected
                );
                assert_eq!(shell.browsers.len(), 3);
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("first-session".into())
                });
                activation.action = LinkAction::Internal;
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                assert_eq!(shell.browsers.len(), 4);
                let weak = shell.browsers[&first].downgrade();
                shell.close_view(RightSurface::Browser(first), cx);
                weak
            })
            .unwrap();
        cx.run_until_parked();
        assert!(weak.upgrade().is_none());
    }

    /// Encoded destinations open the decoded path, and an absolute
    /// destination no root owns opens through the linking chat's own file
    /// context with its absolute path intact.
    #[cfg(unix)]
    #[gpui::test]
    fn encoded_and_outside_file_links_open_in_the_linking_chats_context(cx: &mut TestAppContext) {
        use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("2026-09-26/Some Folder")).unwrap();
        std::fs::write(
            root.join("2026-09-26/Some Folder/it's here.txt"),
            "encoded\n",
        )
        .unwrap();
        let outside = dir.path().join("outside/INFORME.md");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, "# Informe\n").unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                let mut owner = super::tests::chat_with_path(Some(&root.to_string_lossy()), None);
                owner.id = "owner".into();
                owner.device_id = "local".into();
                shell.active_chat = "owner".into();
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("local".into());
                    state.apply_chats(vec![owner]);
                    state.selected_chat = Some("owner".into());
                });

                let mut activation = LinkActivation {
                    target: LinkTarget::new("enc", "2026-09-26/Some%20Folder/it%27s%20here.txt"),
                    action: LinkAction::Primary,
                    source_session: Some("owner".into()),
                };
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                let id = shell.file_surface_seq;
                assert_eq!(
                    shell.file_surface_paths.get(&id).map(String::as_str),
                    Some("2026-09-26/Some Folder/it's here.txt")
                );

                // The absolute spelling of the same file resolves in-root.
                let absolute = format!(
                    "{}/2026-09-26/Some%20Folder/it%27s%20here.txt",
                    root.display()
                );
                activation.target = LinkTarget::new("abs", &absolute);
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                let id = shell.file_surface_seq;
                assert_eq!(
                    shell.file_surface_paths.get(&id).map(String::as_str),
                    Some("2026-09-26/Some Folder/it's here.txt")
                );
                assert_eq!(shell.file_surfaces[&id].read(cx).chat_id(), "owner");

                // Outside every known root the absolute path still opens,
                // through the linking chat.
                let outside_target = outside.to_string_lossy().into_owned();
                activation.target = LinkTarget::new("outside", &outside_target);
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                let id = shell.file_surface_seq;
                assert_eq!(
                    shell.file_surface_paths.get(&id).map(String::as_str),
                    Some(outside_target.as_str())
                );
                assert_eq!(shell.file_surfaces[&id].read(cx).chat_id(), "owner");

                // A path under another project root on this device keeps its
                // absolute spelling: its root-relative tail would name a
                // different file under the linking chat's checkout.
                let project = dir.path().join("other-project");
                shell.state.update(cx, |state, _| {
                    state.apply_spaces(vec![slopty_view_proto::Space {
                        id: "other".into(),
                        device_id: "local".into(),
                        path: project.to_string_lossy().into_owned(),
                        name: None,
                        git_detected: false,
                        git_checked_at: None,
                        checkout_id: None,
                        repository_id: None,
                        created_at: Utc::now(),
                    }]);
                });
                let project_target = project.join("notes.md").to_string_lossy().into_owned();
                activation.target = LinkTarget::new("project", &project_target);
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                let id = shell.file_surface_seq;
                assert_eq!(
                    shell.file_surface_paths.get(&id).map(String::as_str),
                    Some(project_target.as_str())
                );
                assert_eq!(shell.file_surfaces[&id].read(cx).chat_id(), "owner");
            })
            .unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[gpui::test]
    fn markdown_preview_events_open_browser_from_files_and_a_file(cx: &mut TestAppContext) {
        use crate::markdown::render::{LinkAction, LinkActivation, LinkTarget};
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let surfaces = window
            .update(cx, |shell, window, cx| {
                shell.active_chat = "owner".into();
                shell
                    .state
                    .update(cx, |state, _| state.selected_chat = Some("owner".into()));
                shell.summon_files(window, cx);
                shell.add_file_surface("README.md".into(), window, cx);
                [
                    shell.files[&shell.panel_key(cx)].clone(),
                    shell.file_surfaces[&shell.file_surface_seq].clone(),
                ]
            })
            .unwrap();
        let mut last_external_url = None;
        for (index, surface) in surfaces.iter().enumerate() {
            let mut activation = LinkActivation {
                target: LinkTarget::new("Docs", &format!("https://example.com/preview/{index}")),
                action: LinkAction::Primary,
                source_session: Some("owner".into()),
            };
            surface.update(cx, |_, cx| {
                cx.emit(FilesEvent::OpenWebLink(activation.clone()))
            });
            cx.run_until_parked();
            assert_eq!(cx.opened_url(), last_external_url);
            window
                .update(cx, |shell, _, cx| {
                    assert_eq!(shell.browsers.len(), index + 1);
                    assert_eq!(
                        shell.front_view(cx),
                        Some(RightSurface::Browser(shell.browser_seq))
                    );
                    assert_eq!(
                        shell.browsers[&shell.browser_seq]
                            .read(cx)
                            .page
                            .url
                            .as_deref(),
                        Some(activation.target.original.as_str())
                    );
                })
                .unwrap();
            activation.action = LinkAction::External;
            surface.update(cx, |_, cx| {
                cx.emit(FilesEvent::OpenWebLink(activation.clone()))
            });
            cx.run_until_parked();
            assert_eq!(
                cx.opened_url().as_deref(),
                Some(activation.target.original.as_str())
            );
            last_external_url = Some(activation.target.original.clone());
            activation.target = LinkTarget::new("Stale", "https://example.com/stale");
            activation.source_session = Some("stale-owner".into());
            for action in [LinkAction::Internal, LinkAction::External] {
                activation.action = action;
                surface.update(cx, |_, cx| {
                    cx.emit(FilesEvent::OpenWebLink(activation.clone()))
                });
                cx.run_until_parked();
                assert_eq!(cx.opened_url(), last_external_url);
            }
            window
                .update(cx, |shell, _, _| {
                    assert_eq!(shell.browsers.len(), index + 1)
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn browser_profile_changes_clear_tabs_and_reselect_the_stable_identity(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.active_chat = "session".into();
                shell.state.update(cx, |state, _| {
                    state.workspace_scope = Some(slopty_view_proto::WorkspaceScope::Local);
                    state.local_device_id = Some("device-a".into());
                });
                // Selecting the identity happens before navigation, without requiring a render.
                shell.add_browser_surface(None, window, cx);
                let original = shell.browser_profile.clone().unwrap();
                shell.active_chat = "another-chat".into();
                shell.add_browser_surface(None, window, cx);
                assert_eq!(shell.browsers.len(), 2);
                assert_eq!(shell.browser_profile.as_ref(), Some(&original));
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("device-b".into())
                });
                shell.sync_browser_profile(cx);
                assert!(shell.browsers.is_empty());
                assert!(shell.browser_subs.is_empty());
                assert_ne!(shell.browser_profile.as_ref(), Some(&original));
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("device-a".into())
                });
                shell.sync_browser_profile(cx);
                assert_eq!(shell.browser_profile.as_ref(), Some(&original));
                shell.add_browser_surface(None, window, cx);
                shell
                    .state
                    .update(cx, |state, _| state.workspace_scope = None);
                shell.sync_browser_profile(cx);
                assert!(shell.browser_profile.is_none());
                assert!(shell.browsers.is_empty());
            })
            .unwrap();
    }

    /// A page opens as a pane of its own session's view and lets go of its browser when it
    /// closes; a page closed without the keyboard leaves the keyboard where it was.
    #[gpui::test]
    fn pages_keep_their_session_and_let_go_on_close(cx: &mut TestAppContext) {
        use super::panes::Pane;
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let weak = window
            .update(cx, |shell, window, cx| {
                // The new-session canvas never gathers pages nobody owns.
                shell.add_browser_surface(None, window, cx);
                assert!(shell.browsers.is_empty());
                shell.active_chat = "first-session".into();
                shell.add_browser_surface(None, window, cx);
                let first = RightSurface::Browser(shell.browser_seq);
                let weak = shell.browsers[&shell.browser_seq].downgrade();
                shell.add_browser_surface(None, window, cx);
                let second = RightSurface::Browser(shell.browser_seq);
                assert_ne!(first, second);
                assert_eq!(shell.front_view(cx), Some(second));
                shell.active_chat = "second-session".into();
                assert!(shell.panes_now(cx).views().is_empty(), "a session has panes of its own");
                shell.add_browser_surface(None, window, cx);
                let other = RightSurface::Browser(shell.browser_seq);
                shell.active_chat = "first-session".into();
                assert_eq!(shell.panes_now(cx).views(), vec![first, second]);
                assert_eq!(shell.front_view(cx), Some(second));
                shell.close_view(first, cx);
                assert_eq!(shell.front_view(cx), Some(second), "the keyboard stays");
                shell.close_view(second, cx);
                assert_eq!(shell.panes_now(cx).focused(), Pane::Session, "the keyboard goes home");
                assert!(shell.panes_now(cx).views().is_empty());
                shell.active_chat = "second-session".into();
                assert_eq!(shell.front_view(cx), Some(other));
                shell.close_view(other, cx);
                assert!(shell.browsers.is_empty());
                assert!(shell.browser_subs.is_empty());
                weak
            })
            .unwrap();
        cx.run_until_parked();
        assert!(weak.upgrade().is_none(), "a closed page's browser kept alive by a callback");
    }

    /// ⌘W closes the focused pane, a view or a split shell, before the window; the session's
    /// own pane falls through to the window's close.
    #[gpui::test]
    fn cmd_w_closes_the_focused_pane_before_the_window(cx: &mut TestAppContext) {
        use super::panes::Pane;
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.active_chat = "session".into();
                assert!(!shell.close_focused_pane(cx), "the session's own pane is the window's");

                shell.add_browser_surface(None, window, cx);
                let first = RightSurface::Browser(shell.browser_seq);
                shell.add_browser_surface(None, window, cx);
                let second = RightSurface::Browser(shell.browser_seq);
                assert_eq!(shell.panes_now(cx).views(), vec![first, second]);

                assert!(shell.close_focused_pane(cx));
                assert_eq!(shell.front_view(cx), Some(first), "its sibling takes the keyboard");
                assert!(shell.close_focused_pane(cx));
                assert_eq!(shell.panes_now(cx).panes(), vec![Pane::Session]);
                assert!(!shell.close_focused_pane(cx));
            })
            .unwrap();
    }

    #[gpui::test]
    fn lifecycle_actions_keep_pending_and_failed_file_saves_alive(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        for failed in [false, true] {
            window
                .update(cx, |shell, window, cx| {
                    window.activate_window();
                    let state = shell.state.clone();
                    let files = cx.new(|cx| {
                        let mut files = FilesSurface::new(
                            state,
                            "test".into(),
                            false,
                            1000,
                            13.0,
                            false,
                            false,
                            cx,
                        );
                        files.seed_pending_exit_test_document(failed);
                        files
                    });
                    shell.file_surfaces.insert(0, files);
                    shell
                        .file_surface_keys
                        .insert(("test".into(), "test".into(), "test.rs".into()), 0);
                })
                .unwrap();
            cx.update(|cx| cx.dispatch_action(&crate::app_menus::Quit));
            cx.run_until_parked();
            window
                .update(cx, |shell, _, cx| {
                    assert!(matches!(shell.pending_exit, Some(PendingExit::Quit)));
                    assert!(!shell.all_file_edits_flushed(cx));
                    shell.cancel_file_close(RightSurface::File(0), cx);
                    assert!(shell.pending_exit.is_none());
                })
                .unwrap();
            cx.update(|cx| cx.dispatch_action(&crate::app_menus::CloseWindow));
            cx.run_until_parked();
            window
                .update(cx, |shell, _, cx| {
                    assert!(matches!(shell.pending_exit, Some(PendingExit::CloseWindow)));
                    shell.cancel_file_close(RightSurface::File(0), cx);
                    assert!(shell.pending_exit.is_none());
                })
                .unwrap();
        }
    }
}

/// A note's landing: one 0.4 s accent border fade on its session's terminal.
const LANDING: motion::MotionSpec = motion::MotionSpec::new(400, motion::EASE_OUT);

/// The landing border's opacity `elapsed` after the tap: full at once, faded out by
/// [`LANDING`]'s end, then `None`.
fn landing_opacity(elapsed: Duration) -> Option<f32> {
    let total = LANDING.total();
    (elapsed < total).then(|| 1.0 - LANDING.progress(elapsed.as_secs_f32() / total.as_secs_f32()))
}

#[cfg(test)]
mod landing_tests {
    use std::time::Duration;

    /// The landing border is full at the tap, fades, and is gone by 0.4 s.
    #[test]
    fn the_landing_border_fades_once_in_four_tenths_of_a_second() {
        assert_eq!(super::landing_opacity(Duration::ZERO), Some(1.0));
        let half = super::landing_opacity(Duration::from_millis(200)).expect("still fading");
        assert!(half > 0.0 && half < 1.0, "{half}");
        assert!(
            super::landing_opacity(Duration::from_millis(390)).is_some_and(|o| o < half),
            "fainter as it goes"
        );
        assert_eq!(super::landing_opacity(Duration::from_millis(400)), None, "then gone");
    }
}

#[cfg(test)]
mod shortcut_focus_regressions {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    struct ShortcutHost {
        root: FocusHandle,
        unfocused: FocusHandle,
        editor: FocusHandle,
        show_editor: bool,
        jumps: usize,
    }

    impl Render for ShortcutHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            defer_mounted_focus_restore(
                self.root.clone(),
                self.editor.clone(),
                self.unfocused.clone(),
                window,
                cx,
            );
            div()
                .size_full()
                .track_focus(&self.root)
                .child(div().track_focus(&self.unfocused))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // Exercise mouse focus handoffs, hiding a focused pane,
                        // and clicking a control that explicitly clears focus.
                        this.show_editor = event.position.x < px(100.0);
                        if event.position.x < px(200.0) {
                            window.focus(&this.editor, cx);
                        } else {
                            window.blur(cx);
                        }
                        cx.notify();
                    }),
                )
                .on_action(cx.listener(|this, _: &JumpSession, _, _| this.jumps += 1))
                .when(self.show_editor, |el| {
                    el.child(div().track_focus(&self.editor))
                })
        }
    }

    #[gpui::test]
    fn explicit_blur_does_not_refocus_a_mounted_input(cx: &mut TestAppContext) {
        let host = cx.add_window(|_, cx| ShortcutHost {
            root: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            editor: cx.focus_handle(),
            show_editor: true,
            jumps: 0,
        });
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        host.update(cx, |host, window, cx| {
            window.focus(&host.editor, cx);
            window.blur(cx);
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.unfocused.is_focused(window));
            // Subsequent renders must keep the neutral shortcut focus too.
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.unfocused.is_focused(window));
        })
        .unwrap();
    }

    #[gpui::test]
    fn shortcuts_recover_from_retained_editor_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([crate::keys::binding(
                &platform_combo("mod-2"),
                JumpSession(1),
                None,
                cx,
            )]);
        });
        let host = cx.add_window(|_, cx| ShortcutHost {
            root: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            editor: cx.focus_handle(),
            show_editor: true,
            jumps: 0,
        });
        for show_editor in [true, false, true, false] {
            host.update(cx, |host, window, cx| {
                host.show_editor = show_editor;
                // Keep the editor handle alive and focused even when hidden.
                window.focus(&host.editor, cx);
                cx.notify();
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
            host.update(cx, |host, window, cx| {
                restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
                assert!(host.root.contains_focused(window, cx));
                assert_eq!(host.editor.is_focused(window), show_editor);
            })
            .unwrap();
            cx.simulate_keystrokes(host.into(), &platform_combo("mod-2"));
        }
        host.update(cx, |host, window, cx| {
            assert_eq!(host.jumps, 4);
            window.blur(cx);
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.unfocused.is_focused(window));
        })
        .unwrap();
    }

    #[gpui::test]
    fn shortcuts_work_after_mouse_focus_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([crate::keys::binding(
                &platform_combo("mod-2"),
                JumpSession(1),
                None,
                cx,
            )]);
        });
        let host = cx.add_window(|_, cx| ShortcutHost {
            root: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            editor: cx.focus_handle(),
            show_editor: true,
            jumps: 0,
        });
        for (index, x) in [50.0, 150.0, 250.0, 50.0, 150.0, 250.0]
            .into_iter()
            .enumerate()
        {
            cx.update_window(host.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(MouseDownEvent {
                        position: gpui::point(px(x), px(20.0)),
                        button: MouseButton::Left,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
            })
            .unwrap();
            // Dispatch immediately after the mouse event; no manual recovery.
            cx.simulate_keystrokes(host.into(), &platform_combo("mod-2"));
            host.update(cx, |host, window, cx| {
                assert_eq!(
                    host.jumps,
                    index + 1,
                    "shortcut failed after mouse click at {x}"
                );
                assert!(host.root.contains_focused(window, cx));
                assert_eq!(host.editor.is_focused(window), x < 100.0);
            })
            .unwrap();
        }
    }
}


#[cfg(test)]
mod settings_modal_regressions {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    struct FocusHost {
        background: FocusHandle,
        start: FocusHandle,
        first: FocusHandle,
        last: FocusHandle,
        end: FocusHandle,
    }

    impl Render for FocusHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(
                    div()
                        .id("background")
                        .track_focus(&self.background)
                        .tab_index(0),
                )
                .child(
                    div()
                        .id("modal")
                        .track_focus(&self.start)
                        .tab_group()
                        .tab_stop(false)
                        .child(div().id("first").track_focus(&self.first).tab_index(0))
                        .child(div().id("last").track_focus(&self.last).tab_index(0))
                        .child(
                            div()
                                .id("end")
                                .track_focus(&self.end)
                                .tab_index(0)
                                .tab_stop(false),
                        ),
                )
        }
    }

    #[gpui::test]
    fn settings_tab_wraps_in_both_directions_without_entering_workspace(cx: &mut TestAppContext) {
        let host = cx.add_window(|_, cx| FocusHost {
            background: cx.focus_handle().tab_stop(true),
            start: cx.focus_handle(),
            first: cx.focus_handle().tab_stop(true),
            last: cx.focus_handle().tab_stop(true),
            end: cx.focus_handle(),
        });
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        host.update(cx, |host, window, cx| {
            window.focus(&host.start, cx);
            assert!(!host.first.is_focused(window));
            assert!(!host.last.is_focused(window));
            move_settings_focus(&host.start, &host.end, false, window, cx);
            assert!(host.first.is_focused(window));
            for reverse in [false, false, true, true, true, false] {
                let was_first = host.first.is_focused(window);
                move_settings_focus(&host.start, &host.end, reverse, window, cx);
                assert!(host.start.contains_focused(window, cx));
                assert_eq!(host.last.is_focused(window), was_first);
                assert!(!host.background.is_focused(window));
            }
        })
        .unwrap();
    }

    #[gpui::test]
    fn settings_visits_preserve_conversation_and_navigation_history(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, _, cx| {
                let history = shell.nav.current().clone();
                let selected = shell.state.read(cx).selected_chat.clone();
                for section in SettingsSection::ALL {
                    shell.open_settings(section, cx);
                    assert_eq!(shell.route, Route::Settings(section));
                    assert_eq!(shell.nav.current().clone(), history);
                    assert_eq!(shell.state.read(cx).selected_chat, selected);
                    shell.toggle_settings(cx);
                    assert_eq!(shell.route, Route::Chat);
                    assert!(!shell.settings_focus_pending);
                    shell.toggle_settings(cx);
                    assert_eq!(shell.route, Route::Settings(section));
                }
                shell.close_settings(cx);
                assert_eq!(shell.route, Route::Chat);
                assert_eq!(shell.nav.current().clone(), history);
                assert!(shell.settings_restore_pending);
            })
            .unwrap();
    }

    fn init_settings_test(
        saved: settings::UiSettings,
        dir: &std::path::Path,
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            settings::init(saved, dir, cx);
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
    }

    fn test_shell(dir: &std::path::Path, cx: &mut Context<Shell>) -> Shell {
        let state = cx.new(|_| AppState::new());
        Shell::new(
            state,
            EngineBootConfig {
                data_dir: dir.into(),
                default_harness: slopty_view_proto::HarnessId::MOCK,
            },
            cx,
        )
    }

    #[test]
    fn every_section_reads_back_from_its_name() {
        for section in SettingsSection::ALL {
            assert_eq!(SettingsSection::from_slug(section.slug()), Some(section));
        }
    }

    #[test]
    fn open_route_names_a_section_or_reopens_the_remembered_one() {
        let remembered = SettingsSection::Notifications;
        assert_eq!(
            settings_open_route("settings", remembered),
            Some(SettingsSection::Notifications)
        );
        for (route, section) in [
            ("settings/devices", SettingsSection::Devices),
            ("settings/providers", SettingsSection::Harnesses),
            ("settings/harnesses", SettingsSection::Harnesses),
            ("settings/files", SettingsSection::Files),
            ("settings/archived", SettingsSection::Archived),
        ] {
            assert_eq!(
                settings_open_route(route, remembered),
                Some(section),
                "{route}"
            );
        }
        assert_eq!(settings_open_route("settings/billing", remembered), None);
        assert_eq!(settings_open_route("new", remembered), None);
    }

    /// Session navigation shortcuts are not swallowed by Settings: each one
    /// leaves the page and lands on its target, as it would from chat.
    #[gpui::test]
    fn navigation_shortcuts_work_from_settings(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        let keymap = KeymapConfig::default();
        cx.update(|cx| apply_keymap(cx, &keymap));
        let window = cx.add_window(|_, cx| {
            let shell = test_shell(dir.path(), cx);
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.chats = ["older", "newer"]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, id)| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "title": id, "deviceId": "local", "archived": false,
                            "createdAt": Utc::now() - chrono::Duration::minutes(10 - ix as i64),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.selected_chat = Some("older".into());
            });
            shell
        });
        let mut press = |combo: &str| {
            window
                .update(cx, |shell, _, cx| {
                    shell.open_settings(SettingsSection::Shortcuts, cx)
                })
                .unwrap();
            cx.run_until_parked();
            cx.simulate_keystrokes(window.into(), &platform_combo(combo));
            window
                .read_with(cx, |shell, cx| {
                    (shell.route, shell.state.read(cx).selected_chat.clone())
                })
                .unwrap()
        };
        assert_eq!(
            press(keymap.get(ShortcutId::JumpSession(0))),
            (Route::Chat, Some("newer".into()))
        );
        assert_eq!(
            press(&keymap.next_session),
            (Route::Chat, Some("older".into()))
        );
        assert_eq!(
            press(&keymap.prev_session),
            (Route::Chat, Some("newer".into()))
        );
        // ⌘N opens the New session picker over the session on screen.
        assert_eq!(press(&keymap.new_session), (Route::Chat, Some("newer".into())));
        assert!(window.read_with(cx, |shell, _| shell.new_session.is_some()).unwrap());
    }

    /// An AZERTY Mac types ⌘& on the key AppKit reads as ⌘1, and ⌘⇧& — which
    /// gpui reports as a literal ⌘1 — is no shortcut there. Bound in US keys
    /// only, Jump to session 1 answered the second and not the first.
    #[gpui::test]
    fn azerty_jumps_from_the_key_appkit_reads_as_cmd_1(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        // The keymap bound as the app binds it at start.
        cx.update(|cx| apply_keymap(cx, &KeymapConfig::default()));
        let window = cx.add_window(|_, cx| {
            let shell = test_shell(dir.path(), cx);
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.chats = ["older", "newer"]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, id)| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "title": id, "deviceId": "local", "archived": false,
                            "createdAt": Utc::now() - chrono::Duration::minutes(10 - ix as i64),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.selected_chat = Some("older".into());
            });
            shell
        });
        // The keymap was bound against the test platform, which has no layout
        // table; localize it as an AZERTY Mac's `apply_keymap` does.
        cx.run_until_parked();
        cx.update(|cx| localize_key_bindings(cx, &settings::azerty_key_equivalents()));
        let mut press = |combo: &str| {
            cx.simulate_keystrokes(window.into(), &platform_combo(combo));
            window
                .read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
                .unwrap()
        };
        assert_eq!(press("mod-1"), Some("older".into()));
        assert_eq!(press("mod-&"), Some("newer".into()));
    }

    /// Localizing moves exactly the bindings whose key the layout relocates —
    /// Slopty's and gpui-base's alike — and keeps the rest, in order.
    #[gpui::test]
    fn azerty_keymap_moves_only_relocated_keys(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        let keymap = KeymapConfig::default();
        cx.update(|cx| apply_keymap(cx, &keymap));
        let azerty = settings::azerty_key_equivalents();
        let sources = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                cx.key_bindings()
                    .borrow()
                    .bindings()
                    .map(|binding| {
                        let keys = binding
                            .keystrokes()
                            .iter()
                            .map(|keystroke| keystroke.inner().unparse(cx.platform_info().os))
                            .collect::<Vec<_>>()
                            .join(" ");
                        (binding.action().name(), keys)
                    })
                    .collect::<Vec<_>>()
            })
        };
        let before = sources(cx);
        cx.update(|cx| localize_key_bindings(cx, &azerty));
        let after = sources(cx);
        assert_eq!(before.len(), after.len());
        for ((action, us), (localized_action, localized)) in before.iter().zip(&after) {
            assert_eq!(action, localized_action, "binding order must hold");
            let expected = us
                .split(' ')
                .map(|keystroke| settings::localize_combo(keystroke, &azerty))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(localized, &expected, "{action}");
        }
        let bound = |action: &str| {
            after
                .iter()
                .filter(|(name, _)| *name == action)
                .map(|(_, keys)| keys.as_str())
                .collect::<Vec<_>>()
        };
        let primary = platform_combo("mod");
        // Every jump slot shares the action name: slot 1 now sits on &, and
        // none stays on the literal 1 that AZERTY types only with Shift.
        let jumps = bound(JumpSession(0).name());
        assert!(jumps.contains(&format!("{primary}-&").as_str()));
        assert!(!jumps.contains(&format!("{primary}-1").as_str()));
        assert!(bound(crate::browser::Back.name()).contains(&format!("{primary}-^").as_str()));
        assert!(bound(ToggleSidebar.name()).contains(&format!("{primary}-b").as_str()));
    }

    #[gpui::test]
    fn settings_reopen_where_they_were_left(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        let window = cx.add_window(|_, cx| test_shell(dir.path(), cx));
        window
            .update(cx, |shell, _, cx| {
                // Nothing remembered yet: Appearance, the first page.
                shell.toggle_settings(cx);
                assert_eq!(shell.route, Route::Settings(SettingsSection::Appearance));
                // Switching sections inside Settings is remembered.
                shell.open_settings(SettingsSection::Shortcuts, cx);
                assert_eq!(shell.settings.settings_section, SettingsSection::Shortcuts);
                assert_eq!(
                    settings::current(cx).settings_section,
                    SettingsSection::Shortcuts
                );
                shell.toggle_settings(cx);
                assert_eq!(shell.route, Route::Chat);
                // ⌘, / the footer gear reopen it…
                shell.toggle_settings(cx);
                assert_eq!(shell.route, Route::Settings(SettingsSection::Shortcuts));
                shell.close_settings(cx);
                // …as do the palette and `/settings`.
                shell.open_last_settings(cx);
                assert_eq!(shell.route, Route::Settings(SettingsSection::Shortcuts));
                shell.close_settings(cx);

                // A link naming a section wins, and becomes the one remembered.
                shell.open_settings(SettingsSection::Appearance, cx);
                assert_eq!(shell.route, Route::Settings(SettingsSection::Appearance));
                shell.close_settings(cx);
                shell.toggle_settings(cx);
                assert_eq!(shell.route, Route::Settings(SettingsSection::Appearance));
                shell.close_settings(cx);

                settings::flush(cx);
            })
            .unwrap();
        // It survives a restart.
        assert_eq!(
            settings::UiSettings::load(dir.path()).settings_section,
            SettingsSection::Appearance
        );
    }

    #[gpui::test]
    fn unknown_or_hidden_remembered_sections_reopen_general(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            settings::UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "settingsSection": "billing"}"#,
        )
        .unwrap();
        let saved = settings::UiSettings::load(dir.path());
        assert_eq!(saved.sidebar_width, 300.0);
        init_settings_test(saved, dir.path(), cx);
        let window = cx.add_window(|_, cx| test_shell(dir.path(), cx));
        window
            .update(cx, |shell, _, cx| {
                shell.toggle_settings(cx);
                assert_eq!(shell.route, Route::Settings(SettingsSection::Appearance));
                shell.close_settings(cx);
            })
            .unwrap();
    }


    // The first run, with no device to put a project on, lands on Devices with its Add
    // device dialog open.
    #[gpui::test]
    fn the_first_run_without_a_device_opens_add_device(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        let (shell, cx) = cx.add_window_view(|_, cx| test_shell(dir.path(), cx));
        shell.update(cx, |shell, cx| {
            shell.debug_gate = Some(GatePhase::Ready);
            assert!(shell.state.read(cx).devices.is_empty());
            shell.open_add_device(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.route, Route::Settings(SettingsSection::Devices));
        });
        let devices = shell.read_with(cx, |shell, _| shell.devices_page.clone().unwrap());
        assert!(
            devices.update(cx, |page, cx| page.dismiss_on_escape(cx)),
            "the Add device dialog is open"
        );
    }

    #[gpui::test]
    fn escape_closes_what_is_open_inside_settings_before_settings(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        let (shell, cx) = cx.add_window_view(|_, cx| test_shell(dir.path(), cx));
        shell.update(cx, |shell, cx| {
            // No engine in tests: render the workspace, not the boot gate.
            shell.debug_gate = Some(GatePhase::Ready);
            shell.open_settings(SettingsSection::Devices, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // The Add device dialog closes before Settings does.
        let devices = shell.read_with(cx, |shell, _| shell.devices_page.clone().unwrap());
        devices.update(cx, |page, cx| page.open_add_device(cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("escape");
        assert!(!devices.update(cx, |page, cx| page.dismiss_on_escape(cx)));
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.route, Route::Settings(SettingsSection::Devices));
        });

        // So does the account menu opened from the settings footer.
        shell.update(cx, |shell, cx| {
            shell.user_menu.open(());
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("escape");
        shell.read_with(cx, |shell, _| {
            assert!(!shell.user_menu.is_open());
            assert_eq!(shell.route, Route::Settings(SettingsSection::Devices));
        });
        // The exit animation's reap runs on wall-clock time; stand in for it.
        shell.update(cx, |shell, cx| {
            shell.user_menu = popover::Popup::default();
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // With nothing left open, Escape leaves Settings.
        cx.simulate_keystrokes("escape");
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.route, Route::Chat);
            assert_eq!(shell.settings.settings_section, SettingsSection::Devices);
        });
    }
}

