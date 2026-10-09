//! pi's RPC wire, as the pinned build spoke it with Slopty's gate loaded
//! (`cargo xtask pi fixtures`): every record reads into the typed records, what was sent reads
//! back to the same JSON, and the gate's asks and answers go the way the recording shows.

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use slopty_agent::pi::rpc::{
        self, AssistantEvent, Command, Content, Disposition, Entries, Incoming, Message, Request,
        State, Stats, UiMethod,
    };

    struct Line {
        sent: bool,
        msg: Value,
    }

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/pi/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).unwrap()
    }

    fn gate() -> Vec<Line> {
        fixture("gate.jsonl")
            .lines()
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                Line { sent: line["dir"] == "in", msg: line["msg"].clone() }
            })
            .collect()
    }

    fn heard() -> Vec<Incoming> {
        gate()
            .iter()
            .filter(|l| !l.sent)
            .map(|l| rpc::record(l.msg.to_string().as_bytes()).unwrap())
            .collect()
    }

    /// The fixture is the pinned pi's: a new pi is trusted only after a new recording.
    #[test]
    fn the_fixture_is_the_pinned_pis() {
        let recorded: Value = serde_json::from_str(&fixture("recorded.json")).unwrap();
        assert_eq!(
            recorded["pi"],
            slopty_agent::pi::VERSION,
            "re-record with `cargo xtask pi fixtures`"
        );
    }

    /// Every record pi wrote is one this knows, none of them `Other` but the ones the gate's run
    /// has no use for; and every message, block and streamed change reads as its own kind.
    #[test]
    fn every_record_pi_wrote_reads_as_its_kind() {
        let heard = heard();
        assert!(heard.len() > 50, "the run says a lot: {}", heard.len());
        assert!(!heard.contains(&Incoming::Other), "a record of an unknown kind");
        for record in &heard {
            match record {
                Incoming::MessageStart { message } | Incoming::MessageEnd { message } => {
                    if let Message::Assistant { content, .. } = message {
                        assert!(!content.contains(&Content::Other), "{content:?}");
                    }
                }
                Incoming::MessageUpdate { event, .. } => {
                    assert_ne!(*event, AssistantEvent::Other, "an unknown streamed change");
                }
                _ => {}
            }
        }
        let kinds = |want: fn(&Incoming) -> bool| heard.iter().filter(|r| want(r)).count();
        assert_eq!(kinds(|r| matches!(r, Incoming::AgentSettled)), 4, "one per prompt");
        assert_eq!(kinds(|r| matches!(r, Incoming::ToolExecutionEnd { .. })), 3);
        assert_eq!(kinds(|r| matches!(r, Incoming::QueueUpdate { .. })), 2, "queued, then sent");
    }

    /// What went to pi reads back to the very JSON that went.
    #[test]
    fn what_was_sent_reads_back_the_same() {
        let sent: Vec<Value> = gate().into_iter().filter(|l| l.sent).map(|l| l.msg).collect();
        assert_eq!(sent.len(), 11, "{sent:#?}");
        for msg in sent {
            let request: Request = serde_json::from_value(msg.clone()).unwrap();
            assert_ne!(request.command, Command::Other, "{msg}");
            let line = rpc::line(&request).unwrap();
            assert_eq!(line.last(), Some(&b'\n'), "one record, one LF");
            let back: Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(back, msg, "the same record");
        }
    }

    /// A record is split on LF alone: a U+2028 in a string is part of it, and a CR before the
    /// LF is dropped.
    #[test]
    fn a_record_ends_at_its_lf_alone() {
        let line = "{\"type\":\"session_info_changed\",\"name\":\"a\u{2028}b\"}\r\n";
        let record = rpc::record(line.as_bytes()).unwrap();
        assert_eq!(record, Incoming::SessionInfoChanged { name: Some("a\u{2028}b".to_owned()) });
        assert!(rpc::record(b"{\"type\":\"agent_settled\"}").is_ok(), "the last line, unended");
    }

    /// The gate asks about each of the three calls before it runs, after pi said it began, with
    /// the call's id, tool and arguments; the answers are the ones that went.
    #[test]
    fn the_gate_asks_about_each_call_and_is_answered() {
        let heard = heard();
        let asks: Vec<_> = heard
            .iter()
            .filter_map(|r| match r {
                Incoming::ExtensionUiRequest(ui) => Some((ui.id.clone(), ui.gate().unwrap())),
                _ => None,
            })
            .collect();
        let calls: Vec<&str> = asks.iter().map(|(_, ask)| ask.call.as_str()).collect();
        assert_eq!(calls, ["toolu_pi1", "toolu_pi2", "toolu_pi3"]);
        for (_, ask) in &asks {
            assert_eq!(ask.tool, "bash");
            assert!(ask.input["command"].as_str().is_some(), "{ask:?}");
            assert_eq!(ask.parent, None);
        }
        let started = heard.iter().position(|r| {
            matches!(r, Incoming::ToolExecutionStart { tool_call_id, .. } if tool_call_id == "toolu_pi1")
        });
        let asked = heard.iter().position(|r| matches!(r, Incoming::ExtensionUiRequest(_)));
        assert!(started < asked, "pi says a call began before the gate asks about it");

        let answers: Vec<Request> = gate()
            .into_iter()
            .filter(|l| l.sent && l.msg["type"] == "extension_ui_response")
            .map(|l| serde_json::from_value(l.msg).unwrap())
            .collect();
        let first = &asks[0].0;
        let second = &asks[1].0;
        assert_eq!(answers, [rpc::allow(first), rpc::deny(second, Some("Keep the file."))]);
        assert_eq!(rpc::deny(second, Some("  ")), rpc::deny(second, None), "no blank reason");

        let denied = heard.iter().find_map(|r| match r {
            Incoming::ToolExecutionEnd { tool_call_id, result, is_error }
                if tool_call_id == "toolu_pi2" =>
            {
                Some((*is_error, rpc::text_of(&result.content)))
            }
            _ => None,
        });
        assert_eq!(denied, Some((true, "Keep the file.".to_owned())), "the model is told why");
    }

    /// A dialog that is not the gate's is no gate ask.
    #[test]
    fn only_the_gates_select_is_a_gate_ask() {
        let other = r#"{"type":"extension_ui_request","id":"x","method":"select","title":"Pick","options":["a"]}"#;
        let Incoming::ExtensionUiRequest(ui) = rpc::record(other.as_bytes()).unwrap() else {
            panic!("a dialog");
        };
        assert_eq!(ui.gate(), None);
        let notice =
            r#"{"type":"extension_ui_request","id":"y","method":"setStatus","statusKey":"k"}"#;
        let Incoming::ExtensionUiRequest(ui) = rpc::record(notice.as_bytes()).unwrap() else {
            panic!("a notice");
        };
        assert_eq!(ui.method, UiMethod::Other);
    }

    /// The command answers read into what Slopty takes from them: a prompt's disposition, the
    /// session's state, entries and stats.
    #[test]
    fn the_answers_read_into_what_slopty_takes() {
        let responses: Vec<rpc::Response> = heard()
            .into_iter()
            .filter_map(|r| match r {
                Incoming::Response(response) => Some(response),
                _ => None,
            })
            .collect();
        let by = |id: &str| responses.iter().find(|r| r.id.as_deref() == Some(id)).unwrap();
        assert!(responses.iter().all(|r| r.success), "every command was taken");
        assert_eq!(by("hello").disposition(), Some(Disposition::Started));
        assert_eq!(by("steer").disposition(), Some(Disposition::Queued));
        let state: State = serde_json::from_value(by("state").data.clone().unwrap()).unwrap();
        assert!(!state.is_streaming);
        assert_eq!(
            state.session_file.as_deref(),
            Some(
                "/pi-agent/sessions/--work--/0000-00-00T00-00-00-000Z_00000000-0000-7000-8000-000000000001.jsonl"
            ),
            "pi keeps the session it was given the id of with its others"
        );
        assert_eq!(
            state.model.map(|m| (m.provider, m.id)),
            Some(("canned".into(), "canned-1".into()))
        );
        let entries: Entries = serde_json::from_value(by("entries").data.clone().unwrap()).unwrap();
        let prompts: Vec<String> = entries
            .entries
            .iter()
            .filter_map(|e| match &e.message {
                Some(Message::User { content }) => Some(content.text()),
                _ => None,
            })
            .collect();
        let want = [
            "Say hello.",
            "Make a file called made-by-pi.",
            "Also say done.",
            "Remove it.",
            "Remove it again.",
        ];
        assert_eq!(prompts, want, "the session holds every message, the steer in its place");
        let stats: Stats = serde_json::from_value(by("stats").data.clone().unwrap()).unwrap();
        assert_eq!(stats.context_usage.map(|c| c.context_window), Some(200_000));
    }

    mod driven {
        use slopty_agent::pi::driven::{self, Driven};
        use slopty_agent::pi::rpc::{self, Command, Entries, Incoming, Request};
        use slopty_core::WallMs;
        use slopty_proto::thread::{
            Action, Answerer, AskId, Cap, Drive, IntentId, ItemBody, Liveness, Phase, RequestState,
            ThreadState, ToolState, TurnState,
        };

        use super::gate;

        const SESSION: &str = "00000000-0000-7000-8000-000000000001";

        fn slopty() -> Answerer {
            Answerer { client: None, name: "Slopty".to_owned() }
        }

        /// The recording replayed through the codec, as the worker drives it: what went to pi
        /// is asked of the codec, what came back is heard by it, and the thread is what the
        /// reducer makes of every action. Also the thread's state at each gate ask.
        fn replayed() -> (ThreadState, Vec<ThreadState>, Vec<Command>) {
            let (mut driven, begun) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let mut state = ThreadState::new(driven.meta().clone());
            let apply = |state: &mut ThreadState, actions: Vec<Action>| {
                for action in &actions {
                    state.apply(action);
                }
            };
            apply(&mut state, begun);
            let mut at_asks = Vec::new();
            let mut asked = Vec::new();
            let mut sends = 0_u128;
            for line in gate() {
                if line.sent {
                    let request: Request = serde_json::from_value(line.msg.clone()).unwrap();
                    match &request.command {
                        Command::Prompt { message, .. } | Command::Steer { message, .. } => {
                            sends = sends.saturating_add(1);
                            let intent = IntentId::from_uuid(uuid::Uuid::from_u128(sends));
                            asked.push(driven.send(message, &[], intent));
                        }
                        Command::ExtensionUiResponse { value: Some(value), .. } => {
                            let ask = AskId(request.id.clone().unwrap());
                            let (choice, reason) = value.split_once('\n').unwrap_or((value, ""));
                            let reason = Some(reason).filter(|r| !r.is_empty());
                            let answered = driven
                                .answer(&ask, choice, reason, slopty(), WallMs::ZERO)
                                .unwrap();
                            assert_eq!(
                                answered.requests,
                                std::slice::from_ref(&request),
                                "the answer that went"
                            );
                            apply(&mut state, answered.actions);
                        }
                        Command::Abort => asked.extend(driven.interrupt()),
                        _ => {}
                    }
                    continue;
                }
                let record = rpc::record(line.msg.to_string().as_bytes()).unwrap();
                let actions = driven.incoming(&record, WallMs::ZERO);
                apply(&mut state, actions);
                if matches!(record, Incoming::ExtensionUiRequest(_)) {
                    at_asks.push(state.clone());
                }
            }
            (state, at_asks, asked)
        }

        fn user_texts(state: &ThreadState) -> Vec<(u32, String)> {
            state
                .items
                .iter()
                .filter_map(|i| match &i.body {
                    ItemBody::User(m) => Some((i.turn.0, m.text.text.clone())),
                    _ => None,
                })
                .collect()
        }

        fn tool_states(state: &ThreadState) -> Vec<(String, ToolState)> {
            state
                .items
                .iter()
                .filter_map(|i| match &i.body {
                    ItemBody::Tool(call) => Some((i.id.0.clone(), call.state.clone())),
                    _ => None,
                })
                .collect()
        }

        /// Four messages are four turns, the steer joining the turn it was sent into; each
        /// message carries the intent that sent it; the turns end as pi ended them.
        #[test]
        fn each_message_is_a_turn_and_a_steer_joins_its_own() {
            let (state, _, asked) = replayed();
            let want = [
                (1, "Say hello.".to_owned()),
                (2, "Make a file called made-by-pi.".to_owned()),
                (2, "Also say done.".to_owned()),
                (3, "Remove it.".to_owned()),
                (4, "Remove it again.".to_owned()),
            ];
            assert_eq!(user_texts(&state), want);
            let intents: Vec<Option<IntentId>> = state
                .items
                .iter()
                .filter_map(|i| match &i.body {
                    ItemBody::User(m) => Some(m.intent),
                    _ => None,
                })
                .collect();
            assert!(intents.iter().all(Option::is_some), "every message is its intent's");
            let ends: Vec<TurnState> = state.turns.iter().map(|t| t.state.clone()).collect();
            let want = [
                TurnState::Complete,
                TurnState::Complete,
                TurnState::Complete,
                TurnState::Interrupted,
            ];
            assert_eq!(ends, want);
            assert!(state.turns.iter().all(|t| t.input.is_some()), "each turn names its message");
            assert_eq!(state.status.phase, Phase::Stopped, "the last turn was interrupted");
            assert!(asked.contains(&Command::Abort), "the interrupt is an abort");
            let prompt = Command::Prompt {
                message: "Say hello.".to_owned(),
                images: Vec::new(),
                streaming_behavior: Some(rpc::StreamingBehavior::Steer),
            };
            assert_eq!(asked.first(), Some(&prompt), "a message steers, or starts a run");
        }

        /// What the model wrote streams into items and ends as the whole of it.
        #[test]
        fn the_model_writes_its_thinking_and_text() {
            let (state, ..) = replayed();
            let first: Vec<(String, String)> = state
                .items
                .iter()
                .filter(|i| i.turn.0 == 1)
                .filter_map(|i| match &i.body {
                    ItemBody::Reasoning(t) => Some(("thinking".to_owned(), t.text.clone())),
                    ItemBody::Text(t) => Some(("text".to_owned(), t.text.clone())),
                    _ => None,
                })
                .collect();
            let want = [
                ("thinking".to_owned(), "The person wants a greeting.".to_owned()),
                ("text".to_owned(), "Hello there.".to_owned()),
            ];
            assert_eq!(first, want);
        }

        /// Each call waits on the person while the gate asks; allowed, it runs and completes;
        /// denied, it is rejected with the reason the model was given; interrupted at its gate,
        /// it is cancelled and its ask withdrawn.
        #[test]
        fn a_call_waits_on_the_gate_and_ends_as_answered() {
            let (state, at_asks, _) = replayed();
            assert_eq!(at_asks.len(), 3);
            for (n, asking) in at_asks.iter().enumerate() {
                assert_eq!(asking.status.phase, Phase::NeedsYou, "ask {n}");
                let open: Vec<_> = asking.open_requests().collect();
                assert_eq!(open.len(), 1, "ask {n}");
                let call = open[0].item.clone().unwrap();
                let pending = tool_states(asking).into_iter().find(|(id, _)| *id == call.0);
                assert!(matches!(pending, Some((_, ToolState::Pending { .. }))), "{pending:?}");
                let ids: Vec<&str> = open[0].options.iter().map(|c| c.id.as_str()).collect();
                assert_eq!(ids, [driven::ALLOW, driven::DENY, driven::DENY_STOP]);
            }
            let wait = at_asks[0].status.wait.clone().unwrap();
            assert_eq!(wait.text, "Run touch made-by-pi?");
            let want = [
                ("toolu_pi1".to_owned(), ToolState::Completed),
                ("toolu_pi2".to_owned(), ToolState::Rejected),
                ("toolu_pi3".to_owned(), ToolState::Cancelled),
            ];
            assert_eq!(tool_states(&state), want);
            let output = |id: &str| {
                state.items.iter().find(|i| i.id.0 == id).and_then(|i| match &i.body {
                    ItemBody::Tool(call) => call.output.as_ref().map(|o| o.text.clone()),
                    _ => None,
                })
            };
            assert_eq!(output("toolu_pi2").as_deref(), Some("Keep the file."));
            let settled: Vec<RequestState> =
                state.requests.iter().map(|r| r.state.clone()).collect();
            let want = [
                RequestState::Answered { by: slopty(), choice: driven::ALLOW.to_owned() },
                RequestState::Answered { by: slopty(), choice: driven::DENY.to_owned() },
                RequestState::Withdrawn,
            ];
            assert_eq!(settled, want);
        }

        /// The session's own entries rebuild the thread that was streamed: the same turns, the
        /// same items under the same ids, the same words.
        #[test]
        fn the_session_rebuilds_the_thread_it_streamed() {
            let (live, ..) = replayed();
            let data = gate()
                .into_iter()
                .find(|l| !l.sent && l.msg["command"] == "get_entries")
                .map(|l| l.msg["data"].clone())
                .unwrap();
            let entries: Entries = serde_json::from_value(data).unwrap();
            let (mut driven, begun) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let mut read = ThreadState::new(driven.meta().clone());
            for action in begun.iter().chain(&driven.entries(&entries, WallMs::ZERO)) {
                read.apply(action);
            }
            let shape = |state: &ThreadState| -> Vec<(String, u32, String)> {
                state
                    .items
                    .iter()
                    .filter(|i| !matches!(i.body, ItemBody::Notice(_)))
                    .map(|i| {
                        let words = match &i.body {
                            ItemBody::User(m) => m.text.text.clone(),
                            ItemBody::Text(t) | ItemBody::Reasoning(t) => t.text.clone(),
                            ItemBody::Tool(call) => call.title.clone(),
                            _ => String::new(),
                        };
                        (i.id.0.clone(), i.turn.0, words)
                    })
                    .collect()
            };
            assert_eq!(shape(&read), shape(&live));
            assert_eq!(read.turns.len(), live.turns.len());
            let ends: Vec<TurnState> = read.turns.iter().map(|t| t.state.clone()).collect();
            assert_eq!(ends.last(), Some(&TurnState::Interrupted), "{ends:?}");
            assert!(read.open_requests().next().is_none(), "nothing asks of a session read again");
            let states = tool_states(&read);
            assert_eq!(states[0].1, ToolState::Completed);
            assert!(states.iter().all(|(_, s)| s.is_final()), "{states:?}");
            assert_eq!(read.meta.title, "Say hello.", "named by its first message");
        }

        /// An answer is had once and only with the gate's choices; deny and stop also aborts.
        #[test]
        fn an_answer_is_one_of_the_gates_and_is_had_once() {
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let ask = r#"{"type":"extension_ui_request","id":"g1","method":"select","title":"{\"gate\":\"slopty-gate/1\",\"call\":\"c1\",\"tool\":\"bash\",\"input\":{\"command\":\"ls\"}}","options":["allow","deny"]}"#;
            driven.incoming(&rpc::record(ask.as_bytes()).unwrap(), WallMs::ZERO);
            let id = AskId("g1".to_owned());
            assert!(driven.answer(&id, "maybe", None, slopty(), WallMs::ZERO).is_none());
            let answered =
                driven.answer(&id, driven::DENY_STOP, Some("No."), slopty(), WallMs::ZERO).unwrap();
            let want =
                [rpc::deny("g1", Some("No.")), Request { id: None, command: Command::Abort }];
            assert_eq!(answered.requests, want);
            assert!(
                driven.answer(&id, driven::ALLOW, None, slopty(), WallMs::ZERO).is_none(),
                "once"
            );
            assert_eq!(
                Driven::set_model("canned/canned-1"),
                Some(Command::SetModel {
                    provider: "canned".to_owned(),
                    model_id: "canned-1".to_owned(),
                })
            );
            assert_eq!(Driven::set_model("no-provider"), None);
        }

        fn heard(driven: &mut Driven, line: &str, now: u64) -> Vec<Action> {
            driven.incoming(&rpc::record(line.as_bytes()).unwrap(), WallMs::from_millis(now))
        }

        fn opened(actions: &[Action]) -> Vec<slopty_proto::thread::Request> {
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::RequestOpened(r) => Some((**r).clone()),
                    _ => None,
                })
                .collect()
        }

        /// Another extension's dialogs are questions with the answers they offer, answered in
        /// the shape each takes; one pi gives up on at its timeout is withdrawn then; the end of
        /// a turn leaves them open, since pi still waits on them; a notice is a notice.
        #[test]
        fn another_extensions_dialogs_are_questions() {
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let pick = r#"{"type":"extension_ui_request","id":"p","method":"select","title":"Which branch?","options":["main","dev"],"timeout":5000}"#;
            let sure = r#"{"type":"extension_ui_request","id":"c","method":"confirm","title":"Clear it?","message":"All of it."}"#;
            let say = r#"{"type":"extension_ui_request","id":"t","method":"input","title":"Your name?","placeholder":"name"}"#;
            let note = r#"{"type":"extension_ui_request","id":"n","method":"notify","message":"Saved.","notifyType":"info"}"#;

            let asked = opened(&heard(&mut driven, pick, 1_000));
            assert_eq!(asked.len(), 1);
            assert_eq!(asked[0].kind, slopty_proto::thread::Request::QUESTION);
            let ids: Vec<&str> = asked[0].options.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(ids, ["main", "dev"]);
            assert_eq!(asked[0].until_ms, Some(WallMs::from_millis(6_000)));
            assert_eq!(driven.deadline(), Some(WallMs::from_millis(6_000)));
            let asked = opened(&heard(&mut driven, sure, 1_000));
            let ids: Vec<&str> = asked[0].options.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(ids, [driven::YES, driven::NO]);
            assert_eq!(asked[0].text.as_ref().map(|t| t.text.as_str()), Some("All of it."));
            let asked = opened(&heard(&mut driven, say, 1_000));
            assert!(asked[0].options.is_empty(), "nothing offered: written");
            assert_eq!(asked[0].questions[0].text, "Your name?");

            let notice = heard(&mut driven, note, 1_000);
            assert!(
                notice.iter().any(|a| matches!(a, Action::ItemCompleted(i)
                    if matches!(&i.body, ItemBody::Notice(n) if n.text.text == "Saved."))),
                "{notice:?}"
            );

            let ended = heard(&mut driven, r#"{"type":"agent_settled"}"#, 1_500);
            assert!(
                !ended.iter().any(|a| matches!(a, Action::RequestResolved { .. })),
                "pi still waits on them"
            );

            let (p, c, t) = (AskId("p".to_owned()), AskId("c".to_owned()), AskId("t".to_owned()));
            assert!(!driven.takes(&p, "nope"), "only what it offers");
            assert!(!driven.takes(&c, "maybe"));
            let at = WallMs::from_millis(2_000);
            let yes = driven.answer(&c, driven::YES, None, slopty(), at).unwrap();
            assert_eq!(yes.requests, [rpc::confirm("c", true)]);
            let named = driven.answer(&t, "Ada", None, slopty(), at).unwrap();
            assert_eq!(named.requests, [rpc::answer("t", "Ada".to_owned())]);

            assert!(driven.expire(WallMs::from_millis(5_999)).is_empty(), "not yet");
            let gone = driven.expire(WallMs::from_millis(6_000));
            assert!(gone.contains(&Action::RequestResolved {
                id: p.clone(),
                state: RequestState::Withdrawn
            }));
            assert_eq!(driven.deadline(), None);
            assert!(driven.answer(&p, "main", None, slopty(), at).is_none(), "pi gave up on it");
        }

        /// A failed model call pi tries again is a notice that says which attempt comes next,
        /// out of how many and after how long, as pi publishes it (`auto_retry_start`).
        #[test]
        fn a_retry_says_its_attempt_and_its_wait() {
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let retry = r#"{"type":"auto_retry_start","attempt":2,"maxAttempts":3,"delayMs":4000,"errorMessage":"529 overloaded"}"#;
            let heard = heard(&mut driven, retry, 1_000);
            let notice = heard.iter().find_map(|a| match a {
                Action::ItemCompleted(i) => match &i.body {
                    ItemBody::Notice(n) => Some(n.clone()),
                    _ => None,
                },
                _ => None,
            });
            let notice = notice.unwrap_or_else(|| panic!("a notice: {heard:?}"));
            assert_eq!(notice.kind, slopty_proto::thread::Notice::API_ERROR);
            assert_eq!(notice.text.text, "529 overloaded");
            let retry =
                slopty_proto::thread::Retry { attempt: 2, max: Some(3), in_ms: Some(4_000) };
            assert_eq!(notice.retry, Some(retry));
        }

        /// The last tool call `actions` carry.
        fn last_call(actions: &[Action]) -> slopty_proto::thread::ToolCall {
            let call = actions.iter().rev().find_map(|a| match a {
                Action::ItemStarted(i) | Action::ItemUpdated(i) | Action::ItemCompleted(i) => {
                    match &i.body {
                        ItemBody::Tool(call) => Some((**call).clone()),
                        _ => None,
                    }
                }
                _ => None,
            });
            call.unwrap_or_else(|| panic!("a call: {actions:?}"))
        }

        /// pi's `edit` carries its diff: from the texts it replaces while it runs, then the
        /// numbered patch pi's result keeps once it is made; `write` carries the file it
        /// writes as one whole hunk. Shapes as pi 1.1.0 publishes them.
        #[test]
        fn an_edit_and_a_write_carry_their_diff() {
            use slopty_proto::thread::detail::ToolDetail;
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let opened = r#"{"type":"message_start","message":{"role":"assistant","content":[],"model":"canned-1","provider":"canned","timestamp":0}}"#;
            heard(&mut driven, opened, 1);
            let edit = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","contentIndex":0,"toolCall":{"type":"toolCall","id":"e1","name":"edit","arguments":{"path":"src/a.rs","edits":[{"oldText":"fn a() {\n    1\n}","newText":"fn a() {\n    2\n}"},{"oldText":"// old","newText":"// new"}]}}}}"#;
            let call = last_call(&heard(&mut driven, edit, 2));
            let Some(ToolDetail::Edit(detail)) = call.detail else {
                panic!("an edit's detail: {call:?}")
            };
            assert_eq!((detail.path.as_str(), detail.edits), ("src/a.rs", 2));
            assert_eq!((detail.patch.added, detail.patch.removed), (2, 2));
            let lines: Vec<&str> =
                detail.patch.hunks.iter().flat_map(|h| &h.lines).map(String::as_str).collect();
            assert_eq!(lines, [" fn a() {", "-    1", "+    2", " }", "-// old", "+// new"]);

            let ended = r#"{"type":"tool_execution_end","toolCallId":"e1","toolName":"edit","isError":false,"result":{"content":[{"type":"text","text":"Successfully replaced 2 block(s) in src/a.rs."}],"details":{"diff":"","patch":"--- src/a.rs\n+++ src/a.rs\n@@ -9,4 +9,4 @@ mod b\n-// old\n+// new\n fn a() {\n-    1\n+    2\n }\n","firstChangedLine":10}}}"#;
            let call = last_call(&heard(&mut driven, ended, 3));
            let Some(ToolDetail::Edit(detail)) = call.detail else {
                panic!("an edit's detail: {call:?}")
            };
            let hunk = detail.patch.hunks.first().unwrap();
            assert_eq!(
                (hunk.old_start, hunk.new_start, hunk.heading.as_deref()),
                (9, 9, Some("mod b"))
            );
            assert_eq!((hunk.old_lines, hunk.lines.len()), (4, 6), "pi's patch, as made");

            let write = r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","contentIndex":1,"toolCall":{"type":"toolCall","id":"w1","name":"write","arguments":{"path":"notes.md","content":"one\ntwo\n"}}}}"#;
            let call = last_call(&heard(&mut driven, write, 4));
            let Some(ToolDetail::Write(detail)) = call.detail else {
                panic!("a write's detail: {call:?}")
            };
            assert_eq!((detail.path.as_str(), detail.lines, detail.created), ("notes.md", 2, None));
            let lines: Vec<&str> =
                detail.patch.hunks.iter().flat_map(|h| &h.lines).map(String::as_str).collect();
            assert_eq!(lines, ["+one", "+two"]);
        }

        /// Each turn names the model that answered in it, as pi's messages say it.
        #[test]
        fn each_turn_names_the_model_that_answered() {
            let (state, ..) = replayed();
            assert_ne!(state.turns, []);
            for turn in &state.turns {
                assert_eq!(turn.models, ["canned-1"], "turn {}", turn.id.0);
            }
        }

        /// pi's thinking level is the meters' effort, from its state and as it changes; a
        /// compaction says what it came to, a failed one says why; a message's tokens are its
        /// turn's, and its cost is not kept.
        #[test]
        fn effort_compaction_and_tokens_are_carried() {
            let (mut driven, begun) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let mut state = ThreadState::new(driven.meta().clone());
            let lines = [
                r#"{"type":"thinking_level_changed","level":"high"}"#,
                r#"{"type":"compaction_end","reason":"threshold","aborted":false,"willRetry":false,"result":{"summary":"So far","firstKeptEntryId":"e9","tokensBefore":150000,"estimatedTokensAfter":32000,"usage":{},"details":{}}}"#,
                r#"{"type":"compaction_end","reason":"overflow","aborted":false,"willRetry":false,"errorMessage":"no model"}"#,
                r#"{"type":"agent_start"}"#,
                r#"{"type":"message_start","message":{"role":"user","content":"Go"}}"#,
                r#"{"type":"message_end","message":{"role":"user","content":"Go"}}"#,
                r#"{"type":"message_start","message":{"role":"assistant","content":[]}}"#,
                r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"model":"canned-1","provider":"canned","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15,"cost":{"total":0.0125}},"stopReason":"stop"}}"#,
                r#"{"type":"agent_settled"}"#,
            ];
            let mut actions = begun;
            for line in lines {
                actions.extend(heard(&mut driven, line, 1_000));
            }
            for action in &actions {
                state.apply(action);
            }
            assert_eq!(state.meters.effort.as_deref(), Some("high"));
            let compaction = state.items.iter().find_map(|i| match &i.body {
                ItemBody::Compaction(c) => Some(c.clone()),
                _ => None,
            });
            let compaction = compaction.expect("a compaction");
            assert_eq!(compaction.trigger.as_deref(), Some("threshold"));
            assert_eq!(
                (compaction.before_tokens, compaction.after_tokens),
                (Some(150_000), Some(32_000))
            );
            assert_eq!(compaction.summary.map(|s| s.text).as_deref(), Some("So far"));
            assert!(state.items.iter().any(|i| matches!(&i.body,
                ItemBody::Notice(n) if n.text.text.contains("no model"))));
            let last = state.turns.last().expect("a turn");
            assert_eq!(last.usage.tokens(), 15, "the cost is no token");
            assert!(!last.usage.0.keys().any(|k| k.contains("cost")), "{:?}", last.usage);
            assert_eq!(last.models, ["canned-1"]);
        }

        /// The recording heard up to the first gate ask, as the worker hears it: the codec and
        /// the thread.
        fn at_first_ask() -> (Driven, ThreadState) {
            let (mut driven, begun) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let mut state = ThreadState::new(driven.meta().clone());
            for action in &begun {
                state.apply(action);
            }
            for line in gate() {
                if line.sent {
                    if let Ok(Request { command: Command::Prompt { message, .. }, .. }) =
                        serde_json::from_value::<Request>(line.msg.clone())
                    {
                        drop(driven.send(&message, &[], IntentId::new()));
                    }
                    continue;
                }
                let record = rpc::record(line.msg.to_string().as_bytes()).unwrap();
                for action in driven.incoming(&record, WallMs::ZERO) {
                    state.apply(&action);
                }
                if matches!(record, Incoming::ExtensionUiRequest(_)) {
                    return (driven, state);
                }
            }
            panic!("the recording asks");
        }

        /// pi gone while its gate asks: the ask is withdrawn, the call cancelled, and the turn
        /// ends failed with why when pi failed, stopped when it was ended; either way the thread
        /// can be taken up again.
        #[test]
        fn a_pi_that_ends_cuts_its_work_short() {
            for (why, turn, phase) in [
                (
                    Some("pi ended: boom"),
                    TurnState::Failed { error: "pi ended: boom".into(), until_ms: None },
                    Phase::Failed,
                ),
                (None, TurnState::Interrupted, Phase::Stopped),
            ] {
                let (mut driven, mut state) = at_first_ask();
                assert_eq!(state.status.phase, Phase::NeedsYou);
                for action in driven.exited(why, WallMs::ZERO) {
                    state.apply(&action);
                }
                assert!(state.open_requests().next().is_none(), "nothing asks");
                assert_eq!(state.requests[0].state, RequestState::Withdrawn);
                assert_eq!(tool_states(&state), [("toolu_pi1".to_owned(), ToolState::Cancelled)]);
                assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(turn));
                assert_eq!(state.status.phase, phase);
                assert_eq!(state.status.liveness, Liveness::Exited { resumable: true });
            }
        }

        /// A pi that ended unheard, as with the worker that ran it, is cut short from what the
        /// thread holds: the same as a pi heard to end.
        #[test]
        fn a_pi_gone_with_its_worker_is_cut_short_from_the_thread() {
            let (_, mut state) = at_first_ask();
            for action in driven::gone(&state, WallMs::ZERO) {
                state.apply(&action);
            }
            assert!(state.open_requests().next().is_none());
            assert_eq!(tool_states(&state), [("toolu_pi1".to_owned(), ToolState::Cancelled)]);
            assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Interrupted));
            assert_eq!(state.status.phase, Phase::Stopped);
            assert_eq!(state.status.liveness, Liveness::Exited { resumable: true });
        }

        /// The session's entries as the recording ends with them.
        fn recorded_entries() -> Entries {
            let data = gate()
                .into_iter()
                .find(|l| !l.sent && l.msg["command"] == "get_entries")
                .map(|l| l.msg["data"].clone())
                .unwrap();
            serde_json::from_value(data).unwrap()
        }

        /// Held by pi's TUI, the thread names its terminal, can only be taken back, and follows
        /// the entries the TUI appends: a turn the TUI is in stays open and working until the
        /// model's message that ends it; held by Slopty again, it is driven as before.
        #[test]
        fn a_thread_held_by_the_tui_follows_what_it_writes() {
            let (mut driven, begun) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let mut state = ThreadState::new(driven.meta().clone());
            let terminal = slopty_core::SessionId::new();
            for action in begun.iter().chain(&driven.held_by_tui(terminal, WallMs::ZERO)) {
                state.apply(action);
            }
            assert_eq!(state.meta.terminal, Some(terminal));
            assert!(state.meta.drive.is(Drive::OBSERVED));
            assert_eq!(state.meta.caps, [Cap::named(Cap::HANDOFF)]);
            let entries = recorded_entries().entries;
            let first_answer = entries
                .iter()
                .position(|e| matches!(&e.message, Some(rpc::Message::Assistant { .. })))
                .unwrap();
            for entry in &entries[..first_answer] {
                for action in driven.appended(entry, WallMs::ZERO) {
                    state.apply(&action);
                }
            }
            assert_eq!(state.status.phase, Phase::Working, "the TUI works on it");
            assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Active));
            assert!(!driven.rests());
            for action in driven.appended(&entries[first_answer], WallMs::ZERO) {
                state.apply(&action);
            }
            assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Complete));
            assert_eq!(state.status.phase, Phase::Done);
            assert!(driven.rests());
            for entry in &entries[first_answer + 1..] {
                for action in driven.appended(entry, WallMs::ZERO) {
                    state.apply(&action);
                }
            }
            assert_eq!(state.turns.len(), 4);
            for action in driven.held_by_slopty() {
                state.apply(&action);
            }
            assert_eq!(state.meta.terminal, None);
            assert!(state.meta.drive.is(Drive::DRIVEN) && state.meta.can(Cap::STEER));
        }

        /// The flags a thread was started with are kept with it, and read back.
        #[test]
        fn the_flags_a_thread_started_with_are_kept() {
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            assert_eq!(driven::args_of(driven.meta()), Vec::<String>::new());
            let args = ["--offline".to_owned(), "-t".to_owned(), "read".to_owned()];
            assert_ne!(driven.started_with(&args), []);
            assert_eq!(driven::args_of(driven.meta()), args);
            let (again, _) = Driven::of(driven.meta(), WallMs::ZERO);
            assert_eq!(driven::args_of(again.meta()), args, "a pi started again gets them");
        }

        /// A message pi refused is not waited on: the next one in the same words is its own.
        #[test]
        fn a_refused_message_is_not_waited_on() {
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            let first = IntentId::from_uuid(uuid::Uuid::from_u128(1));
            let second = IntentId::from_uuid(uuid::Uuid::from_u128(2));
            drop(driven.send("Say hello.", &[], first));
            driven.unsent(first);
            drop(driven.send("Say hello.", &[], second));
            let start = r#"{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"Say hello."}],"timestamp":0}}"#;
            let end = start.replace("message_start", "message_end");
            heard(&mut driven, start, 1);
            let actions = heard(&mut driven, &end, 1);
            let intent = actions.iter().find_map(|a| match a {
                Action::ItemCompleted(i) => match &i.body {
                    ItemBody::User(m) => m.intent,
                    _ => None,
                },
                _ => None,
            });
            assert_eq!(intent, Some(second));
        }

        /// A message held on the worker waits while pi has anything to do: from the moment a
        /// message is sent until pi echoes it, and through the run. Once pi settles it goes
        /// next, unless the person's stop holds it, until they speak again.
        #[test]
        fn a_queued_message_goes_once_pi_has_nothing_else_to_do() {
            let (mut driven, _) = Driven::new(SESSION, "1.0.0", "/work", WallMs::ZERO);
            assert!(driven.meta().can(Cap::QUEUE));
            let later = IntentId::new();
            let mut held = false;
            for line in gate() {
                if line.sent {
                    let request: Request = serde_json::from_value(line.msg.clone()).unwrap();
                    if let Command::Prompt { message, .. } = &request.command {
                        let _command = driven.send(message, &[], IntentId::new());
                        if !held {
                            let _shown =
                                driven.queue().hold(later, "Then tidy up.", Vec::new(), (), false);
                            held = true;
                            assert!(driven.busy(), "sent and not yet heard back");
                            assert!(driven.next_queued().is_none(), "not before pi takes it");
                        }
                    }
                    continue;
                }
                let record = rpc::record(line.msg.to_string().as_bytes()).unwrap();
                let _actions = driven.incoming(&record, WallMs::ZERO);
                if matches!(record, Incoming::AgentSettled) {
                    break;
                }
                assert!(driven.next_queued().is_none(), "not while pi works: {record:?}");
            }
            assert!(!driven.busy(), "settled, with every message heard back");
            let _held = driven.queue().stop();
            assert!(driven.next_queued().is_none(), "the person's stop holds it");
            assert!(driven.queue().release());
            let (next, shown) = driven.next_queued().expect("it goes next");
            assert_eq!((next.intent, next.text.as_str()), (later, "Then tidy up."));
            assert!(matches!(shown.as_slice(), [Action::PendingSet(p)] if p.is_empty()));
        }
    }
}
