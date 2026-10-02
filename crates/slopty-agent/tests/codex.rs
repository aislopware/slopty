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
        Action, AgentId, Answerer, Drive, Effect, ItemBody, Phase, Request, RequestState, Status,
        ThreadState, TurnId, TurnState,
    };

    /// The notifications Slopty passes over: the app-server's remote-control state, its
    /// deprecation notices, thread goals, and a thread's settings (its collaboration mode, which
    /// the thread does not show yet).
    const PASSED_OVER: [&str; 4] = [
        "remoteControl/status/changed",
        "deprecationNotice",
        "thread/goal/cleared",
        "thread/settings/updated",
    ];

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
    /// the face would, which must send what the recording sent.
    fn mapped(client: &str, answer: Option<&str>) -> (ThreadState, Vec<Status>) {
        let lines = approval();
        let mut shared: Option<Shared> = None;
        let mut state: Option<ThreadState> = None;
        let mut statuses = Vec::new();
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
        }
        (state.unwrap(), statuses)
    }

    fn texts(state: &ThreadState) -> Vec<String> {
        state
            .items
            .iter()
            .map(|item| match &item.body {
                ItemBody::User(message) => format!("user: {}", message.text.text),
                ItemBody::Text(text) => format!("text: {}", text.text),
                ItemBody::Tool(call) => format!("{} {:?}: {}", call.kind, call.state, call.title),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// The thread as the client that started it saw it: two turns, the command it approved
    /// elsewhere in its place, and the request answered by the TUI.
    #[test]
    fn the_starter_sees_the_thread_and_the_approval_settled_elsewhere() {
        let (state, statuses) = mapped("a", None);
        assert_eq!(state.meta.drive, Drive::named(Drive::SHARED));
        assert_eq!(state.meta.agent, AgentId::named(AgentId::CODEX));
        assert_eq!(state.meta.agent_version, "0.160.0");
        assert_eq!(
            texts(&state),
            [
                "user: Say hello.",
                "text: Hello.",
                "user: Make a file called made-by-codex.",
                "exec Completed: /bin/zsh -lc 'touch made-by-codex'",
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
        assert_eq!(request.title, "Run /bin/zsh -lc 'touch made-by-codex'?");
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
        let (state, _) = mapped("b", Some("accept"));
        let [request] = &*state.requests else { panic!("one request: {:?}", state.requests) };
        let by = Answerer { client: None, name: "Slopty".to_owned() };
        assert_eq!(request.state, RequestState::Answered { by, choice: "accept".to_owned() });
        let (started, _) = mapped("a", None);
        assert_eq!(texts(&state), texts(&started), "the first turn came back with the resume");
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
}
