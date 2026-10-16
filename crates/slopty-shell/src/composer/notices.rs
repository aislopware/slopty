// slopty: Slopty's own file, beside zeron's composer notices.
//! What the composer says above itself about the chat's road: its machine out of reach, named
//! and with when it was last heard (after a grace, so a blip says nothing), with Retry and Wake;
//! and a usage limit holding the agent, with when it lifts.
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
        let now = Utc::now();
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
                let since = Instant::now();
                self.extras.degraded_since = Some((chat.clone(), since));
                self.extras.degraded_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(OFFLINE_GRACE).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                }));
                since
            }
        };
        if since.elapsed() < OFFLINE_GRACE {
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

    /// The limit notice's words while a usage limit holds the agent; redrawn when it lifts.
    pub(super) fn limit_notice(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let resets_at = self.state.read(cx).limit_resets_at;
        let now = Utc::now();
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
        self.extras.stopped = Some((chat, Instant::now()));
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
            && at.elapsed() < STOPPED_SHOWN)
            .then(|| "Stopped".to_owned())
    }

    /// Re-kick the chat's delivery, as the transcript's failed-send retry does.
    fn retry_road(&mut self, chat: String, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        self.state.update(cx, |state, cx| {
            state.retry_pending_send(&chat, Utc::now());
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

    /// One quiet caption line above the composer: a dot, the words, and its text actions.
    pub(super) fn render_road_notice(
        &self,
        notice: RoadNotice,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dot = if notice.offline { theme.warning } else { theme.text_faint };
        let action = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .cursor_pointer()
                .text_color(theme.text_muted)
                .hover(|s| s.text_color(theme.text))
                .child(SharedString::from(label))
        };
        let chat = notice.chat.clone();
        crate::motion::fade_in(
            "composer-road-notice",
            div()
                .id("composer-road-notice")
                .role(Role::Status)
                .aria_label(SharedString::from(notice.text.clone()))
                .mx(px(8.0))
                .mt(px(6.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .line_height(px(14.0))
                .text_color(theme.text_faint)
                .child(div().flex_none().size(px(5.0)).rounded_full().bg(dot))
                .child(div().min_w_0().truncate().child(SharedString::from(notice.text)))
                .when(!notice.offline, |el| {
                    el.child(action("composer-road-retry", "Retry").on_click(cx.listener(
                        move |this, _, _, cx| this.retry_road(chat.clone(), cx),
                    )))
                })
                .when_some(notice.wake, |el, (device, wake)| {
                    el.child(action("composer-road-wake", "Wake").on_click(cx.listener(
                        move |this, _, _, cx| this.wake_machine(device.clone(), wake.clone(), cx),
                    )))
                }),
        )
        .into_any_element()
    }

    /// The limit notice: an amber dot and when the limit lifts.
    pub(super) fn render_limit_notice(&self, words: String, theme: &Theme) -> AnyElement {
        crate::motion::fade_in(
            "composer-limit-notice",
            div()
                .id("composer-limit-notice")
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .mx(px(8.0))
                .mt(px(6.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .line_height(px(14.0))
                .text_color(theme.text_faint)
                .child(div().flex_none().size(px(5.0)).rounded_full().bg(theme.warning))
                .child(div().min_w_0().truncate().child(SharedString::from(words))),
        )
        .into_any_element()
    }
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
}
