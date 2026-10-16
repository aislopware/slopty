//! slopty: what a chat's thread says beside its transcript (its tray of background work, its
//! dock, its agent's permission rules) and the calls that act on them: stop a row, move a call
//! to the background, run a `!` command, rewind, change a rule, act in a mod's surface, clear the
//! goal, switch the mode ([`slopty_view_doc::calls`]).
//!
//! Every act is an intent through the thread's agent's own door, answered once its worker said
//! how it went; and the requests of the chats an agent started under a chat are asked, and
//! answered, in that chat.

use std::collections::BTreeSet;

use gpui::{AppContext as _, Context, Task};
use slopty_client::WorkerKey;
use slopty_proto::thread::wire::{Intent, ThreadRow};
use slopty_proto::thread::{
    ItemBody, ItemId, Rule, RuleChange, SurfaceEvent, ThreadId, ThreadState, TreeRef, TurnId,
};
use slopty_view_doc::{RewindUndo, calls};
use slopty_view_proto::UserInputAnswer;
use slopty_view_rpc::{RpcError, RpcReply};

use super::{INTERRUPT_WITHIN, param};
use crate::view_model::work::{Child, dock, rules, tray};
use crate::view_model::{ChatViews, RULES_CHAT, rows, transcript};
use crate::workspace::WorkspaceView;

/// A call's missing parameter.
fn required(key: &str) -> RpcError {
    RpcError::BadParams(format!("{key} is required"))
}

/// The surface event a call names, `None` for one Slopty does not know.
fn surface_event(event: &str, value: Option<&str>) -> Option<SurfaceEvent> {
    Some(match event {
        "press" => SurfaceEvent::Press,
        "input" => SurfaceEvent::Input(value.unwrap_or_default().to_owned()),
        "select" => SurfaceEvent::Select(value?.to_owned()),
        "close" => SurfaceEvent::Close,
        _ => return None,
    })
}

/// The turn of the person's message `message` in `state` (by the id the shell knows it by:
/// `named` its intent's, else its item's), and its words: `said` for the person's own.
fn message_turn(
    state: &ThreadState,
    message: &str,
    named: &dyn Fn(slopty_proto::thread::IntentId) -> Option<String>,
    said: &dyn Fn(slopty_proto::thread::IntentId) -> Option<String>,
) -> Option<(TurnId, String)> {
    state.items.iter().find_map(|item| {
        let ItemBody::User(user) = &item.body else { return None };
        let shown = user.intent.and_then(named).unwrap_or_else(|| item.id.0.clone());
        (shown == message).then(|| {
            let words = user.intent.and_then(said).unwrap_or_else(|| user.text.text.clone());
            (item.turn, words)
        })
    })
}

impl WorkspaceView {
    /// The thread `chat` shows, its worker, and its state as held here.
    fn view_chat_state<'a>(
        &self,
        chat: &str,
        cx: &'a gpui::App,
    ) -> Option<(WorkerKey, ThreadId, &'a ThreadState)> {
        let (key, thread) = self.chat_thread(chat, cx)?;
        let state = self.held_hub(key)?.read(cx).threads().mirror(thread)?.state()?;
        Some((key, thread, state))
    }

    /// The rows of the chats an agent started under `thread`, on any machine.
    fn view_children<'a>(
        &self,
        thread: ThreadId,
        cx: &'a gpui::App,
    ) -> Vec<(WorkerKey, &'a ThreadRow)> {
        self.hubs()
            .flat_map(|(key, hub)| {
                let table = &hub.read(cx).threads().rows().rows;
                table
                    .values()
                    .filter(|row| rows::parent(row) == Some(thread))
                    .map(move |r| (key, r))
            })
            .collect()
    }

    /// The states, as held here, of the chats an agent started under `thread` that ask the
    /// person something, each with its name.
    pub(super) fn view_asking_children<'a>(
        &self,
        thread: ThreadId,
        cx: &'a gpui::App,
    ) -> Vec<(&'a str, &'a ThreadState)> {
        self.view_children(thread, cx)
            .into_iter()
            .filter(|(_, row)| !row.requests.is_empty())
            .filter_map(|(key, row)| {
                let state = self.held_hub(key)?.read(cx).threads().mirror(row.id)?.state()?;
                Some((row.title.as_str(), state))
            })
            .collect()
    }

    /// Keep open the threads of the chats started under a followed chat that ask the person
    /// something, so their requests are asked there; let go of those that no longer do.
    pub(super) fn view_open_asking(&mut self, cx: &mut Context<Self>) {
        let wanted: BTreeSet<(WorkerKey, ThreadId)> = self
            .view_model
            .followed
            .values()
            .flat_map(|&(_, parent)| self.view_children(parent, cx))
            .filter(|(_, row)| !row.requests.is_empty())
            .map(|(key, row)| (key, row.id))
            .collect();
        let had = std::mem::take(&mut self.view_model.asking);
        for &(key, thread) in wanted.difference(&had) {
            let hub = self.thread_hub(key, cx);
            hub.update(cx, |hub, cx| hub.open(thread, cx));
        }
        for &(key, thread) in had.difference(&wanted) {
            if let Some(hub) = self.held_hub(key).cloned() {
                hub.update(cx, |hub, cx| hub.close(thread, cx));
            }
        }
        self.view_model.asking = wanted;
    }

    /// What `chat`'s thread says beside its transcript, once its state is here.
    pub(super) fn view_chat_views(&self, chat: &str, cx: &gpui::App) -> Option<ChatViews> {
        let (_, thread, state) = self.view_chat_state(chat, cx)?;
        let children = self.view_children(thread, cx);
        let children: Vec<Child<'_>> = children.iter().map(|&(_, row)| Child { row }).collect();
        let work = tray(state, &children, &|thread| self.chat_id(thread));
        let now = chrono::Utc::now().timestamp_millis();
        let dock = dock(state, &work.counts, now);
        Some(ChatViews { rules: rules(state, chat), dock, work })
    }

    /// An answer to `RespondInput` for a request of a chat started under `thread`, sent to that
    /// chat's thread; `None` when none of them asks it.
    pub(super) fn view_child_answer(
        &mut self,
        thread: ThreadId,
        request_id: &str,
        answers: &[UserInputAnswer],
        cx: &mut Context<Self>,
    ) -> Option<Result<serde_json::Value, RpcError>> {
        let (key, child, intent) =
            self.view_children(thread, cx).into_iter().find_map(|(key, row)| {
                let state = self.held_hub(key)?.read(cx).threads().mirror(row.id)?.state()?;
                let request =
                    state.requests.iter().find(|r| r.id.0 == request_id && r.is_open())?;
                let answer = transcript::answer(request, answers).map(|(choice, message)| {
                    Intent::Answer { ask: request.id.clone(), choice, message }
                });
                Some((key, row.id, answer))
            })?;
        let intent = match intent {
            Ok(intent) => intent,
            Err(why) => return Some(Err(RpcError::Failed(why))),
        };
        let hub = self.thread_hub(key, cx);
        let id = hub.update(cx, |hub, cx| hub.intent(child, intent, cx));
        Some(Ok(serde_json::json!({ "commandId": id.to_string() })))
    }

    /// Answered once the worker said how intent `id` went, or failed when it did not say within
    /// [`INTERRUPT_WITHIN`].
    pub(super) fn view_await(
        &mut self,
        id: slopty_proto::thread::IntentId,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.view_model.interrupts.insert(id, reply);
        cx.spawn(async move |this, cx| {
            use futures::FutureExt as _;
            let timer = cx.background_executor().timer(INTERRUPT_WITHIN);
            let unanswered =
                || Err(RpcError::Failed("The agent did not answer in time".to_owned()));
            let answer = futures::select_biased! {
                answer = answer.fuse() => answer.unwrap_or_else(|_| unanswered()),
                () = timer.fuse() => unanswered(),
            };
            let _gone = this.update(cx, |view, _| view.view_model.interrupts.remove(&id));
            answer
        })
    }

    /// Send `intent` to `chat`'s thread and answer once its worker said how it went.
    fn view_act(
        &mut self,
        chat: &str,
        intent: Intent,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some((key, thread)) = self.chat_thread(chat, cx) else {
            return Task::ready(Err(RpcError::Failed(format!("no thread {chat}"))));
        };
        let hub = self.thread_hub(key, cx);
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        self.view_await(id, cx)
    }

    /// One of Slopty's own calls on a chat's work, rules and controls; `None` for any other.
    pub(super) fn view_work_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        let known = [
            calls::STOP_WORK,
            calls::STOP_ALL_WORK,
            calls::BACKGROUND,
            calls::RUN_SHELL,
            calls::REWIND,
            calls::UNDO_REWIND,
            calls::CHANGE_RULE,
            calls::ALLOW_WITH_RULE,
            calls::SURFACE_EVENT,
            calls::CLEAR_GOAL,
            calls::SET_MODE,
            RULES_CHAT,
        ];
        if !known.contains(&method) {
            return None;
        }
        if method == RULES_CHAT {
            return Some(Task::ready(self.view_rules_chat(params, cx)));
        }
        if method == calls::UNDO_REWIND {
            return Some(self.view_undo_rewind(params, cx));
        }
        let Some(chat) = param(params, "chatId").map(str::to_owned) else {
            return Some(Task::ready(Err(required("chatId"))));
        };
        let text = |key: &str| param(params, key).map(str::to_owned);
        Some(match method {
            calls::STOP_WORK => match text("rowId") {
                Some(row) => self.view_stop_row(&chat, &row, cx),
                None => Task::ready(Err(required("rowId"))),
            },
            calls::STOP_ALL_WORK => Task::ready(self.view_stop_all(&chat, cx)),
            calls::BACKGROUND => {
                let item = text("partId").map(ItemId);
                self.view_act(&chat, Intent::Background { item }, cx)
            }
            calls::RUN_SHELL => match text("command").filter(|c| !c.trim().is_empty()) {
                Some(command) => self.view_act(&chat, Intent::Shell { command }, cx),
                None => Task::ready(Err(required("command"))),
            },
            calls::REWIND => self.view_rewind(&chat, params, cx),
            calls::CHANGE_RULE => {
                let (Some(behavior), Some(rule), Some(scope)) =
                    (text("behavior"), text("rule"), text("scope"))
                else {
                    return Some(Task::ready(Err(required("behavior, rule and scope"))));
                };
                let add = params.get("add").and_then(serde_json::Value::as_bool).unwrap_or(true);
                let change = RuleChange { add, behavior, rule, scope };
                self.view_act(&chat, Intent::Rule(change), cx)
            }
            calls::ALLOW_WITH_RULE => self.view_allow_with_rule(&chat, params, cx),
            calls::SURFACE_EVENT => {
                let (Some(surface), Some(event)) = (text("surfaceId"), text("event")) else {
                    return Some(Task::ready(Err(required("surfaceId and event"))));
                };
                let Some(event) = surface_event(&event, param(params, "value")) else {
                    return Some(Task::ready(Err(RpcError::BadParams(format!(
                        "no surface event {event}"
                    )))));
                };
                let element = text("element").unwrap_or_default();
                self.view_act(&chat, Intent::Surface { surface, element, event }, cx)
            }
            calls::CLEAR_GOAL => {
                let delivery = self
                    .view_chat_state(&chat, cx)
                    .map_or(slopty_proto::thread::Delivery::Steer, |(_, _, state)| {
                        state.meta.delivery_now()
                    });
                let intent = Intent::Send {
                    text: "/goal clear".to_owned(),
                    delivery,
                    attachments: Vec::new(),
                };
                self.view_act(&chat, intent, cx)
            }
            calls::SET_MODE => match text("mode") {
                Some(mode) => self.view_act(&chat, Intent::SetMode { mode }, cx),
                None => Task::ready(Err(required("mode"))),
            },
            _ => return None,
        })
    }

    /// `StopWork`: a task stopped by its agent, a chat started under this one stopped as its
    /// own Stop does.
    fn view_stop_row(
        &mut self,
        chat: &str,
        row: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        if let Some(task) = row.strip_prefix("task:").filter(|task| !task.is_empty()) {
            return self.view_act(chat, Intent::StopTask { task: task.to_owned() }, cx);
        }
        if let Some(child) = row.strip_prefix("chat:").filter(|child| !child.is_empty()) {
            return self.view_act(child, Intent::Interrupt, cx);
        }
        Task::ready(Err(RpcError::BadParams(format!("no row {row}"))))
    }

    /// `StopAllWork`: one stop for each live row of the chat's tray that can be stopped.
    fn view_stop_all(&mut self, chat: &str, cx: &mut Context<Self>) -> Result<RpcReply, RpcError> {
        let rows: Vec<String> = self
            .view_chat_views(chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?
            .work
            .rows
            .into_iter()
            .filter(|row| row.live && row.can_stop)
            .map(|row| row.id)
            .collect();
        for row in &rows {
            // Each stop says how it went in its row; none waits on another.
            self.view_stop_row(chat, row, cx).detach();
        }
        Ok(RpcReply::Value(serde_json::json!({ "stopped": rows.len() })))
    }

    /// `Rewind`: the files put back as they were before the message's turn, the conversation
    /// branched before it, or both; answered with the message's words for the composer and the
    /// branched chat.
    fn view_rewind(
        &mut self,
        chat: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(message) = param(params, "messageId") else {
            return Task::ready(Err(required("messageId")));
        };
        let flag =
            |key: &str| params.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false);
        let (code, conversation) = (flag("code"), flag("conversation"));
        if !code && !conversation {
            return Task::ready(Err(RpcError::BadParams(
                "code or conversation is required".to_owned(),
            )));
        }
        let (named, said) = (&self.view_model.named, &self.view_model.said);
        let found = self.view_chat_state(chat, cx).and_then(|(key, thread, state)| {
            let turn =
                message_turn(state, message, &|intent| named.get(&intent).cloned(), &|intent| {
                    said.get(&intent).or_else(|| state.said.get(&intent)).cloned()
                })?;
            Some((key, thread, turn))
        });
        let from = chat.to_owned();
        let Some((key, thread, (turn, prompt))) = found else {
            return Task::ready(Err(RpcError::Failed("That message is not here".to_owned())));
        };
        // slopty: the branch goes by the conversation's name, not "New chat".
        let title = self
            .held_hub(key)
            .and_then(|held| {
                held.read(cx).threads().rows().rows.get(&thread).map(|row| row.title.clone())
            })
            .filter(|title| !title.trim().is_empty());
        let hub = self.thread_hub(key, cx);
        let rewound = code.then(|| {
            let id = hub.update(cx, |hub, cx| hub.intent(thread, Intent::Rewind { turn }, cx));
            self.view_await(id, cx)
        });
        let forked = conversation.then(|| {
            let after = Some(TurnId(turn.0.saturating_sub(1)));
            let id = hub.update(cx, |hub, cx| hub.intent(thread, Intent::Fork { after }, cx));
            self.view_await(id, cx)
        });
        cx.spawn(async move |this, cx| {
            let mut undo = RewindUndo { chat_id: from, ..RewindUndo::default() };
            if let Some(rewound) = rewound {
                rewound.await?;
                // The folder as it was before, which the worker names once the rewind is done.
                undo.tree = rewound_tree(&this, &undo.chat_id, turn, cx).await;
            }
            let mut answer = serde_json::json!({ "prompt": prompt });
            let fields = answer.as_object_mut().ok_or_else(|| RpcError::Failed(String::new()))?;
            if let Some(forked) = forked
                && let RpcReply::Value(value) = forked.await?
                && let Some(branch) = value.get("chatId").and_then(serde_json::Value::as_str)
            {
                fields.insert("chatId".to_owned(), branch.into());
                undo.branch_chat_id = Some(branch.to_owned());
                // slopty: the branch keeps the conversation's name, and the words taken back as
                // its draft on its machine, which any device opening it finds.
                if let Ok(branched) = branch.parse::<ThreadId>() {
                    let draft = prompt.clone();
                    let _gone = this.update(cx, |this, cx| {
                        let hub = this.thread_hub(key, cx);
                        hub.update(cx, |hub, cx| {
                            if let Some(title) = title {
                                hub.intent(branched, Intent::Name { title }, cx);
                            }
                            if !draft.is_empty() {
                                hub.intent(branched, Intent::Draft { text: draft }, cx);
                            }
                        });
                    });
                }
            }
            if undo.branch_chat_id.is_some() || undo.tree.is_some() {
                fields.insert("undo".to_owned(), serde_json::to_value(&undo).unwrap_or_default());
            }
            Ok(RpcReply::Value(answer))
        })
    }

    /// `UndoRewind`: the folder put back as it was before the rewind, the branched chat
    /// deleted, and the chat it branched from shown again; the answer waits for the folder.
    fn view_undo_rewind(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let undo: RewindUndo = match params
            .get("undo")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| RpcError::BadParams(e.to_string()))
            .and_then(|undo| undo.ok_or_else(|| required("undo")))
        {
            Ok(undo) => undo,
            Err(e) => return Task::ready(Err(e)),
        };
        let unrewound = undo
            .tree
            .clone()
            .map(|tree| self.view_act(&undo.chat_id, Intent::Unrewind { tree: TreeRef(tree) }, cx));
        if let Some(branch) = &undo.branch_chat_id {
            let delete = serde_json::json!({ "op": "deleteChat", "chatId": branch });
            if let Err(e) = self.view_mutate(&delete, cx) {
                return Task::ready(Err(e));
            }
        }
        cx.background_spawn(async move {
            if let Some(unrewound) = unrewound {
                unrewound.await?;
            }
            Ok(RpcReply::Value(serde_json::json!({ "chatId": undo.chat_id })))
        })
    }

    /// `AllowWithRule`: the request allowed this once, and a rule added so its like is allowed
    /// from now on; the answer waits for the rule.
    fn view_allow_with_rule(
        &mut self,
        chat: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (Some(request), Some(rule), Some(scope)) =
            (param(params, "requestId"), param(params, "rule"), param(params, "scope"))
        else {
            return Task::ready(Err(required("requestId, rule and scope")));
        };
        let once = self.view_chat_state(chat, cx).and_then(|(_, _, state)| {
            let request = state.requests.iter().find(|r| r.id.0 == request && r.is_open())?;
            let choice = slopty_proto::thread::once(&request.options, true)?;
            Some((request.id.clone(), choice.id.clone(), request.needs_presence))
        });
        // No rule answers again what only the person at its card answers.
        if once.as_ref().is_some_and(|(_, _, present)| *present) {
            return Task::ready(Err(RpcError::Failed(
                slopty_proto::thread::Request::NEEDS_PRESENCE_REFUSED.to_owned(),
            )));
        }
        let Some((ask, choice, _)) = once else {
            return Task::ready(Err(RpcError::Failed(
                "It no longer waits on an answer".to_owned(),
            )));
        };
        let change = RuleChange {
            add: true,
            behavior: Rule::ALLOW.to_owned(),
            rule: rule.to_owned(),
            scope: scope.to_owned(),
        };
        let ruled = self.view_act(chat, Intent::Rule(change), cx);
        self.view_act(chat, Intent::Answer { ask, choice, message: None }, cx).detach();
        ruled
    }

    /// The chat whose rules `WatchRules` reads for `{ deviceId, cwd? }`: the newest thread on
    /// that machine (in that folder, else anywhere there) whose agent reads them.
    fn view_rules_chat(
        &self,
        params: &serde_json::Value,
        cx: &gpui::App,
    ) -> Result<RpcReply, RpcError> {
        let device = param(params, "deviceId").ok_or_else(|| required("deviceId"))?;
        let key = self
            .view_worker(device)
            .ok_or_else(|| RpcError::Failed(format!("no machine {device}")))?;
        let hub = self.held_hub(key).ok_or_else(|| RpcError::Failed("Not linked".to_owned()))?;
        let rows = &hub.read(cx).threads().rows().rows;
        let cwd = param(params, "cwd").map(|cwd| cwd.trim_end_matches('/'));
        let reads =
            |row: &&ThreadRow| row.caps.iter().any(|c| c.0 == slopty_proto::thread::Cap::RULES);
        let newest = |inside: bool| {
            rows.values()
                .filter(reads)
                .filter(|row| !inside || row.cwd.as_deref().map(|c| c.trim_end_matches('/')) == cwd)
                .max_by_key(|row| row.updated_ms)
        };
        let row = cwd
            .and_then(|_| newest(true))
            .or_else(|| newest(false))
            .ok_or_else(|| RpcError::Failed("No chat there reads permission rules".to_owned()))?;
        Ok(RpcReply::Value(serde_json::json!({ "chatId": self.chat_id(row.id) })))
    }
}

/// The tree `chat`'s folder was in before its rewind to `turn`, as the worker names it once the
/// rewind is done; `None` when it does not come soon, or nothing can be put back.
async fn rewound_tree(
    this: &gpui::WeakEntity<WorkspaceView>,
    chat: &str,
    turn: TurnId,
    cx: &mut gpui::AsyncApp,
) -> Option<String> {
    for _ in 0..REWOUND_TRIES {
        let tree = this
            .update(cx, |view, cx| {
                let (_, _, state) = view.view_chat_state(chat, cx)?;
                state.rewound.as_ref().filter(|r| r.turn == turn).map(|r| r.tree.0.clone())
            })
            .ok()?;
        if tree.is_some() {
            return tree;
        }
        cx.background_executor().timer(REWOUND_EVERY).await;
    }
    None
}

/// How often, and how many times, a rewind's folder is looked for once it is done.
const REWOUND_EVERY: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`REWOUND_EVERY`]: a second in all.
const REWOUND_TRIES: u32 = 20;

#[cfg(test)]
mod tests;
