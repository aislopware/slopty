// Derived from zeron (MIT, github.com/zeronsh/zeron @ 77a3c0f), see THIRD_PARTY_NOTICES.md
//! Spaces sidebar: the space-filter dropdown (searchable, with "All projects"),
//! the filtered Sessions list, and the add-space palette (device
//! tabs + filtered folder browser).
//!
//! A space = a synced (device, folder) pair. Spaces stopped being a
//! navigation spine when tabs went device-local: the dropdown only FILTERS
//! the sidebar's session list (never the tab strip) and hosts space
//! management (add via the palette; rename/delete via row context menus).
//! Child module of `shell` so it renders straight off `Shell`'s private state.

use super::*;
use super::sidebar::SIDEBAR_ROW_HEIGHT;
use crate::pickers::{
    breadcrumbs, browser_rows, completion_prefix_len, escape_typed_path, folder_path_key,
    is_windows_path, parent_path, resolve_typed_path, same_folder_path, split_typed_leaf,
    unescape_typed_path,
};
use gpui::{FocusHandle, Window};
use std::collections::HashSet;
use crate::state::time_ago_changes;
use slopty_view_proto::{
    Device, DriveEntry, DriveListing, FolderListing, HomeFolderMatch,
    HomeFolderSearchResult, SearchHomeFoldersRequest, Space,
};

// The filter is let go only while every machine is reached: a project on one out of reach is
// missing from the lists, not gone.
/// Whether every device the workspace knows is reached now, so what the lists leave out is gone.
pub(super) fn every_device_reached(state: &AppState, now: chrono::DateTime<chrono::Utc>) -> bool {
    !state.devices.is_empty()
        && state.devices.iter().all(|device| !super::sidebar_status::out_of_reach(state, &device.id, now))
}

#[cfg(test)]
mod sidebar_row_tests {
    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[gpui::test]
    fn archive_pill_follows_a_still_pointer_down_the_list(cx: &mut gpui::TestAppContext) {
        use super::*;

        struct SidebarHost(Entity<Shell>);
        impl Render for SidebarHost {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.0.update(cx, |shell, cx| {
                    div()
                        .w(px(280.0))
                        .h(px(800.0))
                        .child(shell.render_chat_sidebar(&Theme::default(), cx))
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            // slopty: no History surface (ruled out).
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            SidebarHost(cx.new(|cx| {
                let state = cx.new(|_| AppState::new());
                let shell = Shell::new(
                    state,
                    EngineBootConfig {
                        data_dir: dir.path().into(),
                        // slopty: Slopty's server is the only backend.
                        default_harness: slopty_view_proto::HarnessId::MOCK,
                    },
                    cx,
                );
                crate::motion::set_reduced_motion(cx, true);
                shell.state.update(cx, |state, _| {
                    state.workspace_scope = Some(WorkspaceScope::Local);
                    state.local_device_id = Some("local".into());
                    state.chats = ["a", "b", "c"]
                        .into_iter()
                        .enumerate()
                        .map(|(ix, id)| {
                            serde_json::from_value(serde_json::json!({
                                "id": id, "title": id, "deviceId": "local", "archived": false,
                                "createdAt": Utc::now() - chrono::Duration::minutes(ix as i64),
                            }))
                            .unwrap()
                        })
                        .collect();
                });
                shell
            }))
        });
        // slopty: the hover fades run on the test's clock, which the test advances.
        crate::motion::run_on(cx.executor());
        let shell = host.read_with(cx, |host, _| host.0.clone());
        let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert_eq!(order, ids(&["a", "b", "c"]));
        let state = shell.read_with(cx, |shell, _| shell.state.clone());

        // slopty: the archive pill stands at the row's end while the row is under the pointer.
        let row = cx.debug_bounds("chat-a").unwrap();
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        cx.run_until_parked();
        let pointer = cx.debug_bounds("chat-archive-a").unwrap().center();
        cx.simulate_mouse_move(pointer, None, gpui::Modifiers::default());
        for (archived, next) in [
            ("a", Some(("b", "chat-archive-b"))),
            ("b", Some(("c", "chat-archive-c"))),
            ("c", None),
        ] {
            cx.simulate_click(pointer, gpui::Modifiers::default());
            shell.read_with(cx, |shell, _| {
                assert!(shell.chat_hover_resync);
                assert_eq!(
                    shell.chat_status_hover.as_deref(),
                    Some(format!("chat-{archived}").as_str())
                );
            });
            // The engine round-trip lands; the next row slides up under the
            // unmoved pointer.
            state.update(cx, |state, cx| {
                state
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == archived)
                    .unwrap()
                    .archived = true;
                cx.notify();
            });
            cx.run_until_parked();
            shell.update(cx, |shell, cx| {
                shell.sidebar_resort.clear();
                shell.sidebar_new_keys.clear();
                cx.notify();
            });
            cx.run_until_parked();
            // slopty: the row's fade out, run to its end on the test's clock.
            cx.executor().advance_clock(crate::motion::HOVER_FADE.total().mul_f32(crate::motion::speed_scale()));
            cx.run_until_parked();
            // Its Archived-section copy shares the row key and must not stay lit. slopty: the
            // row's fade is in its last frame here, not lit.
            let lit = crate::motion::hover_t(&format!("chat-{archived}-hover"));
            assert!(lit < 0.01, "chat-{archived} still lit at {lit}");
            let expected = next.map(|(next, _)| format!("chat-{next}"));
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.chat_status_hover, expected);
            });
            if let Some((_, corner)) = next {
                assert!(cx.debug_bounds(corner).unwrap().contains(&pointer));
            }
        }

        // Real pointer movement hands hover back to gpui's own tracking.
        let row = cx.debug_bounds("chat-c").unwrap();
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        shell.read_with(cx, |shell, _| assert!(!shell.chat_hover_resync));
    }

    // The filter outlives a machine out of reach.
    #[test]
    fn the_filter_waits_while_a_device_is_out_of_reach() {
        let now = chrono::Utc::now();
        let device = |id: &str, seen: Option<chrono::DateTime<chrono::Utc>>| -> super::Device {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "platform": "macos", "lastSeenAt": seen,
            }))
            .unwrap()
        };
        let mut state = super::AppState::new();
        assert!(!super::every_device_reached(&state, now), "no devices said yet");
        state.devices = vec![device("mac", Some(now)), device("vps", None)];
        assert!(!super::every_device_reached(&state, now), "the vps is out of reach");
        state.devices = vec![device("mac", Some(now)), device("vps", Some(now))];
        assert!(super::every_device_reached(&state, now));
    }
}

// slopty: the sidebar's list is one keyboard stop, as Claude desktop's session list: Tab reaches
// the open chat's row (or the first, with none open), Up and Down walk the rows in the order
// they draw, Enter opens one and Delete archives it.
/// The sidebar's rows as keyboard stops: each row's focus, and the row Tab reaches.
#[derive(Default)]
pub(super) struct SidebarKeys {
    focus: std::cell::RefCell<std::collections::HashMap<String, FocusHandle>>,
    stop: Option<String>,
}

impl SidebarKeys {
    /// The focus of `chat`'s row, made on first ask.
    pub(super) fn focus(&self, chat: &str, cx: &mut App) -> FocusHandle {
        self.focus.borrow_mut().entry(chat.to_owned()).or_insert_with(|| cx.focus_handle()).clone()
    }

    /// Whether Tab reaches `chat`'s row.
    pub(super) fn stops_at(&self, chat: &str) -> bool {
        self.stop.as_deref() == Some(chat)
    }

    /// The row focus of a chat drawn, if it was.
    fn drawn(&self, chat: &str) -> Option<FocusHandle> {
        self.focus.borrow().get(chat).cloned()
    }
}

// slopty: the Archived shelf lists its rows newest first; the live list keeps the order its
// sessions started in (`sidebar_status::list`).
pub(super) fn compare_sidebar_chats(
    left: &slopty_view_proto::Chat,
    right: &slopty_view_proto::Chat,
) -> std::cmp::Ordering {
    right.created_at.cmp(&left.created_at).then_with(|| left.id.cmp(&right.id))
}

/// The space-filter dropdown, `Some` while open. The same searchable-menu
/// recipe as the composer's ref picker: filter input on top
/// (`PaletteSearch` context so ↑↓/⏎ bubble to the card), ranked substring
/// rows, keyboard highlight.
pub(super) struct SpacesMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight — an index into [`Shell::spaces_menu_rows`], or
    /// that list's length when the pinned "New project" footer holds it.
    active: usize,
    /// Tracked on the card — puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    _search_events: Subscription,
}

// list items stay tightly related at 2px, while section boundaries use 12px
// (well over 2x the intra-list gap). Disclosure content gets a small 4px
// handoff from its header without leaving dead space while collapsed.
const SIDEBAR_SECTION_GAP: f32 = 12.0;
pub(super) const SIDEBAR_DISCLOSURE_HEADER_HEIGHT: f32 = 28.0;
/// slopty: a group heading's words and count: 12 medium (Claude desktop's compact heading),
/// a step under the rows' 13 regular titles, so a heading reads as the list's label and not as
/// one more row.
pub(super) const SIDEBAR_HEADER_TEXT: f32 = 12.0;
/// slopty: a heading's chevron turns over Amp's 200 ms ease-out.
const SIDEBAR_CHEVRON_TURN: motion::MotionSpec = motion::MotionSpec::new(200, motion::EASE_OUT);
pub(super) const SIDEBAR_DISCLOSURE_BODY_INSET: f32 = 4.0;
const SIDEBAR_DISCLOSURE_SECTION_HEIGHT: f32 =
    SIDEBAR_SECTION_GAP + SIDEBAR_DISCLOSURE_HEADER_HEIGHT;
/// The Archived shelf shows this many rows, then "Show more" pages by
/// [`ARCHIVED_PAGE_ROWS`].
const ARCHIVED_INITIAL_ROWS: usize = 10;
const ARCHIVED_PAGE_ROWS: usize = 25;

/// Shared quiet rule for sidebar groups and palette sections.
pub(super) fn sidebar_separator(theme: &Theme) -> gpui::Div {
    div().h(px(1.0)).bg(theme.border.opacity(0.6))
}

/// `icon` leads the label — a project group's icon, which its rows then omit.
/// Every sidebar section's header (Sessions, Archived, project groups and
/// custom sections): a muted label, `count` as a muted badge
/// beside it while the section is collapsed, then any action and the
/// disclosure chevron.
// slopty: the label reads at the full muted tone, 12 medium as Claude desktop's compact group
// headings, and brightens to the text's under the pointer over the house's hover fade, as
// Claude desktop's do; the group's count fades in beside it then (Amp), and the chevron follows
// the words: an open group's shows only under the pointer (Claude desktop's caret), a shut
// group's always, since its rows are not there to say it holds any. So the header rests quiet.
// The header is a button a screen reader names, with its count, and hears open or shut, and a
// keyboard stop Enter or Space toggles. `id` is its element's; `key` names its fade among the
// sidebar's; `hovered` is the caller's own hover listener, which the header calls beside its
// fade (an element takes one).
#[allow(clippy::too_many_arguments)]
pub(super) fn sidebar_disclosure_header(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    key: &str,
    icon: Option<AnyElement>,
    label: SharedString,
    count: Option<usize>,
    action: Option<AnyElement>,
    chevron: AnyElement,
    open: bool,
    hovered: Option<Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>>,
) -> gpui::Stateful<gpui::Div> {
    let fade_key = SharedString::from(format!("sidebar-header-{key}"));
    let fade = motion::hover_listener(fade_key.clone());
    let hover = move |is: &bool, window: &mut Window, cx: &mut App| {
        fade(is, window, cx);
        if let Some(hovered) = &hovered {
            hovered(is, window, cx);
        }
    };
    let header = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(SIDEBAR_DISCLOSURE_HEADER_HEIGHT))
        .px(px(Theme::SPACE_SM))
        .rounded(px(8.0))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(match count {
            Some(count) => SharedString::from(format!("{label}, {count}")),
            None => label.clone(),
        })
        .aria_expanded(open)
        .children(icon)
        .child(super::sidebar::sidebar_faded_label(
            "sidebar-disclosure-label".into(),
            false,
            div()
                .text_size(crate::typography::ui_rems(SIDEBAR_HEADER_TEXT))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(motion::hover_blend(&fade_key, theme.text_muted, theme.text))
                .child(label),
        ))
        .children(count.map(|count| {
            let key = key.to_owned();
            div()
                .debug_selector(move || format!("sidebar-header-count-{key}"))
                .flex_none()
                .text_size(crate::typography::ui_rems(SIDEBAR_HEADER_TEXT))
                .font_features(crate::typography::tabular())
                .text_color(motion::hover_blend(
                    &fade_key,
                    theme.text_muted.opacity(0.0),
                    theme.text_muted,
                ))
                .child(SharedString::from(count.to_string()))
        }))
        .child({
            let shown = if open { sidebar_hover_reveal(&fade_key) } else { 1.0 };
            let key = key.to_owned();
            div()
                .debug_selector(move || format!("sidebar-header-chevron-{key}"))
                .flex_none()
                .opacity(shown)
                .child(chevron)
        })
        .child(div().flex_1())
        .children(action)
        .child(motion::fade_driver(fade_key))
        .on_hover(hover);
    crate::surface_chrome::focus_ring_inset(header.tab_index(0), theme)
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
}

/// slopty: what a shut group's header wears at its end: how many rows it holds, and its loudest
/// row's mark (`marks::loudest`), so "which one needs me" has an answer with the group shut.
fn group_rollup(key: &str, count: usize, loudest: Option<&super::marks::Mark>, theme: &Theme) -> AnyElement {
    let count_id = format!("sidebar-group-count-{key}");
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .debug_selector(move || count_id)
                .text_size(crate::typography::ui_rems(SIDEBAR_HEADER_TEXT))
                .font_features(crate::typography::tabular())
                .text_color(theme.text_muted)
                .child(SharedString::from(count.to_string())),
        )
        .children(loudest.and_then(|mark| {
            super::marks::mark_element(
                SharedString::from(format!("sidebar-group-mark-{key}")),
                format!("sidebar-group-mark-{key}"),
                mark,
                crate::loaders::status_mark::MARK_SMALL,
                theme.bg,
            )
        }))
        .into_any_element()
}

/// slopty: how far a sidebar fade `key` reveals what it shows only under the pointer: wholly on
/// a touch screen, where nothing hovers.
pub(super) fn sidebar_hover_reveal(key: &str) -> f32 {
    if super::compact::touch_screen() {
        return 1.0;
    }
    motion::hover_t(key)
}

/// Section header glyphs (chevron, `+`, `…`) share the label's muted tone.
// slopty: a step under the label, always shown (zeron's), where half the muted tone read as
// absent beside the label's.
pub(super) fn sidebar_header_glyph(theme: &Theme) -> gpui::Hsla {
    theme.text_muted.opacity(0.7)
}

/// slopty: one row of the sidebar's filter menu as it draws.
#[derive(Default)]
struct MenuDetail {
    row: SpacesMenuRow,
    label: SharedString,
    /// A project's row: its project, whose artwork it draws.
    space: Option<slopty_view_proto::Space>,
    /// Out of reach: the disconnected glyph.
    offline: bool,
    selected: bool,
    /// A machine's row: its kind's glyph in the artwork's place (`device_glyph`).
    glyph: Option<&'static str>,
    /// A machine that turns this device away: why, for its login badge.
    turned_away: Option<SharedString>,
}

/// One activatable row of the open dropdown, in nav order. `AddSpace` names
/// the card's pinned "New project" footer, not a list row — keyboard nav
/// maps the list-length index to it.
#[derive(Clone, PartialEq, Default)]
pub(super) enum SpacesMenuRow {
    #[default]
    All,
    Space(String),
    // slopty: a machine, by device id: the list narrowed to its sessions (`sidebar_status::Filter`).
    Machine(String),
    AddSpace,
}

/// New project navigates devices, locations, then folders on a command-palette surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectStep {
    Devices,
    Locations,
    Folders,
}

pub(super) struct AddSpaceFlow {
    step: ProjectStep,
    location: Option<(String, Option<String>)>,
    /// The selected device.
    device: Option<Device>,
    /// Filter input; Enter descends into the highlighted folder. Carries the
    /// tab-completion ghost (the faint suffix ⇥ accepts), and a trailing `/`
    /// on a folder-naming query descends immediately.
    search: Entity<ComposerInput>,
    browser: Loadable<FolderListing>,
    /// The selected device's mounted drives/volumes.
    /// Best-effort: an error just leaves the section at Home only.
    drives: Loadable<Vec<DriveEntry>>,
    /// Requested browser path (`None` = the device's default, i.e. home).
    browser_path: Option<String>,
    /// The device's home (the path a `None` browse resolved to) — breadcrumbs
    /// fold everything up to here into the Home crumb.
    home: Option<String>,
    /// Best-effort git seed for the CURRENT browser path (known when we
    /// descended through an entry whose `is_repo` we saw; the owning device's
    /// SpacesSync re-verifies either way).
    browser_repo: bool,
    /// Keyboard highlight within the current step’s filtered rows.
    active: usize,
    submit_busy: bool,
    error: Option<SharedString>,
    /// Tracked on the card (`track_focus`) — puts the card on the keyboard
    /// dispatch path so ↑↓/⌫/esc reach `add_space_key` while the search input
    /// holds focus (the structure every working picker uses).
    focus: FocusHandle,
    /// Folder-list scroll — keyboard navigation keeps the highlighted row in
    /// view (`scroll_to_item`).
    list_scroll: gpui::ScrollHandle,
    /// Horizontal breadcrumb strip; `crumb_key` is the path it last revealed.
    crumb_scroll: gpui::ScrollHandle,
    crumb_key: String,
    focus_pending: bool,
    /// slopty: where the keyboard was before the picker took it, given back when it closes.
    return_focus: Option<FocusHandle>,
    load_task: Option<Task<()>>,
    drives_task: Option<Task<()>>,
    submit_task: Option<Task<()>>,
    /// Folders matching the query anywhere under the device's home (Home
    /// location only).
    home_search: HomeFolderSearch,
    // slopty: the search field takes a repository's address, to clone into the open folder.
    cloning: bool,
    /// The crumb row as a typed path; `None` shows the crumbs.
    path_edit: Option<PathEdit>,
    _search_events: Subscription,
}

impl AddSpaceFlow {
    /// The folder the trail shows as open — while one loads, or after it
    /// failed, the path it asked for; home on the Locations step. A typed
    /// path starts from it, relative paths and ⇥ resolve against it.
    fn open_folder(&self) -> Option<String> {
        match self.step {
            ProjectStep::Folders => self
                .browser
                .ready()
                .map(|listing| listing.path.clone())
                .or_else(|| self.browser_path.clone())
                .or_else(|| self.location.as_ref().and_then(|(_, path)| path.clone()))
                .or_else(|| self.home.clone()),
            _ => self.home.clone(),
        }
    }
}

/// The crumb trail as an editable path, address-bar style: ⌘L, the pen at
/// the row's end, or a click beside the crumbs. The open folder stays
/// listed until the typed path answers, so a typo never loses it.
struct PathEdit {
    input: Entity<ComposerInput>,
    /// ListFolders for the typed path is in flight.
    opening: bool,
    /// ⌘↵ asked for the open in flight to add the project too; a later ↵
    /// doesn't take that back.
    adding: bool,
    /// Why the typed path didn't open; the next edit clears it.
    error: Option<SharedString>,
    /// ⇥'s source: the folders of the typed path's parent, keyed by that
    /// parent's `folder_path_key`.
    siblings: Option<(String, Loadable<Vec<slopty_view_proto::FolderEntry>>)>,
    focus_pending: bool,
    open_task: Option<Task<()>>,
    siblings_task: Option<Task<()>>,
    /// Resolves the device's home for `~` when nothing has listed it yet.
    _home_task: Option<Task<()>>,
    _events: Subscription,
}

/// ListFolders on `device_id`, relayed unless it is `local`; no `path`
/// lists the device's home.
async fn list_folders_on(
    engine: crate::state::EngineHandle,
    path: Option<String>,
    device_id: Option<String>,
    local: Option<String>,
) -> Result<FolderListing, String> {
    let mut params = serde_json::Map::new();
    if let Some(path) = path {
        params.insert("path".into(), serde_json::Value::String(path));
    }
    add_space_target(&mut params, device_id.as_deref(), local.as_deref());
    let value = engine
        .client()
        .call(methods::LIST_FOLDERS, serde_json::Value::Object(params))
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// A typed path that didn't list, in the path field's words: a missing
/// folder (POSIX 2, Windows 2/3) or a path through a file (POSIX 20,
/// Windows 267 — the file may be the path or any folder on it) as the
/// device's io error reports it; anything else as the device said it.
fn path_open_error(path: &str, error: &str) -> String {
    let reported = |codes: &[&str]| codes.iter().any(|code| error.contains(code));
    if reported(&["(os error 2)", "(os error 3)"]) {
        format!("No folder at {path}")
    } else if reported(&["(os error 20)", "(os error 267)"]) {
        format!("{path} isn't a folder")
    } else {
        error.to_owned()
    }
}

/// The location a typed path opens under: Home when it is in home, else
/// the deepest drive holding it, else its filesystem root — so the trail
/// reads `Home › code`, not every folder down from `/`.
fn typed_path_location(
    path: &str,
    home: Option<&str>,
    drives: &[DriveEntry],
) -> (String, Option<String>) {
    if home.is_some_and(|home| path_under(path, home)) {
        return ("Home".to_string(), None);
    }
    if let Some(drive) = drives
        .iter()
        .filter(|drive| path_under(path, &drive.path))
        .max_by_key(|drive| folder_path_key(&drive.path).len())
    {
        return (drive.name.clone(), Some(drive.path.clone()));
    }
    let root = breadcrumbs(path)
        .into_iter()
        .next()
        .map_or_else(|| "/".to_string(), |(_, root)| root);
    (root.clone(), Some(root))
}

/// Shorter queries match nearly every folder under home; they are not sent.
const HOME_FOLDER_MIN_QUERY_CHARS: usize = 2;
const HOME_FOLDER_RESULTS: u16 = 20;
const HOME_FOLDER_INDEX_RETRY: Duration = Duration::from_millis(500);
const HOME_FOLDER_INDEX_RETRY_MAX: Duration = Duration::from_secs(2);

/// The new-project picker's search over the device's home, answered by the
/// host's home search index (the one projectless chats use). Entering Home
/// starts the index; leaving Home or closing the picker releases it unless a
/// projectless chat warmed it.
#[derive(Default)]
struct HomeFolderSearch {
    /// Bumped per request; an answer to an older one is dropped.
    generation: u64,
    loading: bool,
    /// The host's index was still scanning at the last answer.
    indexing: bool,
    results: Vec<HomeFolderMatch>,
    /// slopty: the folders the person goes to most, as the warming answer gave them: listed
    /// first while nothing is typed.
    visited: Vec<HomeFolderMatch>,
    /// A request reached the host, so it may hold an index to release.
    started: bool,
    /// The host predates `SearchHomeFolders`; the section stays hidden.
    unsupported: bool,
    task: Option<Task<()>>,
    /// The request that started the host's index.
    warm_task: Option<Task<()>>,
}

/// Refresh `SearchHomeFolders` until indexing finishes, backing off between
/// replies. `generation` identifies the query; `None` is a one-shot warm-up
/// whose answer only tells whether the host supports the method.
fn request_home_folders(
    engine: crate::state::EngineHandle,
    device_id: Option<String>,
    local: Option<String>,
    query: String,
    generation: Option<u64>,
    cx: &mut Context<Shell>,
) -> Task<()> {
    let request = SearchHomeFoldersRequest {
        query,
        limit: Some(HOME_FOLDER_RESULTS),
    };
    cx.spawn(async move |this, cx| {
        let mut params = match serde_json::to_value(&request) {
            Ok(serde_json::Value::Object(params)) => params,
            _ => return,
        };
        add_space_target(&mut params, device_id.as_deref(), local.as_deref());
        let params = serde_json::Value::Object(params);
        let mut delay = HOME_FOLDER_INDEX_RETRY;
        loop {
            let result = engine
                .client()
                .call(methods::SEARCH_HOME_FOLDERS, params.clone())
                .await;
            let retry = this
                .update(cx, |shell, cx| {
                    let selected = shell
                        .add_space
                        .as_ref()
                        .and_then(|flow| shell.add_space_folder_rows(cx).get(flow.active).cloned());
                    let untyped = shell
                        .add_space
                        .as_ref()
                        .is_some_and(|flow| flow.search.read(cx).text().trim().is_empty());
                    let Some(search) = shell.add_space.as_mut().map(|flow| &mut flow.home_search)
                    else {
                        return false;
                    };
                    // slopty: the warming answer, while nothing is typed, is the folders the
                    // person goes to most: they lead the list.
                    if generation.is_none()
                        && untyped
                        && !search.unsupported
                        && let Ok(Ok(found)) =
                            result.as_ref().map(|v| serde_json::from_value::<HomeFolderSearchResult>(v.clone()))
                    {
                        search.visited = found.matches;
                        cx.notify();
                        return false;
                    }
                    if generation.is_some() && generation != Some(search.generation) {
                        return false;
                    }
                    if let Err(slopty_view_rpc::RpcError::UnknownMethod(_)) = &result {
                        search.unsupported = true;
                        search.started = false;
                        search.loading = false;
                        search.indexing = false;
                        cx.notify();
                        return false;
                    }
                    if generation.is_none() || search.unsupported {
                        return false;
                    }
                    search.loading = false;
                    search.indexing = false;
                    match result.map(serde_json::from_value::<HomeFolderSearchResult>) {
                        Ok(Ok(found)) => {
                            search.indexing = found.indexing;
                            search.results = found.matches;
                        }
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "home folder search answer did not decode");
                        }
                        Err(error) => tracing::debug!(%error, "home folder search failed"),
                    }
                    let retry = search.indexing;
                    // New partial results may reorder the rows under the cursor.
                    if let Some(selected) = selected
                        && let Some(active) = shell
                            .add_space_folder_rows(cx)
                            .iter()
                            .position(|row| row == &selected)
                        && let Some(flow) = shell.add_space.as_mut()
                    {
                        flow.active = active;
                    }
                    cx.notify();
                    retry
                })
                .unwrap_or(false);
            if !retry {
                break;
            }
            // The stored task is dropped on edits, navigation or close. Wait
            // after the reply so slow hosts never accumulate parallel calls.
            cx.background_executor().timer(delay).await;
            delay = (delay * 2).min(HOME_FOLDER_INDEX_RETRY_MAX);
            let current = this
                .update(cx, |shell, _| {
                    shell.add_space_in_home()
                        && shell.add_space.as_ref().is_some_and(|flow| {
                            generation == Some(flow.home_search.generation)
                                && !flow.home_search.unsupported
                        })
                })
                .unwrap_or(false);
            if !current {
                break;
            }
        }
    })
}

/// One row of the Folders step, in navigation order.
#[derive(Clone, Debug, PartialEq)]
enum FolderRow {
    /// A subfolder of the open folder, matched by name.
    Local(slopty_view_proto::FolderEntry),
    /// A folder anywhere under home.
    Home(HomeFolderMatch),
}

/// `targetDeviceId` for a remote device; local calls skip the relay.
fn add_space_target(
    params: &mut serde_json::Map<String, serde_json::Value>,
    device_id: Option<&str>,
    local: Option<&str>,
) {
    if let Some(target) = device_id
        && local != Some(target)
    {
        params.insert(
            "targetDeviceId".into(),
            serde_json::Value::String(target.to_owned()),
        );
    }
}

// slopty: a clone may take a while: its notice stays up until the answer takes it down.
/// How long the clone's notice may stay up, at most.
const CLONE_NOTICE_LASTS: Duration = Duration::from_secs(20 * 60);

// slopty: what passes for a repository's address in "Clone repository…": a URL git takes
// (`https://`, `ssh://`, `git://`) or scp-like `user@host:path`, with no spaces. A path on the
// machine is none: the worker clones only from a remote, which the clone keeps as its origin.
/// Whether `url` reads as an address git can clone from.
fn looks_like_repo_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return false;
    }
    let scheme = ["https://", "http://", "ssh://", "git://", "git+ssh://"]
        .iter()
        .any(|scheme| {
            url.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme))
                && url.len() > scheme.len()
        });
    let scp = url.split_once(':').is_some_and(|(host, path)| {
        host.contains('@') && !host.contains('/') && !path.is_empty()
    });
    (scheme || scp) && clone_repo_name(url).is_some()
}

/// The folder a clone of `url` is named: its last segment, without `.git`.
fn clone_repo_name(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty() && name != "." && name != ".." && !name.contains('\\'))
        .then(|| name.to_owned())
}

/// The project a clone became, as `CloneRepo` answers it: `{spaceId, deviceId, path}`; `None`
/// for an answer that does not say.
fn cloned_space(answer: &serde_json::Value) -> Option<(String, String, String)> {
    let field = |name: &str| {
        answer
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Some((field("spaceId")?, field("deviceId")?, field("path")?))
}

/// The title over the picker's matches from under home, with the host's
/// search status trailing.
fn home_folders_header(theme: &Theme, status: Option<&'static str>) -> gpui::Div {
    div()
        .px(px(8.0))
        .pt(px(2.0))
        .pb(px(4.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .child("In Home")
        .when_some(status, |header, status| {
            header.child(div().opacity(0.7).child(status))
        })
}

/// A folder from under home: its name (query highlighted), then where it
/// lives (`~/…`), marked when it is a repository.
fn home_folder_row(
    row: gpui::Stateful<gpui::Div>,
    found: &HomeFolderMatch,
    query: &str,
    theme: &Theme,
    cx: &Context<Shell>,
) -> gpui::Stateful<gpui::Div> {
    let (name, parent) = match found.relative.rsplit_once('/') {
        Some((parent, name)) => (name, format!("~/{parent}")),
        None => (found.relative.as_str(), "~".to_owned()),
    };
    let (path, is_repo) = (found.path.clone(), found.is_repo);
    row.role(gpui::Role::Button) // slopty: read as a button
        .aria_label(SharedString::from(format!("{name} in {parent}")))
        .child(
            icon(icons::FOLDER)
                .size(px(16.0))
                .flex_none()
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .child(div().flex_none().child(popover::search_highlight(
                    SharedString::from(name.to_owned()),
                    Some(query),
                    theme,
                )))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(parent)),
                ),
        )
        .when(is_repo, |el| {
            el.child(
                icon(icons::GIT_BRANCH)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
        })
        .on_click(
            cx.listener(move |this, _, _, cx| this.add_space_descend(path.clone(), is_repo, cx)),
        )
}

/// Folder crumbs shown before the middle folds into `…`, and how many of the
/// deepest stay visible once it does.
const CRUMB_FOLDERS_MAX: usize = 3;
const CRUMB_FOLDERS_TAIL: usize = 2;

/// Fold the middle of a deep trail: returns the folders hidden behind `…`,
/// leaving the deepest [`CRUMB_FOLDERS_TAIL`] in `folders`.
fn fold_crumb_folders<T>(folders: &mut Vec<T>) -> Vec<T> {
    if folders.len() <= CRUMB_FOLDERS_MAX {
        return Vec::new();
    }
    folders
        .drain(..folders.len() - CRUMB_FOLDERS_TAIL)
        .collect()
}

struct Crumb {
    name: SharedString,
    glyph: Option<&'static str>,
    current: bool,
    target: CrumbTarget,
}

enum CrumbTarget {
    Devices,
    Locations,
    Location(String, Option<String>),
    Folder(String),
    /// The `…` fold; opens [`Shell::render_project_crumb_menu`].
    More,
}

pub(super) fn device_glyph(platform: &str) -> &'static str {
    match platform {
        "macos" | "darwin" => icons::LAPTOP,
        "linux" => icons::REMOTE_SERVER, // slopty: a worker on Linux is a server
        "web" => icons::GLOBAL,
        "ios" | "android" => icons::SMARTPHONE,
        _ => icons::MONITOR,
    }
}

/// Segment-aware "is `path` at or under `base`" (`/media/a` is not under
/// `/media/ab`); a root base covers everything. Either separator counts, so
/// Windows drive paths (`D:\` under `D:\`) work too.
fn path_under(path: &str, base: &str) -> bool {
    folder_path_key(base)
        .trim_end_matches(['/', '\\'])
        .is_empty()
        || path_rest(path, base).is_some()
}

/// What follows `base` in `path` when [`path_under`] holds, in `path`'s
/// lexical key: `""` at `base`, `/code` (`\code`) below it.
fn path_rest(path: &str, base: &str) -> Option<String> {
    let path = folder_path_key(path);
    let base = folder_path_key(base);
    let rest = path.strip_prefix(base.trim_end_matches(['/', '\\']))?;
    (rest.is_empty() || rest.starts_with(['/', '\\'])).then(|| rest.to_owned())
}

/// `path` as the path field spells it: from `~` when it is in `home`
/// (`~/code`, `~\code` on Windows), as it is anywhere else. A home at a
/// filesystem root stays spelled out.
fn tilde_path(path: &str, home: Option<&str>) -> String {
    home.filter(|home| parent_path(home).is_some())
        .and_then(|home| path_rest(path, home))
        .map_or_else(|| path.to_owned(), |rest| format!("~{rest}"))
}

// slopty: what removing a project does, in the dialog's words: it leaves the sidebar and its
// sessions move to Archived, where they are restored; nothing on the machine is deleted.
/// What the Remove project dialog says removing `name`, with `count` sessions on `device`, does.
fn remove_project_body(name: &str, device: &str, count: usize) -> String {
    match count {
        0 => format!("\u{201C}{name}\u{201D} leaves the sidebar. Nothing on {device} is deleted."),
        1 => format!(
            "\u{201C}{name}\u{201D} leaves the sidebar, and its session on {device} moves to \
             Archived, where you can restore it. Nothing on {device} is deleted."
        ),
        _ => format!(
            "\u{201C}{name}\u{201D} leaves the sidebar, and its {count} sessions on {device} move \
             to Archived, where you can restore them. Nothing on {device} is deleted."
        ),
    }
}

/// The space-row Rename dialog (same shape as [`RenameChatDialog`]).
pub(super) struct RenameSpaceDialog {
    pub space_id: String,
    pub input: Entity<ComposerInput>,
    pub focus_pending: bool,
    pub _events: Subscription,
}


// Handle-based rail host for the spaces dropdown: its list is a plain
// tracked scroller, so the trait's default metrics/press/drag (off the live
// ScrollHandle) apply unchanged.
impl popover::ScrollRailHost for Shell {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        &mut self.spaces_menu_bar
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.spaces_menu.get().map(|menu| menu.list_scroll.clone())
    }
}

// slopty: the canvas's and a phone's capsule's one action before the first chat.
/// What has to exist before the first chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FirstStep {
    /// No device yet: there is nowhere for a project.
    AddDevice,
    /// A device, but no project on it.
    AddProject,
}

impl FirstStep {
    /// The action's words.
    pub(super) const fn action(self) -> &'static str {
        match self {
            Self::AddDevice => "Add a device",
            Self::AddProject => "Add a project",
        }
    }

    /// The glyph of what the step adds.
    pub(super) const fn glyph(self) -> &'static str {
        match self {
            Self::AddDevice => crate::icons::LAPTOP,
            Self::AddProject => crate::icons::FOLDER,
        }
    }

    /// Take the step: Settings' Add device, or the project picker.
    pub(super) fn take(self, shell: &mut Shell, cx: &mut Context<Shell>) {
        match self {
            Self::AddDevice => shell.open_add_device(cx),
            Self::AddProject => shell.open_add_space(cx),
        }
    }
}

impl Shell {
    pub(super) fn begin_sidebar_disclosure_motion(
        &mut self,
        key: &str,
        resting_height: f32,
        target_height: f32,
    ) {
        let now = self.now();
        let previous = self.sidebar_disclosure_motion.get(key).copied();
        let from = previous
            .filter(|motion| motion.animating(now))
            .map(|motion| motion.current(now))
            .unwrap_or(resting_height);
        let epoch = previous.map_or(1, |motion| motion.epoch + 1);
        self.sidebar_disclosure_motion.insert(
            key.to_owned(),
            SidebarDisclosureMotion::new(epoch, from, target_height, now),
        );
    }

    pub(super) fn render_sidebar_disclosure_body(
        &self,
        key: &str,
        open: bool,
        full_height: f32,
        content: AnyElement,
    ) -> AnyElement {
        let target = if open { full_height } else { 0.0 };
        let frame = div().w_full().flex_none().overflow_hidden().child(content);
        let Some(tween) = self
            .sidebar_disclosure_motion
            .get(key)
            .copied()
            .filter(|motion| motion.animating(self.now()))
        else {
            return frame.h(px(target)).into_any_element();
        };
        let denominator = full_height.max(1.0);
        frame
            .with_animation(
                SharedString::from(format!("sidebar-disclosure-{key}-{}", tween.epoch)),
                motion::COLLAPSE.animation(),
                move |el, t| {
                    let height = motion::lerp(tween.from, tween.to, t);
                    let reveal = (height / denominator).clamp(0.0, 1.0);
                    el.h(px(height))
                        .opacity(0.35 + 0.65 * reveal)
                        .relative()
                        .top(px(-3.0 * (1.0 - reveal)))
                },
            )
            .into_any_element()
    }

    pub(super) fn sidebar_disclosure_chevron(
        &self,
        key: &str,
        open: bool,
        theme: &Theme,
    ) -> AnyElement {
        let resting_reveal = if open { 1.0 } else { 0.0 };
        let chevron = icon(icons::ALT_ARROW_RIGHT)
            .size(px(12.0))
            .text_color(sidebar_header_glyph(theme));
        if let Some(tween) = self
            .sidebar_disclosure_motion
            .get(key)
            .copied()
            // slopty: only an opening or a shutting turns it; an open body that grows or shrinks
            // (History's rows arriving) leaves it open.
            .filter(|motion| motion.animating(self.now()) && (motion.from == 0.0 || motion.to == 0.0))
        {
            let denominator = tween.from.max(tween.to).max(1.0);
            let from = (tween.from / denominator).clamp(0.0, 1.0);
            let to = (tween.to / denominator).clamp(0.0, 1.0);
            div()
                .flex_none()
                .size(px(12.0))
                .child(chevron.with_animation(
                    SharedString::from(format!("sidebar-chevron-{key}-{}", tween.epoch)),
                    SIDEBAR_CHEVRON_TURN.animation(),
                    move |el, t| {
                        let reveal = motion::lerp(from, to, t);
                        el.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                            reveal * 0.25,
                        )))
                    },
                ))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .size(px(12.0))
                .child(
                    chevron.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                        resting_reveal * 0.25,
                    ))),
                )
                .into_any_element()
        }
    }
    // ---- space filter ----

    /// Set the sidebar's session filter (`None` = All spaces). On the
    /// new-session canvas the space context follows the filter — the canvas
    /// default is "the space you're looking at".
    pub(super) fn set_space_filter(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        self.settings.space_filter = filter.clone();
        if let Some(space_id) = filter
            && self.state.read(cx).selected_chat.is_none()
        {
            self.state
                .update(cx, |s, cx| s.select_space(Some(space_id), cx));
        }
        self.close_spaces_menu(cx);
        self.schedule_save(cx);
        // slopty: no pull-request pages.
        cx.notify();
    }

    /// Close the space-filter dropdown through the exit animation (no-op when
    /// it isn't open). Every close path funnels here so the menu always
    /// animates out instead of vanishing.
    pub(super) fn close_spaces_menu(&mut self, cx: &mut Context<Self>) {
        if self.spaces_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.spaces_menu);
            cx.notify();
        }
    }

    fn on_spaces_menu_list_hover(
        &mut self,
        hovered: &bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.spaces_menu_bar.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// Open the new-session canvas in a just-added space, preserving the
    /// sidebar's current project filter.
    pub(super) fn land_in_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.set_route(Route::Chat, cx);
        self.focus_composer(cx);
        // "All" stays as-is; an explicit project filter follows the new
        // project so the first send lands in a visible session.
        if self.settings.space_filter.is_some() {
            self.settings.space_filter = Some(space_id.clone());
        }
        self.nav.push(NavEntry::Chat(String::new()));
        self.settings.last_space_id = Some(space_id.clone());
        self.state.update(cx, |s, cx| {
            s.select_space(Some(space_id), cx);
            s.select_chat(None, cx);
        });
        // slopty: a phone lands on the new chat's page too, off its Chats page.
        if self.compact() {
            Self::set_compact_sessions(false, cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn spaces_menu_rows(&self, cx: &App) -> Vec<SpacesMenuRow> {
        let query = self
            .spaces_menu
            .get()
            .map(|menu| menu.search.read(cx).text().to_string())
            .unwrap_or_default();
        let state = self.state.read(cx);
        let projects = state.projects();
        let names: Vec<String> = projects
            .iter()
            .map(|members| state.representative_space(members[0]).display_name().to_string())
            .collect();
        let mut rows: Vec<SpacesMenuRow> = Vec::new();
        if query.trim().is_empty() {
            rows.push(SpacesMenuRow::All);
        }
        rows.extend(
            popover::filter_indices(&query, &names)
                .into_iter()
                .map(|ix| SpacesMenuRow::Space(projects[ix][0].id.clone())),
        );
        // slopty: the machines after the projects, while there is more than one to tell apart:
        // the machine is a filter, not a level (`docs/decisions/layout.md`).
        if state.devices.len() > 1 {
            let machines: Vec<String> = state.devices.iter().map(|device| device.name.clone()).collect();
            rows.extend(
                popover::filter_indices(&query, &machines)
                    .into_iter()
                    .map(|ix| SpacesMenuRow::Machine(state.devices[ix].id.clone())),
            );
        }
        rows
    }

    /// slopty: narrow the sidebar to machine `device_id`'s sessions, or to every machine's
    /// again when it is the one already shown.
    pub(super) fn toggle_machine_filter(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.settings.machine_filter =
            (self.settings.machine_filter.as_deref() != Some(device_id.as_str())).then_some(device_id);
        self.close_spaces_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// The repository the sidebar filter names, if any — any checkout's row
    /// selects (and reads as) the whole project.
    fn space_filter_key(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let id = self.settings.space_filter.as_deref()?;
        Some(
            state
                .space_row(id)
                .map_or_else(|| id.to_owned(), slopty_view_proto::view::project_key),
        )
    }

    /// The project key of a menu row's space id.
    fn space_row_key(&self, id: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .space_row(id)
            .map_or_else(|| id.to_owned(), slopty_view_proto::view::project_key)
    }

    pub(super) fn open_spaces_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // "PaletteSearch" context: ↑↓/⏎ stay unbound in the input and bubble
        // to the card's key handler.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search projects…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.spaces_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        // The highlight starts ON the current filter row.
        let current = self.space_filter_key(cx);
        let handle = search.read(cx).focus_handle(cx);
        // slopty: closed, it gives the keyboard back (`Popup::open_in`).
        self.spaces_menu.open_in(
            SpacesMenu {
                search,
                active: 0,
                focus: cx.focus_handle(),
                list_scroll: gpui::ScrollHandle::new(),
                _search_events: search_events,
            },
            &handle,
            window,
            cx,
        );
        // Fresh handle at the top — don't let the stale rail baseline read
        // the reopen as scrolling.
        self.spaces_menu_bar.clear_scroll_baseline();
        let rows = self.spaces_menu_rows(cx);
        let start = match &current {
            None => 0,
            Some(key) => rows
                .iter()
                .position(|row| matches!(row, SpacesMenuRow::Space(s) if self.space_row_key(s, cx) == *key))
                .unwrap_or(0),
        };
        if let Some(menu) = self.spaces_menu.open_mut() {
            menu.active = start;
        }
        // Focusable before first paint (the add-space palette's proven order).
        window.focus(&handle, cx);
        cx.notify();
    }

    fn activate_spaces_menu_row(&mut self, row: SpacesMenuRow, cx: &mut Context<Self>) {
        match row {
            SpacesMenuRow::All => self.set_space_filter(None, cx),
            SpacesMenuRow::Space(id) => self.set_space_filter(Some(id), cx),
            SpacesMenuRow::Machine(id) => self.toggle_machine_filter(id, cx),
            SpacesMenuRow::AddSpace => {
                self.close_spaces_menu(cx);
                self.open_add_space(cx);
            }
        }
    }

    /// Dropdown keys (bubbling from the focused search input): ↑↓ navigate,
    /// ⏎ activates the highlighted row, esc closes.
    fn spaces_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation —
        // keys must not drive a dying menu.
        if !self.spaces_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_spaces_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let rows = self.spaces_menu_rows(cx);
                // +1: the pinned footer stays in the nav order, exactly as
                // when it was the list's last row.
                let count = rows.len() + 1;
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.spaces_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    // The footer renders below the scroller — only in-list
                    // rows can be scrolled to (the footer index would leave
                    // a request pending against a row that never exists).
                    if menu.active < rows.len() {
                        menu.list_scroll.scroll_to_item(menu.active);
                    }
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.spaces_menu.get().map(|m| m.active).unwrap_or(0);
                let rows = self.spaces_menu_rows(cx);
                // One past the scrollable rows is the pinned footer.
                let row = if active < rows.len() {
                    rows[active].clone()
                } else {
                    SpacesMenuRow::AddSpace
                };
                self.activate_spaces_menu_row(row, cx);
            }
            popover::MenuKey::Backspace | popover::MenuKey::Other => {}
        }
    }

    /// The sidebar's space-filter row: current filter ("All projects" or the
    /// space's name), the dropdown floating beneath while open.
    /// Sits OUTSIDE the sidebar's scroll region so the float never clips.
    pub(super) fn render_spaces_filter(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // slopty: before any project and chat there is nothing to filter: keep the row's top
        // breathing room only.
        if self.awaits_first_project(cx) && self.state.read(cx).chats.iter().all(|c| c.archived) {
            return div().flex_none().h(px(4.0)).into_any_element();
        }
        let filter = self.settings.space_filter.clone();
        // The filtered project's artwork and name, as its dropdown row
        // draws them, plus the offline glyph when no host is reachable.
        let (label, space, offline): (SharedString, Option<slopty_view_proto::Space>, bool) = {
            let state = self.state.read(cx);
            match filter.as_deref().and_then(|id| state.space_row(id)) {
                Some(space) => {
                    let members = state.project_members(space);
                    let (_, offline) = state.project_device_tag(&members, crate::clock::utc(cx));
                    (
                        state
                            .representative_space(space)
                            .display_name()
                            .to_string()
                            .into(),
                        Some(space.clone()),
                        offline,
                    )
                }
                None => (SharedString::from("All projects"), None, false),
            }
        };
        // slopty: the machine the list is narrowed to follows the project, out of reach or not.
        let (label, offline) = match self.settings.machine_filter.as_deref() {
            Some(id) => {
                let state = self.state.read(cx);
                let name = state.device_name(id).unwrap_or("?");
                let reached = !super::sidebar_status::out_of_reach(state, id, crate::clock::utc(cx));
                (SharedString::from(format!("{label} \u{b7} {name}")), offline || !reached)
            }
            None => (label, offline),
        };
        let art = match &space {
            Some(space) => {
                let profile = self.active_profile_key(cx);
                self.project_icons
                    .render(&self.state, Some(space), profile, false, cx)
            }
            None => icon(icons::FOLDER)
                .size(px(16.0))
                .text_color(theme.text_muted)
                .into_any_element(),
        };
        let open = self.spaces_menu.is_open();
        // slopty: a caret says the row opens a menu (T3's select triggers, Claude desktop's
        // pickers), turning over the house's chevron morph while it is open.
        let turn = motion::state_t(
            "spaces-filter-caret",
            open,
            motion::CHEVRON,
            self.reduced_motion.get(),
            cx.frame_time(),
        );
        let caret = icon(icons::ALT_ARROW_DOWN)
            .size(px(12.0))
            .flex_none()
            .text_color(sidebar_header_glyph(theme))
            .with_transformation(gpui::Transformation::rotate(gpui::percentage(0.5 * turn)));
        let filter_name = SharedString::from(format!("Filter projects: {label}"));

        let trigger = div()
            .id("spaces-filter")
            // slopty: a screen reader's button that says what it filters to and whether its menu
            // is open; a keyboard stop Enter or Space opens.
            .role(gpui::Role::Button)
            .aria_label(filter_name)
            .aria_expanded(open)
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring_inset(el, theme))
            .flex_1()
            .min_w_0()
            .h(px(29.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .rounded(px(8.0))
            .px(px(Theme::SPACE_SM))
            .text_size(crate::typography::ui_rems(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(motion::hover_blend(
                "spaces-filter",
                theme.text.opacity(0.8),
                theme.text,
            ))
            .bg(if open {
                theme.glass_hover()
            } else {
                motion::hover_blend(
                    "spaces-filter",
                    theme.glass_hover().opacity(0.0),
                    theme.glass_hover(),
                )
            })
            .hover_fade("spaces-filter")
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.spaces_menu.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                // A press that found the menu open closes it (the card's
                // mouse-down-out already began the close) — never reopen.
                if this.spaces_menu.take_press_was_open() {
                    this.close_spaces_menu(cx);
                } else {
                    this.open_spaces_menu(window, cx);
                }
            }))
            .child(popover::picker_icon_slot(art))
            // flex_1 gives long space names a bound to fade against.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(super::sidebar::sidebar_faded_label(
                        "spaces-filter-label".into(),
                        false,
                        label,
                    ))
                    // Disconnected glyph, not the word (user request).
                    .when(offline, |el| {
                        el.child(
                            icon(icons::WIFI_OFF)
                                .size(px(12.0))
                                .flex_none()
                                .text_color(theme.warning.opacity(0.8)),
                        )
                    })
                    .child(caret),
            );
        let trigger = if self.spaces_menu.get().is_some() {
            let closing = self.spaces_menu.closing_at(crate::clock::now(cx));
            let menu = self.render_spaces_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below(
                "spaces-filter-menu",
                menu,
                closing,
            ))
        } else {
            trigger
        };

        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .px(px(Theme::SPACE_SM))
            .pt(px(8.0))
            .pb(px(4.0))
            .child(trigger)
            .into_any_element()
    }

    // slopty: a sidebar row's keys (`SidebarKeys`): Up and Down move to the row drawn above or
    // below, Delete archives it (Undo brings it back, as from its menu).
    /// Handle `key` on the focused row of `chat`; whether it was the row's.
    pub(super) fn sidebar_row_key(
        &mut self,
        chat: &str,
        key: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if key.modifiers.modified() {
            return false;
        }
        match key.key.as_str() {
            "up" | "down" => {
                let order = self.sidebar_visible_order(cx);
                let Some(at) = order.iter().position(|id| id == chat) else { return false };
                let next = if key.key == "up" { at.checked_sub(1) } else { Some(at + 1) };
                if let Some(focus) = next.and_then(|ix| order.get(ix)).and_then(|id| self.sidebar_keys.drawn(id)) {
                    window.focus(&focus, cx);
                }
                true
            }
            "backspace" | "delete" => {
                self.archive_chat(chat.to_owned(), cx);
                true
            }
            _ => false,
        }
    }

    // slopty: what a chat needs first, offered by the canvas and by a phone's capsule alike:
    // a device to work on, then a project on it, unless a chat runs outside any project.
    /// The step before the first chat can start, or `None` once one can.
    pub(super) fn first_step(&self, cx: &App) -> Option<FirstStep> {
        let state = self.state.read(cx);
        if !state.spaces.is_empty() || state.no_project {
            None
        } else if state.devices.is_empty() {
            Some(FirstStep::AddDevice)
        } else {
            Some(FirstStep::AddProject)
        }
    }

    /// Whether the app waits for its first project: a device to hold one is known.
    pub(super) fn awaits_first_project(&self, cx: &App) -> bool {
        self.first_step(cx) == Some(FirstStep::AddProject)
    }

    /// The dropdown card: search on top, "All projects" + space rows (check on
    /// the active filter; right-click for rename/remove) + "New project".
    fn render_spaces_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll) = {
            let Some(menu) = self.spaces_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
            )
        };
        let rows = self.spaces_menu_rows(cx);
        let scrollbar = popover::rail(self, "spaces-menu-scrollbar", theme, cx);
        let filter = self.settings.space_filter.clone();
        let filter_key = self.space_filter_key(cx);
        // Each row is a whole repository: its artwork and name, plus the
        // offline glyph when no device holding it is reachable. Consume
        // `rows` to avoid cloning the list children per frame.
        // slopty: a machine's row draws its kind's glyph (`device_glyph`) in the artwork's place.
        let machine = self.settings.machine_filter.clone();
        let details: Vec<MenuDetail> = {
            let state = self.state.read(cx);
            let now = crate::clock::utc(cx);
            rows.into_iter()
                .map(|row| match row {
                    SpacesMenuRow::All => MenuDetail {
                        row: SpacesMenuRow::All,
                        label: SharedString::from("All projects"),
                        selected: filter.is_none(),
                        ..MenuDetail::default()
                    },
                    SpacesMenuRow::Machine(id) => {
                        let device = state.devices.iter().find(|device| device.id == id);
                        MenuDetail {
                            label: SharedString::from(device.map_or("?", |device| device.name.as_str()).to_owned()),
                            offline: super::sidebar_status::out_of_reach(state, &id, now),
                            selected: machine.as_deref() == Some(id.as_str()),
                            glyph: Some(device_glyph(device.map_or("", |device| device.platform.as_str()))),
                            turned_away: super::sidebar_status::turned_away(state, &id)
                                .map(|why| SharedString::from(why.to_owned())),
                            row: SpacesMenuRow::Machine(id),
                            ..MenuDetail::default()
                        }
                    }
                    SpacesMenuRow::Space(id) => match state.space_row(&id) {
                        Some(space) => {
                            let key = slopty_view_proto::view::project_key(space);
                            let members = state.project_members(space);
                            let (_, offline) = state.project_device_tag(&members, now);
                            MenuDetail {
                                row: SpacesMenuRow::Space(id),
                                label: state.representative_space(space).display_name().to_string().into(),
                                space: Some(space.clone()),
                                offline,
                                selected: filter_key.as_deref() == Some(key.as_str()),
                                ..MenuDetail::default()
                            }
                        }
                        None => MenuDetail {
                            selected: filter.as_deref() == Some(id.as_str()),
                            row: SpacesMenuRow::Space(id),
                            label: SharedString::from("?"),
                            ..MenuDetail::default()
                        },
                    },
                    // spaces_menu_rows never yields this variant — the
                    // footer is rendered by the card, not the list.
                    SpacesMenuRow::AddSpace => unreachable!(),
                })
                .collect()
        };
        // The pinned footer's keyboard-nav index: one past the last
        // scrollable row, its permanent place at the end of the nav order.
        let add_index = details.len();
        let profile = self.active_profile_key(cx);
        let icons: Vec<AnyElement> = details
            .iter()
            .map(|detail| match (&detail.space, detail.glyph) {
                (Some(space), _) => self.project_icons.render(
                    &self.state,
                    Some(space),
                    profile.clone(),
                    detail.selected,
                    cx,
                ),
                (None, Some(glyph)) => icon(glyph)
                    .size(px(14.0))
                    .text_color(theme.text_muted)
                    .into_any_element(),
                (None, None) => icon(icons::FOLDER)
                    .size(px(14.0))
                    .text_color(theme.text_muted)
                    .into_any_element(),
            })
            .collect();
        // slopty: a machine that turns this device away wears the login badge where an
        // unreached one wears the disconnected glyph.
        let badges: Vec<Option<AnyElement>> = details
            .iter()
            .enumerate()
            .map(|(ix, detail)| match (&detail.row, &detail.turned_away) {
                (SpacesMenuRow::Machine(id), Some(why)) => Some(self.turned_away_badge(
                    format!("spaces-menu-turned-away-{ix}").into(),
                    id.clone(),
                    why.clone(),
                    cx,
                )),
                _ => None,
            })
            .collect();

        let list = popover::menu_scroll_host("spaces-menu-list-host")
            .on_hover(cx.listener(Self::on_spaces_menu_list_hover))
            .child(popover::faded_menu_list(
                &list_scroll,
                popover::menu_scroll_list("spaces-menu-list", &list_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    // Same scroll budget as the composer project menu.
                    .max_h(px(224.0))
                    .children(details.into_iter().zip(icons).zip(badges).enumerate().map(
                        |(ix, ((MenuDetail { row, label, offline, selected, .. }, art), badge))| {
                            let menu_space = match &row {
                                SpacesMenuRow::Space(id) => Some(id.clone()),
                                _ => None,
                            };
                            let activate = row;
                            popover::picker_row(
                                theme,
                                selected,
                                ix == active,
                                format!("spaces-menu-row-{ix}"),
                            )
                            .id(("spaces-menu-row", ix))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_spaces_menu_row(activate.clone(), cx);
                            }))
                            .when_some(menu_space, |el, space_id| {
                                el.on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        this.space_menu.open((space_id.clone(), event.position));
                                        cx.notify();
                                    }),
                                )
                            })
                            .child(popover::picker_icon_slot(art))
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            // Disconnected glyph, not the word (user request).
                            .map(|el| match badge {
                                Some(badge) => el.child(div().flex_none().child(badge)),
                                None => el.when(offline, |el| {
                                    el.child(
                                        icon(icons::WIFI_OFF)
                                            .size(px(12.0))
                                            .flex_none()
                                            .text_color(theme.warning.opacity(0.8)),
                                    )
                                }),
                            })
                            // No check glyph — the selected row's glass plate
                            // is the selection signal.
                        },
                    )),
            ))
            .children(scrollbar);

        popover::popover_card(theme)
            // Match the trigger row as the sidebar is resized. Both live
            // inside the same SPACE_SM horizontal gutters.
            .w(px(self.sidebar_content_width() - 2.0 * Theme::SPACE_SM))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.spaces_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_spaces_menu(cx);
            }))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    // Same 2px rhythm as the composer project menu.
                    .gap(px(2.0))
                    .child(list)
                    // "New project" is a pinned action row under the list
                    // (the chat composer's project menu treatment) —
                    // scrolling must never carry it away, and its nav index
                    // (`add_index`) keeps it LAST.
                    .child(popover::picker_divider())
                    .child(
                        popover::picker_row(
                            theme,
                            false,
                            active == add_index,
                            "spaces-menu-add".to_string(),
                        )
                        .id("spaces-menu-add")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.activate_spaces_menu_row(SpacesMenuRow::AddSpace, cx);
                        }))
                        .child(popover::picker_icon_slot(
                            icon(icons::PLUS)
                                .size(px(12.0))
                                .text_color(theme.text_muted)
                                .into_any_element(),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from("New project")),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// slopty: what the sidebar narrows its list to: the project filter and the machine filter.
    pub(super) fn sidebar_filter(&self) -> super::sidebar_status::Filter<'_> {
        super::sidebar_status::Filter {
            project: self.settings.space_filter.as_deref(),
            machine: self.settings.machine_filter.as_deref(),
        }
    }

    /// The list as the sidebar draws it (`sidebar_status::list`) under the sidebar's filter, by
    /// the sidebar's clock, Inactive paged behind its "Show more"; notes when it next reads
    /// otherwise, for the clock's tick.
    pub(super) fn sidebar_list(&self, cx: &App) -> Vec<super::sidebar_status::Group> {
        let state = self.state.read(cx);
        let (mut groups, next) = super::sidebar_status::list(
            state,
            self.sidebar_filter(),
            self.sidebar_clock, // slopty: the sidebar's clock
        );
        if let Some(next) = next {
            self.note_sidebar_change(next);
        }
        let selected = state.selected_chat.as_deref();
        for group in groups.iter_mut().filter(|group| group.key == super::sidebar_status::INACTIVE) {
            let shown = self
                .sidebar_group_shown
                .get(&group.key)
                .copied()
                .unwrap_or(super::sidebar_status::INACTIVE_FIRST);
            super::sidebar_status::page(group, shown, selected);
        }
        groups
    }

    /// Flat top-to-bottom chat ids exactly as [`Self::render_active_rows`] draws them: the
    /// groups in order, without a shut group's rows or those behind a "Show more". Jump
    /// shortcuts and session cycling read this projection so keyboard order never drifts from
    /// the screen.
    pub(super) fn sidebar_visible_order(&self, cx: &Context<Self>) -> Vec<String> {
        self.sidebar_list(cx)
            .into_iter()
            .filter(|group| !self.sidebar_collapsed_groups.contains(&group.key))
            .flat_map(|group| group.rows)
            .map(|row| row.chat.id)
            .collect()
    }

    /// Hover targets include the paged Archived shelf, unlike jump shortcuts.
    pub(super) fn sidebar_hover_card_row_visible(&self, chat_id: &str, cx: &Context<Self>) -> bool {
        if self.sidebar_visible_order(cx).iter().any(|id| id == chat_id) {
            return true;
        }
        self.archived_open
            && self
                .archived_sidebar_chats(cx)
                .iter()
                .take(self.archived_shown.max(ARCHIVED_INITIAL_ROWS))
                .any(|chat| chat.id == chat_id)
    }

    /// Open whatever hides `chat_id`'s sidebar row — the sidebar itself, its shut group, the
    /// Inactive page, the Archived disclosure and its page — so an inline rename has a row to
    /// draw on. Each disclosure opens with its usual motion. False when the project filter
    /// hides the row (the filter is the user's to clear); the sidebar notice says why.
    pub(super) fn reveal_sidebar_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) -> bool {
        let group = {
            let state = self.state.read(cx);
            let (groups, _) = super::sidebar_status::list(state, self.sidebar_filter(), self.sidebar_clock);
            groups
                .into_iter()
                .find(|group| group.rows.iter().any(|row| row.chat.id == chat_id))
                .map(|group| group.key)
        };
        let archived = self
            .archived_sidebar_chats(cx)
            .iter()
            .position(|chat| chat.id == chat_id);
        if group.is_none() && archived.is_none() {
            self.show_notice(crate::toast::ToastKind::Error, "Couldn\u{2019}t rename this session here. Clear the sidebar\u{2019}s filter, then rename it.", cx);
            return false;
        }
        if let Some(key) = group {
            if self.sidebar_collapsed_groups.remove(&key) {
                self.queue_sidebar_reveal(&format!("group:{key}"));
            }
            // slopty: a row behind Inactive's "Show more" shows the whole group.
            if !self.sidebar_visible_order(cx).iter().any(|id| id == chat_id) {
                self.sidebar_group_shown.insert(key, usize::MAX);
            }
        } else if let Some(index) = archived {
            if !self.archived_open {
                self.archived_open = true;
                self.archived_shown = ARCHIVED_INITIAL_ROWS;
                self.queue_sidebar_reveal("archived");
            }
            // Page the shelf just far enough to include the row, in the
            // same steps "Show more" takes.
            let shown = self.archived_shown.max(ARCHIVED_INITIAL_ROWS);
            if index >= shown {
                self.archived_shown = ARCHIVED_INITIAL_ROWS
                    + (index + 1 - ARCHIVED_INITIAL_ROWS).div_ceil(ARCHIVED_PAGE_ROWS)
                        * ARCHIVED_PAGE_ROWS;
            }
        }
        if self.settings.sidebar_collapsed {
            self.toggle_sidebar(cx);
        }
        cx.notify();
        true
    }

    /// Mark a disclosure opened for a reveal: its motion starts on the next
    /// render, where the body height is known. Like a header click, it
    /// replaces the resort glide for this movement.
    fn queue_sidebar_reveal(&mut self, key: &str) {
        self.sidebar_reveal_motions.insert(key.to_owned());
        self.sidebar_prev_order.clear();
        self.sidebar_resort.clear();
        self.sidebar_new_keys.clear();
    }

    /// Start a revealed disclosure's opening motion now that its height is
    /// known.
    pub(super) fn begin_queued_sidebar_reveal(&mut self, key: &str, open: bool, body_height: f32) {
        if open && self.sidebar_reveal_motions.remove(key) {
            self.begin_sidebar_disclosure_motion(key, 0.0, body_height);
        }
    }

    /// slopty: how long ago `then` was, by the sidebar's clock; notes when that next reads
    /// differently, for the clock's tick.
    pub(super) fn sidebar_time_ago(&self, then: chrono::DateTime<Utc>) -> SharedString {
        let now = self.sidebar_clock;
        if let Some(changes) = time_ago_changes(then, now) {
            self.note_sidebar_change(changes);
        }
        format_time_ago(then, now).into()
    }

    /// slopty: something read by the sidebar's clock next reads differently at `at`; its tick
    /// comes no later.
    pub(super) fn note_sidebar_change(&self, at: chrono::DateTime<Utc>) {
        let next = self.sidebar_clock_next.get();
        if next.is_none_or(|next| at < next) {
            self.sidebar_clock_next.set(Some(at));
        }
    }

    /// slopty: take the clock again when the soonest time the last sidebar showed next reads
    /// differently, and draw again then: until then every sidebar built says the same.
    pub(super) fn tick_sidebar_clock(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.sidebar_clock_next.take() else {
            return;
        };
        // slopty: nothing else moves with the clock: a session or machine going quiet is the
        // state's own transition, which draws the sidebar again (`AppState::next_clock_transition`).
        if self.sidebar_clock_tick.as_ref().is_some_and(|(due, _)| *due <= at) {
            return;
        }
        let wait = (at - crate::clock::utc(cx)).to_std().unwrap_or_default() + std::time::Duration::from_millis(5);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.sidebar_clock_tick = None;
                this.sidebar_clock = crate::clock::utc(cx);
                cx.notify();
            })
            .ok();
        });
        self.sidebar_clock_tick = Some((at, task));
    }

    /// The live list's keyed rows (`sidebar_status::list`): each group's header and, open, its
    /// rows; shut, its header wears its loudest row's mark and how many rows it holds.
    pub(super) fn render_active_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> SidebarSessionRows {
        let groups = self.sidebar_list(cx);
        // slopty: Tab reaches the open chat's row, or the first with none open (`SidebarKeys`);
        // the rows of chats gone let their focus go.
        {
            let selected = self.state.read(cx).selected_chat.clone();
            let mut rows = groups.iter().flat_map(|group| &group.rows);
            let first = rows.next().map(|row| row.chat.id.clone());
            let listed = |id: &String| groups.iter().flat_map(|group| &group.rows).any(|row| &row.chat.id == id);
            self.sidebar_keys.stop = selected.filter(listed).or(first);
            let state = self.state.read(cx);
            self.sidebar_keys
                .focus
                .borrow_mut()
                .retain(|id, _| state.chats.iter().any(|chat| &chat.id == id));
        }
        let selected = self.state.read(cx).selected_chat.clone();
        // Re-checked at render so the chips drop the FRAME a popover opens,
        // not on the next modifier event — the jumps are suppressed under it.
        let jump_hints = self.jump_hints && !self.overlay_owns_keyboard();
        let keymap = self.settings.keymap.clone();
        // Flat top-to-bottom slot across groups: the same order
        // `sidebar_visible_order` hands the jump shortcuts and cycling, so a
        // chip always names the key that opens its row.
        let visible_slots: std::collections::HashMap<_, _> = self
            .sidebar_visible_order(cx)
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        let mut rendered = Vec::new();
        for group in groups {
            let super::sidebar_status::Group { key, label, rows, hidden } = group;
            // slopty: a shut group speaks for its rows with the loudest mark among those still
            // heard (a cached row's is the last word from a machine out of reach).
            let loudest = super::marks::loudest(rows.iter().filter(|row| !row.cached).map(|row| &row.mark)).cloned();
            let row_count = rows.len() + hidden;
            let shown_rows = rows.len();
            let motion_key = format!("group:{key}");
            let collapsed = self.sidebar_collapsed_groups.contains(&key);
            // slopty: a shut group builds no rows while no motion shows them, so neither the frame
            // nor a screen reader holds rows that are not on screen.
            let draws = !collapsed
                || self.sidebar_disclosure_motion.get(&motion_key).is_some_and(|motion| motion.animating(self.now()));
            let mut rendered_rows = Vec::with_capacity(rows.len());
            for row in rows.into_iter().filter(|_| draws) {
                let super::sidebar_status::Row { chat, status, hint, .. } = row;
                let is_selected = selected.as_deref() == Some(chat.id.as_str());
                let harness = chat.config.as_ref().map(|c| c.harness);
                let height = SIDEBAR_ROW_HEIGHT;
                // Only rows a jump slot can reach wear a chip.
                let jump_slot = visible_slots.get(&chat.id).copied();
                let jump_label: Option<SharedString> = if jump_hints && let Some(slot) = jump_slot {
                    let combo = keymap.get(ShortcutId::JumpSession(slot));
                    (slot < JUMP_SLOTS && !combo.is_empty()).then(|| crate::keys::combo_text(combo))
                } else {
                    None
                };
                let element = self.render_chat_row(
                    chat.id.clone(),
                    transcript::single_line(
                        &chat.title.clone().unwrap_or_else(|| "New session".into()),
                    )
                    .into(),
                    hint,
                    SharedString::default(),
                    SharedString::default(),
                    None,
                    None,
                    harness,
                    status,
                    is_selected,
                    false,
                    jump_label,
                    None,
                    theme,
                    cx,
                );
                // slopty: on a phone, swipe to archive, press long for its menu.
                let element = self.compact_session_row(element, &chat.id, false, cx);
                let element = div()
                    .id(SharedString::from(format!("session-slot-{}", chat.id)))
                    .h(px(height))
                    .flex_none()
                    .child(element)
                    .into_any_element();
                rendered_rows.push((format!("c:{}", chat.id), height, element));
            }
            // slopty: Inactive's rows past its page wait behind "Show more".
            let more = self.render_group_more(&key, hidden, theme, cx);
            let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
                + SIDEBAR_ROW_HEIGHT * shown_rows as f32
                + more.as_ref().map_or(0.0, |(_, height, _)| *height)
                + SIDEBAR_LIST_GAP * shown_rows.saturating_sub(1) as f32;
            rendered_rows.extend(more.filter(|_| draws));
            self.begin_queued_sidebar_reveal(&motion_key, !collapsed, body_height);
            let body = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP))
                .children(rendered_rows.into_iter().map(|(_, _, row)| row));
            let chevron = self.sidebar_disclosure_chevron(&motion_key, !collapsed, theme);
            let rollup = collapsed.then(|| group_rollup(&key, row_count, loudest.as_ref(), theme));
            let toggle_key = key.clone();
            let toggle_motion_key = motion_key.clone();
            let header = sidebar_disclosure_header(
                theme,
                SharedString::from(format!("sidebar-group-{key}")),
                &key,
                None,
                label.clone(),
                // slopty: open, its count fades in under the pointer; shut, it stands beside the
                // loudest mark.
                (!collapsed).then_some(row_count),
                rollup,
                chevron,
                !collapsed,
                None,
            )
            .debug_selector({
                let selector = format!("sidebar-group-{key}"); // slopty: for the tests
                move || selector
            })
            // slopty: shut, a screen reader hears what the header wears.
            .when(collapsed, |header| {
                let words = match loudest.as_ref().and_then(super::marks::Mark::word) {
                    Some(word) => format!("{label}, {row_count}, {word}"),
                    None => format!("{label}, {row_count}"),
                };
                header.aria_label(SharedString::from(words))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = !this.sidebar_collapsed_groups.contains(&toggle_key);
                this.sidebar_group_shown.remove(&toggle_key); // slopty: its first page again
                this.begin_sidebar_disclosure_motion(
                    &toggle_motion_key,
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                if was_open {
                    this.sidebar_collapsed_groups.insert(toggle_key.clone());
                } else {
                    this.sidebar_collapsed_groups.remove(&toggle_key);
                }
                cx.notify();
            }));
            let body = self.render_sidebar_disclosure_body(
                &motion_key,
                !collapsed,
                body_height,
                body.into_any_element(),
            );
            let height =
                SIDEBAR_DISCLOSURE_SECTION_HEIGHT + if collapsed { 0.0 } else { body_height };
            let element = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_SECTION_GAP))
                .child(header)
                .child(body)
                .into_any_element();
            rendered.push((format!("g:{key}"), height, element));
        }
        SidebarSessionRows { rows: rendered }
    }

    /// The Archived shelf's chats under the sidebar's filter, in sidebar order.
    fn archived_sidebar_chats(&self, cx: &App) -> Vec<slopty_view_proto::Chat> {
        let state = self.state.read(cx);
        let filter = self.sidebar_filter();
        let in_filter = state.project_filter(filter.project);
        let mut rows: Vec<slopty_view_proto::Chat> = state
            .chats
            .iter()
            // Spawned children stay out of the Archived section too — the
            // same top-level rule as `visible_chats`.
            .filter(|c| c.archived && c.is_top_level())
            .filter(|chat| in_filter(chat) && filter.on_machine(chat))
            .cloned()
            .collect();
        rows.sort_by(|left, right| compare_sidebar_chats(left, right));
        rows
    }

    /// Archived sessions share active-row data and layout, with a restore action.
    /// The shelf starts with ten sessions and pages by 25.
    pub(super) fn render_archived_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        const INITIAL: usize = ARCHIVED_INITIAL_ROWS;
        const PAGE: usize = ARCHIVED_PAGE_ROWS;
        let now = self.sidebar_clock; // slopty: the sidebar's clock
        let rows = self.archived_sidebar_chats(cx);
        if rows.is_empty() {
            return None;
        }
        // slopty: each says its project while the shelf holds more than one (`sidebar_status::shelf`).
        let rows = super::sidebar_status::shelf(self.state.read(cx), rows, now);
        let total = rows.len();
        let open = self.archived_open;
        let shown = self.archived_shown.max(INITIAL);
        let visible_count = total.min(shown);
        let has_more = total > shown;
        // "Show more" matches the row slot.
        let more_height = SIDEBAR_ROW_HEIGHT;
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + visible_count as f32 * SIDEBAR_ROW_HEIGHT
            + visible_count.saturating_sub(1) as f32 * SIDEBAR_LIST_GAP
            + if has_more {
                more_height + SIDEBAR_LIST_GAP
            } else {
                0.0
            };
        self.begin_queued_sidebar_reveal("archived", open, body_height);
        // Match Pinned: a muted label with a right-aligned disclosure chevron, its count shown
        // under the pointer.
        let chevron = self.sidebar_disclosure_chevron("archived", open, theme);
        let header = sidebar_disclosure_header(
            theme,
            "archived-toggle",
            "archived",
            None,
            "Archived".into(),
            Some(total),
            None,
            chevron,
            open,
            None,
        )
            .debug_selector(|| "archived-toggle".into())
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = this.archived_open;
                this.begin_sidebar_disclosure_motion(
                    "archived",
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.archived_open = !was_open;
                this.archived_shown = INITIAL;
                cx.notify();
            }));
        let section = div().flex().flex_col().child(header);
        let body = {
            let selected = self.state.read(cx).selected_chat.clone();
            let mut list = div()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP));
            for row in rows.into_iter().take(shown) {
                let chat = row.chat;
                let is_selected = selected.as_deref() == Some(chat.id.as_str());
                let harness = chat.config.as_ref().map(|c| c.harness);
                let element = self.render_chat_row(
                    chat.id.clone(),
                    transcript::single_line(
                        &chat.title.clone().unwrap_or_else(|| "New session".into()),
                    )
                    .into(),
                    row.hint,
                    SharedString::default(),
                    SharedString::default(),
                    None,
                    None,
                    harness,
                    row.status,
                    is_selected,
                    true,
                    None,
                    None,
                    theme,
                    cx,
                );
                // slopty: on a phone, swipe to bring back, press long for its menu.
                list = list.child(self.compact_session_row(element, &chat.id, true, cx));
            }
            let mut body = div().w_full().flex().flex_col().child(list);
            if has_more {
                let remaining = (total - shown).min(PAGE);
                body = body.child(
                    div()
                        .id("archived-more")
                        // Sits outside the rows' gapped column — match the
                        // list's 2px row gap or it fuses with the last row.
                        .mt(px(2.0))
                        .h(px(more_height))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(Theme::SPACE_SM))
                        .rounded(px(6.0))
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text_muted.opacity(0.55))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.archived_shown = this.archived_shown.max(INITIAL) + PAGE;
                            cx.notify();
                        }))
                        .child(
                            crate::icons::icon(crate::icons::PLUS)
                                .size(px(14.0))
                                .flex_none(),
                        )
                        .child(SharedString::from(format!("Show {remaining} more"))),
                );
            }
            body.into_any_element()
        };
        let body = self.render_sidebar_disclosure_body("archived", open, body_height, body);
        // The active list already provides the spacing above Archived.
        let section = section.child(body);
        Some(section.into_any_element())
    }

    // ---- add-space flow ----

    // slopty: no pull-request pages.

    pub(super) fn open_add_space(&mut self, cx: &mut Context<Self>) {
        self.release_home_folder_search(cx);
        self.command_palette = None;
        crate::live::chrome::release_held_keys(cx);
        self.project_crumb_menu = popover::Popup::default();
        // "PaletteSearch" context: navigation keys stay unbound so ↑↓/←/→/⏎
        // bubble to the palette frame (`add_space_key`) instead of moving the
        // text caret — Enter and ⌘Enter are both handled there.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search devices…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                // Typing `/` after a query that names a folder descends into
                // it — the query reads as a path segment, so the slash IS the
                // pick (shell-style). Otherwise the slash stays in the query
                // (it matches nothing, which is honest feedback).
                if this.add_space_slash_descend(cx) {
                    return;
                }
                if let Some(flow) = this.add_space.as_mut() {
                    flow.active = 0;
                    flow.list_scroll.set_offset(gpui::Point::default());
                }
                this.search_home_folders(cx);
                cx.notify();
            }
        });
        self.add_space = Some(AddSpaceFlow {
            step: ProjectStep::Devices,
            location: None,
            device: None,
            search,
            browser: Loadable::Idle,
            drives: Loadable::Idle,
            browser_path: None,
            home: None,
            browser_repo: false,
            active: 0,
            submit_busy: false,
            error: None,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            crumb_scroll: gpui::ScrollHandle::new(),
            crumb_key: String::new(),
            focus_pending: true,
            return_focus: None, // slopty
            load_task: None,
            drives_task: None,
            submit_task: None,
            home_search: HomeFolderSearch::default(),
            cloning: false, // slopty
            path_edit: None,
            _search_events: search_events,
        });
        cx.notify();
    }

    /// Close the new-project picker, releasing the host's home index.
    pub(super) fn close_add_space(&mut self, cx: &mut Context<Self>) {
        self.release_home_folder_search(cx);
        // slopty: the keyboard goes back where it was (`popover::return_focus`).
        if let Some(flow) = self.add_space.take()
            && let Some(before) = flow.return_focus
        {
            popover::give_focus_back(before, flow.search.focus_handle(cx));
        }
        cx.notify();
    }

    /// Whether the picker browses the Home location, where the query also
    /// searches every folder under home.
    fn add_space_in_home(&self) -> bool {
        self.add_space.as_ref().is_some_and(|flow| {
            flow.step == ProjectStep::Folders
                && !flow.cloning // slopty: an address is no folder to look for
                && flow
                    .location
                    .as_ref()
                    .is_some_and(|(_, path)| path.is_none())
        })
    }

    /// Look the query up under the device's home. The first call in Home
    /// also starts the host's index, so the first real query is fast.
    fn search_home_folders(&mut self, cx: &mut Context<Self>) {
        let in_home = self.add_space_in_home();
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let query = flow.search.read(cx).text().trim().to_owned();
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        let search = &mut flow.home_search;
        search.generation += 1;
        search.loading = false;
        search.indexing = false;
        search.task = None;
        search.results.clear();
        let Some(engine) = engine.filter(|_| in_home && !search.unsupported) else {
            return;
        };
        if !search.started {
            // Its own task: typing must not cancel it.
            search.started = true;
            search.warm_task = Some(request_home_folders(
                engine.clone(),
                device_id.clone(),
                local.clone(),
                String::new(),
                None,
                cx,
            ));
        }
        if query.chars().count() < HOME_FOLDER_MIN_QUERY_CHARS
            || crate::pickers::is_typed_path(&query)
        {
            return;
        }
        search.loading = true;
        let generation = Some(search.generation);
        search.task = Some(request_home_folders(
            engine, device_id, local, query, generation, cx,
        ));
    }

    /// Tell the host the picker is done with its home index (it stays when a
    /// projectless chat warmed it).
    fn release_home_folder_search(&mut self, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let search = std::mem::take(&mut flow.home_search);
        flow.home_search.unsupported = search.unsupported;
        let (true, Some(engine)) = (search.started, engine) else {
            return;
        };
        let mut params = serde_json::Map::new();
        add_space_target(
            &mut params,
            flow.device.as_ref().map(|d| d.id.as_str()),
            local.as_deref(),
        );
        cx.spawn(async move |_, _| {
            if let Err(error) = engine
                .client()
                .call(
                    methods::RELEASE_HOME_FOLDER_SEARCH,
                    serde_json::Value::Object(params),
                )
                .await
            {
                tracing::debug!(%error, "home folder search release failed");
            }
        })
        .detach();
    }

    /// Selecting a device advances to its locations.
    fn add_space_pick_device(&mut self, device: Device, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = ProjectStep::Locations;
        flow.location = None;
        flow.load_task = None;
        flow.drives_task = None;
        flow.list_scroll.set_offset(gpui::Point::default());
        flow.device = Some(device);
        flow.browser = Loadable::Idle;
        flow.drives = Loadable::Idle;
        flow.browser_path = None;
        flow.home = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        flow.path_edit = None;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search locations…", cx);
            input.set_text("", cx);
        });
        self.load_space_drives(cx);
        cx.notify();
    }

    fn add_space_goto_location(
        &mut self,
        name: String,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if path.is_some() {
            self.release_home_folder_search(cx);
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = ProjectStep::Folders;
        flow.location = Some((name, path.clone()));
        flow.browser_repo = false;
        flow.path_edit = None;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders…", cx);
            input.set_text("", cx);
        });
        self.load_space_folders(path, cx);
        // Entering Home starts the host's index (an empty query only warms).
        self.search_home_folders(cx);
    }

    fn add_space_back_to(&mut self, step: ProjectStep, cx: &mut Context<Self>) {
        self.release_home_folder_search(cx);
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = step;
        flow.load_task = None;
        flow.browser = Loadable::Idle;
        flow.browser_path = None;
        flow.location = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        flow.path_edit = None;
        flow.list_scroll.set_offset(gpui::Point::default());
        if step == ProjectStep::Devices {
            flow.drives_task = None;
            flow.device = None;
            flow.drives = Loadable::Idle;
            flow.home = None;
        }
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder(
                if step == ProjectStep::Devices {
                    "Search devices…"
                } else {
                    "Search locations…"
                },
                cx,
            );
            input.set_text("", cx);
        });
        cx.notify();
    }

    fn add_space_devices(&self, cx: &App) -> Vec<Device> {
        let Some(flow) = &self.add_space else {
            return Vec::new();
        };
        let devices = &self.state.read(cx).devices;
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        popover::filter_indices(flow.search.read(cx).text(), &names)
            .into_iter()
            .map(|ix| devices[ix].clone())
            .collect()
    }

    fn add_space_locations(&self, cx: &App) -> Vec<(String, Option<String>)> {
        let Some(flow) = &self.add_space else {
            return Vec::new();
        };
        let locations: Vec<_> = std::iter::once(("Home".to_string(), None))
            .chain(
                flow.drives
                    .ready()
                    .into_iter()
                    .flatten()
                    .map(|d| (d.name.clone(), Some(d.path.clone()))),
            )
            .collect();
        let names: Vec<_> = locations.iter().map(|(name, _)| name.as_str()).collect();
        popover::filter_indices(flow.search.read(cx).text(), &names)
            .into_iter()
            .map(|ix| locations[ix].clone())
            .collect()
    }

    /// ListDrives on the flow's device (relay-forwarded when remote).
    /// Failures stay silent — the section just shows Home; the folder
    /// browser's own error row already covers "device didn't respond".
    fn load_space_drives(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        flow.drives = Loadable::Loading;
        flow.drives_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            // Only target remote devices — local calls skip the relay.
            if let (Some(target), local) = (&device_id, &local)
                && local.as_deref() != Some(target.as_str())
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_DRIVES, serde_json::Value::Object(params))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(flow) = shell.add_space.as_mut() {
                    flow.drives = match result {
                        Ok(value) => match serde_json::from_value::<DriveListing>(value) {
                            Ok(listing) => Loadable::Ready(listing.drives),
                            Err(err) => Loadable::Error(err.to_string()),
                        },
                        Err(err) => Loadable::Error(err.to_string()),
                    };
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The current listing's folder rows filtered by the search query
    /// (prefix matches first — `popover::filter_indices`).
    fn add_space_filtered(&self, cx: &App) -> Vec<slopty_view_proto::FolderEntry> {
        let Some(flow) = self.add_space.as_ref() else {
            return Vec::new();
        };
        if flow.step != ProjectStep::Folders {
            return Vec::new();
        }
        let Some(listing) = flow.browser.ready() else {
            return Vec::new();
        };
        let dirs = browser_rows(listing);
        let query = flow.search.read(cx).text().to_string();
        let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
        popover::filter_indices(&query, &names)
            .into_iter()
            .map(|ix| dirs[ix].clone())
            .collect()
    }

    /// The Folders step's rows: the open folder's matching subfolders, then
    /// matches from anywhere under home not already listed.
    fn add_space_folder_rows(&self, cx: &App) -> Vec<FolderRow> {
        let local = self.add_space_filtered(cx);
        let Some(flow) = self.add_space.as_ref() else {
            return Vec::new();
        };
        let open = flow.browser.ready().map(|listing| listing.path.as_str());
        let listed: HashSet<String> = open
            .map(|open| {
                local
                    .iter()
                    .map(|entry| crate::pickers::child_path(open, &entry.name))
                    .chain(std::iter::once(open.to_owned()))
                    .map(|path| folder_path_key(&path))
                    .collect()
            })
            .unwrap_or_default();
        let home = if self.add_space_in_home() {
            flow.home_search.results.as_slice()
        } else {
            &[]
        };
        // slopty: with nothing typed, the folders gone to most lead and the home's own follow.
        let gone_to = if self.add_space_in_home() {
            flow.home_search.visited.as_slice()
        } else {
            &[]
        };
        if flow.search.read(cx).text().trim().is_empty() && !gone_to.is_empty() {
            let keys: HashSet<String> =
                gone_to.iter().map(|found| folder_path_key(&found.path)).collect();
            let shown = |entry: &slopty_view_proto::FolderEntry| {
                open.is_none_or(|open| {
                    let path = crate::pickers::child_path(open, &entry.name);
                    !keys.contains(&folder_path_key(&path))
                })
            };
            return gone_to
                .iter()
                .cloned()
                .map(FolderRow::Home)
                .chain(local.into_iter().filter(|entry| shown(entry)).map(FolderRow::Local))
                .collect();
        }
        local
            .into_iter()
            .map(FolderRow::Local)
            .chain(
                home.iter()
                    .filter(|found| !listed.contains(&folder_path_key(&found.path)))
                    .cloned()
                    .map(FolderRow::Home),
            )
            .collect()
    }

    /// Descend into the highlighted (filtered) folder; clears the query.
    /// A path-shaped query with no matching rows browses the typed path
    /// instead — `/disk2⏎` must work, not sit on "No folders match" (an
    /// absolute query can never match a folder name anyway).
    fn add_space_open_active(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        match flow.step {
            ProjectStep::Devices => {
                if let Some(device) = self.add_space_devices(cx).get(flow.active).cloned() {
                    self.add_space_pick_device(device, cx);
                }
                return;
            }
            ProjectStep::Locations => {
                if let Some((name, path)) = self.add_space_locations(cx).get(flow.active).cloned() {
                    self.add_space_goto_location(name, path, cx);
                }
                return;
            }
            ProjectStep::Folders => {}
        }
        // slopty: the address is cloned; the row after the folders asks for one.
        if self.add_space.as_ref().is_some_and(|flow| flow.cloning) {
            self.add_space_clone(cx);
            return;
        }
        let rows = self.add_space_folder_rows(cx);
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if rows.is_empty() {
            let text = flow.search.read(cx).text().to_string();
            if let Some(target) = crate::pickers::typed_path_target(&text, flow.home.as_deref()) {
                self.add_space_descend(target, false, cx);
                return; // slopty: else the clone row, the only one
            }
        }
        if flow.active >= rows.len() {
            // slopty: "Clone repository…".
            self.add_space_begin_clone(cx);
            return;
        }
        let entry = match rows.get(flow.active) {
            Some(FolderRow::Local(entry)) => entry,
            Some(FolderRow::Home(found)) => {
                let (path, is_repo) = (found.path.clone(), found.is_repo);
                self.add_space_descend(path, is_repo, cx);
                return;
            }
            None => return,
        };
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let full = crate::pickers::child_path(&listing.path, &entry.name);
        let is_repo = entry.is_repo;
        let search = flow.search.clone();
        if let Some(flow) = self.add_space.as_mut() {
            flow.browser_repo = is_repo;
        }
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// Slash-descend: when the query ends in `/` and the part before it names
    /// a folder of the current listing (exact name — matching casing wins
    /// over a case-colliding sibling — else a unique prefix), descend into it
    /// as though it were picked. Returns whether it fired —
    /// descending clears the query, so the caller must not keep acting on the
    /// old text.
    fn add_space_slash_descend(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .add_space
            .as_ref()
            // slopty: an address's slashes are its own.
            .is_none_or(|f| f.step != ProjectStep::Folders || f.cloning)
        {
            return false;
        }
        // A typed PATH jump: an absolute (`/disk2/`), drive-rooted (`D:\x\`)
        // or home-relative (`~/x/`) query browses that path directly — mounts
        // at unconventional roots (and anywhere else) are reachable without a
        // Locations row. Same trailing-separator trigger as the folder-name
        // descend below.
        {
            let Some(flow) = self.add_space.as_ref() else {
                return false;
            };
            let text = flow.search.read(cx).text().to_string();
            if crate::pickers::is_typed_path(&text) && text.ends_with(['/', '\\']) {
                let target = crate::pickers::typed_path_target(&text, flow.home.as_deref());
                let Some(target) = target else {
                    // Path-shaped but unresolvable (`~/…` before home is
                    // known) — leave the query alone.
                    return false;
                };
                self.add_space_descend(target, false, cx);
                return true;
            }
        }
        let target = {
            let Some(flow) = self.add_space.as_ref() else {
                return false;
            };
            let text = flow.search.read(cx).text().to_string();
            let Some(query) = text.strip_suffix('/') else {
                return false;
            };
            if query.is_empty() || query.contains('/') {
                return false;
            }
            let Some(listing) = flow.browser.ready() else {
                return false;
            };
            let dirs = browser_rows(listing);
            let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
            crate::pickers::segment_target(&names, query).map(|ix| {
                (
                    crate::pickers::child_path(&listing.path, &dirs[ix].name),
                    dirs[ix].is_repo,
                )
            })
        };
        let Some((full, is_repo)) = target else {
            return false;
        };
        self.add_space_descend(full, is_repo, cx);
        true
    }

    /// The tab-completion target: the highlighted row when the query prefixes
    /// its name, else the first prefix match (filtering ranks those first).
    /// `(full name, remaining suffix)`; `None` on an empty query or when the
    /// match is already complete.
    fn add_space_completion(&self, cx: &App) -> Option<(String, String)> {
        let flow = self.add_space.as_ref()?;
        let query = flow.search.read(cx).text().to_string();
        if query.is_empty() {
            return None;
        }
        let rows = self.add_space_filtered(cx);
        let entry = rows
            .get(flow.active)
            .filter(|e| completion_prefix_len(&e.name, &query).is_some())
            .or_else(|| {
                rows.iter()
                    .find(|e| completion_prefix_len(&e.name, &query).is_some())
            })?;
        let len = completion_prefix_len(&entry.name, &query)?;
        if len >= entry.name.len() {
            return None;
        }
        Some((entry.name.clone(), entry.name[len..].to_string()))
    }

    /// ⇥: accept the completion — the query becomes the full folder name
    /// (the ghost the input was previewing). Descending stays on `/`/⏎.
    fn add_space_accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some((name, _)) = self.add_space_completion(cx) else {
            return;
        };
        if let Some(flow) = self.add_space.as_ref() {
            let search = flow.search.clone();
            search.update(cx, |input, cx| input.set_text(name, cx));
        }
    }

    /// Descend into a specific folder row (mouse path); clears the query.
    fn add_space_descend(&mut self, full: String, is_repo: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.browser_repo = is_repo;
        let search = flow.search.clone();
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// ListFolders on the flow's device (relay-forwarded when remote).
    pub(super) fn load_space_folders(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.path_edit = None;
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        let went_home = path.is_none();
        flow.browser_path = path.clone();
        flow.browser = Loadable::Loading;
        flow.active = 0;
        flow.list_scroll.set_offset(gpui::Point::default());
        let Some(engine) = engine else {
            flow.browser = Loadable::Error("Device is not connected".into());
            cx.notify();
            return;
        };
        flow.load_task = Some(cx.spawn(async move |this, cx| {
            let result = list_folders_on(engine, path, device_id, local).await;
            this.update(cx, |shell, cx| {
                if let Some(flow) = shell.add_space.as_mut() {
                    flow.browser = match result {
                        Ok(listing) => {
                            // A pathless browse resolved home — remember it
                            // so the breadcrumbs can fold it into the
                            // device crumb.
                            if went_home {
                                flow.home = Some(listing.path.clone());
                            }
                            Loadable::Ready(listing)
                        }
                        Err(error) => Loadable::Error(error),
                    };
                }
                // A path field opened while this loaded: ⇥ can use it now.
                shell.add_space_refresh_path_completion(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Create the space for the browser's current folder.
    fn submit_add_space(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if flow.submit_busy || flow.step != ProjectStep::Folders {
            return;
        }
        let Some(device) = flow.device.clone() else {
            return;
        };
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let path = listing.path.clone();
        let git_detected = flow.browser_repo;
        // Same (device, folder) already has a space → just switch to it. The
        // engine dedupes this case too (a createSpace for a duplicate pair
        // no-ops), so creating would leave the minted id dangling.
        if let Some(existing) = self
            .state
            .read(cx)
            .spaces
            .iter()
            .find(|s| s.device_id == device.id && same_folder_path(&s.path, &path))
            .map(|s| s.id.clone())
        {
            self.close_add_space(cx);
            self.land_in_space(existing, cx);
            return;
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.submit_busy = true;
        flow.error = None;
        let space_id = uuid::Uuid::new_v4().to_string();
        // Optimistic echo: the watch frame carrying the real row replaces it
        // by id (apply_spaces re-sorts; same-id upsert is idempotent).
        let space = Space {
            id: space_id.clone(),
            device_id: device.id.clone(),
            path: path.clone(),
            name: None,
            git_detected,
            git_checked_at: None,
            checkout_id: None,
            repository_id: None,
            created_at: crate::clock::utc(cx),
        };
        self.state.update(cx, |s, cx| {
            if !s.spaces.iter().any(|existing| existing.id == space.id) {
                s.spaces.push(space);
            }
            cx.notify();
        });
        let params = serde_json::json!({
            "op": "createSpace",
            "spaceId": space_id,
            "deviceId": device.id,
            "path": path,
            "gitDetected": git_detected,
        });
        let submit_id = space_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.close_add_space(cx);
                        shell.land_in_space(submit_id.clone(), cx);
                    }
                    Err(err) => {
                        // Roll the optimistic row back; surface the error inline.
                        shell.state.update(cx, |s, cx| {
                            s.spaces.retain(|space| space.id != submit_id);
                            cx.notify();
                        });
                        if let Some(flow) = shell.add_space.as_mut() {
                            flow.submit_busy = false;
                            flow.error = Some(format!("{err}").into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.add_space.as_mut() {
            flow.submit_task = Some(task);
        }
        cx.notify();
    }

    // slopty: "Clone repository…": the search field takes a repository's address, and the clone
    // goes in the open folder on the picked machine (`CloneRepo`), then is added as a project.
    /// Let the search field take an address; one already typed there stays.
    fn add_space_begin_clone(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.cloning = true;
        flow.active = 0;
        flow.error = None;
        flow.focus_pending = true;
        flow.list_scroll.set_offset(gpui::Point::default());
        let search = flow.search.clone();
        let keep = looks_like_repo_url(search.read(cx).text().trim());
        search.update(cx, |input, cx| {
            input.set_placeholder("Repository address or path…", cx);
            if !keep {
                input.set_text("", cx);
            }
        });
        // Not in Home any more: its search lets go.
        self.search_home_folders(cx);
        cx.notify();
    }

    /// Back from the address to the folders.
    fn add_space_end_clone(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.cloning = false;
        flow.active = 0;
        flow.focus_pending = true;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders…", cx);
            input.set_text("", cx);
        });
        self.search_home_folders(cx);
        cx.notify();
    }

    /// Where the typed address would be cloned: its name and the folder it makes.
    fn add_space_clone_target(&self, cx: &App) -> Option<(String, String)> {
        let flow = self.add_space.as_ref().filter(|flow| flow.cloning)?;
        let url = flow.search.read(cx).text().trim().to_owned();
        let name = clone_repo_name(&url).filter(|_| looks_like_repo_url(&url))?;
        let into = crate::pickers::child_path(&flow.browser.ready()?.path, &name);
        Some((name, into))
    }

    /// Clone the typed address into the open folder on the picked machine. The picker closes
    /// at once; a notice says it is under way, and the clone becomes a project, or says why
    /// not.
    fn add_space_clone(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let Some((name, into)) = self.add_space_clone_target(cx) else {
            return;
        };
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        let Some(device) = flow.device.clone() else {
            return;
        };
        let url = flow.search.read(cx).text().trim().to_owned();
        let Some(parent) = flow.browser.ready().map(|folder| folder.path.clone()) else {
            return;
        };
        let mut params = serde_json::Map::new();
        params.insert("deviceId".into(), device.id.clone().into());
        params.insert("url".into(), url.into());
        params.insert("parentPath".into(), parent.into());
        add_space_target(&mut params, Some(device.id.as_str()), local.as_deref());
        self.close_add_space(cx);
        let notice = crate::toast::working(
            cx,
            format!("Cloning {name} on {}", device.name),
            CLONE_NOTICE_LASTS,
        );
        cx.spawn(async move |this, cx| {
            let answer = engine
                .client()
                .call(methods::CLONE_REPO, serde_json::Value::Object(params))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(notice) = notice {
                    crate::toast::withdraw(cx, notice);
                }
                match answer {
                    Ok(answer) => {
                        match cloned_space(&answer) {
                            Some((space, device, path)) => {
                                shell.adopt_cloned_space(space, device, path, cx);
                            }
                            None => crate::toast::error(
                                cx,
                                format!("Cloned {name} into {into}, but Slopty couldn\u{2019}t open it as a project. Add the folder from New project."),
                            ),
                        }
                    }
                    Err(error) => {
                        crate::toast::error_with_detail(
                            cx,
                            format!("Couldn\u{2019}t clone {name}. Check the address and your access, then try again."),
                            error.to_string(),
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Land in the project a clone became, which the clone already added: known at once,
    /// before the list that brings it arrives.
    fn adopt_cloned_space(
        &mut self,
        space_id: String,
        device_id: String,
        path: String,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |s, cx| {
            if !s.spaces.iter().any(|existing| existing.id == space_id) {
                s.spaces.push(Space {
                    id: space_id.clone(),
                    device_id,
                    path,
                    name: None,
                    git_detected: true,
                    git_checked_at: None,
                    checkout_id: None,
                    repository_id: None,
                    created_at: crate::clock::utc(cx),
                });
            }
            cx.notify();
        });
        self.land_in_space(space_id, cx);
    }

    /// Back traverses folders, then locations, then devices.
    fn add_space_go_up(&mut self, cx: &mut Context<Self>) {
        // slopty: out of the address, back to the folder it would go in.
        if self.add_space.as_ref().is_some_and(|flow| flow.cloning) {
            self.add_space_end_clone(cx);
            return;
        }
        let Some(flow) = &self.add_space else {
            return;
        };
        match flow.step {
            ProjectStep::Devices => return,
            ProjectStep::Locations => self.add_space_back_to(ProjectStep::Devices, cx),
            ProjectStep::Folders => {
                let listing = flow.browser.ready();
                let root = flow
                    .location
                    .as_ref()
                    .and_then(|(_, path)| path.as_deref())
                    .or(flow.home.as_deref());
                let parent = listing
                    .filter(|l| !root.is_some_and(|root| same_folder_path(&l.path, root)))
                    .and_then(|l| parent_path(&l.path));
                if let Some(parent) = parent {
                    self.add_space_descend(parent, false, cx);
                } else {
                    self.add_space_back_to(ProjectStep::Locations, cx);
                }
            }
        }
    }

    // ---- typed path: the crumb row as an address bar ----

    /// Whether the device's paths are Windows-shaped (`\` separates them):
    /// what it listed says so; before that, its platform.
    fn add_space_windows_paths(&self) -> bool {
        self.add_space.as_ref().is_some_and(|flow| {
            flow.open_folder().map_or_else(
                || {
                    flow.device
                        .as_ref()
                        .is_some_and(|device| device.platform == "windows")
                },
                |path| is_windows_path(&path),
            )
        })
    }

    /// ⌘L, the pen, or a click beside the crumbs: the trail becomes the open
    /// folder's path, selected — a paste replaces it, → edits its end. On
    /// the device list it types a path on the highlighted device; ⌘L in the
    /// field selects the path again.
    pub(super) fn add_space_edit_path(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if flow.step == ProjectStep::Devices {
            let Some(device) = self.add_space_devices(cx).get(flow.active).cloned() else {
                return;
            };
            // slopty: no Cloud device, so every device takes a path.
            self.add_space_pick_device(device, cx);
        }
        let windows = self.add_space_windows_paths();
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if !matches!(flow.step, ProjectStep::Locations | ProjectStep::Folders) {
            return;
        }
        if let Some(edit) = flow.path_edit.as_mut() {
            edit.input.update(cx, |input, cx| input.select_all_text(cx));
            edit.focus_pending = true;
            cx.notify();
            return;
        }
        // The open folder, spelled from `~` under home and ending in a
        // separator so a name typed after it (→ first) goes inside.
        let mut text = flow.open_folder().map_or_else(
            || "~".to_string(),
            |path| escape_typed_path(&tilde_path(&path, flow.home.as_deref()), windows),
        );
        if !text.ends_with(['/', '\\']) {
            text.push(if windows { '\\' } else { '/' });
        }
        let input = cx.new(|cx| {
            ComposerInput::new(
                if windows {
                    r"Folder path, like C:\code"
                } else {
                    "Folder path, like ~/code"
                },
                cx,
            )
            .with_text_metrics(12.0, 16.0)
            .with_single_line()
            .with_accessibility_role(gpui::Role::TextInput)
        });
        input.update(cx, |input, cx| {
            input.set_text(text, cx);
            input.select_all_text(cx);
        });
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| match event {
            ComposerInputEvent::Edited => this.add_space_path_edited(cx),
            ComposerInputEvent::Submitted => this.add_space_open_typed_path(false, cx),
            // slopty: the field takes no pasted files (Slopty's input pastes text only).
            _ => {}
        });
        // `~` and ⇥ under home need the device's home: list it once.
        let home_task = match (flow.home.is_none(), engine) {
            (true, Some(engine)) => {
                let device_id = flow.device.as_ref().map(|device| device.id.clone());
                Some(cx.spawn(async move |this, cx| {
                    let listing = list_folders_on(engine, None, device_id, local).await;
                    this.update(cx, |shell, cx| {
                        let (Ok(listing), Some(flow)) = (listing, shell.add_space.as_mut()) else {
                            return;
                        };
                        // Home's folders are ⇥'s source for `~/…`, unless it
                        // already lists another folder.
                        let key = folder_path_key(&listing.path);
                        if let Some(edit) = flow.path_edit.as_mut()
                            && edit
                                .siblings
                                .as_ref()
                                .is_none_or(|(known, _)| *known == key)
                        {
                            let folders = browser_rows(&listing).into_iter().cloned().collect();
                            edit.siblings = Some((key, Loadable::Ready(folders)));
                            edit.siblings_task = None;
                        }
                        flow.home.get_or_insert(listing.path);
                        // Whichever listing learned home first, `~` resolves now.
                        shell.add_space_refresh_path_completion(cx);
                        cx.notify();
                    })
                    .ok();
                }))
            }
            _ => None,
        };
        flow.path_edit = Some(PathEdit {
            input,
            opening: false,
            adding: false,
            error: None,
            siblings: None,
            focus_pending: true,
            open_task: None,
            siblings_task: None,
            _home_task: home_task,
            _events: events,
        });
        // The field, not the search, takes the keyboard.
        flow.focus_pending = false;
        self.add_space_refresh_path_completion(cx);
        cx.notify();
    }

    /// Esc in the path field: the crumbs again, the open folder untouched.
    /// `false` when no path field is open (Esc then closes the palette).
    pub(super) fn add_space_cancel_path_edit(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(flow) = self.add_space.as_mut() else {
            return false;
        };
        if flow.path_edit.take().is_none() {
            return false;
        }
        flow.focus_pending = true;
        cx.notify();
        true
    }

    /// The path field's text changed: a fresh attempt (an answer for the
    /// old text must not open it), and ⇥ follows the folder being typed in.
    fn add_space_path_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self
            .add_space
            .as_mut()
            .and_then(|flow| flow.path_edit.as_mut())
        {
            edit.error = None;
            edit.opening = false;
            edit.open_task = None;
        }
        self.add_space_refresh_path_completion(cx);
        cx.notify();
    }

    /// Keep ⇥'s source on the typed path's parent folder: the open listing
    /// answers for itself; any other folder is listed once per parent.
    fn add_space_refresh_path_completion(&mut self, cx: &mut Context<Self>) {
        let windows = self.add_space_windows_paths();
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let open = flow.browser.ready();
        let base = flow.open_folder();
        let device_id = flow.device.as_ref().map(|device| device.id.clone());
        let Some(edit) = flow.path_edit.as_mut() else {
            return;
        };
        let text = edit.input.read(cx).text().to_owned();
        let (folder, _) = split_typed_leaf(&text, windows);
        let parent = if folder.is_empty() {
            base
        } else {
            resolve_typed_path(folder, base.as_deref(), flow.home.as_deref(), windows)
        };
        let Some(parent) = parent else {
            edit.siblings = None;
            edit.siblings_task = None;
            return;
        };
        let key = folder_path_key(&parent);
        if edit
            .siblings
            .as_ref()
            .is_some_and(|(known, _)| *known == key)
        {
            return;
        }
        if let Some(listing) = open.filter(|listing| same_folder_path(&listing.path, &parent)) {
            let folders = browser_rows(listing).into_iter().cloned().collect();
            edit.siblings = Some((key, Loadable::Ready(folders)));
            edit.siblings_task = None;
            return;
        }
        let Some(engine) = engine else {
            edit.siblings = None;
            return;
        };
        edit.siblings = Some((key.clone(), Loadable::Loading));
        edit.siblings_task = Some(cx.spawn(async move |this, cx| {
            let listing = list_folders_on(engine, Some(parent), device_id, local).await;
            this.update(cx, |shell, cx| {
                if let Some((known, folders)) = shell
                    .add_space
                    .as_mut()
                    .and_then(|flow| flow.path_edit.as_mut())
                    .and_then(|edit| edit.siblings.as_mut())
                    && *known == key
                {
                    *folders = match listing {
                        Ok(listing) => {
                            Loadable::Ready(browser_rows(&listing).into_iter().cloned().collect())
                        }
                        Err(error) => Loadable::Error(error),
                    };
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// ⇥ in the path field: the name being typed completed to the folder
    /// of its parent it names exactly, else the first it prefixes
    /// (case-insensitively), separator appended — `(text after ⇥, the faint
    /// suffix previewing it)`.
    fn add_space_path_completion(&self, cx: &App) -> Option<(String, Option<SharedString>)> {
        let windows = self.add_space_windows_paths();
        let edit = self.add_space.as_ref()?.path_edit.as_ref()?;
        let Some((_, Loadable::Ready(folders))) = &edit.siblings else {
            return None;
        };
        let text = edit.input.read(cx).text();
        let (folder, name) = split_typed_leaf(text, windows);
        let name = unescape_typed_path(name, windows);
        if matches!(name.as_str(), "" | "." | "..") {
            return None;
        }
        // `code` stays `code` beside a `Code` listed first.
        let (entry, typed) = folders
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| (entry, entry.name.len()))
            .or_else(|| {
                folders
                    .iter()
                    .find_map(|entry| Some((entry, completion_prefix_len(&entry.name, &name)?)))
            })?;
        let separator = match folder.chars().last() {
            Some(separator @ ('/' | '\\')) => separator,
            _ if windows => '\\',
            _ => '/',
        };
        let ghost =
            (typed < entry.name.len()).then(|| SharedString::from(entry.name[typed..].to_string()));
        let entry_name = escape_typed_path(&entry.name, windows);
        Some((format!("{folder}{entry_name}{separator}"), ghost))
    }

    fn add_space_accept_path_completion(&mut self, cx: &mut Context<Self>) {
        let Some((text, _)) = self.add_space_path_completion(cx) else {
            return;
        };
        if let Some(edit) = self
            .add_space
            .as_ref()
            .and_then(|flow| flow.path_edit.as_ref())
        {
            let input = edit.input.clone();
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    /// ↵ (⌘↵ with `add`) in the path field: list the typed path on the
    /// device; once it answers it is the open folder — and with ⌘↵ the new
    /// project. A path that doesn't list keeps the field and says why.
    fn add_space_open_typed_path(&mut self, add: bool, cx: &mut Context<Self>) {
        let windows = self.add_space_windows_paths();
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.submit_busy {
            return;
        }
        let base = flow.open_folder();
        let home = flow.home.clone();
        let device_id = flow.device.as_ref().map(|device| device.id.clone());
        let Some(edit) = flow.path_edit.as_mut() else {
            return;
        };
        // This text is already on its way (an edit would have stopped it):
        // ⌘↵ adds it when it lands, ↵ doesn't start it over.
        if edit.opening {
            edit.adding |= add;
            return;
        }
        let text = edit.input.read(cx).text().to_owned();
        let engine = match engine {
            Some(engine) if !text.trim().is_empty() => engine,
            engine => {
                edit.error = Some(if engine.is_none() {
                    "Device is not connected".into()
                } else {
                    "Type the folder's path".into()
                });
                cx.notify();
                return;
            }
        };
        edit.opening = true;
        edit.adding = add;
        edit.error = None;
        edit.open_task = Some(cx.spawn(async move |this, cx| {
            let mut home = home;
            let result: Result<FolderListing, String> = async {
                let resolve = |home: &Option<String>| {
                    let base = base.as_deref().or(home.as_deref());
                    resolve_typed_path(&text, base, home.as_deref(), windows)
                };
                let unresolved = || format!("Can't open {}", text.trim());
                let target = match resolve(&home) {
                    Some(target) => target,
                    // `~`, or a relative path with nothing open: learn the
                    // device's home first.
                    None if home.is_none() => {
                        let found =
                            list_folders_on(engine.clone(), None, device_id.clone(), local.clone())
                                .await?;
                        home = Some(found.path);
                        resolve(&home).ok_or_else(unresolved)?
                    }
                    None => return Err(unresolved()),
                };
                let listed = if home.is_none() {
                    // Home is learned alongside, so a path in it lands under
                    // Home rather than under its drive.
                    let (found, listed) = futures::join!(
                        list_folders_on(engine.clone(), None, device_id.clone(), local.clone()),
                        list_folders_on(engine, Some(target.clone()), device_id, local),
                    );
                    home = found.ok().map(|found| found.path);
                    listed
                } else {
                    list_folders_on(engine, Some(target.clone()), device_id, local).await
                };
                listed
                    .map_err(|error| path_open_error(&tilde_path(&target, home.as_deref()), &error))
            }
            .await;
            this.update(cx, |shell, cx| {
                shell.add_space_finish_typed_path(home, result, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    /// The typed path's answer: its listing becomes the open folder under
    /// the location holding it (and the new project, for ⌘↵); an error
    /// stays in the field.
    fn add_space_finish_typed_path(
        &mut self,
        home: Option<String>,
        result: Result<FolderListing, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        // Home learned on the way stays learned, whatever the answer.
        if flow.home.is_none() {
            flow.home = home;
        }
        let Some(edit) = flow.path_edit.as_mut() else {
            return;
        };
        edit.opening = false;
        let add = std::mem::take(&mut edit.adding);
        let listing = match result {
            Ok(listing) => listing,
            Err(error) => {
                edit.error = Some(error.into());
                edit.focus_pending = true;
                cx.notify();
                return;
            }
        };
        let location = typed_path_location(
            &listing.path,
            flow.home.as_deref(),
            flow.drives.ready().map(Vec::as_slice).unwrap_or_default(),
        );
        // Outside Home its folder index goes, as when picking a drive.
        let leaves_home = location.1.is_some();
        // The open folder's git seed holds while that folder stays open.
        flow.browser_repo &= flow
            .browser
            .ready()
            .is_some_and(|open| same_folder_path(&open.path, &listing.path));
        flow.path_edit = None;
        flow.focus_pending = true;
        flow.step = ProjectStep::Folders;
        flow.location = Some(location);
        flow.browser_path = Some(listing.path.clone());
        flow.browser = Loadable::Ready(listing);
        flow.load_task = None;
        flow.active = 0;
        flow.error = None;
        flow.list_scroll.set_offset(gpui::Point::default());
        let search = flow.search.clone();
        if leaves_home {
            self.release_home_folder_search(cx);
        }
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders…", cx);
            // Clearing the query re-runs the Home search, warming it on
            // entry — not worth it when ⌘↵ is about to close the palette.
            if !(add && input.is_empty()) {
                input.set_text("", cx);
            }
        });
        if add {
            self.submit_add_space(cx);
        }
        cx.notify();
    }

    /// Palette keys (bubbling from the focused search input) — every legend
    /// maps to a REAL key: ↑↓ (or ctrl-n/p) navigate, →/⏎ open the
    /// highlighted folder, ← up a level, ⇥ completes the query to the
    /// previewed folder name, ⌘⏎ add the OPEN folder, ⌫ (empty query) also
    /// goes up, esc closes, ⌘L types the path. (Typing `/` also descends —
    /// see the Edited subscription.) While the path field is open it edits
    /// its own text; ⇥ completes it, ⌘⏎ opens and adds it, esc puts the
    /// crumbs back.
    fn add_space_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        // The address-bar chord.
        if keystroke.key == "l" && keystroke.modifiers == gpui::Modifiers::secondary_key(cx) {
            self.add_space_edit_path(cx);
            cx.stop_propagation();
            return;
        }
        if self
            .add_space
            .as_ref()
            .is_some_and(|flow| flow.path_edit.is_some())
        {
            match keystroke.key.as_str() {
                "tab" if !keystroke.modifiers.modified() => {
                    self.add_space_accept_path_completion(cx)
                }
                // Not the Shell's focus walk: the keyboard stays in the field.
                "tab" if keystroke.modifiers.shift => {}
                // Only on Settings, where the Shell's Esc capture stands aside.
                "escape" => {
                    self.add_space_cancel_path_edit(cx);
                }
                "enter" if keystroke.modifiers == gpui::Modifiers::secondary_key(cx) => {
                    self.add_space_open_typed_path(true, cx)
                }
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        // ←/→ act on the FOLDERS, not the text cursor — the palette is a
        // navigator first; queries are short and edited with ⌫.
        match event.keystroke.key.as_str() {
            "right" => {
                self.add_space_open_active(cx);
                return;
            }
            "left" => {
                self.add_space_go_up(cx);
                return;
            }
            // Unbound in "PaletteSearch" (like enter), so it bubbles here
            // instead of editing text — and stops here, short of the
            // Shell's focus walk.
            "tab" => {
                self.add_space_accept_completion(cx);
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_add_space(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = match self.add_space.as_ref().map(|f| f.step) {
                    Some(ProjectStep::Devices) => self.add_space_devices(cx).len(),
                    Some(ProjectStep::Locations) => self.add_space_locations(cx).len(),
                    // slopty: the clone row after the folders, or the one clone row.
                    _ if self.add_space.as_ref().is_some_and(|f| f.cloning) => 1,
                    _ => self.add_space_folder_rows(cx).len() + 1,
                };
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(flow) = self.add_space.as_mut() {
                    flow.active = popover::menu_step(Some(flow.active), count, delta).unwrap_or(0);
                    // Keep the highlighted row in view as the cursor walks
                    // past the viewport (user-reported: the list didn't
                    // follow the keyboard).
                    flow.list_scroll.scroll_to_item(flow.active);
                    cx.notify();
                }
            }
            // ⏎ opens the highlighted folder (an alias for →); the space is
            // added with ⌘⏎ — and the chord acts on the folder OPEN in the
            // breadcrumbs, not the highlight. The highlight auto-rests on the
            // first row, so a chord that took it would add arbitrary
            // subfolders; the usual target (a repo root full of subfolders)
            // is only ever "the folder you're standing in".
            popover::MenuKey::Enter => self.add_space_open_active(cx),
            // slopty: while an address is typed, ⌘⏎ clones it as ⏎ does.
            popover::MenuKey::ModEnter if self.add_space.as_ref().is_some_and(|f| f.cloning) => {
                self.add_space_clone(cx)
            }
            popover::MenuKey::ModEnter => self.submit_add_space(cx),
            popover::MenuKey::Backspace => {
                let empty = self
                    .add_space
                    .as_ref()
                    .is_some_and(|f| f.search.read(cx).is_empty());
                if empty {
                    self.add_space_go_up(cx);
                }
            }
            popover::MenuKey::Other => {}
        }
    }

    pub(super) fn close_project_crumb_menu(&mut self, cx: &mut Context<Self>) {
        if self.project_crumb_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.project_crumb_menu);
            cx.notify();
        }
    }

    /// The folders folded into the `…` crumb, in path order.
    fn render_project_crumb_menu(
        &self,
        hidden: &[(String, String)],
        closing: Option<crate::popover::Closing>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = hidden.iter().enumerate().map(|(ix, (name, full))| {
            let full = full.clone();
            popover::menu_row(theme, false, format!("project-crumb-menu-{ix}"))
                .id(("project-crumb-menu", ix))
                .child(
                    icon(icons::FOLDER)
                        .size(px(16.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(div().min_w_0().truncate().child(name.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.project_crumb_menu = popover::Popup::default();
                    this.add_space_descend(full.clone(), false, cx);
                }))
        });
        let card = popover::popover_card(theme)
            .id("project-crumb-menu")
            .min_w(px(180.0))
            .max_w(px(280.0))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_project_crumb_menu(cx)))
            .children(rows);
        popover::anchored_menu_below_layer(
            "project-crumb-menu",
            card.into_any_element(),
            closing,
            6.0,
            3,
        )
    }

    /// The same glass, header, row rhythm, scroll gutters and footer as Cmd+K.
    pub(super) fn render_add_space_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let flow = self.add_space.as_mut()?;
        if let Some(edit) = flow.path_edit.as_mut() {
            // The path field keeps the keyboard while it is open: a click on
            // the card's chrome hands focus back to it, while focusing the
            // search field puts the crumbs back.
            let field = edit.input.focus_handle(cx);
            if std::mem::take(&mut edit.focus_pending)
                || window
                    .focused(cx)
                    .is_none_or(|focused| focused == flow.focus)
            {
                window.focus(&field, cx);
            } else if flow.search.focus_handle(cx).is_focused(window) {
                flow.path_edit = None;
            }
            flow.focus_pending = false;
        } else if std::mem::take(&mut flow.focus_pending) {
            let search = flow.search.focus_handle(cx);
            if flow.return_focus.is_none() {
                flow.return_focus = window.focused(cx).filter(|before| *before != search);
            }
            window.focus(&search, cx);
        }
        let step = flow.step;
        let search = flow.search.clone();
        let focus = flow.focus.clone();
        let scroll = flow.list_scroll.clone();
        let device = flow.device.clone();
        let listing = flow.browser.ready().cloned();
        let location = flow.location.clone();
        let home = flow.home.clone();
        let browser_path = flow.browser_path.clone();
        let load_error = flow.browser.error().map(str::to_string);
        let error = flow.error.clone();
        let busy = flow.submit_busy;
        let active = flow.active;
        let loading = matches!(flow.browser, Loadable::Idle | Loadable::Loading);
        let cloning = flow.cloning; // slopty
        let drives_loading = matches!(flow.drives, Loadable::Loading);
        let home_loading = flow.home_search.loading;
        let home_status = if flow.home_search.indexing {
            Some("Indexing…")
        } else if home_loading {
            Some("Searching…")
        } else {
            None
        };
        let path_field = flow
            .path_edit
            .as_ref()
            .map(|edit| (edit.input.clone(), edit.opening, edit.error.clone()));
        let ghost = self
            .add_space_completion(cx)
            .map(|(_, suffix)| SharedString::from(suffix));
        search.update(cx, |input, cx| {
            input.set_ghost(ghost, cx);
        });
        if let Some((input, ..)) = &path_field {
            let ghost = self
                .add_space_path_completion(cx)
                .and_then(|(_, ghost)| ghost);
            input.update(cx, |input, cx| input.set_ghost(ghost, cx));
        }
        // slopty: every device's crumbs are a path (no Cloud).
        let can_edit_path = matches!(step, ProjectStep::Locations | ProjectStep::Folders);
        let query = search.read(cx).text().to_string();
        // Cmd+K's row rhythm: 30px rows, 16px muted glyphs, 8px list gutters.
        let row = |ix: usize| {
            popover::menu_row(&theme, ix == active, format!("project-result-{ix}"))
                .id(("project-result", ix))
                .rounded(px(popover::PALETTE_ITEM_RADIUS))
                .min_h(px(30.0))
                .py(px(4.0))
                // Pointer motion moves the highlight, so hover and keyboard
                // never light two rows. Motion only: rows scrolling under a
                // resting pointer must not steal the keyboard's place.
                .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                    if let Some(flow) = this.add_space.as_mut()
                        && flow.active != ix
                    {
                        flow.active = ix;
                        cx.notify();
                    }
                }))
        };
        let glyph_el = |glyph: &'static str| {
            icon(glyph)
                .size(px(16.0))
                .flex_none()
                .text_color(theme.text_muted)
        };
        let label_el = |label: String| {
            div().flex_1().min_w_0().child(popover::search_highlight(
                label.into(),
                Some(&query),
                &theme,
            ))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        // slopty: the clone row after the folders, not one the empty state counts.
        let mut clone_rows = 0;
        match step {
            ProjectStep::Devices => {
                for (ix, device) in self.add_space_devices(cx).into_iter().enumerate() {
                    let online = self.state.read(cx).device_online(&device.id, crate::clock::utc(cx));
                    let name = device.name.clone();
                    // slopty: a screen reader reads each row as the button it is, the dot's
                    // state in words.
                    let said = format!("{name}, {}", if online { "online" } else { "offline" });
                    rows.push(
                        row(ix)
                            .role(gpui::Role::Button)
                            .aria_label(SharedString::from(said))
                            .child(glyph_el(device_glyph(&device.platform)))
                            .child(label_el(name))
                            .child(
                                div()
                                    .size(px(5.0))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(if online {
                                        theme.success
                                    } else {
                                        theme.text_faint
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_space_pick_device(device.clone(), cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
            ProjectStep::Locations => {
                for (ix, (name, path)) in self.add_space_locations(cx).into_iter().enumerate() {
                    let glyph = if path.is_none() {
                        icons::HOME
                    } else {
                        icons::HARD_DRIVE
                    };
                    let label = name.clone();
                    rows.push(
                        row(ix)
                            .role(gpui::Role::Button) // slopty: read as a button
                            .aria_label(SharedString::from(label.clone()))
                            .child(glyph_el(glyph))
                            .child(label_el(label))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_space_goto_location(name.clone(), path.clone(), cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
            // slopty: an address typed to clone: the clone it makes, or what to type.
            ProjectStep::Folders if cloning => {
                clone_rows = 1;
                if let Some((name, into)) = self.add_space_clone_target(cx) {
                    let shown = home
                        .as_deref()
                        .and_then(|home| into.strip_prefix(home.trim_end_matches('/')))
                        .filter(|rest| rest.starts_with('/'))
                        .map_or_else(|| into.clone(), |rest| format!("~{rest}"));
                    rows.push(
                        row(0)
                            .role(gpui::Role::Button)
                            .aria_label(SharedString::from(format!("Clone {name}")))
                            .child(glyph_el(icons::CLOUD))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(div().flex_none().child(format!("Clone {name}")))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.text_muted)
                                            .child(format!("into {shown}")),
                                    ),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.add_space_clone(cx)))
                            .into_any_element(),
                    );
                } else {
                    rows.push(
                        div()
                            .px(px(8.0))
                            .py(px(6.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(if query.trim().is_empty() {
                                "Paste the repository's address: https://… or git@host:owner/repo.git."
                            } else {
                                "Not an address git can clone from."
                            })
                            .into_any_element(),
                    );
                }
            }
            ProjectStep::Folders => {
                if !loading && load_error.is_none() {
                    let base = listing.as_ref().map(|l| l.path.as_str()).unwrap_or("");
                    let mut first_home = true;
                    for (ix, folder) in self.add_space_folder_rows(cx).into_iter().enumerate() {
                        let entry = match folder {
                            FolderRow::Local(entry) => entry,
                            FolderRow::Home(found) => {
                                let el = home_folder_row(row(ix), &found, &query, &theme, cx);
                                rows.push(if std::mem::take(&mut first_home) {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .when(ix > 0, |el| el.pt(px(6.0)))
                                        .child(home_folders_header(&theme, home_status))
                                        .child(el)
                                        .into_any_element()
                                } else {
                                    el.into_any_element()
                                });
                                continue;
                            }
                        };
                        let full = crate::pickers::child_path(base, &entry.name);
                        let is_repo = entry.is_repo;
                        rows.push(
                            row(ix)
                                .role(gpui::Role::Button) // slopty: read as a button
                                .aria_label(SharedString::from(entry.name.clone()))
                                .child(glyph_el(icons::FOLDER))
                                .child(label_el(entry.name))
                                .when(is_repo, |el| {
                                    el.child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(14.0))
                                            .flex_none()
                                            .text_color(theme.text_muted),
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_space_descend(full.clone(), is_repo, cx)
                                }))
                                .into_any_element(),
                        );
                    }
                    // slopty: a repository from elsewhere, cloned into this folder.
                    let ix = rows.len();
                    clone_rows = 1;
                    rows.push(
                        row(ix)
                            .role(gpui::Role::Button)
                            .aria_label("Clone repository…")
                            .child(glyph_el(icons::CLOUD))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(theme.text_muted)
                                    .child("Clone repository…"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.add_space_begin_clone(cx)))
                            .into_any_element(),
                    );
                }
            }
        }
        if let Some(flow) = self.add_space.as_mut() {
            flow.active = flow.active.min(rows.len().saturating_sub(1));
        }
        let count = rows.len();
        // End spacing belongs to the content, so it scrolls out of the fade
        // instead of leaving a permanent gutter beside the chrome (Cmd+K).
        let rows = rows.into_iter().enumerate().map(|(ix, content)| {
            div()
                .flex_none()
                .px(px(8.0))
                .when(ix == 0, |row| row.pt(px(8.0)))
                .when(ix + 1 == count, |row| row.pb(px(8.0)))
                .child(content)
        });
        let mut results = div()
            .id("project-results")
            .min_h_0()
            .max_h(px(command_palette::palette_results_height(viewport)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .children(rows);
        if step == ProjectStep::Folders && loading {
            results = results.child(div().p(px(8.0)).child(popover::skeleton_rows(
                "project-loading",
                &theme,
                5,
                cx.entity_id(),
                cx,
            )));
        } else if let Some(message) = load_error.filter(|_| step == ProjectStep::Folders) {
            results = results.child(
                popover::error_row(&theme, &message).p(px(16.0)).child(
                    popover::btn_ghost(&theme, "Retry", "project-retry")
                        .id("project-retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.add_space.as_ref().and_then(|f| f.browser_path.clone());
                            this.load_space_folders(path, cx);
                        })),
                ),
            );
        } else if count == clone_rows // slopty: the clone row is no folder
            && !cloning
            && !(step == ProjectStep::Locations && drives_loading)
        {
            let (title, hint) = match step {
                ProjectStep::Devices => ("No devices found", "Try another device name.".into()),
                ProjectStep::Locations => {
                    ("No locations found", "Try Home or a drive name.".into())
                }
                // In the path field ⌘↵ adds the TYPED path and ← moves the caret.
                ProjectStep::Folders if query.is_empty() && path_field.is_some() => (
                    "No folders here",
                    "Type another path, or press Esc for the folder trail.".to_string(),
                ),
                ProjectStep::Folders if query.is_empty() => (
                    "No folders here",
                    format!(
                        "Add this folder with {}, or go back with ←.",
                        crate::keys::combo_text("mod-enter")
                    ),
                ),
                ProjectStep::Folders if home_loading => (
                    "Searching Home…",
                    "Looking for matching folders anywhere under Home.".to_string(),
                ),
                ProjectStep::Folders if home_status == Some("Indexing…") => (
                    "No folders match yet",
                    "Home is still being indexed — keep typing or try again.".to_string(),
                ),
                ProjectStep::Folders => (
                    "No folders match",
                    "Type a path like ~/code or /mnt to jump there.".to_string(),
                ),
            };
            results = results.child(command_palette::palette_empty(&theme, title, hint));
        }
        if step == ProjectStep::Locations && drives_loading {
            results = results.child(
                div()
                    .px(px(16.0))
                    .pb(px(8.0))
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(11.0))
                    .child("Loading locations…"),
            );
        }
        let results = command_palette::palette_results_fade(results, &scroll);

        // Breadcrumbs: one line that scrolls sideways under edge fades rather
        // than wrapping, and follows the open folder as the path grows. Deep
        // paths fold their middle folders into a `…` menu.
        let mut specs: Vec<Crumb> = vec![Crumb {
            name: "New project".into(),
            glyph: None,
            current: step == ProjectStep::Devices,
            target: CrumbTarget::Devices,
        }];
        let mut crumb_key = String::new();
        if let Some(device) = device {
            crumb_key.push_str(&device.id);
            specs.push(Crumb {
                name: device.name.into(),
                glyph: Some(device_glyph(&device.platform)),
                current: step == ProjectStep::Locations,
                target: CrumbTarget::Locations,
            });
        }
        let mut hidden: Vec<(String, String)> = Vec::new();
        // While the path is typed, the field stands in for the location and
        // folder crumbs; the device crumb stays to say where it is typed.
        if let Some((name, path)) = location.filter(|_| path_field.is_none()) {
            let root = path.clone().or(home.clone());
            // While a descend loads, keep showing the REQUESTED path so the
            // trail doesn't collapse to the location and pop back.
            let open_path = listing
                .as_ref()
                .map(|l| l.path.clone())
                .or(browser_path)
                .or(root.clone());
            let at_root = open_path.is_none()
                || open_path
                    .as_deref()
                    .zip(root.as_deref())
                    .is_some_and(|(open, root)| same_folder_path(open, root));
            crumb_key.push_str(&name);
            specs.push(Crumb {
                name: name.clone().into(),
                glyph: Some(if path.is_none() {
                    icons::HOME
                } else {
                    icons::HARD_DRIVE
                }),
                current: at_root,
                target: CrumbTarget::Location(name, path),
            });
            if let Some(open_path) = open_path {
                crumb_key.push_str(&open_path);
                let mut folders: Vec<(String, String)> = breadcrumbs(&open_path)
                    .into_iter()
                    .filter(|(_, full)| !root.as_deref().is_some_and(|root| path_under(root, full)))
                    .collect();
                hidden = fold_crumb_folders(&mut folders);
                if !hidden.is_empty() {
                    specs.push(Crumb {
                        name: "…".into(),
                        glyph: None,
                        current: false,
                        target: CrumbTarget::More,
                    });
                }
                for (name, full) in folders {
                    specs.push(Crumb {
                        name: name.into(),
                        glyph: None,
                        current: same_folder_path(&full, &open_path),
                        target: CrumbTarget::Folder(full),
                    });
                }
            }
        }
        if hidden.is_empty() && self.project_crumb_menu.get().is_some() {
            self.project_crumb_menu = popover::Popup::default();
        }
        let menu_open = self.project_crumb_menu.is_open();
        let menu_closing = self.project_crumb_menu.closing_at(crate::clock::now(cx));
        let menu_mounted = self.project_crumb_menu.get().is_some();
        let mut trail: Vec<AnyElement> = Vec::new();
        for (ix, spec) in specs.into_iter().enumerate() {
            if ix > 0 {
                trail.push(
                    icon(icons::ALT_ARROW_RIGHT)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_faint)
                        .into_any_element(),
                );
            }
            let group: SharedString = format!("project-crumb-{ix}").into();
            let more = matches!(spec.target, CrumbTarget::More);
            let color = if spec.current || (more && menu_open) {
                theme.text
            } else {
                theme.text_muted
            };
            let long = spec.name.chars().count() > 26;
            let mut el = div()
                .id(group.clone())
                .group(group.clone())
                .relative()
                .flex_none()
                .h(px(24.0))
                .px(px(6.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_color(color)
                .when(more, |el| el.min_w(px(24.0)).justify_center())
                .when(more && menu_open, |el| el.bg(theme.element_hover))
                .when_some(spec.glyph, |el, glyph| {
                    el.child(
                        icon(glyph)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(color)
                            .group_hover(group.clone(), |s| s.text_color(theme.text)),
                    )
                })
                .child(div().max_w(px(180.0)).truncate().child(spec.name.clone()))
                .when(more || long, |el| {
                    el.tooltip(crate::settings::widgets::text_tooltip(if more {
                        SharedString::from("Show hidden folders")
                    } else {
                        spec.name.clone()
                    }))
                });
            // The open crumb is where you already are: no hover, no click.
            if !spec.current {
                el = el
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label(if more {
                        SharedString::from("Show hidden folders")
                    } else {
                        spec.name.clone()
                    })
                    .hover(|s| s.bg(theme.element_hover).text_color(theme.text));
                el = match spec.target {
                    CrumbTarget::Devices => el.on_click(cx.listener(|this, _, _, cx| {
                        this.add_space_back_to(ProjectStep::Devices, cx)
                    })),
                    CrumbTarget::Locations => el.on_click(cx.listener(|this, _, _, cx| {
                        this.add_space_back_to(ProjectStep::Locations, cx)
                    })),
                    CrumbTarget::Location(name, path) => {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.add_space_goto_location(name.clone(), path.clone(), cx)
                        }))
                    }
                    CrumbTarget::Folder(full) => el.on_click(cx.listener(move |this, _, _, cx| {
                        this.add_space_descend(full.clone(), false, cx)
                    })),
                    CrumbTarget::More => el
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, _| {
                                this.project_crumb_menu.note_trigger_press()
                            }),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            // A press that found the menu open closes it.
                            if this.project_crumb_menu.take_press_was_open() {
                                this.close_project_crumb_menu(cx);
                            } else {
                                this.project_crumb_menu.open(());
                            }
                            cx.notify();
                        })),
                };
            }
            if more && menu_mounted {
                el = el.child(self.render_project_crumb_menu(&hidden, menu_closing, &theme, cx));
            }
            trail.push(el.into_any_element());
        }
        let crumb_scroll = self.add_space.as_mut().map(|flow| {
            // Reveal the open folder whenever the path changes; otherwise
            // leave the strip where the user scrolled it.
            if flow.crumb_key != crumb_key {
                flow.crumb_key = crumb_key;
                // An editable trail ends in the click-to-type spacer: reveal
                // through it, or the right fade stays on the open folder.
                flow.crumb_scroll
                    .scroll_to_item(trail.len().saturating_sub(usize::from(!can_edit_path)));
            }
            flow.crumb_scroll.clone()
        })?;
        let trail_row = match &path_field {
            // Address-bar style: the device crumb, then the typed path with
            // its ↵ (or what it is waiting for) at the end.
            Some((input, opening, error)) => div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .gap(px(2.0))
                .children(trail)
                .child(
                    icon(icons::ALT_ARROW_RIGHT)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_faint),
                )
                .child(
                    div()
                        .id("project-path")
                        .ml(px(4.0))
                        .flex_1()
                        .min_w_0()
                        .h(px(26.0))
                        .pl(px(8.0))
                        .pr(px(3.0))
                        .rounded(px(7.0))
                        .bg(crate::theme::ink(0.05))
                        .border_1()
                        .border_color(if error.is_some() {
                            theme.danger
                        } else {
                            theme.accent.opacity(0.6)
                        })
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            icon(icons::FOLDER)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(16.0))
                                .overflow_hidden()
                                .child(input.clone()),
                        )
                        .child(if *opening {
                            div()
                                .flex_none()
                                .px(px(5.0))
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child("Opening…")
                                .into_any_element()
                        } else {
                            div()
                                .id("project-path-open")
                                .group("project-path-open")
                                // slopty: a touch target on an iPad, as its neighbours.
                                .size(px(24.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.0))
                                .cursor_pointer()
                                .role(gpui::Role::Button)
                                .action_hint("Open this path", Keys::Fixed("enter"), cx)
                                .hover(|s| s.bg(crate::theme::wash(0.10)))
                                // It sits where the pen was: the second click
                                // of a double-click on the pen isn't a request.
                                .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                                    if event.click_count() < 2 {
                                        this.add_space_open_typed_path(false, cx)
                                    }
                                }))
                                .child(
                                    icon(icons::RETURN)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted)
                                        .group_hover("project-path-open", |s| {
                                            s.text_color(theme.text)
                                        }),
                                )
                                .into_any_element()
                        }),
                )
                .into_any_element(),
            None => crate::edge_fade::edge_faded(
                18.0,
                false,
                false,
                div()
                    .id("project-crumbs")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .overflow_x_scroll()
                    .track_scroll(&crumb_scroll)
                    .children(trail)
                    // The row's empty end types the path instead, like a
                    // file manager's address bar.
                    .when(can_edit_path, |trail| {
                        trail.child(
                            div()
                                .id("project-crumb-edit")
                                .flex_1()
                                .min_w(px(24.0))
                                .h_full()
                                .cursor(gpui::CursorStyle::IBeam)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.add_space_edit_path(cx)),
                                ),
                        )
                    }),
            )
            .fade_left(true)
            .fade_right(true)
            .fade_overflow_x(&crumb_scroll)
            .into_any_element(),
        };
        // ← mirrors the Left key: up one level, and back to Cmd+K from the
        // first step.
        let back_label = if step == ProjectStep::Devices {
            "Back to commands"
        } else {
            "Back"
        };
        let crumbs = div()
            .h(px(36.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.06))
            .text_size(crate::typography::ui_rems(12.0))
            .child(
                div()
                    .id("project-crumb-back")
                    .group("project-crumb-back")
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    // slopty: ← goes back, as the legend says.
                    .action_hint(back_label, Keys::Fixed("left"), cx)
                    .hover(|s| s.bg(theme.element_hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.add_space.as_ref().map(|f| f.step) == Some(ProjectStep::Devices) {
                            this.close_add_space(cx);
                            this.toggle_command_palette(window, cx);
                        } else {
                            this.add_space_go_up(cx);
                        }
                    }))
                    .child(
                        icon(icons::ARROW_LEFT)
                            .size(px(16.0))
                            .text_color(theme.text_muted)
                            .group_hover("project-crumb-back", |s| s.text_color(theme.text)),
                    ),
            )
            .child(trail_row)
            .when(can_edit_path && path_field.is_none(), |row| {
                row.child(
                    div()
                        .id("project-path-edit")
                        .group("project-path-edit")
                        .size(px(24.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .role(gpui::Role::Button)
                        // slopty: its tooltip names ⌘L, the key its raw handler takes.
                        .action_hint("Edit path", Keys::Fixed("mod-l"), cx)
                        .hover(|s| s.bg(theme.element_hover))
                        .on_click(cx.listener(|this, _, _, cx| this.add_space_edit_path(cx)))
                        .child(
                            icon(icons::PEN)
                                .size(px(14.0))
                                .text_color(theme.text_muted)
                                .group_hover("project-path-edit", |s| s.text_color(theme.text)),
                        ),
                )
            });
        let path_error = path_field.as_ref().and_then(|(_, _, error)| error.clone());

        let shortcut = {
            let id = ShortcutId::NewProject;
            let combo = self.settings.keymap.get(id);
            let valid = crate::settings::parse_combo(combo).is_ok();
            crate::keys::combo_text(if valid { combo } else { id.default_combo() })
        };
        // While the path is typed, ⌘↵ and the button add the TYPED path.
        let editing = path_field.is_some();
        let can_add = !busy
            && match &path_field {
                Some((input, opening, _)) => !opening && !input.read(cx).text().trim().is_empty(),
                None => listing.is_some(),
            };
        // slopty: its keys where there are keys; a touch screen keeps only the button that adds.
        let keys = !crate::shell::touch_screen();
        let adds = (step == ProjectStep::Folders && !cloning) || editing;
        let footer = if !keys {
            command_palette::palette_footer()
        } else if editing {
            command_palette::palette_footer()
                .child(command_palette::command_key_hint(&theme, "Tab", "Complete", window, cx))
                .child(command_palette::command_key_hint(&theme, "⏎", "Open", window, cx))
                .child(command_palette::command_key_hint(&theme, "Esc", "Cancel", window, cx))
        } else {
            command_palette::palette_footer()
                .child(command_palette::command_key_hint(&theme, "↑ ↓", "Navigate", window, cx))
                .child(command_palette::command_key_hint(
                    &theme,
                    "⏎",
                    if cloning {
                        "Clone" // slopty
                    } else if step == ProjectStep::Folders {
                        "Open"
                    } else {
                        "Select"
                    },
                    window,
                    cx,
                ))
                .when(step != ProjectStep::Devices, |el| {
                    el.child(command_palette::command_key_hint(&theme, "←", "Back", window, cx))
                })
                .child(command_palette::command_key_hint(&theme, "Esc", "Close", window, cx))
                .when(can_edit_path && !cloning, |el| {
                    el.child(command_palette::command_key_hint(
                        &theme,
                        &crate::keys::combo_text("mod-l"),
                        "Edit path",
                        window,
                        cx,
                    ))
                })
        }
        // slopty: a typed address clones from ⏎, so the add button stands down while it shows.
        .when(adds, |el| {
            el.child(div().flex_1()).child(
                div()
                    .id("project-add")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    // Even 3px around the key chip, concentric corners
                    // (chip 5px + 3px); negative margins keep the footer
                    // height and the label on the footer's right inset.
                    .pl(px(3.0))
                    .pr(px(8.0))
                    .py(px(3.0))
                    .my(px(-3.0))
                    .mr(px(-8.0))
                    .rounded(px(8.0))
                    .role(gpui::Role::Button)
                    .aria_label("Add project")
                    .when(can_add, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(crate::theme::card_selected_bg()))
                            .active(|s| s.opacity(0.8))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if editing {
                                    this.add_space_open_typed_path(true, cx)
                                } else {
                                    this.submit_add_space(cx)
                                }
                            }))
                    })
                    .when(!can_add, |el| el.opacity(0.5))
                    .when(keys, |el| {
                        el.child(popover::kbd_hint(&theme, &crate::keys::combo_text("mod-enter")))
                    })
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(10.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(if busy { "Adding…" } else { "Add project" }),
                    ),
            )
        });
        let card =
            command_palette::palette_card("add-space-palette", &focus, viewport, &theme)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    this.add_space_key(event, cx)
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    // The crumb menu floats outside the card; its own
                    // mouse-down-out dismisses it without closing the palette.
                    if this.project_crumb_menu.get().is_some() {
                        return;
                    }
                    this.close_add_space(cx);
                }))
                .child(command_palette::palette_header(
                    &theme,
                    search.into_any_element(),
                    popover::kbd_hint(&theme, &shortcut),
                ))
                .child(crumbs)
                // Why the typed path didn't open, right under the field; the
                // listing below is still the folder that was open.
                .when_some(path_error, |el, error| {
                    el.child(
                        div()
                            .id("project-path-error")
                            .px(px(16.0))
                            .pt(px(8.0))
                            .pb(px(2.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .child(results)
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .px(px(16.0))
                            .pb(px(8.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .when(keys || adds, |el| el.child(footer));
        Some(command_palette::palette_overlay(viewport, card))
    }

    // ---- space context menu / rename / delete overlays ----

    pub(super) fn close_space_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_menu);
            cx.notify();
        }
    }

    pub(super) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let current = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| ComposerInput::new("Project name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_space(cx);
            }
        });
        self.rename_space_dialog = Some(RenameSpaceDialog {
            space_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn submit_rename_space(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_space_dialog.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if !name.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameSpace", "spaceId": dialog.space_id, "name": name }),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.delete_space_confirm = None;
        self.mutate(
            serde_json::json!({ "op": "deleteSpace", "spaceId": space_id }),
            cx,
        );
        cx.notify();
    }

    /// Space context menu + rename dialog + delete confirm (appended to the
    /// shell's overlay list).
    pub(super) fn render_space_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();
        self.delete_space_focus.sync(self.delete_space_confirm.is_some(), window, cx);

        if let Some((space_id, position)) = self.space_menu.get().cloned() {
            let closing = self.space_menu.closing_at(crate::clock::now(cx));
            let rename_id = space_id.clone();
            let delete_id = space_id.clone();
            let menu = popover::popover_card(&theme)
                .w(px(170.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_space_menu(cx);
                }))
                .flex()
                .flex_col()
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-rename-{space_id}"))
                        .id("space-menu-rename")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_rename_space(rename_id.clone(), cx)
                        }))
                        .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Rename…")),
                )
                .child(popover::menu_separator())
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-delete-{space_id}"))
                        .id("space-menu-delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_space_menu(cx);
                            this.delete_space_confirm = Some(delete_id.clone());
                            cx.notify();
                        }))
                        .child(
                            icon(icons::TRASH_BIN_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.danger),
                        )
                        .child(SharedString::from("Remove…")),
                )
                .into_any_element();
            overlays.push(popover::menu_at(
                "space-context-menu",
                position,
                menu,
                closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_space_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_space_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename project"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-space-cancel")
                                .id("rename-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_space_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::named_button( // slopty: a named button
                                popover::btn_primary(&theme, "Rename").id("rename-space-save"),
                                "Rename",
                            )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_space(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-space-dialog", viewport, card));
        }

        if let Some(space_id) = self.delete_space_confirm.clone() {
            let (name, device, count) = {
                let state = self.state.read(cx);
                let space = state.space_row(&space_id);
                (
                    space
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "this project".into()),
                    space
                        .and_then(|s| state.device_name(&s.device_id))
                        .unwrap_or("its device")
                        .to_string(),
                    state.chats_in_space(&space_id).len(),
                )
            };
            // slopty: a session is its agent's own record, so removing a project archives its
            // sessions (Archived restores them) and deletes nothing on the machine.
            let copy = remove_project_body(&name, &device, count);
            let shell = cx.weak_entity();
            let card = crate::surface_chrome::modal_card(
                popover::dialog_card(&theme),
                "delete-space-card",
                self.delete_space_focus.handle(),
                "Remove project?",
                move |_, cx| {
                    let _gone = shell.update(cx, |this, cx| {
                        this.delete_space_confirm = None;
                        cx.notify();
                    });
                },
            )
                .child(popover::dialog_title(&theme, "Remove project?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, copy)))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-space-cancel")
                                .id("delete-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_space_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Remove")
                                .id("delete-space-confirm")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_space(space_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("delete-space-dialog", viewport, card));
        }

        overlays
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::compare_sidebar_chats;

    /// slopty: removing a project says its sessions move to Archived, counted, and that the
    /// machine keeps everything.
    #[test]
    fn removing_a_project_says_its_sessions_are_archived() {
        let none = super::remove_project_body("app", "Studio", 0);
        assert_eq!(none, "\u{201C}app\u{201D} leaves the sidebar. Nothing on Studio is deleted.");
        let one = super::remove_project_body("app", "Studio", 1);
        assert!(one.contains("its session on Studio moves to Archived"), "{one}");
        let many = super::remove_project_body("app", "Studio", 3);
        assert!(many.contains("its 3 sessions on Studio move to Archived"), "{many}");
        assert!(many.ends_with("Nothing on Studio is deleted."), "{many}");
    }

    fn chat(id: &str) -> slopty_view_proto::Chat {
        slopty_view_proto::Chat {
            id: id.into(),
            device_id: "device".into(),
            title: None,
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: Some(Utc.timestamp_opt(10, 0).unwrap()),
            created_at: Utc.timestamp_opt(5, 0).unwrap(),
            harness_session_id: None,
            harness_session_cwd: None,
            parent_chat_id: None,
            subagent: false, // slopty: subagents
            in_parent: false,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            draft: None, // slopty: new field.
        }
    }

    #[test]
    fn equal_sidebar_timestamps_sort_by_stable_chat_id() {
        let alpha = chat("alpha");
        let beta = chat("beta");
        assert!(compare_sidebar_chats(&alpha, &beta).is_lt());
        // slopty: newer sessions first, and a new message moves nothing.
        let mut newer = chat("zeta");
        newer.created_at = Utc.timestamp_opt(6, 0).unwrap();
        assert!(compare_sidebar_chats(&newer, &alpha).is_lt());
        let mut busy = chat("aardvark");
        busy.last_message_at = Some(Utc.timestamp_opt(99, 0).unwrap());
        assert!(compare_sidebar_chats(&alpha, &busy).is_gt(), "by id, not by its last message");
    }
}

/// Synthetic responses for the isolated native screenshot fixture only.
#[cfg(feature = "project-palette-fixture")]
impl Shell {
    pub fn fixture_project_responses(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.device.is_some() && !matches!(flow.drives, Loadable::Ready(_)) {
            flow.drives = Loadable::Ready(
                serde_json::from_value(serde_json::json!([
                    {"name":"Projects", "path":"/projects"},
                    {"name":"System", "path":"/"}
                ]))
                .unwrap(),
            );
            cx.notify();
        }
        if flow.step == ProjectStep::Folders && !matches!(flow.browser, Loadable::Ready(_)) {
            let path = flow
                .browser_path
                .clone()
                .unwrap_or_else(|| "/home/alex".into());
            if flow.browser_path.is_none() {
                flow.home = Some(path.clone());
            }
            let names = match path.as_str() {
                "/home/alex" => vec![
                    "Desktop",
                    "Documents",
                    "Downloads",
                    "Movies",
                    "Music",
                    "Pictures",
                    "Projects",
                    "Public",
                    "dotfiles",
                    "notes",
                    "sandbox",
                    "scratch",
                ],
                "/projects" | "/home/alex/Projects" => vec!["fieldnotes", "mobile-app", "website"],
                _ => vec!["assets", "docs", "src", "tests"],
            };
            flow.browser = Loadable::Ready(
                serde_json::from_value(serde_json::json!({
                    "path":path,
                    "entries":names.into_iter().map(|name| serde_json::json!({
                        "name":name,"isDir":true,"isRepo":name=="fieldnotes"
                    })).collect::<Vec<_>>()
                }))
                .unwrap(),
            );
            cx.notify();
        }
    }
}

// slopty: no typed-path fixture (the examples are not taken).

#[cfg(test)]
mod project_flow_tests {
    use super::*;

    #[test]
    fn deep_crumb_trails_fold_all_but_the_deepest_folders() {
        let mut short = vec!["a", "b", "c"];
        assert!(fold_crumb_folders(&mut short).is_empty());
        assert_eq!(short, ["a", "b", "c"]);
        let mut deep = vec!["a", "b", "c", "d", "e"];
        assert_eq!(fold_crumb_folders(&mut deep), ["a", "b", "c"]);
        assert_eq!(deep, ["d", "e"]);
    }

    #[test]
    fn path_under_handles_posix_and_windows_drive_paths() {
        assert!(path_under("/media/a", "/"));
        assert!(path_under("/media/a", "/media"));
        assert!(!path_under("/media/ab", "/media/a"));
        // A drive-root crumb hides itself, not a sibling drive.
        assert!(path_under(r"D:\", r"D:\"));
        assert!(path_under(r"D:\Random", r"D:\"));
        assert!(!path_under(r"D:\Random2", r"D:\Random"));
        assert!(!path_under(r"C:\Random", r"D:\"));
        assert!(path_under(r"c:\Random\repo", r"C:\Random"));
        assert!(!path_under(r"C:\random\repo", r"C:\Random"));
        assert!(path_under(r"\\?\D:\Random", r"D:\"));
        assert!(path_under(r"D:\Random", r"\\?\D:\"));
        assert!(path_under(r"\\?\UNC\server\share\repo", r"\\server\share\"));
        assert!(!path_under(r"\\server\share2\repo", r"\\server\share\"));
        assert!(!path_under(r"\\other\share\repo", r"\\server\share\"));
    }

    #[gpui::test]
    fn devices_locations_folders_and_back_clear_stale_state(cx: &mut gpui::TestAppContext) {
        let data = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let shell = cx.new(|cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.devices = serde_json::from_value(serde_json::json!([
                    {"id":"local","name":"Studio","platform":"macos","lastSeenAt":null},
                    {"id":"remote","name":"Server","platform":"linux","lastSeenAt":null}
                ]))
                .unwrap();
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data.path().into(),
                    // slopty: Slopty's server is the only backend.
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.open_add_space(cx);
            assert_eq!(shell.add_space.as_ref().unwrap().step, ProjectStep::Devices);
            assert!(shell.add_space.as_ref().unwrap().device.is_none());
            let search = shell.add_space.as_ref().unwrap().search.clone();
            search.update(cx, |input, cx| input.set_text("server", cx));
            assert_eq!(shell.add_space_devices(cx).len(), 1);
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_mut().unwrap();
            assert_eq!(flow.step, ProjectStep::Locations);
            assert_eq!(flow.device.as_ref().unwrap().id, "remote");
            assert!(flow.search.read(cx).is_empty());
            flow.drives = Loadable::Ready(vec![DriveEntry {
                name: "Projects".into(),
                path: "/projects".into(),
            }]);
            search.update(cx, |input, cx| input.set_text("projects", cx));
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_mut().unwrap();
            assert_eq!(flow.step, ProjectStep::Folders);
            assert_eq!(flow.browser_path.as_deref(), Some("/projects"));
            flow.browser = Loadable::Ready(FolderListing {
                path: "/projects".into(),
                entries: Vec::new(),
                truncated: false,
            });
            shell.add_space_go_up(cx);
            assert_eq!(
                shell.add_space.as_ref().unwrap().step,
                ProjectStep::Locations
            );
            assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
            shell.add_space_go_up(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.step, ProjectStep::Devices);
            assert!(flow.device.is_none());
            assert!(flow.drives.ready().is_none());
            assert!(flow.search.read(cx).is_empty());
            // Slash navigation only applies to folders, never device search.
            search.update(cx, |input, cx| input.set_text("/projects/", cx));
            assert!(!shell.add_space_slash_descend(cx));
        });
    }
}

#[cfg(test)]
mod home_folder_search_tests {
    use super::*;
    use gpui::TestAppContext;

    /// The engine side of a shell under test. ListFolders answers with
    /// `listing`; methods not being awaited answer `{}`.
    struct FakeEngine {
        runtime: tokio::runtime::Runtime,
        requests: tokio::sync::mpsc::Receiver<String>,
        replies: tokio::sync::mpsc::Sender<String>,
        listing: serde_json::Value,
    }

    impl FakeEngine {
        fn take(&mut self, method: &str, cx: &mut TestAppContext) -> Vec<serde_json::Value> {
            let mut found = Vec::new();
            loop {
                cx.run_until_parked();
                let Ok(request) = self.requests.try_recv() else {
                    return found;
                };
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                if request["method"] == method {
                    found.push(request);
                } else if request["method"] == methods::LIST_FOLDERS {
                    let mut listing = self.listing.clone();
                    if let Some(path) = request["params"]["path"].as_str() {
                        listing["path"] = path.into();
                        listing["entries"] = serde_json::json!([]);
                    }
                    self.reply(&request, listing);
                } else {
                    self.reply(&request, serde_json::json!({}));
                }
            }
        }

        fn reply(&self, request: &serde_json::Value, ok: serde_json::Value) {
            self.send(serde_json::json!({ "id": request["id"], "ok": ok }));
        }

        fn fail(&self, request: &serde_json::Value, err: &str) {
            self.send(serde_json::json!({ "id": request["id"], "err": err }));
        }

        fn send(&self, reply: serde_json::Value) {
            // The client's reader task runs on this runtime: let it drain.
            self.runtime.block_on(async {
                self.replies.send(reply.to_string()).await.unwrap();
                while self.replies.capacity() < self.replies.max_capacity() {
                    tokio::task::yield_now().await;
                }
            });
        }
    }

    fn device(id: &str) -> Device {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "platform": "linux", "lastSeenAt": null,
        }))
        .unwrap()
    }

    /// A shell with the new-project picker open on `device` (local: "local").
    fn picker(
        cx: &mut TestAppContext,
        device_id: &str,
    ) -> (gpui::WindowHandle<Shell>, tempfile::TempDir, FakeEngine) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (out, requests) = tokio::sync::mpsc::channel(64);
        let (replies, inbound) = tokio::sync::mpsc::channel(64);
        let engine = {
            let _guard = runtime.enter();
            crate::state::EngineHandle::from_test_client(slopty_view_rpc::RpcClient::new(out, inbound))
        };
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data_dir = dir.path().to_path_buf();
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            Shell::new(
                state,
                crate::EngineBootConfig {
                    data_dir,
                    // slopty: Slopty's server is the only backend.
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        let picked = device(device_id);
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("local".into());
                    state.devices = vec![picked.clone()];
                    state.set_test_engine(engine);
                });
                shell.open_add_space(cx);
                shell.add_space_pick_device(picked, cx);
            })
            .unwrap();
        let fake = FakeEngine {
            runtime,
            requests,
            replies,
            listing: serde_json::json!({
                "path": "/home/me",
                "entries": [{ "name": "code", "isDir": true, "isRepo": false }],
            }),
        };
        (window, dir, fake)
    }

    fn go_home(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) {
        window
            .update(cx, |shell, _, cx| {
                shell.add_space_goto_location("Home".into(), None, cx)
            })
            .unwrap();
    }

    fn type_query(window: gpui::WindowHandle<Shell>, query: &str, cx: &mut TestAppContext) {
        window
            .update(cx, |shell, _, cx| {
                let search = shell.add_space.as_ref().unwrap().search.clone();
                search.update(cx, |input, cx| input.set_text(query, cx));
            })
            .unwrap();
    }

    fn rows(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) -> Vec<FolderRow> {
        window
            .read_with(cx, |shell, cx| shell.add_space_folder_rows(cx))
            .unwrap()
    }

    fn found(path: &str, relative: &str, is_repo: bool) -> serde_json::Value {
        serde_json::json!({ "path": path, "relative": relative, "isRepo": is_repo })
    }

    fn begin_home_search(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<Shell>,
        tempfile::TempDir,
        FakeEngine,
        serde_json::Value,
    ) {
        let (window, dir, mut engine) = picker(cx, "remote");
        go_home(window, cx);
        let warm = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(warm.len(), 1);
        engine.reply(
            &warm[0],
            serde_json::json!({ "matches": [], "indexing": true }),
        );
        cx.run_until_parked();
        // Merely warming Home must not start polling an empty query.
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
        type_query(window, "comet", cx);
        let request = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(request.len(), 1);
        (window, dir, engine, request.into_iter().next().unwrap())
    }

    #[gpui::test]
    fn home_search_refreshes_with_capped_backoff_and_no_overlapping_requests(
        cx: &mut TestAppContext,
    ) {
        let (window, _dir, mut engine, first) = begin_home_search(cx);
        engine.reply(
            &first,
            serde_json::json!({ "matches": [], "indexing": true }),
        );
        cx.run_until_parked();
        assert!(rows(window, cx).is_empty());
        let comet = found("/home/me/dev/comet", "dev/comet", true);
        for (round, millis) in [500, 1000, 2000, 2000].into_iter().enumerate() {
            cx.executor()
                .advance_clock(Duration::from_millis(millis - 1));
            assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
            cx.executor().advance_clock(Duration::from_millis(1));
            let requests = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["params"], first["params"]);
            assert_eq!(requests[0]["params"]["targetDeviceId"], "remote");
            // A slow response does not queue more requests or erase rows.
            let partial = rows(window, cx);
            cx.executor().advance_clock(Duration::from_secs(10));
            assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
            assert_eq!(rows(window, cx), partial);
            let matches = if round == 0 {
                vec![comet.clone()]
            } else {
                vec![
                    found("/home/me/new/comet", "new/comet", true),
                    comet.clone(),
                ]
            };
            engine.reply(
                &requests[0],
                serde_json::json!({
                    "matches": matches, "indexing": round < 3,
                }),
            );
            cx.run_until_parked();
            window
                .read_with(cx, |shell, cx| {
                    let flow = shell.add_space.as_ref().unwrap();
                    assert_eq!(flow.home_search.indexing, round < 3);
                    assert!(!flow.home_search.loading);
                    // The original selected folder survives result reordering.
                    assert!(matches!(
                        &shell.add_space_folder_rows(cx)[flow.active],
                        FolderRow::Home(found) if found.relative == "dev/comet"
                    ));
                })
                .unwrap();
        }
        assert_eq!(rows(window, cx).len(), 2);
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
    }

    #[gpui::test]
    fn home_search_cancels_retries_on_edits_navigation_and_close(cx: &mut TestAppContext) {
        for action in ["clear", "short", "path", "drive", "device", "close"] {
            let (window, _dir, mut engine, first) = begin_home_search(cx);
            engine.reply(
                &first,
                serde_json::json!({ "matches": [], "indexing": true }),
            );
            cx.run_until_parked();
            match action {
                "clear" => type_query(window, "", cx),
                "short" => type_query(window, "c", cx),
                "path" => type_query(window, "/mnt/data", cx),
                _ => window
                    .update(cx, |shell, _, cx| match action {
                        "drive" => shell.add_space_goto_location(
                            "Data".into(),
                            Some("/mnt/data".into()),
                            cx,
                        ),
                        "device" => {
                            shell.add_space_back_to(ProjectStep::Devices, cx);
                            shell.add_space_pick_device(device("local"), cx);
                        }
                        "close" => shell.close_add_space(cx),
                        _ => unreachable!(),
                    })
                    .unwrap(),
            }
            cx.executor().advance_clock(Duration::from_secs(10));
            assert!(
                engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty(),
                "{action}"
            );
            window
                .read_with(cx, |shell, _| {
                    assert!(
                        !shell
                            .add_space
                            .as_ref()
                            .is_some_and(|flow| flow.home_search.indexing)
                    );
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn new_home_query_resets_backoff_and_ignores_in_flight_answers(cx: &mut TestAppContext) {
        let (window, _dir, mut engine, first) = begin_home_search(cx);
        engine.reply(
            &first,
            serde_json::json!({ "matches": [], "indexing": true }),
        );
        cx.run_until_parked();
        cx.executor().advance_clock(HOME_FOLDER_INDEX_RETRY);
        let stale = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(stale.len(), 1);
        type_query(window, "updated", cx);
        let updated = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0]["params"]["query"], "updated");
        engine.reply(
            &updated[0],
            serde_json::json!({ "matches": [], "indexing": true }),
        );
        engine.reply(
            &stale[0],
            serde_json::json!({
                "matches": [found("/home/me/dev/comet", "dev/comet", true)], "indexing": true,
            }),
        );
        cx.run_until_parked();
        assert!(rows(window, cx).is_empty());
        cx.executor()
            .advance_clock(HOME_FOLDER_INDEX_RETRY - Duration::from_millis(1));
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
        cx.executor().advance_clock(Duration::from_millis(1));
        let retry = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0]["params"], updated[0]["params"]);
        engine.reply(
            &retry[0],
            serde_json::json!({ "matches": [], "indexing": false }),
        );
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
    }

    #[gpui::test]
    fn home_search_stops_retrying_after_errors(cx: &mut TestAppContext) {
        for error in ["failed", "unsupported", "decode"] {
            let (window, _dir, mut engine, first) = begin_home_search(cx);
            engine.reply(
                &first,
                serde_json::json!({ "matches": [], "indexing": true }),
            );
            cx.run_until_parked();
            cx.executor().advance_clock(HOME_FOLDER_INDEX_RETRY);
            let retry = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
            assert_eq!(retry.len(), 1);
            match error {
                "failed" => engine.fail(&retry[0], "search failed"),
                "unsupported" => engine.fail(&retry[0], "unknown method: SearchHomeFolders"),
                "decode" => engine.reply(&retry[0], serde_json::json!({})),
                _ => unreachable!(),
            }
            cx.run_until_parked();
            window
                .read_with(cx, |shell, _| {
                    let search = &shell.add_space.as_ref().unwrap().home_search;
                    assert!(!search.indexing);
                    assert!(!search.loading);
                })
                .unwrap();
            cx.executor().advance_clock(Duration::from_secs(10));
            assert!(
                engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty(),
                "{error}"
            );
        }
    }

    #[gpui::test]
    fn home_warms_on_entry_and_lists_matches_below_the_open_folder(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        let warm = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(warm.len(), 1);
        assert_eq!(warm[0]["params"]["query"], "");
        assert!(warm[0]["params"].get("targetDeviceId").is_none());
        engine.reply(
            &warm[0],
            serde_json::json!({ "matches": [], "indexing": true }),
        );

        type_query(window, "c", cx);
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
        type_query(window, "co", cx);
        let search = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(search[0]["params"]["query"], "co");
        engine.reply(
            &search[0],
            serde_json::json!({
                "matches": [
                    found("/home/me/code", "code", false),
                    found("/home/me/dev/comet", "dev/comet", true),
                ],
                "indexing": false,
            }),
        );
        cx.run_until_parked();
        // The open folder's own `code` is listed once, as a local row.
        let rows = rows(window, cx);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], FolderRow::Local(entry) if entry.name == "code"));
        assert!(matches!(&rows[1], FolderRow::Home(m) if m.relative == "dev/comet" && m.is_repo));
    }

    // slopty: the folders gone to most lead Home while nothing is typed.
    #[gpui::test]
    fn home_lists_the_folders_gone_to_most_first_until_a_query(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        let warm = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        engine.reply(
            &warm[0],
            serde_json::json!({
                "matches": [
                    found("/home/me/code", "code", false),
                    found("/Volumes/Work/oss/slopty", "/Volumes/Work/oss/slopty", true),
                ],
                "indexing": false,
            }),
        );
        cx.run_until_parked();
        let listed = rows(window, cx);
        assert_eq!(listed.len(), 2, "the home's own code is listed once, as gone to");
        assert!(matches!(&listed[0], FolderRow::Home(m) if m.relative == "code"));
        assert!(matches!(&listed[1], FolderRow::Home(m) if m.is_repo));
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty(), "no polling");
        type_query(window, "zz", cx);
        cx.run_until_parked();
        assert!(rows(window, cx).iter().all(|row| !matches!(row, FolderRow::Home(_))));
        type_query(window, "", cx);
        cx.run_until_parked();
        assert_eq!(rows(window, cx).len(), 2, "cleared, the folders gone to most are back");
    }

    #[gpui::test]
    fn remote_windows_home_matches_navigate_and_reuse_existing_spaces(cx: &mut TestAppContext) {
        for (home, extended_home) in [
            (r"C:\Users\Ana", r"\\?\C:\Users\Ana"),
            (r"\\server\share\Ana", r"\\?\UNC\server\share\Ana"),
        ] {
            let (window, _dir, mut engine) = picker(cx, "remote");
            // slopty: no History surface (ruled out).
            engine.listing["path"] = home.into();
            go_home(window, cx);
            let warm = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
            engine.reply(
                &warm[0],
                serde_json::json!({ "matches": [], "indexing": false }),
            );
            type_query(window, "co", cx);
            let search = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
            let code = format!("{extended_home}\\code");
            let comet = format!("{code}\\comet");
            engine.reply(
                &search[0],
                serde_json::json!({
                    "matches": [found(&code, "code", false), found(&comet, "code/comet", true)],
                    "indexing": false,
                }),
            );
            cx.run_until_parked();
            // Plain ListFolders and extended SearchHomeFolders paths refer
            // to the same local row; only the nested match is added.
            let listed = rows(window, cx);
            assert_eq!(listed.len(), 2);
            assert!(matches!(&listed[0], FolderRow::Local(entry) if entry.name == "code"));
            window
                .update(cx, |shell, _, cx| {
                    shell.add_space.as_mut().unwrap().active = 1;
                    shell.add_space_open_active(cx);
                })
                .unwrap();
            let browse = engine.take(methods::LIST_FOLDERS, cx);
            assert_eq!(browse[0]["params"]["path"], comet);
            assert_eq!(browse[0]["params"]["targetDeviceId"], "remote");
            engine.reply(
                &browse[0],
                serde_json::json!({
                    "path": comet,
                    "entries": [{ "name": "src", "isDir": true, "isRepo": false }],
                }),
            );
            cx.run_until_parked();
            window
                .update(cx, |shell, _, cx| shell.add_space_open_active(cx))
                .unwrap();
            let child = engine.take(methods::LIST_FOLDERS, cx);
            assert_eq!(child[0]["params"]["path"], format!("{comet}\\src"));
            engine.reply(
                &child[0],
                serde_json::json!({ "path": format!("{comet}\\src"), "entries": [] }),
            );
            cx.run_until_parked();
            window
                .update(cx, |shell, _, cx| shell.add_space_go_up(cx))
                .unwrap();
            let parent = engine.take(methods::LIST_FOLDERS, cx);
            assert_eq!(parent[0]["params"]["path"], comet);
            engine.reply(
                &parent[0],
                serde_json::json!({ "path": comet, "entries": [] }),
            );
            cx.run_until_parked();
            window.update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.spaces.push(serde_json::from_value(serde_json::json!({
                        "id": "existing", "deviceId": "remote", "path": format!("{home}\\code\\comet"),
                        "gitDetected": true, "createdAt": "2026-09-20T00:00:00Z",
                    })).unwrap());
                });
                shell.submit_add_space(cx);
                assert!(shell.add_space.is_none());
                assert_eq!(shell.state.read(cx).selected_space.as_deref(), Some("existing"));
                assert_eq!(shell.state.read(cx).spaces.len(), 1);
            }).unwrap();
            assert!(engine.take(methods::MUTATE, cx).is_empty());
        }
    }

    #[gpui::test]
    fn opening_a_home_match_browses_it(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        type_query(window, "comet", cx);
        let search = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        engine.reply(
            &search[0],
            serde_json::json!({
                "matches": [found("/home/me/dev/comet", "dev/comet", true)],
                "indexing": false,
            }),
        );
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| shell.add_space_open_active(cx))
            .unwrap();
        let browse = engine.take(methods::LIST_FOLDERS, cx);
        assert_eq!(browse[0]["params"]["path"], "/home/me/dev/comet");
        window
            .read_with(cx, |shell, _| {
                let flow = shell.add_space.as_ref().unwrap();
                assert!(flow.browser_repo);
            })
            .unwrap();
    }

    // slopty: what passes for an address to clone, and the folder it makes.
    #[test]
    fn a_clone_address_is_a_url_scp_or_path_and_names_its_folder() {
        for url in [
            "https://github.com/zeronsh/zeron.git",
            "https://github.com/zeronsh/zeron",
            "HTTPS://example.com/a/b/",
            "ssh://git@host:22/owner/repo.git",
            "git@github.com:owner/repo.git",
            "git://127.0.0.1:9418/origin.git",
        ] {
            assert!(looks_like_repo_url(url), "{url}");
        }
        for url in [
            "",
            "   ",
            "repo",
            "https://",
            "github.com/owner/repo",
            "/",
            "~/",
            "a b",
            "C:/x",
            "file:///srv/git/origin.git",
            "/srv/git/origin.git",
            "~/src/origin",
        ] {
            assert!(!looks_like_repo_url(url), "{url:?}");
        }
        assert_eq!(clone_repo_name("https://github.com/zeronsh/zeron.git").as_deref(), Some("zeron"));
        assert_eq!(clone_repo_name("git@github.com:owner/repo.git").as_deref(), Some("repo"));
        assert_eq!(clone_repo_name("https://example.com/a/b/").as_deref(), Some("b"));
        assert_eq!(clone_repo_name("/srv/git/origin.git").as_deref(), Some("origin"));
        assert_eq!(clone_repo_name("https://example.com/.git"), None);
        let answer = serde_json::json!({ "spaceId": "s", "deviceId": "d", "path": "/home/me/zeron" });
        assert_eq!(
            cloned_space(&answer),
            Some(("s".to_owned(), "d".to_owned(), "/home/me/zeron".to_owned()))
        );
        assert_eq!(cloned_space(&serde_json::json!({ "path": "/x" })), None);
    }

    /// Opens Home on `device`, its listing answered, then "Clone repository…" with `url` typed.
    fn clone_typed(
        cx: &mut TestAppContext,
        device: &str,
        url: &str,
    ) -> (gpui::WindowHandle<Shell>, tempfile::TempDir, FakeEngine) {
        let (window, dir, mut engine) = picker(cx, device);
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        // The clone row follows the folders; ⏎ on it takes an address.
        window
            .update(cx, |shell, _, cx| {
                let folders = shell.add_space_folder_rows(cx).len();
                shell.add_space.as_mut().unwrap().active = folders;
                shell.add_space_open_active(cx);
                assert!(shell.add_space.as_ref().unwrap().cloning);
            })
            .unwrap();
        type_query(window, url, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        (window, dir, engine)
    }

    // slopty: a clone goes in the open folder on the picked machine, then is its project.
    #[gpui::test]
    fn a_clone_goes_in_the_open_folder_and_becomes_the_project(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) =
            clone_typed(cx, "remote", "https://github.com/zeronsh/zeron.git");
        // Typing an address neither descends at its slashes nor searches Home.
        window
            .read_with(cx, |shell, cx| {
                let flow = shell.add_space.as_ref().unwrap();
                assert_eq!(flow.browser.ready().unwrap().path, "/home/me");
                assert_eq!(
                    shell.add_space_clone_target(cx),
                    Some(("zeron".to_owned(), "/home/me/zeron".to_owned()))
                );
            })
            .unwrap();
        window
            .update(cx, |shell, _, cx| shell.add_space_open_active(cx))
            .unwrap();
        let asked = engine.take(methods::CLONE_REPO, cx);
        assert_eq!(asked.len(), 1);
        let params = &asked[0]["params"];
        assert_eq!(params["url"], "https://github.com/zeronsh/zeron.git");
        assert_eq!(params["parentPath"], "/home/me");
        assert_eq!(params["deviceId"], "remote");
        assert_eq!(params["targetDeviceId"], "remote");
        window
            .read_with(cx, |shell, cx| {
                assert!(shell.add_space.is_none(), "the picker closes at once");
                assert_eq!(crate::toast::messages(cx), ["Cloning zeron on remote"]);
            })
            .unwrap();
        // The clone added the project itself: the window lands in it, adding nothing again.
        engine.reply(
            &asked[0],
            serde_json::json!({ "spaceId": "cloned", "deviceId": "remote", "path": "/home/me/zeron" }),
        );
        assert!(engine.take(methods::MUTATE, cx).is_empty(), "no second project");
        window
            .read_with(cx, |shell, cx| {
                let state = shell.state.read(cx);
                let space = state.spaces.iter().find(|s| s.id == "cloned").unwrap();
                assert_eq!((space.device_id.as_str(), space.path.as_str()), ("remote", "/home/me/zeron"));
                assert_eq!(state.selected_space.as_deref(), Some("cloned"));
                assert!(crate::toast::messages(cx).is_empty(), "the notice is taken down");
            })
            .unwrap();
    }

    // slopty: a clone that fails says why, and adds nothing.
    #[gpui::test]
    fn a_failed_clone_says_why_and_adds_nothing(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = clone_typed(cx, "local", "git@example.com:srv/origin.git");
        window
            .update(cx, |shell, _, cx| shell.add_space_open_active(cx))
            .unwrap();
        let asked = engine.take(methods::CLONE_REPO, cx);
        assert!(asked[0]["params"].get("targetDeviceId").is_none(), "this machine's own");
        engine.fail(&asked[0], "fatal: repository not found");
        cx.run_until_parked();
        assert!(engine.take(methods::MUTATE, cx).is_empty());
        window
            .read_with(cx, |shell, cx| {
                assert!(shell.state.read(cx).spaces.is_empty());
                let said = crate::toast::messages(cx);
                assert_eq!(said.len(), 1);
                assert!(said[0].starts_with("Couldn\u{2019}t clone origin."), "{said:?}");
                let why = crate::toast::details(cx);
                assert_eq!(why[0].as_deref(), Some("fatal: repository not found"), "why, behind Copy error");
            })
            .unwrap();
    }

    // slopty: what is not an address clones nothing; ← goes back to the folders.
    #[gpui::test]
    fn a_non_address_clones_nothing_and_back_returns_to_the_folders(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = clone_typed(cx, "local", "not a url");
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.add_space_clone_target(cx), None);
                shell.add_space_open_active(cx);
                assert!(shell.add_space.is_some(), "still asking");
                shell.add_space_go_up(cx);
                let flow = shell.add_space.as_ref().unwrap();
                assert!(!flow.cloning);
                assert_eq!(flow.step, ProjectStep::Folders);
                assert!(flow.search.read(cx).text().is_empty());
            })
            .unwrap();
        assert!(engine.take(methods::CLONE_REPO, cx).is_empty());
    }

    #[gpui::test]
    fn closing_or_leaving_home_releases_the_index(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "remote");
        go_home(window, cx);
        let warm = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        assert_eq!(warm[0]["params"]["targetDeviceId"], "remote");
        // A drive location neither searches nor keeps the home index.
        window
            .update(cx, |shell, _, cx| {
                shell.add_space_goto_location("Data".into(), Some("/mnt/data".into()), cx)
            })
            .unwrap();
        let released = engine.take(methods::RELEASE_HOME_FOLDER_SEARCH, cx);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0]["params"]["targetDeviceId"], "remote");
        type_query(window, "comet", cx);
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());

        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        window
            .update(cx, |shell, _, cx| shell.close_add_space(cx))
            .unwrap();
        assert_eq!(
            engine.take(methods::RELEASE_HOME_FOLDER_SEARCH, cx).len(),
            1
        );
        // Nothing left to release.
        window
            .update(cx, |shell, _, cx| shell.close_add_space(cx))
            .unwrap();
        assert!(
            engine
                .take(methods::RELEASE_HOME_FOLDER_SEARCH, cx)
                .is_empty()
        );
    }

    #[gpui::test]
    fn an_older_host_is_not_asked_again(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        let warm = engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        engine.fail(&warm[0], "unknown method: SearchHomeFolders");
        cx.run_until_parked();
        type_query(window, "comet", cx);
        assert!(engine.take(methods::SEARCH_HOME_FOLDERS, cx).is_empty());
        window
            .update(cx, |shell, _, cx| shell.close_add_space(cx))
            .unwrap();
        assert!(
            engine
                .take(methods::RELEASE_HOME_FOLDER_SEARCH, cx)
                .is_empty()
        );
    }

    /// The platform's ⌘ (Ctrl elsewhere), as the key handlers read it.
    fn secondary(cx: &mut TestAppContext) -> gpui::Modifiers {
        cx.update(|cx| gpui::Modifiers::secondary_key(cx))
    }

    fn press(
        window: gpui::WindowHandle<Shell>,
        key: &str,
        modifiers: gpui::Modifiers,
        cx: &mut TestAppContext,
    ) {
        window
            .update(cx, |shell, _, cx| {
                shell.add_space_key(
                    &gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            modifiers,
                            key: key.into(),
                            key_char: None,
                        },
                        is_held: false,
                        prefer_character_input: false,
                    },
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();
    }

    fn path_input(
        window: gpui::WindowHandle<Shell>,
        cx: &mut TestAppContext,
    ) -> Entity<ComposerInput> {
        window
            .read_with(cx, |shell, _| {
                let flow = shell.add_space.as_ref().unwrap();
                flow.path_edit
                    .as_ref()
                    .expect("path field open")
                    .input
                    .clone()
            })
            .unwrap()
    }

    fn type_path(window: gpui::WindowHandle<Shell>, text: &str, cx: &mut TestAppContext) {
        let input = path_input(window, cx);
        input.update(cx, |input, cx| input.set_text(text, cx));
        cx.run_until_parked();
    }

    /// ↵ in the path field, through the field's own Submitted event.
    fn submit_path(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) {
        let input = path_input(window, cx);
        input.update(cx, |_, cx| cx.emit(ComposerInputEvent::Submitted));
        cx.run_until_parked();
    }

    /// The ListFolders asking for `path`; the others (⇥ listing the typed
    /// path's parent) answer empty.
    fn listing_request(
        engine: &mut FakeEngine,
        path: &str,
        cx: &mut TestAppContext,
    ) -> serde_json::Value {
        let mut found = None;
        for request in engine.take(methods::LIST_FOLDERS, cx) {
            if found.is_none() && request["params"]["path"] == path {
                found = Some(request);
            } else {
                let listed = request["params"]["path"].clone();
                engine.reply(
                    &request,
                    serde_json::json!({ "path": listed, "entries": [] }),
                );
            }
        }
        found.unwrap_or_else(|| panic!("no ListFolders for {path}"))
    }

    #[gpui::test]
    fn a_typed_path_opens_once_it_lists_and_a_missing_one_keeps_the_field(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        // ⌘L: the trail becomes the open folder's path, ready to replace.
        press(window, "l", secondary(cx), cx);
        assert_eq!(
            path_input(window, cx).read_with(cx, |input, _| input.text().to_owned()),
            "~/"
        );

        type_path(window, "~/code/../notes", cx);
        submit_path(window, cx);
        let missing = listing_request(&mut engine, "/home/me/notes", cx);
        engine.fail(
            &missing,
            "could not read that folder: No such file or directory (os error 2)",
        );
        cx.run_until_parked();
        window
            .read_with(cx, |shell, _| {
                let flow = shell.add_space.as_ref().unwrap();
                let edit = flow.path_edit.as_ref().expect("the field stays");
                assert_eq!(edit.error.as_deref(), Some("No folder at ~/notes"));
                assert!(!edit.opening);
                // The folder that was open is still the one listed.
                assert_eq!(flow.browser.ready().unwrap().path, "/home/me");
            })
            .unwrap();

        type_path(window, "/srv/data", cx);
        window
            .read_with(cx, |shell, _| {
                let edit = shell
                    .add_space
                    .as_ref()
                    .unwrap()
                    .path_edit
                    .as_ref()
                    .unwrap();
                assert!(edit.error.is_none(), "an edit clears the error");
            })
            .unwrap();
        submit_path(window, cx);
        let found = listing_request(&mut engine, "/srv/data", cx);
        assert!(found["params"].get("targetDeviceId").is_none());
        engine.reply(
            &found,
            serde_json::json!({
                "path": "/srv/data",
                "entries": [{ "name": "app", "isDir": true, "isRepo": true }],
            }),
        );
        cx.run_until_parked();
        window
            .read_with(cx, |shell, cx| {
                let flow = shell.add_space.as_ref().unwrap();
                assert!(flow.path_edit.is_none(), "the crumbs are back");
                assert_eq!(flow.step, ProjectStep::Folders);
                assert_eq!(flow.browser.ready().unwrap().path, "/srv/data");
                // Outside home and every drive: the filesystem root holds it.
                assert_eq!(flow.location, Some(("/".into(), Some("/".into()))));
                assert!(matches!(
                    shell.add_space_folder_rows(cx).as_slice(),
                    [FolderRow::Local(entry)] if entry.name == "app"
                ));
            })
            .unwrap();
        assert!(engine.take(methods::MUTATE, cx).is_empty(), "↵ only opens");
    }

    #[gpui::test]
    fn mod_enter_in_the_path_field_adds_the_typed_folder(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "remote");
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        press(window, "l", secondary(cx), cx);
        type_path(window, "'~/code/My\\ App'", cx);
        press(window, "enter", secondary(cx), cx);
        let request = listing_request(&mut engine, "/home/me/code/My App", cx);
        assert_eq!(request["params"]["targetDeviceId"], "remote");
        engine.reply(
            &request,
            serde_json::json!({ "path": "/home/me/code/My App", "entries": [] }),
        );
        let created = engine.take(methods::MUTATE, cx);
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["params"]["op"], "createSpace");
        assert_eq!(created[0]["params"]["deviceId"], "remote");
        assert_eq!(created[0]["params"]["path"], "/home/me/code/My App");
        window
            .read_with(cx, |shell, _| {
                let flow = shell.add_space.as_ref().unwrap();
                // Home holds it: the trail reads Home › code › My App.
                assert_eq!(flow.location, Some(("Home".into(), None)));
                assert!(flow.submit_busy);
            })
            .unwrap();
    }

    #[gpui::test]
    fn mod_l_on_the_device_list_types_a_path_with_tab_completion(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        window
            .update(cx, |shell, _, cx| {
                shell.add_space_back_to(ProjectStep::Devices, cx)
            })
            .unwrap();
        press(window, "l", secondary(cx), cx);
        window
            .read_with(cx, |shell, cx| {
                let flow = shell.add_space.as_ref().unwrap();
                assert_eq!(flow.step, ProjectStep::Locations);
                assert_eq!(flow.device.as_ref().unwrap().id, "local");
                let edit = flow.path_edit.as_ref().expect("path field open");
                // Home isn't known yet: `~/` stands for it.
                assert_eq!(edit.input.read(cx).text(), "~/");
            })
            .unwrap();
        // Listing home resolves `~` and completes its folders.
        let home = engine
            .take(methods::LIST_FOLDERS, cx)
            .into_iter()
            .find(|request| request["params"].get("path").is_none())
            .expect("home listed");
        let listing = engine.listing.clone();
        engine.reply(&home, listing);
        cx.run_until_parked();
        type_path(window, "~/CO", cx);
        window
            .read_with(cx, |shell, cx| {
                assert_eq!(
                    shell.add_space.as_ref().unwrap().home.as_deref(),
                    Some("/home/me")
                );
                assert_eq!(
                    shell.add_space_path_completion(cx),
                    Some(("~/code/".into(), Some("de".into())))
                );
            })
            .unwrap();
        press(window, "tab", gpui::Modifiers::none(), cx);
        assert_eq!(
            path_input(window, cx).read_with(cx, |input, _| input.text().to_owned()),
            "~/code/"
        );
        // The next level lists the completed folder.
        listing_request(&mut engine, "/home/me/code", cx);
        // ⌘L again selects the path rather than reopening the field.
        press(window, "l", secondary(cx), cx);
        assert_eq!(
            path_input(window, cx).read_with(cx, |input, _| input.text().to_owned()),
            "~/code/"
        );
    }

    /// What drawing the whole Shell needs beyond `picker`'s globals.
    fn renderable(cx: &mut TestAppContext) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // slopty: no history to start.
        cx.update(|cx| {
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        dir
    }

    /// A connected, local Shell passes its gate and draws its page, and the
    /// palette over it.
    fn connect(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) {
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.connection = slopty_view_proto::view::ConnectionStatus::Ready;
                    state.workspace_scope = Some(slopty_view_proto::WorkspaceScope::Development);
                })
            })
            .unwrap();
    }

    /// Draws the window, so keys take gpui's real route: bindings, the
    /// Shell root's capture listeners, then the palette's own handler.
    fn draw(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) {
        cx.run_until_parked();
        // The window, not the Shell: drawing updates the Shell itself.
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    }

    fn focused(
        window: gpui::WindowHandle<Shell>,
        input: &Entity<ComposerInput>,
        cx: &mut TestAppContext,
    ) -> bool {
        cx.update_window(window.into(), |_, window, cx| {
            input.focus_handle(cx).is_focused(window)
        })
        .unwrap()
    }

    #[gpui::test]
    fn escape_in_the_path_field_puts_the_crumbs_back(cx: &mut TestAppContext) {
        let _settings = renderable(cx);
        let (window, _dir, mut engine) = picker(cx, "local");
        connect(window, cx);
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        press(window, "l", secondary(cx), cx);
        type_path(window, "/srv", cx);
        draw(window, cx);
        assert!(focused(window, &path_input(window, cx), cx));
        cx.simulate_keystrokes(window.into(), "escape");
        window
            .read_with(cx, |shell, _| {
                let flow = shell.add_space.as_ref().expect("the palette stays open");
                assert!(flow.path_edit.is_none());
                assert_eq!(flow.browser.ready().unwrap().path, "/home/me");
            })
            .unwrap();
        // Esc again closes the palette as before.
        draw(window, cx);
        cx.simulate_keystrokes(window.into(), "escape");
        window
            .read_with(cx, |shell, _| assert!(shell.add_space.is_none()))
            .unwrap();
    }

    #[gpui::test]
    fn tab_keys_keep_the_keyboard_in_the_palette(cx: &mut TestAppContext) {
        let _settings = renderable(cx);
        let (window, _dir, mut engine) = picker(cx, "local");
        connect(window, cx);
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        let search = window
            .read_with(cx, |shell, _| {
                shell.add_space.as_ref().unwrap().search.clone()
            })
            .unwrap();
        draw(window, cx);
        assert!(focused(window, &search, cx));
        // Not the Shell's focus walk to the controls behind the palette.
        cx.simulate_keystrokes(window.into(), "tab");
        draw(window, cx);
        assert!(focused(window, &search, cx), "⇥ in the search");
        press(window, "l", secondary(cx), cx);
        let field = path_input(window, cx);
        for keys in ["tab", "shift-tab"] {
            draw(window, cx);
            cx.simulate_keystrokes(window.into(), keys);
            draw(window, cx);
            assert!(focused(window, &field, cx), "{keys} in the path field");
        }
    }

    #[gpui::test]
    fn enter_during_a_mod_enter_open_still_adds_the_project(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        press(window, "l", secondary(cx), cx);
        type_path(window, "~/code", cx);
        press(window, "enter", secondary(cx), cx);
        // ↵ while ⌘↵'s listing is on its way.
        submit_path(window, cx);
        let requests = engine.take(methods::LIST_FOLDERS, cx);
        let opens: Vec<_> = requests
            .iter()
            .filter(|request| request["params"]["path"] == "/home/me/code")
            .collect();
        assert_eq!(opens.len(), 1, "↵ doesn't start the open over");
        engine.reply(
            opens[0],
            serde_json::json!({ "path": "/home/me/code", "entries": [] }),
        );
        let created = engine.take(methods::MUTATE, cx);
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["params"]["path"], "/home/me/code");
    }

    #[gpui::test]
    fn tab_keeps_a_name_typed_exactly_beside_its_other_case(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        engine.listing = serde_json::json!({
            "path": "/home/me",
            "entries": [
                { "name": "Code", "isDir": true, "isRepo": false },
                { "name": "code", "isDir": true, "isRepo": false },
            ],
        });
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        press(window, "l", secondary(cx), cx);
        let completion = |text: &str, cx: &mut TestAppContext| {
            type_path(window, text, cx);
            window
                .read_with(cx, |shell, cx| shell.add_space_path_completion(cx))
                .unwrap()
        };
        assert_eq!(completion("~/code", cx), Some(("~/code/".into(), None)));
        assert_eq!(
            completion("~/co", cx),
            Some(("~/Code/".into(), Some("de".into())))
        );
    }

    #[gpui::test]
    fn a_typed_path_lands_under_home_when_home_answers_last(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "remote");
        // ⌘L on the device list: nothing has listed the device's home yet.
        window
            .update(cx, |shell, _, cx| {
                shell.add_space_back_to(ProjectStep::Devices, cx)
            })
            .unwrap();
        press(window, "l", secondary(cx), cx);
        type_path(window, "/home/me/code", cx);
        submit_path(window, cx);
        let requests = engine.take(methods::LIST_FOLDERS, cx);
        let open = requests
            .iter()
            .find(|request| request["params"]["path"] == "/home/me/code")
            .expect("the typed path listed");
        engine.reply(
            open,
            serde_json::json!({ "path": "/home/me/code", "entries": [] }),
        );
        cx.run_until_parked();
        for home in requests
            .iter()
            .filter(|request| request["params"].get("path").is_none())
        {
            engine.reply(home, engine.listing.clone());
        }
        cx.run_until_parked();
        window
            .read_with(cx, |shell, _| {
                let flow = shell.add_space.as_ref().unwrap();
                assert_eq!(flow.browser.ready().unwrap().path, "/home/me/code");
                assert_eq!(flow.location, Some(("Home".into(), None)));
            })
            .unwrap();
    }

    #[gpui::test]
    fn relative_paths_start_from_the_folder_still_loading(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = picker(cx, "local");
        go_home(window, cx);
        engine.take(methods::SEARCH_HOME_FOLDERS, cx);
        // Into ~/code, whose listing hasn't answered.
        window
            .update(cx, |shell, _, cx| {
                shell.load_space_folders(Some("/home/me/code".into()), cx)
            })
            .unwrap();
        press(window, "l", secondary(cx), cx);
        assert_eq!(
            path_input(window, cx).read_with(cx, |input, _| input.text().to_owned()),
            "~/code/"
        );
        type_path(window, "api", cx);
        submit_path(window, cx);
        listing_request(&mut engine, "/home/me/code/api", cx);
    }

    #[test]
    fn typed_paths_open_under_home_the_deepest_drive_or_their_root() {
        let drives = [
            DriveEntry {
                name: "Macintosh HD".into(),
                path: "/".into(),
            },
            DriveEntry {
                name: "Data".into(),
                path: "/Volumes/Data".into(),
            },
        ];
        let home = Some("/Users/ana");
        assert_eq!(
            typed_path_location("/Users/ana/code", home, &drives),
            ("Home".into(), None)
        );
        assert_eq!(
            typed_path_location("/Volumes/Data/work", home, &drives),
            ("Data".into(), Some("/Volumes/Data".into()))
        );
        assert_eq!(
            typed_path_location("/opt/tools", home, &drives),
            ("Macintosh HD".into(), Some("/".into()))
        );
        assert_eq!(
            typed_path_location(r"D:\Random\slopty", None, &[]),
            (r"D:\".into(), Some(r"D:\".into()))
        );
    }

    #[test]
    fn paths_in_home_are_spelled_from_tilde() {
        let home = Some("/Users/ana");
        assert_eq!(tilde_path("/Users/ana", home), "~");
        assert_eq!(tilde_path("/Users/ana/code/work", home), "~/code/work");
        assert_eq!(tilde_path("/Users/anabel", home), "/Users/anabel");
        assert_eq!(tilde_path("/opt/tools", home), "/opt/tools");
        assert_eq!(tilde_path("/Users/ana/code", None), "/Users/ana/code");
        // A home at the root would put every path under `~`.
        assert_eq!(tilde_path("/srv", Some("/")), "/srv");
        let home = Some(r"C:\Users\ana");
        assert_eq!(tilde_path(r"c:\Users\ana\code", home), r"~\code");
        assert_eq!(tilde_path(r"D:\code", home), r"D:\code");
        // Spelled from `~`, a path still resolves to itself.
        assert_eq!(
            resolve_typed_path(&tilde_path(r"C:\Users\ana\code", home), None, home, false)
                .as_deref(),
            Some(r"C:\Users\ana\code")
        );
    }

    #[test]
    fn device_errors_read_as_missing_folders_or_files() {
        assert_eq!(
            path_open_error(
                "/x",
                "could not read that folder: No such file or directory (os error 2)"
            ),
            "No folder at /x"
        );
        assert_eq!(
            path_open_error(
                "/x",
                "could not read that folder: Not a directory (os error 20)"
            ),
            // `/x` itself, or a folder on its way, is a file.
            "/x isn't a folder"
        );
        assert_eq!(
            path_open_error(
                r"D:\x",
                "could not read that folder: The system cannot find the path specified. (os error 3)"
            ),
            r"No folder at D:\x"
        );
        let denied = "Slopty doesn't have access to this folder on the device.";
        assert_eq!(path_open_error("/x", denied), denied);
    }
}
