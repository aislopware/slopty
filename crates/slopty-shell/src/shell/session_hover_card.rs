// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! The session hover card: dwell on a sidebar row and a frosted card floats
//! out beside it with what the row has no room for — the whole title, the
//! device and project, the branch, and the pull request.
//! Compact rows drop all of that, so the card is where it lives on.
//!
//! slopty: it also says which machine (project @ machine), the agent and model, how long the
//! chat has stood as it is, what it needs of the person in its agent's words, and how many
//! subagents it has running (`sidebar_status::chat_card`); a chat with no project shows its
//! folder. A screen reader reads it as a tooltip.
//!
//! The card waits out a short dwell so sweeping the pointer down the list
//! stays quiet. Once one has shown, the card is warm: the next row's card
//! follows the pointer at once, until it rests off the list for a moment.
use std::time::Instant;

use super::*;

use crate::change_requests::ChangeRequestBadgeModel;

/// How long the pointer rests on a row before its card shows.
const DWELL: Duration = Duration::from_millis(550);
/// How long after a card hides the next row's card skips the dwell.
const WARM: Duration = Duration::from_millis(400);
const CARD_WIDTH: f32 = 284.0;
/// The gap between the row's right edge and the card.
const CARD_GAP: f32 = 10.0;
const CARD_PAD: f32 = 12.0;

/// Which row's card shows, and which is waiting out its dwell.
#[derive(Default)]
pub(super) struct SessionHoverCard {
    shown: Option<String>,
    pending: Option<(String, Task<()>)>,
    hidden_at: Option<Instant>,
}

impl SessionHoverCard {
    pub(super) fn shown(&self) -> Option<&str> {
        self.shown.as_deref()
    }

    pub(super) fn target(&self) -> Option<&str> {
        self.shown()
            .or_else(|| self.pending.as_ref().map(|(id, _)| id.as_str()))
    }

    /// The pointer entered `chat_id`'s row. True when the card should show
    /// at once: another card is up, or one hid moments ago.
    fn enter(&mut self, chat_id: &str, now: Instant) -> bool {
        if self.shown.as_deref() == Some(chat_id) {
            return false;
        }
        let warm = self.shown.is_some()
            || self
                .hidden_at
                .is_some_and(|hidden| now.duration_since(hidden) < WARM);
        if warm {
            self.pending = None;
            self.shown = Some(chat_id.to_owned());
        }
        warm
    }

    /// The pointer left `chat_id`'s row. True when a shown card hid.
    fn leave(&mut self, chat_id: &str, now: Instant) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| pending == chat_id)
        {
            self.pending = None;
        }
        if self.shown.as_deref() != Some(chat_id) {
            return false;
        }
        self.shown = None;
        self.hidden_at = Some(now);
        true
    }

    /// The dwell on `chat_id` ran out with the pointer still there.
    fn settle(&mut self, chat_id: &str) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|(pending, _)| pending != chat_id)
        {
            return false;
        }
        self.pending = None;
        self.shown = Some(chat_id.to_owned());
        true
    }

    /// A click, menu, drag or scroll: hide now, and go cold so the card does
    /// not chase the pointer straight back.
    pub(super) fn dismiss(&mut self) -> bool {
        self.pending = None;
        self.hidden_at = None;
        self.shown.take().is_some()
    }
}

/// "21h ago" for the card's footer line ("now" stays as it is).
// slopty: then the time in full, which the row's "21h" leaves out: "21h ago · Oct 18, 09:12".
fn time_label(time_ago: &str, at: &str) -> String {
    if time_ago == "now" {
        format!("just now \u{b7} {at}")
    } else {
        format!("{time_ago} ago \u{b7} {at}")
    }
}

impl Shell {
    /// Reconcile from the container as well as the row: an unmounted row
    /// cannot report hover-out or run its paint-time hit test. Runs in the
    /// sidebar's render, which follows every shell notify; the shell itself
    /// only dismisses the card while the sidebar is hidden.
    pub(super) fn reconcile_session_hover_card(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.session_hover_card.target() else {
            return;
        };
        if !self.sidebar_hover_card_row_visible(target, cx)
            || self
                .rename_input_for(target)
                .is_some()
            || cx.has_active_drag()
        {
            self.dismiss_session_card(cx);
        }
    }

    /// Row hover in or out (the row's own hover listener forwards here).
    pub(super) fn hover_session_card(
        &mut self,
        chat_id: &str,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let now = crate::clock::now(cx);
        if !hovered {
            if self.session_hover_card.leave(chat_id, now) {
                cx.notify();
            }
            return;
        }
        if cx.has_active_drag() || self.chat_menu.is_open() {
            return;
        }
        if self.session_hover_card.enter(chat_id, now) {
            cx.notify();
            return;
        }
        if self.session_hover_card.shown() == Some(chat_id) {
            return;
        }
        let target = chat_id.to_owned();
        let task = cx.spawn({
            let target = target.clone();
            async move |this, cx| {
                cx.background_executor().timer(DWELL).await;
                let _ = this.update(cx, |this, cx| {
                    if this.session_hover_card.settle(&target) {
                        cx.notify();
                    }
                });
            }
        });
        self.session_hover_card.pending = Some((target, task));
        // Mount the hit test while waiting too, so moving/clipping the row
        // under a stationary pointer cancels the dwell before it settles.
        cx.notify();
    }

    pub(super) fn dismiss_session_card(&mut self, cx: &mut Context<Self>) {
        if self.session_hover_card.dismiss() {
            cx.notify();
        }
    }

    /// The card for `chat_id`'s row, mounted from the row itself: it floats
    /// from the row's right edge above everything else, clear of the
    /// sidebar's clip.
    pub(super) fn render_session_hover_card(
        &self,
        chat_id: &str,
        time_ago: SharedString,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let now = crate::clock::utc(cx);
        // slopty: Slopty's facts of the row (`sidebar_status::chat_card`).
        let facts = super::sidebar_status::chat_card(self.state.read(cx), chat_id, now)?;
        let (title, project, local, online, branch, change_request, at) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
            // slopty: when the row's time was, in full (the row says only "21h").
            let at = crate::settings::archived::absolute_time(
                chat.last_message_at.unwrap_or(chat.created_at),
                now,
            );
            let space = state.space_for_chat(chat);
            let project = space.map(|space| SharedString::from(space.display_name().to_owned()));
            let local = state.local_device_id.as_deref() == Some(chat.device_id.as_str());
            let online = state.device_online(&chat.device_id, now);
            let branch = crate::change_requests::conversation_branch(chat, &state.spaces)
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
                .map(|branch| SharedString::from(branch.to_owned()));
            let change_request = state
                .change_request_for_chat(chat)
                .map(ChangeRequestBadgeModel::from_summary);
            let title = SharedString::from(
                chat.title
                    .as_deref()
                    .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| "New session".to_owned()),
            );
            (
                title,
                project,
                local,
                online,
                branch,
                change_request,
                at,
            )
        };

        let muted = theme.text_muted;
        let meta_row = |glyph: AnyElement, label: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.0))
                .min_w_0()
                .h(px(18.0))
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(glyph),
                )
                .child(div().flex_1().min_w_0().truncate().child(label))
        };
        let glyph = |path: &'static str| {
            icon(path)
                .size(px(13.0))
                .text_color(muted.opacity(0.8))
                .into_any_element()
        };

        // ── The title and its age on one quiet line, the device's glyph in
        // the corner (this one, another, or out of reach).
        let device = icon(if !online {
            icons::WIFI_OFF
        } else if local {
            icons::LAPTOP
        } else {
            icons::REMOTE_SERVER
        })
        .size(px(13.0))
        .text_color(if online {
            muted.opacity(0.8)
        } else {
            theme.warning.opacity(0.8)
        });
        let header = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .w_full()
                            .text_size(crate::typography::ui_rems(13.0))
                            .line_height(px(18.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .line_clamp(3)
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(muted)
                            // slopty: how long it has stood as it is, said in full.
                            .child(
                                facts
                                    .status
                                    .clone()
                                    .unwrap_or_else(|| time_label(&time_ago, &at).into()),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(device),
            );

        // ── Where it runs: project and branch.
        let project_row = meta_row(
            match &project {
                Some(_) => self.render_project_icon(chat_id, 14.0, false, cx),
                None => glyph(icons::FOLDER),
            },
            // slopty: its machine beside its project ("project @ machine").
            facts.place.clone().or_else(|| project.clone()).unwrap_or_else(|| "No project".into()),
        )
        .text_color(theme.text);
        // slopty: a chat with no project says which folder it works in.
        let folder = project
            .is_none()
            .then(|| facts.folder.clone())
            .flatten()
            .map(|folder| meta_row(glyph(icons::FOLDER), folder));
        // slopty: the agent and model, and the subagents at work, summed up: they live in the
        // chat, not in the sidebar.
        let agent = facts.agent.clone().map(|agent| meta_row(glyph(icons::BOT), agent));
        let subagents =
            facts.subagents.clone().map(|subagents| meta_row(glyph(icons::LIST), subagents));
        // ── The pull request as one more row under the branch: its state
        // lives in the glyph's and number's color (the badge's).
        let pull_request = change_request.map(|model| {
            let tone = model.tone.color(&theme);
            meta_row(
                icon(icons::PULL_REQUEST)
                    .size(px(13.0))
                    .text_color(tone.opacity(0.9))
                    .into_any_element(),
                model.title,
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.font_mono.clone())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(tone.opacity(0.85))
                    .child(SharedString::from(format!("#{}", model.number))),
            )
        });
        let location = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(muted)
            .child(project_row)
            .children(folder)
            .children(branch.clone().map(|branch| meta_row(glyph(icons::GIT_BRANCH), branch)))
            .children(pull_request)
            .children(agent)
            .children(subagents);
        // slopty: what the agent wants of the person, in its records' words, beside the glyph its
        // row wears for it (`marks`): the row has room for the glyph alone.
        let need = facts.need.clone().map(|need| {
            let words = need.words.clone();
            let mark = super::marks::Mark::NeedsYou(need);
            div()
                .debug_selector({
                    let chat_id = chat_id.to_owned();
                    move || format!("session-hover-card-need-{chat_id}")
                })
                .flex()
                .flex_row()
                .items_start()
                .gap(px(7.0))
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .children(super::marks::mark_element(
                            SharedString::from(format!("session-hover-card-mark-{chat_id}")),
                            format!("session-hover-card-mark-{chat_id}"),
                            &mark,
                            crate::loaders::status_mark::MARK_SMALL,
                            popover::surface_bg(&theme),
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(px(16.0))
                        .text_color(theme.text)
                        .line_clamp(3)
                        .child(words),
                )
        });
        // slopty: what a screen reader says of the card, a line per fact.
        let need_words = facts.need.as_ref().map(|need| need.words.clone());
        let spoken = [
            &facts.status,
            &need_words,
            &facts.place,
            &facts.folder,
            &branch,
            &facts.agent,
            &facts.subagents,
        ]
        .into_iter()
        .flatten()
        .map(SharedString::as_ref)
        .collect::<Vec<&str>>()
        .join("\n");

        let card = div()
            .id(SharedString::from(format!("session-hover-card-body-{chat_id}")))
            .role(gpui::Role::Tooltip) // slopty: read as the row's tooltip
            .aria_label(title.clone())
            .aria_value(spoken)
            .w(px(CARD_WIDTH))
            .p(px(CARD_PAD))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .rounded(px(popover::CARD_RADIUS))
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(popover::surface_bg(&theme))
            .when(!theme.is_frost(), |card| card.shadow_lg())
            .child(header)
            .children(need)
            .child(location);
        let card = div()
            .debug_selector({
                let chat_id = chat_id.to_owned();
                move || format!("session-hover-card-{chat_id}")
            })
            .child(crate::frost::frosted(
                popover::CARD_RADIUS,
                crate::frost::MENU_BLUR,
                card,
            ));
        // Drifts in from the row's side, like a submenu leaving its trigger.
        let card = if self.reduced_motion.get() {
            card.into_any_element()
        } else {
            card.with_animation(
                SharedString::from(format!("session-hover-card-{chat_id}")),
                motion::MENU_IN.animation(),
                |el, t| el.relative().opacity(t).left(px(-4.0 * (1.0 - t))),
            )
            .into_any_element()
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_full()
                .child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(div().pl(px(CARD_GAP)).mt(px(-4.0)).child(card)),
                    )
                    .with_priority(1),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    fn test_chat(id: &str) -> slopty_view_proto::Chat {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": id, "deviceId": "local", "archived": false,
            "createdAt": Utc::now(),
        }))
        .unwrap()
    }

    fn test_shell(data_dir: &std::path::Path, cx: &mut Context<Shell>) -> Shell {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.workspace_scope = Some(WorkspaceScope::Development);
            state.connection = ConnectionStatus::Ready;
            state.chats = vec![test_chat("hover-test"), test_chat("next")];
            state
        });
        let mut shell = Shell::new(
            state,
            EngineBootConfig {
                data_dir: data_dir.into(),
                // slopty: Slopty's server is the only backend.
                default_harness: slopty_view_proto::HarnessId::MOCK,
            },
            cx,
        );
        shell.debug_gate = Some(GatePhase::Ready);
        shell
    }

    fn init_test(cx: &mut TestAppContext, dir: &std::path::Path) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx); // slopty: no History surface
            settings::init(settings::UiSettings::default(), dir, cx);
            motion::set_reduced_motion(cx, true);
        });
    }

    struct RowHost {
        shell: Entity<Shell>,
        palette: bool,
        _observation: Subscription,
        _dir: tempfile::TempDir,
    }

    impl Render for RowHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let palette = self.palette;
            self.shell.update(cx, |shell, cx| {
                div().w(px(280.0)).h(px(200.0)).child(shell.render_chat_row(
                    "hover-test".into(),
                    "Hover test".into(),
                    None,
                    "now".into(),
                    "Project".into(),
                    None,
                    Some(slopty_view_proto::ChangeRequestSummary {
                        provider: "github".into(),
                        number: 90,
                        title: "Pull request title".into(),
                        url: "https://github.com/acme/slopty/pull/90".into(),
                        state: slopty_view_proto::ChangeRequestState::Open,
                        base_ref: "main".into(),
                        head_ref: "feature/pr".into(),
                    }),
                    None,
                    slopty_view_proto::ChatIndicator::Idle,
                    false,
                    false,
                    None,
                    palette.then_some(""),
                    &Theme::default(),
                    cx,
                ))
            })
        }
    }

    fn row_setup(
        cx: &mut TestAppContext,
        palette: bool,
    ) -> (Entity<Shell>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        init_test(cx, dir.path());
        let (host, cx) = cx.add_window_view(|_, cx| {
            let shell = cx.new(|cx| test_shell(dir.path(), cx));
            let observation = cx.observe(&shell, |_, _, cx| cx.notify());
            RowHost {
                shell,
                palette,
                _observation: observation,
                _dir: dir,
            }
        });
        let shell = host.read_with(cx, |host, _| host.shell.clone());
        redraw(cx);
        (shell, cx)
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        // Cached frames omit debug selectors; refresh before inspecting
        // either the row's bounds or the absence of a floating card.
        cx.refresh().unwrap();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn wait_for_card(cx: &mut VisualTestContext) {
        cx.executor()
            .advance_clock(DWELL + Duration::from_millis(1));
        redraw(cx);
    }

    #[gpui::test]
    fn archive_press_cancels_pending_and_shown_cards(cx: &mut TestAppContext) {
        for shown in [false, true] {
            let (shell, cx) = row_setup(cx, false);
            let row = cx.debug_bounds("chat-hover-test").unwrap().center();
            cx.simulate_mouse_move(row, None, gpui::Modifiers::default());
            redraw(cx);
            if shown {
                wait_for_card(cx);
            }
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.session_hover_card.target(), Some("hover-test"));
                assert_eq!(shell.session_hover_card.shown().is_some(), shown);
            });
            let position = cx.debug_bounds("chat-time-hover-test").unwrap().center();
            cx.simulate_mouse_down(position, MouseButton::Left, gpui::Modifiers::default());
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.session_hover_card.target(), None)
            });
            wait_for_card(cx);
            assert!(cx.debug_bounds("session-hover-card-hover-test").is_none());
        }
    }

    #[gpui::test]
    fn palette_pr_badge_keeps_its_tooltip_without_a_session_card(cx: &mut TestAppContext) {
        let (shell, cx) = row_setup(cx, true);
        let badge = cx
            .debug_bounds("pull-request-badge-palette-chat-hover-test-pr")
            .unwrap()
            .center();
        cx.simulate_mouse_move(badge, None, gpui::Modifiers::default());
        redraw(cx);
        wait_for_card(cx);
        assert!(cx.debug_bounds("change-request-tooltip").is_some());
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.session_hover_card.target(), None)
        });
    }

    fn shell_setup(
        cx: &mut TestAppContext,
    ) -> (Entity<Shell>, &mut VisualTestContext, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        init_test(cx, dir.path());
        let (shell, cx) = cx.add_window_view(|_, cx| test_shell(dir.path(), cx));
        cx.simulate_resize(gpui::size(px(1400.0), px(800.0)));
        redraw(cx);
        (shell, cx, dir)
    }

    #[gpui::test]
    fn removing_a_pending_row_cancels_its_dwell_and_keeps_the_next_card_cold(
        cx: &mut TestAppContext,
    ) {
        let (shell, cx, _dir) = shell_setup(cx);
        let row = cx.debug_bounds("chat-hover-test").unwrap().center();
        cx.simulate_mouse_move(row, None, gpui::Modifiers::default());
        redraw(cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.session_hover_card.target(), Some("hover-test"))
        });
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.chats.retain(|chat| chat.id != "hover-test");
                cx.notify();
            });
            cx.notify();
        });
        redraw(cx);
        let next = cx.debug_bounds("chat-next").unwrap().center();
        wait_for_card(cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.session_hover_card.target(), None)
        });
        // Leave the old hit region before entering the surviving row.
        cx.simulate_mouse_move(
            gpui::point(px(600.0), px(700.0)),
            None,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_move(next, None, gpui::Modifiers::default());
        redraw(cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.session_hover_card.target(), Some("next"));
            assert_eq!(shell.session_hover_card.shown(), None);
        });
    }

    #[gpui::test]
    fn settings_navigation_cancels_pending_and_shown_cards(cx: &mut TestAppContext) {
        for shown in [false, true] {
            let (shell, cx, _dir) = shell_setup(cx);
            let row = cx.debug_bounds("chat-hover-test").unwrap().center();
            cx.simulate_mouse_move(row, None, gpui::Modifiers::default());
            redraw(cx);
            if shown {
                wait_for_card(cx);
            }
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.session_hover_card.shown().is_some(), shown)
            });
            shell.update(cx, |shell, cx| {
                shell.open_settings(SettingsSection::Appearance, cx)
            });
            redraw(cx);
            wait_for_card(cx);
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.session_hover_card.target(), None)
            });
            shell.update(cx, |shell, cx| shell.close_settings(cx));
            redraw(cx);
            assert!(cx.debug_bounds("session-hover-card-hover-test").is_none());
        }
    }

    fn card() -> SessionHoverCard {
        SessionHoverCard::default()
    }

    #[test]
    fn a_cold_card_waits_out_the_dwell() {
        let mut card = card();
        let now = Instant::now();
        assert!(!card.enter("a", now));
        assert_eq!(card.shown(), None);
        // The dwell only settles a row the pointer is still on.
        assert!(!card.settle("a"));
        card.pending = Some(("a".into(), Task::ready(())));
        assert!(card.settle("a"));
        assert_eq!(card.shown(), Some("a"));
    }

    #[test]
    fn leaving_before_the_dwell_cancels_it() {
        let mut card = card();
        card.pending = Some(("a".into(), Task::ready(())));
        assert!(!card.leave("a", Instant::now()));
        assert!(!card.settle("a"));
        assert_eq!(card.shown(), None);
    }

    #[test]
    fn a_warm_card_follows_the_pointer_in_either_event_order() {
        let now = Instant::now();
        // Leave fires first: the card hid a moment ago, so it is still warm.
        let mut card = card();
        card.shown = Some("a".into());
        assert!(card.leave("a", now));
        assert!(card.enter("b", now + Duration::from_millis(16)));
        assert_eq!(card.shown(), Some("b"));
        // Enter fires first: the stale leave must not hide the new card.
        let mut card = SessionHoverCard {
            shown: Some("a".into()),
            ..Default::default()
        };
        assert!(card.enter("b", now));
        assert!(!card.leave("a", now));
        assert_eq!(card.shown(), Some("b"));
    }

    #[test]
    fn the_card_cools_after_a_rest_or_a_dismissal() {
        let now = Instant::now();
        let mut card = card();
        card.shown = Some("a".into());
        card.leave("a", now);
        assert!(!card.enter("b", now + WARM + Duration::from_millis(1)));
        let mut card = SessionHoverCard {
            shown: Some("a".into()),
            ..Default::default()
        };
        assert!(card.dismiss());
        assert!(!card.enter("a", now));
        assert_eq!(card.shown(), None);
    }

    #[test]
    fn the_age_reads_as_a_sentence() {
        assert_eq!(time_label("21h", "Oct 18, 09:12"), "21h ago \u{b7} Oct 18, 09:12");
        assert_eq!(time_label("now", "Oct 19, 06:04"), "just now \u{b7} Oct 19, 06:04");
    }
}
