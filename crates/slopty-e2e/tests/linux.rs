//! A terminal-only Linux worker, reached from this Mac the way the app reaches any worker: the
//! client core's link over QUIC, a `TermState` fed its frames, typing through the terminal's
//! input requests. The worker, its ptyd and the `slopty` relay are the Linux builds, running in
//! a Debian container on Docker Desktop as an account of their own.
//!
//! Live (`#[ignore]`), run by `cargo xtask linux e2e`, which builds the Linux
//! binaries, starts the container and says where it is (`SLOPTY_LINUX_WORKER`, the address
//! published on this Mac's loopback; `SLOPTY_LINUX_CONTAINER`; `SLOPTY_LINUX_USER`;
//! `SLOPTY_LINUX_BIN_DIR`, where the binaries are inside it). Nothing is typed into a shell the
//! test did not open, and the agent status comes from a hook played through `slopty hook`.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, bail};
    use slopty_client::{Effect, LinkEvent, TermState, WorkerLink};
    use slopty_core::{ClientId, SessionId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_proto::agent::AgentStatus;
    use slopty_proto::file::FileRead;
    use slopty_proto::folder::Listing;
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::server::Os;
    use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
    use slopty_proto::{ClientMsg, WorkerMsg};
    use tokio::io::AsyncWriteExt as _;
    use tokio::sync::mpsc;

    /// How long a shell start, a command or a reply may take.
    const STEP: Duration = Duration::from_secs(20);
    /// Keys timed for the echo round trip.
    const KEYS: usize = 300;
    /// Longest wait for one key's echo before the run fails.
    const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
    /// Keys on one line of `cat` before a return starts the next.
    const LINE: u16 = 60;

    /// Where the Linux worker is, as the xtask started it.
    struct Linux {
        addr: SocketAddr,
        container: String,
        user: String,
        bin_dir: String,
    }

    fn linux() -> Linux {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} unset"));
        Linux {
            addr: var("SLOPTY_LINUX_WORKER").parse().expect("SLOPTY_LINUX_WORKER is ip:port"),
            container: var("SLOPTY_LINUX_CONTAINER"),
            user: var("SLOPTY_LINUX_USER"),
            bin_dir: var("SLOPTY_LINUX_BIN_DIR"),
        }
    }

    /// One client's link to the worker with one terminal on it, its screen kept as the app
    /// keeps it.
    struct Client {
        _endpoint: slopty_net::Endpoint,
        link: WorkerLink,
        events: mpsc::Receiver<LinkEvent>,
        session: SessionId,
        state: TermState,
    }

    impl Client {
        /// Connect, then open `command` (the account's login shell when empty) in `~`.
        async fn open(linux: &Linux, command: &[&str], size: TermSize) -> Result<(Self, HelloAck)> {
            let endpoint = bind_client()?;
            let hello = Hello { client: ClientId::new(), name: "linux e2e".to_owned() };
            let conn = tokio::time::timeout(STEP, connect_addr(&endpoint, linux.addr, hello))
                .await
                .context("connect")??;
            let ack = conn.ack.clone();
            let mut link = WorkerLink::start(conn);
            let mut events = link.events().context("the link's events")?;
            link.send(ClientMsg::OpenSession {
                request: 1,
                spec: OpenSession {
                    size,
                    cwd: Some("~".to_owned()),
                    command: command.iter().map(|&a| a.to_owned()).collect(),
                    env: Vec::new(),
                    title: Some("linux e2e".to_owned()),
                    attach: true,
                },
            })
            .await?;
            let session = tokio::time::timeout(STEP, async {
                loop {
                    match events.recv().await {
                        Some(LinkEvent::Control(WorkerMsg::SessionOpened { summary, .. })) => {
                            break Ok(summary.id);
                        }
                        Some(LinkEvent::Control(WorkerMsg::Term {
                            event: TermEvent::Error(e),
                            ..
                        })) => break Err(anyhow::anyhow!("open: {e}")),
                        Some(LinkEvent::Disconnected(why)) => bail!("disconnected: {why}"),
                        Some(_other) => {}
                        None => bail!("the link closed"),
                    }
                }
            })
            .await
            .context("SessionOpened")??;
            let state = TermState::new(size);
            let client = Self { _endpoint: endpoint, link, events, session, state };
            Ok((client, ack))
        }

        async fn send(&self, req: TermRequest) -> Result<()> {
            Ok(self.link.send(ClientMsg::Term { session: self.session, req }).await?)
        }

        /// Type `text` as the terminal's text input does, then press return.
        async fn type_line(&self, text: &str) -> Result<()> {
            self.send(TermRequest::Raw(text.as_bytes().to_vec())).await?;
            self.send(TermRequest::Key(key(0, KeyCode::Enter, None))).await
        }

        /// The next event, applied: a frame to the screen, and what the screen asks of the
        /// worker sent to it. A control message is handed back.
        async fn step(&mut self, deadline: Instant) -> Result<Option<WorkerMsg>> {
            let event = tokio::time::timeout_at(deadline.into(), self.events.recv())
                .await
                .context("timed out")?
                .context("the link closed")?;
            match event {
                LinkEvent::Term { session, event } if session == self.session => {
                    for effect in self.state.apply(event) {
                        if let Effect::Request(req) = effect {
                            self.send(req).await?;
                        }
                    }
                    Ok(None)
                }
                LinkEvent::Control(msg) => Ok(Some(msg)),
                LinkEvent::Disconnected(why) => bail!("disconnected: {why}"),
                _other => Ok(None),
            }
        }

        /// The visible rows as text, trailing blanks trimmed.
        fn rows(&self) -> Vec<String> {
            self.state
                .view()
                .iter()
                .map(|row| row.line.map(|l| l.text().trim_end().to_owned()).unwrap_or_default())
                .collect()
        }

        /// Apply events until a row reads `row` exactly.
        async fn until_row(&mut self, row: &str) -> Result<()> {
            let deadline = after(STEP);
            while !self.rows().iter().any(|r| r == row) {
                self.step(deadline)
                    .await
                    .with_context(|| format!("{row:?} on screen: {:#?}", self.rows()))?;
            }
            Ok(())
        }

        /// Apply events until a control message `pick` takes comes.
        async fn until_control<T>(
            &mut self,
            what: &str,
            mut pick: impl FnMut(WorkerMsg) -> Option<T>,
        ) -> Result<T> {
            let deadline = after(STEP);
            loop {
                let msg = self.step(deadline).await.with_context(|| what.to_owned())?;
                if let Some(found) = msg.and_then(&mut pick) {
                    return Ok(found);
                }
            }
        }

        async fn close(mut self) {
            let _closed = self.send(TermRequest::Close).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.link.close();
        }
    }

    /// `wait` from now.
    fn after(wait: Duration) -> Instant {
        Instant::now().checked_add(wait).expect("a deadline")
    }

    /// A key press as the app's keyboard sends it.
    fn key(seq: u64, code: KeyCode, text: Option<char>) -> KeyEvent {
        KeyEvent {
            seq,
            action: KeyAction::Press,
            code,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            text: text.map(String::from),
            unshifted: text,
            composing: false,
            option_as_alt: false,
        }
    }

    /// `docker exec` into the worker's container as its account.
    fn exec(linux: &Linux, env: &[(&str, &str)], args: &[&str]) -> tokio::process::Command {
        let mut exec = tokio::process::Command::new("docker");
        exec.args(["exec", "--interactive", "--user", &linux.user]);
        for (name, value) in env {
            exec.arg("--env").arg(format!("{name}={value}"));
        }
        exec.arg(&linux.container).args(args).kill_on_drop(true);
        exec
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live: cargo xtask linux e2e"]
    async fn a_linux_worker_runs_a_shell_serves_files_and_reports_an_agent() {
        let linux = linux();
        let size = TermSize { cols: 80, rows: 24, ..TermSize::default() };
        let (mut shell, ack) = Client::open(&linux, &[], size).await.unwrap();

        // 1. The greeting says Linux, and a desktop it does not have.
        let caps = &ack.caps;
        assert_eq!(caps.os, Os::Linux, "{caps:?}");
        assert_eq!(caps.arch, "aarch64", "{caps:?}");
        assert!(caps.cpus > 0 && caps.memory > 0, "{caps:?}");
        assert!(caps.os_version.starts_with("debian "), "the distribution: {caps:?}");
        assert!(!caps.can_capture && !caps.can_inject, "no capture, no input: {caps:?}");
        assert!(caps.encoders.is_empty() && caps.displays.is_empty(), "{caps:?}");
        let home = format!("/home/{}", linux.user);
        assert_eq!(ack.home, home);

        // 2. The account's shell, from its passwd entry (the container sets no `SHELL`), echoes
        //    what is typed and runs it. Only the output matches: the typed line holds `$((`.
        shell.type_line("echo linux-$((40+2)) $(uname -s) ${BASH##*/} $PWD").await.unwrap();
        shell.until_row(&format!("linux-42 Linux bash {home}")).await.unwrap();

        // 3. Files: a folder made in the shell, listed and read through the worker.
        shell
            .type_line(
                "mkdir -p e2e/inner && printf 'from linux\\n' > e2e/note.txt && echo made-$((1+1))",
            )
            .await
            .unwrap();
        shell.until_row("made-2").await.unwrap();
        shell.link.send(ClientMsg::ListFolder { path: "~/e2e".to_owned() }).await.unwrap();
        let listing = shell
            .until_control("the folder", |msg| match msg {
                WorkerMsg::Folder { listing, .. } => Some(listing),
                _other => None,
            })
            .await
            .unwrap();
        let Listing::Listed { dir, entries, total } = listing else {
            panic!("a directory: {listing:?}")
        };
        assert_eq!(dir, format!("{home}/e2e"));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            (names.as_slice(), total),
            (["inner", "note.txt"].as_slice(), 2),
            "folders first"
        );
        let note = format!("{home}/e2e/note.txt");
        shell.link.send(ClientMsg::ReadFile { path: note.clone() }).await.unwrap();
        let read = shell
            .until_control("the file", |msg| match msg {
                WorkerMsg::File { path, read } if path == note => Some(read),
                _other => None,
            })
            .await
            .unwrap();
        assert!(matches!(&read, FileRead::Text { text, .. } if text == "from linux"), "{read:?}");

        // 4. An agent's status: a hook played through the Linux `slopty hook` inside the container,
        //    finding the worker by the platform's socket rule, reaches this client.
        let session = shell.session.to_string();
        let relay = format!("{}/slopty", linux.bin_dir);
        let mut hook = exec(&linux, &[("SLOPTY_SESSION", &session)], &[&relay, "hook"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let payload = r#"{"hook_event_name":"UserPromptSubmit","session_id":"linux-e2e","prompt":"tidy the linux box"}"#;
        let mut stdin = hook.stdin.take().unwrap();
        stdin.write_all(payload.as_bytes()).await.unwrap();
        drop(stdin);
        let status = tokio::time::timeout(STEP, hook.wait()).await.unwrap().unwrap();
        assert!(status.success(), "slopty hook: {status}");
        let id = shell.session;
        let agent = shell
            .until_control("the agent's status", |msg| match msg {
                WorkerMsg::Agent(event) if event.session == id => Some(event),
                _other => None,
            })
            .await
            .unwrap();
        assert_eq!(agent.status, AgentStatus::Working, "{agent:?}");
        assert_eq!(agent.detail.as_deref(), Some("tidy the linux box"), "{agent:?}");

        shell.close().await;
    }

    /// Each key's round trip, from the press leaving this client to the frame that shows its
    /// echo, on `cat` (the terminal's line discipline echoes, nothing else draws). Printed as
    /// percentiles; fails only when a key does not come back.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live: cargo xtask linux e2e"]
    async fn keystroke_echo_round_trip_on_linux() {
        let linux = linux();
        let size = TermSize { cols: 80, rows: 24, ..TermSize::default() };
        let (mut cat, _ack) = Client::open(&linux, &["/bin/cat"], size).await.unwrap();
        // Settle the attach before timing anything.
        let settle = after(Duration::from_millis(500));
        while cat.step(settle).await.is_ok() {}

        let mut samples = Vec::with_capacity(KEYS);
        let mut seq = 0_u64;
        for i in 0..KEYS {
            let col = cat.state.cursor().col;
            if col >= LINE {
                seq = seq.wrapping_add(1);
                cat.send(TermRequest::Key(key(seq, KeyCode::Enter, None))).await.unwrap();
                let deadline = after(STEP);
                while cat.state.cursor().col != 0 {
                    cat.step(deadline).await.unwrap();
                }
                // `cat` writes the line back after the echo of the return; let it land.
                let quiet = after(Duration::from_millis(50));
                while cat.step(quiet).await.is_ok() {}
            }
            let before = cat.state.cursor().col;
            let (code, text) =
                if i.is_multiple_of(2) { (KeyCode::X, 'x') } else { (KeyCode::Y, 'y') };
            seq = seq.wrapping_add(1);
            let sent = Instant::now();
            cat.send(TermRequest::Key(key(seq, code, Some(text)))).await.unwrap();
            let deadline = sent.checked_add(ECHO_TIMEOUT).expect("a deadline");
            while cat.state.cursor().col == before {
                cat.step(deadline)
                    .await
                    .unwrap_or_else(|e| panic!("key {i} not echoed within {ECHO_TIMEOUT:?}: {e}"));
            }
            samples.push(sent.elapsed());
        }
        let rtt = cat.link.rtt();
        cat.close().await;

        samples.sort_unstable();
        let at = |q: f64| {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index below a few hundred"
            )]
            let i = ((samples.len().saturating_sub(1)) as f64 * q).round() as usize;
            samples[i].as_secs_f64() * 1e3
        };
        eprintln!(
            "linux echo, {KEYS} keys to /bin/cat: min {:.2} ms  p50 {:.2} ms  p90 {:.2} ms  p99 {:.2} ms  max {:.2} ms  (quic rtt {rtt:?})",
            at(0.0),
            at(0.5),
            at(0.9),
            at(0.99),
            at(1.0)
        );
        assert_eq!(samples.len(), KEYS);
    }
}
