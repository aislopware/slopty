// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! Settings → Archived (feature-inventory §1.5): archived sessions across
//! devices, with Unarchive (Mutate setChatArchived false).
//! slopty: no Delete all: a session's record is its agent's own, so Slopty archives and never
//! deletes.

use gpui::{
    AnyElement, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};

use slopty_view_proto::Chat;
use slopty_view_rpc::methods;

use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Archived rows in sidebar (recency) order. Pure. Side chats stay listed:
/// once archived and closed, this is the only place to restore them.
pub fn archived_chats(chats: &[Chat]) -> Vec<&Chat> {
    chats.iter().filter(|c| c.archived).collect()
}

const ARCHIVE_PAGE_SIZE: usize = 40;

fn archive_page(total: usize, requested: usize) -> usize {
    requested.min(total.saturating_sub(1) / ARCHIVE_PAGE_SIZE)
}

pub struct ArchivedPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    error: Option<SharedString>,
    /// Chat with an in-flight unarchive (button shows working state).
    busy: Option<String>,
    page: usize,
    task: Option<Task<()>>,
    /// slopty: the moment the rows say how long ago they moved by: taken as the state moves and
    /// on [`Self::clock_tick`], never as the page is built, so a page built again (from scratch,
    /// or for a hover) says what the one shown says.
    clock: chrono::DateTime<chrono::Utc>,
    /// slopty: the clock taken again when the soonest row's time next reads differently.
    clock_tick: Option<Task<()>>,
    _observe: Subscription,
}

impl ArchivedPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |page, _, cx| {
            page.take_clock(cx);
            cx.notify();
        });
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            error: None,
            busy: None,
            page: 0,
            task: None,
            clock: crate::clock::utc(cx),
            clock_tick: None,
            _observe: observe,
        };
        page.take_clock(cx);
        page
    }

    /// slopty: take the clock now, and again when the soonest archived chat's time next reads
    /// differently ([`crate::state::time_ago_changes`]), drawing the page then.
    fn take_clock(&mut self, cx: &mut Context<Self>) {
        self.clock = crate::clock::utc(cx);
        let now = self.clock;
        let next = archived_chats(&self.state.read(cx).chats)
            .into_iter()
            .filter_map(|chat| crate::state::time_ago_changes(chat.last_message_at.unwrap_or(chat.created_at), now))
            .min();
        self.clock_tick = next.map(|at| {
            // A hair past the change, so the clock taken then reads it.
            let wait = (at - now).to_std().unwrap_or_default() + std::time::Duration::from_millis(5);
            cx.spawn(async move |page, cx| {
                cx.background_executor().timer(wait).await;
                page.update(cx, |page, cx| {
                    page.take_clock(cx);
                    cx.notify();
                })
                .ok();
            })
        });
    }

    fn unarchive(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = Some(chat_id.clone());
        self.error = None;
        let params = serde_json::json!({
            "op": "setChatArchived",
            "chatId": chat_id,
            "archived": false,
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |page, cx| {
                page.busy = None;
                if let Err(err) = result {
                    page.error = Some(format!("Couldn't unarchive this chat: {err}").into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for ArchivedPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for ArchivedPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::toast::take_error(&mut self.error, cx);
        let theme = Theme::of(cx).for_settings_surface();
        // slopty: the page's own clock, which a tick moves and draws ([`Self::take_clock`]).
        let now = self.clock;
        let (rows, device_names, count): (
            Vec<Chat>,
            std::collections::HashMap<String, String>,
            usize,
        ) = {
            let state = self.state.read(cx);
            let archived = archived_chats(&state.chats);
            let count = archived.len();
            let page = archive_page(count, self.page);
            if self.page != page {
                self.page = page;
                self.scroll.reset();
            }
            let rows = archived
                .into_iter()
                .skip(self.page * 40)
                .take(40)
                .cloned()
                .collect();
            let names = state
                .devices
                .iter()
                .map(|d| (d.id.clone(), d.name.clone()))
                .collect();
            (rows, names, count)
        };
        let busy = self.busy.clone();
        let page = self.page;
        let items: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, chat)| {
                let title: SharedString = chat
                    .title
                    .clone()
                    .unwrap_or_else(|| "Untitled chat".into())
                    .into();
                // Unknown device → no fragment at all (slopty renders the
                // device span only when the name resolves).
                let device: Option<SharedString> =
                    device_names.get(&chat.device_id).cloned().map(Into::into);
                let at = chat.last_message_at.unwrap_or(chat.created_at);
                let time_ago: SharedString = crate::state::format_time_ago(at, now).into();
                // slopty: where it ran as the app says it everywhere ("project @ device"), then
                // its branch, as one meta line.
                let place: Option<SharedString> = place_of(&chat, device.as_deref()).map(Into::into);
                let branch: Option<SharedString> = chat
                    .branch
                    .as_deref()
                    .map(str::trim)
                    .filter(|branch| !branch.is_empty())
                    .map(|branch| SharedString::from(branch.to_owned()));
                let is_busy = busy.as_deref() == Some(chat.id.as_str());
                let chat_id = chat.id.clone();
                // slopty: a card's row like every other page's: its tile, its title and meta, its
                // time and Unarchive.
                let meta: Vec<AnyElement> = place
                    .into_iter()
                    .chain(branch)
                    .map(|fragment| div().min_w_0().truncate().child(fragment).into_any_element())
                    .collect();
                widgets::card_row(&theme, ix == 0)
                    .id(("archived-row", ix))
                    .flex_nowrap()
                    .child(widgets::row_tile(&theme, crate::icons::ARCHIVE_MINIMALISTIC))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(&theme, title).truncate())
                            .when(!meta.is_empty(), |el| el.child(widgets::meta_line(&theme, meta))),
                    )
                    // slopty: tabular figures, and the full date on hover (M39).
                    .child(
                        div()
                            .id(("archived-time", ix))
                            .flex_none()
                            .font_features(crate::typography::tabular())
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .tooltip(widgets::text_tooltip(absolute_time(at, now)))
                            .tooltip_show_delay(widgets::TOOLTIP_DELAY)
                            .child(time_ago),
                    )
                    .child(
                        widgets::action_button_when(&theme, widgets::ActionTone::Quiet, !is_busy)
                            .id(("unarchive", ix))
                            .flex_none()
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label("Unarchive") // slopty: named in the accessibility tree.
                            .aria_disabled(is_busy)
                            .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unarchive(chat_id.clone(), cx);
                            }))
                            .child(SharedString::from(if is_busy {
                                "Unarchiving…"
                            } else {
                                "Unarchive"
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        let archive_key = crate::keys::for_action(&crate::shell::ArchiveSession, None, window, cx);
        let body: AnyElement = if items.is_empty() {
            // Centered empty state (slopty settings.archived.tsx).
            div()
                .mt(px(96.0))
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .text_color(theme.text_muted)
                .child(
                    // `opacity-40` on top of the inherited muted/50 — an
                    // effectively ~20% glyph (slopty settings.archived.tsx).
                    crate::icons::icon(crate::icons::ARCHIVE_MINIMALISTIC)
                        .size(px(28.0))
                        .text_color(theme.text_muted.opacity(0.2)),
                )
                .child(
                    div()
                        .mt(px(12.0))
                        .text_size(crate::typography::ui_rems(14.0))
                        .child(SharedString::from("No archived sessions")),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(match archive_key {
                            Some(key) => format!(
                                "Archive a chat from its menu in the sidebar, or with {key}."
                            ),
                            None => "Archive a chat from its menu in the sidebar.".to_owned(),
                        })),
                )
                .into_any_element()
        } else {
            widgets::section_card(&theme).children(items).into_any_element()
        };

        let pagination = (count > ARCHIVE_PAGE_SIZE).then(|| {
            div()
                .mt(px(16.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    widgets::ghost_action(&theme)
                        .id("archived-previous")
                        .role(gpui::Role::Button)
                        .aria_label("Previous archived sessions")
                        .tab_index(0)
                        .when(page == 0, |el| {
                            el.opacity(crate::theme::DISABLED_OPACITY).cursor_default()
                        })
                        .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = this.page.saturating_sub(1);
                            this.scroll.reset();
                            cx.notify();
                        }))
                        .child("Previous"),
                )
                .child(format!(
                    "{}–{} of {}",
                    page * ARCHIVE_PAGE_SIZE + 1,
                    ((page + 1) * ARCHIVE_PAGE_SIZE).min(count),
                    count
                ))
                .child(
                    widgets::ghost_action(&theme)
                        .id("archived-next")
                        .role(gpui::Role::Button)
                        .aria_label("Next archived sessions")
                        .tab_index(0)
                        .when((page + 1) * ARCHIVE_PAGE_SIZE >= count, |el| {
                            el.opacity(crate::theme::DISABLED_OPACITY).cursor_default()
                        })
                        .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.page = archive_page(count, this.page + 1);
                            this.scroll.reset();
                            cx.notify();
                        }))
                        .child("Next"),
                )
        });
        let scrollbar = popover::rail(self, "archived-page-scrollbar", &theme, cx);
        div()
            .id("archived-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("archived-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                // slopty: one page head (`widgets::page_head`), no action: a
                                // session is never deleted here.
                                .child(widgets::page_head(
                                    &theme,
                                    "Archived sessions",
                                    (count > 0).then_some(count),
                                    "Hidden from the sidebar until restored.",
                                    None,
                                ))
                                .child(body)
                                .children(pagination),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
    }
}

// slopty: where an archived chat ran, as the app says it everywhere: "project @ device".
/// Where `chat` ran, on the machine named `device`: "project @ device", the project or the
/// machine alone, or nothing.
fn place_of(chat: &slopty_view_proto::Chat, device: Option<&str>) -> Option<String> {
    let project = chat
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !slopty_view_proto::projectless::is_projectless_cwd(Some(cwd)))
        .map(|cwd| slopty_view_proto::view::project_label(Some(cwd)));
    match (project, device) {
        (Some(project), Some(device)) => Some(format!("{project} @ {device}")),
        (Some(project), None) => Some(project),
        (None, Some(device)) => Some(format!("@ {device}")),
        (None, None) => None,
    }
}

// slopty: when a relative time was, in full, for its tooltip: "Oct 16, 14:05", with the year when
// it is not this one (M39).
pub(crate) fn absolute_time(then: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    absolute_time_in(then, &chrono::Local, now)
}

fn absolute_time_in<Tz: chrono::TimeZone>(
    then: chrono::DateTime<chrono::Utc>,
    zone: &Tz,
    now: chrono::DateTime<chrono::Utc>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    use chrono::Datelike as _;
    let local = then.with_timezone(zone);
    let clock = crate::transcript::clock_format();
    if local.year() == now.with_timezone(zone).year() {
        local.format(&format!("%b %-d, {clock}")).to_string()
    } else {
        local.format(&format!("%b %-d %Y, {clock}")).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn chat(id: &str, archived: bool) -> Chat {
        Chat {
            id: id.into(),
            device_id: "d".into(),
            title: None,
            archived,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc::now(),
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

    // slopty: a row's "now" becomes "1m" as its minute turns, by the page's own clock, which
    // stands still between its ticks however often the page is built.
    #[gpui::test]
    fn the_rows_times_move_on_by_the_page_s_own_clock(cx: &mut gpui::TestAppContext) {
        let now = cx.update(|cx| crate::clock::utc(cx));
        let mut archived = chat("a", true);
        archived.created_at = now - chrono::Duration::seconds(30);
        let at = archived.created_at;
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.chats = vec![archived];
            state
        });
        let page = cx.new(|cx| ArchivedPage::new(state, cx));
        let drawn = std::rc::Rc::new(std::cell::Cell::new(0));
        let _drawn = cx.update(|cx| {
            let drawn = drawn.clone();
            cx.observe(&page, move |_, _| drawn.set(drawn.get() + 1))
        });
        let says = |cx: &mut gpui::TestAppContext| {
            page.read_with(cx, |page, _| crate::state::format_time_ago(at, page.clock))
        };
        assert_eq!(says(cx), "now");
        cx.executor().advance_clock(std::time::Duration::from_secs(29));
        assert_eq!((says(cx), drawn.get()), ("now".to_owned(), 0), "nothing moves before the minute");
        cx.executor().advance_clock(std::time::Duration::from_secs(2));
        assert_eq!((says(cx), drawn.get()), ("1m".to_owned(), 1), "the minute turned, and the page drew");
    }

    // slopty: an archived chat says where it ran as the sidebar and the palette do, "project @
    // device", never the machine first with its own separators.
    #[test]
    fn an_archived_chat_says_where_it_ran_as_the_app_does() {
        let mut ran = chat("a", true);
        ran.cwd = Some("/work/surfaces".into());
        assert_eq!(place_of(&ran, Some("Studio")).as_deref(), Some("surfaces @ Studio"));
        assert_eq!(place_of(&ran, None).as_deref(), Some("surfaces"));
        ran.cwd = Some("~".into());
        assert_eq!(place_of(&ran, Some("Studio")).as_deref(), Some("@ Studio"), "no project");
        assert_eq!(place_of(&ran, None), None);
    }

    #[test]
    fn archive_page_clamps_after_removal_and_handles_exact_boundaries() {
        assert_eq!(archive_page(0, 9), 0);
        assert_eq!(archive_page(40, 1), 0);
        assert_eq!(archive_page(41, 1), 1);
        assert_eq!(archive_page(80, 2), 1);
        assert_eq!(archive_page(81, 2), 2);
    }

    #[test]
    fn only_archived_rows_show() {
        let chats = vec![chat("a", false), chat("b", true), chat("c", true)];
        let rows = archived_chats(&chats);
        let ids: Vec<&str> = rows.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn archived_side_chats_stay_restorable() {
        let mut side = chat("side", true);
        side.parent_chat_id = Some("main".into());
        let chats = vec![chat("main", false), side];
        let ids: Vec<&str> = archived_chats(&chats)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["side"]);
    }

    #[test]
    fn a_relative_time_reads_in_full_on_hover() {
        use chrono::{DateTime, Utc};
        let at = |text: &str| DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc);
        let now = at("2026-10-17T09:00:00Z");
        // slopty: the time of day as the system's clock says it, 12 or 24 hour.
        let (afternoon, morning) = if crate::transcript::clock_format() == "%H:%M" {
            ("14:05", "07:30")
        } else {
            ("2:05 PM", "7:30 AM")
        };
        assert_eq!(absolute_time_in(at("2026-10-16T14:05:00Z"), &Utc, now), format!("Oct 16, {afternoon}"));
        assert_eq!(
            absolute_time_in(at("2025-03-02T07:30:00Z"), &Utc, now),
            format!("Mar 2 2025, {morning}")
        );
    }
}
