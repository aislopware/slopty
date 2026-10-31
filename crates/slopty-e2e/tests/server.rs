//! The server with a real worker behind it, driven the way people and agents drive it: the
//! `slopty` binary with `--json`, and the agents' tools over MCP. `slopty-server`,
//! `slopty-ptyd` and `slopty-worker` from this build run in a temporary directory on ports of
//! their own.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e server`. It needs no permission from
//! the machine, and nothing is typed into a shell the test did not open.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, bail, ensure};
    use serde_json::{Value, json};
    use slopty_e2e::harness::ServerStack;

    /// A lease that ends without a goodbye has the 5 s idle timeout to run out. From the kill to
    /// `unreachable` in the listing took 5.99 to 6.07 s over four runs (2026-09-25), so the
    /// bound gives that a second of slack rather than sitting on it.
    const UNREACHABLE_BOUND: Duration = Duration::from_secs(7);
    /// How long anything else may take: a shell start, a registration, an exit reaching the
    /// server.
    const STEP: Duration = Duration::from_secs(10);
    /// Between two looks at something that has no event to wait on from outside.
    const POLL: Duration = Duration::from_millis(50);

    /// Ask `look` until it answers `Some`, for at most `bound`.
    async fn until<T>(
        what: &str,
        bound: Duration,
        mut look: impl AsyncFnMut() -> Result<Option<T>>,
    ) -> Result<T> {
        let started = Instant::now();
        loop {
            if let Some(found) = look().await? {
                return Ok(found);
            }
            if started.elapsed() > bound {
                bail!("{what}: not within {bound:?}");
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn str_of<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
        value[key].as_str().with_context(|| format!("no string {key:?} in {value}"))
    }

    /// Open a quiet bash (no profile, no rc; the shell integration still marks its commands).
    async fn open_bash(stack: &ServerStack, name: &str) -> Result<String> {
        open_bash_keyed(stack, name, &[]).await
    }

    /// [`open_bash`] with more arguments before the command.
    async fn open_bash_keyed(stack: &ServerStack, name: &str, more: &[&str]) -> Result<String> {
        let cwd = stack.dir.path().to_string_lossy().into_owned();
        let worker = stack.worker.name();
        let args = ["open", "--worker", worker, "--cwd", &cwd, "--name", name];
        let bash = ["--", "/bin/bash", "--noprofile", "--norc", "-i"];
        let opened = stack.slopty(&[&args[..], more, &bash[..]].concat()).await?;
        Ok(str_of(&opened, "term")?.to_owned())
    }

    async fn is_listed(stack: &ServerStack, term: &str) -> Result<bool> {
        let terminals = stack.slopty(&["terminals"]).await?;
        let all = terminals.as_array().context("terminals is a list")?;
        Ok(all.iter().any(|t| t["term"] == term))
    }

    /// The fleet's event cursor as it stands: `slopty events` read from the start, page by page,
    /// to the last `next`.
    async fn cursor_now(stack: &ServerStack) -> Result<u64> {
        let mut since = 0_u64;
        loop {
            let from = since.to_string();
            let page = stack.slopty(&["events", "--since", &from, "--timeout", "0"]).await?;
            let next = page["next"].as_u64().context("next")?;
            let read = page["events"].as_array().context("events")?.len();
            if read < 500 || next == since {
                return Ok(next);
            }
            since = next;
        }
    }

    /// The events after `since`, waiting up to `timeout_ms` for the first.
    async fn events_after(stack: &ServerStack, since: u64, timeout_ms: u32) -> Result<Value> {
        let (since, timeout) = (since.to_string(), timeout_ms.to_string());
        stack.slopty(&["events", "--since", &since, "--timeout", &timeout]).await
    }

    /// The terminal as `slopty terminals --json` lists it.
    async fn terminal_entry(stack: &ServerStack, term: &str) -> Result<Value> {
        let terminals = stack.slopty(&["terminals"]).await?;
        let all = terminals.as_array().context("terminals is a list")?;
        all.iter().find(|t| t["term"] == term).cloned().with_context(|| format!("{term} listed"))
    }

    async fn send_text(stack: &ServerStack, term: &str, text: &str) -> Result<()> {
        let done = stack.slopty(&["send", term, "--text", text]).await?;
        ensure!(done == json!({ "ok": true }), "send: {done}");
        Ok(())
    }

    /// Wall time per step, printed at the end.
    struct Clock {
        started: Instant,
        last: Instant,
        steps: Vec<(&'static str, Duration)>,
    }

    impl Clock {
        fn new() -> Self {
            let now = Instant::now();
            Self { started: now, last: now, steps: Vec::new() }
        }

        fn lap(&mut self, step: &'static str) {
            let now = Instant::now();
            self.steps.push((step, now.duration_since(self.last)));
            self.last = now;
        }

        fn report(&self) {
            for (step, took) in &self.steps {
                eprintln!("  {step:<28} {:>6} ms", took.as_millis());
            }
            eprintln!("  {:<28} {:>6} ms", "total", self.started.elapsed().as_millis());
        }
    }

    #[tokio::test]
    #[ignore = "live: cargo xtask e2e server"]
    async fn the_cli_and_mcp_drive_a_real_worker_through_the_server() {
        let mut clock = Clock::new();
        let mut stack = ServerStack::launch("e2e-worker").await.unwrap();
        clock.lap("launch");
        let outcome = scenario(&mut stack, &mut clock).await;
        stack.shutdown().await;
        clock.report();
        outcome.unwrap();
    }

    /// Search in files from a script or an agent's shell: `slopty search` narrows by glob and
    /// prints the context round a match, `.gitignore` keeps a file out, it stops at the lines
    /// asked for and says there are more, and a pattern that does not parse is refused.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e server"]
    async fn search_in_files_through_the_cli() {
        let stack = ServerStack::launch("e2e-search").await.unwrap();
        let outcome = search_scenario(&stack).await;
        stack.shutdown().await;
        outcome.unwrap();
    }

    async fn search_scenario(stack: &ServerStack) -> Result<()> {
        let root = stack.dir.path().join("project");
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join(".gitignore"), "ignored.rs\n")?;
        std::fs::write(root.join("ignored.rs"), "needle\n")?;
        std::fs::write(root.join("src/a.rs"), "fn one() {}\n    // a needle\nfn two() {}\n")?;
        std::fs::write(root.join("src/b.md"), "needle\nneedle again\n")?;
        let dir = root.to_string_lossy().into_owned();
        let worker = stack.worker.name();

        let started = Instant::now();
        let args = ["search", "needle", &dir, "--worker", worker, "--glob", "*.rs", "-C", "1"];
        let found = stack.slopty(&args).await?;
        eprintln!("  slopty search: {} ms", started.elapsed().as_millis());
        let lines = |file: &Value| -> Vec<(u64, String, bool)> {
            file["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|l| {
                    let context = l["context"] == json!(true);
                    (
                        l["line"].as_u64().unwrap_or(0),
                        str_of(l, "text").unwrap_or("").to_owned(),
                        context,
                    )
                })
                .collect()
        };
        let files = found["files"].as_array().context("files is a list")?;
        ensure!(
            files.len() == 1 && files[0]["path"] == "src/a.rs",
            "the glob and .gitignore: {found}"
        );
        ensure!(
            lines(&files[0])
                == [
                    (1, "fn one() {}".to_owned(), true),
                    (2, "// a needle".to_owned(), false),
                    (3, "fn two() {}".to_owned(), true),
                ],
            "the match and its context: {found}"
        );
        ensure!(files[0]["lines"][1]["matches"] == json!([[5, 11]]), "{found}");

        let capped = ["search", "NEEDLE", &dir, "--worker", worker, "--max", "2"];
        let found = stack.slopty(&capped).await?;
        ensure!(found["capped"] == json!(true) && found["lines"] == json!(2), "{found}");
        let cased = ["search", "NEEDLE", &dir, "--worker", worker, "--case-sensitive"];
        let found = stack.slopty(&cased).await?;
        ensure!(found["files"] == json!([]) && found["capped"] == json!(false), "{found}");

        let bad = stack.slopty(&["search", "(", &dir, "--worker", worker, "--regex"]).await;
        ensure!(bad.is_err(), "a pattern that does not parse: {bad:?}");
        Ok(())
    }

    async fn scenario(stack: &mut ServerStack, clock: &mut Clock) -> Result<()> {
        // 1. The worker is online, with its capabilities.
        let entry = stack.worker_online(STEP).await?;
        ensure!(entry["os"] == "macos", "{entry}");
        ensure!(entry["cpus"].as_u64().is_some_and(|n| n > 0), "{entry}");
        ensure!(entry["memory"].as_u64().is_some_and(|n| n > 0), "{entry}");
        ensure!(!str_of(&entry, "arch")?.is_empty(), "{entry}");
        // Its build, by which a person sees a worker to update: this tree's version and wire.
        let build = str_of(&entry, "build")?;
        ensure!(build.starts_with(slopty_proto::wire::BUILD), "{build} is not this tree's build");
        let worker_id = str_of(&entry, "worker")?.to_owned();
        clock.lap("workers");

        // 2. A shell: typed to, waited on, read back.
        let term = open_bash(stack, "e2e shell").await?;
        ensure!(term.starts_with(&format!("{worker_id}/")), "{term}");
        send_text(stack, &term, "echo slopty-e2e-$RANDOM").await?;
        let enter = stack.slopty(&["send", &term, "--keys", "enter"]).await?;
        ensure!(enter == json!({ "ok": true }), "{enter}");
        // The shell expanded `$RANDOM`: only its output matches, never the typed line.
        let waited = stack.slopty(&["wait", &term, "--output", "^slopty-e2e-[0-9]+$"]).await?;
        ensure!(waited["result"] == "met", "{waited}");
        let marker = str_of(&waited["line"], "text")?.to_owned();
        let output = stack.slopty(&["output", &term]).await?;
        let lines = output["lines"].as_array().context("output lines")?;
        ensure!(lines.iter().any(|l| l["text"] == marker.as_str()), "{marker} in {output}");
        let done = stack.slopty(&["wait", &term, "--command-done"]).await?;
        ensure!(done["result"] == "met", "{done}");
        let commands = stack.slopty(&["commands", &term]).await?;
        let commands = commands.as_array().context("commands is a list")?;
        ensure!(
            commands.iter().any(|c| c["command"] == "echo slopty-e2e-$RANDOM" && c["exit"] == 0),
            "{commands:?}"
        );
        clock.lap("open, send, wait, read");

        // 2b. An open sent again under its idempotency key, as a caller whose answer was lost
        //     sends it, answers the first terminal and opens no second.
        let count = async || -> Result<usize> {
            Ok(stack.slopty(&["terminals"]).await?.as_array().context("terminals")?.len())
        };
        let before = count().await?;
        let key = ["--idempotency-key", "e2e-open-once"];
        let once = open_bash_keyed(stack, "e2e once", &key).await?;
        let again = open_bash_keyed(stack, "e2e once", &key).await?;
        ensure!(once == again, "the same terminal: {once} and {again}");
        let opened = count().await?.saturating_sub(before);
        ensure!(opened == 1, "one terminal opened, not {opened}");
        let other = open_bash_keyed(stack, "e2e twice", &key).await;
        ensure!(other.is_err(), "the key with other arguments is refused: {other:?}");
        let closed = stack.slopty(&["close", &once]).await?;
        ensure!(closed == json!({ "ok": true }), "{closed}");
        clock.lap("open once under a key");

        // 3. Files both ways, text and binary, through standard input and output.
        let worker = stack.worker.name().to_owned();
        let text_path = stack.path("note.txt").to_string_lossy().into_owned();
        let push = ["push", "--worker", &worker, "-", &text_path];
        let pushed = stack.slopty_with_stdin(&push, b"hello, worker\n").await?;
        ensure!(pushed["size"] == 14 && pushed["local"] == "-", "{pushed}");
        ensure!(std::fs::read(&text_path)? == b"hello, worker\n", "push - wrote the file");
        let read = stack.slopty(&["pull", "--worker", &worker, &text_path, "-"]).await?;
        ensure!(read["encoding"] == "utf8" && read["content"] == "hello, worker\n", "{read}");
        let binary: Vec<u8> = (0..=255_u8).rev().chain(0..=255).collect();
        let bin_path = stack.path("blob.bin").to_string_lossy().into_owned();
        stack.slopty_with_stdin(&["push", "--worker", &worker, "-", &bin_path], &binary).await?;
        ensure!(std::fs::read(&bin_path)? == binary, "push - wrote the bytes as they are");
        let read = stack.slopty(&["pull", "--worker", &worker, &bin_path, "-"]).await?;
        ensure!(read["encoding"] == "base64" && read["size"] == binary.len(), "{read}");
        let decoded = data_encoding::BASE64.decode(str_of(&read, "content")?.as_bytes())?;
        ensure!(decoded == binary, "pull - --json read the bytes back");
        let raw = stack.slopty_bytes(&["pull", "--worker", &worker, &bin_path, "-"], b"").await?;
        ensure!(raw == binary, "pull - printed the bytes as they are");
        let part = ["pull", "--worker", &worker, &text_path, "-", "--offset", "7", "--length", "6"];
        let read = stack.slopty(&part).await?;
        ensure!(
            read["content"] == "worker" && read["size"] == 14 && read["more"] == true,
            "{read}"
        );
        ensure!(stack.slopty_bytes(&part, b"").await? == b"worker", "a range printed");
        let dir = stack.dir.path().to_string_lossy().into_owned();
        let listing = stack.slopty(&["ls", "--worker", &worker, &dir]).await?;
        let entries = listing["entries"].as_array().context("entries")?;
        ensure!(
            entries
                .iter()
                .any(|e| e["name"] == "note.txt" && e["kind"] == "file" && e["size"] == 14),
            "{listing}"
        );
        let stat = stack.slopty(&["stat", "--worker", &worker, &bin_path]).await?;
        ensure!(stat["exists"] == true && stat["size"] == binary.len(), "{stat}");
        clock.lap("put, cat, ls, stat");

        // 3b. A file past one reply goes up and comes down in parts.
        bulk(stack, &worker).await?;
        clock.lap("push, pull past 8 MiB");

        // 4. A listener started in the terminal is found in it.
        let port = {
            let free = std::net::TcpListener::bind("127.0.0.1:0")?;
            free.local_addr()?.port()
        };
        send_text(stack, &term, &format!("nc -l 127.0.0.1 {port}\n")).await?;
        let found = until("nc listening", STEP, async || {
            let ports = stack.slopty(&["ports", "--worker", &worker]).await?;
            let ports = ports.as_array().context("ports is a list")?;
            Ok(ports.iter().find(|p| p["port"] == port).cloned())
        })
        .await?;
        ensure!(found["process"] == "nc" && found["term"] == term.as_str(), "{found}");
        stack.slopty(&["send", &term, "--keys", "ctrl+c"]).await?;
        clock.lap("ports");

        // 7. The agents' tools through `slopty mcp`: listed, each once with the schema of its
        //    input, the orchestration ones among them (the registry's own list is the CLI's tests'
        //    to hold it to), and a call the hub answers. The screen of a terminal, as an agent
        //    reads it with `slopty screen`.
        let listed = stack.mcp(1, "tools/list", json!({})).await?;
        let tools = listed["result"]["tools"].as_array().context("tools")?;
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        ensure!(names.len() == tools.len() && unique.len() == names.len(), "{listed}");
        ensure!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"), "{listed}");
        let orchestration = [
            "machines",
            "agent_start",
            "agent_tell",
            "agent_read",
            "agent_wait",
            "agent_stop",
            "agents",
            "branch_bring",
        ];
        for tool in orchestration {
            ensure!(unique.contains(tool), "{tool} listed: {names:?}");
        }
        let nobody = "0199a1b1-c3d4-7000-8000-00000000beef";
        let params = json!({ "name": "agent_read", "arguments": { "agent": nobody } });
        let called = stack.mcp(2, "tools/call", params).await?;
        let said = str_of(&called["result"]["content"][0], "text")?;
        ensure!(called["result"]["isError"] == json!(true), "{called}");
        ensure!(said.contains("(UnknownAgent)"), "the hub's own words: {said}");
        let shown = until("the marker on the screen", STEP, async || {
            let screen = stack.slopty(&["screen", &term]).await?.to_string();
            Ok(screen.contains(&marker).then_some(screen))
        })
        .await?;
        ensure!(shown.contains("nc -l"), "the screen as it is now: {shown}");
        clock.lap("mcp, screen");

        // 8. One events call sees a terminal open on the fleet; a terminal opened at a size under a
        //    key is opened once, then resized, and the program in it sees the new width.
        let cursor = cursor_now(stack).await?;
        let cwd = stack.dir.path().to_string_lossy().into_owned();
        let sized_open = [
            "open",
            "--worker",
            worker.as_str(),
            "--cwd",
            &cwd,
            "--cols",
            "100",
            "--rows",
            "30",
            "--idempotency-key",
            "e2e-sized-open",
            "--",
            "/bin/bash",
            "--noprofile",
            "--norc",
            "-i",
        ];
        let sized = stack.slopty(&sized_open).await?;
        let sized = str_of(&sized, "term")?.to_owned();
        let repeated = stack.slopty(&sized_open).await?;
        ensure!(repeated["term"] == sized.as_str(), "the same terminal: {repeated}");
        let heard = events_after(stack, cursor, 10_000).await?;
        let opened = heard["events"].as_array().context("events")?;
        let opens = opened.iter().filter(|e| e["kind"] == "session_opened").count();
        ensure!(opens == 1, "one terminal opened, not {opens}: {heard}");
        ensure!(
            opened.iter().any(|e| e["kind"] == "session_opened" && e["term"] == sized.as_str()),
            "{heard}"
        );
        let entry = terminal_entry(stack, &sized).await?;
        ensure!(entry["cols"] == 100 && entry["rows"] == 30, "opened at its size: {entry}");
        let resized = stack.slopty(&["resize", &sized, "--cols", "150", "--rows", "40"]).await?;
        ensure!(resized == json!({ "ok": true }), "{resized}");
        let entry = terminal_entry(stack, &sized).await?;
        ensure!(entry["cols"] == 150 && entry["rows"] == 40, "listed at the new size: {entry}");
        send_text(stack, &sized, "tput cols\n").await?;
        let width = stack.slopty(&["wait", &sized, "--output", "^150$"]).await?;
        ensure!(width["result"] == "met", "the program sees 150 columns: {width}");
        let screen = stack.slopty(&["screen", &sized]).await?;
        let rows = screen["lines"].as_array().context("lines")?.len();
        ensure!(rows == 40, "40 rows on the screen, not {rows}");
        let after = heard["next"].as_u64().context("next")?;
        let closed = stack.slopty(&["close", &sized]).await?;
        ensure!(closed == json!({ "ok": true }), "{closed}");
        let closed = events_after(stack, after, 10_000).await?;
        let closed = closed["events"].as_array().context("events")?;
        ensure!(
            closed.iter().any(|e| e["kind"] == "session_closed" && e["term"] == sized.as_str()),
            "{closed:?}"
        );
        let refused = stack.slopty(&["workers", "forget", &worker]).await;
        ensure!(refused.is_err(), "an online worker stays: {refused:?}");
        clock.lap("events, resize");

        // 8b. An agent's thread: its question waits on the person, whom nothing answers for, and
        //     is answered in its terminal; an agent told at rest and stopped at work; and a still
        //     picture asked of a window no worker has, so nothing is ever captured.
        agent_reached(stack, &worker).await?;
        clock.lap("thread read, tell, stop, still");

        // 5. A closed terminal leaves the list. One whose program exited stays, exited with its
        //    status, until it is closed.
        let closed = stack.slopty(&["close", &term]).await?;
        ensure!(closed == json!({ "ok": true }), "{closed}");
        ensure!(!is_listed(stack, &term).await?, "a closed terminal is not listed");
        let quitter = open_bash(stack, "e2e exit").await?;
        ensure!(is_listed(stack, &quitter).await?, "the new terminal is listed");
        send_text(stack, &quitter, "exit 3\n").await?;
        let exited = until("the terminal listed as exited", STEP, async || {
            let entry = terminal_entry(stack, &quitter).await?;
            Ok((entry["running"] == json!(false)).then_some(entry))
        })
        .await?;
        ensure!(exited["exit_status"] == json!(3), "its status is kept: {exited}");
        let closed = stack.slopty(&["close", &quitter]).await?;
        ensure!(closed == json!({ "ok": true }), "{closed}");
        ensure!(!is_listed(stack, &quitter).await?, "closed, it leaves the list");
        clock.lap("close, exit");

        // 6. The worker dies without a goodbye: unreachable within the lease; back under the same
        //    id. The fleet's events say both.
        let cursor = cursor_now(stack).await?.to_string();
        stack.worker.kill_worker().await;
        let killed = Instant::now();
        stack.worker_is("unreachable", UNREACHABLE_BOUND).await?;
        eprintln!("unreachable {} ms after the kill", killed.elapsed().as_millis());
        clock.lap("unreachable");
        stack.worker.restart_worker().await?;
        let back = stack.worker_online(STEP).await?;
        ensure!(back["worker"] == worker_id.as_str(), "the same worker: {back}");
        let moves = stack.slopty(&["events", "--since", &cursor, "--timeout", "0"]).await?;
        let moves: Vec<&Value> = moves["events"]
            .as_array()
            .context("events")?
            .iter()
            .filter(|e| e["kind"] == "worker" && e["worker"] == worker_id.as_str())
            .map(|e| &e["liveness"])
            .collect();
        ensure!(moves == [&json!("unreachable"), &json!("online")], "{moves:?}");
        clock.lap("back online");
        Ok(())
    }

    /// Push a file past the 8 MiB one read carries, pull it back, compare; then the same
    /// through standard input and output.
    async fn bulk(stack: &ServerStack, worker: &str) -> Result<()> {
        // 9 MiB and 3 bytes: past the 8 MiB one read carries, and not a whole number of parts.
        let size: usize = 9_437_187;
        let contents: Vec<u8> = (0..size).map(|i| i.wrapping_mul(31).to_le_bytes()[1]).collect();
        let here = stack.path("big-here.bin");
        std::fs::write(&here, &contents)?;
        let there = stack.path("big-there.bin").to_string_lossy().into_owned();
        let here_arg = here.to_string_lossy().into_owned();
        let pushed = stack
            .slopty(&[
                "push",
                "--worker",
                worker,
                &here_arg,
                &there,
                "--idempotency-key",
                "e2e-push",
            ])
            .await?;
        ensure!(pushed["size"] == size && pushed["path"] == there.as_str(), "{pushed}");
        ensure!(std::fs::read(&there)? == contents, "the worker holds the same bytes");
        let back = stack.path("big-back.bin");
        let back_arg = back.to_string_lossy().into_owned();
        let pulled = stack.slopty(&["pull", "--worker", worker, &there, &back_arg]).await?;
        ensure!(pulled["size"] == size, "{pulled}");
        ensure!(std::fs::read(&back)? == contents, "pulled back whole");
        // The same through standard input and output, past one batch of parts both ways.
        let piped = stack.path("big-piped.bin").to_string_lossy().into_owned();
        let push = ["push", "--worker", worker, "-", &piped];
        let pushed = stack.slopty_with_stdin(&push, &contents).await?;
        ensure!(pushed["size"] == size, "pushed from standard input: {pushed}");
        ensure!(std::fs::read(&piped)? == contents, "the worker holds what was piped");
        let printed = stack.slopty_bytes(&["pull", "--worker", worker, &piped, "-"], b"").await?;
        ensure!(printed == contents, "pulled to standard output whole");
        let leftovers: Vec<String> = std::fs::read_dir(stack.dir.path())?
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|name| name.ends_with(".slopty-upload") || name.ends_with(".slopty-download"))
            .collect();
        ensure!(leftovers.is_empty(), "no parts left behind: {leftovers:?}");
        Ok(())
    }

    /// A Claude Code thread (the stand-in) that asks the person a question: its read says it
    /// needs them and what it asks. Nothing answers for the person: MCP offers an agent no tool
    /// to, and a tell or a stop (an Esc would answer the dialog) is refused, naming what it waits
    /// on. The person answers in its terminal and the agent hears it; at rest it is told again,
    /// and stopped while that turn runs. Then a still picture of a window no worker has, so
    /// nothing is captured (`docs/decisions/agents.md`, "TUI-first").
    async fn agent_reached(stack: &ServerStack, worker: &str) -> Result<()> {
        let folder = stack.path("agent-folder");
        std::fs::create_dir_all(&folder)?;
        let folder = folder.to_string_lossy().into_owned();
        // The stand-in asks what follows `ask` with `AskUserQuestion` and waits on the person.
        let question = "Which colour for the build?";
        let start = ["agent", "start", worker, "--folder", &folder, "ask", question];
        let started = stack.slopty(&start).await?;
        let thread = str_of(&started, "agent")?.to_owned();
        let reading = ["agent", "read", "--thread", thread.as_str()];
        let mut last = String::new();
        let asked = until("the question waits on the person", STEP, async || {
            let read = match stack.slopty(&reading).await {
                Ok(read) => read,
                Err(e) => {
                    last = format!("{e:#}");
                    return Ok(None);
                }
            };
            last = read.to_string();
            let waits = read["phase"] == "needs_you"
                && read["wait"].as_str().is_some_and(|w| w.contains(question));
            Ok(waits.then_some(read))
        })
        .await
        .with_context(|| format!("the last read: {last}"))?;
        let wait = str_of(&asked, "wait")?.to_owned();

        // Only the person answers: MCP offers an agent no tool to answer with, and the CLI
        // types nothing at the dialog: a tell is refused naming what it waits on, and a stop
        // leaves it to the person, as no agent at work.
        let params = json!({
            "name": "answer_request",
            "arguments": { "thread": thread, "choice": "Red" },
        });
        let by_agent = stack.mcp(40, "tools/call", params).await;
        let refused = match &by_agent {
            Err(e) => e.to_string().contains("no tool is called answer_request"),
            Ok(said) => said["error"].is_object() || said["result"]["isError"] == json!(true),
        };
        ensure!(refused, "an agent cannot answer a request: {by_agent:?}");
        let names_the_wait = |answer: &Result<Value>| {
            answer.as_ref().err().is_some_and(|e| format!("{e:#}").contains(wait.as_str()))
        };
        let told = stack.slopty(&["agent", "tell", &thread, "Red"]).await;
        ensure!(names_the_wait(&told), "a tell at the question is refused, naming it: {told:?}");
        let stopped = stack.slopty(&["agent", "stop", &thread]).await;
        let conflict =
            stopped.as_ref().err().is_some_and(|e| format!("{e:#}").contains("(Conflict)"));
        ensure!(conflict, "an Esc would answer it, so a stop is refused: {stopped:?}");
        let still = stack.slopty(&reading).await?;
        ensure!(
            still["phase"] == "needs_you" && still["wait"] == json!(wait),
            "the question still waits: {still}"
        );

        // The person answers in its terminal, and the agent hears it and comes to rest.
        let term = until("the agent's terminal listed", STEP, async || {
            let terminals = stack.slopty(&["terminals"]).await?;
            let all = terminals.as_array().context("terminals is a list")?;
            let found = all.iter().find(|t| t["agent"]["thread"] == thread.as_str());
            Ok(found.and_then(|t| t["term"].as_str()).map(str::to_owned))
        })
        .await?;
        let answer = "Teal";
        send_text(stack, &term, answer).await?;
        let enter = stack.slopty(&["send", &term, "--keys", "enter"]).await?;
        ensure!(enter == json!({ "ok": true }), "{enter}");
        let mut last = String::new();
        let heard = until("the agent hears the answer and rests", STEP, async || {
            let read = stack.slopty(&reading).await?;
            last = read.to_string();
            let said = read["turns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["entries"].as_array())
                .flatten()
                .any(|e| {
                    e["type"] == "agent" && e["text"].as_str().is_some_and(|t| t.contains(answer))
                });
            Ok((read["phase"] == "done" && said).then_some(read))
        })
        .await
        .with_context(|| format!("the last read: {last}"))?;
        ensure!(heard.get("wait").is_none(), "nothing waits on the person: {heard}");

        // At rest it is told, and its turn under way is stopped, as Esc stops it in its terminal.
        let told = stack.slopty(&["agent", "tell", &thread, "hold", "the", "line"]).await?;
        ensure!(told == json!({ "ok": true }), "{told}");
        // The server stops what its own board holds at work.
        until("the told turn runs", STEP, async || {
            let listed = stack.slopty(&["agents"]).await?;
            let all = listed["agents"].as_array().context("agents")?;
            let working = all.iter().any(|a| a["id"] == thread.as_str() && a["phase"] == "working");
            Ok(working.then_some(()))
        })
        .await?;
        let stopped = stack.slopty(&["agent", "stop", &thread]).await?;
        ensure!(stopped == json!({ "ok": true }), "{stopped}");
        let mut last = String::new();
        until("the turn ends interrupted", STEP, async || {
            let read = stack.slopty(&reading).await?;
            last = read.to_string();
            let turns = read["turns"].as_array().context("turns")?;
            let ended = turns.last().is_some_and(|t| t["state"] == "interrupted");
            Ok(ended.then_some(()))
        })
        .await
        .with_context(|| format!("the last read: {last}"))?;

        // A window no worker has: whether or not this one may record its screen, it answers
        // without a picture.
        let cli = stack
            .slopty(&[
                "capture",
                "--worker",
                worker,
                "--window",
                "4294967295",
                "--out",
                "/dev/null",
            ])
            .await;
        ensure!(cli.is_err(), "no picture is taken: {cli:?}");
        Ok(())
    }
}
