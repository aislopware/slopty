//! The shell's calls about a session's files and settings: a picture a message shows, read back
//! from its machine; the project's actions; the calls of features Slopty leaves to the agents or
//! does not have; and a chat's config as its thread runs it. slopty: no prompt box, so no
//! commands, skills, `@` files or uploads are offered to one.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::{Context, Task};
use slopty_client::WorkerKey;
use slopty_proto::file::FileRead;
use slopty_proto::thread::AgentId;
use slopty_proto::thread::wire::ThreadRow;
use slopty_view_proto::{ChatConfig, HarnessId, SandboxLevel};
use slopty_view_rpc::{RpcError, RpcReply, methods};

use super::{param, value, within};
use crate::view_model::transcript;
use crate::workspace::WorkspaceView;

/// How long a call about a machine's agents waits for the machine to say what it has: an app
/// just opened asks before its links are up.
const AGENTS_WITHIN: Duration = Duration::from_secs(15);

/// The attached files the shell reads back (a picture in a message), each read whole once from
/// its machine and kept while its chunks are asked, the latest few.
#[derive(Debug, Default)]
pub(super) struct ReadBack(VecDeque<((WorkerKey, String), Attached)>);

/// An attached file read back: its media type and bytes.
#[derive(Clone, Debug)]
struct Attached {
    mime: String,
    bytes: bytes::Bytes,
}

/// How many attached files are kept while they are read back.
const READ_BACK_KEPT: usize = 4;

/// How much of an attached file one chunk of a read back carries: 45 KiB once in base64.
const READ_BACK_CHUNK: usize = 45 * 1024 / 4 * 3;

/// The shell's harness for `agent`.
fn harness_of(agent: &AgentId) -> HarnessId {
    if agent.is(AgentId::CODEX) {
        HarnessId::CODEX
    } else if agent.is(AgentId::PI) {
        HarnessId::PI
    } else if let Some(name) = agent.acp_name() {
        HarnessId::named(name)
    } else {
        HarnessId::CLAUDE_CODE
    }
}

/// The option the picker shows an agent's modes as (Claude Code's permission modes, an ACP
/// agent's session modes): switched through the agent's own `SetMode`, started in by
/// `Start::mode`, never sent as an option.
pub(in crate::workspace) const MODE_OPTION: &str = "mode";

/// The row of `offered` a thread running `model` (an agent's full id, `claude-opus-5-5`) is
/// on: its own id, else the alias it is of (`opus`), a `[1m]` row only for a 1M model.
pub(in crate::workspace) fn alias_in(
    offered: &[slopty_proto::thread::Model],
    model: &str,
) -> Option<String> {
    let model = model.to_ascii_lowercase();
    if let Some(row) = offered.iter().find(|row| row.id.eq_ignore_ascii_case(&model)) {
        return Some(row.id.clone());
    }
    let wide = model.contains("[1m]") || model.ends_with("-1m");
    offered
        .iter()
        .filter(|row| !row.id.eq_ignore_ascii_case("default"))
        .find(|row| {
            let (base, row_wide) = match row.id.strip_suffix("[1m]") {
                Some(base) => (base, true),
                None => (row.id.as_str(), false),
            };
            row_wide == wide && !base.is_empty() && model.contains(&base.to_ascii_lowercase())
        })
        .map(|row| row.id.clone())
}

impl WorkspaceView {
    /// One of these calls; `None` for one it does not make.
    pub(super) fn composer_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        Some(match method {
            methods::READ_ATTACHMENT_CHUNK => self.read_attachment(params, cx),
            methods::FOCUS_CHAT | methods::PROBE_SYNC | methods::RELEASE_HOME_FOLDER_SEARCH => {
                Task::ready(value(&serde_json::json!({})))
            }
            methods::LIST_PROJECT_ACTIONS => self.list_project_actions(params, cx),
            methods::RUN_PROJECT_ACTION => self.run_project_action(params, cx),
            methods::UPSERT_PROJECT_ACTION => self.upsert_project_action(params, cx),
            methods::DELETE_PROJECT_ACTION => self.delete_project_action(params, cx),
            _ => return None,
        })
    }

    /// `answer` to a call about a machine's agents, once the machine said what it has, or at
    /// most [`AGENTS_WITHIN`] after: answered before, it would say no agent is there.
    pub(super) fn once_known(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
        answer: impl Fn(&Self, &serde_json::Value, &gpui::App) -> Result<RpcReply, RpcError> + 'static,
    ) -> Task<Result<RpcReply, RpcError>> {
        if self.agents_known(params) {
            return Task::ready(answer(self, params, cx));
        }
        let params = params.clone();
        let until = Instant::now().checked_add(AGENTS_WITHIN).unwrap_or_else(Instant::now);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let answered = this
                    .update(cx, |view, cx| {
                        (view.agents_known(&params) || Instant::now() >= until)
                            .then(|| answer(view, &params, cx))
                    })
                    .map_err(super::gone)?;
                if let Some(answered) = answered {
                    return answered;
                }
            }
        })
    }

    /// Whether the machine a call names (the first linked one when it names none) said what
    /// agents it has.
    fn agents_known(&self, params: &serde_json::Value) -> bool {
        let key = self.surface_device(params).or_else(|| {
            self.workers.iter().find(|(_, worker)| worker.link.is_some()).map(|(key, _)| *key)
        });
        key.and_then(|key| self.workers.get(&key))
            .is_some_and(|worker| worker.link.is_some() && worker.caps.is_some())
    }

    /// The machine a composer's call names: `targetDeviceId`, else this Mac's own worker,
    /// else the one in front.
    fn composer_worker(&self, params: &serde_json::Value) -> Option<WorkerKey> {
        self.surface_device(params).or_else(|| self.context_worker())
    }

    /// What the thread `row` on `device` runs, as the shell's chat config: its agent, its model
    /// by its own id (Claude's by the id its alias answers for), its effort and its mode.
    pub(in crate::workspace) fn chat_config(&self, device: &str, row: &ThreadRow) -> ChatConfig {
        let model = row.meters.model_id.as_deref().map(|id| {
            let listed = if row.agent.is(AgentId::CLAUDE_CODE) {
                super::catalog::claude_id(id).map(str::to_owned)
            } else {
                self.view_worker(device)
                    .and_then(|key| self.workers.get(&key))
                    .and_then(|worker| {
                        let caps = worker.caps.as_ref()?;
                        caps.agents.iter().find(|a| a.agent == row.agent)
                    })
                    .and_then(|installed| alias_in(&installed.offers.models, id))
            };
            listed.unwrap_or_else(|| id.to_owned())
        });
        ChatConfig {
            harness: harness_of(&row.agent),
            model,
            reasoning: row
                .meters
                .effort
                .as_deref()
                .and_then(|effort| serde_json::from_value(serde_json::json!(effort)).ok()),
            model_options: row
                .meters
                .mode
                .clone()
                .filter(|mode| !mode.trim().is_empty())
                .map(|mode| {
                    serde_json::Map::from_iter([(
                        MODE_OPTION.to_owned(),
                        serde_json::Value::String(mode),
                    )])
                })
                .unwrap_or_default(),
            sandbox: SandboxLevel::WorkspaceWrite,
        }
    }

    /// `ReadAttachmentChunk`: the attached file `path` on its machine, from `offset`, in base64:
    /// a picture shown in a message.
    fn read_attachment(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        // A picture of a thread's is read through the thread, not as a file.
        if let Some(path) =
            param(params, "path").filter(|path| path.starts_with(transcript::IMAGE_SCHEME))
        {
            let offset = params.get("offset").and_then(serde_json::Value::as_u64).unwrap_or(0);
            let offset = usize::try_from(offset).unwrap_or(usize::MAX);
            return self.view_image_chunk(path, offset, cx);
        }
        // So is a visual a call of a thread's showed: its script and its files.
        if let Some(path) =
            param(params, "path").filter(|path| path.starts_with(transcript::VISUAL_SCHEME))
        {
            let offset = params.get("offset").and_then(serde_json::Value::as_u64).unwrap_or(0);
            let offset = usize::try_from(offset).unwrap_or(usize::MAX);
            return self.view_visual_chunk(path, offset, cx);
        }
        let (Some(key), Some(path)) = (self.composer_worker(params), param(params, "path")) else {
            return Task::ready(Err(RpcError::BadParams("a machine and path".to_owned())));
        };
        let at = (key, path.to_owned());
        let offset = params.get("offset").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let kept = self.view_model.surfaces.read_back.0.iter().find(|(id, _)| *id == at);
        let kept = kept.map(|(_, attached)| attached.clone()).filter(|_| offset > 0);
        let read = kept.is_none().then(|| self.surface_read(key, at.1.clone()));
        cx.spawn(async move |this, cx| {
            let attached = match (kept, read) {
                (Some(attached), _) => attached,
                (None, Some(read)) => {
                    let attached = attached(&at.1, within(cx, read).await?)?;
                    let keep = (at.clone(), attached.clone());
                    this.update(cx, |view, _| {
                        let kept = &mut view.view_model.surfaces.read_back.0;
                        kept.retain(|(id, _)| *id != keep.0);
                        if kept.len() >= READ_BACK_KEPT {
                            kept.pop_front();
                        }
                        kept.push_back(keep);
                    })
                    .map_err(super::gone)?;
                    attached
                }
                (None, None) => return Err(RpcError::Failed("nothing was read".to_owned())),
            };
            let size = attached.bytes.len();
            let start = offset.min(size);
            let end = start.saturating_add(READ_BACK_CHUNK).min(size);
            let name = at.1.rsplit('/').next().unwrap_or_default().to_owned();
            value(&serde_json::json!({
                "name": name,
                "mimeType": attached.mime,
                "data": super::files::base64(attached.bytes.get(start..end).unwrap_or_default()),
                "nextOffset": end,
                "done": end == size,
            }))
        })
    }
}

/// The attached file `path` as its read gave it: a picture or document by its media type, a
/// text file as plain text; any other is not read back.
fn attached(path: &str, read: FileRead) -> Result<Attached, RpcError> {
    match read {
        FileRead::Media { media_type, bytes, .. } => Ok(Attached { mime: media_type, bytes }),
        FileRead::Text { mut text, final_newline, .. } => {
            if final_newline {
                text.push('\n');
            }
            Ok(Attached { mime: "text/plain".to_owned(), bytes: bytes::Bytes::from(text) })
        }
        FileRead::Missing { error } => Err(RpcError::Failed(error)),
        _ => Err(RpcError::Failed(format!("{path} cannot be read back"))),
    }
}
