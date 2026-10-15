//! What the shell fetches of a thread beyond its transcript: a tool call's whole output, an edit's
//! whole diff, and the pictures of its messages and calls, each read through the thread's link
//! to its whole ([`slopty_proto::thread::wire::ThreadRequest::Expand`]) when the worker cut it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use gpui::{AppContext as _, Context, Task};
use slopty_client::WorkerKey;
use slopty_proto::thread::wire::Expanded;
use slopty_proto::thread::{ContentRef, ItemBody, ThreadId, ToolCall, ToolDetail};
use slopty_view_rpc::{RpcError, RpcReply};

use super::param;
use crate::view_model::transcript;
use crate::workspace::WorkspaceView;

/// How long a whole may take to come from its worker.
const WHOLE_WITHIN: Duration = Duration::from_secs(20);

/// How often a whole on its way is looked for.
const WHOLE_EVERY: Duration = Duration::from_millis(50);

/// How much of a picture one chunk of a read back carries: 45 KiB once in base64.
const IMAGE_CHUNK: usize = 45 * 1024 / 4 * 3;

/// The output or diff the shell asked for is not here.
fn missing() -> RpcError {
    RpcError::Failed("That output is not here".to_owned())
}

/// `text` as the shell's blob.
fn blob(text: &str) -> RpcReply {
    RpcReply::Value(serde_json::json!({ "text": text }))
}

/// The whole a worker sent as text, when it did.
fn text_of(whole: Option<&Arc<Expanded>>) -> Option<String> {
    match whole?.as_ref() {
        Expanded::Text(text) => Some(text.clone()),
        Expanded::Bytes(_) | Expanded::Gone => None,
    }
}

impl WorkspaceView {
    /// `FetchToolBlob`: a tool call's whole output (`{thread}/{item}`), or an edit's diff
    /// (`{thread}/{item}.diff`) as the shell draws one ([`transcript::tool_diff`]), each whole
    /// once its worker sent what it had cut.
    pub(super) fn view_tool_blob(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some((chat, item)) = param(params, "blobRef").and_then(|blob| blob.split_once('/'))
        else {
            return Task::ready(Err(missing()));
        };
        let (item, diff) = item.strip_suffix(".diff").map_or((item, false), |item| (item, true));
        let Some((key, thread)) = self.chat_thread(chat, cx) else {
            return Task::ready(Err(missing()));
        };
        let Some(call) = self.view_tool_call(key, thread, item, cx) else {
            return Task::ready(Err(missing()));
        };
        if diff {
            let full = match &call.detail {
                Some(ToolDetail::Edit(edit)) => edit.patch.full.clone(),
                Some(ToolDetail::Write(write)) => write.patch.full.clone(),
                _ => None,
            };
            let Some(full) = full else {
                return Task::ready(diff_blob(&call, None));
            };
            let whole = Self::view_whole(key, thread, full, cx);
            return cx.background_spawn(async move {
                let whole = text_of(whole.await.as_ref());
                diff_blob(&call, whole.as_deref())
            });
        }
        let Some(output) = call.output else { return Task::ready(Err(missing())) };
        let Some(full) = output.full.clone() else { return Task::ready(Ok(blob(&output.text))) };
        let whole = Self::view_whole(key, thread, full, cx);
        cx.background_spawn(async move {
            // What was kept stands in for a whole no longer there.
            Ok(blob(&text_of(whole.await.as_ref()).unwrap_or(output.text)))
        })
    }

    /// The tool call `item` of `thread` on `key`, as its state here holds it.
    fn view_tool_call(
        &self,
        key: WorkerKey,
        thread: ThreadId,
        item: &str,
        cx: &gpui::App,
    ) -> Option<ToolCall> {
        let hub = self.held_hub(key)?.read(cx);
        let state = hub.threads().mirror(thread)?.state()?;
        state.items.iter().find(|i| i.id.0 == item).and_then(|i| match &i.body {
            ItemBody::Tool(call) => Some((**call).clone()),
            _ => None,
        })
    }

    /// The whole of `content` in `thread` on `key`, asked of the worker and asked again while
    /// it does not come (a link that went loses what was asked), for at most [`WHOLE_WITHIN`];
    /// `None` when it did not come.
    pub(super) fn view_whole(
        key: WorkerKey,
        thread: ThreadId,
        content: ContentRef,
        cx: &Context<Self>,
    ) -> Task<Option<Arc<Expanded>>> {
        let until = Instant::now().checked_add(WHOLE_WITHIN).unwrap_or_else(Instant::now);
        cx.spawn(async move |this, cx| {
            loop {
                let held = this
                    .update(cx, |view, cx| {
                        let hub = view.held_hub(key)?.clone();
                        if let Some(whole) = hub.read(cx).whole(&content) {
                            return Some(Some(Arc::clone(whole)));
                        }
                        hub.update(cx, |hub, cx| hub.expand(thread, &content, cx));
                        Some(None)
                    })
                    .ok()??;
                if held.is_some() {
                    return held;
                }
                if Instant::now() >= until {
                    return None;
                }
                cx.background_executor().timer(WHOLE_EVERY).await;
            }
        })
    }

    /// `ReadAttachmentChunk` for a picture of a thread ([`transcript::image_path`]): its bytes
    /// from `offset`, in base64, once its worker sent them.
    pub(super) fn view_image_chunk(
        &self,
        path: &str,
        offset: usize,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let gone = || RpcError::Failed("That picture is not here".to_owned());
        let Some((thread, item, index)) = transcript::image_of(path) else {
            return Task::ready(Err(gone()));
        };
        let Some(key) = self.worker_of_thread(thread, cx) else {
            return Task::ready(Err(gone()));
        };
        let image = self.held_hub(key).and_then(|hub| {
            let state = hub.read(cx).threads().mirror(thread)?.state()?;
            let found = state.items.iter().find(|i| i.id.0 == item)?;
            let images = match &found.body {
                ItemBody::User(message) => &message.images,
                ItemBody::Tool(call) => &call.images,
                _ => return None,
            };
            images.get(index).cloned()
        });
        let Some(image) = image else { return Task::ready(Err(gone())) };
        let name = path.rsplit('/').next().unwrap_or_default().to_owned();
        let whole = Self::view_whole(key, thread, image.at.clone(), cx);
        cx.background_spawn(async move {
            let whole = whole.await.ok_or_else(gone)?;
            let Expanded::Bytes(bytes) = whole.as_ref() else { return Err(gone()) };
            let size = bytes.len();
            let start = offset.min(size);
            let end = start.saturating_add(IMAGE_CHUNK).min(size);
            let data = base64::engine::general_purpose::STANDARD
                .encode(bytes.get(start..end).unwrap_or_default());
            Ok(RpcReply::Value(serde_json::json!({
                "name": name,
                "mimeType": image.media_type,
                "data": data,
                "nextOffset": end,
                "done": end == size,
            })))
        })
    }
}

/// `call`'s diff as the shell's blob, from `whole` when its hunks were cut and it came.
fn diff_blob(call: &ToolCall, whole: Option<&str>) -> Result<RpcReply, RpcError> {
    let diff = transcript::tool_diff(call, whole)
        .or_else(|| transcript::tool_diff(call, None))
        .ok_or_else(missing)?;
    let text = serde_json::to_string(&diff).map_err(|e| RpcError::Failed(e.to_string()))?;
    Ok(blob(&text))
}
