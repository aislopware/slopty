// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The explorer's footer: a collapsible section docked under the file
//! tree — **Chats** (the side chats hanging off the active chat: forks and
//! asides), open or closed as the person last left it. Subagents list under
//! their agent in the sidebar instead. Rows borrow the left sidebar's compact
//! session row — 29px, status glyph, title, time — minus the harness, project and device icons, which
//! say nothing here (every row shares the parent's context). Clicking a row
//! opens it in the right pane's surface host; the Chats header carries "+"
//! and fork beside its caret; the section chrome animates with the same
//! collapse motion as the sidebar's disclosures.
//!
//! The footer has a fixed height budget the open section scrolls inside,
//! and like the sidebar's Archived shelf it shows ten rows before a
//! "Show N more" row pages by ten.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use chrono::{DateTime, Utc};
use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, Entity, EntityId, MouseButton, ScrollHandle,
    SharedString, div, prelude::*, px,
};
use slopty_view_proto::{Chat, ChatIndicator};

use crate::composer::ComposerInput;
use crate::icons::{self, icon};
use crate::state::AppState;
use crate::theme::Theme;
use crate::{loaders, motion};

use super::{FilesEvent, FilesSurface};

const SECTION_HEADER_HEIGHT: f32 = 28.0;
const SECTION_BODY_INSET: f32 = 4.0;
const ROW_HEIGHT: f32 = 29.0;
const ROW_GAP: f32 = 2.0;
/// Empty state: the copy (two 16px lines so it can wrap in a narrow
/// explorer) and, for Chats, a row of pill actions — left-aligned like the
/// rows it stands in for.
const EMPTY_COPY_HEIGHT: f32 = 36.0;
const EMPTY_ACTIONS_HEIGHT: f32 = 40.0;
const EMPTY_PAD: f32 = 10.0;
/// Fade band under a section list's edges (the sidebar's treatment, scaled
/// to the shorter lists).
const LIST_FADE_BAND: f32 = 16.0;
/// Hover group of a section header (reveals its actions).
const HEADER_GROUP: &str = "files-section-header";
/// Rows a section shows before "Show more" pages it, and the page size —
/// the sidebar's Archived shelf numbers.
pub(super) const INITIAL_ROWS: usize = 10;
pub(super) const PAGE_ROWS: usize = 10;
/// An open section never shrinks below this, so one or two rows still
/// leave the section room to breathe.
const MIN_BODY_HEIGHT: f32 = 120.0;
const FOOTER_PAD_TOP: f32 = 4.0;
const FOOTER_PAD_BOTTOM: f32 = 6.0;
/// The footer's height budget; shorter content shrinks the footer to fit.
const FOOTER_HEIGHT: f32 = 510.0;
const TWEEN_GRACE: std::time::Duration = std::time::Duration::from_millis(120);

/// Which footer section a motion or toggle addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Section {
    // slopty: no Subagents section.
    Chats,
}

impl Section {
    fn key(self) -> &'static str {
        match self {
            Section::Chats => "chats",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Section::Chats => "Chats",
        }
    }
}

/// One in-flight open/close of a section body (the sidebar's
/// `SidebarDisclosureMotion`, kept local so the explorer owns its own
/// epochs). A re-toggle mid-flight picks up from the painted height.
#[derive(Debug, Clone, Copy)]
struct DisclosureMotion {
    epoch: u64,
    from: f32,
    to: f32,
    started: std::time::Instant,
}

impl DisclosureMotion {
    fn current(self) -> f32 {
        let total = motion::COLLAPSE.total().as_secs_f32();
        let raw = if total > 0.0 {
            self.started.elapsed().as_secs_f32() / total
        } else {
            1.0
        };
        motion::lerp(self.from, self.to, motion::COLLAPSE.progress(raw))
    }

    fn animating(self) -> bool {
        self.started.elapsed() < motion::COLLAPSE.total() + TWEEN_GRACE
    }
}

/// Footer state on the explorer surface.
#[derive(Debug)]
pub(super) struct ExplorerSections {
    open: HashMap<Section, bool>,
    motion: HashMap<Section, DisclosureMotion>,
    /// Rows revealed per section ("Show more" pages this up).
    shown: HashMap<Section, usize>,
    /// One scroll handle per section list, so the edge fades can read
    /// overflow at paint time.
    scroll: HashMap<Section, ScrollHandle>,
    /// Hash of what the footer would draw, so the state observer only
    /// re-renders the explorer when a section's contents actually changed —
    /// not on every streamed transcript delta.
    fingerprint: u64,
    /// The side chat whose title the shell is editing in place, and its
    /// field — drawn over that row's title.
    chat_rename: Option<(String, Entity<ComposerInput>)>,
    /// A section opened to reveal a renamed row: its motion starts on the
    /// render that knows the body height.
    reveal: Option<Section>,
}

impl Default for ExplorerSections {
    fn default() -> Self {
        Self {
            // slopty: closed until the person opens it (the shell seeds what they left).
            open: [(Section::Chats, false)].into_iter().collect(),
            motion: HashMap::new(),
            shown: HashMap::new(),
            scroll: [(Section::Chats, ScrollHandle::new())]
                .into_iter()
                .collect(),
            fingerprint: 0,
            chat_rename: None,
            reveal: None,
        }
    }
}

impl ExplorerSections {
    pub(super) fn is_open(&self, section: Section) -> bool {
        self.open.get(&section).copied().unwrap_or(true)
    }

    fn shown(&self, section: Section) -> usize {
        self.shown
            .get(&section)
            .copied()
            .unwrap_or(INITIAL_ROWS)
            .max(INITIAL_ROWS)
    }

    fn toggle(&mut self, section: Section, resting: f32, target: f32) {
        let previous = self.motion.get(&section).copied();
        let from = previous
            .filter(|m| m.animating())
            .map(DisclosureMotion::current)
            .unwrap_or(resting);
        let epoch = previous.map_or(1, |m| m.epoch + 1);
        self.motion.insert(
            section,
            DisclosureMotion {
                epoch,
                from,
                to: target,
                started: std::time::Instant::now(),
            },
        );
        let open = self.is_open(section);
        self.open.insert(section, !open);
    }

    /// Start the opening motion of a section a reveal opened.
    fn begin_reveal(&mut self, section: Section, height: f32) {
        if self.reveal != Some(section) || !self.is_open(section) {
            return;
        }
        self.reveal = None;
        let epoch = self.motion.get(&section).map_or(1, |m| m.epoch + 1);
        self.motion.insert(
            section,
            DisclosureMotion {
                epoch,
                from: 0.0,
                to: height,
                started: std::time::Instant::now(),
            },
        );
    }

    fn scroll(&self, section: Section) -> ScrollHandle {
        self.scroll
            .get(&section)
            .cloned()
            .unwrap_or_else(ScrollHandle::new)
    }

    fn live_motion(&self, section: Section) -> Option<DisclosureMotion> {
        self.motion.get(&section).copied().filter(|m| m.animating())
    }
}

/// A side chat of the active chat, as the footer lists it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ChildChatRow {
    pub chat_id: String,
    pub device_id: String,
    pub title: SharedString,
    pub status: ChatIndicator,
    pub time_ago: SharedString,
    /// The chat's linked pull request, drawn as the sidebar's badge.
    pub change_request: Option<slopty_view_proto::ChangeRequestSummary>,
    activity: DateTime<Utc>,
}

/// The live (unarchived) children of `chat_id`, most recent activity first —
/// the same order the sidebar's Sessions list keeps.
pub(super) fn child_chat_rows(
    state: &AppState,
    chat_id: &str,
    now: DateTime<Utc>,
) -> Vec<ChildChatRow> {
    let mut rows: Vec<ChildChatRow> = state
        .chats
        .iter()
        // slopty: a subagent lists under its agent in the sidebar, not here.
        .filter(|chat| !chat.archived && chat.parent_chat_id.as_deref() == Some(chat_id))
        .filter(|chat| !chat.subagent)
        .map(|chat| {
            let activity = chat.last_message_at.unwrap_or(chat.created_at);
            ChildChatRow {
                chat_id: chat.id.clone(),
                device_id: chat.device_id.clone(),
                title: child_chat_title(chat).into(),
                status: state.display_status_for(chat, now),
                time_ago: slopty_view_proto::view::format_time_ago(activity, now).into(),
                change_request: state.change_request_for_chat(chat).cloned(),
                activity,
            }
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.activity));
    rows
}

/// A side chat titles itself on its first turn; until then the preview or a
/// placeholder stands in.
pub(super) fn child_chat_title(chat: &Chat) -> String {
    chat.title
        .clone()
        .or_else(|| chat.last_message_preview.clone())
        .unwrap_or_else(|| "New side chat".into())
}

/// What the footer would draw for `chat_id`, hashed. Cheap enough to run on
/// every state notification.
pub(super) fn fingerprint(state: &AppState, chat_id: &str, now: DateTime<Utc>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // slopty: no Subagents section.
    for row in child_chat_rows(state, chat_id, now) {
        row.chat_id.hash(&mut hasher);
        row.device_id.hash(&mut hasher);
        row.title.as_ref().hash(&mut hasher);
        (row.status as u8).hash(&mut hasher);
        row.time_ago.as_ref().hash(&mut hasher);
        row.change_request
            .as_ref()
            .map(|pr| (pr.number, pr.state as u8))
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// The height a section body wants for `count` rows with `shown` revealed:
/// inset, the visible rows, and a "Show more" row while more remain. Empty
/// sections want their empty-state copy (Chats adds its action row).
pub(super) fn content_height(section: Section, count: usize, shown: usize) -> f32 {
    content_height_unfloored(section, count, shown).max(MIN_BODY_HEIGHT)
}

fn content_height_unfloored(section: Section, count: usize, shown: usize) -> f32 {
    if count == 0 {
        return SECTION_BODY_INSET
            + EMPTY_PAD * 2.0
            + EMPTY_COPY_HEIGHT
            + match section {
                Section::Chats => EMPTY_ACTIONS_HEIGHT,
            };
    }
    let visible = count.min(shown);
    let more = if count > shown { 1 } else { 0 };
    rows_height(visible + more)
}

/// The height of a list of `slots` row-sized items: the inset, the rows and
/// the gaps between them.
fn rows_height(slots: usize) -> f32 {
    SECTION_BODY_INSET + slots as f32 * ROW_HEIGHT + slots.saturating_sub(1) as f32 * ROW_GAP
}

/// The footer's chrome outside the body: padding and the header.
fn chrome_height() -> f32 {
    FOOTER_PAD_TOP + FOOTER_PAD_BOTTOM + SECTION_HEADER_HEIGHT // slopty: one section
}

impl FilesSurface {
    /// Show (or clear) the shell's inline rename of one of the Chats rows.
    pub(crate) fn set_chat_rename(
        &mut self,
        rename: Option<(String, Entity<ComposerInput>)>,
        cx: &mut Context<Self>,
    ) {
        let key = |rename: &Option<(String, Entity<ComposerInput>)>| {
            rename
                .as_ref()
                .map(|(id, input)| (id.clone(), input.entity_id()))
        };
        if key(&self.sections.chat_rename) != key(&rename) {
            if let Some((chat_id, _)) = &rename {
                self.reveal_chat_row(chat_id, cx);
            }
            self.sections.chat_rename = rename;
            cx.notify();
        }
    }

    /// Open the Chats section, page it, and scroll its list so `chat_id`'s
    /// row shows — the inline rename's field must never sit on a hidden row.
    fn reveal_chat_row(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let rows = child_chat_rows(self.state.read(cx), &self.chat_id, Utc::now());
        let Some(index) = rows.iter().position(|row| row.chat_id == chat_id) else {
            return;
        };
        if !self.sections.is_open(Section::Chats) {
            self.sections.open.insert(Section::Chats, true);
            self.sections.reveal = Some(Section::Chats);
            cx.emit(FilesEvent::ChatsOpenChanged(true)); // slopty: remembered
        }
        // Page in the same steps "Show more" takes.
        if index >= self.sections.shown(Section::Chats) {
            self.sections.shown.insert(
                Section::Chats,
                INITIAL_ROWS + (index + 1 - INITIAL_ROWS).div_ceil(PAGE_ROWS) * PAGE_ROWS,
            );
        }
        self.sections.scroll(Section::Chats).scroll_to_item(index);
    }

    /// Re-render only when the footer's contents changed.
    pub(super) fn refresh_sections(&mut self, cx: &mut Context<Self>) {
        let fingerprint = fingerprint(self.state.read(cx), &self.chat_id, Utc::now());
        if fingerprint != self.sections.fingerprint {
            self.sections.fingerprint = fingerprint;
            cx.notify();
        }
    }

    pub(super) fn render_sections(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = Utc::now();
        // slopty: Chats alone; subagents list under their agent in the sidebar.
        let chats = child_chat_rows(self.state.read(cx), &self.chat_id, now);
        self.sections.fingerprint = fingerprint(self.state.read(cx), &self.chat_id, now);
        let wanted = content_height(
            Section::Chats,
            chats.len(),
            self.sections.shown(Section::Chats),
        );
        let open = self.sections.is_open(Section::Chats);
        let budget = (FOOTER_HEIGHT - chrome_height()).max(0.0);
        let height = if open { wanted.min(budget) } else { 0.0 };
        let chat_body = self.render_chat_rows(&chats, theme, cx);
        let chats_actions = self.render_chats_header_actions(theme, cx);
        div()
            .id("files-sections")
            .relative()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .px(px(6.0))
            .pt(px(FOOTER_PAD_TOP))
            .pb(px(FOOTER_PAD_BOTTOM))
            .child(self.render_section(
                Section::Chats,
                chats.len(),
                wanted,
                height,
                Some(chats_actions),
                None,
                chat_body,
                theme,
                cx,
            ))
            .into_any_element()
    }

    /// Open or close the Chats section as the person last left it, in any
    /// explorer: no motion, since it is not this explorer's own click.
    // slopty: the Chats section's state is remembered.
    pub(crate) fn set_chats_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sections.is_open(Section::Chats) != open {
            self.sections.open.insert(Section::Chats, open);
            self.sections.motion.remove(&Section::Chats);
            cx.notify();
        }
    }

    /// "+" and fork beside the Chats caret: a fresh side chat of the active
    /// chat, or a fork of it through its latest completed response.
    fn render_chats_header_actions(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        // Hidden until the header is hovered, like the sidebar's row menus:
        // the caret is the resting state, the actions appear on approach.
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .opacity(0.0)
            .group_hover(HEADER_GROUP, |s| s.opacity(1.0))
            .child(
                header_action(
                    "files-sections-new-chat",
                    icons::PLUS,
                    "New side chat",
                    theme,
                )
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(FilesEvent::NewChildChat);
                })),
            )
            .child(
                header_action(
                    "files-sections-fork",
                    icons::FORK,
                    "Fork this chat",
                    theme,
                )
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(FilesEvent::ForkChat);
                })),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_section(
        &mut self,
        section: Section,
        count: usize,
        wanted: f32,
        height: f32,
        actions: Option<AnyElement>,
        badge: Option<AnyElement>,
        body: AnyElement,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.sections.is_open(section);
        self.sections.begin_reveal(section, height);
        // The collapsed header carries the count; open, the rows speak.
        let label: SharedString = if open || count == 0 {
            section.label().into()
        } else {
            format!("{} ({count})", section.label()).into()
        };
        // Toggling animates between 0 and the height this section gets from
        // the budget (the body scrolls inside it); a closed section reopens
        // to what it wants within the whole budget.
        let full = if open {
            height
        } else {
            wanted.min(FOOTER_HEIGHT - chrome_height()).max(0.0)
        };
        let header = div()
            .id(SharedString::from(format!(
                "files-section-{}",
                section.key()
            )))
            .debug_selector(move || format!("files-section-{}", section.key())) // slopty: tests
            .group(HEADER_GROUP)
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!(
                "{} {}",
                if open { "Collapse" } else { "Expand" },
                section.label()
            )))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .h(px(SECTION_HEADER_HEIGHT))
            .pl(px(Theme::SPACE_SM))
            .pr(px(4.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                // Open → close runs from the painted body height to 0, and
                // back up to what the budget allows.
                let was_open = this.sections.is_open(section);
                let (resting, target) = if was_open { (full, 0.0) } else { (0.0, full) };
                this.sections.toggle(section, resting, target);
                cx.emit(FilesEvent::ChatsOpenChanged(!was_open)); // slopty: remembered
                cx.notify();
            }))
            .child(
                // The badge trails the title, not the caret: the title
                // truncates first, the badge never does.
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted.opacity(0.5))
                            .child(label),
                    )
                    .children(badge),
            )
            .children(actions)
            .child(self.render_chevron(section, open, theme));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(header)
            .child(self.render_disclosure_body(section, open, height, body))
            .into_any_element()
    }

    fn render_chevron(&self, section: Section, open: bool, theme: &Theme) -> AnyElement {
        let chevron = icon(icons::ALT_ARROW_RIGHT)
            .size(px(12.0))
            .text_color(theme.text_muted.opacity(0.5));
        let frame = div()
            .flex_none()
            .size(px(20.0))
            .flex()
            .items_center()
            .justify_center();
        if let Some(tween) = self.sections.live_motion(section) {
            let denominator = tween.from.max(tween.to).max(1.0);
            let from = (tween.from / denominator).clamp(0.0, 1.0);
            let to = (tween.to / denominator).clamp(0.0, 1.0);
            frame
                .child(chevron.with_animation(
                    SharedString::from(format!(
                        "files-section-chevron-{}-{}",
                        section.key(),
                        tween.epoch
                    )),
                    collapse_animation(),
                    move |el, t| {
                        let reveal = motion::lerp(from, to, t);
                        el.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                            reveal * 0.25,
                        )))
                    },
                ))
                .into_any_element()
        } else {
            let resting = if open { 0.25 } else { 0.0 };
            frame
                .child(
                    chevron.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                        resting,
                    ))),
                )
                .into_any_element()
        }
    }

    fn render_disclosure_body(
        &self,
        section: Section,
        open: bool,
        height: f32,
        content: AnyElement,
    ) -> AnyElement {
        let target = if open { height } else { 0.0 };
        let frame = div().w_full().flex_none().overflow_hidden().child(content);
        let Some(tween) = self.sections.live_motion(section) else {
            return frame.h(px(target)).into_any_element();
        };
        let denominator = tween.from.max(tween.to).max(1.0);
        frame
            .with_animation(
                SharedString::from(format!(
                    "files-section-body-{}-{}",
                    section.key(),
                    tween.epoch
                )),
                collapse_animation(),
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

    fn render_show_more(
        &self,
        section: Section,
        remaining: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        show_more_row(
            format!("files-section-{}-more", section.key()),
            remaining,
            0.0,
            theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            let shown = this.sections.shown(section) + PAGE_ROWS;
            this.sections.shown.insert(section, shown);
            cx.notify();
        }))
        .into_any_element()
    }

    fn render_chat_rows(
        &self,
        rows: &[ChildChatRow],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if rows.is_empty() {
            let actions = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(EMPTY_ACTIONS_HEIGHT))
                .child(
                    pill_button(
                        "files-sections-empty-fork",
                        icons::FORK,
                        "Fork",
                        theme,
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(FilesEvent::ForkChat);
                    })),
                )
                .child(
                    pill_button(
                        "files-sections-empty-new",
                        icons::PLUS,
                        "New side chat",
                        theme,
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(FilesEvent::NewChildChat);
                    })),
                )
                .into_any_element();
            return empty_state(
                "Side chats will appear here when they are created",
                Some(actions),
                theme,
            );
        }
        let view = cx.entity_id();
        let shown = self.sections.shown(Section::Chats);
        let scroll = self.sections.scroll(Section::Chats);
        let mut list = row_list("files-chat-rows", &scroll);
        for row in rows.iter().take(shown) {
            let glyph = status_glyph(
                format!("files-chat-{}", row.chat_id),
                row.status,
                view,
                theme,
                cx,
            );
            let open_id = row.chat_id.clone();
            let menu_id = row.chat_id.clone();
            let rename_input = self
                .sections
                .chat_rename
                .as_ref()
                .filter(|(id, _)| *id == row.chat_id)
                .map(|(_, input)| input.clone());
            let title = match rename_input {
                Some(input) => crate::shell::chat_title_editor(
                    format!("files-chat-title-editor-{}", row.chat_id).into(),
                    input,
                    theme,
                ),
                None => row_title(
                    format!("files-chat-title-{}", row.chat_id),
                    row.title.clone(),
                )
                .into_any_element(),
            };
            list = list.child(
                compact_row(format!("files-chat-{}", row.chat_id), theme)
                    .aria_label(SharedString::from(format!("Open side chat {}", row.title)))
                    .on_click(cx.listener(move |_, event: &gpui::ClickEvent, _, cx| {
                        cx.stop_propagation();
                        // The first click opened the tab; the second edits
                        // the title in place.
                        if event.click_count() >= 2 {
                            cx.emit(FilesEvent::RenameChildChat(open_id.clone()));
                        } else {
                            cx.emit(FilesEvent::OpenChildChat(open_id.clone()));
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |_, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            cx.emit(FilesEvent::ChildChatContextMenu {
                                chat_id: menu_id.clone(),
                                position: event.position,
                            });
                        }),
                    )
                    .child(glyph)
                    .child(title)
                    .children(row.change_request.clone().map(|summary| {
                        crate::change_requests::pull_request_badge(
                            format!("files-chat-pr-{}", row.chat_id).into(),
                            summary,
                            crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                            Some(row.device_id.clone()),
                            theme,
                        )
                    }))
                    .child(time_ago_label(row.time_ago.clone(), theme)),
            );
        }
        if rows.len() > shown {
            list = list.child(self.render_show_more(Section::Chats, rows.len() - shown, theme, cx));
        }
        faded_list(list, &scroll)
    }
}

fn collapse_animation() -> Animation {
    motion::COLLAPSE.animation()
}

/// A 20px icon button in a section header, sized to sit beside the caret.
fn header_action(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(20.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(crate::theme::wash(0.09)))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .tooltip(crate::settings::widgets::text_tooltip(label))
        // Icons take their color on the element itself, never inherited.
        .child(
            icon(icon_path)
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.85)),
        )
}

/// A section's "Show N more" row (a page is [`PAGE_ROWS`]), indented to line
/// up with the rows it pages.
fn show_more_row(
    id: String,
    remaining: usize,
    indent: f32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .role(gpui::Role::Button)
        .flex_none()
        .h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .pl(px(Theme::SPACE_SM + indent))
        .pr(px(Theme::SPACE_SM))
        .rounded(px(8.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted.opacity(0.7))
        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text_muted))
        .child(format!("Show {} more", remaining.min(PAGE_ROWS)))
}

/// The empty state's pill buttons — the explorer's Retry button shape.
fn pill_button(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .h(px(26.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(theme.border)
        .bg(crate::theme::wash(0.04))
        .hover(|style| style.bg(crate::theme::wash(0.09)))
        .cursor_pointer()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.0))
        .text_size(px(11.5))
        .text_color(theme.text)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .child(icon(icon_path).size(px(12.0)).text_color(theme.text_muted))
        .child(label)
}

/// The scrolling column an open section's rows live in; the body frame
/// sets the height, the list fills it and reports its overflow through
/// `scroll` for the edge fades.
fn row_list(id: &'static str, scroll: &ScrollHandle) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size_full()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
        .pt(px(SECTION_BODY_INSET))
        .overflow_y_scroll()
        .track_scroll(scroll)
}

/// A section list under the sidebar's overflow fades: rows dissolve at the
/// edges while there is more to scroll to, plain when everything fits.
fn faded_list(list: gpui::Stateful<gpui::Div>, scroll: &ScrollHandle) -> AnyElement {
    crate::edge_fade::edge_faded(
        LIST_FADE_BAND,
        true,
        true,
        div().relative().size_full().child(list),
    )
    .fade_overflow_y(scroll)
    .into_any_element()
}

/// An empty section: its copy where the first row would sit, and any
/// actions on a row below it.
fn empty_state(copy: &'static str, actions: Option<AnyElement>, theme: &Theme) -> AnyElement {
    div()
        .pt(px(SECTION_BODY_INSET + EMPTY_PAD))
        .pb(px(EMPTY_PAD))
        .px(px(Theme::SPACE_SM))
        .flex()
        .flex_col()
        .child(
            div()
                .min_h(px(EMPTY_COPY_HEIGHT))
                .max_h(px(EMPTY_COPY_HEIGHT))
                .overflow_hidden()
                .py(px(2.0))
                .text_size(crate::typography::ui_rems(12.0))
                .line_height(px(16.0))
                .text_color(theme.text_muted.opacity(0.5))
                .child(copy),
        )
        .children(actions)
        .into_any_element()
}

/// The sidebar's compact session row, stripped to status + title (+ time):
/// 29px, 8px radius, the glass hover wash, 13px title on a 17px line.
fn compact_row(id: String, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .role(gpui::Role::Button)
        .flex_none()
        .h(px(ROW_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .rounded(px(8.0))
        .px(px(Theme::SPACE_SM))
        .cursor_pointer()
        .text_color(theme.text.opacity(0.8))
        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
}

/// The compact row's trailing time, 11px in the faded subline color.
fn time_ago_label(text: SharedString, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted.opacity(0.5))
        .child(text)
}

/// The sidebar's fading label: overflow dissolves at the right edge instead
/// of an ellipsis.
fn row_title(id: String, title: SharedString) -> impl IntoElement {
    crate::shell::sidebar_faded_label(
        id.into(),
        true,
        div()
            .text_size(crate::typography::ui_rems(13.0))
            .line_height(px(17.0))
            .child(title),
    )
}

/// The compact row's 13px status slot: Working animates the glyph spinner,
/// Completed wears the check, the rest a 6px dot in the status color.
fn status_glyph(
    key: String,
    status: ChatIndicator,
    view: EntityId,
    theme: &Theme,
    cx: &mut gpui::App,
) -> AnyElement {
    let color = crate::shell::spaces::status_dot_color(status, theme);
    let glyph: AnyElement = match status {
        ChatIndicator::Completed => icon(icons::CHECK)
            .size(px(11.0))
            .flex_none()
            .text_color(color)
            .into_any_element(),
        ChatIndicator::Working => {
            loaders::mini_glyph_spinner(format!("{key}-working"), 2.0, theme.glyph, view, cx)
                .into_any_element()
        }
        _ => div()
            .size(px(6.0))
            .flex_none()
            .rounded_full()
            .bg(color)
            .into_any_element(),
    };
    div()
        .flex_none()
        .size(px(13.0))
        .flex()
        .items_center()
        .justify_center()
        .child(glyph)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, parent: Option<&str>, minutes_ago: i64) -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "deviceId": "dev",
            "archived": false,
            "createdAt": Utc::now() - chrono::Duration::minutes(minutes_ago),
            "parentChatId": parent,
        }))
        .unwrap()
    }

    #[test]
    fn child_chat_rows_are_live_children_newest_first() {
        let mut state = AppState::new();
        let mut archived = chat("old", Some("main"), 1);
        archived.archived = true;
        let mut titled = chat("b", Some("main"), 30);
        titled.title = Some("Investigate caching".into());
        state.apply_chats(vec![
            chat("main", None, 60),
            chat("a", Some("main"), 5),
            titled,
            chat("unrelated", Some("elsewhere"), 2),
            archived,
        ]);
        let rows = child_chat_rows(&state, "main", Utc::now());
        assert_eq!(
            rows.iter().map(|r| r.chat_id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(rows[0].title.as_ref(), "New side chat");
        assert_eq!(rows[1].title.as_ref(), "Investigate caching");
        assert_eq!(rows[0].status, ChatIndicator::Idle);
    }

    #[gpui::test]
    fn double_clicked_side_chat_asks_for_an_inline_rename(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, MouseDownEvent, MouseUpEvent};

        let (files, cx) = super::super::test_support::setup(cx);
        files.update(cx, |files, cx| {
            files.state.update(cx, |state, _| {
                let mut side = chat("side", Some("chat"), 1);
                side.title = Some("Side work".into());
                state.chats.push(side);
            });
            files.set_chats_open(true, cx);
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = events.clone();
        let _sub = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });
        let row = cx.debug_bounds("files-chat-title-side").unwrap();
        for click_count in [1, 2] {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position: row.center(),
                click_count,
                ..Default::default()
            });
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position: row.center(),
                click_count,
                ..Default::default()
            });
        }
        cx.run_until_parked();
        {
            let events = events.borrow();
            assert!(matches!(&events[..], [
                FilesEvent::OpenChildChat(open),
                FilesEvent::RenameChildChat(rename),
            ] if open == "side" && rename == "side"));
        }

        // The shell's field replaces the row title while the edit is open.
        let input = cx.update(|_, cx| cx.new(|cx| ComposerInput::new("Session title", cx)));
        files.update(cx, |files, cx| {
            files.set_chat_rename(Some(("side".into(), input)), cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("files-chat-title-editor-side").is_some());
        assert!(cx.debug_bounds("files-chat-title-side").is_none());
        files.update(cx, |files, cx| files.set_chat_rename(None, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("files-chat-title-side").is_some());
    }

    #[gpui::test]
    fn inline_rename_reveals_a_side_chat_in_a_collapsed_paged_section(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext as _;

        let (files, cx) = super::super::test_support::setup(cx);
        files.update(cx, |files, cx| {
            files.state.update(cx, |state, _| {
                for ix in 0..12 {
                    state
                        .chats
                        .push(chat(&format!("side-{ix:02}"), Some("chat"), ix));
                }
            });
            assert!(!files.sections.is_open(Section::Chats));
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });

        // Newest first: side-11 is the twelfth row, past the first page.
        let input = cx.update(|_, cx| cx.new(|cx| ComposerInput::new("Session title", cx)));
        files.update(cx, |files, cx| {
            files.set_chat_rename(Some(("side-11".into(), input)), cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        files.read_with(cx, |files, _| {
            assert!(files.sections.is_open(Section::Chats));
            assert_eq!(
                files.sections.shown(Section::Chats),
                INITIAL_ROWS + PAGE_ROWS
            );
            // The reveal became the section's opening motion.
            assert_eq!(files.sections.reveal, None);
            assert_eq!(files.sections.motion[&Section::Chats].from, 0.0);
        });
        assert!(cx.debug_bounds("files-chat-title-editor-side-11").is_some());
    }

    /// The Chats section starts closed, says each open and close the person
    /// makes so the shell keeps it, and lists side chats but no subagent.
    #[gpui::test]
    fn chats_section_starts_closed_and_says_its_toggles(cx: &mut gpui::TestAppContext) {
        let (files, cx) = super::super::test_support::setup(cx);
        files.update(cx, |files, cx| {
            files.state.update(cx, |state, _| {
                state.chats.push(chat("side", Some("chat"), 1));
                let mut sub = chat("sub", Some("chat"), 2);
                sub.subagent = true;
                state.chats.push(sub);
            });
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = events.clone();
        let _sub = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                if let FilesEvent::ChatsOpenChanged(open) = event {
                    recorded.borrow_mut().push(*open);
                }
            })
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(files.read_with(cx, |files, _| !files.sections.is_open(Section::Chats)));
        let header = cx.debug_bounds("files-section-chats").unwrap();
        cx.simulate_click(header.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*events.borrow(), [true]);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("files-chat-title-side").is_some());
        assert!(cx.debug_bounds("files-chat-title-sub").is_none(), "no subagent here");
        // Another explorer's toggle reaches this one quietly.
        files.update(cx, |files, cx| files.set_chats_open(false, cx));
        assert!(files.read_with(cx, |files, _| !files.sections.is_open(Section::Chats)));
        assert_eq!(*events.borrow(), [true]);
    }

    #[test]
    fn fingerprint_tracks_membership_and_status() {
        let mut state = AppState::new();
        let now = Utc::now();
        state.apply_chats(vec![chat("main", None, 60)]);
        let empty = fingerprint(&state, "main", now);
        state.apply_chats(vec![chat("main", None, 60), chat("a", Some("main"), 5)]);
        let one = fingerprint(&state, "main", now);
        assert_ne!(empty, one);
        assert_eq!(one, fingerprint(&state, "main", now));
    }

    #[test]
    fn content_height_pages_at_ten_rows_and_counts_the_show_more_row() {
        // Short lists are floored so a section keeps its presence.
        let one = content_height(Section::Chats, 1, INITIAL_ROWS);
        assert_eq!(one, MIN_BODY_HEIGHT);
        let ten = content_height(Section::Chats, 10, INITIAL_ROWS);
        // Eleven rows: ten visible plus the "Show more" slot.
        let eleven = content_height(Section::Chats, 11, INITIAL_ROWS);
        assert_eq!(eleven - ten, ROW_HEIGHT + ROW_GAP);
        // Paging once reveals up to 20 rows before the next "Show more".
        let paged = content_height(Section::Chats, 40, INITIAL_ROWS + PAGE_ROWS);
        assert_eq!(
            paged,
            SECTION_BODY_INSET + 21.0 * ROW_HEIGHT + 20.0 * ROW_GAP
        );
        // An empty section wants its copy and the action row.
        assert_eq!(
            content_height_unfloored(Section::Chats, 0, INITIAL_ROWS),
            SECTION_BODY_INSET + EMPTY_PAD * 2.0 + EMPTY_COPY_HEIGHT + EMPTY_ACTIONS_HEIGHT
        );
    }
}
