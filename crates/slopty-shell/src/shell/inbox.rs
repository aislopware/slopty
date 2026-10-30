//! The inbox (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a
//! feed"): the titlebar's eye, with a dot while anything calls, and behind it a list of what
//! needs the person, what failed and what finished unseen, across every project and machine,
//! then what is at work. Ordered by how loud each calls ([`super::marks::Mark::urgency`]), each
//! kind oldest first. A row is the program's mark, the title, the need or the last line in one
//! line, the state's mark and its age. It works from the keyboard alone: typing filters, ↑ ↓
//! walk, ⏎ lands on the session, Esc puts it away. It answers nothing: Slopty never answers for
//! the person (`docs/decisions/agents.md`, "TUI-first").
#![allow(
    clippy::arithmetic_side_effects,
    clippy::assert_is_empty,
    clippy::indexing_slicing,
    clippy::needless_pass_by_ref_mut,
    clippy::suboptimal_flops,
    reason = "shell lint migration: slice A"
)]

use std::cell::Cell;

use chrono::{DateTime, Utc};
use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Focusable as _, Pixels, Point, SharedString,
    Subscription, Window, div, px,
};
use slopty_view_proto::Chat;

use super::Shell;
use super::marks::Mark;
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::popover;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::state::AppState;
use crate::theme::Theme;

/// One row of the inbox.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
    pub chat_id: String,
    pub title: SharedString,
    /// Its need in the agent's words, else the last line the agent wrote.
    pub line: Option<SharedString>,
    pub mark: Mark,
    /// Since when it has stood as it is.
    pub since: DateTime<Utc>,
    /// Where it runs: its project, and its machine while the list spans machines.
    pub place: Option<SharedString>,
    pub harness: Option<slopty_view_proto::HarnessId>,
}

/// What the inbox lists at `now`: every session that calls or works, the loudest kind first,
/// each kind oldest first; a session at rest is not in it.
pub(super) fn entries(state: &AppState, now: DateTime<Utc>) -> Vec<Entry> {
    let chats = state.overview_chats(now);
    let mut machines: Vec<&str> = chats.iter().map(|(_, chat)| chat.device_id.as_str()).collect();
    machines.sort_unstable();
    machines.dedup();
    let spans_machines = machines.len() > 1;
    let mut entries: Vec<Entry> = chats
        .into_iter()
        .filter_map(|(status, chat)| {
            let session = state.session_for(&chat.id);
            let mark = Mark::of(status, session, now);
            let since = match &mark {
                Mark::AtRest => return None,
                Mark::NeedsYou(_) | Mark::AtWork => {
                    session.and_then(|session| session.status_since)
                }
                Mark::Failed | Mark::DoneUnseen => None,
            }
            .unwrap_or_else(|| touched(chat));
            let line = match &mark {
                Mark::NeedsYou(need) => Some(need.words.clone()),
                _ => chat
                    .last_message_preview
                    .as_deref()
                    .map(crate::transcript::single_line)
                    .filter(|line| !line.trim().is_empty())
                    .map(SharedString::from),
            };
            let project = state.space_for_chat(chat).map(|space| space.display_name().to_owned());
            let machine = spans_machines.then(|| state.device_name(&chat.device_id)).flatten();
            let place = match (project, machine) {
                (Some(project), Some(machine)) => Some(format!("{project} \u{b7} {machine}")),
                (Some(project), None) => Some(project),
                (None, Some(machine)) => Some(machine.to_owned()),
                (None, None) => None,
            };
            Some(Entry {
                chat_id: chat.id.clone(),
                title: crate::transcript::single_line(
                    chat.title.as_deref().unwrap_or("New session"),
                )
                .into(),
                line,
                mark,
                since,
                place: place.map(SharedString::from),
                harness: chat.config.as_ref().map(|config| config.harness),
            })
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .mark
            .urgency()
            .cmp(&left.mark.urgency())
            .then_with(|| left.since.cmp(&right.since))
            .then_with(|| left.chat_id.cmp(&right.chat_id))
    });
    entries
}

/// Of `entries`, those whose title, line or place holds every word of `typed`, any case; all of
/// them while nothing is typed.
pub(super) fn filtered(entries: Vec<Entry>, typed: &str) -> Vec<Entry> {
    let words: Vec<String> = typed.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter(|entry| {
            let text = [Some(&entry.title), entry.line.as_ref(), entry.place.as_ref()]
                .into_iter()
                .flatten()
                .map(|words| words.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
            words.iter().all(|word| text.contains(word.as_str()))
        })
        .collect()
}

/// When `chat` was last touched: its last message, else its start.
fn touched(chat: &Chat) -> DateTime<Utc> {
    chat.last_message_at.unwrap_or(chat.created_at)
}

// ---- the eye and its popover ----

thread_local! {
    /// Where the titlebar's eye was last drawn: the inbox hangs under it, whether a click or
    /// its key asked for it.
    static EYE: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
}

/// Under the eye, its right edge the inbox's: the eye stands at the titlebar's trailing end.
fn under_eye() -> Option<Point<Pixels>> {
    EYE.with(Cell::get)
        .map(|bounds| gpui::point(bounds.right() - px(INBOX_WIDTH), bounds.bottom() + px(4.0)))
}

/// The inbox's width (Rex's eye popover stands 300 to 360 pt wide).
const INBOX_WIDTH: f32 = 360.0;

/// The rows it shows before it scrolls.
const INBOX_ROWS_SHOWN: f32 = 8.0;

/// A row's height: two lines, the title and the need or last line.
const INBOX_ROW_HEIGHT: f32 = 44.0;

/// The inbox's state, one per shell.
#[derive(Default)]
pub(super) struct Inbox {
    open: Option<Open>,
}

/// The inbox while it is up.
struct Open {
    search: Entity<ComposerInput>,
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    /// The row the keyboard is on, by chat id, so a row arriving above it never moves it.
    active: Option<String>,
    scroll: gpui::ScrollHandle,
    _events: Subscription,
}

impl Inbox {
    /// Whether it is up: it holds the keyboard.
    pub(super) const fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

impl Shell {
    /// The rows the inbox shows at the shell's clock, filtered by what is typed in it.
    fn inbox_rows(&self, cx: &gpui::App) -> Vec<Entry> {
        let all = entries(self.state.read(cx), crate::clock::utc(cx));
        let typed = self
            .inbox
            .open
            .as_ref()
            .map(|open| open.search.read(cx).text().to_owned())
            .unwrap_or_default();
        filtered(all, &typed)
    }

    /// The row the keyboard is on: the one it was on while that is still listed, else the first.
    fn inbox_active(&self, rows: &[Entry]) -> usize {
        let active = self.inbox.open.as_ref().and_then(|open| open.active.as_deref());
        active.and_then(|id| rows.iter().position(|row| row.chat_id == id)).unwrap_or(0)
    }

    /// The inbox, or put it away when it is up.
    pub(super) fn toggle_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox.open.is_some() {
            self.close_inbox(Some(window), cx);
            return;
        }
        self.command_palette = None;
        crate::live::chrome::release_held_keys(cx);
        let search =
            cx.new(|cx| ComposerInput::with_context("Filter\u{2026}", "PaletteSearch", cx));
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(open) = this.inbox.open.as_mut() {
                    open.active = None;
                    open.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                }
                cx.notify();
            }
        });
        self.inbox.open = Some(Open {
            search,
            previous_focus: window.focused(cx),
            focus_pending: true,
            active: None,
            scroll: gpui::ScrollHandle::new(),
            _events: events,
        });
        cx.notify();
    }

    /// Put the inbox away, the keyboard back where it was.
    fn close_inbox(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(open) = self.inbox.open.take() else { return };
        if let (Some(window), Some(focus)) = (window, open.previous_focus) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn move_inbox(&mut self, down: bool, cx: &mut Context<Self>) {
        let rows = self.inbox_rows(cx);
        if rows.is_empty() {
            return;
        }
        let at = self.inbox_active(&rows);
        let next = if down { (at + 1) % rows.len() } else { (at + rows.len() - 1) % rows.len() };
        if let Some(open) = self.inbox.open.as_mut() {
            open.active = Some(rows[next].chat_id.clone());
            open.scroll.scroll_to_item(next);
        }
        cx.notify();
    }

    /// Land on the session of row `chat_id`: the inbox goes, the session opens.
    fn land_from_inbox(&mut self, chat_id: String, cx: &mut Context<Self>) {
        // The keyboard goes to the session, not back where it was.
        self.inbox.open = None;
        self.open_chat(chat_id, cx);
        cx.notify();
    }

    /// The titlebar's eye: the inbox's door, with a dot in the loudest calling row's hue while
    /// anything calls. `ink` is the titlebar's theme, inked for the ground under the row; H's
    /// row places it at its trailing end.
    pub(super) fn render_inbox_button(
        &mut self,
        ink: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let calls = entries(self.state.read(cx), crate::clock::utc(cx));
        let calling = |entry: &&Entry| entry.mark.urgency() > Mark::AtWork.urgency();
        let loudest =
            super::marks::loudest(calls.iter().filter(calling).map(|entry| &entry.mark)).cloned();
        let waiting = calls.iter().filter(calling).count();
        let dot = loudest.as_ref().and_then(|mark| mark.hue(ink)).map(|hue| {
            div()
                .debug_selector(|| "inbox-dot".into())
                .absolute()
                .top(px(3.0))
                .right(px(3.0))
                .size(px(6.0))
                .rounded_full()
                .bg(hue)
        });
        let said: SharedString = match waiting {
            0 => "Inbox".into(),
            n => format!("Inbox, {n} waiting").into(),
        };
        let button = super::window_control_button(
            "titlebar-inbox",
            icons::EYE,
            ink,
            cx.listener(|this, _: &gpui::ClickEvent, window, cx| this.toggle_inbox(window, cx)),
        )
        .action_hint("Inbox", Keys::Action(Box::new(super::chrome_keys::Inbox)), cx)
        .role(gpui::Role::Button)
        .aria_label(said);
        div()
            .relative()
            .flex_none()
            .on_children_prepainted(|bounds, _, _| EYE.with(|eye| eye.set(bounds.first().copied())))
            .child(button)
            .children(dot)
            .into_any_element()
    }

    /// The inbox under the eye, while it is up.
    pub(super) fn render_inbox(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        // Under the eye; where no eye is drawn (a phone's list), under the titlebar's end.
        let position = under_eye().unwrap_or_else(|| {
            let viewport = window.viewport_size();
            gpui::point(viewport.width - px(INBOX_WIDTH + 8.0), px(Theme::TITLEBAR_HEIGHT))
        });
        let rows = self.inbox_rows(cx);
        let active = self.inbox_active(&rows);
        let now = crate::clock::utc(cx);
        let open = self.inbox.open.as_mut()?;
        if std::mem::take(&mut open.focus_pending) {
            window.focus(&open.search.focus_handle(cx), cx);
        }
        let search = open.search.clone();
        let scroll = open.scroll.clone();
        let typed = search.read(cx).text().to_owned();
        let mut children: Vec<AnyElement> = Vec::new();
        for (ix, row) in rows.iter().enumerate() {
            let id = row.chat_id.clone();
            let hover_id = row.chat_id.clone();
            children.push(
                inbox_row(row, ix == active, &typed, now, &theme)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.land_from_inbox(id.clone(), cx)),
                    )
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                        if let Some(open) = this.inbox.open.as_mut()
                            && open.active.as_deref() != Some(hover_id.as_str())
                        {
                            open.active = Some(hover_id.clone());
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty().then(|| {
            let (title, hint) = if typed.trim().is_empty() {
                ("Nothing needs you", "A session that asks, fails or finishes shows here.")
            } else {
                ("Nothing matches", "No session here says that.")
            };
            super::command_palette::palette_empty(&theme, title, hint)
        });
        let body = div()
            .id("inbox-rows")
            .max_h(px(INBOX_ROW_HEIGHT * INBOX_ROWS_SHOWN + 16.0))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(2.0))
            .p(px(6.0))
            .children(children)
            .children(empty);
        let card = popover::popover_card_flush(&theme)
            .id("inbox-card")
            .debug_selector(|| "inbox-card".into())
            .role(gpui::Role::Dialog)
            .aria_label("Inbox")
            .w(px(INBOX_WIDTH))
            .key_context("Inbox")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" | "down" => this.move_inbox(event.keystroke.key == "down", cx),
                    "enter" => {
                        let rows = this.inbox_rows(cx);
                        if let Some(row) = rows.get(this.inbox_active(&rows)) {
                            this.land_from_inbox(row.chat_id.clone(), cx);
                        }
                    }
                    "escape" => this.close_inbox(Some(window), cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_inbox(Some(window), cx)),
            )
            .child(body)
            // The filter at its foot, as Rex's.
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .border_t_1()
                    .border_color(crate::theme::hairline(0.06))
                    .child(popover::palette_search_icon(&theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(13.0))
                            .child(search),
                    ),
            )
            .into_any_element();
        Some(popover::menu_at("inbox-menu", position, card, None))
    }
}

/// One row of the inbox: the program's mark, the title over the need or last line, and the
/// state's mark over its age.
fn inbox_row(
    row: &Entry,
    active: bool,
    typed: &str,
    now: DateTime<Utc>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let brand = row.harness.map_or((icons::TERMINAL, None), crate::pickers::harness_brand_icon);
    let program =
        icon(brand.0).size(px(14.0)).flex_none().text_color(brand.1.unwrap_or(theme.text_muted));
    let age: SharedString = crate::state::format_time_ago(row.since, now).into();
    let mark = super::marks::mark_element(
        SharedString::from(format!("inbox-mark-{}", row.chat_id)),
        format!("inbox-mark-{}", row.chat_id),
        &row.mark,
        crate::loaders::status_mark::MARK_SMALL,
        theme.bg,
    );
    let said = [
        Some(row.title.to_string()),
        row.mark.word().map(str::to_owned),
        row.line.as_ref().map(ToString::to_string),
        row.place.as_ref().map(ToString::to_string),
        Some(age.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");
    let words = (!typed.trim().is_empty()).then_some(typed);
    popover::menu_row(theme, active, format!("inbox-row-{}", row.chat_id))
        .id(SharedString::from(format!("inbox-row-{}", row.chat_id)))
        .debug_selector({
            let id = format!("inbox-row-{}", row.chat_id);
            move || id
        })
        .rounded(px(popover::PALETTE_ITEM_RADIUS))
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(said))
        .min_h(px(INBOX_ROW_HEIGHT))
        .py(px(5.0))
        .child(program)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().truncate().child(popover::search_highlight(
                    row.title.clone(),
                    words,
                    theme,
                )))
                .children(row.line.clone().map(|line| {
                    div()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(popover::search_highlight(line, words, theme))
                })),
        )
        .child(
            div().flex_none().flex().flex_col().items_end().gap(px(4.0)).children(mark).child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_features(crate::typography::tabular())
                    .text_color(theme.text_faint)
                    .child(age),
            ),
        )
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use slopty_view_proto::{Session, SessionStatus, SessionWait};

    use super::*;

    fn chat(id: &str, device: &str, touched: i64, seen: bool, now: DateTime<Utc>) -> Chat {
        let last = now - TimeDelta::minutes(touched);
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("Chat {id}"), "deviceId": device, "archived": false,
            "createdAt": now - TimeDelta::hours(1), "lastMessageAt": last,
            "lastSeenAt": if seen { last } else { last - TimeDelta::seconds(1) },
            "lastMessagePreview": format!("{id} said this"),
        }))
        .unwrap()
    }

    fn session(chat: &str, status: SessionStatus, since: i64, now: DateTime<Utc>) -> Session {
        Session {
            last_completed_turn: None,
            chat_id: chat.into(),
            device_id: "local".into(),
            status,
            started_at: Some(now - TimeDelta::minutes(30)),
            updated_at: now,
            running_subagents: 0,
            status_since: Some(now - TimeDelta::minutes(since)),
            waiting_on: None,
        }
    }

    /// Two asking (the longer waiting first), one done unseen, one at work and one at rest.
    fn state(now: DateTime<Utc>) -> AppState {
        let mut state = AppState::new();
        state.local_device_id = Some("local".into());
        state.chats = vec![
            chat("rest", "local", 1, true, now),
            chat("work", "local", 1, true, now),
            chat("done", "local", 3, false, now),
            chat("ask-new", "local", 1, true, now),
            chat("ask-old", "local", 1, true, now),
        ];
        let mut old = session("ask-old", SessionStatus::AwaitingInput, 9, now);
        old.waiting_on = Some(SessionWait {
            kind: SessionWait::APPROVAL.into(),
            summary: Some("run cargo test".into()),
        });
        state.apply_sessions(vec![
            session("work", SessionStatus::Working, 5, now),
            session("ask-new", SessionStatus::AwaitingInput, 2, now),
            old,
        ]);
        state
    }

    /// The loudest kind first, each kind oldest first, at work last; at rest is not listed. An
    /// asking row says its need in the agent's words; another its last line.
    #[test]
    fn the_inbox_lists_by_urgency_then_age() {
        let now = Utc::now();
        let rows = entries(&state(now), now);
        let ids: Vec<&str> = rows.iter().map(|row| row.chat_id.as_str()).collect();
        assert_eq!(ids, ["ask-old", "ask-new", "done", "work"]);
        assert!(
            rows[0].line.as_deref().is_some_and(|line| line.contains("run cargo test")),
            "{:?}",
            rows[0].line
        );
        assert_eq!(rows[2].line.as_deref(), Some("done said this"));
        assert_eq!(rows[2].mark, Mark::DoneUnseen);
    }

    /// Typing narrows by title, need and place, every word, any case; nothing typed lists all.
    #[test]
    fn typing_filters_the_inbox() {
        let now = Utc::now();
        let rows = entries(&state(now), now);
        let ids = |typed: &str| -> Vec<String> {
            filtered(rows.clone(), typed).into_iter().map(|row| row.chat_id).collect()
        };
        assert_eq!(ids("").len(), 4);
        assert_eq!(ids("CARGO"), ["ask-old"], "by the need's words");
        assert_eq!(ids("chat done"), ["done"], "every word");
        assert!(ids("nothing here").is_empty());
    }
}
