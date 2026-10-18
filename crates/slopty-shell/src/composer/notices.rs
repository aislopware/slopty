// slopty: Slopty's own file, beside zeron's composer notices.
//! What the composer says above itself about the chat's road: its machine out of reach, named
//! and with when it was last heard (after a grace, so a blip says nothing), with Retry and Wake;
//! a turn that made no progress for a while, on which machine and after which call, with Keep
//! waiting and Stop; and a usage limit holding the agent, with when it lifts.
use chrono::{DateTime, Utc};
use gpui::AnyElement;

use super::*;

/// How long a chat's road stays degraded before the composer says so.
pub(super) const OFFLINE_GRACE: Duration = Duration::from_secs(3);

/// How long "Stopped" stays once a stop is confirmed.
pub(super) const STOPPED_SHOWN: Duration = Duration::from_millis(2500);

/// The offline notice of a chat.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RoadNotice {
    pub(super) text: String,
    /// This Mac itself is offline (amber), not the chat's machine.
    pub(super) offline: bool,
    pub(super) chat: String,
    /// The machine, and its wake action when it lists one.
    pub(super) wake: Option<(String, String)>,
}

/// What a chat's session waits on while its turn makes no progress (the worker's `stalled`).
pub(super) const STALLED: &str = "stalled";

/// How the worker begins a stall's words, before the call it last made.
const STALLED_TEXT: &str = "No progress for a while";

/// The stall notice of a chat.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StallNotice {
    pub(super) text: String,
    pub(super) chat: String,
    /// What the worker said of it, which keeping waiting is keyed by.
    pub(super) said: Option<String>,
}

/// The words of a stall on the machine `host`, from what the worker said of it (`said`): "No
/// progress on studio for a while · last: Bash(cargo test)".
pub(super) fn stall_words(host: &str, said: Option<&str>) -> String {
    match said.map(str::trim).filter(|said| !said.is_empty()) {
        Some(said) => match said.strip_prefix(STALLED_TEXT) {
            Some(rest) => format!("No progress on {host} for a while{rest}"),
            None => format!("{host}: {said}"),
        },
        None => format!("No progress on {host} for a while"),
    }
}

/// When a machine was last heard, in words: "just now", "2 min ago", "3 h ago", "2 d ago".
pub(super) fn last_heard(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(at).num_seconds().max(0);
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86_400 {
        format!("{} h ago", secs / 3600)
    } else {
        format!("{} d ago", secs / 86_400)
    }
}

/// The words of a chat's offline notice: this Mac offline, or the chat's machine `name` out of
/// reach since `last_seen`.
pub(super) fn road_words(
    this_mac_offline: bool,
    name: &str,
    last_seen: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    if this_mac_offline {
        return "Offline — messages will send when you're back online.".to_owned();
    }
    match last_seen {
        Some(at) => format!("{name} is out of reach · last heard {}", last_heard(at, now)),
        None => format!("{name} is out of reach · messages will send once it's back"),
    }
}

/// The words of the limit notice while `resets_at` is ahead.
pub(super) fn limit_words(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    let at = resets_at.filter(|at| *at > now)?;
    let reset = crate::account_usage::format_reset(Some(at), now)?;
    Some(format!("Limit reached · {reset}"))
}

impl Composer {
    /// The selected chat's offline notice, once its road has been degraded for
    /// [`OFFLINE_GRACE`].
    pub(super) fn road_notice(&mut self, cx: &mut Context<Self>) -> Option<RoadNotice> {
        let now = crate::clock::utc(cx);
        let (chat, degraded, offline, device) = {
            let state = self.state.read(cx);
            let chat = state.selected_chat.clone()?;
            let degraded = state.chat_delivery_degraded(&chat);
            let offline =
                state.connectivity.state == slopty_view_proto::ConnectivityState::Offline;
            let device = state
                .chats
                .iter()
                .find(|c| c.id == chat)
                .and_then(|c| state.devices.iter().find(|d| d.id == c.device_id))
                .cloned();
            (chat, degraded, offline, device)
        };
        if !degraded {
            self.extras.degraded_since = None;
            self.extras.degraded_task = None;
            return None;
        }
        let since = match &self.extras.degraded_since {
            Some((at_chat, since)) if *at_chat == chat => *since,
            _ => {
                let since = crate::clock::now(cx);
                self.extras.degraded_since = Some((chat.clone(), since));
                self.extras.degraded_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(OFFLINE_GRACE).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                }));
                since
            }
        };
        if crate::clock::now(cx).saturating_duration_since(since) < OFFLINE_GRACE {
            return None;
        }
        let name = device.as_ref().map_or("The chat's machine", |d| d.name.as_str());
        let text = road_words(offline, name, device.as_ref().and_then(|d| d.last_seen_at), now);
        let wake = device.as_ref().filter(|_| !offline).and_then(|d| {
            d.actions
                .iter()
                .find(|a| a.id.eq_ignore_ascii_case("wake") || a.label.eq_ignore_ascii_case("wake"))
                .map(|a| (d.id.clone(), a.id.clone()))
        });
        Some(RoadNotice { text, offline, chat, wake })
    }

    /// The stall notice: the selected chat's turn made no progress for a while, unless the
    /// person chose to keep waiting through this very stall.
    pub(super) fn stall_notice(&mut self, cx: &App) -> Option<StallNotice> {
        let (chat, said, host) = {
            let state = self.state.read(cx);
            let chat = state.selected_chat.clone()?;
            let now = crate::clock::utc(cx);
            let wait = state
                .session_for(&chat)
                .filter(|s| {
                    now.signed_duration_since(s.updated_at).num_milliseconds()
                        <= crate::state::SESSION_STALE_MS
                })
                .and_then(|s| s.waiting_on.as_ref().map(|wait| (s, wait)))
                .filter(|(_, wait)| wait.kind == STALLED);
            let Some((session, wait)) = wait else {
                // The stall is over: a new one is said again.
                if self.extras.stall_kept.as_ref().is_some_and(|(kept, _)| *kept == chat) {
                    self.extras.stall_kept = None;
                }
                return None;
            };
            let host = state
                .devices
                .iter()
                .find(|d| d.id == session.device_id)
                .map_or_else(|| "its machine".to_owned(), |d| d.name.clone());
            (chat, wait.summary.clone(), host)
        };
        if self.extras.stall_kept.as_ref().is_some_and(|kept| kept.0 == chat && kept.1 == said) {
            return None;
        }
        Some(StallNotice { text: stall_words(&host, said.as_deref()), chat, said })
    }

    /// Keep waiting through `notice`'s stall: the composer stops saying it until it ends.
    pub(super) fn keep_waiting(&mut self, notice: &StallNotice, cx: &mut Context<Self>) {
        self.extras.stall_kept = Some((notice.chat.clone(), notice.said.clone()));
        cx.notify();
    }

    /// The limit notice's words while a usage limit holds the agent; redrawn when it lifts.
    pub(super) fn limit_notice(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let resets_at = self.state.read(cx).limit_resets_at;
        let now = crate::clock::utc(cx);
        let words = limit_words(resets_at, now)?;
        let at = resets_at?;
        if self.extras.limit_task.as_ref().is_none_or(|(when, _)| *when != at) {
            let wait = (at - now).to_std().unwrap_or_default() + Duration::from_millis(50);
            let task = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            });
            self.extras.limit_task = Some((at, task));
        }
        Some(words)
    }

    /// Whether the selected chat's stop was asked and not yet confirmed.
    pub(super) fn stopping(&self, cx: &App) -> bool {
        self.state.read(cx).selected_chat.as_deref().is_some_and(|chat| self.is_interrupting(chat))
    }

    /// A stop the run came to rest under: say "Stopped" for [`STOPPED_SHOWN`].
    pub(super) fn note_stopped(&mut self, chat: String, cx: &mut Context<Self>) {
        self.extras.stopped = Some((chat, crate::clock::now(cx)));
        self.extras.stopped_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STOPPED_SHOWN).await;
            this.update(cx, |composer, cx| {
                composer.extras.stopped = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// "Stopped", while a confirmed stop of the selected chat is fresh.
    pub(super) fn stopped_notice(&self, cx: &App) -> Option<String> {
        let (chat, at) = self.extras.stopped.as_ref()?;
        (self.state.read(cx).selected_chat.as_deref() == Some(chat.as_str())
            && crate::clock::now(cx).saturating_duration_since(*at) < STOPPED_SHOWN)
            .then(|| "Stopped".to_owned())
    }

    /// Re-kick the chat's delivery, as the transcript's failed-send retry does.
    fn retry_road(&mut self, chat: String, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        self.state.update(cx, |state, cx| {
            state.retry_pending_send(&chat, crate::clock::utc(cx));
            cx.notify();
        });
        let Some(engine) = engine else { return };
        self.extras.notice_task = Some(cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "chatId": chat });
            if let Err(err) = engine.client().call(methods::RETRY_DELIVERY, params).await {
                tracing::warn!(error = %err, "delivery retry RPC failed");
            }
        }));
    }

    /// Wake the chat's machine, as Settings → Devices does.
    fn wake_machine(&mut self, device: String, action: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.extras.notice_task = Some(cx.spawn(async move |this, cx| {
            let params = serde_json::json!({ "deviceId": device, "action": action });
            let result = engine.client().call(methods::DEVICE_ACTION, params).await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Couldn't wake the machine: {err}").into());
                    composer.failure_key = None;
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The new-chat canvas's word on its target: a send will wait for the connection.
    pub(super) fn queue_notice(&self, cx: &App) -> Option<(SharedString, bool)> {
        use slopty_view_proto::ConnectivityState as S;
        let state = self.state.read(cx);
        if state.selected_chat.is_some() {
            return None;
        }
        // New-chat canvas: judge by the picked target device.
        let remote_target = state
            .effective_device_id()
            .is_some_and(|id| state.local_device_id.as_deref() != Some(id.as_str()));
        let degraded = remote_target
            && (matches!(state.connectivity.state, S::Offline | S::Reconnecting)
                || state
                    .effective_device_id()
                    .is_some_and(|id| !state.device_online(&id, crate::clock::utc(cx))));
        let offline = state.connectivity.state == S::Offline;
        degraded.then(|| {
            let text: SharedString = if offline {
                "Offline — messages will send when you're back online.".into()
            } else {
                "Messages will send once the connection recovers.".into()
            };
            (text, offline)
        })
    }

    /// slopty: the one line the composer says about its chat, drawn in the shell's reserved
    /// status strip under the composer (`Theme::STATUS_STRIP_HEIGHT`), so a notice arriving or
    /// leaving never moves the composer or the transcript. One at a time, the most pressing
    /// first: the road out of reach, a usage limit, the new chat's queued send, a fresh stop.
    /// Each enters with a quick fade keyed by its kind, so a change of notice cross-fades.
    pub fn status_line(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        if let Some(notice) = self.road_notice(cx) {
            return Some(self.render_road_notice(notice, &theme, cx));
        }
        if let Some(notice) = self.stall_notice(cx) {
            return Some(self.render_stall_notice(notice, &theme, cx));
        }
        if let Some(words) = self.limit_notice(cx) {
            let mark = status_dot(theme.warning);
            let id = "composer-limit-notice";
            return Some(status_in(id, status_words(id, mark, words.into(), &theme)));
        }
        if let Some((text, offline)) = self.queue_notice(cx) {
            let mark = if offline { offline_mark(&theme) } else { status_dot(theme.text_faint) };
            let id = "composer-queue-notice";
            return Some(status_in(id, status_words(id, mark, text, &theme)));
        }
        // A stop the person asked for is no warning: a faint dot.
        let words = self.stopped_notice(cx)?;
        let mark = status_dot(theme.text_faint);
        let id = "composer-stopped-notice";
        Some(status_in(id, status_words(id, mark, words.into(), &theme)))
    }

    /// The stall notice: its mark, the words, Keep waiting, and Stop while no stop is asked.
    fn render_stall_notice(
        &self,
        notice: StallNotice,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stopping = self.is_interrupting(&notice.chat);
        let id = "composer-stall-notice";
        let kept = notice.clone();
        let chat = notice.chat.clone();
        let line = status_words(id, status_dot(theme.warning), notice.text.into(), theme)
            .child(notice_action("composer-stall-keep", "Keep waiting", theme).on_click(cx.listener(
                move |this, _, _, cx| this.keep_waiting(&kept, cx),
            )))
            .when(!stopping, |el| {
                el.child(notice_action("composer-stall-stop", "Stop", theme).on_click(cx.listener(
                    move |this, _, _, cx| this.interrupt_chat(chat.clone(), cx),
                )))
            });
        status_in(id, line)
    }

    /// The road notice: its mark, the words, and its text actions.
    fn render_road_notice(
        &self,
        notice: RoadNotice,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mark = if notice.offline { offline_mark(theme) } else { status_dot(theme.text_faint) };
        let action = |id: &'static str, label: &'static str| notice_action(id, label, theme);
        let chat = notice.chat.clone();
        let id = "composer-road-notice";
        let line = status_words(id, mark, notice.text.into(), theme)
            .when(!notice.offline, |el| {
                el.child(action("composer-road-retry", "Try again").on_click(cx.listener(
                    move |this, _, _, cx| this.retry_road(chat.clone(), cx),
                )))
            })
            .when_some(notice.wake, |el, (device, wake)| {
                el.child(action("composer-road-wake", "Wake").on_click(cx.listener(
                    move |this, _, _, cx| this.wake_machine(device.clone(), wake.clone(), cx),
                )))
            });
        status_in(id, line)
    }
}

/// A status line's text action: a keyboard stop with the shared ring, as a pane's control is.
fn notice_action(id: &'static str, label: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let fade: SharedString = format!("{id}-hover").into();
    let action = div().id(id);
    crate::surface_chrome::key_stop(action, theme)
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .cursor_pointer()
        .text_color(crate::motion::hover_blend(&fade, theme.text_muted, theme.text))
        .hover_fade(fade)
        .child(SharedString::from(label))
}

/// A status line's dot.
fn status_dot(color: gpui::Hsla) -> AnyElement {
    div().flex_none().size(px(5.0)).rounded_full().bg(color).into_any_element()
}

/// This Mac offline: the offline glyph, quiet (amber is for a send that is stuck).
fn offline_mark(theme: &Theme) -> AnyElement {
    crate::icons::icon(crate::icons::WIFI_OFF)
        .flex_none()
        .size(px(12.0))
        .text_color(theme.text_muted)
        .into_any_element()
}

/// A status line entering with a quick fade, keyed by its kind.
fn status_in(id: &'static str, line: gpui::Stateful<gpui::Div>) -> AnyElement {
    crate::motion::fade_quick(SharedString::from(format!("{id}-in")), line).into_any_element()
}

/// One status line: its mark and words on the strip's single row.
fn status_words(
    id: &'static str,
    mark: AnyElement,
    words: SharedString,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Status)
        .aria_label(words.clone())
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_faint)
        .child(mark)
        .child(div().min_w_0().truncate().child(words))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_machine_and_when_it_was_last_heard() {
        let now = Utc::now();
        let ago = |secs| Some(now - chrono::TimeDelta::seconds(secs));
        assert_eq!(
            road_words(false, "studio", ago(130), now),
            "studio is out of reach · last heard 2 min ago"
        );
        assert_eq!(road_words(false, "studio", ago(5), now), "studio is out of reach · last heard just now");
        assert_eq!(road_words(false, "vm", ago(7300), now), "vm is out of reach · last heard 2 h ago");
        assert!(road_words(false, "vm", None, now).starts_with("vm is out of reach"));
        assert!(road_words(true, "vm", ago(5), now).starts_with("Offline"));
    }

    #[test]
    fn the_limit_notice_says_when_it_lifts_and_goes_once_it_has() {
        let now = Utc::now();
        let words = limit_words(Some(now + chrono::TimeDelta::minutes(40)), now).unwrap();
        assert!(words.starts_with("Limit reached · resets "), "{words}");
        assert_eq!(limit_words(Some(now - chrono::TimeDelta::minutes(1)), now), None);
        assert_eq!(limit_words(None, now), None);
    }

    struct Line {
        pressed: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl gpui::Render for Line {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            let theme = Theme::of(cx).clone();
            let pressed = self.pressed.clone();
            div()
                .flex()
                .items_start()
                .p(px(20.0))
                .child(
                    notice_action("composer-stall-keep", "Keep waiting", &theme)
                        .on_click(move |_, _, _| pressed.set(pressed.get() + 1)),
                )
                .child(crate::context_usage::chip(None, false, &theme))
        }
    }

    /// slopty: a status line's actions and the context meter are keyboard stops: Tab reaches
    /// each in turn, and Enter presses the action it is on.
    #[gpui::test]
    fn the_notice_actions_and_the_meter_take_the_keyboard(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let pressed = std::rc::Rc::new(std::cell::Cell::new(0));
        let line = Line { pressed: pressed.clone() };
        let (_, cx) = cx.add_window_view(|_, _| line);
        let focused = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.refresh();
                window.draw(cx).clear(cx);
                let tree = window.a11y_tree()?;
                tree.nodes
                    .iter()
                    .find(|(id, _)| *id == tree.focus)
                    .and_then(|(_, node)| node.label().map(str::to_owned))
            })
        };
        cx.update(|window, cx| window.focus_next(cx));
        assert_eq!(focused(cx).as_deref(), Some("Keep waiting"));
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("enter").unwrap() });
        cx.run_until_parked();
        assert_eq!(pressed.get(), 1, "Enter presses it");
        cx.update(|window, cx| window.focus_next(cx));
        assert_eq!(focused(cx).as_deref(), Some("Context —"));
    }

    /// slopty: on a touch screen the context meter takes a finger's 44 pt press, drawn 24 tall
    /// as on a Mac (#23).
    #[gpui::test]
    fn the_meter_takes_a_fingers_press(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let line = Line { pressed: std::rc::Rc::default() };
        let (_, cx) = cx.add_window_view(|_, _| line);
        let meter = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.refresh();
                window.draw(cx).clear(cx);
                let scale = f64::from(window.scale_factor());
                let tree = window.a11y_tree().expect("a tree");
                let rect = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some("Context —"))
                    .and_then(|(_, node)| node.bounds())
                    .expect("the meter");
                (((rect.x1 - rect.x0) / scale) as f32, ((rect.y1 - rect.y0) / scale) as f32)
            })
        };
        crate::shell::force_touch(true);
        let (w, h) = meter(cx);
        assert!(w.min(h) >= 43.9, "under a finger's 44 pt: {w}x{h}");
        crate::shell::force_touch(false);
        let (_, h) = meter(cx);
        assert_eq!(h.round(), 24.0, "as drawn on a Mac");
    }
}
