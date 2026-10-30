// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! slopty-view-rpc — the calls the shell makes of Slopty's view model
//! (`slopty_ui::view_model::ViewModel`), in process.
//!
//! A call is a JSON envelope on a pair of string channels, one object per line:
//!
//! - client → server: `{id, method, params}` to invoke, `{id, cancel: true}` to stop a stream;
//! - server → client: `{id, ok}` / `{id, err}` for unary calls, `{id, item}`* then `{id, done:
//!   true}` (or `{id, err}`) for streams.
//!
//! The server dispatches into an [`RpcService`]; the [`RpcClient`] offers `call` and the
//! subscriptions. [`memory_client`] joins the two over in-process channels, the one transport;
//! a test hands [`RpcClient::new`] a duplex of its own.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

mod client;
mod server;

pub use client::{RpcClient, RpcSubscription};
pub use server::serve_connection;

/// The method names the shell calls and the view model answers, one list for both ends;
/// `slopty_view_doc::calls` names the view model's own calls beyond these.
pub mod methods {
    /// The previews of the sessions' live screens, the current set first, then every change.
    pub const WATCH_PREVIEWS: &str = "WatchPreviews";
    /// The agent programs a device can run, with whether each is installed and enabled.
    pub const LIST_HARNESSES: &str = "ListHarnesses";
    /// Install an agent program on a device.
    pub const INSTALL_HARNESS: &str = "InstallHarness";
    /// Flip a harness's enablement on the target device (Settings → Providers);
    /// replies with the device's fresh `ListHarnesses` catalog.
    pub const SET_HARNESS_ENABLED: &str = "SetHarnessEnabled";
    /// What the chats said, searched: `{query, limit}` →
    /// `[{chatId, role: "user"|"agent", excerpt, matchStart, matchEnd}]`, byte offsets into
    /// the excerpt.
    pub const SEARCH_CHATS: &str = "SearchChats";
    /// A chat's transcript: a baseline, then its changes as frames.
    pub const WATCH_DOC_MESSAGES: &str = "WatchDocMessages";
    /// The links to the server and the machines (`slopty_view_proto::Connectivity`): the
    /// current value first, then every change, for the connection pill. No params.
    pub const WATCH_CONNECTIVITY: &str = "WatchConnectivity";
    /// The chats, once the workspace knows them, then every change.
    pub const WATCH_CHATS: &str = "WatchChats";
    /// The devices, then every change.
    pub const WATCH_DEVICES: &str = "WatchDevices";
    /// The sessions, each a chat's run on its machine, then every change.
    pub const WATCH_SESSIONS: &str = "WatchSessions";
    /// The projects, each a folder on a device, as the workspace keeps them.
    pub const WATCH_SPACES: &str = "WatchSpaces";
    /// A change to a chat or a project, tagged `{op: markChatSeen|renameChat|setChatArchived|
    /// createSpace|renameSpace|deleteSpace, …}`; any other op fails with its name.
    pub const MUTATE: &str = "Mutate";
    /// This machine → `{deviceId}`: its worker's id once the app knows there is one, the
    /// view model's own id while there is none.
    pub const LOCAL_DEVICE: &str = "LocalDevice";
    /// The view model's fixed device and workspace identity.
    pub const ENGINE_INFO: &str = "EngineInfo";
    /// Clone a repository into a folder on a device.
    pub const CLONE_REPO: &str = "CloneRepo";
    /// A repository's branches on a device.
    pub const LIST_BRANCHES: &str = "ListBranches";
    /// The folders in a folder on a device.
    pub const LIST_FOLDERS: &str = "ListFolders";
    /// The device's browse roots: home plus mounted drives/volumes.
    pub const LIST_DRIVES: &str = "ListDrives";
    /// Folders under the host's home by name (new-project picker).
    pub const SEARCH_HOME_FOLDERS: &str = "SearchHomeFolders";
    /// The new-project picker closed: drop the host's home index if unpinned.
    pub const RELEASE_HOME_FOLDER_SEARCH: &str = "ReleaseHomeFolderSearch";
    /// The entries of a directory in a workspace, asked of the worker the call names.
    pub const LIST_WORKSPACE_DIRECTORY: &str = "ListWorkspaceDirectory";
    /// A workspace's files by name.
    pub const SEARCH_WORKSPACE_FILES: &str = "SearchWorkspaceFiles";
    /// Start or keep alive a workspace's search index (fire-and-forget warm).
    pub const WARM_WORKSPACE_SEARCH: &str = "WarmWorkspaceSearch";
    /// Plain-text search over a workspace's file contents.
    pub const SEARCH_WORKSPACE_CONTENT: &str = "SearchWorkspaceContent";
    /// An image in a workspace, read whole.
    pub const READ_WORKSPACE_IMAGE: &str = "ReadWorkspaceImage";
    /// A text file in a workspace, read whole.
    pub const READ_WORKSPACE_FILE: &str = "ReadWorkspaceFile";
    /// Move or rename a file or folder in a workspace.
    pub const MOVE_WORKSPACE_ENTRY: &str = "MoveWorkspaceEntry";
    /// Delete a file or folder in a workspace.
    pub const DELETE_WORKSPACE_ENTRY: &str = "DeleteWorkspaceEntry";
    /// Write a text file in a workspace.
    pub const WRITE_WORKSPACE_FILE: &str = "WriteWorkspaceFile";
    /// A workspace's file changes as they happen, the one stream among its file calls.
    pub const WATCH_WORKSPACE_FILES: &str = "WatchWorkspaceFiles";
    /// Open a terminal on a device.
    pub const OPEN_TERMINAL: &str = "OpenTerminal";
    /// Close a terminal.
    pub const CLOSE_TERMINAL: &str = "CloseTerminal";
    /// Checkout-diff stream for the target device's chats, made on the worker where the
    /// checkout lives.
    pub const WATCH_CHECKOUT_DIFFS: &str = "WatchCheckoutDiffs";
    /// A workspace's git status, then every change.
    pub const WATCH_WORKSPACE_GIT_STATUS: &str = "WatchWorkspaceGitStatus";
    /// A checkout's diff, file by file.
    pub const GET_CHECKOUT_DIFF: &str = "GetCheckoutDiff";
    /// One file's diff in a checkout, as text.
    pub const GET_CHECKOUT_FILE_DIFF_TEXT: &str = "GetCheckoutFileDiffText";
    /// A chunk of an attachment, read back from the chat's machine.
    pub const READ_ATTACHMENT_CHUNK: &str = "ReadAttachmentChunk";
    /// What a transcript left out of its thread, by `{blobRef: "<chatId>/<item>"}`: a tool
    /// call's whole output, an edit's whole diff, a picture or a visual, read whole from its
    /// worker.
    pub const FETCH_TOOL_BLOB: &str = "FetchToolBlob";
    /// The person's one setting for how freely agents act, `[agents] autonomy` in Slopty's
    /// settings: `{ "autonomy": "auto" | "full" | "ask" }`.
    pub const GET_AGENT_SETTINGS: &str = "GetAgentSettings";
    /// [`GET_AGENT_SETTINGS`]'s setting, written.
    pub const SET_AGENT_SETTINGS: &str = "SetAgentSettings";
    /// Settings → Devices' Add device, streaming the run's progress.
    ///
    /// `AddDevice { kind: thisMac | ssh | server | serveSsh, target }` streams `run`, `steps`,
    /// `share`, `failure`, `choices` and `added`. Dropping the stream stops the run.
    pub const ADD_DEVICE: &str = "AddDevice";
    /// `AnswerAddDevice { run, trust | password + addKey | fix | choose }` answers an
    /// [`ADD_DEVICE`] run that stopped to ask.
    pub const ANSWER_ADD_DEVICE: &str = "AnswerAddDevice";
    /// The servers and machines the tailnet answers with, as a stream.
    pub const LOOK_ON_TAILNET: &str = "LookOnTailnet";
    /// One of a device's (or the server's, `deviceId: "server"`) actions pressed:
    /// `DeviceAction { deviceId, action }`; the app runs it and says how it went in a toast.
    pub const DEVICE_ACTION: &str = "DeviceAction";
}

/// Why a call failed. Its words cross the wire as the frame's `err`, and the client reads the
/// first two back by their prefixes.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The service does not answer this method, by its name.
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    /// The params did not read as the method's, in serde's words.
    #[error("bad params: {0}")]
    BadParams(String),
    /// The method ran and failed, in its words.
    #[error("{0}")]
    Failed(String),
    /// A frame could not be written, in serde's words.
    #[error("transport: {0}")]
    Transport(String),
    /// The other end went away before it answered.
    #[error("connection closed")]
    Closed,
}

/// A client-originated frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientFrame {
    /// The call's number, unique on its connection; the answer carries it back.
    pub id: u64,
    /// The method invoked; none on a cancel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The method's params.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    /// Stop the stream `id` names.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancel: bool,
}

/// A server-originated frame. Exactly one of `ok` / `err` / `item` / `done` is meaningful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerFrame {
    /// The number of the call this answers.
    pub id: u64,
    /// `"ok": null` is a reply whose value is `null` (for example an absent
    /// `Option`), distinct from a frame without `ok`.
    #[serde(default, deserialize_with = "present_value", skip_serializing_if = "Option::is_none")]
    pub ok: Option<serde_json::Value>,
    /// The call failed, in an [`RpcError`]'s words; it ends a stream too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    /// One item of a stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<serde_json::Value>,
    /// The stream ended.
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

impl std::fmt::Debug for RpcReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(value) => f.debug_tuple("Value").field(value).finish(),
            Self::Stream(_) => f.write_str("Stream"),
        }
    }
}

impl RpcReply {
    /// Serialize a value into a unary reply.
    pub fn value<T: Serialize>(value: &T) -> Result<Self, RpcError> {
        serde_json::to_value(value)
            .map(RpcReply::Value)
            .map_err(|e| RpcError::Failed(format!("serialize response: {e}")))
    }
}

/// Server-side dispatch: what answers the calls.
#[async_trait]
pub trait RpcService: Send + Sync + 'static {
    /// Answer one call of `method` with `params`: a value, a stream, or why not.
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError>;
}

/// Deserialize typed params out of the envelope's `params` value.
pub fn parse_params<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))
}

/// Spawn an in-memory server for `service` and return a connected client: how the shell
/// reaches its view model.
pub fn memory_client(service: Arc<dyn RpcService>) -> RpcClient {
    let (client_out, server_in) = tokio::sync::mpsc::channel::<String>(256);
    let (server_out, client_in) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, server_out, server_in));
    RpcClient::new(client_out, client_in)
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use parking_lot::Mutex;

    use super::*;

    struct TestService;

    struct CancelAwareService {
        dropped: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _gone = dropped.send(());
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
            // `Once` says one thing and then goes quiet; `Silent` says nothing.
            let first = match method {
                "Once" => Some(serde_json::json!("ready")),
                "Silent" => None,
                _ => return Err(RpcError::UnknownMethod(method.into())),
            };
            let guard = DropSignal(self.dropped.lock().take());
            let stream = futures::stream::unfold((guard, first), async |(guard, first)| {
                if let Some(item) = first {
                    return Some((item, (guard, None)));
                }
                std::future::pending::<()>().await;
                drop(guard);
                None
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
                    let n = params.get("n").and_then(serde_json::Value::as_u64).unwrap_or(0);
                    Ok(RpcReply::Stream(
                        futures::stream::iter((0..n).map(|i| serde_json::json!(i))).boxed(),
                    ))
                }
                "Never" => Ok(RpcReply::Stream(futures::stream::pending().boxed())),
                "Boom" => Err(RpcError::Failed("boom".into())),
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    #[tokio::test]
    async fn memory_call_stream_and_error() {
        let client = memory_client(Arc::new(TestService));

        let echoed = client.call("Echo", serde_json::json!({"x": 1})).await.unwrap();
        assert_eq!(echoed, serde_json::json!({"x": 1}));

        let mut items = client.subscribe("Count", serde_json::json!({"n": 3})).await.unwrap();
        let mut seen = Vec::new();
        while let Some(v) = items.recv().await {
            seen.push(v);
        }
        assert_eq!(seen, vec![serde_json::json!(0), serde_json::json!(1), serde_json::json!(2)]);

        let err = client.call("Boom", serde_json::Value::Null).await.unwrap_err();
        assert!(matches!(err, RpcError::Failed(m) if m == "boom"));
    }

    #[tokio::test]
    async fn checked_stream_acknowledges_support_and_preserves_unknown_method() {
        let client = memory_client(Arc::new(TestService));

        let mut items =
            client.subscribe_checked("Count", serde_json::json!({"n": 1})).await.unwrap();
        assert_eq!(items.recv().await, Some(serde_json::json!(0)));
        assert_eq!(items.recv().await, None);

        let Err(error) = client.subscribe_checked("FutureStream", serde_json::Value::Null).await
        else {
            panic!("old service must reject unknown stream")
        };
        assert!(matches!(error, RpcError::UnknownMethod(method) if method == "FutureStream"));
    }

    #[tokio::test]
    async fn dropping_checked_subscription_cancels_pending_server_stream() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let client =
            memory_client(Arc::new(CancelAwareService { dropped: Mutex::new(Some(dropped_tx)) }));
        let mut stream = client.subscribe_checked("Once", serde_json::Value::Null).await.unwrap();
        assert_eq!(stream.recv().await, Some(serde_json::json!("ready")));

        drop(stream);

        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("server stream cancelled")
            .expect("drop signal");
    }

    #[tokio::test]
    async fn scoped_subscription_returns_before_first_item_and_cancels_silence() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let service = Arc::new(CancelAwareService { dropped: Mutex::new(Some(dropped_tx)) });
        let client = memory_client(Arc::<CancelAwareService>::clone(&service));
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.subscribe_scoped("Silent", serde_json::Value::Null),
        )
        .await
        .unwrap()
        .unwrap();
        while service.dropped.lock().is_some() {
            tokio::task::yield_now().await;
        }
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dropping_one_quiet_watch_preserves_the_other() {
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let (second_tx, mut second_rx) = tokio::sync::oneshot::channel();
        let first =
            memory_client(Arc::new(CancelAwareService { dropped: Mutex::new(Some(first_tx)) }));
        let second =
            memory_client(Arc::new(CancelAwareService { dropped: Mutex::new(Some(second_tx)) }));
        let first_watch = first.subscribe_checked("Once", serde_json::Value::Null).await.unwrap();
        let second_watch = second.subscribe_checked("Once", serde_json::Value::Null).await.unwrap();

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

    #[tokio::test]
    async fn dropping_stream_receiver_cancels_server_side() {
        let client = memory_client(Arc::new(TestService));
        let items = client.subscribe("Never", serde_json::Value::Null).await.unwrap();
        drop(items);
        // The next unary call still works — the dead stream didn't wedge the connection.
        let echoed = client.call("Echo", serde_json::json!(2)).await.unwrap();
        assert_eq!(echoed, serde_json::json!(2));
    }
}
