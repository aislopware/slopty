//! slopty: going back to before a message of the person's (Slopty's own; zeron has none).
//!
//! A user message's hover strip offers "Rewind…" when the chat's agent can go back
//! ([`slopty_view_doc::dock::DockCan`]: `rewind` puts the files back, `fork` branches the
//! conversation). It opens a small menu of what to take back: the code and the conversation, the
//! conversation alone, or the code alone (`.research/tui-presentation-2026-10-16.md` §1.13). The
//! view model answers with the message's words, which go back into the composer as a draft for
//! the person to send or change, and the branched chat when the conversation was branched; the
//! session view moves to it. While a turn is under way the action stays, dimmed, and says why.

use gpui::{AnyElement, Context, FocusHandle, Pixels, Point, SharedString, Task, div, prelude::*, px};
use slopty_view_doc::calls;
use slopty_view_doc::dock::{ChatDock, DockCan};

use super::{Transcript, TranscriptEvent};
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// What the menu takes back, in its order: (label, code, conversation).
const CHOICES: [(&str, bool, bool); 3] = [
    ("Code and conversation", true, true),
    ("Conversation only", false, true),
    ("Code only", true, false),
];

/// The menu's width.
const MENU_WIDTH: f32 = 240.0;

/// What choice `choice` of [`CHOICES`] does, said under the menu while it is the active row (or
/// the first, before any is): a choice that puts files back says that a shell command's changes
/// stay, as the agent's rewind only undoes its own edits.
fn note(choice: usize) -> String {
    let (_, code, conversation) = CHOICES[choice];
    let what = match (code, conversation) {
        (true, true) => {
            "Files and the chat go back to before this message; the message returns to the composer."
        }
        (false, _) => "The chat goes back; files stay as they are.",
        (true, false) => "Files go back; the chat stays.",
    };
    if code {
        format!("{what} Changes made by shell commands aren't undone.")
    } else {
        what.to_owned()
    }
}

/// The choices `can` allows, as indices into [`CHOICES`].
pub(super) fn choices(can: DockCan) -> Vec<usize> {
    CHOICES
        .iter()
        .enumerate()
        .filter(|(_, (_, code, conversation))| {
            (!code || can.rewind) && (!conversation || can.fork)
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// What the rewind action keeps between frames.
#[derive(Default)]
pub(super) struct RewindState {
    menu: crate::popover::Popup<RewindMenu>,
    focus: Option<FocusHandle>,
    /// The chat whose dock is watched, and the watch.
    watch: Option<(String, Task<()>)>,
    /// What the watched chat's agent can be asked.
    pub(super) can: DockCan,
    /// A turn is under way in the watched chat.
    pub(super) busy: bool,
    /// The rewind in flight.
    pending: Option<Task<()>>,
    /// Where each person's bubble was last drawn, by its entry: the menu hangs from its left edge.
    feet: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, gpui::Bounds<Pixels>>>>,
}

struct RewindMenu {
    chat: String,
    message: String,
    position: Point<Pixels>,
    active: Option<usize>,
}

impl Transcript {
    /// What notes where the bubble of entry `entry` draws, set in its foot.
    pub(super) fn bubble_foot_watch(&self, entry: &SharedString) -> AnyElement {
        let (feet, entry) = (self.rewind.feet.clone(), entry.to_string());
        gpui::canvas(
            move |bounds, _, _| {
                feet.borrow_mut().insert(entry, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }

    /// Keep the shown chat's dock watched for what its agent can be asked.
    pub(super) fn watch_rewind(&mut self, cx: &mut Context<Self>) {
        // A subagent's run shown in its card goes back only with its parent.
        let chat = self.doc_override.is_none().then(|| self.chat_id.clone()).flatten();
        if self.rewind.watch.as_ref().map(|(watched, _)| watched) == chat.as_ref() {
            return;
        }
        self.rewind.can = DockCan::default();
        self.rewind.busy = false;
        self.rewind.watch = None;
        let Some(chat) = chat else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({ "chatId": chat });
        let task = cx.spawn(async move |this, cx| {
            let Ok(mut rx) = engine.client().subscribe_checked(calls::WATCH_DOCK, params).await
            else {
                return;
            };
            while let Some(value) = rx.recv().await {
                let Ok(dock) = serde_json::from_value::<ChatDock>(value) else {
                    continue;
                };
                let (can, busy) = (dock.can, dock.activity.is_some());
                let alive = this.update(cx, |this, cx| {
                    if (this.rewind.can, this.rewind.busy) != (can, busy) {
                        this.rewind.can = can;
                        this.rewind.busy = busy;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        self.rewind.watch = Some((chat, task));
    }

    /// The hover strip's "Rewind…" for the user message `message`, when its agent can go back.
    pub(super) fn rewind_action(
        &self,
        message: &SharedString,
        focus: &gpui::FocusHandle,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chat = self.work_chat()?;
        if choices(self.rewind.can).is_empty() {
            return None;
        }
        let busy = self.rewind.busy || self.rewind.pending.is_some();
        let message_id = message.to_string();
        let fade_key = format!("rewind-message-hover-{message}");
        // slopty: a finger's press on a touch screen, the wash drawn on the 24 (#23).
        let (press, reach) = super::strip_press(format!("rewind-message-{message}"));
        let wash = if busy {
            gpui::transparent_black()
        } else {
            crate::motion::hover_blend(&fade_key, gpui::transparent_black(), crate::theme::ink(0.08))
        };
        Some(
            // slopty: a tab stop, whose focus shows its strip (#9).
            super::strip_control(press.track_focus(focus), theme, reach)
                .role(gpui::Role::Button)
                .aria_label("Rewind…")
                .when(busy, |el| el.aria_description("Once the turn ends"))
                .when(!busy, |el| {
                    el.cursor_pointer()
                        .hover_fade(fade_key)
                        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            this.open_rewind_menu(
                                chat.clone(),
                                message_id.clone(),
                                event.position(),
                                window,
                                cx,
                            );
                        }))
                })
                .tooltip(crate::settings::widgets::text_tooltip(if busy {
                    "Rewind once the turn ends"
                } else {
                    "Rewind…"
                }))
                .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                .child(
                    super::strip_drawn().bg(wash).child(
                        crate::icons::icon(crate::icons::RESTART)
                            .size(px(14.0))
                            .text_color(if busy { theme.text_faint } else { theme.text_muted }),
                    ),
                )
                .into_any_element(),
        )
    }

    fn open_rewind_menu(
        &mut self,
        chat: String,
        message: String,
        position: Point<Pixels>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.rewind.menu.take_press_was_open() {
            return;
        }
        // Hung from the bubble's left edge, under it, not from wherever the press landed; under
        // a bubble narrower than it, flush with its right edge, so it stays in the column.
        let position = self.rewind.feet.borrow().get(&message).map_or(position, |foot| {
            let left = foot.left().min(foot.right() - px(MENU_WIDTH));
            gpui::point(left, foot.bottom() + px(Theme::SPACE_XS))
        });
        self.rewind.menu.open(RewindMenu {
            chat,
            message,
            position,
            active: None,
        });
        let focus = self.rewind.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_rewind_menu(&mut self, cx: &mut Context<Self>) {
        if self.rewind.menu.begin_close(crate::clock::now(cx)) {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.rewind.menu);
            cx.notify();
        }
    }

    fn rewind_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        use crate::popover::MenuKey;
        let count = choices(self.rewind.can).len();
        let Some(menu) = self.rewind.menu.open_mut() else {
            return;
        };
        match crate::popover::classify_key(
            &event.keystroke.key,
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            MenuKey::Escape => self.close_rewind_menu(cx),
            MenuKey::Up => menu.active = crate::popover::menu_step(menu.active, count, -1),
            MenuKey::Down => menu.active = crate::popover::menu_step(menu.active, count, 1),
            MenuKey::Enter => {
                if let Some(active) = menu.active {
                    self.rewind_to(active, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Take back what the menu's `nth` shown choice names.
    fn rewind_to(&mut self, nth: usize, cx: &mut Context<Self>) {
        let Some(&choice) = choices(self.rewind.can).get(nth) else {
            return;
        };
        let Some(menu) = self.rewind.menu.as_open() else {
            return;
        };
        let (chat, message) = (menu.chat.clone(), menu.message.clone());
        self.close_rewind_menu(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let (_, code, conversation) = CHOICES[choice];
        let params = serde_json::json!({
            "chatId": chat.clone(),
            "messageId": message,
            "code": code,
            "conversation": conversation,
        });
        self.rewind.pending = Some(cx.spawn(async move |this, cx| {
            let answer = engine.client().call(calls::REWIND, params).await;
            this.update(cx, |this, cx| {
                this.rewind.pending = None;
                match answer {
                    Ok(value) => {
                        let prompt = value
                            .get("prompt")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let branched = value
                            .get("chatId")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned);
                        // slopty: S2's "Undo rewind" toast, when the rewind branched.
                        crate::composer::offer_undo_rewind(&value, this.state.clone(), cx);
                        // A branched chat takes the prompt as its draft whenever its composer
                        // comes to it, a fresh one (before the first turn) arriving later.
                        if let Some(branched) = &branched {
                            crate::composer::put_back_rewound(branched.clone(), prompt.clone(), cx);
                        }
                        cx.emit(TranscriptEvent::Rewound {
                            from: chat,
                            chat_id: branched,
                            prompt,
                        });
                    }
                    Err(error) => crate::toast::error(cx, format!("Couldn't rewind: {error}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Have the agent of chat `chat` take back what `can` says, as its dock would.
    #[cfg(test)]
    pub(super) fn let_rewind(&mut self, chat: &str, can: DockCan) {
        self.rewind.watch = Some((chat.to_owned(), Task::ready(())));
        self.rewind.can = can;
    }

    /// What the open menu's note says: what its active row does, or its first row's before any
    /// row is active.
    pub(super) fn rewind_note(&self) -> Option<String> {
        let menu = self.rewind.menu.get()?;
        let offered = choices(self.rewind.can);
        offered.get(menu.active.unwrap_or(0)).or(offered.first()).map(|&choice| note(choice))
    }

    /// The menu of what to take back, while open.
    pub(super) fn render_rewind_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.rewind.menu.get()?;
        let focus = self.rewind.focus.clone()?;
        let theme = Theme::of(cx).for_popup();
        let mut card = crate::popover::popover_card(&theme)
            .id("transcript-rewind-menu-card")
            .track_focus(&focus)
            .role(gpui::Role::Menu)
            .aria_label("Rewind to before this message")
            .w(px(MENU_WIDTH))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, _, cx| this.rewind_menu_key(event, cx)))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_rewind_menu(cx)))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(4.0))
                    .pb(px(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("Rewind to before this message"),
            );
        for (nth, choice) in choices(self.rewind.can).into_iter().enumerate() {
            let (label, _, _) = CHOICES[choice];
            let id = SharedString::from(format!("transcript-rewind-menu-{choice}"));
            card = card.child(
                crate::popover::menu_row(&theme, menu.active == Some(nth), id.clone())
                    .id(id)
                    .role(gpui::Role::MenuItem)
                    .aria_label(label)
                    // The note says what the row under the pointer does, as the keys' row.
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                        if let Some(menu) = this.rewind.menu.open_mut()
                            && menu.active != Some(nth)
                        {
                            menu.active = Some(nth);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.rewind_to(nth, cx)))
                    .child(label),
            );
        }
        let said = self.rewind_note().unwrap_or_default();
        card = card.child(
            div()
                .px(px(8.0))
                .pt(px(6.0))
                .pb(px(4.0))
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text_faint)
                .child(said),
        );
        Some(crate::popover::menu_at(
            "transcript-rewind-menu",
            menu.position,
            card.into_any_element(),
            self.rewind.menu.closing_at(crate::clock::now(cx)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menu offers only what the agent can do: a code rewind needs `rewind`, a branched
    /// conversation `fork`, both for the first choice.
    #[test]
    fn the_menu_offers_what_the_agent_can_take_back() {
        let can = |rewind, fork| DockCan { rewind, fork, ..DockCan::default() };
        let labels = |can| choices(can).into_iter().map(|ix| CHOICES[ix].0).collect::<Vec<_>>();
        assert_eq!(
            labels(can(true, true)),
            ["Code and conversation", "Conversation only", "Code only"]
        );
        assert_eq!(labels(can(false, true)), ["Conversation only"]);
        assert_eq!(labels(can(true, false)), ["Code only"]);
        assert!(labels(can(false, false)).is_empty());
    }

    /// The note says what each choice does, and a choice that puts files back that a shell
    /// command's changes stay.
    #[test]
    fn the_note_says_what_the_choice_does() {
        assert_eq!(
            note(0),
            "Files and the chat go back to before this message; the message returns to the \
             composer. Changes made by shell commands aren't undone."
        );
        assert_eq!(note(1), "The chat goes back; files stay as they are.");
        assert_eq!(note(2), "Files go back; the chat stays. Changes made by shell commands aren't undone.");
    }
}
