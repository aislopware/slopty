//! The server with a real worker behind it, driven the way people and agents drive it: the
//! `slopty` binary with `--json`, and a project's tools over MCP's HTTP. `slopty-server`,
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
        ensure!(!str_of(&entry, "arch")?.is_empty() && !str_of(&entry, "version")?.is_empty());
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

        // 7. A project's tools over MCP's HTTP: the eight listed, and a call the hub answers. The
        //    screen of a terminal, as an agent reads it with `slopty screen`.
        let listed = stack.mcp(1, "tools/list", json!({})).await?;
        let tools = listed["result"]["tools"].as_array().context("tools")?;
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        ensure!(names.len() == 8 && names.contains(&"task_start"), "{listed}");
        let params = json!({ "name": "project_status", "arguments": { "project": "nope" } });
        let called = stack.mcp(2, "tools/call", params).await?;
        let said = str_of(&called["result"]["content"][0], "text")?;
        ensure!(called["result"]["isError"] == json!(true), "{called}");
        ensure!(said.contains("(UnknownProject)"), "the hub's own words: {said}");
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

        // 8b. Another agent, played through the real hook relay: its thread read, its
        //     permission prompt held for orchestration and answered over the CLI; and a still
        //     picture refused, or asked of a window no worker has, so nothing is ever captured.
        agent_reached(stack, &worker).await?;
        clock.lap("thread read, answer, still");

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

    /// `slopty hook` as Claude Code runs it for `session`, `payload` on its stdin, returned
    /// running: a `PermissionRequest` relay waits for its answer.
    fn relay(stack: &ServerStack, session: &str, payload: &Value) -> Result<tokio::process::Child> {
        use tokio::io::AsyncWriteExt as _;
        let slopty = slopty_e2e::harness::bin_dir()?.join("slopty");
        let mut child = tokio::process::Command::new(slopty);
        slopty_testkit::env::scrub(child.as_std_mut(), &stack.path("hook-data").join("home"));
        let mut child = child
            .arg("--data-dir")
            .arg(stack.path("hook-data"))
            .arg("hook")
            .env("SLOPTY_SESSION", session)
            .env("SLOPTY_WORKER_SOCKET", stack.worker.ctl_socket())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawn slopty hook")?;
        let mut stdin = child.stdin.take().context("the hook's stdin")?;
        let bytes = payload.to_string().into_bytes();
        tokio::spawn(async move {
            let _written = stdin.write_all(&bytes).await;
        });
        Ok(child)
    }

    /// A played agent in a quiet shell: its thread read over the CLI, which makes orchestration
    /// follow it; a permission prompt the relay asks is held, shown as the thread's request on
    /// the next read, refused when an agent answers it over MCP, answered by the person over
    /// the CLI under a key (and again, answering the first), and the relay prints the denial.
    /// Then a still picture of a window no worker has, so nothing is captured.
    async fn agent_reached(stack: &ServerStack, worker: &str) -> Result<()> {
        let term = open_bash(stack, "e2e agent").await?;
        let session = term.rsplit('/').next().context("a session in the term")?.to_owned();
        // Named for its session, as Claude Code names a transcript.
        let transcript = stack.path("e2e.jsonl");
        let records = [
            json!({
                "type": "user", "uuid": "u1", "parentUuid": null,
                "timestamp": "2026-09-28T03:15:25.849Z",
                "message": { "role": "user", "content": "tidy the build folder" },
            }),
            json!({
                "type": "user", "uuid": "u2", "parentUuid": "u1",
                "timestamp": "2026-09-28T03:15:27.000Z",
                "message": { "role": "user", "content": "and keep the logs" },
            }),
        ];
        let mut jsonl = String::new();
        for record in &records {
            jsonl.push_str(&record.to_string());
            jsonl.push('\n');
        }
        std::fs::write(&transcript, jsonl)?;
        let hook = |event: &str| {
            json!({
                "hook_event_name": event, "session_id": "e2e",
                "transcript_path": transcript.to_string_lossy(),
            })
        };
        let mut prompted = relay(stack, &session, &hook("UserPromptSubmit"))?;
        let status = tokio::time::timeout(STEP, prompted.wait()).await??;
        ensure!(status.success(), "the relay exits 0: {status}");

        // Read as the person, over the CLI: from now on its prompts wait for an answer here.
        let read = until("the prompts in the agent's thread", STEP, async || {
            // Until the worker's table shows the thread, the server finds none in the terminal.
            let Ok(read) = stack.slopty(&["agent", "read", "--term", &term]).await else {
                return Ok(None);
            };
            Ok(read.to_string().contains("tidy the build folder").then_some(read))
        })
        .await?;
        ensure!(read["agent"] == "claude-code" && read["requests"] == json!([]), "{read}");
        let thread = str_of(&read, "thread")?.to_owned();

        let mut asking = hook("PermissionRequest");
        asking["tool_name"] = json!("Bash");
        asking["tool_input"] = json!({ "command": "rm -rf build" });
        asking["permission_mode"] = json!("default");
        let asked = relay(stack, &session, &asking)?;
        let request = until("the prompt held as the thread's request", STEP, async || {
            let read = stack.slopty(&["agent", "read", "--thread", &thread]).await?;
            Ok(read["requests"].as_array().and_then(|r| r.first()).cloned())
        })
        .await?;
        let ask = str_of(&request, "ask")?.to_owned();
        let choices = request["choices"].to_string();
        ensure!(choices.contains("\"deny\""), "{request}");
        // Only the person answers: MCP offers an agent no tool to answer with, and the prompt
        // waits.
        let params = json!({
            "name": "answer_request",
            "arguments": { "thread": thread, "ask": ask, "choice": "allow" },
        });
        let by_agent = stack.mcp(40, "tools/call", params).await;
        let refused = match &by_agent {
            Err(e) => e.to_string().contains("no tool is called answer_request"),
            Ok(said) => said["error"].is_object() || said["result"]["isError"] == json!(true),
        };
        ensure!(refused, "an agent cannot answer a request: {by_agent:?}");
        let still = stack.slopty(&["agent", "read", "--term", &term]).await?;
        ensure!(still["requests"][0]["ask"] == json!(ask), "the prompt still waits: {still}");
        let answer = [
            "--idempotency-key",
            "e2e-answer",
            "agent",
            "answer",
            "--term",
            term.as_str(),
            ask.as_str(),
            "deny",
            "--message",
            "not the build folder",
        ];
        let done = stack.slopty(&answer).await?;
        ensure!(done == json!({ "ok": true }), "{done}");
        let out = tokio::time::timeout(STEP, asked.wait_with_output()).await??;
        let printed: Value = serde_json::from_slice(&out.stdout).with_context(|| {
            format!("the relay printed {:?}", String::from_utf8_lossy(&out.stdout))
        })?;
        let decision = &printed["hookSpecificOutput"]["decision"];
        ensure!(decision["behavior"] == "deny", "{printed}");
        ensure!(decision["message"] == "not the build folder", "{printed}");
        let again = stack.slopty(&answer).await?;
        ensure!(again == json!({ "ok": true }), "the same key answers the first: {again}");
        let late = stack.slopty(&["agent", "answer", "--term", &term, &ask, "allow"]).await;
        ensure!(late.is_err(), "a second answer finds nothing: {late:?}");

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
        let closed = stack.slopty(&["close", &term]).await?;
        ensure!(closed == json!({ "ok": true }), "{closed}");
        Ok(())
    }
}
