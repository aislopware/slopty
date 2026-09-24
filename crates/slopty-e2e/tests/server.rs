//! The server with a real worker behind it, driven the way people and agents drive it: the
//! `slopty` binary with `--json`, and MCP over HTTP. `slopty-server`, `slopty-ptyd` and
//! `slopty-hostd` from this build run in a temporary directory on ports of their own.
//!
//! Runs only with `SLOPTY_SERVER_E2E=1` (`cargo xtask e2e server`). It needs no permission from
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

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_SERVER_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_SERVER_E2E=1 (or run `cargo xtask e2e server`)");
            return false;
        }
        true
    }

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
        let cwd = stack.dir.path().to_string_lossy().into_owned();
        let worker = stack.worker.name();
        let args = ["open", "--worker", worker, "--cwd", &cwd, "--name", name, "--"];
        let bash = ["/bin/bash", "--noprofile", "--norc", "-i"];
        let opened = stack.slopty(&[&args[..], &bash[..]].concat()).await?;
        Ok(str_of(&opened, "term")?.to_owned())
    }

    async fn is_listed(stack: &ServerStack, term: &str) -> Result<bool> {
        let terminals = stack.slopty(&["terminals"]).await?;
        let all = terminals.as_array().context("terminals is a list")?;
        Ok(all.iter().any(|t| t["term"] == term))
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
    async fn the_cli_and_mcp_drive_a_real_worker_through_the_server() {
        if !gated() {
            return;
        }
        let mut clock = Clock::new();
        let mut stack = ServerStack::launch("e2e-worker").await.unwrap();
        clock.lap("launch");
        let outcome = scenario(&mut stack, &mut clock).await;
        stack.shutdown().await;
        clock.report();
        outcome.unwrap();
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

        // 3. Files both ways, text and binary.
        let worker = stack.worker.name().to_owned();
        let text_path = stack.path("note.txt").to_string_lossy().into_owned();
        let put = stack
            .slopty_with_stdin(&["put", "--worker", &worker, &text_path], b"hello, worker\n")
            .await?;
        ensure!(put == json!({ "ok": true }), "{put}");
        ensure!(std::fs::read(&text_path)? == b"hello, worker\n", "put wrote the file");
        let cat = stack.slopty(&["cat", "--worker", &worker, &text_path]).await?;
        ensure!(cat["encoding"] == "utf8" && cat["content"] == "hello, worker\n", "{cat}");
        let binary: Vec<u8> = (0..=255_u8).rev().chain(0..=255).collect();
        let bin_path = stack.path("blob.bin").to_string_lossy().into_owned();
        stack.slopty_with_stdin(&["put", "--worker", &worker, &bin_path], &binary).await?;
        ensure!(std::fs::read(&bin_path)? == binary, "put wrote the bytes as they are");
        let cat = stack.slopty(&["cat", "--worker", &worker, &bin_path]).await?;
        ensure!(cat["encoding"] == "base64" && cat["size"] == binary.len(), "{cat}");
        let decoded = data_encoding::BASE64.decode(str_of(&cat, "content")?.as_bytes())?;
        ensure!(decoded == binary, "cat read the bytes back");
        clock.lap("put, cat");

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

        // 7. MCP over HTTP: the tools, and the screen of a terminal as a tool reads it.
        let listed = stack.mcp(1, "tools/list", json!({})).await?;
        let tools = listed["result"]["tools"].as_array().context("tools")?;
        ensure!(tools.iter().any(|t| t["name"] == "read_screen"), "{listed}");
        let shown = until("the marker on the screen MCP reads", STEP, async || {
            let params = json!({ "name": "read_screen", "arguments": { "term": term } });
            let called = stack.mcp(2, "tools/call", params).await?;
            let result = &called["result"];
            ensure!(result["isError"] != json!(true), "{called}");
            let text = str_of(&result["content"][0], "text")?;
            Ok(text.contains(&marker).then(|| text.to_owned()))
        })
        .await?;
        ensure!(shown.contains("nc -l"), "the screen as it is now: {shown}");
        clock.lap("mcp");

        // 5. Closed, and exited on its own: both leave the list.
        let closed = stack.slopty(&["close", &term]).await?;
        ensure!(closed == json!({ "ok": true }), "{closed}");
        ensure!(!is_listed(stack, &term).await?, "a closed terminal is not listed");
        let quitter = open_bash(stack, "e2e exit").await?;
        ensure!(is_listed(stack, &quitter).await?, "the new terminal is listed");
        send_text(stack, &quitter, "exit\n").await?;
        until("the exited terminal leaving the list", STEP, async || {
            Ok((!is_listed(stack, &quitter).await?).then_some(()))
        })
        .await?;
        clock.lap("close, exit");

        // 6. hostd dies without a goodbye: unreachable within the lease; back under the same id.
        stack.worker.kill_hostd().await;
        let killed = Instant::now();
        stack.worker_is("unreachable", UNREACHABLE_BOUND).await?;
        eprintln!("unreachable {} ms after the kill", killed.elapsed().as_millis());
        clock.lap("unreachable");
        stack.worker.restart_hostd().await?;
        let back = stack.worker_online(STEP).await?;
        ensure!(back["worker"] == worker_id.as_str(), "the same worker: {back}");
        clock.lap("back online");
        Ok(())
    }
}
