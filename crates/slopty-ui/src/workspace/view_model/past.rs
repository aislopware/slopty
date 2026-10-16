//! Claude Code's past sessions, as the shell lists, searches and opens them: a project's
//! History in the sidebar and the "Resume a session…" picker (`slopty_shell`'s
//! `past_sessions`). Every answer is the session's own machine's, through its worker
//! (`ThreadRequest::Sessions`, answered by `WorkerMsg::Sessions`), so a remote project reads
//! exactly as a local one.
//!
//! **A folder's sessions** are one ask of its machine: Claude Code's own list of the sessions it
//! kept there, newest first (every session, the TUI's and `claude -p`'s alike, by its
//! transcript's name and time), each with its title, its last prompt and its branch as the
//! machine read them. A session the person never typed into has no title, and the shell names
//! it by its id.
//!
//! **A search** is one ask: the sessions whose prompts hold every word, best first, each with
//! the prompt that matched and where the words are in it; in one folder or every folder of the
//! machine.
//!
//! **Opening** a session the machine keeps a thread of opens that chat. Any other is started
//! through Claude Code's own door, `--resume <id>` in the folder it ran in, which reads the
//! session's whole conversation from its transcript first; the shell opens the chat once the
//! worker says it started, and a refusal is said in the worker's words.

use std::collections::HashMap;

use futures::FutureExt as _;
use gpui::{AsyncApp, Context, Task};
use slopty_client::WorkerKey;
use slopty_proto::thread::wire::{
    IntentDone, Outcome, PAST_RUNNING, PastSession, PastSessions, Start, ThreadRequest,
};
use slopty_proto::thread::{AgentId, IntentId};
use slopty_proto::{ClientMsg, WorkerMsg};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::oneshot;

use super::param;
use crate::workspace::WorkspaceView;

/// What the shell asks of past sessions beyond zeron's own calls.
pub mod calls {
    /// Past Claude Code sessions: `{ spaceIds, query, limit }` for the folders of those spaces,
    /// or `{ deviceId, allFolders: true, query, limit }` for every folder of a machine →
    /// `{ sessions: [PastRow], failures: [{ deviceId, device, words }], more, cut }`.
    pub const PAST_SESSIONS: &str = "slopty.PastSessions";
    /// Take a past session up again: `{ deviceId, cwd, resume }` → `{ chatId }` once its machine
    /// started it, or the machine's words for why not.
    pub const OPEN_PAST_SESSION: &str = "slopty.OpenPastSession";
}

/// The fact a session's branch is said by, when its machine says it.
pub const BRANCH: &str = "branch";

/// The most sessions one call lists.
const MOST: u32 = 200;

/// How long a machine has to answer before the call fails.
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// What a machine's answer echoes of its ask: the machine, the agent, the folder and the words.
type Asked = (WorkerKey, Option<AgentId>, Option<String>, String);

/// What waits on the machines.
#[derive(Default)]
pub(in crate::workspace) struct Past {
    /// Asks of a machine waiting for its answer: the most sessions the ask sent asked for, and
    /// each asker. An ask the same as one on its way waits for that one's answer.
    asked: HashMap<Asked, (u32, Vec<oneshot::Sender<PastSessions>>)>,
    /// Sessions being taken up again, by their start's intent.
    opening: HashMap<IntentId, oneshot::Sender<Result<RpcReply, RpcError>>>,
}

impl std::fmt::Debug for Past {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Past")
            .field("asked", &self.asked.len())
            .field("opening", &self.opening.len())
            .finish()
    }
}

/// Claude Code, the agent whose sessions are listed.
fn claude() -> AgentId {
    AgentId::named(AgentId::CLAUDE_CODE)
}

/// One folder or machine asked, and what it answered.
#[derive(Debug)]
struct Answered {
    key: WorkerKey,
    /// The sessions, in the order to show them, at most one more than asked.
    sessions: Vec<PastSession>,
    /// It lists a folder's sessions, newest first, rather than ranking what words found.
    listing: bool,
    /// Why there are none, in words, when the machine could not say.
    absent: Option<String>,
    /// Why some may be missing, in words.
    cut: Option<String>,
}

/// Whether a session can be opened: the machine keeps a thread of it, or Claude Code can take it
/// up again (its transcript is there).
const fn openable(session: &PastSession) -> bool {
    session.thread.is_some() || !session.resume.is_empty()
}

/// The answer of a call made now.
fn now(answer: Result<serde_json::Value, RpcError>) -> Task<Result<RpcReply, RpcError>> {
    Task::ready(answer.map(RpcReply::Value))
}

/// Wait for `rx` at most [`ANSWER_WITHIN`]; `None` when the machine went quiet or away.
async fn within<T>(cx: &AsyncApp, rx: oneshot::Receiver<T>) -> Option<T> {
    let timer = cx.background_executor().timer(ANSWER_WITHIN);
    futures::select_biased! {
        answer = rx.fuse() => answer.ok(),
        () = timer.fuse() => None,
    }
}

/// The places a call names: each space's machine and folder, or one machine's every folder.
#[derive(Debug)]
struct Places {
    /// The machine and folder of each place; no folder for every folder.
    asked: Vec<(WorkerKey, Option<String>)>,
    /// The places that cannot be asked, by device id, and why.
    failed: Vec<(String, String)>,
}

impl WorkspaceView {
    /// The answer to `method` when it is a past session's call; `None` for every other call.
    pub(super) fn past_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        Some(match method {
            calls::PAST_SESSIONS => self.past_sessions(params, cx),
            calls::OPEN_PAST_SESSION => self.open_past_session(params, cx),
            _ => return None,
        })
    }

    /// A worker's message, heard before anything else: an answer to a past sessions' ask goes
    /// to whoever waits on it. Whether it was one.
    pub(super) fn past_heard(&mut self, key: WorkerKey, msg: &WorkerMsg) -> bool {
        let WorkerMsg::Sessions(answer) = msg else { return false };
        let asked = (key, answer.agent.clone(), answer.cwd.clone(), answer.query.clone());
        let Some((_, waiting)) = self.view_model.past.asked.remove(&asked) else { return false };
        for reply in waiting {
            let _gone = reply.send(answer.clone());
        }
        true
    }

    /// The answer to any intent: a session being opened hears whether it started.
    pub(super) fn past_done(&mut self, done: &IntentDone) {
        let Some(reply) = self.view_model.past.opening.remove(&done.id) else { return };
        let answer = match &done.outcome {
            Outcome::Started { thread } => {
                Ok(RpcReply::Value(serde_json::json!({ "chatId": self.chat_id(*thread) })))
            }
            Outcome::Refused { reason } => Err(RpcError::Failed(reason.clone())),
            Outcome::Unsupported { cap } => {
                Err(RpcError::Failed(format!("Claude Code here cannot {}", cap.0)))
            }
            _ => Err(RpcError::Failed("The session did not open".to_owned())),
        };
        let _gone = reply.send(answer);
    }

    /// Ask `key` for `agent`'s sessions in `cwd` that `query` finds, at most `limit`: the answer
    /// comes on the returned channel. An ask the same as one on its way waits for its answer.
    fn past_ask(
        &mut self,
        key: WorkerKey,
        agent: Option<AgentId>,
        cwd: Option<String>,
        query: String,
        limit: u32,
    ) -> oneshot::Receiver<PastSessions> {
        let (tx, rx) = oneshot::channel();
        let asked = (key, agent.clone(), cwd.clone(), query.clone());
        if let Some((sent, waiting)) = self.view_model.past.asked.get_mut(&asked)
            && *sent >= limit
        {
            waiting.push(tx);
            return rx;
        }
        let waiting = self.view_model.past.asked.entry(asked).or_insert((limit, Vec::new()));
        waiting.0 = waiting.0.max(limit);
        waiting.1.push(tx);
        self.send(key, ClientMsg::Thread(ThreadRequest::Sessions { agent, cwd, query, limit }));
        rx
    }

    /// The places `params` names to ask.
    fn past_places(&self, params: &serde_json::Value, cx: &Context<Self>) -> Places {
        let mut places = Places { asked: Vec::new(), failed: Vec::new() };
        let reach = |key: WorkerKey, cwd: Option<String>, places: &mut Places| {
            if self.reachable_for(key, "past sessions", cx) {
                places.asked.push((key, cwd));
            } else {
                let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
                places.failed.push((key.to_string(), format!("{name} is out of reach")));
            }
        };
        let every = params.get("allFolders").and_then(serde_json::Value::as_bool) == Some(true);
        if every {
            match param(params, "deviceId").and_then(|device| self.view_worker(device)) {
                Some(key) => reach(key, None, &mut places),
                None => places.failed.push((String::new(), "No machine to ask".to_owned())),
            }
            return places;
        }
        let spaces = params.get("spaceIds").and_then(serde_json::Value::as_array);
        for space in spaces.into_iter().flatten().filter_map(serde_json::Value::as_str) {
            match self.view_space(space) {
                Some((key, path)) => reach(key, Some(path), &mut places),
                None => places.failed.push((String::new(), format!("No project {space} here"))),
            }
        }
        places
    }

    /// `slopty.PastSessions`: Claude Code's past sessions in the folders `params.spaceIds` name,
    /// or every folder of `params.deviceId`'s machine with `params.allFolders`, found by
    /// `params.query`, at most `params.limit` from each.
    fn past_sessions(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let query = param(params, "query").map(str::trim).unwrap_or_default().to_owned();
        let limit = params
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(10)
            .clamp(1, MOST);
        let places = self.past_places(params, cx);
        if places.asked.is_empty() && places.failed.is_empty() {
            return now(Err(RpcError::BadParams("name spaceIds, or a deviceId".to_owned())));
        }
        let mut asks = Vec::with_capacity(places.asked.len());
        for (key, cwd) in places.asked {
            // A folder with no words is listed by Claude Code, newest first.
            let listing = query.is_empty() && cwd.is_some();
            let main =
                self.past_ask(key, Some(claude()), cwd, query.clone(), limit.saturating_add(1));
            asks.push((key, main, listing));
        }
        let failed = places.failed;
        cx.spawn(async move |this, cx| {
            let mut answered = Vec::with_capacity(asks.len());
            let mut silent = Vec::new();
            for (key, main, listing) in asks {
                let Some(main) = within(cx, main).await else {
                    silent.push(key);
                    continue;
                };
                answered.push(Answered {
                    key,
                    sessions: main.sessions,
                    listing,
                    absent: main.absent,
                    cut: main.cut,
                });
            }
            this.update(cx, |view, _| view.past_rows(answered, silent, failed, limit))
                .map_err(|_gone| RpcError::Closed)
        })
    }

    /// What the shell is answered: the sessions every place gave, newest first across places
    /// for a listing (a search keeps each place's best first), the places that failed, whether
    /// any place has more, and what any place may have left out.
    fn past_rows(
        &self,
        answered: Vec<Answered>,
        silent: Vec<WorkerKey>,
        failed: Vec<(String, String)>,
        limit: u32,
    ) -> RpcReply {
        let most = usize::try_from(limit).unwrap_or(usize::MAX);
        let mut more = false;
        let mut rows = Vec::new();
        let mut failures: Vec<serde_json::Value> = failed
            .into_iter()
            .map(|(device, words)| {
                let name = self.view_worker(&device).map(|key| self.past_machine(key));
                serde_json::json!({ "deviceId": device, "device": name, "words": words })
            })
            .collect();
        for key in silent {
            let name = self.past_machine(key);
            failures.push(serde_json::json!({
                "deviceId": key.to_string(),
                "device": name,
                "words": format!("{name} did not answer"),
            }));
        }
        let mut cut = None;
        let listing = answered.iter().all(|a| a.listing);
        for answer in answered {
            if let Some(words) = answer.absent {
                let name = self.past_machine(answer.key);
                failures.push(serde_json::json!({
                    "deviceId": answer.key.to_string(),
                    "device": name,
                    "words": words,
                }));
            }
            cut = cut.or(answer.cut);
            let mut sessions = answer.sessions;
            sessions.retain(openable);
            more |= sessions.len() > most;
            sessions.truncate(most);
            rows.extend(sessions.iter().map(|session| self.past_row(answer.key, session)));
        }
        if listing {
            rows.sort_by_key(|row| {
                std::cmp::Reverse(row.get("updatedAt").and_then(serde_json::Value::as_u64))
            });
        }
        RpcReply::Value(serde_json::json!({
            "sessions": rows,
            "failures": failures,
            "more": more,
            "cut": cut,
        }))
    }

    /// A machine's name, as the person named it.
    fn past_machine(&self, key: WorkerKey) -> String {
        self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default()
    }

    /// One session as the shell reads it.
    fn past_row(&self, key: WorkerKey, session: &PastSession) -> serde_json::Value {
        let device = key.to_string();
        let prompt = session.prompts.first().map(|hit| {
            let spans: Vec<[u32; 2]> = hit.spans.iter().map(|s| [s.start, s.end]).collect();
            serde_json::json!({
                "text": hit.text,
                "spans": spans,
                "cutBefore": hit.cut_before,
                "cutAfter": hit.cut_after,
            })
        });
        let space = session.cwd.as_deref().and_then(|cwd| {
            let place = self.view_place(&device, Some(cwd), None)?;
            Some(self.view_space_id(&device, &place))
        });
        serde_json::json!({
            "key": format!("{device}:{}", session.native),
            "deviceId": device,
            "device": self.past_machine(key),
            "native": session.native,
            "title": session.title.as_deref().map(str::trim).filter(|t| !t.is_empty()),
            "prompt": prompt,
            "cwd": session.cwd,
            "spaceId": space,
            "updatedAt": session.updated_ms.map(slopty_core::WallMs::as_millis),
            "branch": session.facts.get(BRANCH),
            "running": session.facts.get(PAST_RUNNING),
            "chatId": session.thread.map(|thread| self.chat_id(thread)),
            "resume": session.resume,
        })
    }

    /// `slopty.OpenPastSession`: take Claude Code's session up again on `params.deviceId`'s
    /// machine, in `params.cwd`, with the words its listing gave (`params.resume`); answered
    /// with the chat once the machine started it, or the machine's words for why not.
    fn open_past_session(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(key) = param(params, "deviceId").and_then(|device| self.view_worker(device))
        else {
            return now(Err(RpcError::Failed("Its machine is not one of these".to_owned())));
        };
        let Some(cwd) = param(params, "cwd").map(str::to_owned) else {
            return now(Err(RpcError::BadParams("cwd is required".to_owned())));
        };
        let resume: Vec<String> = params
            .get("resume")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        if resume.is_empty() {
            return now(Err(RpcError::Failed(
                "Claude Code no longer keeps this session's record".to_owned(),
            )));
        }
        if !self.reachable_for(key, "a past session", cx) {
            let name = self.past_machine(key);
            return now(Err(RpcError::Failed(format!("{name} is out of reach"))));
        }
        let id = IntentId::new();
        let start = Start {
            agent: claude(),
            cwd,
            drive: None,
            prompt: None,
            model: None,
            mode: None,
            effort: None,
            attachments: Vec::new(),
            args: resume,
            options: std::collections::BTreeMap::new(),
            worktree: None,
        };
        let (reply, answer) = oneshot::channel();
        self.view_model.past.opening.insert(id, reply);
        self.view_send_kept(
            key,
            id,
            ClientMsg::Thread(ThreadRequest::Start { id, start: Box::new(start) }),
        );
        cx.spawn(async move |this, cx| {
            if let Some(answer) = within(cx, answer).await {
                return answer;
            }
            let _forgot = this.update(cx, |view, _| {
                view.view_model.past.opening.remove(&id);
                view.view_model.unanswered.remove(&id);
            });
            Err(RpcError::Failed("Its machine did not answer".to_owned()))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn session(native: &str, title: Option<&str>) -> PastSession {
        PastSession {
            agent: claude(),
            native: native.to_owned(),
            cwd: Some("/work".to_owned()),
            title: title.map(str::to_owned),
            updated_ms: None,
            thread: None,
            resume: vec!["--resume".to_owned(), native.to_owned()],
            facts: BTreeMap::new(),
            prompts: Vec::new(),
        }
    }

    /// A session whose record is gone and that no thread keeps cannot be opened.
    #[test]
    fn a_session_with_no_record_cannot_be_opened() {
        let mut gone = session("a", None);
        gone.resume.clear();
        assert!(!openable(&gone));
        gone.thread = Some(slopty_proto::thread::ThreadId::new());
        assert!(openable(&gone), "a thread kept of it opens");
        assert!(openable(&session("b", None)));
    }
}
