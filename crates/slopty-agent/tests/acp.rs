//! ACP's wire, as the fixtures hold an agent's session (`tests/fixtures/acp/`, recorded from
//! `OpenCode` by `cargo xtask acp fixtures` but for `auth.jsonl`, written to the schema): every
//! message the agent wrote reads into the protocol's types, what Slopty sends is the very message
//! the agent was sent, and the session maps onto the thread model as the worker drives it.

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use slopty_agent::acp::driven::{self, Session};
    use slopty_agent::acp::rpc::{self, Incoming};
    use slopty_agent::acp::schema as acp;
    use slopty_core::WallMs;
    use slopty_proto::thread::{
        Action, Answerer, AskId, Cap, IntentId, ItemBody, Liveness, Phase, RequestState,
        ThreadState, ToolDetail, ToolState, TurnState, Usage,
    };

    const SESSION: &str = "ses_00000000000000000000000001";

    struct Line {
        sent: bool,
        msg: Value,
    }

    fn fixture(name: &str) -> Vec<Line> {
        let path = format!("{}/tests/fixtures/acp/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                Line { sent: line["dir"] == "in", msg: line["msg"].clone() }
            })
            .collect()
    }

    fn slopty() -> Answerer {
        Answerer { client: None, name: "Slopty".to_owned() }
    }

    fn intent(n: u128) -> IntentId {
        IntentId::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// Every message the agent wrote is a JSON-RPC message of the protocol, each update and
    /// permission request reads as its type, and each answer as the answer to what it answers.
    #[test]
    fn every_message_the_agent_wrote_reads_as_its_kind() {
        for name in ["turns.jsonl", "load.jsonl", "auth.jsonl"] {
            let lines = fixture(name);
            let methods: std::collections::HashMap<String, String> = lines
                .iter()
                .filter(|l| l.sent)
                .filter_map(|l| {
                    Some((l.msg.get("id")?.to_string(), l.msg["method"].as_str()?.to_owned()))
                })
                .collect();
            for line in lines.iter().filter(|l| !l.sent) {
                let read = rpc::incoming(line.msg.to_string().as_bytes()).unwrap();
                match read {
                    Incoming::Notification { method, params } => {
                        assert_eq!(method, "session/update");
                        let note: acp::SessionNotification =
                            serde_json::from_value(params).unwrap();
                        assert_eq!(*note.session_id.0, *SESSION);
                    }
                    Incoming::Request { method, params, .. } => match method.as_str() {
                        "session/request_permission" => {
                            let _asked: acp::RequestPermissionRequest =
                                serde_json::from_value(params).unwrap();
                        }
                        "fs/write_text_file" => {
                            let _asked: acp::WriteTextFileRequest =
                                serde_json::from_value(params).unwrap();
                        }
                        other => panic!("the agent asked {other}"),
                    },
                    Incoming::Response { outcome: Err(error), .. } => {
                        assert_eq!(error.code, acp::ErrorCode::AuthRequired, "{error:?}");
                    }
                    Incoming::Response { id, outcome: Ok(result) } => {
                        let method = &methods[&serde_json::to_value(&id).unwrap().to_string()];
                        let read = match method.as_str() {
                            "initialize" => {
                                serde_json::from_value::<acp::InitializeResponse>(result).map(drop)
                            }
                            "session/new" => {
                                serde_json::from_value::<acp::NewSessionResponse>(result).map(drop)
                            }
                            "session/load" => {
                                serde_json::from_value::<acp::LoadSessionResponse>(result).map(drop)
                            }
                            "session/prompt" => {
                                serde_json::from_value::<acp::PromptResponse>(result).map(drop)
                            }
                            other => panic!("an answer to {other}"),
                        };
                        read.unwrap();
                    }
                }
            }
        }
    }

    /// The fixture replayed through the codec, as the worker drives it: what went to the agent
    /// is asked of the codec and must be the very message that went, what came back is heard by
    /// it, and the thread is what the reducer makes of every action.
    fn replayed(name: &str, loading: bool) -> ThreadState {
        let agent = slopty_agent::acp::agent_id("opencode");
        let thread = driven::thread_of(intent(0));
        let (mut session, begun) = Session::new(agent, thread, "/work", WallMs::ZERO);
        let mut state = ThreadState::new(session.meta().clone());
        let apply = |state: &mut ThreadState, actions: Vec<Action>| {
            for action in &actions {
                state.apply(action);
            }
        };
        apply(&mut state, begun);
        let mut sends = 0_u128;
        let mut asked = std::collections::BTreeMap::<String, AskId>::default();
        let mut refused = std::collections::BTreeMap::<String, Value>::default();
        for line in fixture(name) {
            let msg = &line.msg;
            let method = msg.get("method").and_then(Value::as_str);
            if line.sent {
                let sent = match method {
                    Some("initialize") => {
                        let mut ours = serde_json::to_value(driven::initialize()).unwrap();
                        ours["clientInfo"]["version"] =
                            msg["params"]["clientInfo"]["version"].clone();
                        ours
                    }
                    Some("session/new") => serde_json::to_value(session.session_new()).unwrap(),
                    Some("session/load") => {
                        // The thread as the log keeps it, which names its session.
                        let mut meta = session.meta().clone();
                        meta.native = SESSION.to_owned();
                        let empty = ThreadState::new(meta);
                        session = Session::of(&empty, WallMs::ZERO);
                        state = empty;
                        serde_json::to_value(session.session_load().unwrap()).unwrap()
                    }
                    Some("session/prompt") => {
                        sends = sends.saturating_add(1);
                        let text = msg["params"]["prompt"][0]["text"].as_str().unwrap();
                        let (prompt, actions) =
                            session.prompt(text, &[], intent(sends), WallMs::ZERO);
                        apply(&mut state, actions);
                        serde_json::to_value(prompt).unwrap()
                    }
                    Some("session/cancel") => {
                        let cancelled = session.cancel(WallMs::ZERO).unwrap();
                        apply(&mut state, cancelled.actions);
                        assert_eq!(cancelled.answers.len(), 1, "the open request, answered");
                        let (id, response) = &cancelled.answers[0];
                        let answered = serde_json::to_value(response).unwrap();
                        asked.insert(rpc::id_text(id), AskId(format!("cancelled:{answered}")));
                        serde_json::to_value(cancelled.notification).unwrap()
                    }
                    Some(other) => panic!("sent {other}"),
                    None if msg.get("error").is_some() => {
                        let refusal = refused.remove(&msg["id"].to_string()).unwrap();
                        assert_eq!(*msg, refusal, "the refusal that went");
                        continue;
                    }
                    None => {
                        let id = msg["id"].to_string();
                        let ask = asked.remove(&id).unwrap();
                        let result = if let Some(answered) = ask.0.strip_prefix("cancelled:") {
                            serde_json::from_str(answered).unwrap()
                        } else {
                            let choice = msg["result"]["outcome"]["optionId"].as_str().unwrap();
                            let answered =
                                session.answer(&ask, choice, slopty(), WallMs::ZERO).unwrap();
                            apply(&mut state, answered.actions);
                            serde_json::to_value(answered.response).unwrap()
                        };
                        assert_eq!(result, msg["result"], "the answer that went");
                        continue;
                    }
                };
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                assert_eq!(sent, params, "what went to the agent");
                continue;
            }
            match rpc::incoming(msg.to_string().as_bytes()).unwrap() {
                Incoming::Notification { params, .. } => {
                    apply(&mut state, session.update(&params, WallMs::ZERO));
                }
                Incoming::Request { id, method, params } => {
                    if method == "session/request_permission" {
                        let actions = session.permission(&id, &params, WallMs::ZERO).unwrap();
                        apply(&mut state, actions);
                        asked.insert(msg["id"].to_string(), AskId(rpc::id_text(&id)));
                    } else {
                        // What the worker refuses: no file system, no terminal.
                        let refusal = rpc::error(&id, acp::Error::method_not_found()).unwrap();
                        let refusal = serde_json::from_slice(&refusal).unwrap();
                        refused.insert(msg["id"].to_string(), refusal);
                    }
                }
                Incoming::Response { outcome, .. } => {
                    let result = outcome.unwrap();
                    let actions = if result.get("protocolVersion").is_some() {
                        let init: acp::InitializeResponse = serde_json::from_value(result).unwrap();
                        session.initialized(&init).unwrap()
                    } else if result.get("stopReason").is_some() {
                        session.prompted(Ok(&result), WallMs::ZERO)
                    } else if loading {
                        let load: acp::LoadSessionResponse =
                            serde_json::from_value(result).unwrap();
                        session.opened(
                            None,
                            load.modes.as_ref(),
                            load.config_options.as_deref(),
                            WallMs::ZERO,
                        )
                    } else {
                        let new: acp::NewSessionResponse = serde_json::from_value(result).unwrap();
                        let options = new.config_options.as_deref();
                        session.opened(
                            Some(&new.session_id),
                            new.modes.as_ref(),
                            options,
                            WallMs::ZERO,
                        )
                    };
                    apply(&mut state, actions);
                }
            }
        }
        state
    }

    /// A session's turns map onto the thread: the agent's answer and thinking as items, each
    /// call it asks about a request with the answers it offers, and each call ending as the
    /// person answered it; the session's mode, model and context in the meters.
    #[test]
    fn the_session_maps_onto_the_thread() {
        let state = replayed("turns.jsonl", false);
        assert_eq!(state.meta.native, SESSION);
        assert_eq!(state.meta.agent_version, "1.18.34");
        assert!(driven::resumable(&state.meta), "the agent loads sessions");
        assert_eq!(state.meta.title, "Say hello.");
        for cap in [Cap::APPROVALS, Cap::INTERRUPT, Cap::QUEUE, Cap::SET_MODE, Cap::SET_MODEL] {
            assert!(state.meta.can(cap), "{cap}");
        }
        assert_eq!(state.meta.models.len(), 1, "the agent's model choices");
        assert_eq!(state.meters.model_id.as_deref(), Some("canned/canned-1"));
        assert_eq!(state.meters.mode.as_deref(), Some("build"));
        assert_eq!(state.meters.context_window, Some(200_000));
        assert_eq!(state.commands.len(), 3);

        let ends: Vec<&TurnState> = state.turns.iter().map(|t| &t.state).collect();
        let want = [
            &TurnState::Complete,
            &TurnState::Complete,
            &TurnState::Complete,
            &TurnState::Interrupted,
        ];
        assert_eq!(ends, want);
        assert!(state.turns.iter().all(|t| t.input.is_some()), "each opened by its message");
        let kinds: Vec<&str> = state
            .items
            .iter()
            .filter(|i| i.turn == state.turns[0].id)
            .map(|i| match &i.body {
                ItemBody::User(_) => "user",
                ItemBody::Reasoning(_) => "thinking",
                ItemBody::Text(_) => "text",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["user", "thinking", "text"]);

        let calls: Vec<(&str, &ToolState, &str)> = state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Tool(call) => Some((i.id.0.as_str(), &call.state, call.kind.as_str())),
                _ => None,
            })
            .collect();
        let want = [
            ("toolu_1", &ToolState::Completed, "write"),
            ("toolu_2", &ToolState::Rejected, "exec"),
            ("toolu_3", &ToolState::Cancelled, "exec"),
        ];
        assert_eq!(calls, want);
        let written = state.items.iter().find_map(|i| match &i.body {
            ItemBody::Tool(call) => match &call.detail {
                Some(ToolDetail::Write(write)) => Some(write),
                _ => None,
            },
            _ => None,
        });
        let written = written.unwrap();
        assert_eq!((written.path.as_str(), written.lines), ("/work/made-by-acp", 1));
        let hunks: Vec<(u32, u32, u32, u32)> = written
            .patch
            .hunks
            .iter()
            .map(|h| (h.old_start, h.old_lines, h.new_start, h.new_lines))
            .collect();
        assert_eq!(hunks, [(0, 0, 1, 1)], "a new file is numbered from its first line");
        let requests: Vec<&RequestState> = state.requests.iter().map(|r| &r.state).collect();
        let answered =
            |choice: &str| RequestState::Answered { by: slopty(), choice: choice.into() };
        assert_eq!(requests, [&answered("once"), &answered("reject"), &RequestState::Withdrawn]);
        let offered: Vec<(&str, Option<&str>)> =
            state.requests[0].options.iter().map(|o| (o.id.as_str(), o.scope.as_deref())).collect();
        assert_eq!(offered, [("once", None), ("always", Some("always")), ("reject", None)]);
        assert_eq!(state.status.phase, Phase::Stopped);
        assert_eq!(state.status.liveness, Liveness::Live);

        // What each turn took, as the agent counts it with its answer, and what it changed.
        let first = &state.turns[0];
        assert_eq!(first.usage.get(Usage::INPUT), 12);
        assert_eq!(first.usage.get(Usage::OUTPUT), 20);
        let wrote = state
            .items
            .iter()
            .find(|i| i.id.0 == "toolu_1")
            .map(|i| i.turn)
            .expect("the write's turn");
        let turn = state.turns.iter().find(|t| t.id == wrote).unwrap();
        assert_eq!((turn.changed.added, turn.changed.removed), (1, 0), "the file it wrote");
    }

    /// An agent's thought-level option is the meters' effort, by the name it gives its value.
    #[test]
    fn a_thought_level_option_is_the_effort() {
        let line = fixture("turns.jsonl")
            .into_iter()
            .find(|l| !l.sent && l.msg["result"].get("configOptions").is_some())
            .expect("the new session's answer");
        let mut result = line.msg["result"].clone();
        let level = serde_json::json!({"category": "thought_level", "currentValue": "hi",
            "id": "effort", "name": "Effort", "type": "select",
            "options": [{"name": "Low", "value": "lo"}, {"name": "High", "value": "hi"}]});
        result["configOptions"].as_array_mut().unwrap().push(level);
        let new: acp::NewSessionResponse = serde_json::from_value(result).unwrap();
        let agent = slopty_agent::acp::agent_id("opencode");
        let (mut session, begun) =
            Session::new(agent, driven::thread_of(intent(0)), "/work", WallMs::ZERO);
        let mut state = ThreadState::new(session.meta().clone());
        let opened = session.opened(
            Some(&new.session_id),
            new.modes.as_ref(),
            new.config_options.as_deref(),
            WallMs::ZERO,
        );
        for action in begun.iter().chain(&opened) {
            state.apply(action);
        }
        assert_eq!(state.meters.effort.as_deref(), Some("High"));
    }

    /// A session loaded again is the thread it was: the person's messages open its turns, and
    /// what the agent said is whole, with nothing streamed.
    #[test]
    fn a_loaded_session_is_read_again_as_the_thread_it_was() {
        let state = replayed("load.jsonl", true);
        let users: Vec<(String, Option<IntentId>)> = state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::User(m) => Some((m.text.text.clone(), m.intent)),
                _ => None,
            })
            .collect();
        let replayed =
            ["Say hello.", "Make a file called made-by-acp.", "Remove it.", "Remove it again."];
        let want: Vec<(String, Option<IntentId>)> = replayed
            .iter()
            .map(|text| ((*text).to_owned(), None))
            .chain([("Say hello again.".to_owned(), Some(intent(1)))])
            .collect();
        assert_eq!(users, want, "a replayed message carries no intent of the codec's");
        assert_eq!(state.turns.len(), 5);
        assert!(state.turns.iter().all(|t| t.state == TurnState::Complete));
        let calls: Vec<(&str, &ToolState)> = state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Tool(call) => Some((i.id.0.as_str(), &call.state)),
                _ => None,
            })
            .collect();
        let want = [
            ("toolu_1", &ToolState::Completed),
            ("toolu_2", &ToolState::Failed),
            ("toolu_3", &ToolState::Failed),
        ];
        assert_eq!(calls, want, "each call as it ended, with no request to tell why");
        assert_eq!(state.meta.title, "Say hello.", "named by its first message");
    }

    /// An agent that asks to be signed in is left as it is, with the reason in words: Slopty
    /// signs no agent in.
    #[test]
    fn an_agent_that_asks_to_be_signed_in_is_told_in_words() {
        let error = acp::Error::auth_required();
        let said = driven::said(&error);
        assert!(said.contains("Slopty signs no agent in"), "{said}");
    }
}
