//! What the shell fetches of a thread beyond its transcript: a tool call's whole output, an edit's
//! whole diff, the pictures of its messages and calls, and the visuals its calls showed, each read
//! through the thread's link to its whole ([`slopty_proto::thread::wire::ThreadRequest::Expand`])
//! when the worker cut it.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use gpui::{AppContext as _, Context, Task};
use slopty_client::WorkerKey;
use slopty_proto::thread::detail::ShellEdit;
use slopty_proto::thread::wire::Expanded;
use slopty_proto::thread::{ContentRef, ItemBody, ThreadId, ToolCall, ToolDetail};
use slopty_view_rpc::{RpcError, RpcReply};

use super::param;
use crate::view_model::transcript::{self, VisualRead};
use crate::view_model::work;
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
    /// (`{thread}/{item}.diff`) as the shell draws one ([`transcript::tool_diff`]), or a file's
    /// a command changed (`{thread}/{item}:edit:{n}.diff`), or the text the model read of its
    /// result ([`transcript::SEEN_BLOB`]), or its records as one JSON array
    /// ([`transcript::RAW_BLOB`]), each whole once its worker sent what it had cut.
    pub(super) fn view_tool_blob(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some((chat, item)) = param(params, "blobRef").and_then(|blob| blob.split_once('/'))
        else {
            return Task::ready(Err(missing()));
        };
        let Some((key, thread)) = self.chat_thread(chat, cx) else {
            return Task::ready(Err(missing()));
        };
        // What the model read of a call's result, and the call's own records.
        if let Some(item) = item.strip_suffix(transcript::SEEN_BLOB) {
            let Some(seen) = self.view_tool_call(key, thread, item, cx).and_then(|call| call.seen)
            else {
                return Task::ready(Err(missing()));
            };
            let Some(full) = seen.full.clone() else { return Task::ready(Ok(blob(&seen.text))) };
            let whole = Self::view_whole(key, thread, full, cx);
            return cx.background_spawn(async move {
                text_of(whole.await.as_ref()).map(|text| blob(&text)).ok_or_else(missing)
            });
        }
        if let Some(item) = item.strip_suffix(transcript::RAW_BLOB) {
            let Some(call) = self.view_tool_call(key, thread, item, cx) else {
                return Task::ready(Err(missing()));
            };
            let wholes: Vec<_> = call
                .raw
                .into_iter()
                .map(|record| Self::view_whole(key, thread, record, cx))
                .collect();
            return cx.background_spawn(async move {
                let mut records = Vec::with_capacity(wholes.len());
                for whole in wholes {
                    records.extend(text_of(whole.await.as_ref()));
                }
                // Each record is a JSON object: together, an array of them.
                (!records.is_empty())
                    .then(|| blob(&format!("[{}]", records.join(",\n"))))
                    .ok_or_else(missing)
            });
        }
        let (item, diff) = item.strip_suffix(".diff").map_or((item, false), |item| (item, true));
        // A file a command changed, drawn as an edit's chip under the command's.
        if diff && let Some((item, n)) = transcript::shell_edit_of(item) {
            let Some(edit) =
                self.view_tool_call(key, thread, item, cx).and_then(|call| match call.detail {
                    Some(ToolDetail::Exec(exec)) => exec.edits?.files.into_iter().nth(n),
                    _ => None,
                })
            else {
                return Task::ready(Err(missing()));
            };
            let Some(full) = edit.patch.full.clone() else {
                return Task::ready(edit_blob(&edit, None));
            };
            let whole = Self::view_whole(key, thread, full, cx);
            return cx.background_spawn(async move {
                let whole = text_of(whole.await.as_ref());
                edit_blob(&edit, whole.as_deref())
            });
        }
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
    pub(super) fn view_tool_call(
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
        Self::view_whole_within(key, thread, content, false, cx)
    }

    /// [`Self::view_whole`]; `patient`, it waits on while the link to `key` is down, asks again
    /// the moment it is back, and gives up only after [`WHOLE_WITHIN`] linked without an
    /// answer. What the worker answers, [`Expanded::Gone`] too, ends the wait.
    pub(super) fn view_whole_within(
        key: WorkerKey,
        thread: ThreadId,
        content: ContentRef,
        patient: bool,
        cx: &Context<Self>,
    ) -> Task<Option<Arc<Expanded>>> {
        cx.spawn(async move |this, cx| {
            let mut until = cx.background_executor().now().checked_add(WHOLE_WITHIN);
            loop {
                let (whole, linked) = this
                    .update(cx, |view, cx| {
                        let hub = view.held_hub(key)?.clone();
                        if let Some(whole) = hub.read(cx).whole(&content) {
                            return Some((Some(Arc::clone(whole)), true));
                        }
                        let linked = hub.read(cx).linked();
                        hub.update(cx, |hub, cx| hub.expand(thread, &content, cx));
                        Some((None, linked))
                    })
                    .ok()??;
                if whole.is_some() {
                    return whole;
                }
                let now = cx.background_executor().now();
                if patient && !linked {
                    // Out of reach is not slow: the wait starts again once the link is back.
                    until = now.checked_add(WHOLE_WITHIN);
                }
                if until.is_none_or(|until| now >= until) {
                    return None;
                }
                cx.background_executor().timer(WHOLE_EVERY).await;
            }
        })
    }

    /// `ReadAttachmentChunk` for a picture of a thread ([`transcript::image_path`]): a message's,
    /// a call's, or one pasted into its prompt box ([`work::DRAFT_ITEM`]); its bytes from
    /// `offset`, in base64, once its worker sent them.
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
            if item == work::DRAFT_ITEM {
                return Some(state.draft.as_ref()?.images.get(index)?.image.clone());
            }
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
            Ok(chunk(&name, &image.media_type, bytes, offset))
        })
    }

    /// `ReadAttachmentChunk` for a visual a call of a thread showed ([`transcript::visual_path`]):
    /// its script, the names of its files, or one of its files, from `offset`, in base64. The
    /// script is the call's own input (whole once its worker sent what it cut); a file is read on
    /// the worker by the path the call named ([`ContentRef::item_asset`]).
    pub(super) fn view_visual_chunk(
        &self,
        path: &str,
        offset: usize,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let gone = || RpcError::Failed("That visual is not here".to_owned());
        let Some((thread, item, read)) = transcript::visual_of(path) else {
            return Task::ready(Err(gone()));
        };
        let Some(key) = self.worker_of_thread(thread, cx) else {
            return Task::ready(Err(gone()));
        };
        let Some(call) = self.view_tool_call(key, thread, item, cx) else {
            return Task::ready(Err(gone()));
        };
        let Some(ToolDetail::Visual(visual)) = call.detail else {
            return Task::ready(Err(gone()));
        };
        let name = path.rsplit('/').next().unwrap_or_default().to_owned();
        let bytes: Task<Option<Vec<u8>>> = match read {
            VisualRead::Script => match visual.script.full.clone() {
                Some(full) => {
                    let whole = Self::view_whole(key, thread, full, cx);
                    cx.background_spawn(async move {
                        text_of(whole.await.as_ref()).map(String::into_bytes)
                    })
                }
                None => Task::ready(Some(visual.script.text.into_bytes())),
            },
            VisualRead::Assets => {
                let names: Vec<&String> = visual.assets.keys().collect();
                Task::ready(serde_json::to_vec(&names).ok())
            }
            VisualRead::Asset(asset) => {
                if !visual.assets.contains_key(asset) {
                    return Task::ready(Err(gone()));
                }
                let content = ContentRef::item_asset(item, asset);
                let whole = Self::view_whole(key, thread, content, cx);
                cx.background_spawn(async move {
                    match whole.await?.as_ref() {
                        Expanded::Bytes(bytes) => Some(bytes.clone()),
                        Expanded::Text(_) | Expanded::Gone => None,
                    }
                })
            }
        };
        cx.background_spawn(async move {
            let bytes = bytes.await.ok_or_else(gone)?;
            Ok(chunk(&name, "application/octet-stream", &bytes, offset))
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

/// The diff of a file a command changed as the shell's blob, from `whole` when its hunks were
/// cut and it came.
fn edit_blob(edit: &ShellEdit, whole: Option<&str>) -> Result<RpcReply, RpcError> {
    let diff = transcript::shell_edit_diff(edit, whole)
        .or_else(|| transcript::shell_edit_diff(edit, None))
        .ok_or_else(missing)?;
    let text = serde_json::to_string(&diff).map_err(|e| RpcError::Failed(e.to_string()))?;
    Ok(blob(&text))
}

/// One chunk of a read back: `bytes` from `offset`, at most [`IMAGE_CHUNK`] of them, in base64.
fn chunk(name: &str, mime: &str, bytes: &[u8], offset: usize) -> RpcReply {
    let size = bytes.len();
    let start = offset.min(size);
    let end = start.saturating_add(IMAGE_CHUNK).min(size);
    let data =
        base64::engine::general_purpose::STANDARD.encode(bytes.get(start..end).unwrap_or_default());
    RpcReply::Value(serde_json::json!({
        "name": name,
        "mimeType": mime,
        "data": data,
        "nextOffset": end,
        "done": end == size,
    }))
}
