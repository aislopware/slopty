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
        let served = codex::spawn(host.clone(), socket, None, asks);

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
        let _served = codex::spawn(host, dir.path().join("none.sock"), None, asks);
        let outcome = handle.start(IntentId::new(), start(dir.path(), "Say hello.")).await;
        assert_eq!(outcome, Outcome::Refused { reason: codex::NOT_RUNNING.to_owned() });
    }

    /// A stand-in for the person's `codex`, alone in a folder of `dir`: it says its version,
    /// and for anything else notes its arguments in `ran` and runs `daemon`, a shell line.
    fn stand_in(dir: &Path, ran: &Path, daemon: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'codex-cli 0.157.0'; exit 0; fi\n\
             printf '%s\\n' \"$*\" >> '{}'\n{daemon}\n",
            ran.display()
        );
        let codex = bin.join("codex");
        std::fs::write(&codex, script).unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// What the stand-in was run with, a line a run.
    fn runs(ran: &Path) -> Vec<String> {
        std::fs::read_to_string(ran)
            .map(|r| r.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// A start with no daemon running brings the person's daemon up with Codex's own command,
    /// once, however often the start is asked meanwhile, and goes on as a start does once the
    /// daemon answers.
    #[tokio::test]
    async fn a_codex_start_with_no_daemon_starts_the_persons_daemon_once() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let ran = dir.path().join("ran");
        // As Codex's start does, it answers once the app-server listens.
        let script = format!(
            "while [ ! -S '{0}' ]; do /bin/sleep 0.05; done\n\
             printf '{{\"status\":\"started\",\"socketPath\":\"%s\"}}\\n' '{0}'",
            socket.display()
        );
        let bin = stand_in(dir.path(), &ran, &script);
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let launch = codex::Launch::new(Some(bin.into_os_string()));
        let _served = codex::spawn(host.clone(), socket.clone(), Some(launch), asks);

        let id = IntentId::new();
        let (first, again) = (handle.clone(), handle.clone());
        let work_a = work.clone();
        let asked =
            tokio::spawn(async move { first.start(id, start(&work_a, "Say hello.")).await });
        let asked_again =
            tokio::spawn(async move { again.start(id, start(&work, "Say hello.")).await });
        tokio::time::timeout(BOUND, async {
            while runs(&ran).is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Codex's start ran");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, _heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(daemon(listener, tx));

        let outcome = tokio::time::timeout(BOUND, asked).await.unwrap().unwrap();
        assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
        let repeat = tokio::time::timeout(BOUND, asked_again).await.unwrap().unwrap();
        assert_eq!(repeat, outcome, "one start, however often asked");
        assert_eq!(runs(&ran), ["app-server daemon start"], "Codex's own command, once");
    }

    /// A daemon Codex could not start refuses the start in Codex's own words, and a machine
    /// with no `codex` says that; nothing tries again by itself.
    #[tokio::test]
    async fn a_failed_daemon_start_is_refused_in_codexs_words() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let failing = concat!(
            "echo 'Error: app server is running but is not managed by codex app-server daemon' ",
            ">&2\nexit 1"
        );
        let bin = stand_in(dir.path(), &ran, failing);
        let first = host(dir.path());
        let (handle, asks) = Codex::channel();
        let launch = codex::Launch::new(Some(bin.into_os_string()));
        let _served = codex::spawn(first, dir.path().join("none.sock"), Some(launch), asks);
        let outcome = handle.start(IntentId::new(), start(dir.path(), "Say hello.")).await;
        let words = "app server is running but is not managed by codex app-server daemon";
        assert_eq!(outcome, Outcome::Refused { reason: codex::daemon_failed(words) });
        tokio::time::sleep(codex::RETRY.saturating_mul(2)).await;
        assert_eq!(runs(&ran).len(), 1, "not tried again");

        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let second = host(&other);
        let (handle, asks) = Codex::channel();
        let launch = codex::Launch::new(Some(empty.into_os_string()));
        let _served = codex::spawn(second, dir.path().join("none.sock"), Some(launch), asks);
        let outcome = handle.start(IntentId::new(), start(dir.path(), "Say hello.")).await;
        assert_eq!(outcome, Outcome::Refused { reason: codex::NO_CODEX.to_owned() });
    }

    /// A worker that follows Codex starts nothing of it by itself, however long no daemon runs.
    #[tokio::test]
    async fn a_worker_alone_never_starts_codex() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let bin = stand_in(dir.path(), &ran, "exit 0");
        let host = host(dir.path());
        let (_handle, asks) = Codex::channel();
        let launch = codex::Launch::new(Some(bin.into_os_string()));
        let _served = codex::spawn(host, dir.path().join("none.sock"), Some(launch), asks);
        tokio::time::sleep(codex::RETRY.saturating_mul(3)).await;
        assert!(runs(&ran).is_empty(), "nothing ran: {:?}", runs(&ran));
    }

    /// The recorded answer to the resume of client `b`, which took the thread up again.
    fn resumed() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../slopty-agent/tests/fixtures/codex/approval.jsonl"
        );
        let lines: Vec<Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["client"] == "b")
            .collect();
        let asked = lines
            .iter()
            .find(|l| l["dir"] == "sent" && l["msg"]["method"] == "thread/resume")
            .unwrap();
        let id = &asked["msg"]["id"];
        lines.iter().find(|l| l["dir"] == "heard" && l["msg"]["id"] == *id).unwrap()["msg"].clone()
    }

    /// A stand-in daemon with no thread loaded that answers each `thread/resume` with the
    /// recording's answer, or with an error once `refuse` says so.
    async fn resumer(listener: UnixListener, refuse: bool) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let (heard, _kept) = mpsc::unbounded_channel();
        let lines = starter();
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let answer = match msg["method"].as_str() {
                Some("initialize") => {
                    let mut answer = lines[recorded(&lines, "initialize").1].msg.clone();
                    answer["id"] = id;
                    answer
                }
                Some("thread/loaded/list") => {
                    json!({ "id": id, "result": { "data": [], "nextCursor": null } })
                }
                Some("thread/resume") if refuse => {
                    json!({ "id": id, "error": { "code": -32600, "message": "no rollout found" } })
                }
                Some("thread/resume") => {
                    let mut answer = resumed();
                    answer["id"] = id;
                    answer
                }
                _ => continue,
            };
            say(&mut ws, &answer).await;
        }
    }

    /// A start that names a Codex thread (`resume <thread>`) takes it up again under the same
    /// thread, once, and one already followed is answered at once; a thread Codex cannot take up
    /// again is refused in its words.
    #[tokio::test]
    async fn a_start_that_names_a_thread_takes_it_up_again() {
        for refuse in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
            let socket: PathBuf = short.path().join("s.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let _server = tokio::spawn(resumer(listener, refuse));
            let host = host(dir.path());
            let (handle, asks) = Codex::channel();
            let _served = codex::spawn(host, socket, None, asks);

            let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
            let mut again = start(dir.path(), "");
            again.prompt = None;
            again.args = shared::resume_args(&native);
            let outcome = tokio::time::timeout(BOUND, handle.start(IntentId::new(), again.clone()))
                .await
                .unwrap();
            if refuse {
                let Outcome::Refused { reason } = &outcome else { panic!("{outcome:?}") };
                assert!(reason.starts_with("Codex couldn't take the thread up again"), "{reason}");
                continue;
            }
            assert_eq!(outcome, Outcome::Started { thread: shared::thread_of(&native) });
            let followed = handle.start(IntentId::new(), again).await;
            assert_eq!(followed, outcome, "followed already: the same thread at once");
        }
    }
}
