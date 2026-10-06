//! The phone's side of pushed notes (`docs/decisions/platform.md`, "Notes reach a pocketed
//! phone"): what it tells the server it may push to, and when it stops hearing notices on its
//! link.
//!
//! On every link, whenever APNs hands the app a new token, and on each move to or from the
//! front, the phone says who it is, its token, the public half of its key, and how long a turn
//! must run before its end is worth a note ([`device`]). While notes don't reach the person, it
//! takes that back instead: a pushed note would not show either. Just before the background
//! grace runs out, it says it no longer listens ([`until_deaf`]), so the server pushes what it
//! would have sent on the link, and the app posts none of it.

use std::time::Duration;

#[cfg(target_os = "ios")]
use gpui::Context;
use slopty_platform::notify::Alerts;
use slopty_proto::push::PushDevice;

#[cfg(target_os = "ios")]
use crate::Workspace;

/// How long before the background grace runs out the phone says it no longer listens: time
/// enough for the word to leave on the link before the system suspends the app.
pub(crate) const STOP_BEFORE: Duration = Duration::from_secs(5);

/// How often the grace is read while the system counts none of it yet: the app is out of front
/// but not in the background (a system alert over it, Control Center pulled down).
pub(crate) const LOOK_AGAIN: Duration = Duration::from_secs(1);

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

/// How long to wait before reading the grace again, given what is `left` of it; `None` when the
/// phone should stop listening now. The system counts nothing (`left` is `None`) while the app
/// is merely out of front.
pub(crate) const fn until_deaf(left: Option<Duration>) -> Option<Duration> {
    match left {
        None => Some(LOOK_AGAIN),
        Some(left) => match left.checked_sub(STOP_BEFORE) {
            Some(wait) if !wait.is_zero() => Some(wait),
            Some(_) | None => None,
        },
    }
}

/// The phone's push state, kept by the app.
#[cfg(target_os = "ios")]
#[derive(Debug, Default)]
pub(crate) struct Pushing {
    /// This installation's id and the public half of its key, read once both could be.
    me: Option<(slopty_core::ClientId, [u8; 32])>,
    /// The clock that stops the phone listening near the grace's end, while the app is away.
    ending: Option<gpui::Task<()>>,
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

    /// The app came to the front or left it. In front it hears its link again. Away, it hears
    /// until just before the grace runs out, or not at all when the system granted none.
    pub(crate) fn listen_while(&mut self, active: bool, cx: &Context<Self>) {
        if active {
            self.pushing.ending = None;
            self.set_listening(true);
            return;
        }
        if self.grace.is_none() {
            self.set_listening(false);
            return;
        }
        self.pushing.ending = Some(cx.spawn(async move |this, cx| {
            loop {
                let next = this.update(cx, |ws, _cx| {
                    until_deaf(
                        ws.grace
                            .as_ref()
                            .and_then(slopty_platform::notify::BackgroundGrace::remaining),
                    )
                });
                match next {
                    Ok(Some(wait)) => cx.background_executor().timer(wait).await,
                    Ok(None) => {
                        let _gone = this.update(cx, |ws, cx| {
                            ws.set_listening(false);
                            ws.tell_presence(cx);
                        });
                        break;
                    }
                    Err(_gone) => break,
                }
            }
        }));
    }

    /// Whether the app still hears notices on its link: the next presence says so, and while
    /// it does not, the server's pushes say what the app would have posted.
    fn set_listening(&mut self, listening: bool) {
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

    #[test]
    fn the_phone_stops_listening_just_before_its_grace_runs_out() {
        assert_eq!(until_deaf(None), Some(LOOK_AGAIN), "out of front, no grace counted yet");
        assert_eq!(until_deaf(Some(Duration::from_secs(30))), Some(Duration::from_secs(25)));
        assert_eq!(until_deaf(Some(STOP_BEFORE)), None, "at the margin it stops");
        assert_eq!(until_deaf(Some(Duration::from_secs(1))), None);
        assert_eq!(until_deaf(Some(Duration::ZERO)), None, "a spent grace stops it too");
    }
}
