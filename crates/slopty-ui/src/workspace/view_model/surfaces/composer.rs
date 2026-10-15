//! The composer's calls: the models, commands and files it offers, the branch a project's
//! clone is on, a file attached to a message, and the calls of features Slopty leaves to the
//! agents or does not have.
//!
//! What an agent can start with is what its worker says it offers
//! ([`slopty_proto::server::InstalledAgent::offers`]); a file attached is sent to the worker
//! into a fresh drop folder ([`slopty_proto::transfer::Dest::Attachment`]) and its path goes
//! with the message. A message held for the turn's end waits in the worker's queue for the
//! thread, which the shell shows and edits, sends now or withdraws; a side chat is the agent's
//! own aside of the chat ([`Intent::Aside`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use base64::Engine as _;
use gpui::{AppContext as _, Context, Task};
use slopty_client::WorkerKey;
use slopty_core::XferId;
use slopty_proto::ClientMsg;
use slopty_proto::file::FileRead;
use slopty_proto::git::{GitDone, GitOp, GitOutcome};
use slopty_proto::thread::wire::{Intent, ThreadRequest, ThreadRow};
use slopty_proto::thread::{AgentId, Offers};
use slopty_proto::transfer::Dest;
use slopty_view_proto::{
    ChatConfig, FileSearchMatch, HarnessId, Model, ReasoningLevel, RepoRef, SandboxLevel,
    SlashCommand,
};
use slopty_view_rpc::{RpcError, RpcReply, methods};

use super::super::{agent_of, agent_prompt};
use super::{param, value, within};
use crate::view_model::transcript;
use crate::workspace::WorkspaceView;

/// How many files an `@` search answers with.
const FILE_MATCHES: usize = 50;

/// How long a call about a machine's agents waits for the machine to say what it has: an app
/// just opened asks before its links are up.
const AGENTS_WITHIN: Duration = Duration::from_secs(15);

/// The largest file attached to a message.
const ATTACHMENT_MAX: usize = 64 << 20;

/// How long a side chat may take to start: its agent branches the whole thread first.
const ASIDE_WITHIN: Duration = Duration::from_secs(60);

/// The files being attached, by the shell's upload id: their parts as they came, by number.
#[derive(Debug, Default)]
pub(super) struct Uploads(HashMap<String, Upload>);

/// The model and effort the person picked for each chat, by its id, until its thread runs
/// them or turns the switch down; a chat they never picked for shows what its thread runs.
#[derive(Debug, Default)]
pub(super) struct Configs(HashMap<String, Picked>);

/// A chat's pick, and the switches it asked of the chat's thread.
#[derive(Debug)]
struct Picked {
    config: ChatConfig,
    /// The thread and the intents that switch it; a refused one drops the pick.
    asked: Option<(WorkerKey, slopty_proto::thread::ThreadId, Vec<slopty_proto::thread::IntentId>)>,
}

/// The attached files the shell reads back (a picture in a message, a queued message restored
/// for editing), each read whole once from its machine and kept while its chunks are asked,
/// the latest few.
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

/// One file being attached.
#[derive(Debug, Default)]
struct Upload {
    /// The machine it goes to.
    worker: Option<WorkerKey>,
    /// Its parts, base64, by number.
    parts: BTreeMap<u64, String>,
    /// Their size so far, base64.
    size: usize,
}

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

/// What a queued message says and the files it names, as one hash an edit lease compares: the
/// lease lives in this app only, so the hash need not outlive it.
fn text_hash(text: &str, attachments: &[String]) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    attachments.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// A call refused before it is asked of a machine.
fn fail(error: impl Into<String>) -> Task<Result<RpcReply, RpcError>> {
    Task::ready(Err(RpcError::Failed(error.into())))
}

/// An agent's offered models as the shell's, each with the efforts it offers.
fn models(offers: &Offers, harness: HarnessId) -> Vec<Model> {
    let efforts: Vec<ReasoningLevel> = offers
        .efforts
        .iter()
        .filter_map(|effort| serde_json::from_value(serde_json::json!(effort.id)).ok())
        .collect();
    let efforts = if efforts.is_empty() { super::settings::efforts(harness) } else { efforts };
    offers
        .models
        .iter()
        .map(|model| Model {
            id: model.id.clone(),
            label: model.label.clone(),
            description: None,
            reasoning_levels: efforts.clone(),
            options: Vec::new(),
        })
        .collect()
}

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

/// An agent's skills as the shell's, for its `$` menu: each by the command that runs it, which
/// is how the agent takes it (`invocation::harness_prompt`), so the identity is the agent's own
/// (`harness-skill:`), never a file this app reads.
fn skills(offers: &Offers, harness: HarnessId) -> Vec<slopty_view_proto::invocation::Skill> {
    use slopty_view_proto::invocation::{Skill, SkillCommand};
    offers
        .commands
        .iter()
        .filter(|command| command.skill)
        .map(|command| Skill {
            name: command.name.clone(),
            path: format!("harness-skill:{harness}:{}", command.name),
            description: command.description.clone(),
            enabled: true,
            command: Some(SkillCommand { name: command.name.clone(), harness }),
        })
        .collect()
}

/// An agent's commands as the shell's.
fn commands(offers: &Offers) -> Vec<SlashCommand> {
    offers
        .commands
        .iter()
        .map(|command| SlashCommand {
            name: command.name.clone(),
            description: command.description.clone(),
            input_hint: command.argument_hint.clone(),
        })
        .collect()
}

/// A clone's branches as the shell's refs: the one checked out first, then the clone's own,
/// then those only `origin` has.
fn refs(branches: &slopty_proto::git::Branches) -> Vec<RepoRef> {
    let current = branches.current.as_deref();
    let mut refs: Vec<(u8, RepoRef)> = branches
        .list
        .iter()
        .map(|branch| {
            let here = current == Some(branch.name.as_str());
            let rank = if here {
                0
            } else if branch.local {
                1
            } else {
                2
            };
            (rank, RepoRef { name: branch.name.clone(), current: here, worktree_path: None })
        })
        .collect();
    refs.sort_by_key(|(rank, _)| *rank);
    refs.into_iter().map(|(_, r)| r).collect()
}

impl WorkspaceView {
    /// A composer's call; `None` for one it does not make.
    pub(super) fn composer_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        Some(match method {
            methods::LIST_MODELS => self
                .once_known(params, cx, |view, params, cx| value(&view.listed_models(params, cx))),
            methods::LIST_COMMANDS => self.once_known(params, cx, |view, params, _| {
                let offered = view.offered_to(params);
                value(
                    &offered.map(|(installed, ..)| commands(&installed.offers)).unwrap_or_default(),
                )
            }),
            methods::LIST_SKILLS => self.once_known(params, cx, |view, params, _| {
                let offered = view.offered_to(params);
                value(
                    &offered
                        .map(|(installed, harness, _)| skills(&installed.offers, harness))
                        .unwrap_or_default(),
                )
            }),
            methods::SEARCH_FILES => self.find_mentioned(params, cx),
            methods::LIST_REFS => self.list_refs(params, cx),
            methods::SWITCH_REF => self.switch_ref(params, cx),
            methods::UPLOAD_CHUNK => Task::ready(self.upload_chunk(params)),
            methods::UPLOAD_COMMIT => self.upload_commit(params, cx),
            methods::READ_ATTACHMENT_CHUNK => self.read_attachment(params, cx),
            methods::FOCUS_CHAT | methods::PROBE_SYNC | methods::RELEASE_HOME_FOLDER_SEARCH => {
                Task::ready(value(&serde_json::json!({})))
            }
            methods::LIST_PROJECT_ACTIONS => self.list_project_actions(params, cx),
            methods::RUN_PROJECT_ACTION => self.run_project_action(params, cx),
            methods::TAKE_PROJECT_ACTION_SETUP => self.take_project_action_setup(params, cx),
            methods::UPSERT_PROJECT_ACTION => self.upsert_project_action(params, cx),
            methods::DELETE_PROJECT_ACTION => self.delete_project_action(params, cx),
            methods::APPLY_HARNESS_UPDATE
            | methods::CANCEL_HARNESS_UPDATE
            | methods::SET_HARNESS_UPDATE_POLICY => {
                fail("An agent updates itself on its machine; Slopty installs and updates none")
            }
            methods::BEGIN_QUEUED_MESSAGE_EDIT
            | methods::RENEW_QUEUED_MESSAGE_EDIT
            | methods::FINISH_QUEUED_MESSAGE_EDIT
            | methods::UPDATE_QUEUED_MESSAGE
            | methods::MOVE_QUEUED_MESSAGE
            | methods::REMOVE_QUEUED_MESSAGE
            | methods::SEND_QUEUED_MESSAGE_NOW
            | methods::STEER_QUEUED_MESSAGE_NOW => Task::ready(self.queue_call(method, params, cx)),
            methods::FORK_SIDE_CHAT => self.fork_side_chat(params, cx),
            methods::RETRY_DELIVERY => Task::ready(self.retry_delivery(params, cx)),
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

    /// The agent of `params.harness` as the machine the call names has it, its harness, and
    /// the machine; nothing where the agent is not there.
    fn offered_to(
        &self,
        params: &serde_json::Value,
    ) -> Option<(&slopty_proto::server::InstalledAgent, HarnessId, WorkerKey)> {
        let harness: Option<HarnessId> =
            params.get("harness").cloned().and_then(|h| serde_json::from_value(h).ok());
        let agent = agent_of(harness);
        let key = self.composer_worker(params)?;
        let worker = self.workers.get(&key)?;
        let installed =
            worker.caps.iter().flat_map(|caps| &caps.agents).find(|a| a.agent == agent)?;
        Some((installed, harness.unwrap_or(HarnessId::CLAUDE_CODE), key))
    }

    /// The model the latest thread of `agent` on `key` runs, by the agent's id for it.
    fn latest_model(&self, key: WorkerKey, agent: &AgentId, cx: &gpui::App) -> Option<String> {
        let hub = self.held_hub(key)?.read(cx);
        let rows = &hub.threads().rows().rows;
        let latest = rows
            .values()
            .filter(|row| &row.agent == agent && row.meters.model_id.is_some())
            .max_by_key(|row| row.updated_ms)?;
        latest.meters.model_id.clone()
    }

    /// `ListModels`: the models the agent offers on the machine, the one a new chat starts on
    /// first: the model its latest thread there runs, else, for Claude Code, Opus, its own
    /// default on the plans that offer it. Claude's are its current models by id
    /// ([`super::catalog`]), as the installed Claude Code runs them.
    fn listed_models(&self, params: &serde_json::Value, cx: &gpui::App) -> Vec<Model> {
        let Some((installed, harness, key)) = self.offered_to(params) else {
            return Vec::new();
        };
        let latest = self.latest_model(key, &installed.agent, cx);
        let (mut listed, first) = if installed.agent.is(AgentId::CLAUDE_CODE) {
            let latest = latest.as_deref().and_then(super::catalog::claude_id);
            let menu: Vec<String> = installed.offers.models.iter().map(|m| m.id.clone()).collect();
            let listed = super::catalog::claude_menu(&installed.version, &menu, latest);
            let first = latest.or_else(|| super::catalog::claude_id("opus"));
            (listed, first.map(str::to_owned))
        } else {
            let first = latest.and_then(|model| alias_in(&installed.offers.models, &model));
            (models(&installed.offers, harness), first)
        };
        if listed.is_empty() {
            listed = models(&installed.offers, harness);
        }
        if let Some(at) = first.and_then(|first| listed.iter().position(|m| m.id == first)) {
            let row = listed.remove(at);
            listed.insert(0, row);
        }
        listed
    }

    /// `SearchFiles`: the files under the chat's or the project's folder an `@` names.
    fn find_mentioned(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let place = self.surface_place(params, cx).ok().or_else(|| {
            let root = param(params, "path").or_else(|| param(params, "cwd"))?;
            Some((self.composer_worker(params)?, root.to_owned()))
        });
        let Some((key, root)) = place else {
            return Task::ready(Err(RpcError::BadParams("no folder to search".to_owned())));
        };
        let root = param(params, "path").map_or(root, str::to_owned);
        let query = param(params, "query").unwrap_or_default().to_owned();
        let rx = self.surface_find(key, root, query);
        cx.spawn(async move |_, cx| {
            let paths = within(cx, rx).await?;
            let found: Vec<FileSearchMatch> = paths
                .into_iter()
                .take(FILE_MATCHES)
                .map(|path| match path.strip_suffix('/') {
                    Some(dir) => FileSearchMatch { path: dir.to_owned(), is_dir: true },
                    None => FileSearchMatch { path, is_dir: false },
                })
                .collect();
            value(&found)
        })
    }

    /// `ListRefs`: the branches of the clone at `repoPath`.
    fn list_refs(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (Some(key), Some(repo)) = (self.composer_worker(params), param(params, "repoPath"))
        else {
            return Task::ready(Err(RpcError::BadParams("a machine and repoPath".to_owned())));
        };
        let rx = self.surface_git(key, repo.to_owned(), GitOp::Branches);
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            GitOutcome::Done(GitDone::Branches(branches)) => value(&refs(&branches)),
            outcome => Err(git_failure(outcome)),
        })
    }

    /// `SwitchRef`: check out `refName` in the clone at `repoPath`.
    fn switch_ref(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (Some(key), Some(repo), Some(branch)) =
            (self.composer_worker(params), param(params, "repoPath"), param(params, "refName"))
        else {
            return Task::ready(Err(RpcError::BadParams("a machine, repoPath and refName".into())));
        };
        let op = GitOp::Switch { branch: branch.to_owned() };
        let rx = self.surface_git(key, repo.to_owned(), op);
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            GitOutcome::Done(GitDone::Switched { branch }) => {
                value(&serde_json::json!({ "refName": branch }))
            }
            outcome => Err(git_failure(outcome)),
        })
    }

    /// What the thread `row` on `device` runs, as the shell's chat config: its agent, its model
    /// by the id the picker lists it by, its effort.
    fn thread_config(&self, device: &str, row: &ThreadRow) -> ChatConfig {
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
            model_options: serde_json::Map::new(),
            sandbox: SandboxLevel::WorkspaceWrite,
        }
    }

    /// The chat `chat`'s config as the shell shows it: what the person picked for it, else what
    /// its thread `row` on `device` runs.
    pub(in crate::workspace) fn chat_config(
        &self,
        chat: &str,
        device: &str,
        row: &ThreadRow,
    ) -> ChatConfig {
        let picked = self.view_model.surfaces.configs.0.get(chat).map(|p| p.config.clone());
        picked.unwrap_or_else(|| self.thread_config(device, row))
    }

    /// The model and effort the person picked for the chat `chat`, if they did.
    pub(in crate::workspace) fn picked_config(&self, chat: &str) -> Option<ChatConfig> {
        self.view_model.surfaces.configs.0.get(chat).map(|p| p.config.clone())
    }

    /// The person picked `config` for the chat `chat` before it has a thread: it starts with it.
    pub(in crate::workspace) fn pick_config(&mut self, chat: &str, config: ChatConfig) {
        self.view_model.surfaces.configs.0.insert(chat.to_owned(), Picked { config, asked: None });
    }

    /// Drop each pick the chat's thread settled: one whose switch it turned down, so the chat
    /// shows what it runs again beside the refusal its transcript says, and one its meters say
    /// it runs now, so what it runs from then on (a switch in its terminal) shows as it moves.
    pub(in crate::workspace) fn settle_configs(&mut self, cx: &Context<Self>) {
        let settled: Vec<String> = self
            .view_model
            .surfaces
            .configs
            .0
            .iter()
            .filter(|(chat, picked)| {
                if let Some((key, thread, asked)) = &picked.asked
                    && self.held_hub(*key).is_some_and(|hub| {
                        hub.read(cx).refusals(*thread).any(|refusal| asked.contains(&refusal.id))
                    })
                {
                    return true;
                }
                let Some((key, thread)) = self.chat_thread(chat, cx) else { return false };
                let Some(hub) = self.held_hub(key) else { return false };
                let Some(row) = hub.read(cx).threads().rows().rows.get(&thread) else {
                    return false;
                };
                let runs = self.thread_config(&key.to_string(), row);
                let model = picked.config.model.is_none() || picked.config.model == runs.model;
                let effort =
                    picked.config.reasoning.is_none() || picked.config.reasoning == runs.reasoning;
                model && effort
            })
            .map(|(chat, _)| chat.clone())
            .collect();
        for chat in settled {
            self.view_model.surfaces.configs.0.remove(&chat);
        }
    }

    /// `setChatConfig`: the model and effort the person picked for a chat, kept for it and,
    /// where it has a thread, switched there through its agent's own door (`SetModel`,
    /// `SetEffort`) when they differ from what it runs. `None` for every other operation.
    pub(in crate::workspace) fn config_mutate(
        &mut self,
        op: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Result<serde_json::Value, RpcError>> {
        if op != "setChatConfig" {
            return None;
        }
        let config: Option<ChatConfig> =
            params.get("config").cloned().and_then(|c| serde_json::from_value(c).ok());
        let (Some(chat), Some(config)) = (param(params, "chatId"), config) else {
            return Some(Err(RpcError::BadParams("chatId and config".to_owned())));
        };
        let chat = chat.to_owned();
        let asked = if let Some((key, thread)) = self.chat_thread(&chat, cx) {
            let hub = self.thread_hub(key, cx);
            let shown = {
                let hub = hub.read(cx);
                let row = hub.threads().rows().rows.get(&thread).cloned();
                row.map(|row| self.chat_config(&chat, &key.to_string(), &row))
            };
            let mut intents = Vec::new();
            if let Some(model) = config.model.clone()
                && shown.as_ref().and_then(|s| s.model.as_ref()) != Some(&model)
            {
                intents.push(Intent::SetModel { model });
            }
            if let Some(level) = config.reasoning
                && shown.as_ref().and_then(|s| s.reasoning) != Some(level)
                && let Some(effort) = serde_json::to_value(level)
                    .ok()
                    .and_then(|level| level.as_str().map(str::to_owned))
            {
                intents.push(Intent::SetEffort { effort });
            }
            let ids = intents
                .into_iter()
                .map(|intent| hub.update(cx, |hub, cx| hub.intent(thread, intent, cx)))
                .collect();
            Some((key, thread, ids))
        } else {
            None
        };
        self.view_model.surfaces.configs.0.insert(chat, Picked { config, asked });
        self.view_changed(cx);
        Some(Ok(serde_json::json!({})))
    }

    /// `RetryDelivery`: what this client asked of the chat's thread (or, for a chat not started
    /// yet, its start) and the worker has not answered, sent again under the same ids, so each is
    /// still acted on once.
    fn retry_delivery(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Result<RpcReply, RpcError> {
        let chat = param(params, "chatId").ok_or_else(|| RpcError::BadParams("chatId".into()))?;
        // A start turned down goes again, under a new id.
        if self.view_retry_start(chat, cx) {
            return value(&serde_json::json!({ "retried": 1 }));
        }
        if let Some(minted) = self.view_model.minted.get(chat) {
            let retried = self.view_resend(minted.worker, Some(chat));
            return value(&serde_json::json!({ "retried": retried }));
        }
        let (key, thread) = self
            .chat_thread(chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
        let (_, hub) = self
            .hubs()
            .find(|(at, _)| *at == key)
            .ok_or_else(|| RpcError::Failed("the machine is not connected".to_owned()))?;
        let unanswered: Vec<ClientMsg> = hub
            .read(cx)
            .threads()
            .outbox()
            .of(thread)
            .filter(|sent| sent.outcome.is_none())
            .map(|sent| {
                let (id, intent) = (sent.id, sent.intent.clone());
                ClientMsg::Thread(ThreadRequest::Intent { id, thread, intent })
            })
            .collect();
        let retried = unanswered.len();
        for msg in unanswered {
            self.send(key, msg);
        }
        value(&serde_json::json!({ "retried": retried }))
    }

    /// `ReadAttachmentChunk`: the attached file `path` on its machine, from `offset`, in base64:
    /// a picture shown in a message, a queued message's file restored for editing.
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

    /// `forkSideChat`: a side chat, asked aside of a chat (`sourceChatId`) through its agent's
    /// own door ([`Intent::Aside`]): the whole thread branched into one marked as its aside,
    /// which shares its context and stays out of the lists. It goes by the shell's `chatId`,
    /// under `parentChatId`, and is answered once it started, as the chat it is.
    fn fork_side_chat(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (Some(chat), Some(source)) = (param(params, "chatId"), param(params, "sourceChatId"))
        else {
            return Task::ready(Err(RpcError::BadParams("chatId and sourceChatId".to_owned())));
        };
        let (chat, parent) = (chat.to_owned(), param(params, "parentChatId").map(str::to_owned));
        let Some((key, thread)) = self.chat_thread(source, cx) else {
            return Task::ready(Err(RpcError::Failed(format!("no thread {source}"))));
        };
        let id = slopty_proto::thread::IntentId::new();
        self.view_model.starts.insert(id, chat.clone());
        let intent = Intent::Aside;
        self.view_send_kept(
            key,
            id,
            ClientMsg::Thread(ThreadRequest::Intent { id, thread, intent }),
        );
        let until = Instant::now().checked_add(ASIDE_WITHIN).unwrap_or_else(Instant::now);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let answered = this
                    .update(cx, |view, cx| view.side_chat(id, &chat, parent.as_deref(), until, cx))
                    .map_err(super::gone)?;
                if let Some(answered) = answered {
                    return answered;
                }
            }
        })
    }

    /// The side chat `chat` asked by intent `id`, under `parent`, once its aside started: `None`
    /// while it is starting, an error once it did not start or `until` passed.
    fn side_chat(
        &mut self,
        id: slopty_proto::thread::IntentId,
        chat: &str,
        parent: Option<&str>,
        until: Instant,
        cx: &mut Context<Self>,
    ) -> Option<Result<RpcReply, RpcError>> {
        let started =
            self.view_model.aliases.iter().find(|(_, alias)| *alias == chat).map(|(t, _)| *t);
        let shown = started.and_then(|thread| {
            let key = self.worker_of_thread(thread, cx)?;
            let hub = self.thread_hub(key, cx);
            let row = hub.read(cx).threads().rows().rows.get(&thread).cloned();
            Some(key).zip(row)
        });
        let Some((key, row)) = shown else {
            if started.is_none() && !self.view_model.starts.contains_key(&id) {
                return Some(Err(RpcError::Failed("The side chat did not start".to_owned())));
            }
            if Instant::now() >= until {
                self.view_model.starts.remove(&id);
                self.view_model.unanswered.remove(&id);
                return Some(Err(RpcError::Failed("The side chat took too long".to_owned())));
            }
            return None;
        };
        let device = key.to_string();
        let mut side = crate::view_model::rows::chat(&row, &device);
        side.space_id =
            crate::view_model::rows::folder(&row).map(|path| self.view_space_id(&device, path));
        side.config = Some(self.chat_config(chat, &device, &row));
        chat.clone_into(&mut side.id);
        side.parent_chat_id = parent.map(str::to_owned).or(side.parent_chat_id);
        Some(value(&side))
    }

    /// A call on a message waiting in a chat's queue, the thread's own (`Pending`): removed
    /// (`Withdraw`), changed with its files (`Edit`), sent now (`Promote`) or moved (`Move`).
    /// The composer's edit lease is the message itself, held while it waits: a commit over a
    /// message changed meanwhile conflicts.
    pub(in crate::workspace) fn queue_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<RpcReply, RpcError> {
        let chat = param(params, "chatId").ok_or_else(|| RpcError::BadParams("chatId".into()))?;
        let row = param(params, "id").ok_or_else(|| RpcError::BadParams("id".to_owned()))?;
        // A chat still starting holds its messages here until its thread is there.
        if self.view_model.minted.contains_key(chat) {
            return Ok(RpcReply::Value(self.held_call(method, chat, row, params)));
        }
        let pending =
            self.queued_intent(row).ok_or_else(|| RpcError::BadParams("id".to_owned()))?;
        let (key, thread) = self
            .chat_thread(chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
        let harness = self.thread_harness(key, thread, cx);
        let state = self.view_queue_state(key, thread, pending, cx);
        // A message being typed into the agent can no longer be changed, moved or taken back.
        if state == Some(slopty_proto::thread::PendingState::Sending)
            && method != methods::RENEW_QUEUED_MESSAGE_EDIT
        {
            return Err(RpcError::Failed("That message is going to the agent now".to_owned()));
        }
        let waiting = state.is_some();
        let text = param(params, "text").map(str::to_owned);
        let queued = || self.view_queue(chat, cx).into_iter().find(|m| m.id == row);
        // The files a change names; one that names none keeps the message's own.
        let attachments = params
            .get("attachments")
            .and_then(|a| serde_json::from_value::<Vec<String>>(a.clone()).ok())
            .or_else(|| queued().map(|m| m.attachments))
            .unwrap_or_default();
        let (intent, answer) = match method {
            methods::REMOVE_QUEUED_MESSAGE => {
                (Some(Intent::Withdraw { pending }), serde_json::json!({ "removed": waiting }))
            }
            // Sent now: the turn stops and the message goes next.
            methods::SEND_QUEUED_MESSAGE_NOW => {
                if waiting {
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.intent(thread, Intent::Interrupt, cx));
                }
                (Some(Intent::Promote { pending }), serde_json::json!({ "sent": waiting }))
            }
            methods::STEER_QUEUED_MESSAGE_NOW => {
                (Some(Intent::Promote { pending }), serde_json::json!({ "sent": waiting }))
            }
            methods::UPDATE_QUEUED_MESSAGE => (
                text.map(|text| self.queued_edit(pending, text, harness, attachments)),
                serde_json::json!({ "changed": waiting }),
            ),
            // The lease is the message's own id; its hash lets a commit tell whether the message
            // changed meanwhile, from another device.
            methods::BEGIN_QUEUED_MESSAGE_EDIT => {
                let message = queued();
                let lease = match message.filter(|_| waiting) {
                    Some(m) => serde_json::json!({
                        "outcome": "acquired",
                        "leaseId": row,
                        "text": m.text,
                        "baseTextHash": text_hash(&m.text, &m.attachments),
                        "attachments": m.attachments,
                    }),
                    None => serde_json::json!({ "outcome": "missing" }),
                };
                (None, lease)
            }
            methods::RENEW_QUEUED_MESSAGE_EDIT => {
                (None, serde_json::json!({ "outcome": if waiting { "renewed" } else { "gone" } }))
            }
            methods::FINISH_QUEUED_MESSAGE_EDIT => {
                if param(params, "action") == Some("commit") {
                    let now = queued().map(|m| text_hash(&m.text, &m.attachments));
                    let outcome = match (now, param(params, "expectedTextHash")) {
                        (None, _) => "missing",
                        (Some(now), Some(expected)) if now != expected => "conflict",
                        (Some(_), _) => "committed",
                    };
                    let edit = text
                        .filter(|_| outcome == "committed")
                        .map(|text| self.queued_edit(pending, text, harness, attachments));
                    (edit, serde_json::json!({ "outcome": outcome }))
                } else {
                    (None, serde_json::json!({ "outcome": "cancelled" }))
                }
            }
            // `toIndex` is the place in the queue as the shell shows it, the moved one taken out.
            methods::MOVE_QUEUED_MESSAGE => {
                let to = params.get("toIndex").and_then(serde_json::Value::as_u64);
                let to = to.and_then(|to| usize::try_from(to).ok()).unwrap_or(usize::MAX);
                let others: Vec<String> = self
                    .view_queue(chat, cx)
                    .into_iter()
                    .map(|m| m.id)
                    .filter(|id| id != row)
                    .collect();
                let before = others.get(to).and_then(|id| self.queued_intent(id));
                (Some(Intent::Move { pending, before }), serde_json::json!({ "changed": waiting }))
            }
            _ => (None, serde_json::json!({ "changed": false })),
        };
        if waiting && let Some(intent) = intent {
            // An edit's own words go with it, for every device; empty words take back the old.
            let said = match &intent {
                Intent::Edit { pending, .. } => Some(Intent::Said {
                    message: *pending,
                    words: self.view_model.said.get(pending).cloned().unwrap_or_default(),
                }),
                _ => None,
            };
            let hub = self.thread_hub(key, cx);
            hub.update(cx, |hub, cx| {
                let _edit = hub.intent(thread, intent, cx);
                if let Some(said) = said {
                    let _said = hub.intent(thread, said, cx);
                }
            });
        }
        Ok(RpcReply::Value(answer))
    }

    /// The change of the queued message `pending` to `text` as the person wrote it: the agent
    /// is given its chips in its own words, and the queue keeps showing the person's.
    fn queued_edit(
        &mut self,
        pending: slopty_proto::thread::IntentId,
        text: String,
        harness: HarnessId,
        attachments: Vec<String>,
    ) -> Intent {
        let agent = agent_prompt(&text, harness);
        if agent == text {
            self.view_model.said.remove(&pending);
        } else {
            self.view_model.said.insert(pending, text);
        }
        Intent::Edit { pending, text: agent, attachments }
    }

    /// A call on a message a chat still starting holds for its thread ([`Self::queue_call`]):
    /// removed, changed, moved, or sent the moment the thread is there.
    fn held_call(
        &mut self,
        method: &str,
        chat: &str,
        row: &str,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let held = self.view_model.held.entry(chat.to_owned()).or_default();
        let Some(at) = held.iter().position(|h| h.queued && h.name == row) else {
            return match method {
                methods::BEGIN_QUEUED_MESSAGE_EDIT | methods::FINISH_QUEUED_MESSAGE_EDIT => {
                    serde_json::json!({ "outcome": "missing" })
                }
                methods::RENEW_QUEUED_MESSAGE_EDIT => serde_json::json!({ "outcome": "gone" }),
                methods::REMOVE_QUEUED_MESSAGE => serde_json::json!({ "removed": false }),
                methods::SEND_QUEUED_MESSAGE_NOW | methods::STEER_QUEUED_MESSAGE_NOW => {
                    serde_json::json!({ "sent": false })
                }
                _ => serde_json::json!({ "changed": false }),
            };
        };
        let text = param(params, "text").map(str::to_owned);
        let attachments = params
            .get("attachments")
            .and_then(|a| serde_json::from_value::<Vec<String>>(a.clone()).ok());
        match method {
            methods::REMOVE_QUEUED_MESSAGE => {
                held.remove(at);
                serde_json::json!({ "removed": true })
            }
            methods::SEND_QUEUED_MESSAGE_NOW | methods::STEER_QUEUED_MESSAGE_NOW => {
                if let Some(message) = held.get_mut(at) {
                    message.hold = false;
                }
                serde_json::json!({ "sent": true })
            }
            methods::UPDATE_QUEUED_MESSAGE => {
                if let (Some(message), Some(text)) = (held.get_mut(at), text) {
                    message.text = text;
                }
                serde_json::json!({ "changed": true })
            }
            methods::BEGIN_QUEUED_MESSAGE_EDIT => {
                let Some(message) = held.get(at) else { return serde_json::json!({}) };
                serde_json::json!({
                    "outcome": "acquired",
                    "leaseId": row,
                    "text": message.text,
                    "baseTextHash": text_hash(&message.text, &message.attachments),
                    "attachments": message.attachments,
                })
            }
            methods::RENEW_QUEUED_MESSAGE_EDIT => serde_json::json!({ "outcome": "renewed" }),
            methods::FINISH_QUEUED_MESSAGE_EDIT => {
                if param(params, "action") != Some("commit") {
                    return serde_json::json!({ "outcome": "cancelled" });
                }
                let Some(message) = held.get_mut(at) else { return serde_json::json!({}) };
                let now = text_hash(&message.text, &message.attachments);
                if param(params, "expectedTextHash").is_some_and(|expected| expected != now) {
                    return serde_json::json!({ "outcome": "conflict" });
                }
                if let Some(text) = text {
                    message.text = text;
                }
                if let Some(attachments) = attachments {
                    message.attachments = attachments;
                }
                serde_json::json!({ "outcome": "committed" })
            }
            methods::MOVE_QUEUED_MESSAGE => {
                let to = params.get("toIndex").and_then(serde_json::Value::as_u64);
                let to = to.and_then(|to| usize::try_from(to).ok()).unwrap_or(usize::MAX);
                let moved = held.remove(at);
                // The place among the queued messages, the moved one taken out.
                let before =
                    held.iter().enumerate().filter(|(_, h)| h.queued).nth(to).map(|(ix, _)| ix);
                held.insert(before.unwrap_or(held.len()), moved);
                serde_json::json!({ "changed": true })
            }
            _ => serde_json::json!({ "changed": false }),
        }
    }

    /// How `pending` stands in `thread`'s queue on `key`; `None` once it left.
    fn view_queue_state(
        &self,
        key: WorkerKey,
        thread: slopty_proto::thread::ThreadId,
        pending: slopty_proto::thread::IntentId,
        cx: &gpui::App,
    ) -> Option<slopty_proto::thread::PendingState> {
        let hub = self.held_hub(key)?.read(cx);
        let state = hub.threads().mirror(thread).and_then(slopty_client::threads::Mirror::state)?;
        state.pending.iter().find(|p| p.intent == pending).map(|p| p.state.clone())
    }

    /// `UploadChunk`: one part of a file being attached.
    fn upload_chunk(&mut self, params: &serde_json::Value) -> Result<RpcReply, RpcError> {
        let (Some(id), Some(seq), Some(data)) = (
            param(params, "uploadId"),
            params.get("seq").and_then(serde_json::Value::as_u64),
            params.get("data").and_then(serde_json::Value::as_str),
        ) else {
            return Err(RpcError::BadParams("uploadId, seq and data".to_owned()));
        };
        let worker = self.composer_worker(params);
        let upload = self.view_model.surfaces.uploads.0.entry(id.to_owned()).or_default();
        upload.worker = upload.worker.or(worker);
        let before = upload.parts.get(&seq).map_or(0, String::len);
        let size = upload.size.saturating_sub(before).saturating_add(data.len());
        if size.div_ceil(4).saturating_mul(3) > ATTACHMENT_MAX {
            self.view_model.surfaces.uploads.0.remove(id);
            return Err(RpcError::Failed("the file is too large to attach".to_owned()));
        }
        upload.size = size;
        upload.parts.insert(seq, data.to_owned());
        value(&serde_json::json!({}))
    }

    /// `UploadCommit`: the file whole, sent to its machine; the path it landed at there.
    fn upload_commit(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(id) = param(params, "uploadId") else {
            return Task::ready(Err(RpcError::BadParams("uploadId".to_owned())));
        };
        let Some(upload) = self.view_model.surfaces.uploads.0.remove(id) else {
            return fail("no file was sent for this attachment");
        };
        let name = param(params, "fileName").unwrap_or("attachment").replace(['/', '\\'], "_");
        let Some(key) = upload.worker.or_else(|| self.composer_worker(params)) else {
            return fail("no machine to attach the file on");
        };
        let Some(remote) =
            self.workers.get(&key).and_then(|w| w.link.as_ref()).and_then(|l| l.remote.clone())
        else {
            return fail("the machine is not connected");
        };
        cx.background_spawn(async move {
            let encoded: String = upload.parts.into_values().collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| RpcError::BadParams(format!("the file's parts do not read: {e}")))?;
            let xfer = XferId::new();
            let folder = std::env::temp_dir().join(format!("slopty-attach-{xfer}"));
            let file = folder.join(&name);
            let written =
                std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&file, bytes));
            let landed = written.map_err(|e| RpcError::Failed(e.to_string())).and_then(|()| {
                remote
                    .upload_landed(xfer, vec![file], Dest::Attachment)
                    .map_err(|e| RpcError::Failed(e.to_string()))
            });
            let _cleaned = std::fs::remove_dir_all(&folder);
            let landed = landed?;
            let path = landed
                .into_iter()
                .next()
                .ok_or_else(|| RpcError::Failed("the machine kept nothing".to_owned()))?;
            value(&serde_json::json!({ "path": path }))
        })
    }
}

/// A git op's answer that was not what was asked, in the words the machine gave.
fn git_failure(outcome: GitOutcome) -> RpcError {
    match outcome {
        GitOutcome::Done(_) => RpcError::Failed("the machine answered something else".into()),
        GitOutcome::Refused { why } | GitOutcome::Unavailable { why, .. } => RpcError::Failed(why),
        GitOutcome::Failed { said } => RpcError::Failed(said),
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

#[cfg(test)]
mod tests {
    use slopty_proto::git::{Branch, Branches};
    use slopty_proto::thread::{Command, Effort, Model as Offered};

    use super::*;

    fn branch(name: &str, local: bool) -> Branch {
        Branch { name: name.to_owned(), local, remote: !local, committed: 0 }
    }

    #[test]
    fn the_branch_checked_out_comes_first_then_the_clones_own() {
        let branches = Branches {
            current: Some("feature".to_owned()),
            default: Some("main".to_owned()),
            list: vec![branch("only-origin", false), branch("main", true), branch("feature", true)],
            more: 0,
        };
        let shown: Vec<(String, bool)> =
            refs(&branches).into_iter().map(|r| (r.name, r.current)).collect();
        assert_eq!(
            shown,
            [("feature".into(), true), ("main".into(), false), ("only-origin".into(), false)]
        );
    }

    #[test]
    fn an_agents_models_carry_the_efforts_it_offers() {
        let offers = Offers {
            models: vec![Offered { id: "opus".into(), label: "Opus".into() }],
            efforts: vec![
                Effort { id: "low".into(), label: "Low".into(), description: None },
                Effort { id: "xhigh".into(), label: "X-High".into(), description: None },
                Effort { id: "unheard-of".into(), label: "?".into(), description: None },
            ],
            commands: vec![Command {
                name: "compact".into(),
                description: "Compact".into(),
                argument_hint: Some("[focus]".into()),
                source: "built-in".into(),
                skill: false,
            }],
            ..Offers::default()
        };
        let shown = models(&offers, HarnessId::CLAUDE_CODE);
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].id.as_str(), shown[0].label.as_str()), ("opus", "Opus"));
        assert_eq!(shown[0].reasoning_levels, [ReasoningLevel::Low, ReasoningLevel::XHigh]);
        let none = models(&Offers { efforts: Vec::new(), ..offers.clone() }, HarnessId::CODEX);
        assert_eq!(none[0].reasoning_levels, super::super::settings::efforts(HarnessId::CODEX));
        let said = commands(&offers);
        assert_eq!(said[0].name, "compact");
        assert_eq!(said[0].input_hint.as_deref(), Some("[focus]"));
    }
}

#[cfg(test)]
mod picks {
    use gpui::AppContext as _;
    use slopty_core::WallMs;
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::TableFrame;
    use slopty_view_proto::{ChatConfig, HarnessId, ReasoningLevel, SandboxLevel};

    use crate::conversation::thread::fixtures;
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac};

    /// A pick stays until the thread runs it, then goes, so what the thread runs from then on
    /// shows as it moves.
    #[gpui::test]
    fn a_pick_goes_once_the_thread_runs_it(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.meters.effort = Some("low".to_owned());
        let chat = state.meta.id.to_string();
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let table = |state: &slopty_proto::thread::ThreadState, seq| TableFrame::Snapshot {
            cursor: Cursor { epoch: 1, seq },
            rows: vec![state.row(WallMs::ZERO)],
        };
        let pick = ChatConfig {
            harness: HarnessId::CLAUDE_CODE,
            model: None,
            reasoning: Some(ReasoningLevel::High),
            model_options: serde_json::Map::new(),
            sandbox: SandboxLevel::WorkspaceWrite,
        };
        workspace.update(cx, |v, cx| {
            v.threads_linked(KEY, cx);
            v.thread_table(KEY, &table(&state, 1), cx);
            v.pick_config(&chat, pick);
            v.settle_configs(cx);
            assert!(v.picked_config(&chat).is_some(), "not run yet: the pick shows");
        });
        state.meters.effort = Some("high".to_owned());
        workspace.update(cx, |v, cx| {
            v.thread_table(KEY, &table(&state, 2), cx);
            v.settle_configs(cx);
            assert!(v.picked_config(&chat).is_none(), "run now: the thread's own shows");
        });
    }
}
