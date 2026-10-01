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

    /// The notifications Slopty passes over: the app-server's remote-control state, its
    /// deprecation notices and thread goals.
    const PASSED_OVER: [&str; 3] =
        ["remoteControl/status/changed", "deprecationNotice", "thread/goal/cleared"];

    struct Line {
        client: String,
        sent: bool,
        msg: Value,
    }

    fn approval() -> Vec<Line> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex/approval.jsonl");
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
            other => panic!("no types named for {other}"),
        }
    }

    /// Every frame the app-server sent reads, and each notification it sent is one these types
    /// name or one Slopty passes over on purpose.
    #[test]
    fn every_frame_the_app_server_sent_reads() {
        let mut unnamed = BTreeSet::new();
        let mut read = 0;
        for line in approval().iter().filter(|line| !line.sent) {
            match rpc::read(&line.msg.to_string()).unwrap() {
                Incoming::UnknownNotification { method } => {
                    unnamed.insert(method);
                }
                Incoming::UnknownRequest { method, .. } => panic!("an unnamed request {method}"),
                Incoming::Answer { outcome, .. } => {
                    outcome.unwrap();
                }
                Incoming::Request { .. } | Incoming::Notification(_) => read += 1,
            }
        }
        assert_eq!(unnamed, PASSED_OVER.iter().map(|m| (*m).to_owned()).collect());
        assert!(read > 40, "only {read} frames read into named types");
    }

    /// What either side sent reads back to the same JSON: Slopty's requests and the answers to
    /// them, and the approval and the answer to it.
    #[test]
    fn what_both_sides_sent_reads_back() {
        let lines = approval();
        let mut asked: HashMap<(String, String), (String, Value)> = HashMap::new();
        let mut checked = BTreeSet::new();
        for line in &lines {
            let id = line.msg.get("id").map(Value::to_string);
            let method = line.msg.get("method").and_then(Value::as_str);
            match (id, method) {
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
        let want = want.into_iter().chain(["item/commandExecution/requestApproval"]);
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
            if let Some(Incoming::Request { id, request }) = heard(line) {
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
                Some(Incoming::Notification(note)) => match *note {
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
}
