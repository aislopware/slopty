//! The Codex app-server's wire, as the pinned build spoke it to two clients of one thread
//! (`cargo xtask codex fixtures`): every frame reads into the generated types, what both sides
//! sent reads back to the same JSON, and the approval goes the way the recording shows.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use slopty_agent::codex::protocol::{self as p, ServerNotification, ServerRequest};
    use slopty_agent::codex::rpc::{self, Incoming};
    use slopty_agent::codex::shared::{self, Shared};
    use slopty_core::WallMs;
    use slopty_proto::thread::{
        Action, AgentId, Answerer, Drive, Effect, ItemBody, Notice, Phase, Request, RequestState,
        Status, ThreadState, ToolDetail, ToolState, TurnId, TurnState,
    };

    /// The notifications Slopty passes over: the app-server's remote-control state and its
    /// deprecation notices.
    const PASSED_OVER: [&str; 2] = ["remoteControl/status/changed", "deprecationNotice"];

    struct Line {
        client: String,
        sent: bool,
        msg: Value,
    }

    fn approval() -> Vec<Line> {
        fixture("approval.jsonl")
    }

    fn fixture(name: &str) -> Vec<Line> {
        let path = format!("{}/tests/fixtures/codex/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                Line {
                    client: line["client"].as_str().unwrap().to_owned(),
                    sent: line["dir"] == "sent",
                    msg: line["msg"].clone(),
                }
            })
            .collect()
    }

    /// `value` without the fields that are `null`, which a type of optional fields leaves out.
    fn bare(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, bare(v))).collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(bare).collect()),
            other => other,
        }
    }

    /// `value` read as `T` and written back is `value`.
    fn reads_back<T: DeserializeOwned + Serialize>(what: &str, value: &Value) {
        let typed: T = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("{what} does not read: {e}\n{value:#}"));
        let again = serde_json::to_value(&typed).unwrap();
        assert_eq!(bare(again), bare(value.clone()), "{what} reads back otherwise");
    }

    /// Request params of `method`, then its answer's result, each read back.
    fn reads_back_call(method: &str, params: &Value, result: &Value) {
        macro_rules! call {
            ($params:ty) => {{
                reads_back::<$params>(&format!("{method}'s params"), params);
                reads_back::<<$params as p::Method>::Response>(
                    &format!("{method}'s result"),
                    result,
                );
            }};
        }
        match method {
            "initialize" => call!(p::InitializeParams),
            "thread/start" => call!(p::ThreadStartParams),
            "thread/resume" => call!(p::ThreadResumeParams),
            "turn/start" => call!(p::TurnStartParams),
            "item/commandExecution/requestApproval" => {
                call!(p::CommandExecutionRequestApprovalParams);
            }
            "item/tool/requestUserInput" => call!(p::ToolRequestUserInputParams),
            other => panic!("no types named for {other}"),
        }
    }

    /// Every frame the app-server sent reads, and each notification it sent is one these types
    /// name or one Slopty passes over on purpose.
    #[test]
    fn every_frame_the_app_server_sent_reads() {
        let mut unnamed = BTreeSet::new();
        let mut read = 0;
        let lines: Vec<Line> =
            [approval(), fixture("question.jsonl")].into_iter().flatten().collect();
        for line in lines.iter().filter(|line| !line.sent) {
            match rpc::read(&line.msg.to_string()).unwrap() {
                Incoming::UnknownNotification { method } => {
                    unnamed.insert(method);
                }
                Incoming::UnknownRequest { method, .. } => panic!("an unnamed request {method}"),
                Incoming::Answer { outcome, .. } => {
                    outcome.unwrap();
                }
                Incoming::Request { .. } | Incoming::Notification { .. } => read += 1,
            }
        }
        assert_eq!(unnamed, PASSED_OVER.iter().map(|m| (*m).to_owned()).collect());
        assert!(read > 40, "only {read} frames read into named types");
    }

    /// What either side sent reads back to the same JSON: Slopty's requests and the answers to
    /// them, and the approval and the answer to it.
    #[test]
    fn what_both_sides_sent_reads_back() {
        let lines: Vec<Line> =
            [approval(), fixture("question.jsonl")].into_iter().flatten().collect();
        let mut asked: HashMap<(String, String), (String, Value)> = HashMap::new();
        let mut checked = BTreeSet::new();
        for line in &lines {
            let id = line.msg.get("id").map(Value::to_string);
            let method = line.msg.get("method").and_then(Value::as_str);
            match (id, method) {
                // Each fixture numbers from 1 again: a request replaces the one before it.
                (Some(id), Some(method)) => {
                    let params = line.msg.get("params").cloned().unwrap_or(Value::Null);
                    asked.insert((line.client.clone(), id), (method.to_owned(), params));
                }
                (Some(id), None) => {
                    let Some(result) = line.msg.get("result") else { continue };
                    let Some((method, params)) = asked.get(&(line.client.clone(), id)) else {
                        continue;
                    };
                    reads_back_call(method, params, result);
                    checked.insert(method.clone());
                }
                _ => {}
            }
        }
        let want = ["initialize", "thread/start", "thread/resume", "turn/start"];
        let asks = ["item/commandExecution/requestApproval", "item/tool/requestUserInput"];
        let want = want.into_iter().chain(asks);
        assert_eq!(checked, want.map(str::to_owned).collect());
    }

    /// Both clients of the thread are asked, under one id. The first answer settles it: both
    /// hear `serverRequest/resolved`, the turn goes on, and an answer after that is never
    /// answered.
    #[test]
    fn both_clients_are_asked_and_the_first_answer_settles_it() {
        let lines = approval();
        let heard = |line: &Line| (!line.sent).then(|| rpc::read(&line.msg.to_string()).unwrap());
        let mut asked = Vec::new();
        for line in &lines {
            if let Some(Incoming::Request { id, request, .. }) = heard(line) {
                assert!(matches!(*request, ServerRequest::ItemCommandExecutionRequestApproval(_)));
                asked.push((line.client.clone(), id));
            }
        }
        assert_eq!(asked.len(), 2, "one request each: {asked:?}");
        let id = asked[0].1.clone();
        assert!(asked.iter().all(|(_, other)| *other == id), "one id for both: {asked:?}");
        let clients: BTreeSet<&str> = asked.iter().map(|(client, _)| client.as_str()).collect();
        assert_eq!(clients, BTreeSet::from(["a", "b"]));

        let answers: Vec<usize> = (0..lines.len())
            .filter(|&at| lines[at].sent && lines[at].msg.get("result").is_some())
            .collect();
        let [first, second] = answers[..] else { panic!("two answers: {answers:?}") };
        assert_eq!(lines[first].client, "b", "the follower answered first");
        assert_eq!(lines[first].msg["result"], json!({ "decision": "accept" }));
        let resolved: Vec<&str> = lines[first..second]
            .iter()
            .filter_map(|line| match heard(line) {
                Some(Incoming::Notification { note, .. }) => match *note {
                    ServerNotification::ServerRequestResolved(done) => {
                        assert_eq!(done.request_id, id);
                        Some(line.client.as_str())
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(resolved.iter().copied().collect::<BTreeSet<_>>(), clients, "both hear it");
        let completed = lines[first..second]
            .iter()
            .filter(|line| !line.sent && line.msg["method"] == "turn/completed")
            .count();
        assert_eq!(completed, 2, "the turn went on to its end, for both");
        assert_eq!(lines[second].client, "a");
        let after: Vec<&Value> = lines[second..]
            .iter()
            .filter(|line| {
                !line.sent && line.msg.get("id") == Some(&serde_json::to_value(&id).unwrap())
            })
            .map(|line| &line.msg)
            .collect();
        assert!(after.is_empty(), "a late answer is never answered: {after:?}");
    }

    /// One client's view of the recorded thread, mapped: the thread its `thread/start` or
    /// `thread/resume` brought back, then every request and notification it heard about it.
    /// `answer` is how the client answers the approval, when it does: through the mapper, as
    /// the face would, which must send what the recording sent. With the statuses and every
    /// action applied, in order.
    fn mapped(client: &str, answer: Option<&str>) -> (ThreadState, Vec<Status>, Vec<Action>) {
        let lines = approval();
        let mut shared: Option<Shared> = None;
        let mut state: Option<ThreadState> = None;
        let mut statuses = Vec::new();
        let mut applied = Vec::new();
        let now = WallMs::from_millis(1);
        for line in lines.iter().filter(|line| line.client == client) {
            if line.sent {
                if line.msg.get("result").is_some()
                    && let (Some(choice), Some(shared)) = (answer, shared.as_mut())
                {
                    let id = serde_json::from_value(line.msg["id"].clone()).unwrap();
                    let by = Answerer { client: None, name: "Slopty".to_owned() };
                    let (to, result) = shared.answer(&shared::ask_of(&id), choice, by).unwrap();
                    assert_eq!(
                        (to, result),
                        (id, line.msg["result"].clone()),
                        "as the TUI answers"
                    );
                }
                continue;
            }
            let actions = match rpc::read(&line.msg.to_string()).unwrap() {
                Incoming::Answer { outcome: Ok(result), .. } if result.get("thread").is_some() => {
                    let thread: p::Thread =
                        serde_json::from_value(result["thread"].clone()).unwrap();
                    let (begun, actions) = Shared::new(&thread, None);
                    state = Some(ThreadState::new(begun.meta().clone()));
                    shared = Some(begun);
                    actions
                }
                Incoming::Request { id, request, .. } => {
                    shared.as_mut().unwrap().request(&id, &request, now)
                }
                Incoming::Notification { note, .. } => match shared.as_mut() {
                    Some(shared) => shared.notification(&note, now),
                    None => Vec::new(),
                },
                _ => Vec::new(),
            };
            let Some(state) = state.as_mut() else { continue };
            for action in &actions {
                state.apply(action);
                if let Action::Status(status) = action {
                    statuses.push(status.clone());
                }
            }
            applied.extend(actions);
        }
        (state.unwrap(), statuses, applied)
    }

    fn texts(state: &ThreadState) -> Vec<String> {
        state
            .items
            .iter()
            .map(|item| match &item.body {
                ItemBody::User(message) => format!("user: {}", message.text.text),
                ItemBody::Text(text) => format!("text: {}", text.text),
                ItemBody::Tool(call) => format!("{} {:?}: {}", call.kind, call.state, call.title),
                ItemBody::Notice(notice) => format!("{}: {}", notice.kind, notice.text.text),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// The thread as the client that started it saw it: two turns, the command it approved
    /// elsewhere in its place, and the request answered by the TUI.
    #[test]
    fn the_starter_sees_the_thread_and_the_approval_settled_elsewhere() {
        let (state, statuses, applied) = mapped("a", None);
        assert_eq!(state.meta.drive, Drive::named(Drive::SHARED));
        assert_eq!(state.meta.agent, AgentId::named(AgentId::CODEX));
        assert_eq!(state.meta.agent_version, "0.160.0");
        let warned = "info: Model metadata for `mock-model` not found. Defaulting to fallback \
                      metadata; this can degrade performance and cause issues.";
        assert_eq!(
            texts(&state),
            [
                warned,
                "user: Say hello.",
                "text: Hello.",
                warned,
                "user: Make a file called made-by-codex.",
                "exec Completed: Run touch made-by-codex",
                "text: Made it.",
            ]
        );
        let turns: Vec<(TurnId, TurnState)> =
            state.turns.iter().map(|t| (t.id, t.state.clone())).collect();
        assert_eq!(turns, [(TurnId(1), TurnState::Complete), (TurnId(2), TurnState::Complete)]);
        let inputs: Vec<Option<&str>> =
            state.turns.iter().map(|t| t.input.as_ref().map(|i| i.0.as_str())).collect();
        let users: Vec<Option<&str>> = state
            .items
            .iter()
            .filter(|i| matches!(i.body, ItemBody::User(_)))
            .map(|i| Some(i.id.0.as_str()))
            .collect();
        assert_eq!(inputs, users, "each turn names the message that started it");
        let [request] = &*state.requests else { panic!("one request: {:?}", state.requests) };
        assert_eq!(request.kind, Request::APPROVAL);
        assert_eq!(request.title, "Run touch made-by-codex?");
        assert_eq!(state.meta.title, "Say hello.", "named by its first prompt, as Codex did not");
        // The call begun before Codex asked about it waits on the person while it asks.
        let call_states: Vec<String> = applied
            .iter()
            .filter_map(|action| match action {
                Action::ItemStarted(item)
                | Action::ItemUpdated(item)
                | Action::ItemCompleted(item) => match &item.body {
                    ItemBody::Tool(call) => Some(format!("{:?}", call.state)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let pending = format!("{:?}", ToolState::Pending { ask: request.id.clone() });
        assert_eq!(call_states, ["Running", pending.as_str(), "Running", "Completed"]);
        let exec = state.items.iter().find_map(|item| match &item.body {
            ItemBody::Tool(call) => match &call.detail {
                Some(ToolDetail::Exec(exec)) => Some(exec.command.text.clone()),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(exec.as_deref(), Some("touch made-by-codex"), "the shell's wrapper taken off");
        let offered: Vec<(&str, Effect, bool)> =
            request.options.iter().map(|c| (c.id.as_str(), c.effect, c.stops)).collect();
        assert_eq!(
            offered,
            [
                ("accept", Effect::Allow, false),
                ("acceptWithExecpolicyAmendment", Effect::Allow, false),
                ("cancel", Effect::Deny, true),
            ]
        );
        let by = Answerer { client: None, name: shared::ELSEWHERE.to_owned() };
        assert_eq!(request.state, RequestState::Answered { by, choice: String::new() });
        let asked =
            statuses.iter().rfind(|s| s.phase == Phase::NeedsYou).expect("it needed the person");
        assert_eq!(asked.wait.as_ref().map(|w| w.text.as_str()), Some(request.title.as_str()));
        assert_eq!(state.status.phase, Phase::Done);
    }

    /// The client that answered first sees its own answer settle the request, and what it
    /// sent is what the recording sent: the TUI's own JSON-RPC answer. It joined after the
    /// first turn, which its `thread/resume` brought back.
    #[test]
    fn the_first_answer_from_here_is_named_as_ours() {
        let (state, ..) = mapped("b", Some("accept"));
        let [request] = &*state.requests else { panic!("one request: {:?}", state.requests) };
        let by = Answerer { client: None, name: "Slopty".to_owned() };
        assert_eq!(request.state, RequestState::Answered { by, choice: "accept".to_owned() });
        let (started, ..) = mapped("a", None);
        // Codex's warnings are said once, to whoever is there: a resume does not bring them back.
        let kept = |state: &ThreadState| -> Vec<String> {
            texts(state).into_iter().filter(|t| !t.starts_with("info: ")).collect()
        };
        assert_eq!(kept(&state), kept(&started), "the first turn came back with the resume");
    }

    /// Codex's questions, as the pinned build asked them in Plan mode
    /// (`tests/fixtures/codex/question.jsonl`): a request carrying them, each with the answers
    /// Codex offers, answered by the one choice the questionnaire makes for them all, which goes
    /// back under each question's id exactly as the recording sent it and Codex took it; an
    /// answer that leaves one out goes nowhere.
    #[test]
    fn codex_questions_are_carried_and_answered_by_their_ids() {
        use slopty_proto::thread::detail::Answer;
        let lines = fixture("question.jsonl");
        assert!(lines.iter().all(|l| l.client == "a"), "one client asks and answers");
        let by = Answerer { client: None, name: "Slopty".to_owned() };
        let now = WallMs::from_millis(1);
        let mut shared: Option<Shared> = None;
        let mut state: Option<ThreadState> = None;
        let mut answered = 0;
        for line in &lines {
            if line.sent {
                if line.msg.get("result").is_none() {
                    continue;
                }
                reads_back::<p::ToolRequestUserInputResponse>("the answers", &line.msg["result"]);
                let (shared, state) = (shared.as_mut().unwrap(), state.as_ref().unwrap());
                let id = serde_json::from_value(line.msg["id"].clone()).unwrap();
                let ask = shared::ask_of(&id);
                let request = state.requests.iter().find(|r| r.id == ask).unwrap();
                assert_eq!(request.kind, Request::QUESTION);
                let asked = &request.questions;
                let answer = |q: usize, a: &str| Answer {
                    question: asked[q].text.clone(),
                    answer: a.into(),
                };
                let partial = Answer::choice(asked, &[answer(0, &asked[0].options[0].label)]);
                assert_eq!(shared.answer(&ask, &partial, by.clone()), None, "each is answered");
                let given = [answer(0, &asked[0].options[0].label), answer(1, "plans.md")];
                let choice = Answer::choice(asked, &given);
                let sent = shared.answer(&ask, &choice, by.clone()).unwrap();
                assert_eq!(sent, (id, line.msg["result"].clone()), "as the recording sent it");
                answered += 1;
                continue;
            }
            let actions = match rpc::read(&line.msg.to_string()).unwrap() {
                Incoming::Answer { outcome: Ok(result), .. } if result.get("thread").is_some() => {
                    let thread: p::Thread =
                        serde_json::from_value(result["thread"].clone()).unwrap();
                    let (begun, actions) = Shared::new(&thread, None);
                    state = Some(ThreadState::new(begun.meta().clone()));
                    shared = Some(begun);
                    actions
                }
                Incoming::Request { id, request, .. } => {
                    reads_back::<p::ToolRequestUserInputParams>(
                        "the questions",
                        &line.msg["params"],
                    );
                    shared.as_mut().unwrap().request(&id, &request, now)
                }
                Incoming::Notification { note, .. } => match shared.as_mut() {
                    Some(shared) => shared.notification(&note, now),
                    None => Vec::new(),
                },
                _ => Vec::new(),
            };
            if let Some(state) = state.as_mut() {
                for action in &actions {
                    state.apply(action);
                }
            }
        }
        assert_eq!(answered, 1);
        let state = state.unwrap();
        let [request] = &*state.requests else { panic!("one request: {:?}", state.requests) };
        assert_eq!(request.title, "2 questions");
        let asked: Vec<(&str, Option<&str>, Vec<&str>)> = request
            .questions
            .iter()
            .map(|q| {
                let labels = q.options.iter().map(|o| o.label.as_str()).collect();
                (q.text.as_str(), q.header.as_deref(), labels)
            })
            .collect();
        let want = [
            ("Which layout?", Some("Layout"), vec!["Split (Recommended)", "Tabs"]),
            ("What should the file be called?", Some("Name"), vec!["notes.md", "plan.md"]),
        ];
        assert_eq!(asked, want);
        assert_eq!(
            request.questions[0].options[0].description.as_deref(),
            Some("Two panes side by side.")
        );
        let RequestState::Answered { by: who, .. } = &request.state else {
            panic!("settled: {:?}", request.state)
        };
        assert_eq!(*who, by, "answered from here");
        assert!(texts(&state).iter().any(|t| t == "text: Split it is."), "{:?}", texts(&state));
        assert_eq!(state.turns.last().map(|t| &t.state), Some(&TurnState::Complete));
    }

    /// A Codex question for a secret is not carried: its answer would be kept in the thread's
    /// log, so the card says to answer it in Codex's own terminal. The model cannot mark one
    /// secret, so the recorded request is the base and only `isSecret` is set.
    #[test]
    fn a_codex_question_for_a_secret_is_left_to_its_terminal() {
        let lines = fixture("question.jsonl");
        let started = lines.iter().find(|l| l.msg["result"].get("thread").is_some()).unwrap();
        let thread: p::Thread =
            serde_json::from_value(started.msg["result"]["thread"].clone()).unwrap();
        let (mut shared, _) = Shared::new(&thread, None);
        let asked = lines
            .iter()
            .find(|l| l.msg.get("method").is_some_and(|m| m == "item/tool/requestUserInput"))
            .unwrap();
        let mut msg = asked.msg.clone();
        msg["params"]["questions"][1]["isSecret"] = json!(true);
        let Incoming::Request { id, request, .. } = rpc::read(&msg.to_string()).unwrap() else {
            panic!("a request")
        };
        let mut state = ThreadState::new(shared.meta().clone());
        for action in &shared.request(&id, &request, WallMs::from_millis(1)) {
            state.apply(action);
        }
        let secret = state.requests.last().unwrap();
        assert!(secret.questions.is_empty() && secret.options.is_empty(), "nothing to answer here");
        assert!(secret.text.as_ref().is_some_and(|t| t.text.contains("Codex's own terminal")));
    }

    /// The recorded thread, begun, with a state that mirrors it.
    fn begun() -> (Shared, ThreadState) {
        let lines = fixture("question.jsonl");
        let started = lines.iter().find(|l| l.msg["result"].get("thread").is_some()).unwrap();
        let thread: p::Thread =
            serde_json::from_value(started.msg["result"]["thread"].clone()).unwrap();
        let (shared, actions) = Shared::new(&thread, None);
        let mut state = ThreadState::new(shared.meta().clone());
        for action in &actions {
            state.apply(action);
        }
        (shared, state)
    }

    /// The notification `method` with `params`, heard by `shared` and applied to `state`.
    fn hear(shared: &mut Shared, state: &mut ThreadState, method: &str, params: &Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let Incoming::Notification { note, .. } = rpc::read(&msg.to_string()).unwrap() else {
            panic!("a notification: {msg}")
        };
        for action in &shared.notification(&note, WallMs::from_millis(1)) {
            state.apply(action);
        }
    }

    fn notices(state: &ThreadState) -> Vec<Notice> {
        state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Notice(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    /// An error Codex will retry is a notice counting the turn's attempts, since Codex numbers
    /// none and says no wait; the error it gives up on is a plain one.
    #[test]
    fn an_error_codex_retries_counts_its_attempts() {
        let (mut shared, mut state) = begun();
        let thread = shared.meta().native.clone();
        let error = |retry: bool, message: &str| {
            json!({"threadId": thread, "turnId": "t1", "willRetry": retry,
                "error": {"message": message}})
        };
        hear(&mut shared, &mut state, "error", &error(true, "stream disconnected"));
        hear(&mut shared, &mut state, "error", &error(true, "stream disconnected"));
        hear(&mut shared, &mut state, "error", &error(false, "gave up"));
        let said: Vec<(String, Option<u32>)> = notices(&state)
            .into_iter()
            .map(|n| (n.text.text, n.retry.map(|r| r.attempt)))
            .collect();
        assert_eq!(
            said,
            [
                ("stream disconnected".to_owned(), Some(2)),
                ("stream disconnected".to_owned(), Some(3)),
                ("gave up".to_owned(), None),
            ]
        );
        let retry = notices(&state)[0].retry.unwrap();
        assert_eq!((retry.max, retry.in_ms), (None, None), "Codex says neither");
    }

    /// A turn a usage limit stopped fails with when the full window resets, its error a limit
    /// notice; a turn any other error ended fails with no reset.
    #[test]
    fn a_turn_a_usage_limit_stopped_fails_until_it_resets() {
        let stopped = |info: &str| {
            let (mut shared, mut state) = begun();
            let turn = turn_begun(&mut shared, &mut state);
            let thread = shared.meta().native.clone();
            hear(
                &mut shared,
                &mut state,
                "account/rateLimits/updated",
                &json!({"rateLimits": {
                "primary": {"usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1_800_000_000},
                "secondary": {"usedPercent": 30, "windowDurationMins": 10_080, "resetsAt": 1_800_500_000}}}),
            );
            let error = json!({"message": "You've hit your usage limit.", "codexErrorInfo": info});
            hear(
                &mut shared,
                &mut state,
                "error",
                &json!({"threadId": thread, "turnId": turn, "willRetry": false, "error": error}),
            );
            hear(
                &mut shared,
                &mut state,
                "turn/completed",
                &json!({"threadId": thread, "turn": {"completedAt": 2, "durationMs": 1,
                    "error": error, "id": turn, "items": [], "itemsView": "notLoaded",
                    "startedAt": 0, "status": "failed"}}),
            );
            let kinds: Vec<String> = notices(&state).into_iter().map(|n| n.kind).collect();
            (state.turns.last().unwrap().state.clone(), kinds)
        };
        let (state, kinds) = stopped("usageLimitExceeded");
        assert_eq!(
            state,
            TurnState::Failed {
                error: "You've hit your usage limit.".to_owned(),
                until_ms: Some(WallMs::from_millis(1_800_000_000_000)),
            }
        );
        assert_eq!(kinds, [Notice::LIMIT]);
        let (state, kinds) = stopped("serverOverloaded");
        assert!(matches!(state, TurnState::Failed { until_ms: None, .. }), "{state:?}");
        assert_eq!(kinds, [Notice::API_ERROR]);
    }

    /// A request open on an active thread waits on the person, though Codex flagged no wait
    /// (a build that says it late, or not at all); it is worded by what it asks.
    #[test]
    fn an_open_request_waits_on_the_person_whatever_the_flags() {
        let (mut shared, mut state) = begun();
        let thread = shared.meta().native.clone();
        let active = json!({"threadId": thread, "status": {"type": "active", "activeFlags": []}});
        hear(&mut shared, &mut state, "thread/status/changed", &active);
        assert_eq!(state.status.phase, Phase::Working);
        let asked = approval()
            .into_iter()
            .find(|l| !l.sent && l.msg["method"] == "item/commandExecution/requestApproval")
            .expect("the recorded approval");
        let Incoming::Request { id, request, .. } = rpc::read(&asked.msg.to_string()).unwrap()
        else {
            panic!("a request: {}", asked.msg)
        };
        for action in &shared.request(&id, &request, WallMs::from_millis(2)) {
            state.apply(action);
        }
        assert_eq!(state.status.phase, Phase::NeedsYou);
        let wait = state.status.wait.as_ref().map(|w| (w.kind.as_str(), w.text.as_str()));
        assert_eq!(wait, Some(("permission", "Run touch made-by-codex?")));
    }

    /// A turn begun on the recorded thread, as Codex says it.
    fn turn_begun(shared: &mut Shared, state: &mut ThreadState) -> String {
        let thread = shared.meta().native.clone();
        let turn = "00000000-0000-7000-8000-0000000000aa".to_owned();
        let params = json!({"threadId": thread, "turn": {"completedAt": null, "durationMs": null,
            "error": null, "id": turn, "items": [], "itemsView": "notLoaded", "startedAt": 0,
            "status": "inProgress"}});
        hear(shared, state, "turn/started", &params);
        turn
    }

    /// What a turn changed is the whole turn's diff as Codex sends it, counted again each time
    /// it moves, across files.
    #[test]
    fn a_turns_diff_is_what_it_changed() {
        let (mut shared, mut state) = begun();
        let turn = turn_begun(&mut shared, &mut state);
        let thread = shared.meta().native.clone();
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,3 @@\n one\n-two\n+2\n+three\n\
                    diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -4 +4 @@\n-x\n+y\n";
        hear(
            &mut shared,
            &mut state,
            "turn/diff/updated",
            &json!({"threadId": thread, "turnId": turn, "diff": diff}),
        );
        let last = state.turns.last().unwrap();
        assert_eq!((last.changed.added, last.changed.removed), (3, 2));
    }

    /// A review is Codex's own reviewer over a commit's changes, in the thread (`review/start`,
    /// inline), and never while a turn is under way. What it found is its answer, in Codex's
    /// words, so a client reads the findings where it reads any answer.
    #[test]
    fn a_review_asks_codexs_reviewer_and_its_findings_are_its_answer() {
        let (mut shared, mut state) = begun();
        assert!(shared.meta().can(slopty_proto::thread::Cap::REVIEW));
        let asked = shared.review("9d1e7aa", "Changes in turn 3").unwrap();
        assert_eq!(
            serde_json::to_value(&asked).unwrap(),
            json!({"threadId": shared.meta().native, "delivery": "inline",
                "target": {"type": "commit", "sha": "9d1e7aa", "title": "Changes in turn 3"}})
        );
        let turn = turn_begun(&mut shared, &mut state);
        assert!(shared.review("9d1e7aa", "again").is_err(), "not while it works");

        let thread = shared.meta().native.clone();
        let found = "One finding.\n\nReview comment:\n\n- [P1] Off by one \u{2014} /repo/src/a.rs:3-4\n  The loop skips the last line.";
        hear(
            &mut shared,
            &mut state,
            "item/completed",
            &json!({"threadId": thread, "turnId": turn, "completedAtMs": 1,
                "item": {"type": "exitedReviewMode", "id": "r1", "review": found}}),
        );
        assert_eq!(texts(&state).last(), Some(&format!("text: {found}")));
    }

    /// An edit's hunks keep the heading Codex's diff names after their ranges.
    #[test]
    fn an_edits_hunks_keep_their_heading() {
        let (mut shared, mut state) = begun();
        let turn = turn_begun(&mut shared, &mut state);
        let thread = shared.meta().native.clone();
        let diff = "@@ -1 +1 @@\n-use std::io;\n+use std::fmt;\n@@ -9,3 +9,3 @@ impl Client {\n a\n-b\n+c\n d\n";
        let item = json!({"type": "fileChange", "id": "f1", "status": "completed",
            "changes": [{"path": "/w/client.rs", "kind": {"type": "update"}, "diff": diff}]});
        hear(
            &mut shared,
            &mut state,
            "item/completed",
            &json!({"threadId": thread, "turnId": turn, "item": item, "completedAtMs": 5}),
        );
        let edit = state
            .items
            .iter()
            .find_map(|i| match &i.body {
                ItemBody::Tool(call) => match &call.detail {
                    Some(ToolDetail::Edit(edit)) => Some(edit.clone()),
                    _ => None,
                },
                _ => None,
            })
            .expect("the edit");
        let headings: Vec<Option<&str>> =
            edit.patch.hunks.iter().map(|h| h.heading.as_deref()).collect();
        assert_eq!(headings, [None, Some("impl Client {")]);
        assert_eq!(edit.patch.hunks[1].lines, [" a", "-b", "+c", " d"]);
    }

    /// The account's windows are the thread's limits, named by their length; a turn Codex moves
    /// to another model counts that model and says why; a warning is a notice.
    #[test]
    fn limits_reroutes_and_warnings_are_carried() {
        let (mut shared, mut state) = begun();
        let turn = turn_begun(&mut shared, &mut state);
        let thread = shared.meta().native.clone();
        hear(
            &mut shared,
            &mut state,
            "account/rateLimits/updated",
            &json!({"rateLimits": {
            "primary": {"usedPercent": 42, "windowDurationMins": 300, "resetsAt": 1_800_000_000},
            "secondary": {"usedPercent": 7, "windowDurationMins": 10_080, "resetsAt": null}}}),
        );
        let limits: Vec<(String, u32)> =
            state.meters.limits.iter().map(|l| (l.name.clone(), l.used_bp)).collect();
        assert_eq!(limits, [("five-hour".to_owned(), 4_200), ("seven-day".to_owned(), 700)]);
        assert_eq!(state.meters.limits[0].resets_ms, Some(WallMs::from_millis(1_800_000_000_000)));

        hear(
            &mut shared,
            &mut state,
            "model/rerouted",
            &json!({"threadId": thread,
            "turnId": turn, "fromModel": "gpt-6", "toModel": "gpt-6-safe",
            "reason": "highRiskCyberActivity"}),
        );
        assert!(state.turns.last().unwrap().models.contains(&"gpt-6-safe".to_owned()));
        hear(
            &mut shared,
            &mut state,
            "warning",
            &json!({"threadId": thread, "message": "Config is deprecated"}),
        );
        let said: Vec<String> = notices(&state).into_iter().map(|n| n.text.text).collect();
        assert!(said[0].contains("gpt-6-safe"), "{said:?}");
        assert_eq!(said[1], "Config is deprecated");
    }

    /// A spawn call starts a subagent's thread: the call names the child it started, with what
    /// it was told; a thread Codex says has a parent is a subagent's.
    #[test]
    fn a_spawn_call_names_its_subagents_thread() {
        let (mut shared, mut state) = begun();
        let turn = turn_begun(&mut shared, &mut state);
        let thread = shared.meta().native.clone();
        let child = "00000000-0000-7000-8000-0000000000cc";
        let item = json!({"type": "collabAgentToolCall", "id": "c1", "tool": "spawnAgent",
            "status": "completed", "senderThreadId": thread, "receiverThreadIds": [child],
            "prompt": "Read the tests", "agentsStates": {child: {"status": "running"}}});
        hear(
            &mut shared,
            &mut state,
            "item/completed",
            &json!({"threadId": thread,
            "turnId": turn, "item": item, "completedAtMs": 5}),
        );
        let call = state
            .items
            .iter()
            .find_map(|i| match &i.body {
                ItemBody::Tool(call) => Some(call.clone()),
                _ => None,
            })
            .expect("the call");
        assert_eq!(call.kind, slopty_proto::thread::kind::AGENT);
        assert_eq!(call.child, Some(shared::thread_of(child)));
        let Some(ToolDetail::Agent(agent)) = &call.detail else { panic!("{call:?}") };
        assert_eq!(agent.prompt.text, "Read the tests");

        let link = slopty_proto::thread::Link {
            thread: shared.meta().id,
            item: slopty_proto::thread::ItemId("c1".to_owned()),
        };
        let mut sub = begun().0;
        let adopted = sub.adopted(link.clone());
        assert!(
            matches!(adopted.as_slice(), [Action::Meta(meta)] if meta.parent == Some(link.clone()))
        );
        assert!(sub.adopted(link).is_empty(), "once");
        assert_eq!(sub.meta().origin, slopty_proto::thread::ThreadMeta::SUBAGENT);
    }

    /// Codex's approval policy is the thread's mode and its sandbox a fact; its reasoning
    /// effort is the meters' effort.
    #[test]
    fn the_threads_settings_are_its_mode_and_effort() {
        let lines = fixture("question.jsonl");
        let started = lines.iter().find(|l| l.msg["result"].get("thread").is_some()).unwrap();
        let mut thread = started.msg["result"]["thread"].clone();
        thread["reasoningEffort"] = json!("high");
        let thread: p::Thread = serde_json::from_value(thread).unwrap();
        let (mut shared, actions) = Shared::new(&thread, None);
        let mut state = ThreadState::new(shared.meta().clone());
        for action in &actions {
            state.apply(action);
        }
        assert_eq!(state.meters.effort.as_deref(), Some("high"));
        let approval: p::AskForApproval = serde_json::from_value(json!("on-request")).unwrap();
        let sandbox: p::SandboxPolicy =
            serde_json::from_value(json!({"type": "workspaceWrite"})).unwrap();
        let settings = shared::Settings {
            approval,
            sandbox: &sandbox,
            model: thread.model.as_deref().unwrap_or_default(),
            effort: Some("high"),
        };
        for action in &shared.settings(&settings) {
            state.apply(action);
        }
        assert_eq!(state.meters.mode.as_deref(), Some("on-request"));
        assert_eq!(state.meta.facts.get("sandbox").map(String::as_str), Some("workspaceWrite"));
        assert!(shared.settings(&settings).is_empty(), "nothing moved");
    }

    /// Codex's model catalog as `model/list` answers it (Codex 0.160.0's schema): two models
    /// with their reasoning efforts, and a hidden one.
    fn catalog(running: &str) -> Vec<p::Model> {
        let model = |slug: &str, name: &str, hidden: bool, efforts: &[&str], default: &str| {
            json!({
                "id": slug, "model": slug, "displayName": name, "description": "",
                "hidden": hidden, "isDefault": false, "defaultReasoningEffort": default,
                "supportedReasoningEfforts": efforts.iter().map(|e| json!({
                    "reasoningEffort": e, "description": format!("{e} effort")
                })).collect::<Vec<_>>(),
            })
        };
        let list = json!({ "data": [
            model(running, "Running", false, &["low", "medium", "high", "xhigh"], "medium"),
            model("gpt-mini", "Mini", false, &["low", "medium"], "low"),
            model("gpt-secret", "Secret", true, &["high"], "high"),
        ], "nextCursor": null });
        serde_json::from_value::<p::ModelListResponse>(list).unwrap().data
    }

    /// What Codex offers is what the thread can be switched to: its models but the hidden one,
    /// the running model's efforts, and the three approval policies as its modes. A switch
    /// names only what it changes (`thread/settings/update`); a model that lacks the thread's
    /// effort goes with its own default effort, as Codex's own picker sets it. Anything not
    /// offered is refused in words, and a switch Codex took shows in the meters at once.
    #[test]
    fn a_thread_switches_among_what_codex_offers() {
        use shared::Setting;
        let lines = fixture("question.jsonl");
        let started = lines.iter().find(|l| l.msg["result"].get("thread").is_some()).unwrap();
        let mut thread = started.msg["result"]["thread"].clone();
        thread["reasoningEffort"] = json!("high");
        thread["model"] = json!("mock-model");
        let thread: p::Thread = serde_json::from_value(thread).unwrap();
        let (mut shared, actions) = Shared::new(&thread, None);
        let mut state = ThreadState::new(shared.meta().clone());
        for action in actions.iter().chain(&shared.models(&catalog("mock-model"))) {
            state.apply(action);
        }
        let models: Vec<&str> = state.meta.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(models, ["mock-model", "gpt-mini"], "the hidden model is left out");
        assert_eq!(state.meters.model.as_deref(), Some("Running"), "its name for people");
        let efforts: Vec<(&str, &str)> =
            state.meta.efforts.iter().map(|e| (e.id.as_str(), e.label.as_str())).collect();
        assert_eq!(
            efforts,
            [("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "Extra high")]
        );
        let modes: Vec<&str> = state.meta.modes.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(modes, ["untrusted", "on-request", "never"]);
        for cap in ["set-model", "set-effort", "set-mode"] {
            assert!(state.meta.can(cap), "{cap}");
        }

        let to_mini = shared.switch(&Setting::Model("gpt-mini".to_owned())).unwrap();
        let sent = serde_json::to_value(&to_mini).unwrap();
        let native = &thread.id;
        assert_eq!(
            sent,
            json!({ "threadId": native, "model": "gpt-mini", "effort": "low" }),
            "high is not Mini's, so Mini's own default goes with it"
        );
        let effort = shared.switch(&Setting::Effort("xhigh".to_owned())).unwrap();
        assert_eq!(
            serde_json::to_value(&effort).unwrap(),
            json!({ "threadId": native, "effort": "xhigh" })
        );
        let mode = shared.switch(&Setting::Mode("never".to_owned())).unwrap();
        assert_eq!(
            serde_json::to_value(&mode).unwrap(),
            json!({ "threadId": native, "approvalPolicy": "never" })
        );
        for refused in [
            Setting::Model("gpt-secret".to_owned()),
            Setting::Effort("max".to_owned()),
            Setting::Mode("granular".to_owned()),
        ] {
            let why = shared.switch(&refused).unwrap_err();
            assert!(!why.is_empty(), "{refused:?}");
        }

        for action in &shared.switched(&to_mini) {
            state.apply(action);
        }
        assert_eq!(state.meters.model_id.as_deref(), Some("gpt-mini"));
        assert_eq!(state.meters.model.as_deref(), Some("Mini"));
        assert_eq!(state.meters.effort.as_deref(), Some("low"));
        let efforts: Vec<&str> = state.meta.efforts.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(efforts, ["low", "medium"], "the efforts of the model it runs now");

        // What another client switched, as Codex tells every client.
        let said =
            lines.iter().find(|l| !l.sent && l.msg["method"] == "thread/settings/updated").unwrap();
        let Incoming::Notification { note, .. } = rpc::read(&said.msg.to_string()).unwrap() else {
            panic!("a notification");
        };
        for action in &shared.notification(&note, WallMs::ZERO) {
            state.apply(action);
        }
        let held = &said.msg["params"]["threadSettings"];
        assert_eq!(state.meters.model_id.as_deref(), held["model"].as_str());
        assert_eq!(state.meters.mode.as_deref(), held["approvalPolicy"].as_str());
        assert_eq!(state.meters.effort.as_deref(), held["effort"].as_str());
    }

    /// A fork names Codex's own id of the turn it branches after, or none for the whole thread,
    /// or of the turn it branches before for an edit from there; a turn Codex does not hold, or
    /// the one under way, is refused in words. A thread Codex
    /// forked says which it came from, as Codex records it, with no turn.
    #[test]
    fn a_fork_names_codexs_turn_and_a_forked_thread_says_where_it_came_from() {
        use slopty_proto::thread::{Fork, ThreadMeta, TurnId};
        let (mut shared, mut state) = begun();
        let whole = shared.fork(None).unwrap();
        assert_eq!(
            (whole.thread_id.as_str(), whole.last_turn_id),
            (shared.meta().native.as_str(), None)
        );
        let started = recorded_note("question.jsonl", "turn/started");
        let codex_turn = started["turn"]["id"].as_str().unwrap().to_owned();
        hear(&mut shared, &mut state, "turn/started", &started);
        let turn = state.last_turn().unwrap().id;
        assert!(shared.fork(Some(turn)).is_err(), "not through the turn under way");
        hear(
            &mut shared,
            &mut state,
            "turn/completed",
            &recorded_note("question.jsonl", "turn/completed"),
        );
        assert_eq!(shared.fork(Some(turn)).unwrap().last_turn_id, Some(codex_turn.clone()));
        assert!(shared.fork(Some(TurnId(99))).is_err(), "no such turn");
        let before = shared.fork_before(turn).unwrap();
        assert_eq!((before.before_turn_id, before.last_turn_id), (Some(codex_turn), None));
        assert!(shared.fork_before(TurnId(99)).is_err(), "no such turn");

        let lines = fixture("question.jsonl");
        let started = lines.iter().find(|l| l.msg["result"].get("thread").is_some()).unwrap();
        let mut thread = started.msg["result"]["thread"].clone();
        thread["forkedFromId"] = json!("019a-parent");
        let thread: p::Thread = serde_json::from_value(thread).unwrap();
        let (forked, _) = Shared::new(&thread, None);
        assert_eq!(
            forked.meta().forked_from,
            Some(Fork { thread: shared::thread_of("019a-parent"), turn: None })
        );
        assert_eq!(forked.meta().origin, ThreadMeta::FORK);
    }

    /// A folder's past threads are asked of `thread/list` by the folder, the latest first, and
    /// each is listed by Codex's id with the words that take it up again.
    #[test]
    fn past_threads_are_listed_with_the_words_that_resume_them() {
        let asked = serde_json::to_value(shared::list("/work", 25)).unwrap();
        assert_eq!(asked, json!({"cwd": "/work", "limit": 25, "sortKey": "updated_at"}));
        let lines = fixture("question.jsonl");
        let started = lines.iter().find(|l| l.msg["result"].get("thread").is_some()).unwrap();
        let mut thread = started.msg["result"]["thread"].clone();
        thread["name"] = json!("Fix the build");
        thread["updatedAt"] = json!(1_700_000_000);
        let thread: p::Thread = serde_json::from_value(thread).unwrap();
        let past = shared::past(&thread);
        assert_eq!(past.native, thread.id);
        assert_eq!(past.title.as_deref(), Some("Fix the build"));
        assert_eq!(past.updated_ms, Some(WallMs::from_millis(1_700_000_000_000)));
        assert_eq!(past.resume, shared::resume_args(&thread.id));
        assert_eq!(shared::resumed(&past.resume), Some(thread.id.as_str()));
    }

    /// The recorded notification `method` of `name`'s fixture, as its params.
    /// A call keeps when it began once Codex says it is whole, and ends when Codex says; a hook
    /// that blocked, failed or left words is a hook notice in its turn, one that ran quietly is
    /// none; an MCP server that did not start says why, or that it needs signing in again.
    #[test]
    fn calls_keep_their_times_and_hooks_and_mcp_failures_are_said() {
        let (mut shared, mut state) = begun();
        let turn = recorded_note("question.jsonl", "turn/started");
        hear(&mut shared, &mut state, "turn/started", &turn);
        let turn_id = turn["turn"]["id"].clone();
        let thread = turn["threadId"].clone();
        let mut call = json!({"aggregatedOutput": null, "command": "cargo test",
            "commandActions": [], "cwd": "/work", "durationMs": null, "exitCode": null,
            "id": "call-9", "pluginId": null, "processId": null, "scriptPath": null,
            "source": "agent", "status": "inProgress", "type": "commandExecution"});
        let started = json!({"item": call, "startedAtMs": 1_000, "threadId": thread,
            "turnId": turn_id});
        hear(&mut shared, &mut state, "item/started", &started);
        call["status"] = json!("completed");
        call["exitCode"] = json!(0);
        let done = json!({"item": call, "completedAtMs": 4_000, "threadId": thread,
            "turnId": turn_id});
        hear(&mut shared, &mut state, "item/completed", &done);
        let item = state.items.iter().find(|i| i.id.0 == "call-9").unwrap();
        assert_eq!(item.at_ms, WallMs::from_millis(1_000), "when it began");
        let ItemBody::Tool(tool) = &item.body else { panic!("a call") };
        assert_eq!(tool.ended_ms, Some(WallMs::from_millis(4_000)), "when it ended");

        let run = |id: &str, status: &str, message: Value, entries: Value| {
            json!({"threadId": thread, "turnId": turn_id, "run": {
                "id": id, "displayOrder": 0, "entries": entries, "eventName": "preToolUse",
                "executionMode": "sync", "handlerType": "command", "scope": "thread",
                "sourcePath": "/work/.codex/hooks.json", "startedAt": 5_000,
                "completedAt": 5_200, "status": status, "statusMessage": message}})
        };
        let quiet = run("h1", "completed", Value::Null, json!([]));
        hear(&mut shared, &mut state, "hook/completed", &quiet);
        let blocked = run(
            "h2",
            "blocked",
            json!("rm -rf is not allowed"),
            json!([{"kind": "feedback", "text": "Use trash instead"}]),
        );
        hear(&mut shared, &mut state, "hook/completed", &blocked);
        let mcp = |name: &str, status: &str, error: Value, reason: Value| {
            json!({"threadId": thread, "name": name, "status": status, "error": error,
                "failureReason": reason})
        };
        hear(
            &mut shared,
            &mut state,
            "mcpServer/startupStatus/updated",
            &mcp("docs", "ready", Value::Null, Value::Null),
        );
        hear(
            &mut shared,
            &mut state,
            "mcpServer/startupStatus/updated",
            &mcp("linear", "failed", json!("connection refused"), Value::Null),
        );
        hear(
            &mut shared,
            &mut state,
            "mcpServer/startupStatus/updated",
            &mcp("github", "failed", Value::Null, json!("reauthenticationRequired")),
        );
        let said: Vec<(String, String)> =
            notices(&state).into_iter().map(|n| (n.kind, n.text.text)).collect();
        let want = [
            (Notice::HOOK, "PreToolUse hook blocked: rm -rf is not allowed\nUse trash instead"),
            (Notice::INFO, "MCP server linear didn't start: connection refused"),
            (Notice::INFO, "MCP server github needs signing in again"),
        ];
        let want: Vec<(String, String)> =
            want.iter().map(|(k, t)| ((*k).to_owned(), (*t).to_owned())).collect();
        assert_eq!(said, want);
        let hook = state.items.iter().find(|i| i.id.0 == "hook:h2").unwrap();
        assert_eq!(hook.turn, state.turns.last().unwrap().id, "in its turn");
    }

    fn recorded_note(name: &str, method: &str) -> Value {
        fixture(name).into_iter().find(|l| l.msg["method"] == method).unwrap().msg["params"].clone()
    }

    /// A message held while a turn runs, once promoted, leaves the queue and goes into the turn
    /// under way as a steer, the rest still held.
    #[test]
    fn a_held_message_promoted_steers_the_turn() {
        use slopty_proto::thread::{Delivery, IntentId};
        let (mut shared, mut state) = begun();
        hear(
            &mut shared,
            &mut state,
            "turn/started",
            &recorded_note("question.jsonl", "turn/started"),
        );
        let turn = shared.current().unwrap().to_owned();
        let ids = [IntentId::new(), IntentId::new(), IntentId::new()];
        for (intent, text) in ids.iter().zip(["one", "two", "three"]) {
            let shared::Send::Held(actions) = shared.send(text, vec![], Delivery::Queue, *intent)
            else {
                panic!("held while the turn runs")
            };
            for action in &actions {
                state.apply(action);
            }
        }
        let order =
            |state: &ThreadState| state.pending.iter().map(|p| p.text.clone()).collect::<Vec<_>>();
        let (send, actions) = shared.promote(ids[0]).unwrap();
        for action in &actions {
            state.apply(action);
        }
        let shared::Send::Steer(steer) = send else { panic!("a steer: {send:?}") };
        assert_eq!(steer.expected_turn_id, turn);
        assert_eq!(serde_json::to_value(&steer.input).unwrap()[0]["text"], "one");
        assert_eq!(order(&state), ["two", "three"], "the rest still held");
        assert!(shared.promote(ids[0]).is_none(), "gone once");
    }

    /// A message queued while a turn runs is held by the thread, not sent: it shows waiting,
    /// can be changed or taken back, and goes as the next turn, changed, once Codex says the
    /// turn ended; a message to a thread at rest goes at once.
    #[test]
    fn a_queued_message_waits_for_the_turn_and_goes_as_the_next() {
        use slopty_agent::attach::Attached;
        use slopty_proto::thread::{Delivery, IntentId};
        let (mut shared, mut state) = begun();
        let at_rest = shared.send("Now", vec![], Delivery::Queue, IntentId::new());
        assert!(matches!(at_rest, shared::Send::Start(_)), "nothing runs: it goes at once");
        hear(
            &mut shared,
            &mut state,
            "turn/started",
            &recorded_note("question.jsonl", "turn/started"),
        );
        assert!(shared.current().is_some());

        let (kept, dropped) = (IntentId::new(), IntentId::new());
        let files = vec![
            Attached::Picture {
                path: "/drop/x/shot.png".to_owned(),
                media_type: "image/png",
                bytes: vec![0x89],
            },
            Attached::File { path: "/drop/x/my notes.md".to_owned() },
        ];
        for (intent, text, attached) in
            [(kept, "Then the docs", files), (dropped, "And the changelog", vec![])]
        {
            let shared::Send::Held(actions) = shared.send(text, attached, Delivery::Queue, intent)
            else {
                panic!("held while the turn runs")
            };
            for action in &actions {
                state.apply(action);
            }
        }
        assert_eq!(state.pending.len(), 2, "both show waiting");
        for action in &shared.edit(kept, "Then the docs, briefly").unwrap() {
            state.apply(action);
        }
        for action in &shared.withdraw(dropped).unwrap() {
            state.apply(action);
        }
        assert_eq!(
            state.pending.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(),
            ["Then the docs, briefly"]
        );
        assert_eq!(
            state.pending[0].attachments,
            ["/drop/x/shot.png", "/drop/x/my notes.md"],
            "the change kept its files"
        );
        assert!(shared.withdraw(dropped).is_none(), "taken back once");
        assert!(shared.next_queued().is_none(), "not while the turn runs");

        hear(
            &mut shared,
            &mut state,
            "turn/completed",
            &recorded_note("question.jsonl", "turn/completed"),
        );
        let (turn, taken) = shared.next_queued().expect("the held message goes next");
        for action in &taken {
            state.apply(action);
        }
        let sent = serde_json::to_value(&*turn).unwrap();
        assert_eq!(
            sent["input"],
            json!([
                {"type": "text", "text": "Then the docs, briefly '/drop/x/my notes.md'", "text_elements": []},
                {"type": "localImage", "path": "/drop/x/shot.png"},
            ]),
            "a picture is Codex's own localImage; any other file goes by its path"
        );
        assert!(state.pending.is_empty(), "no longer waiting");
        assert!(shared.next_queued().is_none());
    }

    /// A goal Codex works toward shows on the thread as Codex holds it, its state in Slopty's
    /// open words, and goes once Codex clears it. Nothing here sets one.
    #[test]
    fn a_codex_goal_shows_as_codex_holds_it() {
        let (mut shared, mut state) = begun();
        let native = shared.meta().native.clone();
        let goal = json!({"threadId": native, "turnId": null, "goal": {
            "threadId": native, "objective": "Make every fixture pass", "status": "budgetLimited",
            "tokensUsed": 41_000, "tokenBudget": 500_000, "timeUsedSeconds": 380,
            "createdAt": 1_790_000_000_i64, "updatedAt": 1_790_000_380_i64}});
        hear(&mut shared, &mut state, "thread/goal/updated", &goal);
        let held = state.goal.clone().expect("a goal");
        assert_eq!(
            (held.objective.as_str(), held.state.as_str()),
            ("Make every fixture pass", "budget-limited")
        );
        assert_eq!(
            (held.tokens_used, held.token_budget, held.time_used_s),
            (41_000, Some(500_000), 380)
        );
        assert_eq!(held.updated_ms, WallMs::from_millis(1_790_000_380_000));
        assert!(!held.is_active());
        hear(&mut shared, &mut state, "thread/goal/cleared", &json!({"threadId": native}));
        assert_eq!(state.goal, None);
    }

    /// The person's stop holds what is queued: the turn is interrupted, every message queued
    /// says it waits on the stop, and none goes as the turn ends. Their next message lets them
    /// go again in their order, ahead of it; sending one held now lets the rest go after it.
    #[test]
    fn a_stop_holds_the_queue_until_the_person_sends_again() {
        use slopty_proto::thread::{Delivery, IntentId, Pending};
        let (mut shared, mut state) = begun();
        let started = recorded_note("question.jsonl", "turn/started");
        let completed = recorded_note("question.jsonl", "turn/completed");
        let apply = |state: &mut ThreadState, actions: &[Action]| {
            for action in actions {
                state.apply(action);
            }
        };
        let queue = |shared: &mut Shared, state: &mut ThreadState, text: &str| {
            let intent = IntentId::new();
            let shared::Send::Held(actions) = shared.send(text, vec![], Delivery::Queue, intent)
            else {
                panic!("held: {text}")
            };
            apply(state, &actions);
            intent
        };
        let words = |turn: &p::TurnStartParams| {
            serde_json::to_value(turn).unwrap()["input"][0]["text"].as_str().unwrap().to_owned()
        };
        assert!(shared.stop().is_none(), "nothing to stop at rest");
        hear(&mut shared, &mut state, "turn/started", &started);
        let one = queue(&mut shared, &mut state, "one");
        queue(&mut shared, &mut state, "two");

        let (interrupt, held) = shared.stop().expect("a turn to stop");
        assert_eq!(interrupt.turn_id, shared.current().unwrap());
        apply(&mut state, &held);
        assert!(state.pending.iter().all(Pending::stopped), "{:?}", state.pending);
        hear(&mut shared, &mut state, "turn/completed", &completed);
        assert!(shared.next_queued().is_none(), "the stop holds them");

        let three = queue(&mut shared, &mut state, "three");
        assert!(state.pending.iter().all(|p| !p.stopped()), "let go: {:?}", state.pending);
        let (turn, taken) = shared.next_queued().expect("the first held goes");
        apply(&mut state, &taken);
        assert_eq!(words(&turn), "one");
        let order =
            |state: &ThreadState| state.pending.iter().map(|p| p.text.clone()).collect::<Vec<_>>();
        assert_eq!(order(&state), ["two", "three"], "the rest in their order, the new one last");

        hear(&mut shared, &mut state, "turn/started", &started);
        let (_, held) = shared.stop().unwrap();
        apply(&mut state, &held);
        hear(&mut shared, &mut state, "turn/completed", &completed);
        let (send, taken) = shared.promote(three).expect("held");
        apply(&mut state, &taken);
        let shared::Send::Start(turn) = send else { panic!("a turn of its own: {send:?}") };
        assert_eq!(words(&turn), "three", "the one sent now goes first");
        assert_eq!(order(&state), ["two"]);
        assert!(!state.pending[0].stopped(), "the rest go after it");
        assert!(shared.promote(one).is_none(), "gone already");
    }

    /// An MCP server's form, asked through Codex, is a request whose questions are its fields,
    /// beside a decline and a cancel; the answers go back as the form's content, each value of
    /// its field's type, and a decline as a decline.
    #[test]
    fn an_mcp_form_is_answered_as_its_content() {
        use slopty_proto::thread::detail::Answer;
        let (mut shared, mut state) = begun();
        let native = shared.meta().native.clone();
        let asked = |id: u64| {
            json!({"jsonrpc": "2.0", "id": id, "method": "mcpServer/elicitation/request", "params": {
                "serverName": "linear", "threadId": native, "mode": "form",
                "message": "File the issue?",
                "requestedSchema": {"type": "object", "properties": {
                    "notify": {"type": "boolean", "title": "Notify the team"}}}}})
        };
        let mut answered = Vec::new();
        for (id, deny) in [(7, false), (8, true)] {
            let msg = asked(id);
            let Incoming::Request { id, request, .. } = rpc::read(&msg.to_string()).unwrap() else {
                panic!("a request: {msg}")
            };
            for action in &shared.request(&id, &request, WallMs::from_millis(1)) {
                state.apply(action);
            }
            let open = state.requests.last().unwrap().clone();
            assert_eq!(open.questions.len(), 1, "one field, one question");
            assert_eq!(
                open.options.iter().map(|c| c.effect).collect::<Vec<_>>(),
                [Effect::Deny, Effect::Deny],
                "decline and cancel"
            );
            let choice = if deny {
                open.options.iter().find(|c| !c.stops).unwrap().id.clone()
            } else {
                let given =
                    Answer { question: open.questions[0].text.clone(), answer: "Yes".to_owned() };
                Answer::choice(&open.questions, &[given])
            };
            let (_id, result) = shared
                .answer(&open.id, &choice, Answerer { client: None, name: "Slopty".to_owned() })
                .unwrap();
            answered.push(result);
        }
        assert_eq!(answered[0], json!({"action": "accept", "content": {"notify": true}}));
        assert_eq!(answered[1]["action"], "decline");
        assert!(answered[1].get("content").is_none_or(Value::is_null));
    }
}
