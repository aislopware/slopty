//! The phone's side of pushed notes (`docs/decisions/platform.md`, "Notes reach a pocketed
//! phone"): what it tells the server it may push to, and when it stops hearing notices on its
//! link.
//!
//! On every link, whenever APNs hands the app a new token, and on each move to or from the
//! front, the phone says who it is, its token, the public half of its key, and how long a turn
//! must run before its end is worth a note ([`device`]). While notes don't reach the person, it
//! takes that back instead: a pushed note would not show either. The moment it leaves the
//! front it says it no longer listens, so a pocketed phone gets its notes by push alone: one
//! path, which the server takes back when the person answers elsewhere. The background grace
//! still keeps the links up a while, for the transfers under way.
//!
//! Nothing on iOS asks for notes by itself until the first note is posted, and that happens
//! only away from the front, so a new phone would never be pushed to. Once its server link is
//! up, and on each return to the front, the phone reads whether notes reach the person and its
//! navigator says so while they do not (`Workspace::look_at_notes`): never asked, its line
//! asks, and the answer goes to the server at once; turned off, it opens the system's settings.

#[cfg(target_os = "ios")]
use std::rc::Rc;
use std::time::Duration;

#[cfg(target_os = "ios")]
use gpui::Context;
use slopty_platform::notify::Alerts;
use slopty_proto::push::PushDevice;

#[cfg(target_os = "ios")]
use crate::Workspace;

/// The app's bundle identifier, APNs' topic for its pushes.
pub(crate) const TOPIC: &str = "dev.aislopware.slopty";

/// What the phone tells the server to push to, by its `token` and the public half of its `key`:
/// nothing until APNs gave it a token, or while notes don't reach the person (`alerts`). A turn
/// shorter than `quiet` ends with no push, as it would with no note here.
pub(crate) fn device(
    token: Option<&str>,
    alerts: Alerts,
    key: [u8; 32],
    quiet: Duration,
) -> Option<PushDevice> {
    let token = token.filter(|t| !t.is_empty())?;
    (alerts == Alerts::Allowed).then(|| PushDevice {
        token: token.to_owned(),
        key,
        sandbox: cfg!(debug_assertions),
        topic: TOPIC.to_owned(),
        quiet_ms: u64::try_from(quiet.as_millis()).unwrap_or(u64::MAX),
    })
}

/// The phone's push state, kept by the app.
#[cfg(target_os = "ios")]
#[derive(Debug, Default)]
pub(crate) struct Pushing {
    /// This installation's id and the public half of its key, read once both could be.
    me: Option<(slopty_core::ClientId, [u8; 32])>,
    /// The system's question about notes is up: a second press waits for its answer.
    asking: bool,
}

#[cfg(target_os = "ios")]
impl Pushing {
    /// This installation's id and the public half of its key: made on first use and kept, so
    /// read from disk and the Keychain once. `None`, said in the log, when either can't be.
    fn me(&mut self) -> Option<(slopty_core::ClientId, [u8; 32])> {
        if self.me.is_none() {
            let client = crate::net::client_id()
                .inspect_err(|e| tracing::warn!(error = %e, "no client id for pushes"))
                .ok()?;
            let key = slopty_platform::notify::pushed::device_key()
                .inspect_err(|e| tracing::warn!(error = %e, "no key for pushes"))
                .ok()?;
            self.me = Some((client, key.public()));
        }
        self.me
    }
}

#[cfg(target_os = "ios")]
impl Workspace {
    /// Tell the server again whenever APNs hands the app a new token, for as long as it runs.
    pub(crate) fn watch_push_tokens(cx: &Context<Self>) {
        let mut tokens = slopty_platform::notify::pushed::tokens();
        cx.spawn(async move |this, cx| {
            while tokens.changed().await.is_ok() {
                if this.update(cx, Self::tell_phone).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Tell the server what it may push to, once the system says whether notes reach the
    /// person now: the person may have turned them on or off since. The link sends it again on
    /// every new link by itself.
    pub(crate) fn tell_phone(&mut self, cx: &mut Context<Self>) {
        let Some(caller) = self.server_caller() else { return };
        let Some((client, key)) = self.pushing.me() else { return };
        let quiet = self.view.read(cx).slow_command();
        cx.spawn(async move |_this, _cx| {
            let alerts = slopty_platform::notify::settings().await;
            let token = slopty_platform::notify::pushed::tokens().borrow().clone();
            caller.push_device(client, device(token.as_deref(), alerts, key, quiet));
        })
        .detach();
    }

    /// Read whether notes reach the person, and have the navigator say so while they do not:
    /// once the server link is up, and on each return to the front, since the person may have
    /// turned them on or off in Settings meanwhile.
    pub(crate) fn look_at_notes(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            let alerts = slopty_platform::notify::settings().await;
            let _gone = this.update(cx, |ws, cx| ws.show_notes(alerts, cx));
        })
        .detach();
    }

    /// The navigator's line for notes that stand at `alerts`, and its door.
    fn show_notes(&self, alerts: Alerts, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let door: crate::MenuRun = Rc::new(move |_window, cx| {
            let _gone = this.update(cx, |ws, cx| match alerts {
                Alerts::Unasked => ws.ask_phone_notes(cx),
                Alerts::Denied => slopty_platform::notify::open_settings(),
                Alerts::Allowed | Alerts::Unavailable => {}
            });
        });
        self.view.update(cx, |v, cx| v.set_notes(alerts, door, cx));
    }

    /// The navigator's "Allow": the system's question, once while it is up, then the answer
    /// to the navigator and the server, which may push to this phone from now on.
    fn ask_phone_notes(&mut self, cx: &Context<Self>) {
        if std::mem::replace(&mut self.pushing.asking, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let alerts = slopty_platform::notify::ask().await;
            let _gone = this.update(cx, |ws, cx| {
                ws.pushing.asking = false;
                ws.show_notes(alerts, cx);
                ws.tell_phone(cx);
            });
        })
        .detach();
    }

    /// Whether the app hears notices on its link: in front it does; away it does not, from the
    /// moment it leaves, so notes come by push alone and a note heard on the link in the grace
    /// never outlives its answer at another device. The presence the window sends next says
    /// so, and while it does not listen the server's pushes say what the app would have
    /// posted.
    pub(crate) fn set_listening(&mut self, listening: bool) {
        if listening != self.presenting.listening() {
            tracing::debug!(listening, "notices on the link");
        }
        self.presenting.set_listening(listening);
        self.attention.set_listening(listening);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];
    const QUIET: Duration = Duration::from_secs(30);

    #[test]
    fn a_phone_is_pushed_to_only_with_a_token_and_notes_allowed() {
        let allowed = device(Some("ab01"), Alerts::Allowed, KEY, QUIET);
        assert_eq!(
            allowed,
            Some(PushDevice {
                token: "ab01".to_owned(),
                key: KEY,
                sandbox: cfg!(debug_assertions),
                topic: TOPIC.to_owned(),
                quiet_ms: 30_000,
            })
        );
        assert_eq!(device(None, Alerts::Allowed, KEY, QUIET), None, "no token yet");
        assert_eq!(device(Some(""), Alerts::Allowed, KEY, QUIET), None, "an empty token");
        for alerts in [Alerts::Denied, Alerts::Unasked, Alerts::Unavailable] {
            assert_eq!(device(Some("ab01"), alerts, KEY, QUIET), None, "{alerts:?} withdraws");
        }
    }
}
