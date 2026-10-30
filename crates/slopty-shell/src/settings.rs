// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3d2f1e6), see THIRD_PARTY_NOTICES.md
//! UI settings persisted to a small JSON file in the data dir — pane widths and
//! collapse flags (slopty persisted the same set in localStorage).
//!
//! Loaded once at boot and then owned by [`SettingsStore`], the only production
//! writer. Frequent geometry changes are debounced; durable choices flush
//! immediately through that same writer. Corrupt or missing files fall back to
//! defaults, and loaded values are clamped so a hand-edited file can't wedge the
//! layout.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, Global, Task};
use serde::{Deserialize, Serialize};
use slopty_view_proto::WorkspaceScope;

// Settings → Devices' Add device dialog.
pub mod add_device;
pub mod appearance;
pub mod archived;
pub mod devices;
pub mod files;
pub mod harnesses;
// The code a phone or iPad scans for the server.
pub mod invite;
pub mod notifications;
pub mod shortcuts;
pub mod widgets;

/// Sidebar drag-resize bounds (px). The minimum keeps the sidebar options
/// button inside the sidebar beside the titlebar controls: 88 (past the
/// traffic lights) + 82 (toggle, back, forward) + 32 (new session) + 8 gap
/// + 24 (options) + 8 inset.
pub const SIDEBAR_MIN: f32 = 242.0;
pub const SIDEBAR_MAX: f32 = 400.0;
pub const SIDEBAR_DEFAULT: f32 = 256.0;

/// Surface pane floor and default (px). Runtime sizing also reserves space
/// for the conversation and any docked file explorer.
pub const RIGHT_PANE_MIN: f32 = 360.0;
pub const RIGHT_PANE_DEFAULT: f32 = 520.0;
/// The Pull requests route's side pane defaults wider than the chat's: the
/// detail's Code tab needs room beside the board.
/// Minimum width retained for the conversation when the right pane is open.
pub const CHAT_PANEL_MIN: f32 = 300.0;

/// Terminal panel height bounds: 160px … 55% of the viewport (§1.10). The
/// viewport-relative cap applies at runtime; the absolute cap here only heals
/// hand-edited files.
pub const TERMINAL_MIN_HEIGHT: f32 = 160.0;
pub const TERMINAL_MAX_VH: f32 = 0.55;
pub const TERMINAL_ABS_MAX_HEIGHT: f32 = 2000.0;
pub const TERMINAL_DEFAULT_HEIGHT: f32 = 280.0;

/// Debounce for settings writes after a drag/toggle.
pub const SAVE_DEBOUNCE_MS: u64 = 400;

pub const FILES_AUTOSAVE_DELAY_DEFAULT_MS: u64 = 900;
pub const FILES_AUTOSAVE_DELAY_MIN_MS: u64 = 100;
pub const FILES_AUTOSAVE_DELAY_MAX_MS: u64 = 10_000;

const FILE_NAME: &str = "ui-settings.json";

/// How wide a conversation reads, as one of Amp's three presets (prune P9,
/// `docs/decisions/ui.md`): measured against the window, as Amp's breakpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationWidth {
    /// 672, then 724 in a window 1280 wide and 750 at 1600.
    #[default]
    Default,
    /// Six tenths of the window, never under the default's 672.
    Wide,
    /// The whole column.
    Full,
}

impl ConversationWidth {
    pub const ALL: [Self; 3] = [Self::Default, Self::Wide, Self::Full];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Wide => "Wide",
            Self::Full => "Full",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Default => "Grows a little with the window.",
            Self::Wide => "Six tenths of the window.",
            Self::Full => "As wide as the window allows.",
        }
    }

    /// The widest messages and the docked composer go in a window `window` wide.
    pub fn cap(self, window: f32) -> f32 {
        const NARROWEST: f32 = 672.0;
        match self {
            Self::Default if window >= 1600.0 => 750.0,
            Self::Default if window >= 1280.0 => 724.0,
            Self::Default => NARROWEST,
            Self::Wide => (window * 0.6).max(NARROWEST),
            Self::Full => window.max(NARROWEST),
        }
    }
}

pub fn conversation_width(cx: &App) -> ConversationWidth {
    cx.try_global::<SettingsStore>()
        .map(|store| store.current.conversation_width)
        .unwrap_or_default()
}

/// The widest messages and the docked composer go, in a window `window` wide.
pub fn transcript_width(window: f32, cx: &App) -> f32 {
    conversation_width(cx).cap(window)
}

pub fn set_conversation_width(width: ConversationWidth, cx: &mut App) {
    if update(SavePolicy::Debounced, cx, |settings| {
        settings.conversation_width = width;
    }) {
        cx.refresh_windows();
    }
}

/// Whether a settings mutation should wait for the normal coalescing window or
/// reach disk before returning to the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavePolicy {
    Debounced,
    Immediate,
}

/// The sole in-process owner and writer of `ui-settings.json`.
///
/// Mutations land in `current` before any timer starts. Replacing a pending
/// task cancels its stale snapshot, and immediate mutations cancel the timer
/// before flushing synchronously.
pub struct SettingsStore {
    current: UiSettings,
    data_dir: PathBuf,
    revision: u64,
    saved_revision: u64,
    save_task: Option<Task<()>>,
    // A file [`UiSettings::load`] moved aside, until [`announce_set_aside`] says so.
    set_aside: Option<Option<PathBuf>>,
}

impl Global for SettingsStore {}

impl SettingsStore {
    fn snapshot(&self) -> (UiSettings, u64) {
        (self.current.clone(), self.revision)
    }

    fn mark_saved(&mut self, revision: u64) -> bool {
        self.saved_revision = self.saved_revision.max(revision);
        self.saved_revision == self.revision
    }

    fn update_current(&mut self, mutate: impl FnOnce(&mut UiSettings)) -> bool {
        let before = self.current.clone();
        mutate(&mut self.current);
        self.current = self.current.clone().clamped();
        if self.current == before {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

pub fn init(settings: UiSettings, data_dir: impl Into<PathBuf>, cx: &mut App) {
    let data_dir: PathBuf = data_dir.into();
    let set_aside = take_set_aside(&data_dir);
    cx.set_global(SettingsStore {
        current: settings,
        data_dir,
        revision: 0,
        saved_revision: 0,
        save_task: None,
        set_aside,
    });
}

/// Latest settings, including mutations still inside the debounce window.
pub fn current(cx: &App) -> UiSettings {
    cx.try_global::<SettingsStore>().map(|store| store.current.clone()).unwrap_or_default()
}

pub fn update(policy: SavePolicy, cx: &mut App, mutate: impl FnOnce(&mut UiSettings)) -> bool {
    if !cx.has_global::<SettingsStore>() {
        return false;
    }
    if !cx.global_mut::<SettingsStore>().update_current(mutate) {
        return false;
    }
    schedule(policy, cx);
    true
}

pub fn replace(settings: UiSettings, policy: SavePolicy, cx: &mut App) -> bool {
    update(policy, cx, |current| *current = settings)
}

/// Read on every wheel notch, so without cloning the settings. Without a
/// store (not the app) wheel input is left alone.
pub fn smooth_scrolling(cx: &App) -> bool {
    cx.try_global::<SettingsStore>().is_some_and(|store| store.current.smooth_scrolling)
}

/// The format the person chose for the output of steps of kind `kind`, read on every
/// frame an open step draws, so without cloning the settings; `None` when they never chose one.
pub fn output_format(kind: &str, cx: &App) -> Option<String> {
    cx.try_global::<SettingsStore>()
        .and_then(|store| store.current.output_formats.get(kind).cloned())
}

/// Keep `format` as how the output of steps of kind `kind` is shown.
pub fn set_output_format(kind: &str, format: &str, cx: &mut App) {
    update(SavePolicy::Immediate, cx, |settings| {
        settings.output_formats.insert(kind.to_owned(), format.to_owned());
    });
}

pub fn set_smooth_scrolling(enabled: bool, cx: &mut App) {
    update(SavePolicy::Immediate, cx, |settings| {
        settings.smooth_scrolling = enabled;
    });
}

fn schedule(policy: SavePolicy, cx: &mut App) {
    let old_task = cx.global_mut::<SettingsStore>().save_task.take();
    drop(old_task);

    match policy {
        SavePolicy::Immediate => flush(cx),
        SavePolicy::Debounced => {
            let task = cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(SAVE_DEBOUNCE_MS)).await;
                cx.update(flush_latest);
            });
            cx.global_mut::<SettingsStore>().save_task = Some(task);
        }
    }
}

/// Persist the latest revision. Safe to call at shutdown.
pub fn flush(cx: &mut App) {
    if !cx.has_global::<SettingsStore>() {
        return;
    }
    let pending = cx.global_mut::<SettingsStore>().save_task.take();
    drop(pending);
    flush_latest(cx);
}

fn flush_latest(cx: &mut App) {
    let Some(store) = cx.try_global::<SettingsStore>() else {
        return;
    };
    if store.saved_revision == store.revision {
        return;
    }
    let (settings, revision) = store.snapshot();
    let data_dir = store.data_dir.clone();
    match settings.save(&data_dir) {
        Ok(()) => {
            let current = cx.global_mut::<SettingsStore>().mark_saved(revision);
            debug_assert!(current, "foreground settings write cannot be overtaken");
        }
        Err(err) => tracing::warn!(error = %err, revision, "failed to persist ui settings"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_uuid: Option<uuid::Uuid>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowGeometry {
    pub fn is_valid(self) -> bool {
        [self.x, self.y, self.width, self.height].into_iter().all(f32::is_finite)
            && self.width > 0.0
            && self.height > 0.0
    }

    pub fn from_bounds(bounds: gpui::Bounds<gpui::Pixels>) -> Self {
        Self {
            display_uuid: None,
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
        }
    }

    pub fn restore(self, displays: &[Self], primary: usize) -> Option<(usize, Self)> {
        if !self.is_valid() {
            return None;
        }
        let matched = self.display_uuid.and_then(|uuid| {
            displays
                .iter()
                .position(|display| display.is_valid() && display.display_uuid == Some(uuid))
        });
        let index = matched
            .or_else(|| displays.get(primary).filter(|display| display.is_valid()).map(|_| primary))
            .or_else(|| displays.iter().position(|display| display.is_valid()))?;
        let display = displays[index];
        let mut geometry = self.fit(display);
        if self.display_uuid.is_some() && matched.is_none() {
            geometry.x = display.x + (display.width - geometry.width) / 2.0;
            geometry.y = display.y + (display.height - geometry.height) / 2.0;
        }
        geometry.display_uuid = display.display_uuid;
        Some((index, geometry))
    }

    pub fn fit(self, display: Self) -> Self {
        let width = self.width.max(900.0).min(display.width);
        let height = self.height.max(600.0).min(display.height);
        Self {
            display_uuid: self.display_uuid,
            x: self.x.clamp(display.x, display.x + display.width - width),
            y: self.y.clamp(display.y, display.y + display.height - height),
            width,
            height,
        }
    }

    pub fn bounds(self) -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds::new(
            gpui::point(gpui::px(self.x), gpui::px(self.y)),
            gpui::size(gpui::px(self.width), gpui::px(self.height)),
        )
    }
}

pub const SKILL_COMPLETION_HARNESSES: [(slopty_view_proto::HarnessId, &str); 9] = [
    (slopty_view_proto::HarnessId::ANTIGRAVITY, "Antigravity"),
    (slopty_view_proto::HarnessId::CLAUDE_CODE, "Claude Code"),
    (slopty_view_proto::HarnessId::CODEX, "Codex"),
    (slopty_view_proto::HarnessId::CURSOR, "Cursor"),
    (slopty_view_proto::HarnessId::DEVIN, "Devin"),
    (slopty_view_proto::HarnessId::GROK, "Grok"),
    (slopty_view_proto::HarnessId::HERMES, "Hermes"),
    (slopty_view_proto::HarnessId::PI, "Pi"),
    (slopty_view_proto::HarnessId::OPENCODE, "OpenCode"),
];

/// The surface tabs one chat's pane showed, in their order, and the one in front.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedTabs {
    pub tabs: Vec<SavedSurface>,
    pub active: Option<usize>,
}

/// One surface tab as kept: what opens it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SavedSurface {
    /// A terminal, by its session.
    Terminal { id: String },
    /// A file, owned by chat `chat`.
    File { chat: String, path: String },
    /// A web page.
    Browser { url: String },
    /// A diff.
    Diff,
    /// A remote window or desktop, by what was chosen.
    Screen { screen: String, title: String },
    /// The session's inspector.
    Inspector,
    /// The session's files.
    Files,
}

/// How often the person ran a palette command, and when last, as `ui-settings.json` keeps it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PaletteUse {
    /// The times it ran.
    pub count: u32,
    /// When it last ran, in milliseconds since the Unix epoch.
    pub last_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_geometry: Option<WindowGeometry>,
    pub sidebar_width: f32,
    pub sidebar_collapsed: bool,
    /// The last selected space — restored on boot when the row still exists;
    /// also the new-tab default when the sidebar filter is "All".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_space_id: Option<String>,
    /// Sidebar session filter: a space id, or `None` for "All spaces".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_filter: Option<String>,
    /// The sidebar's machine filter, beside the project's: a device id, or `None` for
    /// every machine (`docs/decisions/layout.md`, "the machine is a quiet subtitle on a row and
    /// a filter, not a level").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_filter: Option<String>,
    /// The system's notes for a session in the background that needs the person, failed
    /// or finished (`slopty_ui`'s `workspace::attention`); none make a sound
    /// (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a feed").
    pub notifications_enabled: bool,
    pub right_pane_width: f32,
    pub terminal_height: f32,
    /// What each chat's pane showed, kept across launches (`shell::kept_tabs`).
    pub session_tabs: std::collections::BTreeMap<String, SavedTabs>,
    /// How often and how lately the person ran each palette command, by its name: the palette
    /// ranks by it (`shell::command_palette`, WezTerm's frecency).
    pub palette_uses: std::collections::BTreeMap<String, PaletteUse>,
    /// How the inspector shows each kind of step's output, by the kind's name, as the
    /// person last chose it in a step's format menu (`transcript::detail`): `text`, `json` or
    /// `markdown`. A kind not here is shown as its output reads.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub output_formats: std::collections::BTreeMap<String, String>,
    /// The shortcuts the person rebound in Settings, kept as `shortcuts`: only what they
    /// changed.
    #[serde(rename = "shortcuts")]
    pub keymap: KeymapConfig,
    /// The Settings section last viewed. ⌘, / Ctrl+,, the footer gear and the
    /// palette reopen it; links naming a section replace it. Files without
    /// it, or with a name this build does not know, open on General.
    pub settings_section: crate::shell::SettingsSection,
    /// Light/dark preference. Defaults to following the OS.
    pub appearance: crate::appearance::AppearanceMode,
    /// Interface and conversational-prose family. Device-local by design.
    pub ui_font_family: crate::typography::UiFontFamily,
    /// Base size for interface and conversational prose.
    pub ui_font_size: crate::typography::UiFontSize,
    /// Terminal family and absolute pixel size. Only fixed-width families
    /// qualify, including compatible Nerd Fonts.
    pub terminal_font_family: crate::typography::UiFontFamily,
    pub terminal_font_size: f32,
    /// Family and absolute pixel size for code, diffs, and file editors.
    pub code_font_family: crate::typography::UiFontFamily,
    pub code_font_size: f32,
    /// Independently selected light and dark theme variants.
    pub theme_selection: slopty_shell_theme::ThemeSelection,
    /// Changes pane: side-by-side diffs instead of the unified stack.
    pub diff_split: bool,
    /// Changes pane: wrap long source lines instead of scrolling horizontally.
    pub diff_wrap: bool,
    // The review aids' last choices, kept across launches.
    /// Changes pane: leave whitespace-only changes out of the diff.
    pub diff_ignore_whitespace: bool,
    /// Changes pane: the changed files' tree beside the diff.
    pub diff_show_tree: bool,
    /// How wide messages and the docked composer go ([`ConversationWidth`]).
    /// The centered new-chat composer keeps its own width.
    pub conversation_width: ConversationWidth,
    /// Open a normal web-link activation in the session Browser. Explicit
    /// context-menu actions remain available regardless of this preference.
    pub open_web_links_in_slopty: bool,
    /// Save edited workspace files automatically after the configured delay.
    pub files_autosave_enabled: bool,
    /// Idle time before an edited workspace file is saved automatically.
    pub files_autosave_delay_ms: u64,
    /// Wrap long lines in workspace file editors and previews.
    pub files_word_wrap: bool,
    /// Include hidden and ignored entries in workspace file trees.
    pub files_show_all: bool,
    /// Interactive identity overlay; the theme default keeps each theme's own accent.
    pub accent: slopty_shell_theme::AccentSelection,
    /// Glass policy, independent from the selected appearance, theme, and accent.
    pub surface: slopty_shell_theme::SurfacePreference,
    /// Also snap animations while the main window is not focused.
    pub pause_animations_in_background: bool,
    /// Mouse-wheel notches glide instead of jumping (`smooth_scroll`).
    /// Trackpads already scroll smoothly; reduced motion overrides.
    pub smooth_scrolling: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            window_geometry: None,
            sidebar_width: SIDEBAR_DEFAULT,
            sidebar_collapsed: false,
            last_space_id: None,
            space_filter: None,
            machine_filter: None,
            notifications_enabled: true,
            right_pane_width: RIGHT_PANE_DEFAULT,
            terminal_height: TERMINAL_DEFAULT_HEIGHT,
            session_tabs: std::collections::BTreeMap::new(),
            palette_uses: std::collections::BTreeMap::new(),
            output_formats: std::collections::BTreeMap::new(),
            keymap: KeymapConfig::default(),
            settings_section: crate::shell::SettingsSection::default(),
            appearance: crate::appearance::AppearanceMode::default(),
            ui_font_family: crate::typography::UiFontFamily::default(),
            ui_font_size: crate::typography::UiFontSize::default(),
            terminal_font_family: crate::typography::UiFontFamily::GeistMono,
            terminal_font_size: crate::typography::TERMINAL_FONT_SIZE_DEFAULT,
            code_font_family: crate::typography::UiFontFamily::GeistMono,
            code_font_size: crate::typography::CODE_FONT_SIZE_DEFAULT,
            theme_selection: slopty_shell_theme::ThemeSelection::default(),
            diff_split: false,
            diff_wrap: false,
            diff_ignore_whitespace: false,
            diff_show_tree: false,
            conversation_width: ConversationWidth::Default,
            open_web_links_in_slopty: true,
            files_autosave_enabled: false,
            files_autosave_delay_ms: FILES_AUTOSAVE_DELAY_DEFAULT_MS,
            files_word_wrap: false,
            files_show_all: false,
            accent: slopty_shell_theme::AccentSelection::default(),
            surface: slopty_shell_theme::SurfacePreference::default(),
            pause_animations_in_background: false,
            smooth_scrolling: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Keymap (customizable shortcuts, §1.4)
// ---------------------------------------------------------------------------

/// How many sidebar rows the jump shortcuts reach (t3code's
/// `THREAD_JUMP_KEYBINDING_COMMANDS`, nine slots).
pub const JUMP_SLOTS: usize = 9;

/// Default combo per jump slot, and the label the shortcuts table shows.
const JUMP_DEFAULTS: [&str; JUMP_SLOTS] =
    ["mod-1", "mod-2", "mod-3", "mod-4", "mod-5", "mod-6", "mod-7", "mod-8", "mod-9"];
// One noun for what an agent works in everywhere: session (TUI-first, `docs/decisions/ui.md`,
// "A session is its terminal, its inspector beside it").
const JUMP_LABELS: [&str; JUMP_SLOTS] = [
    "Jump to session 1",
    "Jump to session 2",
    "Jump to session 3",
    "Jump to session 4",
    "Jump to session 5",
    "Jump to session 6",
    "Jump to session 7",
    "Jump to session 8",
    "Jump to session 9",
];

/// The rebindable app shortcuts. `JumpSession(slot)` is zero-based; a slot at
/// or past [`JUMP_SLOTS`] has no combo and no label, so it reads as unbound
/// rather than panicking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutId {
    SaveFile,
    BrowserReload,
    ToggleSidebar,
    ToggleChanges,
    ToggleFiles,
    NewSession,
    NewProject,
    NextSession,
    PrevSession,
    ArchiveSession,
    // The right pane's last closed tab.
    ReopenClosedTab,
    NextNeedsYou, // the next chat waiting on the person (`shell::sidebar_status`)
    Inbox,        // what needs the person, across sessions and machines (`shell::inbox`)
    JumpSession(usize),
}

impl ShortcutId {
    pub const ALL: [ShortcutId; 13 + JUMP_SLOTS] = [
        ShortcutId::SaveFile,
        ShortcutId::BrowserReload,
        ShortcutId::ToggleSidebar,
        ShortcutId::ToggleChanges,
        ShortcutId::ToggleFiles,
        ShortcutId::NewSession,
        ShortcutId::NewProject,
        ShortcutId::NextSession,
        ShortcutId::PrevSession,
        ShortcutId::ArchiveSession,
        ShortcutId::ReopenClosedTab,
        ShortcutId::NextNeedsYou,
        ShortcutId::Inbox,
        ShortcutId::JumpSession(0),
        ShortcutId::JumpSession(1),
        ShortcutId::JumpSession(2),
        ShortcutId::JumpSession(3),
        ShortcutId::JumpSession(4),
        ShortcutId::JumpSession(5),
        ShortcutId::JumpSession(6),
        ShortcutId::JumpSession(7),
        ShortcutId::JumpSession(8),
    ];

    /// The shortcut's name on its Settings row.
    pub fn label(self) -> &'static str {
        match self {
            ShortcutId::SaveFile => "Save file",
            // Each name is the one its button, menu and palette row use.
            ShortcutId::BrowserReload => "Reload page",
            ShortcutId::ToggleSidebar => "Toggle sidebar",
            // The views a key summons beside the session, by the View menu's names.
            ShortcutId::ToggleChanges => "Changes",
            ShortcutId::ToggleFiles => "Files",
            ShortcutId::NewSession => "New session",
            ShortcutId::NewProject => "New project",
            ShortcutId::NextSession => "Next session",
            ShortcutId::PrevSession => "Previous session",
            ShortcutId::ArchiveSession => "Archive session",
            ShortcutId::ReopenClosedTab => "Reopen closed view",
            ShortcutId::NextNeedsYou => "Next session that needs you",
            ShortcutId::Inbox => "Inbox", // the titlebar eye's name
            ShortcutId::JumpSession(slot) => JUMP_LABELS.get(slot).copied().unwrap_or(""),
        }
    }

    pub fn default_combo(self) -> &'static str {
        self.default_combo_on(crate::keys::MOD_IS_COMMAND) // every Apple keyboard
    }

    /// `default_combo` for an explicit platform, so the spelling invariant is
    /// testable for both from any machine (see the tests below — the mismatch
    /// this guards against only exists off macOS).
    pub fn default_combo_on(self, mac: bool) -> &'static str {
        match self {
            ShortcutId::SaveFile => "mod-s",
            ShortcutId::BrowserReload => "mod-shift-r",
            ShortcutId::ToggleSidebar => "mod-b",
            ShortcutId::ToggleChanges => "mod-r",
            ShortcutId::ToggleFiles => "mod-e",
            ShortcutId::NewSession => "mod-n",
            ShortcutId::NewProject => "mod-shift-n",
            // Ctrl+Tab on every platform — but spelled the way THAT platform's
            // recorder spells ctrl (see `combo_from_keystroke`). Off macOS
            // ctrl IS the primary and stores as "mod"; on macOS it is its own
            // modifier, and "mod" would mean Cmd+Tab, which the OS app
            // switcher eats.
            //
            // Off macOS "ctrl-tab" and "mod-tab" resolve to the same keystroke
            // through `platform_combo`, but conflict detection compares the
            // STORED spelling — so a default the recorder cannot reproduce
            // would let a rebind onto that same physical key pass as
            // conflict-free, bind twice, and silently kill one shortcut.
            ShortcutId::NextSession if mac => "ctrl-tab",
            ShortcutId::NextSession => "mod-tab",
            ShortcutId::PrevSession if mac => "ctrl-shift-tab",
            ShortcutId::PrevSession => "mod-shift-tab",
            // Mod+A is the composer's Select all, so archiving takes the
            // shifted combo.
            ShortcutId::ArchiveSession => "mod-shift-a",
            ShortcutId::ReopenClosedTab => "mod-shift-t",
            // ⌘J is the jump, in the bottom panel's toggle's place, and the inbox it
            // walks sits beside it on ⇧⌘J (`docs/decisions/layout.md`).
            ShortcutId::NextNeedsYou => "mod-j",
            ShortcutId::Inbox => "mod-shift-j",
            ShortcutId::JumpSession(slot) => JUMP_DEFAULTS.get(slot).copied().unwrap_or(""),
        }
    }

    /// The sidebar row this id jumps to, if it is a jump shortcut.
    pub fn jump_slot(self) -> Option<usize> {
        match self {
            ShortcutId::JumpSession(slot) if slot < JUMP_SLOTS => Some(slot),
            _ => None,
        }
    }

    /// Its name in `ui-settings.json`'s `shortcuts` ([`StoredKeymap`]); a jump slot is
    /// counted from 1, as its key.
    pub fn stored_key(self) -> std::borrow::Cow<'static, str> {
        let name = match self {
            ShortcutId::SaveFile => "saveFile",
            ShortcutId::BrowserReload => "browserReload",
            ShortcutId::ToggleSidebar => "toggleSidebar",
            ShortcutId::ToggleChanges => "toggleChanges",
            ShortcutId::ToggleFiles => "toggleFiles",
            ShortcutId::NewSession => "newSession",
            ShortcutId::NewProject => "newProject",
            ShortcutId::NextSession => "nextSession",
            ShortcutId::PrevSession => "prevSession",
            ShortcutId::ArchiveSession => "archiveSession",
            ShortcutId::ReopenClosedTab => "reopenClosedTab",
            ShortcutId::NextNeedsYou => "nextNeedsYou",
            ShortcutId::Inbox => "inbox",
            ShortcutId::JumpSession(slot) => return format!("jumpSession{}", slot + 1).into(),
        };
        name.into()
    }
}

/// Persisted shortcut combos. Stored platform-neutral ("mod-s"); translated to
/// "cmd-s"/"ctrl-s" at bind time by [`platform_combo`]. The file keeps only what the
/// person changed ([`StoredKeymap`]), so a default that moves reaches everyone who left it be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "StoredKeymap", into = "StoredKeymap")]
pub struct KeymapConfig {
    pub save_file: String,
    pub browser_reload: String,
    pub toggle_sidebar: String,
    pub toggle_changes: String,
    pub toggle_files: String,
    pub new_session: String,
    pub new_project: String,
    pub next_session: String,
    pub prev_session: String,
    pub archive_session: String,
    // The right pane's last closed tab.
    pub reopen_closed_tab: String,
    pub next_needs_you: String, // the next chat waiting on the person
    pub inbox: String,          // what needs the person
    /// One combo per jump slot, in slot order; [`Self::heal_jump_slots`] keeps its length.
    pub jump_session: Vec<String>,
}

/// The keymap as `ui-settings.json` keeps it: by each shortcut's [`ShortcutId::stored_key`],
/// only the combos that differ from their default ("" for one the person unbound). An entry
/// that is not a string is skipped, so one bad value costs nothing else.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct StoredKeymap(std::collections::BTreeMap<String, serde_json::Value>);

impl From<StoredKeymap> for KeymapConfig {
    /// Every shortcut the file leaves out takes its default, except that a default never takes a
    /// chord the person gave another action: that shortcut arrives unbound instead.
    fn from(StoredKeymap(stored): StoredKeymap) -> Self {
        let chosen = |id: ShortcutId| {
            stored.get(id.stored_key().as_ref()).and_then(serde_json::Value::as_str)
        };
        let mut keymap = Self::default();
        for id in ShortcutId::ALL {
            if let Some(combo) = chosen(id) {
                keymap.set(id, combo.to_string());
            } else {
                let default = platform_combo(id.default_combo());
                let taken = ShortcutId::ALL.into_iter().any(|other| {
                    chosen(other)
                        .is_some_and(|combo| !combo.is_empty() && platform_combo(combo) == default)
                });
                if taken {
                    keymap.set(id, String::new());
                }
            }
        }
        keymap
    }
}

impl From<KeymapConfig> for StoredKeymap {
    fn from(keymap: KeymapConfig) -> Self {
        Self(
            ShortcutId::ALL
                .into_iter()
                .filter(|&id| keymap.get(id) != id.default_combo())
                .map(|id| (id.stored_key().into_owned(), keymap.get(id).into()))
                .collect(),
        )
    }
}

/// Stable key for device-local preferences that belong to one workspace profile; `None` until
/// the engine has attached, which callers read as "identity not ready", never as a reason for
/// destructive cleanup.
pub fn workspace_profile_key(scope: Option<WorkspaceScope>) -> Option<String> {
    let WorkspaceScope::Local = scope?;
    Some("local".to_owned())
}

impl Default for KeymapConfig {
    fn default() -> Self {
        Self {
            save_file: ShortcutId::SaveFile.default_combo().into(),
            browser_reload: ShortcutId::BrowserReload.default_combo().into(),
            toggle_sidebar: ShortcutId::ToggleSidebar.default_combo().into(),
            toggle_changes: ShortcutId::ToggleChanges.default_combo().into(),
            toggle_files: ShortcutId::ToggleFiles.default_combo().into(),
            new_session: ShortcutId::NewSession.default_combo().into(),
            new_project: ShortcutId::NewProject.default_combo().into(),
            next_session: ShortcutId::NextSession.default_combo().into(),
            prev_session: ShortcutId::PrevSession.default_combo().into(),
            archive_session: ShortcutId::ArchiveSession.default_combo().into(),
            reopen_closed_tab: ShortcutId::ReopenClosedTab.default_combo().into(),
            next_needs_you: ShortcutId::NextNeedsYou.default_combo().into(),
            inbox: ShortcutId::Inbox.default_combo().into(),
            jump_session: JUMP_DEFAULTS.iter().map(|c| (*c).to_string()).collect(),
        }
    }
}

impl KeymapConfig {
    pub fn get(&self, id: ShortcutId) -> &str {
        match id {
            ShortcutId::SaveFile => &self.save_file,
            ShortcutId::BrowserReload => &self.browser_reload,
            ShortcutId::ToggleSidebar => &self.toggle_sidebar,
            ShortcutId::ToggleChanges => &self.toggle_changes,
            ShortcutId::ToggleFiles => &self.toggle_files,
            ShortcutId::NewSession => &self.new_session,
            ShortcutId::NewProject => &self.new_project,
            ShortcutId::NextSession => &self.next_session,
            ShortcutId::PrevSession => &self.prev_session,
            ShortcutId::ArchiveSession => &self.archive_session,
            ShortcutId::ReopenClosedTab => &self.reopen_closed_tab,
            ShortcutId::NextNeedsYou => &self.next_needs_you,
            ShortcutId::Inbox => &self.inbox,
            ShortcutId::JumpSession(slot) => {
                self.jump_session.get(slot).map(String::as_str).unwrap_or("")
            }
        }
    }

    pub fn set(&mut self, id: ShortcutId, combo: String) {
        match id {
            ShortcutId::SaveFile => self.save_file = combo,
            ShortcutId::BrowserReload => self.browser_reload = combo,
            ShortcutId::ToggleSidebar => self.toggle_sidebar = combo,
            ShortcutId::ToggleChanges => self.toggle_changes = combo,
            ShortcutId::ToggleFiles => self.toggle_files = combo,
            ShortcutId::NewSession => self.new_session = combo,
            ShortcutId::NewProject => self.new_project = combo,
            ShortcutId::NextSession => self.next_session = combo,
            ShortcutId::PrevSession => self.prev_session = combo,
            ShortcutId::ArchiveSession => self.archive_session = combo,
            ShortcutId::ReopenClosedTab => self.reopen_closed_tab = combo,
            ShortcutId::NextNeedsYou => self.next_needs_you = combo,
            ShortcutId::Inbox => self.inbox = combo,
            ShortcutId::JumpSession(slot) => {
                if slot < JUMP_SLOTS {
                    if self.jump_session.len() < JUMP_SLOTS {
                        self.heal_jump_slots();
                    }
                    self.jump_session[slot] = combo;
                }
            }
        }
    }

    pub fn reset(&mut self, id: ShortcutId) {
        self.set(id, id.default_combo().to_string());
    }

    /// Restore the jump list to exactly [`JUMP_SLOTS`] entries: a hand-edited
    /// or older file may carry a short, long or absent list. Surviving entries
    /// keep their slot; missing ones take the default.
    pub fn heal_jump_slots(&mut self) {
        self.jump_session.truncate(JUMP_SLOTS);
        while self.jump_session.len() < JUMP_SLOTS {
            self.jump_session.push(JUMP_DEFAULTS[self.jump_session.len()].to_string());
        }
    }
}

/// Build a combo string from a recorded keystroke. The primary modifier
/// (cmd on macOS, ctrl elsewhere) becomes "mod"; bare modifier presses record
/// nothing.
pub fn combo_from_keystroke(
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    key: &str,
) -> Option<String> {
    // An iPad's ⌘ too.
    combo_from_keystroke_on(crate::keys::MOD_IS_COMMAND, ctrl, alt, shift, cmd, key)
}

/// [`combo_from_keystroke`] for an explicit platform — the ctrl spelling is
/// platform-dependent, so both paths need to be exercisable from one machine.
pub fn combo_from_keystroke_on(
    mac: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    key: &str,
) -> Option<String> {
    let key = key.trim().to_lowercase();
    if key.is_empty()
        || matches!(key.as_str(), "ctrl" | "control" | "alt" | "shift" | "cmd" | "platform" | "fn")
    {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    // On macOS ctrl stays its own modifier rather than folding into "mod":
    // re-recording Ctrl+Tab as Cmd+Tab would hand the combo to the OS app
    // switcher, which never delivers it to the window.
    let ctrl_is_primary = ctrl && !mac;
    if cmd || ctrl_is_primary {
        parts.push("mod");
    }
    if ctrl && !ctrl_is_primary {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    parts.push(&key);
    Some(parts.join("-"))
}

/// Shortcut ids whose combos collide with another shortcut (conflict detection).
pub fn conflicted_shortcuts(keymap: &KeymapConfig) -> Vec<ShortcutId> {
    ShortcutId::ALL
        .into_iter()
        .filter(|&id| {
            let combo = keymap.get(id);
            !combo.is_empty()
                && ShortcutId::ALL
                    .into_iter()
                    .any(|other| other != id && keymap.get(other) == combo)
        })
        .collect()
}

/// The modifiers a stored combo carries, as `(mod, alt, shift)`. Everything
/// before the final segment is a modifier; the final segment is the key.
pub fn combo_modifiers(combo: &str) -> (bool, bool, bool) {
    let mut parts: Vec<&str> = combo.split('-').collect();
    parts.pop();
    (parts.contains(&"mod"), parts.contains(&"alt"), parts.contains(&"shift"))
}

/// Whether the sidebar should show its jump hints for the currently held
/// modifiers (t3code `shouldShowThreadJumpHintsForModifiers`). The held set
/// must match a jump combo EXACTLY, so adding Shift or Alt hides the hints and
/// a chord like Cmd+Shift+4 never flashes the overlay. `primary` is the held
/// "mod" key — cmd on macOS, ctrl elsewhere.
///
/// A jump combo with no modifiers at all never shows hints: it would otherwise
/// match the resting state and pin the overlay open. Pure.
pub fn jump_hints_visible(keymap: &KeymapConfig, primary: bool, alt: bool, shift: bool) -> bool {
    if !(primary || alt || shift) {
        return false;
    }
    ShortcutId::ALL
        .into_iter()
        .filter(|id| id.jump_slot().is_some())
        .any(|id| combo_modifiers(keymap.get(id)) == (primary, alt, shift))
}

/// Translate a stored combo into a bindable keystroke for this platform.
pub fn platform_combo(combo: &str) -> String {
    platform_combo_on(crate::keys::MOD_IS_COMMAND, combo) // an iPad's ⌘ too
}

/// A stored combo as the keystroke [`platform_combo`] binds, parsed for this platform.
pub fn parse_combo(combo: &str) -> Result<gpui::Keystroke, gpui::InvalidKeystrokeError> {
    gpui::Keystroke::parse_with_os(&platform_combo(combo), crate::keys::OS)
}

/// [`platform_combo`] for an explicit platform (see [`combo_from_keystroke_on`]).
pub fn platform_combo_on(mac: bool, combo: &str) -> String {
    let primary = if mac { "cmd" } else { "ctrl" };
    combo
        .split('-')
        .map(|part| if part == "mod" { primary } else { part })
        .collect::<Vec<_>>()
        .join("-")
}

/// Where the active keyboard layout puts shortcut keys: the US key a combo
/// is written with → the key that layout types in its place. This is
/// Apple's own relocation — an AZERTY Mac reaches ⌘1 as ⌘&, ⌘/ as ⌘: and
/// ⌘[ as ⌘^ — which gpui ships as `get_key_equivalents` but applies only to
/// bindings loaded with it. Empty on US layouts and off macOS.
pub type KeyEquivalents = HashMap<char, char>;

/// The active layout's [`KeyEquivalents`].
pub fn key_equivalents(cx: &App) -> KeyEquivalents {
    gpui::PlatformKeyboardMapper::get_key_equivalents(cx.keyboard_mapper().as_ref())
        .map(|table| table.iter().map(|(&us, &here)| (us, here)).collect())
        .unwrap_or_default()
}

thread_local! {
    /// The table the keymap was last bound against. A chord read outside gpui's keymap
    /// matches by its US spelling through it ([`bound_canonical_combo`]); hints read the bound
    /// keymap itself.
    static BOUND_KEY_EQUIVALENTS: RefCell<KeyEquivalents> = RefCell::default();
}

/// Called by `shell::apply_keymap` with the table it just bound against.
pub(crate) fn set_bound_key_equivalents(equivalents: KeyEquivalents) {
    BOUND_KEY_EQUIVALENTS.set(equivalents);
}

/// The key `equivalents` puts in place of the US `key` — gpui's
/// `MacKeyboardMapper::map_key_equivalent`, for Slopty's own strings. Named
/// keys ("tab", "f5") and characters the layout keeps pass through.
pub fn localize_key<'a>(key: &'a str, equivalents: &KeyEquivalents) -> Cow<'a, str> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(us), None) => {
            equivalents.get(&us).map_or(Cow::Borrowed(key), |here| Cow::Owned(here.to_string()))
        }
        _ => Cow::Borrowed(key),
    }
}

/// The US key a combo stores for a key this layout reported: the inverse of
/// [`localize_key`], so a recorded chord keeps the spelling its default uses
/// (AZERTY ⌘& records as "mod-1") and conflict checks on stored combos still
/// see one chord per key. `None` when the layout relocates `key` itself and
/// nothing lands on it — AZERTY's @ key — since no stored combo reaches it.
/// Two US keys sharing one target (Icelandic ] and }) resolve to the lower.
pub fn canonical_key(key: &str, equivalents: &KeyEquivalents) -> Option<String> {
    let mut chars = key.chars();
    let (Some(here), None) = (chars.next(), chars.next()) else {
        return Some(key.to_string());
    };
    if let Some(us) =
        equivalents.iter().filter(|&(_, &target)| target == here).map(|(&us, _)| us).min()
    {
        return Some(us.to_string());
    }
    (!equivalents.contains_key(&here)).then(|| key.to_string())
}

/// Split a combo into its modifier prefix and key: ("mod-shift-", "/").
fn split_combo(combo: &str) -> (&str, &str) {
    // "-" is a key too: "mod--" is Mod+Minus.
    let key_start = if combo.ends_with('-') {
        combo.len() - 1
    } else {
        combo.rfind('-').map_or(0, |ix| ix + 1)
    };
    combo.split_at(key_start)
}

/// `combo` with its key localized ("mod-/" → "mod-:" on AZERTY), in either
/// the stored ("mod-…") or the bindable ("cmd-…") spelling.
pub fn localize_combo(combo: &str, equivalents: &KeyEquivalents) -> String {
    let (modifiers, key) = split_combo(combo);
    format!("{modifiers}{}", localize_key(key, equivalents))
}

/// The inverse of [`localize_combo`] (see [`canonical_key`]).
pub fn canonical_combo(combo: &str, equivalents: &KeyEquivalents) -> Option<String> {
    let (modifiers, key) = split_combo(combo);
    canonical_key(key, equivalents).map(|key| format!("{modifiers}{key}"))
}

/// macOS French (AZERTY), as gpui_macos `keyboard.rs` lists it for
/// `com.apple.keylayout.French` — the test platform has no layout table.
#[cfg(test)]
pub(crate) fn azerty_key_equivalents() -> KeyEquivalents {
    [
        ('!', '1'),
        ('"', '%'),
        ('#', '3'),
        ('$', '4'),
        ('%', '5'),
        ('&', '7'),
        ('(', '9'),
        (')', '0'),
        ('*', '8'),
        ('.', ';'),
        ('/', ':'),
        ('0', 'à'),
        ('1', '&'),
        ('2', 'é'),
        ('3', '"'),
        ('4', '\''),
        ('5', '('),
        ('6', '§'),
        ('7', 'è'),
        ('8', '!'),
        ('9', 'ç'),
        (':', '°'),
        (';', ')'),
        ('<', '.'),
        ('>', '/'),
        ('@', '2'),
        ('[', '^'),
        ('\'', 'ù'),
        ('\\', '`'),
        (']', '$'),
        ('^', '6'),
        ('`', '<'),
        ('{', '¨'),
        ('|', '£'),
        ('}', '*'),
        ('~', '>'),
    ]
    .into_iter()
    .collect()
}

/// The stored (US-key) spelling of a chord read outside gpui's keymap, by
/// the table the keymap is bound to (see [`canonical_combo`]).
pub fn bound_canonical_combo(combo: &str) -> Option<String> {
    BOUND_KEY_EQUIVALENTS.with_borrow(|equivalents| canonical_combo(combo, equivalents))
}
impl UiSettings {
    /// Three-way merge for a view that keeps a working copy of the settings:
    /// fields `edited` changed since `base` win, every other field keeps
    /// `current`. A stale copy therefore never reverts a choice another
    /// surface saved meanwhile. The destructure is exhaustive, so a new field
    /// does not compile until it is merged here too.
    pub fn merge_changes(base: &Self, edited: &Self, mut current: Self) -> Self {
        macro_rules! merge {
            ($($field:ident),* $(,)?) => {{
                let Self { $($field),* } = edited;
                $(if *$field != base.$field {
                    current.$field = $field.clone();
                })*
            }};
        }
        merge!(
            window_geometry,
            sidebar_width,
            sidebar_collapsed,
            last_space_id,
            space_filter,
            machine_filter,
            notifications_enabled,
            right_pane_width,
            terminal_height,
            session_tabs,
            output_formats,
            palette_uses,
            keymap,
            settings_section,
            appearance,
            ui_font_family,
            ui_font_size,
            terminal_font_family,
            terminal_font_size,
            code_font_family,
            code_font_size,
            theme_selection,
            diff_split,
            diff_wrap,
            diff_ignore_whitespace,
            diff_show_tree,
            conversation_width,
            open_web_links_in_slopty,
            files_autosave_enabled,
            files_autosave_delay_ms,
            files_word_wrap,
            files_show_all,
            accent,
            surface,
            pause_animations_in_background,
            smooth_scrolling,
        );
        current
    }

    /// Clamp widths into their legal ranges (also heals NaN to defaults).
    pub fn clamped(mut self) -> Self {
        self.window_geometry = self.window_geometry.filter(|geometry| geometry.is_valid());
        self.sidebar_width =
            clamp_or(self.sidebar_width, SIDEBAR_MIN, SIDEBAR_MAX, SIDEBAR_DEFAULT);
        // The right pane has no persisted upper bound: its live drag clamps
        // against the current window, which is unavailable while loading.
        self.right_pane_width = min_or(self.right_pane_width, RIGHT_PANE_MIN, RIGHT_PANE_DEFAULT);
        self.terminal_height = clamp_or(
            self.terminal_height,
            TERMINAL_MIN_HEIGHT,
            TERMINAL_ABS_MAX_HEIGHT,
            TERMINAL_DEFAULT_HEIGHT,
        );
        self.files_autosave_delay_ms = self
            .files_autosave_delay_ms
            .clamp(FILES_AUTOSAVE_DELAY_MIN_MS, FILES_AUTOSAVE_DELAY_MAX_MS);
        self.terminal_font_size = clamp_or(
            self.terminal_font_size,
            crate::typography::FONT_SIZE_MIN,
            crate::typography::FONT_SIZE_MAX,
            crate::typography::TERMINAL_FONT_SIZE_DEFAULT,
        );
        self.code_font_size = clamp_or(
            self.code_font_size,
            crate::typography::FONT_SIZE_MIN,
            crate::typography::FONT_SIZE_MAX,
            crate::typography::CODE_FONT_SIZE_DEFAULT,
        );
        self.ui_font_size = self.ui_font_size.normalized();
        self.keymap.heal_jump_slots();
        self
    }

    /// Load from `{data_dir}/ui-settings.json`; defaults when there is none. A file that cannot
    /// be read or parsed gives the defaults too, and is moved aside as `ui-settings.json.bak`
    /// first ([`announce_set_aside`] says so once the window is up).
    pub fn load(data_dir: &Path) -> Self {
        match std::fs::read_to_string(Self::path(data_dir)) {
            Ok(text) => {
                match serde_json::from_str::<serde_json::Value>(&text).and_then(|mut value| {
                    if let Some(settings) = value.as_object_mut() {
                        // The files-editor size was the first user-facing code
                        // size; it now drives every code surface.
                        if let Some(legacy) = settings.remove("filesEditorFontSize") {
                            settings.entry("codeFontSize").or_insert(legacy);
                        }
                    }
                    serde_json::from_value::<UiSettings>(value)
                }) {
                    Ok(settings) => settings.clamped(),
                    Err(err) => {
                        // A file that does not parse is moved aside and said, never
                        // overwritten by the next save.
                        tracing::warn!(error = %err, "ui-settings corrupt; using defaults");
                        set_aside(data_dir);
                        Self::default()
                    }
                }
            }
            // None yet is the defaults; one that cannot be read (not UTF-8, refused)
            // is moved aside as a corrupt one is.
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(error = %err, "ui-settings unreadable; using defaults");
                set_aside(data_dir);
                Self::default()
            }
        }
    }

    /// Write atomically (temp file + rename) so a crash mid-write never corrupts.
    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(FILE_NAME)
    }
}

/// Settings files [`UiSettings::load`] could not read, by the data directory they were in, with
/// where each was moved (`None` when the move failed too), until [`init`] for that directory
/// takes them.
static SET_ASIDE: std::sync::Mutex<Vec<(PathBuf, Option<PathBuf>)>> =
    std::sync::Mutex::new(Vec::new());

/// Where a settings file that could not be read goes: beside it, so nothing writes over it.
pub fn backup_path(data_dir: &Path) -> PathBuf {
    UiSettings::path(data_dir).with_extension("json.bak")
}

/// Move `data_dir`'s settings file aside as [`backup_path`], replacing an older one, and note it
/// for [`init`].
fn set_aside(data_dir: &Path) {
    let backup = backup_path(data_dir);
    let moved = match std::fs::rename(UiSettings::path(data_dir), &backup) {
        Ok(()) => Some(backup),
        Err(err) => {
            tracing::warn!(error = %err, "the unreadable ui-settings could not be moved aside");
            None
        }
    };
    if let Ok(mut noted) = SET_ASIDE.lock() {
        noted.push((data_dir.to_path_buf(), moved));
    }
}

/// The note [`set_aside`] left for `data_dir`, taken.
fn take_set_aside(data_dir: &Path) -> Option<Option<PathBuf>> {
    let mut noted = SET_ASIDE.lock().ok()?;
    let at = noted.iter().position(|(dir, _)| dir == data_dir)?;
    Some(noted.remove(at).1)
}

/// Say, once, as an error toast, that the settings file could not be read and where it went;
/// whether there was one to say.
pub fn announce_set_aside(cx: &mut App) -> bool {
    let Some(set_aside) = cx
        .try_global::<SettingsStore>()
        .is_some()
        .then(|| cx.global_mut::<SettingsStore>().set_aside.take())
        .flatten()
    else {
        return false;
    };
    let words = match set_aside {
        Some(backup) => format!(
            "Couldn\u{2019}t read your settings, so Slopty started from its defaults. The old file is kept as {}.",
            backup.file_name().map_or_else(
                || backup.display().to_string(),
                |name| name.to_string_lossy().into_owned()
            ),
        ),
        None => {
            "Couldn\u{2019}t read your settings, so Slopty started from its defaults.".to_owned()
        }
    };
    crate::toast::error(cx, words);
    true
}

fn clamp_or(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() { value.clamp(min, max) } else { default }
}

fn min_or(value: f32, min: f32, default: f32) -> f32 {
    if value.is_finite() { value.max(min) } else { default }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_and_partial_settings_load_with_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "notificationsEnabled": false}"#,
        )
        .unwrap();

        let loaded = UiSettings::load(dir.path());
        assert!(loaded.open_web_links_in_slopty);
        assert!(!loaded.pause_animations_in_background);
        assert!(loaded.smooth_scrolling);
        assert_eq!(loaded.sidebar_width, 300.0);
        assert!(!loaded.notifications_enabled);
    }

    #[test]
    fn window_geometry_round_trips_and_legacy_settings_default() {
        let dir = tempfile::tempdir().unwrap();
        let geometry = WindowGeometry {
            display_uuid: None,
            x: -1500.0,
            y: 40.0,
            width: 1200.0,
            height: 800.0,
        };
        let settings = UiSettings { window_geometry: Some(geometry), ..Default::default() };
        settings.save(dir.path()).unwrap();
        assert_eq!(UiSettings::load(dir.path()).window_geometry, Some(geometry));
        assert_eq!(WindowGeometry::from_bounds(geometry.bounds()), geometry);
        let legacy: UiSettings = serde_json::from_str(r#"{"sidebarWidth":300}"#).unwrap();
        assert_eq!(legacy.window_geometry, None);
        assert_eq!(legacy.sidebar_width, 300.0);
    }

    #[test]
    fn window_geometry_restores_display_identity_with_overlapping_local_coordinates() {
        let primary = WindowGeometry {
            display_uuid: Some(uuid::Uuid::from_u128(1)),
            x: 0.0,
            y: 25.0,
            width: 1920.0,
            height: 1080.0,
        };
        let secondary = WindowGeometry { display_uuid: Some(uuid::Uuid::from_u128(2)), ..primary };
        let saved = WindowGeometry { x: 100.0, y: 80.0, width: 1200.0, height: 800.0, ..secondary };
        let encoded = serde_json::to_string(&saved).unwrap();
        let saved: WindowGeometry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(saved.restore(&[primary, secondary], 0), Some((1, saved)));
        assert_eq!(saved.restore(&[secondary, primary], 1), Some((0, saved)));
    }

    #[test]
    fn settings_section_round_trips_and_old_or_unknown_values_read_as_general() {
        use crate::shell::SettingsSection;
        let dir = tempfile::tempdir().unwrap();
        let settings =
            UiSettings { settings_section: SettingsSection::Appearance, ..Default::default() };
        settings.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(UiSettings::path(dir.path())).unwrap();
        assert!(text.contains(r#""settingsSection": "appearance""#), "{text}");
        assert_eq!(UiSettings::load(dir.path()).settings_section, SettingsSection::Appearance);
        for section in SettingsSection::ALL {
            let encoded = serde_json::to_string(&UiSettings {
                settings_section: section,
                ..Default::default()
            })
            .unwrap();
            let decoded: UiSettings = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded.settings_section, section);
        }

        // A file written before the field existed.
        let legacy: UiSettings = serde_json::from_str(r#"{"sidebarWidth":300}"#).unwrap();
        assert_eq!(legacy.settings_section, SettingsSection::Appearance);
        assert_eq!(legacy.sidebar_width, 300.0);
        // Unknown names and malformed values read as Appearance without
        // defaulting the rest of the file.
        for raw in [r#""billing""#, r#""general""#, "42", "null", r#"{"section":"devices"}"#] {
            std::fs::write(
                UiSettings::path(dir.path()),
                format!(r#"{{"sidebarWidth": 300, "settingsSection": {raw}}}"#),
            )
            .unwrap();
            let loaded = UiSettings::load(dir.path());
            assert_eq!(loaded.settings_section, SettingsSection::Appearance, "{raw}");
            assert_eq!(loaded.sidebar_width, 300.0, "{raw}");
        }
    }

    #[test]
    fn window_geometry_recenters_when_saved_display_is_disconnected() {
        let primary = WindowGeometry {
            display_uuid: Some(uuid::Uuid::from_u128(1)),
            x: 0.0,
            y: 25.0,
            width: 1440.0,
            height: 900.0,
        };
        let saved = WindowGeometry {
            display_uuid: Some(uuid::Uuid::from_u128(2)),
            x: 500.0,
            y: 300.0,
            width: 1200.0,
            height: 800.0,
        };
        assert_eq!(
            saved.restore(&[primary], 0),
            Some((
                0,
                WindowGeometry { display_uuid: primary.display_uuid, x: 120.0, y: 75.0, ..saved }
            ))
        );
        assert_eq!(saved.restore(&[], 0), None);
        let oversized = WindowGeometry { width: 2400.0, height: 1600.0, ..saved };
        assert_eq!(oversized.restore(&[primary], 0), Some((0, primary)));
    }

    #[test]
    fn window_geometry_without_display_identity_uses_primary() {
        let saved: WindowGeometry =
            serde_json::from_str(r#"{"x":100,"y":80,"width":1200,"height":800}"#).unwrap();
        assert_eq!(saved.display_uuid, None);
        let display = WindowGeometry {
            display_uuid: Some(uuid::Uuid::from_u128(1)),
            x: 0.0,
            y: 25.0,
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            saved.restore(&[display, display], 1),
            Some((1, WindowGeometry { display_uuid: display.display_uuid, ..saved }))
        );
    }

    #[test]
    fn window_geometry_rejects_invalid_values_without_resetting_settings() {
        let valid =
            WindowGeometry { display_uuid: None, x: 40.0, y: 50.0, width: 1200.0, height: 800.0 };
        for geometry in [
            WindowGeometry { x: f32::NAN, ..valid },
            WindowGeometry { y: f32::INFINITY, ..valid },
            WindowGeometry { width: 0.0, ..valid },
            WindowGeometry { height: -1.0, ..valid },
        ] {
            let settings = UiSettings {
                window_geometry: Some(geometry),
                sidebar_width: 300.0,
                ..Default::default()
            }
            .clamped();
            assert_eq!(settings.window_geometry, None);
            assert_eq!(settings.sidebar_width, 300.0);
        }
    }

    #[test]
    fn window_geometry_preserves_position_on_negative_coordinate_display() {
        let display = WindowGeometry {
            display_uuid: None,
            x: -1920.0,
            y: -200.0,
            width: 1920.0,
            height: 1080.0,
        };
        let geometry = WindowGeometry {
            display_uuid: None,
            x: -1800.0,
            y: -100.0,
            width: 1200.0,
            height: 800.0,
        };
        assert_eq!(geometry.fit(display), geometry);
    }

    #[test]
    fn window_geometry_fits_smaller_display_and_keeps_titlebar_visible() {
        let display =
            WindowGeometry { display_uuid: None, x: 0.0, y: 25.0, width: 1280.0, height: 720.0 };
        let geometry = WindowGeometry {
            display_uuid: None,
            x: 2000.0,
            y: -1000.0,
            width: 2000.0,
            height: 1500.0,
        };
        assert_eq!(geometry.fit(display), display);
        let small = WindowGeometry { width: 800.0, height: 500.0, ..display };
        assert_eq!(geometry.fit(small), small);
        let tiny = WindowGeometry { width: 100.0, height: 100.0, ..display };
        assert_eq!(tiny.fit(display).width, 900.0);
        assert_eq!(tiny.fit(display).height, 600.0);
    }

    #[test]
    fn unknown_keys_from_a_newer_build_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth":300.0,"someFutureKey":"whatever","anotherOne":{"nested":1}}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, 300.0);
        assert_eq!(loaded.terminal_font_family, crate::typography::UiFontFamily::GeistMono);
        assert_eq!(loaded.terminal_font_size, crate::typography::TERMINAL_FONT_SIZE_DEFAULT);
        assert_eq!(loaded.code_font_family, crate::typography::UiFontFamily::GeistMono);
        assert_eq!(loaded.code_font_size, crate::typography::CODE_FONT_SIZE_DEFAULT);
    }

    #[test]
    fn obsolete_steering_preference_does_not_reset_other_settings() {
        let loaded: UiSettings = serde_json::from_str(
            r#"{"activeTurnSendBehavior":"steer","sidebarWidth":300,"notificationsEnabled":false}"#,
        )
        .unwrap();
        assert_eq!(loaded.sidebar_width, 300.0);
        assert!(!loaded.notifications_enabled);
        assert!(serde_json::to_value(&loaded).unwrap().get("activeTurnSendBehavior").is_none());
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let settings = UiSettings {
            window_geometry: None,
            sidebar_width: 300.0,
            sidebar_collapsed: true,
            last_space_id: Some("space-1".into()),
            space_filter: Some("space-1".into()),
            machine_filter: Some("studio".into()),
            notifications_enabled: false,
            right_pane_width: 700.0,
            terminal_height: 320.0,
            session_tabs: std::collections::BTreeMap::new(),
            palette_uses: std::collections::BTreeMap::from([(
                "Toggle sidebar".into(),
                PaletteUse { count: 3, last_ms: 1_793_000_000_000 },
            )]),
            output_formats: std::collections::BTreeMap::from([("Bash".into(), "markdown".into())]),
            keymap: KeymapConfig {
                toggle_sidebar: "mod-shift-s".into(),
                ..KeymapConfig::default()
            },
            settings_section: crate::shell::SettingsSection::Shortcuts,
            appearance: crate::appearance::AppearanceMode::Light,
            ui_font_family: crate::typography::UiFontFamily::Installed("Arial".into()),
            ui_font_size: crate::typography::UiFontSize::ALL[5],
            theme_selection: slopty_shell_theme::ThemeSelection {
                light: "rose-pine-dawn".into(),
                dark: "catppuccin-mocha".into(),
            },
            diff_split: true,
            diff_wrap: true,
            diff_ignore_whitespace: true,
            diff_show_tree: true,
            conversation_width: ConversationWidth::Wide,
            open_web_links_in_slopty: false,
            files_autosave_enabled: true,
            files_autosave_delay_ms: 1_500,
            files_word_wrap: true,
            terminal_font_family: crate::typography::UiFontFamily::Installed("Menlo".into()),
            terminal_font_size: 15.0,
            code_font_family: crate::typography::UiFontFamily::Geist,
            code_font_size: 11.0,
            files_show_all: true,
            accent: slopty_shell_theme::AccentSelection::Preset(
                slopty_shell_theme::AccentPreset::Cyan,
            ),
            surface: slopty_shell_theme::SurfacePreference::Frosted,
            pause_animations_in_background: true,
            smooth_scrolling: false,
        };
        settings.save(dir.path()).unwrap();
        let json = std::fs::read_to_string(UiSettings::path(dir.path())).unwrap();
        assert!(json.contains(r#""diffWrap": true"#));
        assert!(json.contains(r#""machineFilter": "studio""#));
        assert!(json.contains(r#""outputFormats": {"#));
        // The shortcuts keep only the changed combo.
        assert!(json.contains(r#""toggleSidebar": "mod-shift-s""#));
        assert!(!json.contains(r#""saveFile""#));
        assert_eq!(UiSettings::load(dir.path()), settings);
        assert!(json.contains(r#""openWebLinksInSlopty": false"#));
        assert!(json.contains(r#""conversationWidth": "wide""#));
        assert!(json.contains(r#""pauseAnimationsInBackground": true"#));
        assert!(json.contains(r#""smoothScrolling": false"#));
        assert!(json.contains(r#""terminalFontFamily": "installed:Menlo""#));
        assert!(json.contains(r#""terminalFontSize": 15.0"#));
        assert!(json.contains(r#""codeFontFamily": "geist""#));
        assert!(json.contains(r#""codeFontSize": 11.0"#));
    }

    #[test]
    fn stale_revision_cannot_be_considered_the_latest_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore {
            current: UiSettings::default(),
            data_dir: dir.path().to_path_buf(),
            revision: 0,
            saved_revision: 0,
            save_task: None,
            set_aside: None,
        };

        store.current.sidebar_width = 300.0;
        store.revision += 1;
        let (stale, stale_revision) = store.snapshot();

        store.current.ui_font_family = crate::typography::UiFontFamily::Installed("Arial".into());
        store.revision += 1;
        stale.save(dir.path()).unwrap();
        assert!(!store.mark_saved(stale_revision));

        let (latest, latest_revision) = store.snapshot();
        latest.save(dir.path()).unwrap();
        assert!(store.mark_saved(latest_revision));
        let reloaded = UiSettings::load(dir.path());
        assert_eq!(reloaded.sidebar_width, 300.0);
        assert_eq!(
            reloaded.ui_font_family,
            crate::typography::UiFontFamily::Installed("Arial".into())
        );
    }

    // Each preset's cap against the window, Amp's breakpoints; the choice persists.
    #[test]
    fn conversation_widths_follow_the_window_and_persist() {
        use ConversationWidth::*;
        assert_eq!(UiSettings::default().conversation_width, Default);
        for (window, default, wide, full) in [
            (1000.0, 672.0, 672.0, 1000.0),
            (1279.0, 672.0, 767.4, 1279.0),
            (1280.0, 724.0, 768.0, 1280.0),
            (1600.0, 750.0, 960.0, 1600.0),
            (500.0, 672.0, 672.0, 672.0),
        ] {
            assert_eq!(Default.cap(window), default, "{window}");
            assert!((Wide.cap(window) - wide).abs() < 0.01, "{window}");
            assert_eq!(Full.cap(window), full, "{window}");
        }
        let dir = tempfile::tempdir().unwrap();
        let settings = UiSettings { conversation_width: Full, ..UiSettings::default() };
        settings.save(dir.path()).unwrap();
        assert_eq!(UiSettings::load(dir.path()).conversation_width, Full);
    }

    /// A settings file written before light mode existed has no `appearance`
    /// key; it must load as "follow the OS" rather than failing the whole parse
    /// and resetting every other preference to defaults.
    #[test]
    fn settings_without_appearance_default_to_system() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "notificationsEnabled": false}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.appearance, crate::appearance::AppearanceMode::System);
        assert_eq!(loaded.accent, slopty_shell_theme::AccentSelection::ThemeDefault);
        assert_eq!(loaded.surface, slopty_shell_theme::SurfacePreference::ThemeDefault);
        assert_eq!(loaded.sidebar_width, 300.0);
        assert!(!loaded.notifications_enabled, "other keys still parse");
        assert_eq!(loaded.files_autosave_delay_ms, FILES_AUTOSAVE_DELAY_DEFAULT_MS);
        assert!(!loaded.files_autosave_enabled);
        assert!(!loaded.files_word_wrap);
        assert_eq!(loaded.code_font_size, crate::typography::CODE_FONT_SIZE_DEFAULT);
        assert_eq!(loaded.terminal_font_size, crate::typography::TERMINAL_FONT_SIZE_DEFAULT);
        assert!(!loaded.files_show_all);
    }

    #[test]
    fn settings_without_ui_font_default_to_geist() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "notificationsEnabled": false}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.ui_font_family, crate::typography::UiFontFamily::Geist);
        assert_eq!(loaded.sidebar_width, 300.0);
        assert!(!loaded.notifications_enabled);
        assert!(!loaded.diff_wrap);
        assert_eq!(loaded.ui_font_size, crate::typography::UiFontSize::default());
    }

    #[test]
    fn unsupported_ui_font_size_snaps_to_the_nearest_choice() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"uiFontSize": 19, "notificationsEnabled": false}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.ui_font_size.pixels(), 18.0);
        assert!(!loaded.notifications_enabled);
    }

    #[test]
    fn unknown_ui_font_falls_back_without_resetting_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "uiFontFamily": "futureSans"}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.ui_font_family, crate::typography::UiFontFamily::Geist);
        assert_eq!(loaded.sidebar_width, 300.0);
    }

    // A file that cannot be read is moved aside and said, and the next save never
    // writes over it; none at all is quietly the defaults.
    #[gpui::test]
    fn a_corrupt_file_is_moved_aside_and_said(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UiSettings::load(dir.path()), UiSettings::default());
        cx.update(|cx| {
            init(UiSettings::default(), dir.path(), cx);
            assert!(!announce_set_aside(cx), "no file is nothing to say");
        });
        for bad in [b"{not json".as_slice(), &[0xff, 0xfe, b'{', b'}']] {
            std::fs::write(UiSettings::path(dir.path()), bad).unwrap();
            assert_eq!(UiSettings::load(dir.path()), UiSettings::default());
            assert!(!UiSettings::path(dir.path()).exists(), "moved, not left to be saved over");
            assert_eq!(std::fs::read(backup_path(dir.path())).unwrap(), bad, "kept whole");
            cx.update(|cx| {
                crate::toast::clear(cx);
                init(UiSettings::default(), dir.path(), cx);
                assert!(announce_set_aside(cx));
                let shown = crate::toast::shown(cx);
                assert_eq!(shown.len(), 1, "{shown:?}");
                assert_eq!(shown[0].0, crate::toast::ToastKind::Error);
                assert!(shown[0].1.contains("ui-settings.json.bak"), "{shown:?}");
                assert!(!announce_set_aside(cx), "said once");
                update(SavePolicy::Immediate, cx, |settings| settings.sidebar_width = 300.0);
            });
            assert_eq!(UiSettings::load(dir.path()).sidebar_width, 300.0, "saved anew");
            assert_eq!(std::fs::read(backup_path(dir.path())).unwrap(), bad, "the old one kept");
        }
    }

    #[test]
    fn the_workspace_profile_key_waits_for_the_engine() {
        assert_eq!(workspace_profile_key(None), None);
        assert_eq!(workspace_profile_key(Some(WorkspaceScope::Local)).as_deref(), Some("local"));
    }

    #[test]
    fn loaded_values_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 10000, "rightPaneWidth": 1}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, SIDEBAR_MAX);
        assert_eq!(loaded.right_pane_width, RIGHT_PANE_MIN);
        assert_eq!(
            UiSettings { sidebar_width: 1.0, ..Default::default() }.clamped().sidebar_width,
            SIDEBAR_MIN
        );
        assert_eq!(
            UiSettings { files_autosave_delay_ms: 1, ..Default::default() }
                .clamped()
                .files_autosave_delay_ms,
            FILES_AUTOSAVE_DELAY_MIN_MS
        );
        assert_eq!(
            UiSettings { code_font_size: 100.0, ..Default::default() }.clamped().code_font_size,
            crate::typography::FONT_SIZE_MAX
        );
        assert_eq!(
            UiSettings { terminal_font_size: 1.0, ..Default::default() }
                .clamped()
                .terminal_font_size,
            crate::typography::FONT_SIZE_MIN
        );
        assert_eq!(
            UiSettings { code_font_size: f32::NAN, ..Default::default() }.clamped().code_font_size,
            crate::typography::CODE_FONT_SIZE_DEFAULT
        );
    }

    #[test]
    fn legacy_files_editor_font_size_becomes_the_code_font_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(UiSettings::path(dir.path()), r#"{"filesEditorFontSize": 15.0}"#).unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.code_font_size, 15.0);
        assert_eq!(loaded.terminal_font_size, crate::typography::TERMINAL_FONT_SIZE_DEFAULT);
    }

    #[test]
    fn large_right_pane_width_is_preserved() {
        let loaded = UiSettings { right_pane_width: 2400.0, ..Default::default() }.clamped();
        assert_eq!(loaded.right_pane_width, 2400.0);
    }

    #[test]
    fn nan_heals_to_default() {
        let healed = UiSettings { sidebar_width: f32::NAN, ..Default::default() }.clamped();
        assert_eq!(healed.sidebar_width, SIDEBAR_DEFAULT);
    }

    #[test]
    fn defaults_match_slopty() {
        let d = UiSettings::default();
        assert_eq!(d.sidebar_width, 256.0);
        assert_eq!(d.right_pane_width, 520.0);
        assert_eq!(d.terminal_height, 280.0);
        assert!(!d.sidebar_collapsed);
    }

    // Settings a pre-release build stopped keeping are deleted outright; a file that
    // still holds them loads as if they were not there, its other choices kept.
    #[test]
    fn a_file_holding_deleted_settings_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{
                "sidebarWidth": 300,
                "tabOrder": {"space-1": ["b", "a"]},
                "spaceOrder": ["space-2"],
                "rightPaneOpen": true,
                "terminalOpen": true,
                "skillsInSlashMenu": true,
                "accentColor": "cyan",
                "pullRequestPaneWidth": 820,
                "settingsSection": "agents",
                "sidebarOrganization": "byProject",
                "sidebarCompact": false,
                "sidebarShowProjectLabel": false,
                "sidebarShowProjectIcon": false,
                "sidebarShowHarness": false,
                "sidebarShowBranch": false,
                "sidebarShowPullRequest": false,
                "sidebarShowProjectFilter": false,
                "sidebarShowArchived": false,
                "sidebarSectionsByProfile": {"local": [{"id": "a", "name": "Focus", "sessionIds": ["c"], "collapsed": false}]},
                "sidebarPinnedSessionIdsByProfile": {"local": ["c"]},
                "compactModelPicker": false,
                "skillCompletionByHarness": {"claudeCode": {"dollar": false, "separateFromSlash": false}},
                "reduceMotion": "on",
                "transcriptWidth": 960,
                "wallpaperFolder": "/wallpapers",
                "wallpaperSource": "/wallpapers/a.png",
                "wallpaperHistory": ["/wallpapers/a.png"],
                "newThreadBackgroundEffect": "ascii",
                "newThreadComposerBackground": {"path": "/tmp/a.png", "name": "a.png"},
                "wallpaperThemeColors": true,
                "wallpaperColor": [51, 102, 153],
                "shortcuts": {"randomWallpaper": "mod-shift-u", "saveFile": "mod-alt-s"}
            }"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, 300.0);
        assert_eq!(loaded.settings_section, crate::shell::SettingsSection::Appearance);
        assert!(!backup_path(dir.path()).exists(), "nothing was set aside");
        assert_eq!(loaded.keymap.save_file, "mod-alt-s", "the keymap's live combos kept");
        loaded.save(dir.path()).unwrap();
        let saved = std::fs::read_to_string(UiSettings::path(dir.path())).unwrap();
        for gone in [
            "tabOrder",
            "spaceOrder",
            "rightPaneOpen",
            "skillsInSlashMenu",
            "accentColor",
            "sidebarOrganization",
            "sidebarCompact",
            "sidebarShow",
            "sidebarSections",
            "compactModelPicker",
            "skillCompletion",
            "reduceMotion",
            "transcriptWidth",
            "wallpaperFolder",
            "wallpaperSource",
            "wallpaperHistory",
            "newThreadBackgroundEffect",
            "newThreadComposerBackground",
            "wallpaperThemeColors",
            "wallpaperColor",
            "randomWallpaper",
        ] {
            assert!(!saved.contains(gone), "{gone} is not written back");
        }
    }

    /// The file keeps only what the person changed, so a default that moves reaches
    /// every shortcut left alone.
    #[test]
    fn the_file_keeps_only_the_combos_the_person_changed() {
        let stored = |keymap: &KeymapConfig| serde_json::to_value(keymap).unwrap();
        let mut keymap = KeymapConfig::default();
        assert_eq!(stored(&keymap), serde_json::json!({}), "nothing changed, nothing kept");
        keymap.set(ShortcutId::NewProject, "mod-alt-p".into());
        keymap.set(ShortcutId::JumpSession(2), "mod-alt-3".into());
        keymap.set(ShortcutId::ArchiveSession, String::new());
        assert_eq!(
            stored(&keymap),
            serde_json::json!({"newProject": "mod-alt-p", "jumpSession3": "mod-alt-3", "archiveSession": ""}),
            "a change, a slot counted from 1, and an unbound row"
        );
        let restored: KeymapConfig = serde_json::from_value(stored(&keymap)).unwrap();
        assert_eq!(restored, keymap, "it reads back as written");
        keymap.reset(ShortcutId::NewProject);
        keymap.reset(ShortcutId::JumpSession(2));
        keymap.reset(ShortcutId::ArchiveSession);
        assert_eq!(stored(&keymap), serde_json::json!({}), "a reset row leaves the file");
        // Every shortcut has its own name there.
        let names: std::collections::HashSet<_> =
            ShortcutId::ALL.into_iter().map(ShortcutId::stored_key).collect();
        assert_eq!(names.len(), ShortcutId::ALL.len());
    }

    #[test]
    fn keymap_defaults_and_reset() {
        let mut keymap = KeymapConfig::default();
        assert_eq!(keymap.get(ShortcutId::SaveFile), "mod-s");
        keymap.set(ShortcutId::SaveFile, "mod-shift-s".into());
        keymap.reset(ShortcutId::SaveFile);
        assert_eq!(keymap.get(ShortcutId::SaveFile), "mod-s");
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-b");
        assert_eq!(keymap.get(ShortcutId::ToggleChanges), "mod-r");
        assert_eq!(keymap.get(ShortcutId::NextNeedsYou), "mod-j");
        assert_eq!(keymap.get(ShortcutId::Inbox), "mod-shift-j");
        let ctrl = if crate::keys::MOD_IS_COMMAND { "ctrl" } else { "mod" };
        assert_eq!(keymap.get(ShortcutId::NextSession), format!("{ctrl}-tab"));
        assert_eq!(keymap.get(ShortcutId::PrevSession), format!("{ctrl}-shift-tab"));
        assert_eq!(keymap.get(ShortcutId::NewSession), "mod-n");
        assert_eq!(keymap.get(ShortcutId::ArchiveSession), "mod-shift-a");
        keymap.set(ShortcutId::ToggleSidebar, "mod-shift-x".into());
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
        keymap.reset(ShortcutId::ToggleSidebar);
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-b");
        keymap.set(ShortcutId::ArchiveSession, "mod-shift-y".into());
        assert_eq!(keymap.get(ShortcutId::ArchiveSession), "mod-shift-y");
        keymap.reset(ShortcutId::ArchiveSession);
        assert_eq!(keymap.get(ShortcutId::ArchiveSession), "mod-shift-a");
    }

    #[test]
    fn every_shortcut_default_is_unique_and_bindable() {
        // A new shortcut must not ship in conflict with an existing one, and
        // its default must parse on this platform.
        assert!(conflicted_shortcuts(&KeymapConfig::default()).is_empty());
        for id in ShortcutId::ALL {
            assert!(
                parse_combo(id.default_combo()).is_ok(),
                "{:?} default combo does not parse",
                id
            );
        }
    }

    #[test]
    fn combo_recording() {
        // How this platform spells a recorded ctrl (see `combo_from_keystroke_on`).
        let ctrl_combo = |suffix: &str| {
            if crate::keys::MOD_IS_COMMAND {
                format!("ctrl-{suffix}")
            } else {
                format!("mod-{suffix}")
            }
        };
        assert_eq!(combo_from_keystroke(true, false, false, false, "s"), Some(ctrl_combo("s")));
        assert_eq!(combo_from_keystroke(false, false, false, true, "s"), Some("mod-s".into()));
        assert_eq!(
            combo_from_keystroke(true, false, true, false, "tab"),
            Some(ctrl_combo("shift-tab"))
        );
        assert_eq!(
            combo_from_keystroke(true, true, true, false, "K"),
            Some(ctrl_combo("alt-shift-k"))
        );
        // Plain keys record without modifiers (Esc is filtered by the caller).
        assert_eq!(combo_from_keystroke(false, false, false, false, "f5"), Some("f5".into()));
        // Bare modifier presses record nothing.
        assert_eq!(combo_from_keystroke(true, false, false, false, "ctrl"), None);
        assert_eq!(combo_from_keystroke(false, false, true, false, "shift"), None);
        assert_eq!(combo_from_keystroke(false, false, false, false, ""), None);
    }

    #[test]
    fn every_default_is_spelled_the_way_the_recorder_spells_it() {
        // The invariant `default_combo_on` documents. Checked for BOTH
        // platforms because the hazard only exists off macOS, so a single-OS
        // CI run would never see it.
        for mac in [true, false] {
            for id in ShortcutId::ALL {
                let combo = id.default_combo_on(mac);
                // Via the platform spelling, where modifier names are
                // unambiguous, so the decode can't inherit the bug it checks.
                let bound = platform_combo_on(mac, combo);
                let mut parts: Vec<&str> = bound.split('-').collect();
                let key = parts.pop().expect("a combo always ends in a key");
                let recorded = combo_from_keystroke_on(
                    mac,
                    parts.contains(&"ctrl"),
                    parts.contains(&"alt"),
                    parts.contains(&"shift"),
                    parts.contains(&"cmd"),
                    key,
                );
                assert_eq!(
                    recorded.as_deref(),
                    Some(combo),
                    "{} default {combo:?} is unreachable from the recorder (mac={mac})",
                    id.label()
                );
            }
        }
    }

    #[test]
    fn defaults_are_distinct_physical_keys() {
        // Distinct STRINGS is not enough — two defaults could still resolve to
        // the same keystroke through `platform_combo`.
        let mut seen = std::collections::HashSet::new();
        for id in ShortcutId::ALL {
            let bound = platform_combo(id.default_combo());
            assert!(seen.insert(bound.clone()), "{bound:?} bound twice");
        }
    }

    #[test]
    fn conflict_detection() {
        let mut keymap = KeymapConfig::default();
        assert!(conflicted_shortcuts(&keymap).is_empty());
        keymap.set(ShortcutId::ToggleChanges, "mod-b".into());
        let conflicts = conflicted_shortcuts(&keymap);
        assert!(conflicts.contains(&ShortcutId::ToggleSidebar));
        assert!(conflicts.contains(&ShortcutId::ToggleChanges));
        assert!(!conflicts.contains(&ShortcutId::Inbox));
        keymap.reset(ShortcutId::ToggleChanges);
        assert!(conflicted_shortcuts(&keymap).is_empty());
    }

    #[test]
    fn combo_translation() {
        let primary = if crate::keys::MOD_IS_COMMAND { "cmd" } else { "ctrl" };
        assert_eq!(platform_combo("mod-s"), format!("{primary}-s"));
        assert_eq!(platform_combo("alt-f4"), "alt-f4");
        // Literal ctrl passes through untouched — the macOS spelling of
        // session cycling.
        assert_eq!(platform_combo("ctrl-shift-tab"), "ctrl-shift-tab");
    }

    #[test]
    fn a_file_without_shortcuts_takes_every_default() {
        // The old `keymap`, which froze every default it was saved with, is not read.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "keymap": {"nextNeedsYou": "mod-.", "toggleTerminal": "mod-j"}}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, 300.0);
        assert_eq!(loaded.keymap, KeymapConfig::default());
        assert_eq!(loaded.keymap.get(ShortcutId::NextNeedsYou), "mod-j");
    }

    #[test]
    fn jump_slots_get_set_and_reset() {
        let mut keymap = KeymapConfig::default();
        assert_eq!(keymap.get(ShortcutId::JumpSession(0)), "mod-1");
        assert_eq!(keymap.get(ShortcutId::JumpSession(8)), "mod-9");
        // Past the last slot there is no shortcut, not a panic.
        assert_eq!(keymap.get(ShortcutId::JumpSession(9)), "");
        assert_eq!(ShortcutId::JumpSession(9).jump_slot(), None);
        assert_eq!(ShortcutId::JumpSession(0).jump_slot(), Some(0));
        assert_eq!(ShortcutId::ArchiveSession.jump_slot(), None);

        keymap.set(ShortcutId::JumpSession(2), "mod-alt-3".into());
        assert_eq!(keymap.get(ShortcutId::JumpSession(2)), "mod-alt-3");
        keymap.reset(ShortcutId::JumpSession(2));
        assert_eq!(keymap.get(ShortcutId::JumpSession(2)), "mod-3");
        // A write past the last slot is dropped, and grows nothing.
        keymap.set(ShortcutId::JumpSession(9), "mod-0".into());
        assert_eq!(keymap.jump_session.len(), JUMP_SLOTS);
    }

    #[test]
    fn short_or_long_jump_lists_heal_to_the_slot_count() {
        // Short: surviving entries keep their slot, the rest take defaults.
        let mut keymap =
            KeymapConfig { jump_session: vec!["mod-alt-1".into()], ..KeymapConfig::default() };
        keymap.heal_jump_slots();
        assert_eq!(keymap.jump_session.len(), JUMP_SLOTS);
        assert_eq!(keymap.get(ShortcutId::JumpSession(0)), "mod-alt-1");
        assert_eq!(keymap.get(ShortcutId::JumpSession(1)), "mod-2");

        // Long: the tail is dropped.
        let mut keymap = KeymapConfig {
            jump_session: (0..20).map(|i| format!("mod-{i}")).collect(),
            ..KeymapConfig::default()
        };
        keymap.heal_jump_slots();
        assert_eq!(keymap.jump_session.len(), JUMP_SLOTS);

        // Absent: the whole list comes back.
        let mut keymap = KeymapConfig { jump_session: Vec::new(), ..KeymapConfig::default() };
        keymap.heal_jump_slots();
        assert_eq!(keymap.jump_session, KeymapConfig::default().jump_session);
    }

    #[test]
    fn a_malformed_shortcut_is_skipped_without_losing_other_settings() {
        // An odd entry must not cost the person their sidebar width or their other combos.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"sidebarWidth": 300, "shortcuts": {"toggleSidebar": "mod-shift-x", "jumpSession1": "mod-alt-1", "jumpSession2": 7, "jumpSession10": "mod-0"}}"#,
        )
        .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.sidebar_width, 300.0);
        assert_eq!(loaded.keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
        assert_eq!(loaded.keymap.get(ShortcutId::JumpSession(0)), "mod-alt-1");
        assert_eq!(
            loaded.keymap.get(ShortcutId::JumpSession(1)),
            "mod-2",
            "not a string: the default"
        );
        assert_eq!(loaded.keymap.get(ShortcutId::JumpSession(8)), "mod-9");
        assert_eq!(loaded.keymap.jump_session.len(), JUMP_SLOTS, "no tenth slot");
    }

    #[test]
    fn jump_hints_need_an_exact_modifier_match() {
        let keymap = KeymapConfig::default();
        // Mod alone matches mod-1..9.
        assert!(jump_hints_visible(&keymap, true, false, false));
        // Extra modifiers are a different chord (Cmd+Shift+4 screenshots).
        assert!(!jump_hints_visible(&keymap, true, false, true));
        assert!(!jump_hints_visible(&keymap, true, true, false));
        // Nothing held, nothing shown.
        assert!(!jump_hints_visible(&keymap, false, false, false));
        // Alt alone is not a jump modifier by default.
        assert!(!jump_hints_visible(&keymap, false, true, false));

        // Rebinding moves the trigger with it.
        let mut rebound = KeymapConfig::default();
        for slot in 0..JUMP_SLOTS {
            rebound.set(ShortcutId::JumpSession(slot), format!("mod-alt-{slot}"));
        }
        assert!(!jump_hints_visible(&rebound, true, false, false));
        assert!(jump_hints_visible(&rebound, true, true, false));

        // A jump combo with no modifiers must not pin the overlay open.
        let mut bare = KeymapConfig::default();
        bare.set(ShortcutId::JumpSession(0), "f5".into());
        assert!(!jump_hints_visible(&bare, false, false, false));
    }

    #[test]
    fn azerty_localizes_shortcuts_where_appkit_puts_them() {
        let azerty = azerty_key_equivalents();
        // The relocations an AZERTY Mac's own menus apply to these shortcuts.
        for (us, here) in [
            ("mod-1", "mod-&"),
            ("mod-2", "mod-é"),
            ("mod-/", "mod-:"),
            ("mod-[", "mod-^"),
            ("mod-]", "mod-$"),
        ] {
            assert_eq!(localize_combo(us, &azerty), here);
        }
        // Letters, named keys and keys AZERTY keeps in place stay put.
        for combo in ["mod-b", "mod-shift-a", "ctrl-tab", "mod-enter", "f5", "mod-,", "mod--"] {
            assert_eq!(localize_combo(combo, &azerty), combo);
        }
        // So does every key on a US layout, and the bindable spelling
        // localizes like the stored one.
        assert_eq!(localize_combo("mod-1", &KeyEquivalents::default()), "mod-1");
        assert_eq!(localize_combo("cmd-shift-/", &azerty), "cmd-shift-:");
    }

    #[test]
    fn canonical_keys_invert_localization_on_every_reachable_key() {
        let azerty = azerty_key_equivalents();
        for here in azerty.values().copied().chain("abz,-=+?".chars()) {
            let here = here.to_string();
            let us = canonical_key(&here, &azerty).unwrap_or_else(|| panic!("{here} unreachable"));
            assert_eq!(localize_key(&us, &azerty), here.as_str(), "{here} via {us}");
        }
        assert_eq!(canonical_key("tab", &azerty).as_deref(), Some("tab"));
        assert_eq!(canonical_combo("mod-alt-&", &azerty).as_deref(), Some("mod-alt-1"));
        // AZERTY relocates @ and nothing lands on that key.
        assert_eq!(canonical_key("@", &azerty), None);
        // Two US keys sharing one target (Icelandic ] and }) take the lower.
        let icelandic: KeyEquivalents = [(']', '´'), ('}', '´')].into_iter().collect();
        assert_eq!(canonical_key("´", &icelandic).as_deref(), Some("]"));
    }

    #[test]
    fn combo_modifiers_reads_the_stored_form() {
        assert_eq!(combo_modifiers("mod-1"), (true, false, false));
        assert_eq!(combo_modifiers("mod-alt-shift-k"), (true, true, true));
        assert_eq!(combo_modifiers("f5"), (false, false, false));
        assert_eq!(combo_modifiers("shift-tab"), (false, false, true));
    }

    #[test]
    fn a_default_yields_to_a_chord_the_person_gave_another_action() {
        // ⌘J is the jump's; a person who had put it on another action keeps it there
        // and the jump arrives unbound, rather than one chord binding twice.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(UiSettings::path(dir.path()), r#"{"shortcuts": {"saveFile": "mod-j"}}"#)
            .unwrap();
        let loaded = UiSettings::load(dir.path());
        assert_eq!(loaded.keymap.get(ShortcutId::SaveFile), "mod-j");
        assert_eq!(loaded.keymap.get(ShortcutId::NextNeedsYou), "");
        assert!(conflicted_shortcuts(&loaded.keymap).is_empty());
        loaded.save(dir.path()).unwrap();
        assert_eq!(
            UiSettings::load(dir.path()).keymap.get(ShortcutId::NextNeedsYou),
            "",
            "it stays unbound once saved"
        );

        // With the chord free, the shortcut follows its default, and the rest keep theirs.
        std::fs::write(
            UiSettings::path(dir.path()),
            r#"{"shortcuts": {"toggleSidebar": "mod-shift-x"}}"#,
        )
        .unwrap();
        let keymap = UiSettings::load(dir.path()).keymap;
        assert_eq!(keymap.get(ShortcutId::ToggleSidebar), "mod-shift-x");
        assert_eq!(keymap.get(ShortcutId::NextNeedsYou), "mod-j");
        assert_eq!(keymap.get(ShortcutId::NextSession), ShortcutId::NextSession.default_combo());
    }

    #[test]
    fn terminal_height_clamps_on_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(UiSettings::path(dir.path()), r#"{"terminalHeight": 5}"#).unwrap();
        assert_eq!(UiSettings::load(dir.path()).terminal_height, TERMINAL_MIN_HEIGHT);
        std::fs::write(UiSettings::path(dir.path()), r#"{"terminalHeight": 99999}"#).unwrap();
        assert_eq!(UiSettings::load(dir.path()).terminal_height, TERMINAL_ABS_MAX_HEIGHT);
    }
}
