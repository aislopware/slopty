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
    use slopty_core::SessionId;
    use slopty_proto::thread::wire::{Outcome, Start};
    use slopty_proto::thread::{
        AgentId, Answerer, AskId, Delivery, Fork, IntentId, ItemBody, PendingState, ThreadId,
        ThreadMeta, ThreadState, TurnState,
    };
    use slopty_worker::thread::codex::{self, Codex};
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::{Host, Seated};
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

    /// The next frame the worker sent, handed to `heard`. Its ask for Codex's models, made once
    /// as it joins, is answered here with [`catalog`] and passed over. Its asks for a followed
    /// thread's skills and background commands are answered with none and not handed on: the
    /// test of those plays its own daemon ([`background`]).
    async fn next(ws: &mut Ws, heard: &mpsc::UnboundedSender<Value>) -> Option<Value> {
        loop {
            let Some(Ok(Message::Text(text))) = ws.next().await else { return None };
            let msg: Value = serde_json::from_str(&text).unwrap();
            let none = json!({ "data": [], "nextCursor": null });
            let quiet = match msg["method"].as_str() {
                Some("skills/list" | "thread/backgroundTerminals/list") => Some(none),
                _ => None,
            };
            if let Some(result) = quiet {
                say(ws, &json!({ "id": msg["id"], "result": result })).await;
                continue;
            }
            heard.send(msg.clone()).unwrap();
            if msg["method"] != "model/list" {
                return Some(msg);
            }
            say(ws, &json!({ "id": msg["id"], "result": catalog() })).await;
        }
    }

    /// Codex's models as `model/list` answers it (Codex 0.160.0's schema): the recording's model
    /// and a smaller one, each with the reasoning efforts it supports.
    fn catalog() -> Value {
        let model = |slug: &str, name: &str, efforts: &[&str], default: &str| {
            json!({
                "id": slug, "model": slug, "displayName": name, "description": "",
                "hidden": false, "isDefault": false, "defaultReasoningEffort": default,
                "supportedReasoningEfforts": efforts.iter().map(|e| json!({
                    "reasoningEffort": e, "description": ""
                })).collect::<Vec<_>>(),
            })
        };
        json!({ "data": [
            model("mock-model", "Mock", &["low", "medium", "high"], "medium"),
            model("mini-model", "Mini", &["low"], "low"),
        ], "nextCursor": null })
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
        // What the thread cost is never asked: what the plan bills is not knowable here.
        while let Some(msg) = next(&mut ws, &heard).await {
            assert_ne!(msg["method"], "account/usage/read", "no cost is asked");
        }
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

    /// `thread` once `done` holds of it, looked at every few milliseconds: for a change to the
    /// thread that leaves its row in the table as it was (a notice).
    async fn polled(
        host: &Host,
        thread: ThreadId,
        done: impl Fn(&ThreadState) -> bool,
    ) -> ThreadState {
        let waited = tokio::time::timeout(BOUND, async {
            loop {
                if let Some((state, _)) = host.state(thread).filter(|(s, _)| done(s)) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
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
            mode: None,
            effort: None,
            attachments: Vec::new(),
            args: Vec::new(),
            worktree: None,
        }
    }

    /// What a thread Slopty starts, takes up again or forks is given, so its commands never
    /// claim the Slopty terminal or its proof of whoever started Codex's
    /// daemon: each set empty, which names no terminal.
    fn unclaimed() -> Value {
        json!({
            "shell_environment_policy.set.SLOPTY_SESSION": "",
            "shell_environment_policy.set.SLOPTY_SESSION_TOKEN": "",
        })
    }

    fn host(dir: &Path) -> Host {
        Host::open(&dir.join("threads"), Limits::default()).unwrap()
    }

    /// A start asks the person's Codex daemon for a thread in the folder, with nothing set but
    /// the folder, so the policies are their configuration's; the start is answered with the
    /// thread Codex made, and the prompt goes as its first turn, the person's message marked with
    /// the start's intent. A start Codex cannot take is refused before it is asked, and a repeat
    /// of the intent starts nothing. A TUI found on it names its terminal while it runs.
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
        // What Codex's answer and the account say of the thread: its policy, its sandbox, the
        // account's window, which names no thread.
        let state = until(&host, thread, |s| !s.meters.limits.is_empty()).await;
        assert_eq!(state.meters.mode.as_deref(), Some("on-request"));
        assert_eq!(state.meta.facts.get("sandbox").map(String::as_str), Some("readOnly"));
        assert_eq!(state.meters.limits[0].name, "five-hour");
        assert_eq!(state.meters.limits[0].used_bp, 3_000);
        // A Codex TUI on the thread that the worker did not open, found by the agent scan in a
        // terminal: the thread names that terminal while it runs, and none once it has gone.
        // Another terminal's end leaves it named.
        let tile = SessionId::new();
        handle.tui_runs(&native, tile, true);
        polled(&host, thread, |s| s.meta.terminal == Some(tile)).await;
        handle.tui_runs(&native, SessionId::new(), false);
        handle.tui_runs(&native, tile, false);
        polled(&host, thread, |s| s.meta.terminal.is_none()).await;

        served.abort();
        let _done = tokio::time::timeout(BOUND, server).await;
        let mut sent = Vec::new();
        while let Ok(msg) = heard.try_recv() {
            sent.push(msg);
        }
        let starts: Vec<&Value> = sent.iter().filter(|m| m["method"] == "thread/start").collect();
        let cwd = work.to_string_lossy();
        assert_eq!(starts.len(), 1, "one thread asked for: {sent:?}");
        let want = json!({ "cwd": cwd, "config": unclaimed() });
        assert_eq!(starts[0]["params"], want, "nothing loosened, no terminal claimed");
        let turns: Vec<&Value> = sent.iter().filter(|m| m["method"] == "turn/start").collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["params"]["input"][0]["text"], "Say hello.");
        assert_eq!(turns[0]["params"]["threadId"], native.as_str());
        assert!(
            !sent.iter().any(|m| m["method"] == "account/usage/read"),
            "no spend is asked: {sent:?}"
        );
    }

    /// A seated thread (an agent another started) is asked of Codex with the seat's variables
    /// set in the commands it runs, Slopty's tools among its MCP servers given those variables'
    /// values (the daemon's environment is nobody's seat, so no `env_vars`), the system text as
    /// its developer instructions, the approval policy its start names and a sandbox that writes
    /// only in its workspace; the first message goes as written. Its row names the seat, kept
    /// whatever Codex says of the thread, and the policy and sandbox Codex says it runs with; a
    /// start repeated at the seat answers with the thread the first one started.
    #[tokio::test]
    async fn a_seated_start_gives_codex_the_seat_its_tools_and_its_system_text() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let server = tokio::spawn(daemon(listener, tx));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let served = codex::spawn(host.clone(), socket, None, asks);

        let seat = SessionId::new();
        let env = vec![
            ("SLOPTY_SERVER".to_owned(), "studio:7171".to_owned()),
            ("SLOPTY_SESSION".to_owned(), seat.to_string()),
            ("SLOPTY_SESSION_TOKEN".to_owned(), "proof".to_owned()),
        ];
        let given = env.clone();
        host.set_seat_env(std::sync::Arc::new(move |_| given.clone()));
        let relay = "/opt/slopty/bin/slopty";
        let seated = Seated {
            seat,
            facts: Vec::new(),
            system: Some("You review.".to_owned()),
            relay: Some(relay.to_owned()),
        };
        let held = Start { mode: Some("on-request".to_owned()), ..start(&work, "Say hello.") };
        let outcome = handle.start_seated(held.clone(), seated.clone()).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let again = handle.start_seated(held, seated).await;
        assert_eq!(again, outcome, "one thread a seat");
        assert_eq!(host.seated_at(seat), Some(thread));
        let state = until(&host, thread, |s| {
            s.turns.first().is_some_and(|t| t.state == TurnState::Complete)
        })
        .await;
        let fact = state.meta.facts.get(slopty_proto::agent::SEAT_FACT);
        assert_eq!(fact, Some(&seat.to_string()), "kept over what Codex says of the thread");
        assert_eq!(state.meters.mode.as_deref(), Some("on-request"), "the policy Codex said");
        let sandbox = state.meta.facts.get("sandbox").map(String::as_str);
        assert_eq!(sandbox, Some("readOnly"), "the sandbox Codex said, as the stand-in answers");

        served.abort();
        let _done = tokio::time::timeout(BOUND, server).await;
        let mut sent = Vec::new();
        while let Ok(msg) = heard.try_recv() {
            sent.push(msg);
        }
        let starts: Vec<&Value> = sent.iter().filter(|m| m["method"] == "thread/start").collect();
        assert_eq!(starts.len(), 1, "one thread asked for: {sent:?}");
        let config = shared::seated(&env, Some(relay));
        assert_eq!(config["shell_environment_policy.set.SLOPTY_SESSION"], seat.to_string());
        let tools = json!({
            "SLOPTY_SERVER": "studio:7171",
            "SLOPTY_SESSION": seat.to_string(),
            "SLOPTY_SESSION_TOKEN": "proof",
        });
        assert_eq!(config["mcp_servers.slopty.env"], tools, "every value, as the seat has it");
        assert!(!config.contains_key("mcp_servers.slopty.env_vars"), "no name to look up");
        let want = json!({
            "cwd": work.to_string_lossy(),
            "config": config,
            "developerInstructions": "You review.",
            "approvalPolicy": "on-request",
            "sandbox": "workspace-write",
        });
        assert_eq!(starts[0]["params"], want);
        let turns: Vec<&Value> = sent.iter().filter(|m| m["method"] == "turn/start").collect();
        assert_eq!(
            turns[0]["params"]["input"][0]["text"], "Say hello.",
            "the system text went apart"
        );
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

    /// `answer`, its thread in folder `cwd` when one is given.
    fn in_folder(mut answer: Value, cwd: Option<&str>) -> Value {
        if let Some(cwd) = cwd {
            answer["result"]["thread"]["cwd"] = json!(cwd);
        }
        answer
    }

    /// A stand-in daemon with no thread loaded that takes the recording's thread up again, forks
    /// it into `forked-1`, lists it among the folder's threads under the name Codex keeps, and
    /// answers a turn as the recording did. Every frame the worker sent goes to `heard`.
    async fn brancher(
        listener: UnixListener,
        heard: mpsc::UnboundedSender<Value>,
        cwd: Option<String>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let mut answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [], "nextCursor": null } })
                }
                Some("thread/resume") => in_folder(resumed(), cwd.as_deref()),
                Some("thread/fork") => {
                    let mut forked = in_folder(resumed(), cwd.as_deref());
                    let from = forked["result"]["thread"]["id"].clone();
                    forked["result"]["thread"]["id"] = json!("forked-1");
                    forked["result"]["thread"]["forkedFromId"] = from;
                    forked
                }
                Some("thread/list") => {
                    let mut listed = resumed()["result"]["thread"].clone();
                    listed["name"] = json!("Fix the login");
                    listed["updatedAt"] = json!(1_700_000_000);
                    json!({ "result": { "data": [listed], "nextCursor": null } })
                }
                Some("turn/start") => lines[recorded(&lines, "turn/start").1].msg.clone(),
                _ => continue,
            };
            answer["id"] = id;
            say(&mut ws, &answer).await;
        }
    }

    /// Sent with files, a turn gives Codex the person's words with each file's path after
    /// them, and each picture as a local image Codex reads itself. A fork asks Codex to branch
    /// the thread and follows the branch, which says where it came from; Codex's threads in a
    /// folder are listed with the words that take each up again.
    #[tokio::test]
    async fn files_go_by_path_and_a_fork_and_a_listing_go_to_codex() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(brancher(listener, tx, None));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);

        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let mut again = start(dir.path(), "");
        again.prompt = None;
        again.args = shared::resume_args(&native);
        let Outcome::Started { thread } = handle.start(IntentId::new(), again).await else {
            panic!("not taken up again");
        };

        let shot = dir.path().join("shot.png");
        std::fs::write(&shot, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        let notes = dir.path().join("my notes.md");
        std::fs::write(&notes, "# notes").unwrap();
        let paths = [&notes, &shot].map(|p| p.to_string_lossy().into_owned());
        let send = IntentId::new();
        handle.send(thread, "Look.".to_owned(), paths.to_vec(), Delivery::Steer, send);
        let turn = tokio::time::timeout(BOUND, async {
            loop {
                let msg = heard.recv().await.unwrap();
                if msg["method"] == "turn/start" {
                    return msg;
                }
            }
        })
        .await
        .unwrap();
        let words = format!("Look. '{}'", paths[0]);
        assert_eq!(
            turn["params"]["input"],
            json!([
                { "type": "text", "text": words, "text_elements": [] },
                { "type": "localImage", "path": paths[1] },
            ])
        );

        let fork = IntentId::new();
        let outcome = tokio::time::timeout(BOUND, handle.fork(thread, fork, None)).await.unwrap();
        let forked = shared::thread_of("forked-1");
        assert_eq!(outcome, Outcome::Started { thread: forked });
        assert_eq!(handle.fork(thread, fork, None).await, outcome, "forked once");
        let state = until(&host, forked, |s| s.meta.forked_from.is_some()).await;
        let from = host.state(thread).unwrap().0.last_turn().map(|t| t.id);
        assert_eq!(state.meta.forked_from, Some(Fork { thread, turn: from }));
        assert_eq!(state.meta.origin, ThreadMeta::FORK);

        let cwd = dir.path().to_string_lossy().into_owned();
        let listed = handle.sessions(cwd.clone(), 5).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].native, native);
        assert_eq!(listed[0].title.as_deref(), Some("Fix the login"));
        assert_eq!(listed[0].resume, shared::resume_args(&native));
        assert_eq!(listed[0].updated_ms, Some(slopty_core::WallMs::from_millis(1_700_000_000_000)));
        let mut sent = Vec::new();
        while let Ok(msg) = heard.try_recv() {
            sent.push(msg);
        }
        let forks: Vec<&Value> = sent.iter().filter(|m| m["method"] == "thread/fork").collect();
        assert_eq!(forks.len(), 1, "asked once: {sent:?}");
        assert_eq!(forks[0]["params"], json!({ "threadId": native, "config": unclaimed() }));
        let lists: Vec<&Value> = sent.iter().filter(|m| m["method"] == "thread/list").collect();
        assert_eq!(lists[0]["params"]["cwd"], json!(cwd));
        assert_eq!(lists[0]["params"]["limit"], 5);
    }

    /// A stand-in daemon that has the recording's thread loaded, takes it up again on each
    /// `thread/resume`, lets it go on `thread/unsubscribe` and then says it is unloaded, and
    /// answers a turn as the recording did. Every frame the worker sent goes to `heard`.
    async fn unloader(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let native = resumed()["result"]["thread"]["id"].clone();
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let mut answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [native], "nextCursor": null } })
                }
                Some("thread/resume") => resumed(),
                Some("thread/unsubscribe") => json!({ "result": { "status": "unsubscribed" } }),
                Some("turn/start") => lines[recorded(&lines, "turn/start").1].msg.clone(),
                _ => continue,
            };
            answer["id"] = id;
            say(&mut ws, &answer).await;
            if msg["method"] == "thread/unsubscribe" {
                let unloaded = json!({ "method": "thread/status/changed", "params": {
                    "threadId": native, "status": { "type": "notLoaded" } } });
                say(&mut ws, &unloaded).await;
            }
        }
    }

    /// The frames the worker sends from now on, until one says `method`.
    async fn until_sent(heard: &mut mpsc::UnboundedReceiver<Value>, method: &str) -> Vec<Value> {
        tokio::time::timeout(BOUND, async {
            let mut sent = Vec::new();
            loop {
                let msg = heard.recv().await.unwrap();
                let done = msg["method"] == method;
                sent.push(msg);
                if done {
                    return sent;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {method}"))
    }

    /// A thread that rests with nobody following it is let go (`thread/unsubscribe`), so Codex
    /// can unload it, and stays in the table as it was; Codex saying it is unloaded does not take
    /// it up again. A follower keeps it. A message to a thread let go takes it up again first,
    /// then goes as its turn, and a follow takes it up again too.
    #[tokio::test]
    async fn a_rested_thread_nobody_follows_is_let_go_and_taken_up_again() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(unloader(listener, tx));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let rest = Duration::from_millis(300);
        let _served = codex::spawn(host.clone(), socket, None, asks.rest_after(rest));
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let resumed = until_sent(&mut heard, "thread/resume").await;
        let want = json!({ "threadId": native, "config": unclaimed() });
        assert_eq!(resumed.last().unwrap()["params"], want, "no terminal claimed");
        let state = until(&host, thread, |s| !s.turns.is_empty()).await;

        let follower = host.follow(thread, None, 1).unwrap();
        tokio::time::sleep(rest * 4).await;
        let mut sent = Vec::new();
        while let Ok(msg) = heard.try_recv() {
            sent.push(msg);
        }
        assert!(sent.iter().all(|m| m["method"] != "thread/unsubscribe"), "followed: {sent:?}");
        drop(follower);
        let sent = until_sent(&mut heard, "thread/unsubscribe").await;
        assert_eq!(sent.last().unwrap()["params"], json!({ "threadId": native }));
        tokio::time::sleep(rest * 2).await;
        assert!(heard.try_recv().is_err(), "an unloaded thread is not taken up again");
        let (kept, _) = host.state(thread).unwrap();
        assert_eq!(kept.turns, state.turns, "kept in the table as it was");
        assert_eq!(kept.status.liveness, state.status.liveness);

        let send = IntentId::new();
        handle.send(thread, "Say hello.".to_owned(), Vec::new(), Delivery::Steer, send);
        let sent = until_sent(&mut heard, "turn/start").await;
        let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
        assert_eq!(methods, ["thread/resume", "turn/start"], "taken up again first");
        assert_eq!(sent[1]["params"]["threadId"], native.as_str());

        until_sent(&mut heard, "thread/unsubscribe").await;
        handle.wake(thread);
        until_sent(&mut heard, "thread/resume").await;
    }

    /// A stand-in for the person's Codex after the worker restarted: it holds no thread loaded
    /// (the worker had let the recording's go, and Codex unloaded it), takes it up again on
    /// `thread/resume` and answers a turn as the recording did.
    async fn restarted(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let mut answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [], "nextCursor": null } })
                }
                Some("thread/resume") => resumed(),
                Some("turn/start") => lines[recorded(&lines, "turn/start").1].msg.clone(),
                _ => continue,
            };
            answer["id"] = id;
            say(&mut ws, &answer).await;
        }
    }

    /// A thread the worker let go before it restarted is not among those Codex holds when the
    /// worker joins it again, yet stays in the table. A message to it takes it up again first
    /// and goes as its turn, rather than vanishing.
    #[tokio::test]
    async fn a_thread_let_go_before_a_restart_is_taken_up_again_by_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        {
            let socket: PathBuf = short.path().join("before.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (tx, _heard) = mpsc::unbounded_channel();
            let _server = tokio::spawn(unloader(listener, tx));
            let host = host(dir.path());
            let (_handle, asks) = Codex::channel();
            let served = codex::spawn(host.clone(), socket, None, asks);
            until(&host, thread, |s| !s.turns.is_empty()).await;
            served.abort();
            let _ended = served.await;
        }

        let socket: PathBuf = short.path().join("after.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(restarted(listener, tx));
        let host = host(dir.path());
        assert!(host.holds(thread), "kept in the table across the restart");
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        handle.send(thread, "Say hello.".to_owned(), Vec::new(), Delivery::Steer, IntentId::new());
        let sent = until_sent(&mut heard, "turn/start").await;
        let methods: Vec<&str> = sent
            .iter()
            .filter_map(|m| m["method"].as_str())
            .filter(|m| !matches!(*m, "initialize" | "initialized" | "model/list"))
            .collect();
        assert_eq!(methods, ["thread/loaded/list", "thread/resume", "turn/start"]);
        assert_eq!(sent.last().unwrap()["params"]["threadId"], native.as_str());
    }

    /// A stand-in daemon that has the recording's thread loaded and takes it up again, then
    /// takes each `thread/settings/update` and tells every client the settings it now holds
    /// (`thread/settings/updated`), but refuses a switch to `mini-model`, as Codex refuses one
    /// it cannot make. Every frame the worker sent goes to `heard`.
    async fn switcher(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let resumed = resumed();
        let native = resumed["result"]["thread"]["id"].clone();
        let mut held = json!({
            "approvalPolicy": "on-request", "approvalsReviewer": "user",
            "collaborationMode": { "mode": "default", "settings": { "model": "mock-model" } },
            "cwd": resumed["result"]["cwd"], "model": "mock-model", "modelProvider": "mock",
            "sandboxPolicy": { "type": "readOnly" }, "effort": "medium",
        });
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [native], "nextCursor": null } })
                }
                Some("thread/resume") => resumed.clone(),
                Some("thread/settings/update") if msg["params"]["model"] == "mini-model" => {
                    json!({ "error": { "code": -32600, "message": "mini-model is not available" } })
                }
                Some("thread/settings/update") => {
                    for (field, value) in msg["params"].as_object().unwrap() {
                        if field != "threadId" {
                            held[field] = value.clone();
                        }
                    }
                    json!({ "result": {} })
                }
                _ => continue,
            };
            let mut answer = answer;
            answer["id"] = id;
            say(&mut ws, &answer).await;
            if msg["method"] == "thread/settings/update" && answer.get("result").is_some() {
                let told = json!({ "method": "thread/settings/updated", "params": {
                    "threadId": native, "threadSettings": held } });
                say(&mut ws, &told).await;
            }
        }
    }

    /// A stand-in daemon that has the recording's thread loaded and begins Codex's reviewer on
    /// the first `review/start`, refusing a second as Codex refuses one while a review runs.
    /// Every frame the worker sent goes to `heard`.
    async fn reviewer(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let resumed = resumed();
        let native = resumed["result"]["thread"]["id"].clone();
        let mut reviewing = false;
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [native], "nextCursor": null } })
                }
                Some("thread/resume") => resumed.clone(),
                Some("review/start") if reviewing => {
                    json!({ "error": { "code": -32600, "message": "a review is already running" } })
                }
                Some("review/start") => {
                    reviewing = true;
                    json!({ "result": { "reviewThreadId": native, "turn": {
                        "id": "review-turn", "items": [], "itemsView": "notLoaded",
                        "status": "inProgress", "error": null, "startedAt": 0,
                        "completedAt": null, "durationMs": null } } })
                }
                _ => continue,
            };
            let mut answer = answer;
            answer["id"] = id;
            say(&mut ws, &answer).await;
        }
    }

    /// A review goes to Codex's own reviewer over the head commit of the change, in the thread
    /// (`review/start`, inline), and is done once Codex has begun it; one Codex refuses says so
    /// in Codex's words.
    #[tokio::test]
    async fn a_review_goes_to_codexs_reviewer_over_the_head_commit() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(reviewer(listener, tx));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let state = until(&host, thread, |s| !s.turns.is_empty()).await;
        assert!(state.meta.can(slopty_proto::thread::Cap::REVIEW));

        let title = "The changes on show".to_owned();
        let outcome =
            tokio::time::timeout(BOUND, handle.review(thread, "9d1e7aa".to_owned(), title.clone()))
                .await
                .unwrap();
        assert_eq!(outcome, Outcome::Done);
        let sent = until_sent(&mut heard, "review/start").await;
        assert_eq!(
            sent.last().unwrap()["params"],
            json!({ "threadId": native, "delivery": "inline",
                "target": { "type": "commit", "sha": "9d1e7aa", "title": title } })
        );
        let again = handle.review(thread, "9d1e7aa".to_owned(), "again".to_owned()).await;
        let Outcome::Refused { reason } = again else { panic!("{again:?}") };
        assert!(reason.contains("a review is already running"), "{reason}");
    }

    /// A stand-in daemon whose one loaded thread was archived outside Slopty: it refuses the
    /// first resume as Codex does, and takes it up again once unarchived.
    async fn archivist(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let resumed = resumed();
        let native = resumed["result"]["thread"]["id"].clone();
        let mut archived = true;
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [native], "nextCursor": null } })
                }
                Some("thread/resume") if archived => {
                    let said = format!("session {} is archived", native.as_str().unwrap());
                    json!({ "error": { "code": -32600, "message": said } })
                }
                Some("thread/resume") => resumed.clone(),
                Some("thread/unarchive") => {
                    archived = false;
                    json!({ "result": { "thread": resumed["result"]["thread"] } })
                }
                _ => continue,
            };
            let mut answer = answer;
            answer["id"] = id;
            say(&mut ws, &answer).await;
        }
    }

    /// A thread archived outside Slopty is put back from Codex's archive and taken up again by
    /// the same resume, once (`thread/unarchive`), never by starting a fresh session.
    #[tokio::test]
    async fn an_archived_thread_is_unarchived_and_taken_up_again() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(archivist(listener, tx));
        let host = host(dir.path());
        let (_handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let sent = until_sent(&mut heard, "thread/unarchive").await;
        let asked: Vec<&str> = sent
            .iter()
            .filter_map(|m| m["method"].as_str())
            .filter(|m| m.starts_with("thread/") && *m != "thread/loaded/list")
            .collect();
        assert_eq!(asked, ["thread/resume", "thread/unarchive"]);
        let state = polled(&host, thread, |s| !s.turns.is_empty()).await;
        assert!(!state.turns.is_empty(), "taken up again, the thread as Codex holds it");
    }

    /// A followed thread offers Codex's models, its model's efforts and the approval policies.
    /// A switch goes to Codex as the thread's settings for its next turns, the TUI's included
    /// (`thread/settings/update`), naming only what changes; the meters say what Codex then
    /// holds. A switch Codex refuses leaves the meters as they were and says why in the thread.
    #[tokio::test]
    async fn a_switch_goes_to_codex_as_the_threads_settings() {
        use slopty_agent::codex::shared::Setting;
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(switcher(listener, tx));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let state = until(&host, thread, |s| !s.meta.efforts.is_empty()).await;
        let models: Vec<&str> = state.meta.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(models, ["mock-model", "mini-model"]);
        let efforts: Vec<&str> = state.meta.efforts.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(efforts, ["low", "medium", "high"], "the running model's");
        let modes: Vec<&str> = state.meta.modes.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(modes, ["untrusted", "on-request", "never"]);
        assert_eq!(state.meters.model.as_deref(), Some("Mock"));
        handle.set(thread, Setting::Effort("high".to_owned()));
        let sent = until_sent(&mut heard, "thread/settings/update").await;
        assert_eq!(
            sent.last().unwrap()["params"],
            json!({ "threadId": native, "effort": "high" }),
            "only what changes"
        );
        until(&host, thread, |s| s.meters.effort.as_deref() == Some("high")).await;

        handle.set(thread, Setting::Mode("never".to_owned()));
        let sent = until_sent(&mut heard, "thread/settings/update").await;
        assert_eq!(
            sent.last().unwrap()["params"],
            json!({ "threadId": native, "approvalPolicy": "never" })
        );
        until(&host, thread, |s| s.meters.mode.as_deref() == Some("never")).await;

        handle.set(thread, Setting::Model("mini-model".to_owned()));
        let sent = until_sent(&mut heard, "thread/settings/update").await;
        assert_eq!(
            sent.last().unwrap()["params"],
            json!({ "threadId": native, "model": "mini-model", "effort": "low" }),
            "high is not the smaller model's, so its own default goes with it"
        );
        // A notice moves nothing in the table, which `until` waits on: it is looked for again.
        let refused = |s: &ThreadState| {
            s.items.iter().any(
                |i| matches!(&i.body, ItemBody::Notice(n) if n.text.text.contains("not available")),
            )
        };
        let state = polled(&host, thread, refused).await;
        assert_eq!(state.meters.model_id.as_deref(), Some("mock-model"), "refused, so kept");
        assert_eq!(state.meters.effort.as_deref(), Some("high"));
    }

    /// A stand-in daemon that has the recording's thread loaded and runs its second turn on the
    /// first `turn/start`, as the recording did. On each word from `reverts` it says another
    /// client rewrote the thread's history (`thread/reverted`). Its first resume answers with the
    /// thread as the recording left it, the second with the second turn still running, and every
    /// later one with that turn undone. Every frame the worker sent goes to `heard`.
    async fn reverter(
        listener: UnixListener,
        heard: mpsc::UnboundedSender<Value>,
        mut reverts: mpsc::UnboundedReceiver<()>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let resumed = resumed();
        let native = resumed["result"]["thread"]["id"].clone();
        let second = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.sent && l.msg["method"] == "turn/start")
            .nth(1)
            .unwrap()
            .0;
        let asked = &lines[second].msg["id"];
        let rest = &lines[second..];
        let turn = rest.iter().find(|l| !l.sent && l.msg["id"] == *asked).unwrap().msg.clone();
        let started = rest.iter().find(|l| l.msg["method"] == "turn/started").unwrap().msg.clone();
        let mut running = resumed.clone();
        let thread = &mut running["result"]["thread"];
        thread["turns"].as_array_mut().unwrap().push(turn["result"]["turn"].clone());
        thread["status"] = json!({ "type": "active", "activeFlags": [] });
        let (mut resumes, mut turns) = (0_u32, 0_u32);
        loop {
            let msg = tokio::select! {
                msg = next(&mut ws, &heard) => match msg { Some(msg) => msg, None => return },
                Some(()) = reverts.recv() => {
                    let reverted = json!({ "method": "thread/reverted", "params": {
                        "threadId": native } });
                    say(&mut ws, &reverted).await;
                    continue;
                }
            };
            let mut answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [native], "nextCursor": null } })
                }
                Some("thread/resume") => {
                    resumes = resumes.saturating_add(1);
                    if resumes == 2 { running.clone() } else { resumed.clone() }
                }
                Some("turn/start") if turns == 0 => {
                    turns = 1;
                    turn.clone()
                }
                _ => continue,
            };
            answer["id"] = msg["id"].clone();
            say(&mut ws, &answer).await;
            if msg["method"] == "turn/start" {
                say(&mut ws, &started).await;
            }
        }
    }

    /// A thread another client rewrote (`thread/reverted`) is taken up again and read whole, and
    /// the message the person queued stays held through it. While the re-read thread still runs
    /// its turn the message waits for it; once a re-read shows that turn undone, nothing is left
    /// to end, so the message goes as the next turn.
    #[tokio::test]
    async fn a_reverted_thread_is_read_again_and_keeps_its_held_message() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let (revert, reverts) = mpsc::unbounded_channel();
        let _server = tokio::spawn(reverter(listener, tx, reverts));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        until_sent(&mut heard, "thread/resume").await;
        until(&host, thread, |s| s.turns.len() == 1).await;

        let first = "Make a file called made-by-codex.".to_owned();
        handle.send(thread, first, Vec::new(), Delivery::Steer, IntentId::new());
        until_sent(&mut heard, "turn/start").await;
        polled(&host, thread, |s| s.turns.len() == 2).await;
        let queued = IntentId::new();
        handle.send(thread, "Then say done.".to_owned(), Vec::new(), Delivery::Queue, queued);
        let held = |s: &ThreadState| s.pending.iter().map(|p| p.intent).eq([queued]);
        polled(&host, thread, held).await;

        revert.send(()).unwrap();
        let sent = until_sent(&mut heard, "thread/resume").await;
        let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
        assert_eq!(methods, ["thread/resume"], "read again, and the turn still runs: nothing sent");
        let (state, _) = host.state(thread).unwrap();
        assert_eq!(state.turns.len(), 2, "as the re-read says");
        assert!(held(&state), "held through it: {:?}", state.pending);

        revert.send(()).unwrap();
        let sent = until_sent(&mut heard, "turn/start").await;
        let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
        assert_eq!(methods, ["thread/resume", "turn/start"], "the turn undone: it goes");
        let params = &sent[1]["params"];
        assert_eq!(params["threadId"], native.as_str());
        assert_eq!(params["input"][0]["text"], "Then say done.");
        assert_eq!(params["clientUserMessageId"], queued.to_string());
        let (state, _) = host.state(thread).unwrap();
        assert_eq!(state.turns.len(), 1, "the undone turn is gone");
        assert!(state.pending.is_empty(), "taken off the queue: {:?}", state.pending);
    }

    /// A stand-in daemon that has the recording's thread loaded. It lists two skills for the
    /// thread's folder, one of them turned off, and two commands left running in the
    /// background, a page each, until the turn `turn` ends; after that, none. On `cue` it says
    /// the skills changed (`"skills"`) or that the recording's turn completed (`"turn"`). Every
    /// frame the worker sent goes to `heard`.
    async fn background(
        listener: UnixListener,
        heard: mpsc::UnboundedSender<Value>,
        mut cue: mpsc::UnboundedReceiver<&'static str>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let resumed = resumed();
        let native = resumed["result"]["thread"]["id"].clone();
        let completed = lines.iter().find(|l| l.msg["method"] == "turn/completed").unwrap();
        let completed = completed.msg.clone();
        let mut ended = false;
        let skill = |name: &str, enabled: bool| {
            json!({ "name": name, "description": format!("{name} it"), "enabled": enabled,
                "path": format!("/skills/{name}/SKILL.md"), "scope": "repo" })
        };
        let terminal = |id: &str, command: &str| json!({ "processId": id, "command": command, "cwd": "/w", "itemId": format!("call-{id}") });
        loop {
            let msg = tokio::select! {
                frame = ws.next() => match frame {
                    Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text).unwrap(),
                    _ => return,
                },
                Some(said) = cue.recv() => {
                    if said == "skills" {
                        say(&mut ws, &json!({ "method": "skills/changed", "params": {} })).await;
                    } else {
                        ended = true;
                        say(&mut ws, &completed).await;
                    }
                    continue;
                }
            };
            heard.send(msg.clone()).unwrap();
            let result = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg["result"].clone(),
                Some("thread/loaded/list") => json!({ "data": [native], "nextCursor": null }),
                Some("thread/resume") => resumed["result"].clone(),
                Some("model/list") => catalog(),
                Some("skills/list") => json!({ "data": [{
                    "cwd": msg["params"]["cwds"][0], "errors": [],
                    "skills": [skill("deploy", true), skill("lint", false)] }] }),
                Some("thread/backgroundTerminals/list") if ended => {
                    json!({ "data": [], "nextCursor": null })
                }
                Some("thread/backgroundTerminals/list") if msg["params"]["cursor"].is_null() => {
                    json!({ "data": [terminal("p1", "npm run dev")], "nextCursor": "2" })
                }
                Some("thread/backgroundTerminals/list") => {
                    json!({ "data": [terminal("p2", "cargo watch")], "nextCursor": null })
                }
                _ => continue,
            };
            say(&mut ws, &json!({ "id": msg["id"], "result": result })).await;
        }
    }

    /// A followed thread's commands are its folder's enabled skills, asked again when Codex says
    /// they changed; its background work is the commands Codex left running, read page by page
    /// as it is followed, and read again when a turn ends, which here shows them gone.
    #[tokio::test]
    async fn a_thread_offers_its_skills_and_shows_what_runs_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let (cue, cues) = mpsc::unbounded_channel();
        let _server = tokio::spawn(background(listener, tx, cues));
        let host = host(dir.path());
        let (_handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);

        let state = polled(&host, thread, |s| !s.commands.is_empty() && s.tasks.len() == 2).await;
        let names: Vec<&str> = state.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["deploy"], "the enabled skill only");
        let running: Vec<&str> = state.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(running, ["npm run dev", "cargo watch"], "both pages");
        let sent = until_sent(&mut heard, "thread/backgroundTerminals/list").await;
        let asked = &sent.last().unwrap()["params"];
        assert_eq!(asked["threadId"], native.as_str());
        let skills = sent.iter().find(|m| m["method"] == "skills/list").unwrap();
        assert_eq!(skills["params"]["cwds"][0], state.meta.cwd.as_str(), "the thread's folder");

        cue.send("skills").unwrap();
        until_sent(&mut heard, "skills/list").await;
        cue.send("turn").unwrap();
        until_sent(&mut heard, "thread/backgroundTerminals/list").await;
        polled(&host, thread, |s| s.tasks.is_empty()).await;
    }

    /// A host in `dir` that keeps the recording's thread, as a worker does after Codex let it
    /// go: one link to a stand-in that has it loaded, ended once the thread has its turns.
    async fn kept_thread(dir: &Path, short: &Path) -> (Host, ThreadId) {
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let socket: PathBuf = short.join("before.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, _heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(unloader(listener, tx));
        let host = host(dir);
        let (_handle, asks) = Codex::channel();
        let served = codex::spawn(host.clone(), socket, None, asks);
        until(&host, thread, |s| !s.turns.is_empty()).await;
        served.abort();
        let _ended = served.await;
        (host, thread)
    }

    /// A thread someone follows while Codex restarts is taken up again as soon as Codex is
    /// joined, with nothing asked of it, so it goes on for whoever watches.
    #[tokio::test]
    async fn a_followed_thread_is_taken_up_again_once_codex_is_back() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let _watching = host.watch(thread).unwrap();
        let socket: PathBuf = short.path().join("after.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(restarted(listener, tx));
        let (_handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let sent = until_sent(&mut heard, "thread/resume").await;
        let native = resumed()["result"]["thread"]["id"].clone();
        assert_eq!(sent.last().unwrap()["params"]["threadId"], native);
    }

    /// A thread the host still holds at work that Codex no longer has loaded (a link that missed
    /// the end of its turn, a worker that restarted meanwhile) is taken up again as soon as Codex
    /// is joined, with nobody following it, and reads as Codex has it: not working for good.
    #[tokio::test]
    async fn a_thread_left_working_is_read_again_once_codex_is_joined() {
        use slopty_proto::thread::{Action, Liveness, Phase, Status};
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let working = Status {
            phase: Phase::Working,
            wait: None,
            liveness: Liveness::Live,
            since_ms: slopty_core::WallMs::ZERO,
        };
        host.apply(thread, vec![Action::Status(working)]);
        let socket: PathBuf = short.path().join("after.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(restarted(listener, tx));
        let (_handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let sent = until_sent(&mut heard, "thread/resume").await;
        let native = resumed()["result"]["thread"]["id"].clone();
        assert_eq!(sent.last().unwrap()["params"]["threadId"], native);
        until(&host, thread, |s| s.status.phase != Phase::Working).await;
    }

    /// A stand-in daemon that has the recording's thread loaded and, taken up again, says its
    /// turn is still under way, with none of its items; it never says the turn ended. Read again
    /// (`thread/read`), it has the thread as the recording ended it. Every frame the worker sent
    /// goes to `heard`.
    async fn silent(listener: UnixListener, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let native = resumed()["result"]["thread"]["id"].clone();
        while let Some(msg) = next(&mut ws, &heard).await {
            let id = msg["id"].clone();
            let mut answer = match msg["method"].as_str() {
                Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                Some("thread/loaded/list") => {
                    json!({ "result": { "data": [native], "nextCursor": null } })
                }
                Some("thread/resume") => {
                    let mut running = resumed();
                    let thread = &mut running["result"]["thread"];
                    thread["status"] = json!({ "type": "active", "activeFlags": [] });
                    let turn = &mut thread["turns"][0];
                    turn["status"] = json!("inProgress");
                    turn["items"] = json!([]);
                    turn["itemsView"] = json!("notLoaded");
                    running
                }
                Some("thread/read") => {
                    json!({ "result": { "thread": resumed()["result"]["thread"] } })
                }
                _ => continue,
            };
            answer["id"] = id;
            say(&mut ws, &answer).await;
        }
    }

    /// A thread followed from a turn under way that then says nothing is read again once it has
    /// been quiet a while, and reads as Codex has it: its turn ended, not working for good.
    #[tokio::test]
    async fn a_quiet_turn_is_read_again_and_ends_as_codex_has_it() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(silent(listener, tx));
        let host = host(dir.path());
        let (_handle, asks) = Codex::channel();
        let rest = Duration::from_millis(300);
        let _served = codex::spawn(host.clone(), socket, None, asks.rest_after(rest));
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let active = |s: &ThreadState| s.turns.last().is_some_and(|t| t.state == TurnState::Active);
        until(&host, thread, active).await;
        let sent = until_sent(&mut heard, "thread/read").await;
        assert_eq!(sent.last().unwrap()["params"]["threadId"], native.as_str());
        let state = until(&host, thread, |s| !active(s)).await;
        assert!(!state.items.is_empty(), "read whole");
    }

    /// A stand-in for the person's Codex whose link goes in the middle of a turn: it has the
    /// recording's thread loaded, takes it up again, starts the first turn as the recording did
    /// and leaves it running; once `cut` fires it closes the link, and the next one is Codex
    /// after a restart ([`restarted`]). Every frame the worker sent goes to `heard`.
    async fn cut_mid_turn(
        listener: UnixListener,
        heard: mpsc::UnboundedSender<Value>,
        cut: tokio::sync::oneshot::Receiver<()>,
    ) {
        let lines = starter();
        let native = resumed()["result"]["thread"]["id"].clone();
        {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut cut = std::pin::pin!(cut);
            loop {
                let msg = tokio::select! {
                    _ = &mut cut => break,
                    msg = next(&mut ws, &heard) => match msg { Some(msg) => msg, None => break },
                };
                let id = msg["id"].clone();
                let mut answer = match msg["method"].as_str() {
                    Some("initialize") => lines[recorded(&lines, "initialize").1].msg.clone(),
                    Some("thread/loaded/list") => {
                        json!({ "result": { "data": [native], "nextCursor": null } })
                    }
                    Some("thread/resume") => resumed(),
                    Some("turn/start") => lines[recorded(&lines, "turn/start").1].msg.clone(),
                    _ => continue,
                };
                answer["id"] = id;
                say(&mut ws, &answer).await;
                if msg["method"] == "turn/start" {
                    let started = lines.iter().find(|l| l.msg["method"] == "turn/started");
                    let mut started = started.unwrap().msg.clone();
                    started["params"]["threadId"] = native.clone();
                    say(&mut ws, &started).await;
                }
            }
        }
        restarted(listener, heard).await;
    }

    /// A message queued behind a turn when Codex's link goes is not lost with the link's own
    /// queue: it waits for the next link and goes there as a turn of its own, leaving the list
    /// of messages on their way.
    #[tokio::test]
    async fn a_message_queued_behind_a_turn_goes_after_codex_s_link_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let socket: PathBuf = short.path().join("after.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let (cut, cutting) = tokio::sync::oneshot::channel();
        let _server = tokio::spawn(cut_mid_turn(listener, tx, cutting));
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        handle.send(thread, "First.".to_owned(), Vec::new(), Delivery::Steer, IntentId::new());
        until_sent(&mut heard, "turn/start").await;
        polled(&host, thread, |s| s.turns.last().is_some_and(|t| t.state == TurnState::Active))
            .await;
        let queued = IntentId::new();
        handle.send(thread, "Then this.".to_owned(), Vec::new(), Delivery::Queue, queued);
        polled(&host, thread, |s| s.pending.iter().any(|p| p.intent == queued)).await;

        cut.send(()).unwrap();
        let sent = until_sent(&mut heard, "turn/start").await;
        let turn = sent.last().unwrap();
        assert_eq!(turn["params"]["input"][0]["text"], "Then this.", "{sent:?}");
        polled(&host, thread, |s| !s.pending.iter().any(|p| p.intent == queued)).await;
    }

    /// Whether `state` holds the message `intent` back, saying `reason`.
    fn held_back(state: &ThreadState, intent: IntentId, reason: &str) -> bool {
        state.pending.iter().any(|p| {
            p.intent == intent
                && matches!(&p.state, PendingState::Held { reason: r } if r == reason)
        })
    }

    /// A message written while Codex's app-server is away waits for it: once it is back, the
    /// thread is taken up again and the message goes as its turn, never said not to have gone.
    #[tokio::test]
    async fn a_message_written_while_codex_is_away_goes_once_it_is_back() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let socket: PathBuf = short.path().join("after.sock");
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket.clone(), None, asks);
        let intent = IntentId::new();
        handle.send(thread, "Say hello.".to_owned(), Vec::new(), Delivery::Steer, intent);
        // Away for a moment, as an app-server that restarts is.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(restarted(listener, tx));
        let sent = until_sent(&mut heard, "turn/start").await;
        let methods: Vec<&str> = sent
            .iter()
            .filter_map(|m| m["method"].as_str())
            .filter(|m| !matches!(*m, "initialize" | "initialized" | "model/list"))
            .collect();
        assert_eq!(methods, ["thread/loaded/list", "thread/resume", "turn/start"]);
        assert_eq!(sent.last().unwrap()["params"]["input"][0]["text"], "Say hello.");
        let (state, _) = host.state(thread).unwrap();
        assert!(!state.pending.iter().any(|p| p.intent == intent), "{:?}", state.pending);
    }

    /// A message written while Codex's app-server is away, which does not come back within
    /// [`codex::HOLD_FOR`], is held in the thread's list saying it did not go, for the person
    /// to send again.
    #[tokio::test]
    async fn a_message_codex_does_not_come_back_for_is_said_not_to_have_gone() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        // Half a minute of nobody answering, on a clock of the test's own.
        tokio::time::pause();
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), short.path().join("gone.sock"), None, asks);
        let intent = IntentId::new();
        handle.send(thread, "Say hello.".to_owned(), Vec::new(), Delivery::Steer, intent);
        let waited = tokio::time::timeout(codex::HOLD_FOR * 4, async {
            loop {
                if let Some((state, _)) = host.state(thread)
                    && held_back(&state, intent, codex::NOT_BACK)
                {
                    return state;
                }
                tokio::time::sleep(codex::RETRY).await;
            }
        });
        let state = waited.await.expect("said not to have gone");
        let held = state.pending.iter().find(|p| p.intent == intent).unwrap();
        assert_eq!(held.text, "Say hello.");
    }

    /// An answer given while Codex's app-server is away goes nowhere, as its request ended with
    /// the app-server: the thread says so at once, under the answer's intent, in its choice's
    /// words, rather than taking it as given.
    #[tokio::test]
    async fn an_answer_given_while_codex_is_away_is_said_not_to_have_gone_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), short.path().join("gone.sock"), None, asks);
        let intent = IntentId::new();
        let by = Answerer { client: None, name: "Slopty".to_owned() };
        handle.answer(thread, AskId("0".to_owned()), "accept".to_owned(), by, intent);
        let state = polled(&host, thread, |s| held_back(s, intent, codex::REQUEST_ENDED)).await;
        let held = state.pending.iter().find(|p| p.intent == intent).unwrap();
        assert_eq!(held.text, "accept", "the choice, as no request names it");
    }

    /// An answer to the worker's `msg`, with `result`.
    fn reply(msg: &Value, result: &Value) -> Value {
        json!({ "id": msg["id"], "result": result })
    }

    /// An error answer to the worker's `msg`, in Codex's `message`.
    fn refusal(msg: &Value, message: &str) -> Value {
        json!({ "id": msg["id"], "error": { "code": -32600, "message": message } })
    }

    /// The recording's answer to `turn/start`, to the worker's `msg`.
    fn turn_answer(msg: &Value) -> Value {
        let lines = starter();
        reply(msg, &lines[recorded(&lines, "turn/start").1].msg["result"])
    }

    /// The recording's own thread taken up again, as Codex answers the worker's `msg`.
    fn resume_answer(msg: &Value) -> Value {
        let mut answer = resumed();
        answer["id"] = msg["id"].clone();
        answer
    }

    /// Codex beginning a turn on the recording's thread, as the recording told it.
    fn turn_started() -> Value {
        starter().into_iter().find(|l| l.msg["method"] == "turn/started").unwrap().msg
    }

    /// The recording's thread, Codex says, is closed.
    fn closed() -> Value {
        let native = resumed()["result"]["thread"]["id"].clone();
        json!({ "method": "thread/closed", "params": { "threadId": native } })
    }

    /// A stand-in daemon that answers the handshake as the recording did, with the recording's
    /// thread loaded when `loaded` says so, and every other frame the worker sends as `script`
    /// says: the frames it gives are said back in order, and with none it closes the link and
    /// listens no more. Every frame the worker sent goes to `heard`.
    async fn scripted(
        listener: UnixListener,
        heard: mpsc::UnboundedSender<Value>,
        loaded: bool,
        mut script: impl FnMut(&Value) -> Option<Vec<Value>>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let lines = starter();
        let native = resumed()["result"]["thread"]["id"].clone();
        while let Some(msg) = next(&mut ws, &heard).await {
            let said = match msg["method"].as_str() {
                Some("initialize") => {
                    let mut answer = lines[recorded(&lines, "initialize").1].msg.clone();
                    answer["id"] = msg["id"].clone();
                    vec![answer]
                }
                Some("thread/loaded/list") => {
                    let data = if loaded { vec![native.clone()] } else { Vec::new() };
                    vec![reply(&msg, &json!({ "data": data, "nextCursor": null }))]
                }
                _ => match script(&msg) {
                    Some(said) => said,
                    None => return,
                },
            };
            for frame in said {
                say(&mut ws, &frame).await;
            }
        }
    }

    /// Ended as Codex left it: exited and resumable, its last turn stopped, nothing working.
    fn cut(state: &ThreadState) -> bool {
        use slopty_proto::thread::{Liveness, Phase};
        state.status.liveness == Liveness::Exited { resumable: true }
            && state.status.phase != Phase::Working
            && state.turns.last().is_some_and(|t| t.state != TurnState::Active)
    }

    /// A turn under way when Codex's app-server goes ends with it, stopped, rather than
    /// working for good: Codex is not there to say it ended.
    #[tokio::test]
    async fn a_turn_under_way_ends_stopped_when_codex_goes() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, _heard) = mpsc::unbounded_channel();
        let server = tokio::spawn(scripted(listener, tx, true, |msg| {
            Some(match msg["method"].as_str() {
                Some("thread/resume") => vec![resume_answer(msg)],
                Some("turn/start") => vec![turn_answer(msg), turn_started()],
                _ => Vec::new(),
            })
        }));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        until(&host, thread, |s| !s.turns.is_empty()).await;
        handle.send(thread, "Go on.".to_owned(), Vec::new(), Delivery::Steer, IntentId::new());
        let active = |s: &ThreadState| s.turns.last().is_some_and(|t| t.state == TurnState::Active);
        polled(&host, thread, active).await;

        server.abort();
        let state = until(&host, thread, cut).await;
        assert_eq!(state.turns.last().unwrap().state, TurnState::Interrupted);
    }

    /// A thread an earlier worker left working, while no Codex answers as this one starts, ends
    /// as it was left rather than working for good: no Codex is there to read it again from.
    #[tokio::test]
    async fn a_thread_left_working_ends_when_no_codex_answers_at_the_start() {
        use slopty_proto::thread::{Action, Liveness, Phase, Status};
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let working = Status {
            phase: Phase::Working,
            wait: None,
            liveness: Liveness::Live,
            since_ms: slopty_core::WallMs::ZERO,
        };
        host.apply(thread, vec![Action::Status(working)]);
        let (_handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), short.path().join("gone.sock"), None, asks);
        until(&host, thread, cut).await;
    }

    /// A thread Codex closes mid-turn ends as Codex left it, its turn stopped, and counts as let
    /// go: the next message takes it up again first, then goes as its turn.
    #[tokio::test]
    async fn a_thread_codex_closes_ends_and_is_taken_up_again_by_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let mut turns = 0_u32;
        let _server = tokio::spawn(scripted(listener, tx, true, move |msg| {
            Some(match msg["method"].as_str() {
                Some("thread/resume") => vec![resume_answer(msg)],
                Some("turn/start") => {
                    turns = turns.saturating_add(1);
                    if turns == 1 {
                        vec![turn_answer(msg), turn_started(), closed()]
                    } else {
                        vec![turn_answer(msg)]
                    }
                }
                _ => Vec::new(),
            })
        }));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        until(&host, thread, |s| !s.turns.is_empty()).await;
        handle.send(thread, "Go on.".to_owned(), Vec::new(), Delivery::Steer, IntentId::new());
        until_sent(&mut heard, "turn/start").await;
        let state = until(&host, thread, cut).await;
        assert_eq!(state.turns.last().unwrap().state, TurnState::Interrupted);

        handle.send(thread, "Again.".to_owned(), Vec::new(), Delivery::Steer, IntentId::new());
        let sent = until_sent(&mut heard, "turn/start").await;
        let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
        assert_eq!(methods, ["thread/resume", "turn/start"], "taken up again first");
        assert_eq!(sent[1]["params"]["input"][0]["text"], "Again.");
    }

    /// A thread kept at work that Codex cannot take up again yet is tried again every
    /// [`codex::RETRY`], on its own timer rather than at the next look over resting threads,
    /// and is not cut short while tries are left: once Codex takes it up, it reads as Codex has
    /// it.
    #[tokio::test]
    async fn a_thread_codex_cannot_take_up_yet_is_tried_again_before_it_is_cut_short() {
        use slopty_proto::thread::{Action, Liveness, Phase, Status};
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let (host, thread) = kept_thread(dir.path(), short.path()).await;
        let working = Status {
            phase: Phase::Working,
            wait: None,
            liveness: Liveness::Live,
            since_ms: slopty_core::WallMs::ZERO,
        };
        host.apply(thread, vec![Action::Status(working)]);
        let socket: PathBuf = short.path().join("after.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let mut resumes = 0_u32;
        let _server = tokio::spawn(scripted(listener, tx, false, move |msg| {
            Some(match msg["method"].as_str() {
                Some("thread/resume") => {
                    resumes = resumes.saturating_add(1);
                    if resumes <= 2 {
                        vec![refusal(msg, "no rollout found")]
                    } else {
                        vec![resume_answer(msg)]
                    }
                }
                _ => Vec::new(),
            })
        }));
        let (_handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        until_sent(&mut heard, "thread/resume").await;
        until_sent(&mut heard, "thread/resume").await;
        let (state, _) = host.state(thread).unwrap();
        assert_eq!(state.status.phase, Phase::Working, "not cut short while tries are left");
        until_sent(&mut heard, "thread/resume").await;
        until(&host, thread, |s| {
            s.status.phase != Phase::Working && s.status.liveness == Liveness::Live
        })
        .await;
    }

    /// A message queued behind a turn whose end went unheard goes as the next turn once the
    /// quiet thread is read again and the turn turns out to have ended.
    #[tokio::test]
    async fn a_message_queued_behind_a_quiet_turn_goes_once_it_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(scripted(listener, tx, true, |msg| {
            Some(match msg["method"].as_str() {
                Some("thread/resume") => {
                    let mut running = resume_answer(msg);
                    let thread = &mut running["result"]["thread"];
                    thread["status"] = json!({ "type": "active", "activeFlags": [] });
                    let turn = &mut thread["turns"][0];
                    turn["status"] = json!("inProgress");
                    turn["items"] = json!([]);
                    turn["itemsView"] = json!("notLoaded");
                    vec![running]
                }
                Some("thread/read") => {
                    vec![reply(msg, &json!({ "thread": resumed()["result"]["thread"] }))]
                }
                Some("turn/start") => vec![turn_answer(msg)],
                _ => Vec::new(),
            })
        }));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let rest = Duration::from_millis(300);
        let _served = codex::spawn(host.clone(), socket, None, asks.rest_after(rest));
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        let active = |s: &ThreadState| s.turns.last().is_some_and(|t| t.state == TurnState::Active);
        until(&host, thread, active).await;
        let queued = IntentId::new();
        handle.send(thread, "Then this.".to_owned(), Vec::new(), Delivery::Queue, queued);
        polled(&host, thread, |s| s.pending.iter().any(|p| p.intent == queued)).await;
        let sent = until_sent(&mut heard, "turn/start").await;
        assert!(sent.iter().any(|m| m["method"] == "thread/read"), "read again first: {sent:?}");
        assert_eq!(sent.last().unwrap()["params"]["input"][0]["text"], "Then this.");
        polled(&host, thread, |s| !s.pending.iter().any(|p| p.intent == queued)).await;
    }

    /// A message Codex refuses (its turn or its steer) is held in the thread's list saying so,
    /// in Codex's words, rather than gone, and the person can take it back.
    #[tokio::test]
    async fn a_message_codex_refuses_is_held_and_can_be_taken_back() {
        let dir = tempfile::tempdir().unwrap();
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket: PathBuf = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let _server = tokio::spawn(scripted(listener, tx, true, |msg| {
            Some(match msg["method"].as_str() {
                Some("thread/resume") => vec![resume_answer(msg)],
                Some("turn/start") => vec![refusal(msg, "the model is unavailable")],
                _ => Vec::new(),
            })
        }));
        let host = host(dir.path());
        let (handle, asks) = Codex::channel();
        let _served = codex::spawn(host.clone(), socket, None, asks);
        let native = resumed()["result"]["thread"]["id"].as_str().unwrap().to_owned();
        let thread = shared::thread_of(&native);
        until(&host, thread, |s| !s.turns.is_empty()).await;
        let intent = IntentId::new();
        handle.send(thread, "Say hello.".to_owned(), Vec::new(), Delivery::Steer, intent);
        until_sent(&mut heard, "turn/start").await;
        let reason = format!("{}: the model is unavailable", codex::REFUSED);
        let state = polled(&host, thread, |s| held_back(s, intent, &reason)).await;
        let held = state.pending.iter().find(|p| p.intent == intent).unwrap();
        assert_eq!(held.text, "Say hello.");

        handle.withdraw(thread, intent);
        polled(&host, thread, |s| !s.pending.iter().any(|p| p.intent == intent)).await;
    }
}
