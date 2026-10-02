//! A Codex thread started from a client, on the worker's side: the person's Codex daemon is
//! played by what the pinned Codex said to the client that started a thread
//! (`slopty-agent/tests/fixtures/codex/approval.jsonl`, client `a`), on a control socket of the
//! test's own, and the thread is judged from the host. No Codex runs.

#[cfg(test)]
mod codex {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use futures_util::{SinkExt as _, StreamExt as _};
    use serde_json::{Value, json};
    use slopty_agent::codex::shared;
    use slopty_proto::thread::wire::{Outcome, Start};
    use slopty_proto::thread::{AgentId, IntentId, ItemBody, ThreadId, ThreadState, TurnState};
    use slopty_worker::thread::Host;
    use slopty_worker::thread::codex::{self, Codex};
    use slopty_worker::thread::log::Limits;
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;

    /// Long enough for any step on a loaded machine; the tests judge the state, not the time.
    const BOUND: Duration = Duration::from_secs(30);

    /// One recorded frame of the client that started the thread.
    struct Line {
        sent: bool,
        msg: Value,
    }

    fn starter() -> Vec<Line> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../slopty-agent/tests/fixtures/codex/approval.jsonl"
        );
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["client"] == "a")
            .map(|line| Line { sent: line["dir"] == "sent", msg: line["msg"].clone() })
            .collect()
    }

    /// Where the recording's client sent `method`, and where its answer came.
    fn recorded(lines: &[Line], method: &str) -> (usize, usize) {
        let at = lines.iter().position(|l| l.sent && l.msg["method"] == method).unwrap();
        let id = &lines[at].msg["id"];
        let answered = lines.iter().skip(at).position(|l| !l.sent && l.msg["id"] == *id).unwrap();
        (at, at.saturating_add(answered))
    }

    type Ws = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

    async fn next(ws: &mut Ws, heard: &mpsc::UnboundedSender<Value>) -> Option<Value> {
        let Some(Ok(Message::Text(text))) = ws.next().await else { return None };
        let msg: Value = serde_json::from_str(&text).unwrap();
        heard.send(msg.clone()).unwrap();
        Some(msg)
    }

    async fn say(ws: &mut Ws, msg: &Value) {
        ws.send(Message::text(msg.to_string())).await.unwrap();
    }

    /// The stand-in daemon at `listener`: the handshake as the recording answered it, no thread
    /// loaded, then the start and the first turn as the recording went, each answer under the id
    /// the worker chose. Codex marks the person's message with the id its turn named
    /// (`clientUserMessageId`), which the recording's client did not give, so the replayed one is
    /// marked with the worker's. Every frame the worker sent is handed to `heard`.
    async fn daemon(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let answer = |at: usize, id: &Value| {
            let mut answer = lines[at].msg.clone();
            answer["id"] = id.clone();
            answer
        };
        let init = next(&mut ws, &heard).await.unwrap();
        assert_eq!(init["method"], "initialize");
        say(&mut ws, &answer(recorded(&lines, "initialize").1, &init["id"])).await;
        assert_eq!(next(&mut ws, &heard).await.unwrap(), json!({ "method": "initialized" }));
        let list = next(&mut ws, &heard).await.unwrap();
        assert_eq!(list["method"], "thread/loaded/list");
        say(&mut ws, &json!({ "id": list["id"], "result": { "data": [], "nextCursor": null } }))
            .await;

        let start = next(&mut ws, &heard).await.unwrap();
        assert_eq!(start["method"], "thread/start");
        let (_, started) = recorded(&lines, "thread/start");
        say(&mut ws, &answer(started, &start["id"])).await;
        let turn = next(&mut ws, &heard).await.unwrap();
        assert_eq!(turn["method"], "turn/start");
        let client = turn["params"]["clientUserMessageId"].clone();
        let (turn_at, turn_answered) = recorded(&lines, "turn/start");
        for (at, line) in lines.iter().enumerate().skip(started.saturating_add(1)) {
            if at == turn_at || line.sent {
                continue;
            }
            let mut msg =
                if at == turn_answered { answer(at, &turn["id"]) } else { line.msg.clone() };
            if msg["params"]["item"]["type"] == "userMessage" {
                msg["params"]["item"]["clientId"] = client.clone();
            }
            say(&mut ws, &msg).await;
            if msg["method"] == "turn/completed" {
                break;
            }
        }
        // The account's windows, which name no thread, as Codex 0.160.0's schema shapes them.
        let limits = json!({"method": "account/rateLimits/updated", "params": {"rateLimits": {
            "primary": {"usedPercent": 30, "windowDurationMins": 300, "resetsAt": null}}}});
        say(&mut ws, &limits).await;
        while next(&mut ws, &heard).await.is_some() {}
    }

    /// Wait until `thread` in `host` satisfies `done`.
    async fn until(
        host: &Host,
        thread: ThreadId,
        done: impl Fn(&ThreadState) -> bool,
    ) -> ThreadState {
        let mut table = host.table_watch();
        let waited = tokio::time::timeout(BOUND, async {
            loop {
                if let Some((state, _)) = host.state(thread)
                    && done(&state)
                {
                    return state;
                }
                table.changed().await.unwrap();
            }
        });
        waited.await.expect("the thread came to the state awaited")
    }

    fn start(cwd: &Path, prompt: &str) -> Start {
        Start {
            agent: AgentId::named(AgentId::CODEX),
            cwd: cwd.to_string_lossy().into_owned(),
            drive: None,
            prompt: Some(prompt.to_owned()),
            model: None,
            args: Vec::new(),
        }
    }

    fn host(dir: &Path) -> Host {
        Host::open(&dir.join("threads"), Limits::default()).unwrap()
    }

    /// A start asks the person's Codex daemon for a thread in the folder, with nothing set but
    /// the folder, so the policies are their configuration's; the start is answered with the
    /// thread Codex made, and the prompt goes as its first turn, the person's message marked with
    /// the start's intent. A start Codex cannot take is refused before it is asked, and a repeat
    /// of the intent starts nothing.
    #[tokio::test]
    async fn a_start_asks_the_persons_codex_for_a_thread_and_sends_its_first_turn() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        // A socket's own path must fit 104 bytes.
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let server = tokio::spawn(daemon(listener, tx));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let served = codex::spawn(host.clone(), socket, asks);

        let mut args = start(&work, "Say hello.");
        args.args = vec!["--yolo".to_owned()];
        let refused = handle.start(IntentId::new(), args).await;
        assert!(matches!(refused, Outcome::Refused { .. }), "{refused:?}");

        let id = IntentId::new();
        let outcome = handle.start(id, start(&work, "Say hello.")).await;
        let native = starter()
            .iter()
            .find_map(|l| l.msg["result"]["thread"]["id"].as_str().map(str::to_owned))
            .unwrap();
        let thread = shared::thread_of(&native);
        assert_eq!(outcome, Outcome::Started { thread }, "the thread Codex made");
        let state = until(&host, thread, |s| {
            s.turns.first().is_some_and(|t| t.state == TurnState::Complete)
        })
        .await;
        let users: Vec<(String, Option<IntentId>)> = state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::User(m) => Some((m.text.text.clone(), m.intent)),
                _ => None,
            })
            .collect();
        assert_eq!(users, [("Say hello.".to_owned(), Some(id))]);
        assert!(state.meta.agent.is(AgentId::CODEX));
        assert_eq!(handle.start(id, start(&work, "Say hello.")).await, outcome, "started once");
        // What Codex's answer and the account say of the thread: its policy, its sandbox, and
        // the account's window, which names no thread.
        let state = until(&host, thread, |s| !s.meters.limits.is_empty()).await;
        assert_eq!(state.meters.mode.as_deref(), Some("on-request"));
        assert_eq!(state.meta.facts.get("sandbox").map(String::as_str), Some("readOnly"));
        assert_eq!(state.meters.limits[0].name, "five-hour");
        assert_eq!(state.meters.limits[0].used_bp, 3_000);

        served.abort();
        let _done = tokio::time::timeout(BOUND, server).await;
        let mut sent = Vec::new();
        while let Ok(msg) = heard.try_recv() {
            sent.push(msg);
        }
        let starts: Vec<&Value> = sent.iter().filter(|m| m["method"] == "thread/start").collect();
        let cwd = work.to_string_lossy();
        assert_eq!(starts.len(), 1, "one thread asked for: {sent:?}");
        assert_eq!(starts[0]["params"], json!({ "cwd": cwd }), "nothing loosened");
        let turns: Vec<&Value> = sent.iter().filter(|m| m["method"] == "turn/start").collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["params"]["input"][0]["text"], "Say hello.");
        assert_eq!(turns[0]["params"]["threadId"], native.as_str());
    }

    /// With no Codex daemon running, a start is refused in words, and Slopty starts none.
    #[tokio::test]
    async fn a_start_with_no_codex_running_is_refused_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host, dir.path().join("none.sock"), asks);
        let outcome = handle.start(IntentId::new(), start(dir.path(), "Say hello.")).await;
        assert_eq!(outcome, Outcome::Refused { reason: codex::NOT_RUNNING.to_owned() });
    }
}
