// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! slopty-view-rpc — the typed control plane (UiRpc / ControlRpc) over WebSocket + in-memory
//! transports, plus the device-room relay transport ({s,k,to,from} frames — `device_room`; slopty: cut).
//!
//! Framing: ndjson envelopes, one JSON object per WebSocket text message (or per line on
//! byte transports), matching the shape of slopty's Effect RPC without the Effect runtime:
//!
//! - client → server: `{id, method, params}` to invoke, `{id, cancel: true}` to stop a stream;
//! - server → client: `{id, ok}` / `{id, err}` for unary calls,
//!   `{id, item}`* then `{id, done: true}` (or `{id, err}`) for streams.
//!
//! The server dispatches into an [`RpcService`]; the [`RpcClient`] offers `call` and
//! `subscribe`. Both ends run over any pair of string channels, so the in-memory transport
//! ([`memory_client`]) exercises the exact same code path as the WebSocket one.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

mod client;
mod server;

pub use client::{RpcClient, RpcSubscription};
// slopty: device rooms (the relay transport) are cut with sync.
pub use server::serve_connection;

/// RPC method names — single source of truth for both ends.
/// Full surface: docs/research/feature-inventory.md §2.
pub mod methods {
    // slopty: voice removed.
    pub const WATCH_PREVIEWS: &str = "WatchPreviews";
    pub const LIST_HARNESSES: &str = "ListHarnesses";
    pub const INSTALL_HARNESS: &str = "InstallHarness";
    // slopty: no thread naming: an agent's thread is titled by the agent.
    /// Flip a harness's enablement on the target device (Settings → Providers);
    /// replies with the device's fresh `ListHarnesses` catalog.
    pub const SET_HARNESS_ENABLED: &str = "SetHarnessEnabled";
    // slopty: what the chats said, searched: `{query, limit}` →
    // `[{chatId, role: "user"|"agent", excerpt, matchStart, matchEnd}]`, byte offsets into
    // the excerpt.
    pub const SEARCH_CHATS: &str = "SearchChats";
    pub const WATCH_DOC_MESSAGES: &str = "WatchDocMessages";
    /// Explicit user navigation, `{chatId}`. Prioritizes this device's sync
    /// connection; automatic subscriptions and retries must not call it.
    pub const FOCUS_CHAT: &str = "FocusChat";
    /// Nudge every open room client to verify liveness NOW (window focus,
    /// app foregrounded). No params; IPC-only. Each room ignores the hint
    /// unless it has been broadcast-quiet ≥30s, so this is cheap to spam.
    pub const PROBE_SYNC: &str = "ProbeSync";
    /// Pushed edge-connectivity posture (`slopty_view_proto::Connectivity`):
    /// current value first, then every change — the connection pill /
    /// composer-honesty / queued-badge feed. No params; IPC-only.
    pub const WATCH_CONNECTIVITY: &str = "WatchConnectivity";
    pub const WATCH_CHATS: &str = "WatchChats";
    pub const WATCH_SIDEBAR_PREFERENCES: &str = "WatchSidebarPreferences";
    pub const WATCH_DEVICES: &str = "WatchDevices";
    pub const WATCH_SESSIONS: &str = "WatchSessions";
    /// Spaces registry (device+folder pairs) from the workspace doc.
    pub const WATCH_SPACES: &str = "WatchSpaces";
    /// Entity mutations against the workspace doc (feature-inventory §2 DataRpc).
    /// Params are tagged `{op: createChat|createSpace|renameSpace|deleteSpace|
    /// renameChat|setChatArchived|deleteChat|renameDevice|markChatSeen, …}`.
    pub const MUTATE: &str = "Mutate";
    /// This engine's identity → `{deviceId}` (IPC-only; never relay-forwarded —
    /// the answer is about whichever engine you are directly connected to).
    pub const LOCAL_DEVICE: &str = "LocalDevice";
    /// This engine runtime's fixed device and workspace identity.
    pub const ENGINE_INFO: &str = "EngineInfo";
    /// Readiness barrier for the engine runtime. The call completes once stores
    /// and journals are assembled, or fails with the assembly error.
    pub const ENGINE_READY: &str = "EngineReady";
    /// Ask a headless IPC owner to drain its runtime and exit successfully.
    /// Headed IPC owners do not implement this method: closing another app's
    /// engine behind its windows would leave that process unusable.
    pub const STOP_ENGINE: &str = "StopEngine";
    pub const AUTH_STATUS: &str = "AuthStatus";
    // AuthRpc mutations (feature-inventory §2 AuthRpc; IPC-only).
    pub const SIGN_IN: &str = "SignIn";
    // Repos / worktrees / folders (ControlRpc, relay-forwardable).
    pub const CLONE_REPO: &str = "CloneRepo";
    pub const LIST_BRANCHES: &str = "ListBranches";
    pub const LIST_REFS: &str = "ListRefs";
    // slopty: no History surface (ruled out).
    /// Update remote-tracking refs without changing HEAD, the index, or files.
    pub const FETCH_ALL: &str = "FetchAll";
    pub const LIST_FOLDERS: &str = "ListFolders";
    /// The device's browse roots: home plus mounted drives/volumes.
    pub const LIST_DRIVES: &str = "ListDrives";
    /// Folders under the host's home by name (new-project picker).
    pub const SEARCH_HOME_FOLDERS: &str = "SearchHomeFolders";
    /// The new-project picker closed: drop the host's home index if unpinned.
    pub const RELEASE_HOME_FOLDER_SEARCH: &str = "ReleaseHomeFolderSearch";
    // Device-local workspace filesystem operations. All are relay-forwardable;
    // WatchWorkspaceFiles is the only streaming method in this group.
    pub const LIST_WORKSPACE_DIRECTORY: &str = "ListWorkspaceDirectory";
    pub const SEARCH_WORKSPACE_FILES: &str = "SearchWorkspaceFiles";
    /// Start or keep alive a workspace's search index (fire-and-forget warm).
    pub const WARM_WORKSPACE_SEARCH: &str = "WarmWorkspaceSearch";
    /// Plain-text search over a workspace's file contents.
    pub const SEARCH_WORKSPACE_CONTENT: &str = "SearchWorkspaceContent";
    pub const READ_WORKSPACE_IMAGE: &str = "ReadWorkspaceImage";
    pub const READ_WORKSPACE_FILE: &str = "ReadWorkspaceFile";
    pub const MOVE_WORKSPACE_ENTRY: &str = "MoveWorkspaceEntry";
    pub const DELETE_WORKSPACE_ENTRY: &str = "DeleteWorkspaceEntry";
    pub const WRITE_WORKSPACE_FILE: &str = "WriteWorkspaceFile";
    pub const WATCH_WORKSPACE_FILES: &str = "WatchWorkspaceFiles";
    // Project Actions are private state on the device that owns the project.
    pub const LIST_PROJECT_ACTIONS: &str = "ListProjectActions";
    pub const UPSERT_PROJECT_ACTION: &str = "UpsertProjectAction";
    pub const DELETE_PROJECT_ACTION: &str = "DeleteProjectAction";
    pub const RUN_PROJECT_ACTION: &str = "RunProjectAction";
    // Terminals (ControlRpc, relay-forwardable).
    pub const OPEN_TERMINAL: &str = "OpenTerminal";
    pub const CLOSE_TERMINAL: &str = "CloseTerminal";
    /// Checkout-diff stream for the target device's chats (DataRpc,
    /// relay-forwardable — diffs are produced where the checkout lives).
    pub const WATCH_CHECKOUT_DIFFS: &str = "WatchCheckoutDiffs";
    pub const WATCH_WORKSPACE_GIT_STATUS: &str = "WatchWorkspaceGitStatus";
    /// Current pull request for one checkout, resolved on the checkout's host device.
    pub const WATCH_CHECKOUT_CHANGE_REQUEST: &str = "WatchCheckoutChangeRequest";
    pub const GET_CHECKOUT_DIFF: &str = "GetCheckoutDiff";
    // slopty: no Discard changes (ruled out).
    pub const GET_CHECKOUT_FILE_DIFF_TEXT: &str = "GetCheckoutFileDiffText";
    // slopty: no agent accounts or logins: an agent signs in in its own TUI.
    // Attachments read back (ControlRpc, relay-forwardable — target the chat's host device).
    pub const READ_ATTACHMENT_CHUNK: &str = "ReadAttachmentChunk";
    // slopty: no agent visuals.
    /// Lazy full-tool-output fetch from the R2 sidecar by doc-resident ref
    /// (chat2-sync A3). Edge-direct from any device — never relay-forwarded.
    pub const FETCH_TOOL_BLOB: &str = "FetchToolBlob";
    // Updates (ControlRpc, relay-forwardable — a device applies its own binary's update).
    /// Download + apply the newest release on the target device (symlink-managed
    /// installs; the service restart is scheduled after the reply flushes).
    pub const APPLY_UPDATE: &str = "ApplyUpdate";
    // slopty: the person's one setting for how freely agents act, `[agents] autonomy` in
    // Slopty's settings: `{ "autonomy": "auto" | "full" | "ask" }` read and written.
    pub const GET_AGENT_SETTINGS: &str = "GetAgentSettings";
    pub const SET_AGENT_SETTINGS: &str = "SetAgentSettings";
    // slopty: Settings → Devices' Add device. `AddDevice { kind: thisMac | ssh | server |
    // serveSsh, target }` streams the run's progress (`run`, `steps`, `share`, `failure`,
    // `choices`, `added`); `AnswerAddDevice { run, trust | password + addKey | fix | choose }`
    // answers a run that stopped to ask. Dropping the stream stops the run. `LookOnTailnet`
    // streams the servers and machines the tailnet answers with.
    pub const ADD_DEVICE: &str = "AddDevice";
    pub const ANSWER_ADD_DEVICE: &str = "AnswerAddDevice";
    pub const LOOK_ON_TAILNET: &str = "LookOnTailnet";
    // slopty: one of a device's (or the server's, `deviceId: "server"`) actions pressed:
    // `DeviceAction { deviceId, action }`; the app runs it and says how it went in a toast.
    pub const DEVICE_ACTION: &str = "DeviceAction";
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("bad params: {0}")]
    BadParams(String),
    /// Stable machine-readable capability failure preserved across transports.
    #[error("capability error: {0}")]
    Capability(String),
    #[error("{0}")]
    Failed(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("connection closed")]
    Closed,
}

/// A client-originated frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientFrame {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancel: bool,
}

/// A server-originated frame. Exactly one of `ok` / `err` / `item` / `done` is meaningful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerFrame {
    pub id: u64,
    /// `"ok": null` is a reply whose value is `null` (for example an absent
    /// `Option`), distinct from a frame without `ok`.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub ok: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

fn present_value<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// What a service returns for one invocation.
pub enum RpcReply {
    /// Unary response — sent as `{id, ok}`.
    Value(serde_json::Value),
    /// Stream — each item sent as `{id, item}`, then `{id, done: true}` when it ends.
    Stream(BoxStream<'static, serde_json::Value>),
}

impl RpcReply {
    /// Serialize a value into a unary reply.
    pub fn value<T: Serialize>(value: &T) -> Result<Self, RpcError> {
        serde_json::to_value(value)
            .map(RpcReply::Value)
            .map_err(|e| RpcError::Failed(format!("serialize response: {e}")))
    }
}

/// Server-side dispatch: one implementation serves every transport.
#[async_trait]
pub trait RpcService: Send + Sync + 'static {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError>;
}

/// Deserialize typed params out of the envelope's `params` value.
pub fn parse_params<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))
}

/// Spawn an in-memory server for `service` and return a connected client.
/// Same envelopes, same dispatch loop as the WebSocket path — the in-process UI
/// transport (ARCHITECTURE §1 "zero serialization shortcuts").
pub fn memory_client(service: Arc<dyn RpcService>) -> RpcClient {
    let (client_out, server_in) = tokio::sync::mpsc::channel::<String>(256);
    let (server_out, client_in) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, server_out, server_in));
    RpcClient::new(client_out, client_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Mutex;

    struct TestService;

    struct CancelAwareService {
        dropped: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[async_trait]
    impl RpcService for CancelAwareService {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if !matches!(
                method,
                methods::WATCH_CHECKOUT_CHANGE_REQUEST | "Silent"
            ) {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let guard = DropSignal(self.dropped.lock().unwrap().take());
            let stream = futures::stream::unfold(guard, |guard| async move {
                let item = std::future::pending::<Option<(serde_json::Value, DropSignal)>>().await;
                drop(guard);
                item
            });
            Ok(RpcReply::Stream(stream.boxed()))
        }
    }

    #[async_trait]
    impl RpcService for TestService {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                "Echo" => Ok(RpcReply::Value(params)),
                "Count" => {
                    let n = params.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                    Ok(RpcReply::Stream(
                        futures::stream::iter((0..n).map(|i| serde_json::json!(i))).boxed(),
                    ))
                }
                "Never" => Ok(RpcReply::Stream(futures::stream::pending().boxed())),
                "Boom" => Err(RpcError::Failed("boom".into())),
                "Capability" => Err(RpcError::Capability("harness.missing".into())),
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    #[tokio::test]
    async fn memory_call_stream_and_error() {
        let client = memory_client(Arc::new(TestService));

        let echoed = client
            .call("Echo", serde_json::json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(echoed, serde_json::json!({"x": 1}));

        let mut items = client
            .subscribe("Count", serde_json::json!({"n": 3}))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(v) = items.recv().await {
            seen.push(v);
        }
        assert_eq!(
            seen,
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );

        let err = client
            .call("Boom", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, RpcError::Failed(m) if m == "boom"));

        let err = client
            .call("Capability", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RpcError::Capability(code) if code == "harness.missing"
        ));
    }

    #[tokio::test]
    async fn checked_stream_acknowledges_support_and_preserves_unknown_method() {
        let client = memory_client(Arc::new(TestService));

        let mut items = client
            .subscribe_checked("Count", serde_json::json!({"n": 1}))
            .await
            .unwrap();
        assert_eq!(items.recv().await, Some(serde_json::json!(0)));
        assert_eq!(items.recv().await, None);

        let error = match client
            .subscribe_checked("FutureStream", serde_json::Value::Null)
            .await
        {
            Ok(_) => panic!("old service must reject unknown stream"),
            Err(error) => error,
        };
        assert!(matches!(error, RpcError::UnknownMethod(method) if method == "FutureStream"));
    }

    #[tokio::test]
    async fn dropping_checked_subscription_cancels_pending_server_stream() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let client = memory_client(Arc::new(CancelAwareService {
            dropped: Mutex::new(Some(dropped_tx)),
        }));
        let stream = client
            .subscribe_checked(
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                serde_json::Value::Null,
            )
            .await
            .unwrap();

        drop(stream);

        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("server stream cancelled")
            .expect("drop signal");
    }

    #[tokio::test]
    async fn scoped_subscription_returns_before_first_item_and_cancels_silence() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let service = Arc::new(CancelAwareService {
            dropped: Mutex::new(Some(dropped_tx)),
        });
        let client = memory_client(service.clone());
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.subscribe_scoped("Silent", serde_json::Value::Null),
        )
        .await
        .unwrap()
        .unwrap();
        while service.dropped.lock().unwrap().is_some() {
            tokio::task::yield_now().await;
        }
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn dropping_one_quiet_watch_preserves_the_other() {
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let (second_tx, mut second_rx) = tokio::sync::oneshot::channel();
        let first = memory_client(Arc::new(CancelAwareService {
            dropped: Mutex::new(Some(first_tx)),
        }));
        let second = memory_client(Arc::new(CancelAwareService {
            dropped: Mutex::new(Some(second_tx)),
        }));
        let first_watch = first
            .subscribe_checked(methods::WATCH_CHECKOUT_CHANGE_REQUEST, serde_json::Value::Null)
            .await
            .unwrap();
        let second_watch = second
            .subscribe_checked(methods::WATCH_CHECKOUT_CHANGE_REQUEST, serde_json::Value::Null)
            .await
            .unwrap();

        drop(first_watch);
        tokio::time::timeout(std::time::Duration::from_secs(1), first_rx)
            .await
            .expect("first device's quiet stream cancelled")
            .expect("first drop signal");
        assert!(matches!(
            second_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        drop(second_watch);
        tokio::time::timeout(std::time::Duration::from_secs(1), second_rx)
            .await
            .expect("second device's quiet stream cancelled")
            .expect("second drop signal");
    }

    #[tokio::test]
    async fn unary_null_reply_resolves_the_call() {
        let client = memory_client(Arc::new(TestService));
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.call("Echo", serde_json::Value::Null),
        )
        .await
        .expect("a null reply is a reply")
        .unwrap();
        assert_eq!(reply, serde_json::Value::Null);
        let frame: ServerFrame = serde_json::from_str(r#"{"id":1,"ok":null}"#).unwrap();
        assert_eq!(frame.ok, Some(serde_json::Value::Null));
        let frame: ServerFrame = serde_json::from_str(r#"{"id":1,"done":true}"#).unwrap();
        assert_eq!(frame.ok, None);
    }

    // slopty: the WebSocket round-trip and Origin tests go with the transport.

    #[tokio::test]
    async fn dropping_stream_receiver_cancels_server_side() {
        let client = memory_client(Arc::new(TestService));
        let items = client
            .subscribe("Never", serde_json::Value::Null)
            .await
            .unwrap();
        drop(items);
        // The next unary call still works — the dead stream didn't wedge the connection.
        let echoed = client.call("Echo", serde_json::json!(2)).await.unwrap();
        assert_eq!(echoed, serde_json::json!(2));
    }
}
