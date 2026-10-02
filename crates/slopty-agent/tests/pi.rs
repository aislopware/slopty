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
        assert_eq!(state.session_file.as_deref(), Some("/scratch/sessions/session.jsonl"));
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
}
