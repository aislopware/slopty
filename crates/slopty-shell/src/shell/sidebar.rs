//! slopty: the session sidebar as `impl Shell`: its column, the list body and its rows, the
//! connection pill, and the server foot with its menu (`docs/decisions/layout.md`).

use super::*;

/// `connected` already includes the engine's degradation grace. A brief
/// focus-triggered dial needs no sidebar status; queued changes or a sustained
/// outage still deserve one.
fn chat_sync_pill_caption(chat: &slopty_view_proto::ChatConnectivity) -> Option<&'static str> {
    use slopty_view_proto::ChatSyncState as S;
    let sustained_or_queued = !chat.connected || chat.pending_pushes > 0;
    match chat.sync_state {
        S::Waiting if sustained_or_queued => Some("Sync queued — changes are saved"),
        S::Connecting if sustained_or_queued => Some("Syncing…"),
        S::Offline if !chat.connected => Some("Offline — changes are saved"),
        _ => None,
    }
}

/// Sidebar resort glide (feature-inventory §1.6): 260ms
/// `cubic-bezier(0.22,1,0.36,1)` per-row translate, the View Transitions
/// equivalent.
const RESORT: MotionSpec = MotionSpec::new(260, motion::EASE_RESORT);

/// FLIP diff for a keyed list: given the previously rendered order and the new
/// order (key + row height), return each surviving key's paint-only start
/// offset `old_y - new_y` (only keys whose position actually moved). `gap` is
/// the flex gap between rows. Pure — drives the sidebar resort glide.
fn resort_offsets(
    old: &[(String, f32)],
    new: &[(String, f32)],
    gap: f32,
) -> std::collections::HashMap<String, f32> {
    let mut old_y = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in old {
        old_y.insert(key.as_str(), y);
        y += height + gap;
    }
    let mut offsets = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in new {
        if let Some(prev) = old_y.get(key.as_str()) {
            let dy = prev - y;
            if dy.abs() > 0.5 {
                offsets.insert(key.clone(), dy);
            }
        }
        y += height + gap;
    }
    offsets
}

/// Height changes do not constitute a list reorder. In particular, sidebar
/// disclosures animate their own height and must not also trigger FLIP offsets
/// on every following keyed section.
fn sidebar_key_order_changed(old: &[(String, f32)], new: &[(String, f32)]) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|((old_key, _), (new_key, _))| old_key != new_key)
}

/// Exact active-session row height. Harness identity lives on the title line
/// and the Working glyph lives in the status corner, so neither adds a third
/// line. Compact rows omit the metadata line and its preceding gap entirely;
/// branch / pull-request rows add the exact height of their tallest child.
/// Keeping this calculation beside the renderer's metrics prevents disclosure
/// clips when view options alter the row structure.
fn chat_row_height(shows_branch: bool, shows_pull_request: bool) -> f32 {
    let mut metadata_height: f32 = 0.0;
    if shows_branch {
        metadata_height = metadata_height.max(14.0);
    }
    if shows_pull_request {
        metadata_height = metadata_height.max(16.0);
    }
    if metadata_height == 0.0 {
        45.0
    } else {
        47.0 + metadata_height
    }
}
// slopty: one sidebar row of one line (`docs/decisions/layout.md`, "The session list is stable"
// and "A state is a glyph"): the program's icon, the title and its quiet hint, and the state's
// mark, 29 tall.
pub(super) const SIDEBAR_ROW_HEIGHT: f32 = 29.0;

pub(super) const SIDEBAR_LIST_PAD_TOP: f32 = 4.0;

/// slopty: the sidebar's width while it is put away: the rail of one rolled-up mark per project
/// (`docs/decisions/layout.md`, "The session list is stable").
pub(super) const SIDEBAR_RAIL_WIDTH: f32 = 40.0;
/// A rail entry's slot, and its project's icon within it.
const RAIL_SLOT: f32 = 30.0;
const RAIL_ICON: f32 = 18.0;

/// Active and archived sessions share harness/title geometry.
const SIDEBAR_ACTIVE_HARNESS_ICON_SIZE: f32 = 13.0;
/// slopty: a sidebar row's leading slot (its program's mark) and its trailing one (its state's),
/// and the air between the leading one and the title (opencode's 16 px avatar slot).
pub(super) const SIDEBAR_LEADING_SLOT: f32 = 16.0;
pub(super) const SIDEBAR_LEADING_GAP: f32 = 6.0;
/// The jump hint's slot while a text hint ("Ctrl+9") stands at a row's end: wide enough for
/// the widest of them at 11px on one line. In default-size pixels; scaled with the UI font via
/// `ui_rems`.
const COMPACT_JUMP_HINT_WIDTH: f32 = 42.0;
const SIDEBAR_ACTIVE_HARNESS_TITLE_GAP: f32 = Theme::SPACE_SM;

const SIDEBAR_FOOTER_AVATAR_SIZE: f32 = 16.0;

/// Keep the fade short so only the last few glyphs recede. Tracking clipped
/// content lets the shared paint-time overflow gate leave fitting labels intact.
pub(super) fn sidebar_faded_label(
    id: SharedString,
    fill: bool,
    label: impl IntoElement,
) -> impl IntoElement {
    let overflow = gpui::ScrollHandle::new();
    crate::edge_fade::edge_faded(
        20.0,
        false,
        false,
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .when(fill, |el| el.flex_1())
            .min_w_0()
            .overflow_hidden()
            .track_scroll(&overflow)
            .flex()
            .child(div().flex_none().whitespace_nowrap().child(label)),
    )
    .fade_right(true)
    .fade_label_overflow(&overflow)
}

/// Square action beside the account trigger in the sidebar footer.
const SIDEBAR_FOOTER_ACTION_SIZE: f32 = SIDEBAR_FOOTER_BUTTON_SIZE;
/// Breathing room between the account trigger and its adjacent action.
const SIDEBAR_FOOTER_ACTION_GAP: f32 = 4.0;

/// A chat row's inline title editor, swapped in for its faded title while
/// the chat is renamed in place: the title's 13/17 type in an accent-ringed
/// field.
fn chat_title_editor(
    id: SharedString,
    input: Entity<ComposerInput>,
    theme: &Theme,
) -> AnyElement {
    div()
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .flex_1()
        .min_w_0()
        .h(px(21.0))
        .my(px(-2.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.input_glass_bg())
        .text_color(theme.text)
        .cursor_text()
        // The field is not the row: clicks place the caret instead of
        // opening the chat or starting a drag.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(div().flex_1().min_w_0().overflow_hidden().child(input))
        .into_any_element()
}

/// The sidebar footer's profile label and the account menu's identity line.
/// Anything without a signed-in account reads "Local"; the menu carries the
/// storage/sync detail.
// slopty: the foot names the server this client follows, not an account: its name, a line
// saying whether the link is up, and that state for the avatar's dot (`None` without a server).
fn sidebar_server_identity(
    server: Option<&slopty_view_proto::ServerPresence>,
) -> (SharedString, SharedString, Option<bool>) {
    match server {
        Some(server) => {
            let line = if server.online { "Connected" } else { "Offline" };
            (server.name.clone().into(), line.into(), Some(server.online))
        }
        None => ("No server".into(), "Not following a server".into(), None),
    }
}

// slopty: a row's archive pill names ⇧⌘A only on the open chat's row, the one the key archives.
/// The tooltip of a sidebar row's archive pill, above it (below, it would cover the next row).
fn archive_tooltip(
    archived: bool,
    open: bool,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    use crate::settings::widgets::{action_tooltip, text_tooltip};
    let tooltip: Box<dyn Fn(&mut Window, &mut App) -> gpui::AnyView> = match (archived, open) {
        (true, _) => Box::new(text_tooltip("Unarchive chat")),
        (false, true) => Box::new(action_tooltip(
            ShortcutId::ArchiveSession.label(),
            Keys::Action(Box::new(ArchiveSession)),
        )),
        (false, false) => Box::new(text_tooltip(ShortcutId::ArchiveSession.label())),
    };
    move |window, cx| tooltip(window, cx)
}

impl Shell {
    /// Scroll the sidebar so the renamed row (painted at `row`) clears the
    /// list's edge fades, while the rename is still revealing it.
    fn keep_rename_row_in_view(&mut self, row: gpui::Bounds<Pixels>, cx: &mut Context<Self>) {
        if !self.chat_rename_revealing() {
            return;
        }
        let viewport = self.sidebar_scroll.bounds();
        let band = px(SIDEBAR_GLASS_FADE_BAND);
        let delta = if row.top() < viewport.top() + band {
            row.top() - (viewport.top() + band)
        } else if row.bottom() > viewport.bottom() - band {
            row.bottom() - (viewport.bottom() - band)
        } else {
            return;
        };
        let offset = self.sidebar_scroll.offset();
        let max = self.sidebar_scroll.max_offset().y;
        let y = (offset.y - delta).clamp(-max, px(0.0));
        if y != offset.y {
            self.sidebar_scroll.set_offset(gpui::point(offset.x, y));
            cx.notify();
        }
    }

    /// The inline title editor for `chat_id`'s sidebar row, if one is open.
    pub(super) fn rename_input_for(&self, chat_id: &str) -> Option<Entity<ComposerInput>> {
        self.chat_rename
            .as_ref()
            .filter(|rename| rename.chat_id == chat_id)
            .map(|rename| rename.input.clone())
    }

    /// Move the row hover (wash + archive pill) onto or off `row_id` to match a
    /// pointer that has not moved since the last archive-pill click. Releasing
    /// matters as much as adopting: an archived row keeps its hover key when it
    /// lands in the Archived section, and would otherwise stay lit there.
    fn sync_chat_row_hover(&mut self, row_id: String, hovered: bool, cx: &mut Context<Self>) {
        let current = self.chat_status_hover.as_deref() == Some(row_id.as_str());
        if !self.chat_hover_resync || current == hovered {
            return;
        }
        if hovered {
            if let Some(previous) = self.chat_status_hover.replace(row_id.clone()) {
                motion::set_hover(&format!("{previous}-hover"), false, self.reduced_motion.get());
            }
        } else {
            self.chat_status_hover = None;
        }
        motion::set_hover(&format!("{row_id}-hover"), hovered, self.reduced_motion.get());
        cx.notify();
    }

    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // The sidebar is part of the resolved theme. A second fixed-Slopty
        // palette here made imported families look split in half and froze
        // activity/glyph personality independently of the selected variant.
        let inner = self.sidebar_pane.clone().cached(
            gpui::StyleRefinement::default()
                .w(px(self.sidebar_content_width()))
                .h_full()
                .flex_none(),
        );
        // Transparent — the sidebar sits directly on the frost shell; the main
        // card's own border provides the separation. The content row spans the
        // full window height (the titlebar overlays it), so the column pads
        // itself below the chrome.
        div()
            .id("sidebar-column")
            .debug_selector(|| "sidebar-column".into())
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(self.sidebar_now()))
            // slopty: a hidden sidebar (fit away, a phone's chat page) draws nothing, so a screen
            // reader never reads rows that are off screen; put away, it leaves its rail.
            .when(self.sidebar_now() > 0.5, |el| {
                if self.settings.sidebar_collapsed && !self.compact() {
                    el.child(self.render_sidebar_rail(cx))
                } else {
                    el.child(div().h_full().pt(px(Theme::TITLEBAR_HEIGHT)).child(inner))
                }
            })
            .into_any_element()
    }

    /// slopty: the rail the put-away sidebar leaves (`docs/decisions/layout.md`, "The session
    /// list is stable"): per project, in the list's order, its icon wearing its loudest row's
    /// mark (`marks::loudest`), none while the project rests, so "which one needs me" has an
    /// answer with the list away. A press opens the project's loudest row, else its newest; a
    /// screen reader and the tooltip say the project, its count and what its mark says. Inactive,
    /// at rest by its nature, has no place on it.
    fn render_sidebar_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let groups = self.sidebar_list(cx);
        self.tick_sidebar_clock(cx);
        let mut entries: Vec<AnyElement> = Vec::new();
        for group in groups.into_iter().filter(|group| group.key != sidebar_status::INACTIVE) {
            let Some(first) = group.rows.first() else { continue };
            // A cached row's mark is the last word from a machine out of reach.
            let loudest = super::marks::loudest(group.rows.iter().filter(|row| !row.cached).map(|row| &row.mark)).cloned();
            let target = group
                .rows
                .iter()
                .find(|row| !row.cached && Some(&row.mark) == loudest.as_ref())
                .unwrap_or(first)
                .chat
                .id
                .clone();
            let count = group.rows.len() + group.hidden;
            let said = match loudest.as_ref().and_then(super::marks::Mark::word) {
                Some(word) => format!("{}, {count}, {word}", group.label),
                None => format!("{}, {count}", group.label),
            };
            let key = group.key.clone();
            let mark = loudest.as_ref().and_then(|mark| {
                super::marks::mark_element(
                    SharedString::from(format!("sidebar-rail-mark-{key}")),
                    format!("sidebar-rail-mark-{key}"),
                    mark,
                    crate::loaders::status_mark::MARK_SMALL,
                    theme.bg,
                )
            });
            // The artwork alone: the entry's own tooltip says the project and its state.
            let art = div().size(px(RAIL_ICON)).flex_none().child(self.project_icon_art(&first.chat.id, false, cx));
            let selector = format!("sidebar-rail-{key}");
            entries.push(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector)
                    .relative()
                    .size(px(RAIL_SLOT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.glass_hover()))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(said.clone()))
                    .tooltip(crate::settings::widgets::text_tooltip(said))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_chat(target.clone(), cx)))
                    .child(art)
                    .children(mark.map(|mark| div().absolute().right(px(-1.0)).bottom(px(-1.0)).child(mark)))
                    .into_any_element(),
            );
        }
        div()
            .id("sidebar-rail")
            .debug_selector(|| "sidebar-rail".into())
            .w(px(SIDEBAR_RAIL_WIDTH))
            .h_full()
            .pt(px(Theme::TITLEBAR_HEIGHT + SIDEBAR_LIST_PAD_TOP))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.0))
            .role(gpui::Role::List)
            .aria_label("Projects")
            .children(entries)
            .into_any_element()
    }

    /// One session row. The sidebar's (`search_query` none) is one line, about 28 pt
    /// (`docs/decisions/layout.md`, "The session list is stable"): the program's icon, the title
    /// the agent gave, and the state's mark (`marks`), with `title_hint`, quiet, after the title
    /// only when something tells the row from those beside it (`sidebar_status`). A row whose
    /// machine is out of reach dims and wears the cached glyph in its mark's place. The palette's
    /// copy spells out where the chat runs, its status or time, its branch and its pull request.
    /// Click selects; right-click opens the context menu.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_chat_row(
        &self,
        id: String,
        title: SharedString,
        // slopty: what tells this row from those beside it (`sidebar_status::Row::hint`), quiet
        // after its title.
        title_hint: Option<SharedString>,
        time_ago: SharedString,
        space_name: SharedString,
        branch: Option<SharedString>,
        change_request: Option<slopty_view_proto::ChangeRequestSummary>,
        harness: Option<slopty_view_proto::HarnessId>,
        status: slopty_view_proto::ChatIndicator,
        selected: bool,
        archived: bool,
        // This row's jump combo while the hint overlay is up. It takes the
        // corner outright — above hover and above the status word — so all
        // nine chips appear together instead of leaving a hole on whichever
        // row is busy or under the pointer.
        jump_label: Option<SharedString>,
        search_query: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Activity, not position (t3code Sidebar): status is a small colored
        // word + glyph in the row's top-right corner — Working animates the
        // composer-strip spinner, Done wears a check; Idle rows show the
        // relative time instead. Hovering the ROW swaps the corner for the
        // ARCHIVE button. Compact rows keep status first and elapsed time last;
        // their archive control occupies the remote-icon slot on hover.
        // A chat can appear on both surfaces at once. Namespace every hover
        // key and child id so the palette never animates the sidebar copy.
        let row_id = if search_query.is_some() {
            format!("palette-chat-{id}")
        } else {
            format!("chat-{id}")
        };
        let pr_device = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .map(|chat| chat.device_id.clone());
        // slopty: the sidebar draws one row (`SIDEBAR_ROW_HEIGHT`); the palette's copy spells out
        // where the chat runs, its branch and its pull request.
        let compact = search_query.is_none();
        let pr_surface = if search_query.is_some() {
            crate::change_requests::ChangeRequestBadgeSurface::Sidebar
        } else {
            crate::change_requests::ChangeRequestBadgeSurface::SessionRow
        };
        let corner_hovered = self.chat_status_hover.as_deref() == Some(row_id.as_str());
        let archived_muted = archived && search_query.is_none() && !selected && !corner_hovered;
        let content_id = id.clone();
        // slopty: the sidebar's rows read the sidebar's clock, as its groups do
        // (`spaces::Shell::sidebar_list`); the palette's copy reads the shell's.
        let now = if search_query.is_none() { self.sidebar_clock } else { crate::clock::utc(cx) };
        // slopty: the row stands in one of the five states (`marks`): its word, its hue and its
        // mark, one mark that morphs between them (`loaders::status_mark`), the tick cut through
        // its disc in the row's own ground. An agent at rest on its own work (a background
        // command, a subagent it left running) is at work; a result nobody has seen sets the
        // title in the heavier weight.
        let mark = super::marks::Mark::of(status, self.state.read(cx).session_for(&id), now);
        let status_label = mark.word();
        let status_color = mark.hue(theme).unwrap_or(theme.text_muted);
        let shows_metadata = branch.is_some() || change_request.is_some();
        let unread = mark.heavy() && !archived;
        // slopty: a sidebar row whose machine is out of reach shows the last word from there
        // (`sidebar_status::Row::cached`).
        let cached = compact
            && pr_device
                .as_deref()
                .is_some_and(|device| !self.state.read(cx).device_online(device, now));
        // slopty: a need's glyph says which need; its words are the hover card's and a screen
        // reader's, never a line of the row (`docs/decisions/layout.md`, "A state is a glyph").
        // A cached row asks nothing in its name.
        let need = match &mark {
            super::marks::Mark::NeedsYou(need) if !cached => Some(need.words.clone()),
            _ => None,
        };
        let mark_ground = if selected {
            crate::theme::flatten(crate::theme::glass_selected_bg(), theme.bg)
        } else {
            theme.bg
        };
        let status_mark = || {
            super::marks::mark_element(
                SharedString::from(format!("{row_id}-mark")),
                format!("chat-mark-{id}"), // slopty: for the tests
                &mark,
                crate::loaders::status_mark::MARK_SMALL,
                mark_ground,
            )
        };
        let subline = if search_query.is_some() {
            theme.text_muted
        } else {
            theme.text_muted.opacity(0.5)
        };
        // slopty: the program's own mark leads the row (a plain terminal's the terminal glyph),
        // in its full colour; an archived or cached row dims it with the rest of the row.
        let brand = harness.map(crate::pickers::harness_brand_icon);
        let dimmed = archived_muted || cached;
        let agent_mark = |(path, tint): (&'static str, Option<gpui::Hsla>)| {
            icon(path)
                .size(px(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE))
                .flex_none()
                .text_color(match tint {
                    Some(tint) if !dimmed => tint,
                    Some(tint) => tint.opacity(0.4),
                    None if dimmed => subline.opacity(0.4),
                    None => subline.opacity(0.8),
                })
        };
        let program = compact.then(|| {
            div()
                .debug_selector({
                    let id = id.clone();
                    move || format!("chat-agent-{id}") // slopty: for the tests
                })
                .size(px(SIDEBAR_LEADING_SLOT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(agent_mark(brand.unwrap_or((icons::TERMINAL, None))))
                .into_any_element()
        });
        // slopty: the state's mark trails the title in a slot of its own, kept while at rest so
        // every title ends at one edge; a cached row's slot says so instead.
        let mark_slot = compact.then(|| {
            let glyph = if cached {
                Some(
                    div()
                        .debug_selector({
                            let id = id.clone();
                            move || format!("chat-cached-{id}") // slopty: for the tests
                        })
                        .child(icon(icons::WIFI_OFF).size(px(12.0)).text_color(theme.text_muted))
                        .into_any_element(),
                )
            } else {
                status_mark()
            };
            let said = if cached { Some("Cached") } else { status_label };
            div()
                .id(SharedString::from(format!("{row_id}-status")))
                .debug_selector({
                    let id = id.clone();
                    move || format!("chat-status-{id}")
                })
                .size(px(SIDEBAR_LEADING_SLOT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                // slopty: a node needs a role to be one of the tree a screen reader reads; a row
                // at rest has nothing to say. A need's words are its description.
                .when_some(said, |el, label| el.role(gpui::Role::Status).aria_label(label))
                .when_some(need.clone(), |el, need| el.aria_description(need))
                .children(glyph)
                .into_any_element()
        });
        let compact_jump_label = jump_label.filter(|_| compact);
        // slopty: the palette's copy says its status, or its time, in its corner; the sidebar's
        // rows draw their own trailing cluster.
        let corner_body: AnyElement = {
            match status_label {
                Some(label) => {
                    // slopty: the state's mark beside its word (`marks`).
                    let glyph = status_mark().unwrap_or_else(|| div().into_any_element());
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                // slopty: the status says itself to a screen reader, as the
                                // compact row's does.
                                .id(SharedString::from(format!("{row_id}-status")))
                                .role(gpui::Role::Status)
                                .aria_label(label)
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.0))
                                // slopty: one 11-square slot for every status's glyph, so each
                                // word starts at one x.
                                .child(
                                    div()
                                        .size(px(11.0))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(glyph),
                                )
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(status_color)
                                        .child(SharedString::from(label)),
                                ),
                        )
                        .into_any_element()
                }
                None => div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    // slopty: tabular figures, so "1m" and "11m" keep the column.
                    .font_features(crate::typography::tabular())
                    .child(time_ago.clone())
                    .into_any_element(),
            }
        };
        // slopty: the sidebar row's archive control sits in its trailing cluster.
        let corner: AnyElement = {
            div()
                .id(SharedString::from(format!("{row_id}-corner")))
                .debug_selector({
                    let row_id = row_id.clone();
                    move || format!("{row_id}-corner")
                })
                .aria_label(status_label.unwrap_or("Idle"))
                .flex_none()
                .h(px(14.0))
                .flex()
                .items_center()
                .child(corner_body)
                .into_any_element()
        };
        let mut corner = Some(corner);
        let (hover, text) = (theme.glass_hover(), theme.text);
        let selected_wash = crate::theme::glass_selected_bg();
        let select_id = id.clone();
        let menu_id = id.clone();
        // slopty: the row as a screen reader names it, with what tells it from the rows beside it.
        let row_label = match &title_hint {
            Some(hint) => SharedString::from(format!("{title}, {hint}")),
            None => title.clone(),
        };
        let search_query_none = search_query.is_none();
        let rename_input = search_query_none
            .then(|| self.rename_input_for(&id))
            .flatten();
        let rolling_title = search_query_none
            .then(|| {
                crate::roll_text::rolling(
                    format!("sidebar-title-{id}"),
                    title.clone(),
                    motion::reduced_motion(cx),
                    crate::clock::now(cx),
                )
            })
            .flatten();
        // Sidebar rows only: the palette's copy already spells out its row,
        // and a row being renamed keeps the pointer's attention on the field.
        let hover_card = (search_query_none
            && rename_input.is_none()
            && self.session_hover_card.shown() == Some(id.as_str()))
        .then(|| {
            self.render_session_hover_card(&id, cx)
        })
        .flatten();
        let card_id = id.clone();
        // Hover fades over transition-colors (slopty session-row.tsx) — both
        // the wash and the title brighten ride the same 150ms blend.
        let fade_key = format!("{row_id}-hover");
        let rest_bg = if selected {
            selected_wash
        } else {
            crate::theme::wash(0.0)
        };
        // A selected row must NOT drift toward the hover wash: in dark the two
        // fills are identical so the blend is a no-op, but light's hover sits
        // below its near-opaque selected fill, and blending toward it visibly
        // dimmed the active row under the pointer (user report).
        // slopty: under the pointer the selected row goes a step past its selection, as Claude
        // desktop's (`theme::selected_hover_wash`), so the pointer reads on it too.
        let hover_bg = if selected { crate::theme::selected_hover_wash() } else { hover };
        // slopty: an unread result reads at full strength, as a selected row does; a cached
        // row recedes, as an archived one.
        let rest_text = if cached {
            text.opacity(if selected { 0.7 } else { 0.45 })
        } else if selected || search_query.is_some() || unread {
            text
        } else if archived {
            text.opacity(0.55)
        } else {
            text.opacity(0.8)
        };
        div()
            .id(SharedString::from(row_id.clone()))
            .group("sidebar-session-row")
            .debug_selector({
                let row_id = row_id.clone();
                move || row_id.clone()
            })
            .h(px(if compact {
                SIDEBAR_ROW_HEIGHT
            } else {
                chat_row_height(branch.is_some(), change_request.is_some())
            }))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .rounded(px(if search_query.is_some() {
                popover::PALETTE_ITEM_RADIUS
            } else {
                8.0
            }))
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .text_color(motion::hover_blend(&fade_key, rest_text, text))
            .bg(motion::hover_blend(&fade_key, rest_bg, hover_bg))
            // slopty: the fades hold the row's wash while the sidebar is drawn from its last frame.
            .child(motion::fade_driver(fade_key.clone().into()))
            // No selection ring (user request) — the wash alone marks the
            // active row.
            // Row hover drives BOTH the wash blend and the corner's
            // status→Archive swap (one listener — gpui allows a single
            // hover listener per element).
            .on_hover({
                let fade_hover = motion::hover_listener(fade_key.clone());
                let hover_id = row_id.clone();
                cx.listener(move |this, hovered: &bool, window, cx| {
                    fade_hover(hovered, window, cx);
                    this.chat_hover_resync = false;
                    if search_query_none {
                        this.hover_session_card(&card_id, *hovered, cx);
                    }
                    if *hovered {
                        if this.chat_status_hover.as_deref() != Some(hover_id.as_str()) {
                            // slopty: the row the hover leaves hears no leave of its own when
                            // it moved from under a still pointer (an archived row, landing
                            // in the Archived section): its wash is let go here, or it stays
                            // lit there.
                            if let Some(previous) = this.chat_status_hover.replace(hover_id.clone()) {
                                motion::set_hover(&format!("{previous}-hover"), false, this.reduced_motion.get());
                            }
                            cx.notify();
                        }
                    } else if this.chat_status_hover.as_deref() == Some(hover_id.as_str()) {
                        this.chat_status_hover = None;
                        cx.notify();
                    }
                })
            })
            .cursor_pointer()
            // slopty: the row is a button named by its title; its hover card is zeron's
            // (`session_hover_card`), carrying Slopty's facts.
            .when(search_query_none, |el| {
                el.role(gpui::Role::Button).aria_label(row_label.clone())
            })
            // slopty: the list is one keyboard stop, its rows walked with Up and Down
            // (`spaces::SidebarKeys`); a press leaves the keyboard where it was.
            .when(search_query_none, |el| {
                let focus = self.sidebar_keys.focus(&id, cx);
                let key_id = id.clone();
                el.track_focus(&focus)
                    .tab_index(0)
                    .tab_stop(self.sidebar_keys.stops_at(&id))
                    .map(|el| crate::surface_chrome::focus_ring_inset(el, theme))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                        if this.sidebar_row_key(&key_id, &event.keystroke, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
            })
            // Pressing, scrolling or opening the menu puts the hover card
            // away: the row has the pointer's intent now.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_session_card(cx)),
            )
            .on_scroll(cx.listener(|this, _, _, cx| this.dismiss_session_card(cx)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                // The palette's copy of the row only opens; the sidebar's
                // double-click edits the title in place.
                if event.click_count() >= 2 && search_query_none {
                    this.open_rename_chat(select_id.clone(), cx);
                } else {
                    this.open_chat(select_id.clone(), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.dismiss_session_card(cx);
                    this.chat_menu.open(ChatMenuState {
                        tab: None,
                        chat_id: menu_id.clone(),
                        position: event.position,
                        page: ChatMenuPage::Root,
                    });
                    cx.notify();
                }),
            )
            // A row revealed for an inline rename reports where it painted,
            // frame by frame while its disclosure opens, so the list can
            // scroll it clear of the edges.
            .when(
                rename_input.is_some() && self.chat_rename_revealing(),
                |el| {
                    let shell = cx.weak_entity();
                    el.child(
                        gpui::canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                window.defer(cx, move |_, cx| {
                                    shell
                                        .update(cx, |this, cx| {
                                            this.keep_rename_row_in_view(bounds, cx)
                                        })
                                        .ok();
                                });
                            },
                        )
                        .absolute()
                        .inset_0(),
                    )
                },
            )
            .when(self.chat_hover_resync, |el| {
                let shell = cx.weak_entity();
                let row_id = row_id.clone();
                el.child(
                    gpui::canvas(
                        |bounds, window, _| {
                            window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
                        },
                        move |_, hitbox, window, cx| {
                            // The hit test is final by paint, so popovers
                            // over the row still occlude it.
                            let hovered = hitbox.is_hovered(window) && !cx.has_active_drag();
                            if hovered == corner_hovered {
                                return;
                            }
                            window.defer(cx, move |_, cx| {
                                shell
                                    .update(cx, |this, cx| {
                                        this.sync_chat_row_hover(row_id, hovered, cx)
                                    })
                                    .ok();
                            });
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            })
            // Check both shown and pending cards after layout/occlusion
            // changes under a stationary pointer. Unmounted rows are handled
            // by the container's reconciliation.
            .when(
                search_query_none
                    && rename_input.is_none()
                    && self.session_hover_card.target() == Some(id.as_str()),
                |el| {
                    let shell = cx.weak_entity();
                    let chat_id = id.clone();
                    el.child(
                        gpui::canvas(
                            |bounds, window, _| {
                                window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
                            },
                            move |_, hitbox, window, cx| {
                                if hitbox.is_hovered(window) {
                                    return;
                                }
                                window.defer(cx, move |_, cx| {
                                    shell
                                        .update(cx, |this, cx| {
                                            this.hover_session_card(&chat_id, false, cx)
                                        })
                                        .ok();
                                });
                            },
                        )
                        .absolute()
                        .inset_0(),
                    )
                },
            )
            .children(hover_card)
            // Line 1: "project @ device", status word / time-ago right.
            .when(!compact, |el| {
                el.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .child(sidebar_faded_label(
                            format!("chat-device-{content_id}").into(),
                            true,
                            div()
                                .text_size(crate::typography::ui_rems(11.0))
                                .line_height(px(14.0))
                                .text_color(subline)
                                .child(popover::search_highlight(space_name, search_query, theme)),
                        ))
                        .child(div().text_color(subline).children(corner.take())),
                )
            })
            // Line 2: harness identity belongs directly with the title,
            // instead of floating as unrelated metadata below it.
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(if compact {
                        SIDEBAR_LEADING_GAP
                    } else {
                        SIDEBAR_ACTIVE_HARNESS_TITLE_GAP
                    }))
                    .children(program)
                    // slopty: the palette's copy keeps the agent's mark beside its title; the
                    // sidebar's is in the leading slot.
                    .when_some(brand.filter(|_| !compact), |el, brand| el.child(agent_mark(brand)))
                    .child(match rename_input {
                        Some(input) => chat_title_editor(
                            format!("chat-title-editor-{content_id}").into(),
                            input,
                            theme,
                        ),
                        None => sidebar_faded_label(
                            format!("chat-title-{content_id}").into(),
                            true,
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(6.0))
                                .text_size(crate::typography::ui_rems(13.0))
                                .line_height(px(17.0))
                                .child(
                                    div()
                                        // slopty: the heavier weight marks a result nobody has
                                        // seen.
                                        .when(unread, |el| el.font_weight(gpui::FontWeight::SEMIBOLD))
                                        // slopty: a title the agent gives rises in over the old
                                        // (Amp's), on the house's roll.
                                        .child(rolling_title.unwrap_or_else(|| {
                                            popover::search_highlight(title, search_query, theme)
                                        })),
                                )
                                // slopty: what tells the row from those beside it, quiet after
                                // its title, as an editor's tabs of one file name do.
                                .when_some(title_hint, |el, hint| {
                                    let hint_id = format!("chat-hint-{content_id}");
                                    el.child(
                                        div()
                                            .debug_selector(move || hint_id)
                                            .flex_none()
                                            .text_color(theme.text_muted)
                                            .child(hint),
                                    )
                                }),
                        )
                        .into_any_element(),
                    })
                    // slopty: the sidebar row's trailing cluster: the archive control while
                    // the row is under the pointer, then the state's mark in its slot. While the
                    // jump keys' modifier is held, the row's key stands there instead; it wins
                    // over the hover archive (the person's ask).
                    .when(compact, |el| {
                        let archive_id = id.clone();
                        match compact_jump_label {
                            Some(label) => el.child(
                                div()
                                    .debug_selector({
                                        let id = format!("jump-hint-{id}"); // slopty: for the tests
                                        move || id
                                    })
                                    .flex_none()
                                    // A text-length hint ("Ctrl+1") keeps one column with
                                    // the others.
                                    .when(label.chars().count() > 3, |el| {
                                        el.min_w(crate::typography::ui_rems(COMPACT_JUMP_HINT_WIDTH))
                                    })
                                    .whitespace_nowrap()
                                    .text_right()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .font_features(crate::typography::tabular())
                                    .text_color(subline)
                                    .child(label),
                            ),
                            None => el
                                .when(corner_hovered, |el| {
                                    el.child(
                                        div()
                                            .id(SharedString::from(format!("{row_id}-corner")))
                                            .debug_selector({
                                                let id = id.clone();
                                                move || format!("chat-archive-{id}")
                                            })
                                            .size(px(SIDEBAR_LEADING_SLOT))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            // slopty: a button a screen reader names while it shows.
                                            .role(gpui::Role::Button)
                                            .aria_label(if archived { "Unarchive" } else { "Archive" })
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.dismiss_session_card(cx);
                                                    cx.stop_propagation();
                                                }),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.chat_hover_resync = true;
                                                this.set_chat_archived(archive_id.clone(), !archived, cx);
                                            }))
                                            .tooltip(archive_tooltip(archived, selected))
                                            .tooltip_placement(gpui::TooltipPlacement::Above)
                                            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                                            .child(
                                                icon(if archived {
                                                    icons::ARCHIVE_UP_MINIMALISTIC
                                                } else {
                                                    icons::ARCHIVE_MINIMALISTIC
                                                })
                                                .size(px(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE))
                                                .flex_none()
                                                .text_color(theme.text_muted),
                                            ),
                                    )
                                })
                                .children(mark_slot),
                        }
                    }),
            )
            // Line 3 is structural, not reserved whitespace: compact states
            // omit it completely when both Branch and Pull request are hidden.
            .when(!compact && shows_metadata, |row| {
                row.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .when_some(branch, |el, branch| {
                            el.child(
                                icon(icons::GIT_BRANCH)
                                    .size(px(11.0))
                                    .flex_none()
                                    .text_color(subline),
                            )
                            .child(sidebar_faded_label(
                                format!("chat-branch-{content_id}").into(),
                                false,
                                div()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .line_height(px(14.0))
                                    .text_color(subline)
                                    .child(popover::search_highlight(branch, search_query, theme)),
                            ))
                        })
                        // Stable invisible spring keeps the optional PR badge
                        // pinned right without changing no-PR paint.
                        .child(div().flex_1().min_w_0())
                        .when_some(change_request, |el, summary| {
                            el.child(crate::change_requests::pull_request_badge_with_query(
                                format!("{row_id}-pr").into(),
                                summary,
                                pr_surface,
                                search_query,
                                pr_device.clone(),
                                theme,
                            ))
                        }),
                )
            })
            .into_any_element()
    }

    /// Chat-mode sidebar (spaces overhaul): window-control strip, the Spaces
    /// section (folder + device rows, add-space), the global Active sessions
    /// list, the notice strip, and the UserMenu (§1.6).
    /// Global connection health, with selected-chat queue and storage status.
    /// Persistence failures take precedence even when the network is offline.
    /// No surface,
    /// no border (v0.2.12 feedback): a bare spinner + faint caption while
    /// reconnecting; an amber dot only when the OS says offline. The
    /// transport error belongs in logs, not the sidebar.
    fn render_connection_pill(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        use slopty_view_proto::ConnectivityState as S;
        let conn = self.state.read(cx).connectivity.clone();
        let selected = self.state.read(cx).selected_chat.as_deref();
        let chat = conn.chats.iter()
            .find(|c| Some(c.chat_id.as_str()) == selected);
        let chat_state = chat.map(|c| c.sync_state);
        let (label, glyph): (SharedString, AnyElement) = match conn.state {
            _ if chat_state == Some(slopty_view_proto::ChatSyncState::StorageError) => (
                // slopty: work may be lost: the failure's red, not the waiting amber.
                "Couldn\u{2019}t save changes".into(),
                div().size(px(5.0)).rounded_full().bg(theme.danger).into_any_element(),
            ),
            S::Disabled => return None,
            S::Connected => {
                let caption = chat_sync_pill_caption(chat?)?;
                (
                    caption.into(),
                    loaders::mini_mono_spinner(
                        "chat-sync-spinner", 2.0, theme.text_muted,
                        self.sidebar_pane.entity_id(), cx,
                    ).into_any_element(),
                )
            }
            S::Offline => (
                "Offline — sends are saved".into(),
                div()
                    .size(px(5.0))
                    .rounded_full()
                    .bg(theme.warning)
                    .into_any_element(),
            ),
            S::Reconnecting => (
                "Reconnecting…".into(),
                loaders::mini_mono_spinner(
                    "connection-spinner",
                    2.0,
                    theme.text_muted,
                    self.sidebar_pane.entity_id(),
                    cx,
                )
                .into_any_element(),
            ),
        };
        Some(
            crate::motion::fade_in(
                "connection-pill",
                div()
                    .id("connection-pill")
                    .mx(px(Theme::SPACE_SM + 4.0))
                    .mb(px(Theme::SPACE_SM))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child(label),
                    ),
            )
            .into_any_element(),
        )
    }

    pub(super) fn render_chat_sidebar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        // slopty: the rows note when their times next read differently, and the clock ticks
        // then (`spaces::Shell::tick_sidebar_clock`).
        self.reconcile_session_hover_card(cx);
        self.sidebar_clock_next.set(None);
        let sidebar = self.render_chat_sidebar_body(theme, cx);
        self.tick_sidebar_clock(cx);
        sidebar
    }

    fn render_chat_sidebar_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        // Keyed rows: (stable key, estimated height, element) — the key + height
        // list drives the §1.6 resort FLIP diff below (attention-bucket
        // promotions glide; cleared rows just go).
        let keyed = self.render_active_rows(theme, cx).rows;

        // Resort glide (§1.6 View Transitions parity): when the ORDER of a live
        // list changes (new activity resort, grouping flip), surviving rows
        // glide from their old y to the new one — layout is already at the new
        // position; the offset is a paint-only relative inset animated to 0
        // over 260ms cubic-bezier(0.22,1,0.36,1). New rows fade in; removals
        // just go (matching the original). First fill and chat switches (which
        // don't reorder) never animate.
        let order: Vec<(String, f32)> =
            keyed.iter().map(|(key, height, _)| (key.clone(), *height)).collect();
        if self.sidebar_prev_order != order {
            let key_order_changed = sidebar_key_order_changed(&self.sidebar_prev_order, &order);
            if !self.sidebar_prev_order.is_empty() {
                // A disclosure already animates its own body height. Applying
                // FLIP offsets when only keyed heights change double-counts
                // that movement, leaving gaps and momentary overlaps between
                // the first group, following groups, and Archived.
                let offsets = if key_order_changed {
                    resort_offsets(&self.sidebar_prev_order, &order, SIDEBAR_LIST_GAP)
                } else {
                    std::collections::HashMap::new()
                };
                let prev_keys: std::collections::HashSet<&str> = self
                    .sidebar_prev_order
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect();
                let new_keys: std::collections::HashSet<String> = order
                    .iter()
                    .filter(|(k, _)| !prev_keys.contains(k.as_str()))
                    .map(|(k, _)| k.clone())
                    .collect();
                if key_order_changed && (!offsets.is_empty() || !new_keys.is_empty()) {
                    self.resort_epoch += 1;
                    self.sidebar_resort = offsets;
                    self.sidebar_new_keys = new_keys;
                }
            }
            self.sidebar_prev_order = order;
        }
        let epoch = self.resort_epoch;
        let list_items: Vec<AnyElement> = keyed
            .into_iter()
            .map(|(key, _, element)| {
                if let Some(dy) = self.sidebar_resort.get(&key).copied() {
                    let id = SharedString::from(format!("resort-{epoch}-{key}"));
                    div()
                        .child(element)
                        .with_animation(id, RESORT.animation(), move |el, t| {
                            el.relative().top(px(dy * (1.0 - t)))
                        })
                        .into_any_element()
                } else if self.sidebar_new_keys.contains(&key) {
                    let id = SharedString::from(format!("row-in-{epoch}-{key}"));
                    motion::fade_quick(id, div().child(element)).into_any_element()
                } else {
                    element
                }
            })
            .collect();

        // t3code's archived accordion, below the active list.
        let archived_section = self.render_archived_section(theme, cx);
        let history_section = self.render_sidebar_history(theme, cx); // slopty: past_sessions

        // The space filter lives ABOVE the scroll region (fixed) so its
        // dropdown can float without being clipped by the list's overflow.
        let filter_row = self.render_spaces_filter(theme, cx);
        let active_list = if !list_items.is_empty() {
            div()
                .id("sidebar-active-sessions")
                .flex()
                .flex_col()
                .gap(px(SIDEBAR_LIST_GAP))
                .pb(px(Theme::SPACE_SM))
                .children(list_items)
                .into_any_element()
        } else {
            // slopty: the empty list names the key that fills it (M16); a phone's Sessions page
            // has no key to offer and says its own noun.
            let compact = self.compact();
            // slopty: before any project, the list says what fills it first, and offers no key
            // that cannot make a chat yet (the canvas offers Add a project).
            let no_projects = self.awaits_first_project(cx);
            let combo = if compact || no_projects { "" } else { self.settings.keymap.get(ShortcutId::NewSession) };
            let empty: &'static str = if no_projects { "No projects yet" } else { "No sessions yet" };
            div()
                .px(px(Theme::SPACE_SM))
                .pb(px(Theme::SPACE_SM))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_faint)
                .child(
                    // slopty: a screen reader (and the goldens' text) reads the empty line too.
                    div()
                        .id("sidebar-empty")
                        .debug_selector(move || format!("sidebar-empty: {empty}"))
                        .role(gpui::Role::Label)
                        .aria_label(empty)
                        .child(SharedString::from(empty)),
                )
                .when(!combo.is_empty(), |el| {
                    el.child(
                        div()
                            .debug_selector(|| "sidebar-empty-key".to_owned())
                            .child(popover::kbd_hint(theme, &crate::keys::combo_text(combo))),
                    )
                })
                .into_any_element()
        };

        // The (filtered) Sessions list scrolls inside an EdgeFade scope —
        // a true per-glyph gradient at active overflow edges. Glass-safe
        // (no painted overlay can fade content over see-through blur) and
        // equivalent on opaque themes: alpha→0 reveals the surface tone
        // underneath, same as the gradient overlays it replaced. Overflow
        // is read at PAINT time via the scroll handle — render-time gating
        // rode the previous frame's offset, so the last frame of a content
        // shrink (row archived while scrolled) left a phantom fade stuck
        // over an unscrollable list (user report).
        let sidebar_lists = crate::edge_fade::edge_faded(
            SIDEBAR_GLASS_FADE_BAND,
            true,
            true,
            div().relative().flex_1().min_h_0().child(crate::overscroll::overscroll(
                "sidebar-overscroll",
                crate::overscroll::OverscrollSource::Handle(self.sidebar_scroll.clone()),
                div()
                    .id("sidebar-lists")
                    .relative()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_col()
                    // No "Sessions" header (user request) — the list
                    // is the whole column; a little air stands in.
                    .pt(px(SIDEBAR_LIST_PAD_TOP))
                    .child(active_list)
                    .children(history_section) // slopty
                    .children(archived_section),
            )),
        )
        // slopty: each fade as tall as what lies past its edge, up to its band.
        .fade_scroll_y(&self.sidebar_scroll);

        // slopty: no self-updater and no Star on GitHub banner.

        div()
            .w(px(self.sidebar_content_width()))
            .h_full()
            .flex()
            .flex_col()
            // (No titlebar strip: the unified window titlebar spans the whole
            // window above this column.)
            .child(filter_row)
            .child(sidebar_lists)
            // Global connection pill (durable-by-design UI truth): appears
            // whenever the edge posture is degraded; hidden while healthy —
            // appearing IS the signal.
            .when_some(self.render_connection_pill(theme, cx), |el, pill| {
                el.child(pill)
            })
            // slopty: no self-updater and no Star on GitHub banner.
            // slopty: a phone has no sidebar foot: Settings is a tab, the server its page's.
            .when(!self.compact(), |el| {
                el.child(
                    div()
                        .p(px(Theme::SPACE_SM))
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(self.render_sidebar_footer_row(theme, cx)),
                )
            })
            .into_any_element()
    }

    // slopty: no Star on GitHub banner.
    // slopty: no self-updater.
    /// The sidebar's bottom row: account menu on the left, settings toggle on
    /// the right. Shared by the chat sidebar and the settings page so both
    /// sit in one position. Local runtimes advertise their storage boundary
    /// and offer sync; synced runtimes offer sign-out.
    /// Account, settings, and the Pull requests entry. Chat and Settings both
    /// render this exact row so the footer holds still across the swap.
    pub(super) fn render_sidebar_footer_row(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("sidebar-footer-row")
            .debug_selector(|| "sidebar-footer-row".into())
            .w_full()
            .flex()
            .items_center()
            .gap(px(SIDEBAR_FOOTER_ACTION_GAP))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.render_sidebar_footer(theme, cx)),
            )
            .into_any_element()
    }

    // slopty: no pull-request pages.

    pub(super) fn render_sidebar_footer(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (user_line, menu_identity, online) = {
            let state = self.state.read(cx);
            sidebar_server_identity(state.connectivity.server.as_ref()) // slopty: the server, not an account.
        };
        self.render_user_menu(user_line, menu_identity, online, theme, cx)
    }

    // slopty: the server menu's rows: Devices, Connect a phone (while the server offers its
    // code), and Change server (Add device on "A server").
    fn server_menu_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let invite = self
            .state
            .read(cx)
            .connectivity
            .server
            .as_ref()
            .is_some_and(|server| server.invite.is_some());
        let row = |id: &'static str, glyph: &'static str, label: &'static str| {
            popover::menu_row(theme, false, format!("user-menu-{id}"))
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::MenuItem)
                .aria_label(label)
                .child(icon(glyph).size(px(14.0)).flex_none().text_color(theme.text_muted))
                .child(div().flex_1().min_w_0().truncate().child(label))
        };
        let mut rows = vec![
            row("user-menu-devices", icons::LAPTOP, "Devices")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_user_menu(cx);
                    this.open_settings(SettingsSection::Devices, cx);
                }))
                .into_any_element(),
        ];
        if invite {
            rows.push(
                row("user-menu-connect-phone", icons::SMARTPHONE, crate::settings::devices::INVITE)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_user_menu(cx);
                        this.open_connect_phone(cx);
                    }))
                    .into_any_element(),
            );
        }
        rows.push(
            row("user-menu-change-server", icons::REMOTE_SERVER, "Change server")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_user_menu(cx);
                    this.open_add_device_on(crate::settings::add_device::Choice::Server, cx);
                }))
                .into_any_element(),
        );
        rows
    }

    // slopty: the server menu's "Connect a phone": Settings → Devices with the server's code up.
    fn open_connect_phone(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Devices, cx);
        let state = self.state.clone();
        let page = self.devices_page.get_or_insert_with(|| cx.new(|cx| DevicesPage::new(state, cx)));
        page.update(cx, |page, cx| page.open_invite(cx));
    }

    fn render_user_menu(
        &mut self,
        user_line: SharedString,
        menu_identity: SharedString,
        online: Option<bool>, // slopty: the server's link, as the avatar's dot.
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // slopty: no pull-request pages; a pull request opens in the web browser.
        let theme = &theme.for_popup();
        let open = self.user_menu.is_open();
        // slopty: no accounts, sync or sign-in.
        // The profile pill hugs avatar + name (shrinking so long names fade
        // out); the gap between it and the settings button is not interactive.
        let initial: SharedString = user_line
            .trim()
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into())
            .into();
        let mut trigger = div()
            .id("user-menu")
            .debug_selector(|| "user-menu".into())
            .role(gpui::Role::Button)
            .aria_label(format!("Server: {user_line}, {menu_identity}")) // slopty: the server, not an account.
            .aria_expanded(open)
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, theme)) // slopty: paint only (M8)
            .relative()
            // slopty: a finger's row on a touch screen (#23).
            .h(px(crate::surface_chrome::touch_row_height(SIDEBAR_FOOTER_BUTTON_SIZE)))
            .min_w_0()
            .flex_shrink_1()
            .w_full()
            .rounded(px(8.0))
            .px(px(Theme::SPACE_SM))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .text_size(crate::typography::ui_rems(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(if open {
                theme.text
            } else {
                motion::hover_blend("user-menu-trigger", theme.text.opacity(0.8), theme.text)
            })
            .cursor_pointer()
            // user-menu.tsx trigger: hover `bg-white/[0.04]`, open state
            // (`data-[state=open]`) the slightly stronger `bg-white/[0.06]`;
            // the hover wash fades over `transition-colors`.
            .bg(if open {
                theme.glass_hover()
            } else {
                motion::hover_blend(
                    "user-menu-trigger",
                    theme.glass_hover().opacity(0.0),
                    theme.glass_hover().opacity(0.8),
                )
            })
            .hover_fade("user-menu-trigger")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.user_menu.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                // A press that found the menu open closes it (the card's
                // mouse-down-out already began the close) — never reopen.
                if this.user_menu.take_press_was_open() {
                    this.close_user_menu(cx);
                } else {
                    this.user_menu.open(());
                }
                cx.notify();
            }))
            .child(
                // Avatar: white circle, initial in near-black (slopty user-menu.tsx).
                div()
                    .size(px(SIDEBAR_FOOTER_AVATAR_SIZE))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.text)
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(10.0))
                    .line_height(px(SIDEBAR_FOOTER_AVATAR_SIZE))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.bg)
                    .child(div().w_full().text_center().child(initial))
                    // slopty: the server's link: green while it is up, muted while it is down.
                    .relative()
                    .children(online.map(|online| {
                        div()
                            .absolute()
                            .right(px(-2.0))
                            .bottom(px(-2.0))
                            .size(px(7.0))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.bg)
                            .bg(if online { theme.success } else { theme.text_muted })
                    })),
            )
            .child(sidebar_faded_label(
                "user-menu-label".into(),
                false,
                div().line_height(px(17.0)).child(user_line),
            ));
        if self.user_menu.get().is_some() {
            let closing = self.user_menu.closing_at(crate::clock::now(cx));

            let menu = popover::popover_card(theme)
                .w(px(self.sidebar_content_width()
                    - 2.0 * Theme::SPACE_SM
                    - SIDEBAR_FOOTER_ACTION_SIZE
                    - SIDEBAR_FOOTER_ACTION_GAP))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_user_menu(cx);
                }))
                .flex()
                .flex_col()
                .gap(px(2.0))
                // slopty: the server's link as the heading, then what the person does about
                // servers and machines (zeron's account rows' anatomy).
                .child(popover::menu_heading(theme, &menu_identity))
                .children(self.server_menu_rows(theme, cx))
                // slopty: no accounts, sync or sign-in.
                // slopty: no self-updater.
                .into_any_element();
            // Opens upward, left-aligned with the pill: the card is as wide as
            // the footer row, so it covers the row instead of the pane beside it.
            trigger = trigger.child(popover::anchored_menu_above(
                "user-menu-popover",
                menu,
                closing,
            ));
        }
        // Settings is a toggle: while its page is up the button keeps the
        // hover wash + full-strength glyph, and a click returns to chat.
        let settings_open = matches!(self.route, Route::Settings(_));
        // slopty: its key is the keymap's, as every tooltip's is.
        let settings_label = if settings_open { "Close settings" } else { "Settings" };
        // slopty: voice removed.
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(4.0))
            .child(trigger)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.0))
                    // slopty: voice removed.
                    .child({
                        // slopty: on a touch screen it takes presses over a finger's 44 pt
                        // around the button it draws (#23).
                        let reach = crate::surface_chrome::touch_reach(SIDEBAR_FOOTER_BUTTON_SIZE);
                        let drawn = div()
                            .size(px(SIDEBAR_FOOTER_BUTTON_SIZE))
                            .rounded(px(8.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(if settings_open {
                                theme.glass_hover()
                            } else {
                                motion::hover_blend(
                                    "settings-trigger",
                                    theme.glass_hover().opacity(0.0),
                                    theme.glass_hover(),
                                )
                            })
                            .child(icon(icons::SETTINGS).size(px(15.0)).text_color(
                                if settings_open {
                                    theme.text
                                } else {
                                    motion::hover_blend(
                                        "settings-trigger",
                                        theme.text_muted,
                                        theme.text,
                                    )
                                },
                            ));
                        div()
                            .id("settings-trigger")
                            .debug_selector(|| "settings-trigger".into())
                            .role(gpui::Role::Button)
                            .action_hint(settings_label, Keys::Action(Box::new(OpenSettings)), cx)
                            .aria_toggled(if settings_open {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .tab_index(0)
                            .size(px(SIDEBAR_FOOTER_BUTTON_SIZE + 2.0 * reach))
                            .m(px(-reach))
                            .flex_none()
                            .rounded(px(8.0 + reach))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover_fade("settings-trigger")
                            .map(|el| crate::surface_chrome::focus_ring_within(el, &theme, reach))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_settings(cx)))
                            .child(drawn)
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::accesskit::Role;
    use gpui::{TestAppContext, VisualTestContext};

    use super::super::retention_tests::{DESKTOP, draw, find, find_starting, launch, session_row, space, tree};

    #[test]
    fn sidebar_sync_status_waits_for_grace_or_queued_changes() {
        use slopty_view_proto::{ChatConnectivity, ChatSyncState as S};

        let mut chat = ChatConnectivity {
            chat_id: "remote".into(),
            sync_state: S::Local,
            connected: false,
            delivery_live: false,
            pending_pushes: 0,
        };
        assert_eq!(chat_sync_pill_caption(&chat), None);

        for state in [S::Waiting, S::Connecting, S::Offline] {
            chat.sync_state = state;
            chat.connected = true;
            assert_eq!(chat_sync_pill_caption(&chat), None, "transient {state:?}");
        }

        chat.sync_state = S::Waiting;
        chat.connected = false;
        assert_eq!(chat_sync_pill_caption(&chat), Some("Sync queued — changes are saved"));
        chat.sync_state = S::Connecting;
        assert_eq!(chat_sync_pill_caption(&chat), Some("Syncing…"));
        chat.sync_state = S::Offline;
        assert_eq!(chat_sync_pill_caption(&chat), Some("Offline — changes are saved"));

        // Real pending pushes remain visible even with a live room.
        chat.connected = true;
        chat.pending_pushes = 1;
        chat.sync_state = S::Waiting;
        assert_eq!(chat_sync_pill_caption(&chat), Some("Sync queued — changes are saved"));
        chat.sync_state = S::Connecting;
        assert_eq!(chat_sync_pill_caption(&chat), Some("Syncing…"));
    }

    // slopty: the foot names the server this client follows.
    #[test]
    fn sidebar_footer_names_the_server_and_its_link() {
        let server = |online| slopty_view_proto::ServerPresence { name: "studio".into(), online, actions: Vec::new(), invite: None };
        let up = server(true);
        let down = server(false);
        assert_eq!(
            sidebar_server_identity(Some(&up)),
            ("studio".into(), "Connected".into(), Some(true))
        );
        assert_eq!(
            sidebar_server_identity(Some(&down)),
            ("studio".into(), "Offline".into(), Some(false))
        );
        assert_eq!(sidebar_server_identity(None).2, None);
    }

    // ---- sidebar resort FLIP diff (§1.6) ----

    fn keys(list: &[(&str, f32)]) -> Vec<(String, f32)> {
        list.iter().map(|(k, h)| (k.to_string(), *h)).collect()
    }

    #[test]
    fn sidebar_chat_height_tracks_visible_metadata() {
        assert_eq!(chat_row_height(false, false), 45.0);
        assert_eq!(chat_row_height(true, false), 61.0);
        assert_eq!(chat_row_height(false, true), 63.0);
        assert_eq!(chat_row_height(true, true), 63.0);
    }

    #[test]
    fn sidebar_harness_geometry_reflects_row_hierarchy() {
        assert_eq!(SIDEBAR_ACTIVE_HARNESS_TITLE_GAP, Theme::SPACE_SM);
    }

    #[test]
    fn sidebar_height_change_is_not_a_reorder() {
        let open = keys(&[("first-group", 105.0), ("second-group", 240.0)]);
        let collapsed = keys(&[("first-group", 40.0), ("second-group", 240.0)]);
        assert!(!sidebar_key_order_changed(&open, &collapsed));

        let reordered = keys(&[("second-group", 240.0), ("first-group", 40.0)]);
        assert!(sidebar_key_order_changed(&collapsed, &reordered));
    }

    #[test]
    fn resort_offsets_empty_when_order_unchanged() {
        let order = keys(&[("a", 29.0), ("b", 29.0), ("c", 45.0)]);
        assert!(resort_offsets(&order, &order, 2.0).is_empty());
    }

    #[test]
    fn resort_offsets_activity_moves_row_to_top() {
        // c (bottom, y=62) jumps to top: c glides down-from-above? No — c's
        // old y is 62, new y is 0 → starts +62 below… offset = old - new = +62,
        // painted at +62 decaying to 0 (a glide UP into place). a and b shift
        // down by c's height + gap (31).
        let old = keys(&[("a", 29.0), ("b", 29.0), ("c", 29.0)]);
        let new = keys(&[("c", 29.0), ("a", 29.0), ("b", 29.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        assert_eq!(offsets.get("c"), Some(&62.0));
        assert_eq!(offsets.get("a"), Some(&-31.0));
        assert_eq!(offsets.get("b"), Some(&-31.0));
    }

    #[test]
    fn resort_offsets_respect_heights_and_gap() {
        // Tall row (45px) swaps with a short one (29px).
        let old = keys(&[("tall", 45.0), ("short", 29.0)]);
        let new = keys(&[("short", 29.0), ("tall", 45.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        // short: old y 47 → new y 0; tall: old y 0 → new y 31.
        assert_eq!(offsets.get("short"), Some(&47.0));
        assert_eq!(offsets.get("tall"), Some(&-31.0));
    }

    #[test]
    fn resort_offsets_ignore_added_and_removed_keys() {
        let old = keys(&[("a", 29.0), ("gone", 29.0), ("b", 29.0)]);
        let new = keys(&[("new", 29.0), ("a", 29.0), ("b", 29.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        // "new" has no old position (fades in instead); "gone" just goes.
        assert!(!offsets.contains_key("new"));
        assert!(!offsets.contains_key("gone"));
        // a: old 0 → new 31 (pushed down by the insert); b: 62 → 62 (gone's
        // slot replaced by "new" of equal height — no move, no entry).
        assert_eq!(offsets.get("a"), Some(&-31.0));
        assert_eq!(offsets.get("b"), None);
    }

    #[test]
    fn resort_glide_spec_matches_original() {
        // §1.6: 260ms cubic-bezier(0.22, 1, 0.36, 1).
        assert_eq!(RESORT.duration_ms, 260);
        assert_eq!(RESORT.curve, motion::EASE_RESORT);
    }

    // ---- the whole sidebar drawn in a shell window ----

    /// A sidebar group's header is a button a screen reader names by its project and its count and
    /// hears open or shut; a press folds it, and shut it says the state its loudest row wears.
    #[gpui::test]
    fn a_sidebar_header_is_a_named_button_that_says_it_is_open(cx: &mut TestAppContext) {
        let (_shell, cx) = launch(cx, DESKTOP);
        let nodes = tree(cx);
        let app = find(&nodes, Role::Button, "app, 2").expect("the project's header");
        assert_eq!(app.expanded, Some(true), "open: {app:?}");
        cx.simulate_click(app.bounds.center(), gpui::Modifiers::default());
        let nodes = tree(cx);
        let app = find(&nodes, Role::Button, "app, 2, Working").expect("the header, saying what it wears");
        assert_eq!(app.expanded, Some(false), "a press folds it: {app:?}");
    }

    /// slopty: a group heading reads 12 medium in the muted ink, its count (Amp's) and an open
    /// group's chevron (Claude desktop's caret) drawn only under the pointer.
    #[gpui::test]
    fn a_sidebar_headers_count_shows_under_the_pointer(cx: &mut TestAppContext) {
        let (_shell, cx) = launch(cx, DESKTOP);
        let nodes = tree(cx);
        let working = find(&nodes, Role::Button, "app, 2").expect("the project's header");
        let key = "folder:/work/app";
        let count = |cx: &mut VisualTestContext| {
            draw(cx);
            cx.debug_bounds("sidebar-header-count-folder:/work/app").expect("the count holds its place")
        };
        let resting = count(cx);
        let header_key = format!("sidebar-header-{key}");
        let rest = cx.update(|_, cx| {
            let theme = Theme::of(cx);
            crate::motion::hover_blend(&header_key, theme.text_muted.opacity(0.0), theme.text_muted)
        });
        assert_eq!(rest.a, 0.0, "unseen at rest");
        let chevron = |cx: &mut VisualTestContext| cx.update(|_, _| spaces::sidebar_hover_reveal(&header_key));
        assert_eq!(chevron(cx), 0.0, "an open group's chevron rests unseen with it");
        // Settled at once, so the frame read is the faded-in one.
        cx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
        cx.simulate_mouse_move(working.bounds.center(), None, gpui::Modifiers::default());
        let hovered = count(cx);
        assert_eq!(hovered, resting, "the count fades in where it stood: nothing moves");
        let shown = cx.update(|_, cx| {
            let theme = Theme::of(cx);
            crate::motion::hover_blend(&header_key, theme.text_muted.opacity(0.0), theme.text_muted)
        });
        assert!(shown.a > 0.9, "shown under the pointer: {shown:?}");
        assert!(chevron(cx) > 0.9, "and its chevron");
    }

    /// slopty: a session untouched for 72 hours folds into Inactive, the last group and shut at
    /// first (Amp's), its row under its header once opened; with none in it there is no Inactive.
    #[gpui::test]
    fn a_chat_untouched_for_three_days_folds_into_inactive(cx: &mut TestAppContext) {
        let (shell, cx) = launch(cx, DESKTOP);
        assert!(find_starting(&tree(cx), Role::Button, "Inactive").is_none(), "none rests: no Inactive");
        // The finished chat goes untouched for three days: it leaves its project for Inactive.
        shell.update(cx, |shell, cx| {
            let now = Utc::now();
            shell.state.update(cx, |state, _| {
                let chat = state.chats.iter_mut().find(|chat| chat.id == "b").unwrap();
                chat.created_at = now - chrono::TimeDelta::hours(80);
                chat.last_message_at = Some(now - chrono::TimeDelta::hours(73));
            });
            shell.sidebar_clock = now;
            cx.notify();
        });
        let nodes = tree(cx);
        let inactive = find(&nodes, Role::Button, "Inactive, 1").expect("it holds the chat");
        let app = find(&nodes, Role::Button, "app, 1").expect("its project holds the other");
        assert!(inactive.bounds.top() > app.bounds.top(), "last");
        assert_eq!(inactive.expanded, Some(false), "shut at first");
        assert!(cx.debug_bounds("chat-b").is_none(), "its row waits behind it");
        cx.simulate_click(inactive.bounds.center(), gpui::Modifiers::default());
        draw(cx);
        let row = cx.debug_bounds("chat-b").expect("its row, in the open group");
        assert!(row.top() > inactive.bounds.top(), "under its header");
    }

    /// A row at rest wears no mark: only a state worth a glance does (one that waits on the person),
    /// while its slot keeps the titles' ends on one edge.
    #[gpui::test]
    fn a_row_at_rest_draws_no_dot(cx: &mut TestAppContext) {
        let (shell, cx) = launch(cx, DESKTOP);
        shell.update(cx, |shell, cx| {
            let now = Utc::now();
            shell.state.update(cx, |state, _| {
                state.apply_sessions(vec![
                    session_row("a", slopty_view_proto::SessionStatus::AwaitingInput, now),
                    session_row("b", slopty_view_proto::SessionStatus::Idle, now),
                ]);
            });
        });
        draw(cx);
        assert!(cx.debug_bounds("chat-mark-a").is_some(), "a row that needs you has its mark");
        assert!(cx.debug_bounds("chat-mark-b").is_none(), "a row at rest has none");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.apply_sessions(vec![
                    session_row("a", slopty_view_proto::SessionStatus::Working, Utc::now()),
                    session_row("b", slopty_view_proto::SessionStatus::Idle, Utc::now()),
                ]);
            });
        });
        draw(cx);
        assert!(cx.debug_bounds("chat-mark-a").is_some(), "the same mark, now working");
        let (a, b) = (cx.debug_bounds("chat-status-a").unwrap(), cx.debug_bounds("chat-status-b").unwrap());
        assert_eq!(a.left(), b.left(), "the slot stays");
        assert_eq!(a.size, b.size, "the slot stays");
    }

    /// The sidebar's list is one keyboard stop, at the open chat's row; Up and Down walk the rows
    /// in the order they draw, and Enter opens the one with the keyboard.
    #[gpui::test]
    fn the_sidebar_list_is_one_stop_walked_with_up_and_down(cx: &mut TestAppContext) {
        let (shell, cx) = launch(cx, DESKTOP);
        shell.update(cx, |shell, cx| shell.open_chat("a".into(), cx));
        draw(cx);
        let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert_eq!(order, ["a", "b"], "the rows as they draw");
        shell.read_with(cx, |shell, _| {
            assert!(shell.sidebar_keys.stops_at("a"), "Tab reaches the open chat's row");
            assert!(!shell.sidebar_keys.stops_at("b"), "and only it");
        });
        let (a, b) = shell.update(cx, |shell, cx| (shell.sidebar_keys.focus("a", cx), shell.sidebar_keys.focus("b", cx)));
        cx.update(|window, cx| window.focus(&a, cx));
        draw(cx);
        cx.simulate_keystrokes("down");
        draw(cx);
        cx.update(|window, _| assert!(b.is_focused(window), "Down moves to the row below"));
        cx.simulate_keystrokes("up");
        draw(cx);
        cx.update(|window, _| assert!(a.is_focused(window), "Up moves back"));
        cx.simulate_keystrokes("down");
        draw(cx);
        // A key press clicks as it comes up.
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("enter").unwrap() });
        draw(cx);
        assert_eq!(
            shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone()).as_deref(),
            Some("b"),
            "Enter opens it"
        );
    }

    /// The project filter says it opens a menu, what it filters to and whether the menu is open.
    #[gpui::test]
    fn the_project_filter_is_a_named_menu_button(cx: &mut TestAppContext) {
        let (shell, cx) = launch(cx, DESKTOP);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.spaces = vec![space("app")]);
        });
        let nodes = tree(cx);
        let filter = find(&nodes, Role::Button, "Filter projects: All projects").expect("the filter");
        assert_eq!(filter.expanded, Some(false), "shut: {filter:?}");
        cx.simulate_click(filter.bounds.center(), gpui::Modifiers::default());
        let nodes = tree(cx);
        let filter = find(&nodes, Role::Button, "Filter projects: All projects").expect("the filter");
        assert_eq!(filter.expanded, Some(true), "open: {filter:?}");
    }

    /// slopty: the machine is a filter, not a level: narrowed to one machine, the list holds its
    /// sessions alone, the filter names the machine after the project, and choosing the machine
    /// again shows every machine's.
    #[gpui::test]
    fn the_machine_filter_narrows_the_list_to_one_machine(cx: &mut TestAppContext) {
        let (shell, cx) = launch(cx, DESKTOP);
        let device = |id: &str| -> slopty_view_proto::Device {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "platform": "linux", "lastSeenAt": Utc::now(),
            }))
            .unwrap()
        };
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices = vec![device("local"), device("studio")];
                state.chats.iter_mut().find(|chat| chat.id == "b").unwrap().device_id = "studio".into();
            });
            shell.toggle_machine_filter("studio".into(), cx);
        });
        let nodes = tree(cx);
        assert!(find(&nodes, Role::Button, "Filter projects: All projects \u{b7} studio").is_some(), "it names the machine");
        assert_eq!(shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx)), ["b"], "its sessions alone");
        assert!(cx.debug_bounds("chat-a").is_none(), "another machine's row is not drawn");
        shell.update(cx, |shell, cx| shell.toggle_machine_filter("studio".into(), cx));
        draw(cx);
        assert_eq!(shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx)), ["a", "b"], "every machine's again");
        assert!(shell.read_with(cx, |shell, _| shell.settings.machine_filter.is_none()));
    }

    fn init_settings_test(
        saved: settings::UiSettings,
        dir: &std::path::Path,
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            settings::init(saved, dir, cx);
            // slopty: no History surface (ruled out).
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
                // slopty: Slopty's server is the only backend.
                default_harness: slopty_view_proto::HarnessId::MOCK,
            },
            cx,
        )
    }

    /// What the window shows against the same state drawn from scratch, as the app's own check
    /// judges a frame: the primitives only the frame shown paints, and only the scratch paints.
    fn stale_sidebar(cx: &mut gpui::VisualTestContext) -> Option<String> {
        cx.update(|window, cx| {
            let mut shown = window.painted_primitives();
            shown.sort_unstable();
            window.refresh();
            window.draw(cx).clear(cx);
            let mut scratch = window.painted_primitives();
            scratch.sort_unstable();
            (shown != scratch).then(|| {
                // What differs beyond the depths the differences push down.
                let bare = |lines: &[String]| {
                    let mut bare: Vec<String> = lines
                        .iter()
                        .map(|line| line.split_once(' ').map_or(line.clone(), |(_, rest)| rest.to_owned()))
                        .collect();
                    bare.sort_unstable();
                    bare
                };
                let (shown, scratch) = (bare(&shown), bare(&scratch));
                let only = |a: &[String], b: &[String]| {
                    let mut b = b.to_vec();
                    a.iter()
                        .filter(|line| match b.iter().position(|other| other == *line) {
                            Some(at) => {
                                b.remove(at);
                                false
                            }
                            None => true,
                        })
                        .take(6)
                        .map(|line| line.chars().take(420).collect::<String>())
                        .collect::<Vec<_>>()
                };
                format!("shown only {:#?}\nscratch only {:#?}", only(&shown, &scratch), only(&scratch, &shown))
            })
        })
    }

    // slopty: the sidebar's list reads the sidebar's clock, not the wall's, so a sidebar built
    // again lists what the one shown lists; the clock ticks when a row next folds into Inactive.
    #[gpui::test]
    fn the_sidebars_list_reads_its_clock_and_ticks_when_a_row_folds(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        let (shell, cx) = cx.add_window_view(|_, cx| test_shell(dir.path(), cx));
        cx.simulate_resize(gpui::size(px(1320.0), px(880.0)));
        // A clock taken five minutes ago, a chat that folds two minutes after it: folded by the
        // wall's clock, not yet by the sidebar's.
        let clock = Utc::now() - chrono::Duration::minutes(5);
        let folds = clock + chrono::Duration::minutes(2);
        let moved = folds - chrono::Duration::hours(super::sidebar_status::INACTIVE_AFTER_HOURS);
        shell.update(cx, |shell, cx| {
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "a", "title": "Chat a", "deviceId": "local", "archived": false,
                        "createdAt": moved, "lastMessageAt": moved, "lastSeenAt": moved,
                    }))
                    .unwrap(),
                ];
            });
        });
        cx.run_until_parked();
        shell.update(cx, |shell, _| shell.sidebar_clock = clock);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("chat-a").is_some(), "in its project by the sidebar's clock");
        shell.read_with(cx, |shell, _| {
            let due = shell.sidebar_clock_tick.as_ref().map(|(due, _)| *due);
            assert_eq!(due, Some(folds), "ticks when it folds");
        });
    }

    // slopty: the sidebar under a resting pointer draws as a frame from scratch would: on a row
    // whose hover card comes up, and on the rows Inactive's "Show more" brings under it.
    #[gpui::test]
    fn the_sidebar_under_a_resting_pointer_is_drawn_as_from_scratch(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_settings_test(settings::UiSettings::default(), dir.path(), cx);
        cx.update(|cx| cx.set_reduce_motion(true));
        let (shell, cx) = cx.add_window_view(|_, cx| test_shell(dir.path(), cx));
        cx.simulate_resize(gpui::size(px(1320.0), px(1100.0)));
        shell.update(cx, |shell, cx| {
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                // Untouched for days, so they rest in Inactive, which pages them.
                state.chats = (0..12)
                    .map(|n| {
                        serde_json::from_value(serde_json::json!({
                            "id": format!("c{n}"), "title": format!("Say hello {n}"),
                            "deviceId": "local", "archived": false,
                            "createdAt": Utc::now() - chrono::Duration::hours(80) - chrono::Duration::minutes(n),
                        }))
                        .unwrap()
                    })
                    .collect();
            });
            shell.sidebar_collapsed_groups.remove(super::sidebar_status::INACTIVE);
            // The gate and splash are set behind the shell's back: it draws them on its next
            // frame only when told.
            cx.notify();
        });
        crate::motion::run_on(cx.executor());
        let settle = |cx: &mut gpui::VisualTestContext, wait: std::time::Duration| {
            cx.executor().advance_clock(wait);
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        let mut stale = Vec::new();
        settle(cx, std::time::Duration::ZERO);
        stale.extend(stale_sidebar(cx).map(|s| format!("at rest: {s}")));
        let row = cx.debug_bounds("chat-c0").expect("a sidebar row");
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        settle(cx, std::time::Duration::from_millis(50));
        stale.extend(stale_sidebar(cx).map(|s| format!("on the row: {s}")));
        settle(cx, crate::settings::widgets::TOOLTIP_DELAY * 3);
        stale.extend(stale_sidebar(cx).map(|s| format!("its card up: {s}")));
        let more = cx.debug_bounds("sidebar-group-more-inactive");
        if let Some(more) = more {
            cx.simulate_click(more.center(), gpui::Modifiers::default());
            settle(cx, std::time::Duration::from_millis(50));
            stale.extend(stale_sidebar(cx).map(|s| format!("after Show more: {s}")));
            settle(cx, crate::settings::widgets::TOOLTIP_DELAY * 3);
            stale.extend(stale_sidebar(cx).map(|s| format!("a row under it: {s}")));
        } else {
            stale.push("no Show more drawn".to_owned());
        }
        assert!(stale.is_empty(), "{}", stale.join("\n\n"));
    }
}
