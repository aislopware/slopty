//! slopty: a call the person waits on that runs past ten seconds says so in one toast ("Waiting
//! on studio · List folders"), which goes when the call answers. Calls meant to run long
//! (watches, streams, clones, installs, updates, uploads, a device being set up) never raise it.
//!
//! The watch wraps the engine's service, so every call the shell makes passes through it,
//! whatever view made it; the toast is kept by a task on the app's side.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::App;
use slopty_view_rpc::{RpcError, RpcReply, RpcService, methods};

use super::AppState;

/// How long a call runs before the person hears of it.
pub(crate) const SLOW_AFTER: Duration = Duration::from_secs(10);
/// How long the toast may stay up for a call that never answers.
const TOAST_LASTS: Duration = Duration::from_secs(600);

/// What the watch says of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlowCall {
    /// Call `id` has run past the threshold.
    Slow { id: u64, call: Call },
    /// A slow call answered, failed or was dropped.
    Answered(u64),
}

/// A slow call as the person hears of it: what it does and whom it waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    method: String,
    /// `Mutate`'s own name for what it changes.
    op: Option<String>,
    device: Option<String>,
    chat: Option<String>,
}

impl Call {
    fn of(method: &str, params: &serde_json::Value) -> Self {
        let text = |key: &str| params.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        Self {
            method: method.to_owned(),
            op: text("op"),
            device: text("deviceId"),
            chat: text("chatId"),
        }
    }
}

/// Methods that run long by design: the person started something that takes its time and
/// shows its own progress, or nobody waits on the answer.
fn long_by_design(method: &str) -> bool {
    method.starts_with("Watch")
        || method.starts_with("Subscribe")
        || matches!(
            method,
            methods::CLONE_REPO
                | methods::INSTALL_HARNESS
                | methods::APPLY_UPDATE
                | methods::FETCH_ALL
                | methods::UPLOAD_CHUNK
                | methods::UPLOAD_COMMIT
                | methods::READ_ATTACHMENT_CHUNK
                | methods::ADD_DEVICE
                | methods::ANSWER_ADD_DEVICE
                | methods::LOOK_ON_TAILNET
                | methods::DEVICE_ACTION
                | methods::START_AGENT_LOGIN
                | methods::COMPLETE_AGENT_LOGIN
                | methods::POLL_AGENT_LOGIN
                | methods::STOP_ENGINE
        )
}

/// The engine's service, with every call timed.
pub(crate) struct SlowCallWatch {
    inner: Arc<dyn RpcService>,
    tx: UnboundedSender<SlowCall>,
    after: Duration,
    next: AtomicU64,
}

impl SlowCallWatch {
    /// `inner`, telling the receiver of each call that runs past `after`, and of its answer.
    pub(crate) fn wrap(
        inner: Arc<dyn RpcService>,
        after: Duration,
    ) -> (Arc<dyn RpcService>, UnboundedReceiver<SlowCall>) {
        let (tx, rx) = unbounded();
        let watch = Self { inner, tx, after, next: AtomicU64::new(1) };
        (Arc::new(watch), rx)
    }
}

/// Says a slow call answered, however its future ends (an answer, an error, or dropped).
struct Answered {
    id: u64,
    tx: UnboundedSender<SlowCall>,
    slow: bool,
}

impl Drop for Answered {
    fn drop(&mut self) {
        if self.slow {
            let _gone = self.tx.unbounded_send(SlowCall::Answered(self.id));
        }
    }
}

#[async_trait]
impl RpcService for SlowCallWatch {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if long_by_design(method) {
            return self.inner.handle(method, params).await;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let call = Call::of(method, &params);
        let reply = self.inner.handle(method, params);
        futures::pin_mut!(reply);
        let mut answered = Answered { id, tx: self.tx.clone(), slow: false };
        match tokio::time::timeout(self.after, &mut reply).await {
            Ok(reply) => reply,
            Err(_) => {
                answered.slow = self.tx.unbounded_send(SlowCall::Slow { id, call }).is_ok();
                reply.await
            }
        }
    }
}

/// The slow calls outstanding, and the toast that says so.
#[derive(Default)]
pub(crate) struct SlowCalls {
    open: BTreeMap<u64, Call>,
    toast: Option<u64>,
}

impl SlowCalls {
    /// Take `event` in, and show, move or take down the toast.
    pub(crate) fn apply(&mut self, event: SlowCall, state: &AppState, cx: &mut App) {
        match event {
            SlowCall::Slow { id, call } => {
                self.open.insert(id, call);
            }
            SlowCall::Answered(id) => {
                self.open.remove(&id);
            }
        }
        let Some(words) = self.words(state) else {
            if let Some(toast) = self.toast.take() {
                crate::toast::withdraw(cx, toast);
            }
            return;
        };
        match self.toast.filter(|toast| crate::toast::showing(cx, *toast)) {
            Some(toast) => crate::toast::update(cx, toast, words, None),
            None => self.toast = crate::toast::working(cx, words, TOAST_LASTS),
        }
    }

    /// What the toast says: the newest slow call, and how many more wait.
    fn words(&self, state: &AppState) -> Option<String> {
        let (_, newest) = self.open.iter().next_back()?;
        let machine = newest
            .device
            .as_deref()
            .or_else(|| {
                let chat = newest.chat.as_deref()?;
                state.chats.iter().find(|row| row.id == chat).map(|row| row.device_id.as_str())
            })
            .and_then(|device| state.device_name(device))
            .unwrap_or("the server");
        let what = said(newest.op.as_deref().unwrap_or(&newest.method));
        let more = self.open.len() - 1;
        Some(match more {
            0 => format!("Waiting on {machine} \u{b7} {what}"),
            1 => format!("Waiting on {machine} \u{b7} {what}, and 1 more"),
            more => format!("Waiting on {machine} \u{b7} {what}, and {more} more"),
        })
    }
}

/// A method's name as words: `ListFolders` or `setChatArchived` reads "List folders" or "Set
/// chat archived".
fn said(name: &str) -> String {
    let mut words = String::with_capacity(name.len() + 4);
    for (ix, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && ix > 0 {
            words.push(' ');
            words.extend(ch.to_lowercase());
        } else if ix == 0 {
            words.extend(ch.to_uppercase());
        } else {
            words.push(ch);
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// Answers after its delay, or at once.
    struct Held(Duration);

    #[async_trait]
    impl RpcService for Held {
        async fn handle(&self, _: &str, _: serde_json::Value) -> Result<RpcReply, RpcError> {
            tokio::time::sleep(self.0).await;
            Ok(RpcReply::Value(serde_json::json!({})))
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    /// A held call says so once past the threshold and again when it answers; a quick call and
    /// a call long by design say nothing.
    #[test]
    fn a_held_call_is_slow_until_it_answers() {
        runtime().block_on(async {
            let after = Duration::from_millis(20);
            let (held, mut events) = SlowCallWatch::wrap(Arc::new(Held(after * 4)), after);
            let params = serde_json::json!({ "deviceId": "studio" });
            held.handle(methods::LIST_FOLDERS, params.clone()).await.unwrap();
            assert!(matches!(
                events.next().await,
                Some(SlowCall::Slow { id: 1, call }) if call.device.as_deref() == Some("studio")
                    && call.method == methods::LIST_FOLDERS
            ));
            assert_eq!(events.next().await, Some(SlowCall::Answered(1)));
            held.handle(methods::WATCH_SESSIONS, params.clone()).await.unwrap();
            held.handle(methods::CLONE_REPO, params).await.unwrap();
            let (quick, mut quiet) = SlowCallWatch::wrap(Arc::new(Held(Duration::ZERO)), after);
            quick.handle(methods::LIST_FOLDERS, serde_json::json!({})).await.unwrap();
            drop((held, quick));
            assert_eq!(events.next().await, None, "long by design: nothing said");
            assert_eq!(quiet.next().await, None, "quick: nothing said");
        });
    }

    /// A slow call dropped before it answers still takes its toast down.
    #[test]
    fn a_dropped_call_answers_too() {
        runtime().block_on(async {
            let after = Duration::from_millis(10);
            let (held, mut events) = SlowCallWatch::wrap(Arc::new(Held(Duration::from_secs(60))), after);
            let call = held.handle(methods::LIST_REFS, serde_json::json!({}));
            let _timed_out = tokio::time::timeout(after * 5, call).await;
            assert!(matches!(events.next().await, Some(SlowCall::Slow { id: 1, .. })));
            assert_eq!(events.next().await, Some(SlowCall::Answered(1)));
        });
    }

    #[test]
    fn a_method_reads_as_words() {
        assert_eq!(said("ListFolders"), "List folders");
        assert_eq!(said("setChatArchived"), "Set chat archived");
        assert_eq!(said("Mutate"), "Mutate");
    }

    /// The toast names the machine (by the call's device, else its chat's), says how many more
    /// wait, and goes when the last answers.
    #[gpui::test]
    fn the_toast_names_the_machine_and_goes_with_the_last_answer(cx: &mut gpui::TestAppContext) {
        let mut state = AppState::new();
        state.devices = vec![
            serde_json::from_value(serde_json::json!({
                "id": "d1", "name": "studio", "platform": "macos",
            }))
            .unwrap(),
        ];
        state.chats = vec![
            serde_json::from_value(serde_json::json!({
                "id": "c1", "deviceId": "d1", "archived": false, "createdAt": chrono::Utc::now(),
            }))
            .unwrap(),
        ];
        let call = |method: &str, params: serde_json::Value| Call::of(method, &params);
        let mut slow = SlowCalls::default();
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::dark());
            slow.apply(
                SlowCall::Slow { id: 1, call: call("ListRefs", serde_json::json!({ "chatId": "c1" })) },
                &state,
                cx,
            );
            assert_eq!(crate::toast::messages(cx), ["Waiting on studio \u{b7} List refs"]);
            slow.apply(
                SlowCall::Slow {
                    id: 2,
                    call: call("Mutate", serde_json::json!({ "op": "setChatArchived" })),
                },
                &state,
                cx,
            );
            assert_eq!(
                crate::toast::messages(cx),
                ["Waiting on the server \u{b7} Set chat archived, and 1 more"],
                "one toast, moved in place"
            );
            slow.apply(SlowCall::Answered(2), &state, cx);
            assert_eq!(crate::toast::messages(cx), ["Waiting on studio \u{b7} List refs"]);
            slow.apply(SlowCall::Answered(1), &state, cx);
            assert!(crate::toast::messages(cx).is_empty(), "gone with the last answer");
        });
    }
}
