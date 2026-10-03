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
//! Files the worker is to notice change through `docker exec` as its account, so inotify hears
//! another process write them, as it would an editor.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, bail};
    use slopty_client::tunnel::Forward;
    use slopty_client::{Effect, LinkEvent, TermState, WorkerLink};
    use slopty_core::{ClientId, SessionId, XferId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_proto::agent::AgentStatus;
    use slopty_proto::file::FileRead;
    use slopty_proto::folder::{FsOp, FsOutcome, Listing};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::search::{SearchEvent, SearchQuery, SearchRequest};
    use slopty_proto::server::Os;
    use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
    use slopty_proto::transfer::{
        ClipEntry, ClipFormat, ClipMsg, ClipType, Dest, Offer, Peer, Rep, XferMsg,
    };
    use slopty_proto::{ClientMsg, WorkerMsg};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
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
        /// The session's listening ports as the link last forwarded them here.
        forwards: Vec<Forward>,
    }

    impl Client {
        /// Connect, then open `command` (the account's login shell when empty) in `~`.
        async fn open(linux: &Linux, command: &[&str], size: TermSize) -> Result<(Self, HelloAck)> {
            Self::open_on(linux, command, size, WorkerLink::start).await
        }

        /// [`Self::open`] on a link `start` makes of the connection: the app's own forwards
        /// every port the worker's shells listen on.
        async fn open_on(
            linux: &Linux,
            command: &[&str],
            size: TermSize,
            start: fn(slopty_net::client::WorkerConn) -> WorkerLink,
        ) -> Result<(Self, HelloAck)> {
            let endpoint = bind_client()?;
            let hello = Hello { client: ClientId::new(), name: "linux e2e".to_owned() };
            let conn = tokio::time::timeout(STEP, connect_addr(&endpoint, linux.addr, hello))
                .await
                .context("connect")??;
            let ack = conn.ack.clone();
            let mut link = start(conn);
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
            let client =
                Self { _endpoint: endpoint, link, events, session, state, forwards: Vec::new() };
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
                LinkEvent::Ports { session, forwards } if session == self.session => {
                    self.forwards = forwards;
                    Ok(None)
                }
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

    /// `script` run by `sh` in the worker's container as its account, to its end.
    async fn sh(linux: &Linux, script: &str) {
        let status = exec(linux, &[], &["sh", "-c", script]).stdin(Stdio::null()).status().await;
        let status = status.unwrap();
        assert!(status.success(), "`{script}`: {status}");
    }

    /// What `script` printed, run as [`sh`] runs it.
    async fn sh_out(linux: &Linux, script: &str) -> String {
        let out = exec(linux, &[], &["sh", "-c", script]).stdin(Stdio::null()).output().await;
        let out = out.unwrap();
        assert!(out.status.success(), "`{script}`: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// The next `WorkerMsg::File` for `path`.
    async fn file(client: &mut Client, path: &str) -> FileRead {
        client
            .until_control("the file", |msg| match msg {
                WorkerMsg::File { path: at, read } if at == path => Some(read),
                _other => None,
            })
            .await
            .unwrap()
    }

    /// The Linux worker follows a file tile's file and a folder tile's folder on inotify: a
    /// write by another process there, a new entry and a removal each reach the client unasked.
    /// A text search over a tree streams its matches, with what `.gitignore` leaves out left
    /// out.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live: cargo xtask linux e2e"]
    async fn a_linux_worker_follows_files_on_inotify_and_searches_them() {
        let linux = linux();
        let size = TermSize { cols: 80, rows: 24, ..TermSize::default() };
        let (mut client, ack) = Client::open(&linux, &[], size).await.unwrap();
        let dir = format!("{}/watched", ack.home);
        let note = format!("{dir}/note.txt");
        sh(&linux, &format!("mkdir -p {dir} && printf one > {note}")).await;

        // 1. The file, read, then followed; the folder followed beside it.
        client.link.send(ClientMsg::ReadFile { path: note.clone() }).await.unwrap();
        let read = file(&mut client, &note).await;
        assert!(matches!(&read, FileRead::Text { text, .. } if text == "one"), "{read:?}");
        client.link.send(ClientMsg::WatchFiles { paths: vec![note.clone()] }).await.unwrap();
        client.link.send(ClientMsg::WatchFolders { paths: vec![dir.clone()] }).await.unwrap();
        // The watch stamps the file as it is; the write must come after that.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // 2. Another process writes it: read again, unasked.
        let wrote = Instant::now();
        sh(&linux, &format!("printf two > {note}")).await;
        let read = file(&mut client, &note).await;
        let heard = wrote.elapsed();
        assert!(matches!(&read, FileRead::Text { text, .. } if text == "two"), "{read:?}");
        eprintln!("linux inotify: a write reached the client {heard:?} after docker exec began");

        // 3. A new entry in the folder: listed again with it.
        sh(&linux, &format!("touch {dir}/added.txt")).await;
        let listing = client
            .until_control("the folder listed again", |msg| match msg {
                WorkerMsg::Folder { listing: Listing::Listed { entries, .. }, .. }
                    if entries.iter().any(|e| e.name == "added.txt") =>
                {
                    Some(entries.len())
                }
                _other => None,
            })
            .await
            .unwrap();
        assert_eq!(listing, 2, "note.txt and added.txt");

        // 4. Removed: said to be missing.
        sh(&linux, &format!("rm {note}")).await;
        let read = file(&mut client, &note).await;
        assert!(matches!(read, FileRead::Missing { .. }), "{read:?}");

        // 5. A text search over a tree, its ignored build left out.
        let tree = format!("{}/tree", ack.home);
        sh(
            &linux,
            &format!(
                "mkdir -p {tree}/src {tree}/build && printf 'build\\n' > {tree}/.gitignore \
                 && printf '//! Docs.\\npub fn needle() {{}}\\n' > {tree}/src/lib.rs \
                 && printf 'needle\\n' > {tree}/build/out.rs"
            ),
        )
        .await;
        let query = SearchQuery { pattern: "needle".to_owned(), ..SearchQuery::default() };
        let start = SearchRequest::Start { id: 1, root: "~/tree".to_owned(), query };
        client.link.send(ClientMsg::Search(start)).await.unwrap();
        let mut found = Vec::new();
        let summary = loop {
            let event = client
                .until_control("the search", |msg| match msg {
                    WorkerMsg::Search(event) => Some(event),
                    _other => None,
                })
                .await
                .unwrap();
            match event {
                SearchEvent::Hits { id: 1, files } => found.extend(files),
                SearchEvent::Done { id: 1, summary } => break summary,
                other => panic!("{other:?}"),
            }
        };
        let paths: Vec<&str> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["src/lib.rs"], "the ignored build is not searched");
        let line = found.first().and_then(|f| f.lines.first()).unwrap();
        assert_eq!((line.line, line.text.as_str()), (2, "pub fn needle() {}"));
        assert_eq!((summary.files, summary.lines, summary.capped), (1, 1, false));

        client.close().await;
    }

    /// A file dropped on a Linux terminal goes up whole into its shell's directory, and a
    /// server the shell starts is forwarded to this Mac's loopback by the app's own link and
    /// answers through it.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live: cargo xtask linux e2e"]
    async fn an_upload_lands_in_a_linux_shell_and_its_server_is_tunnelled_here() {
        let linux = linux();
        let size = TermSize { cols: 120, rows: 24, ..TermSize::default() };
        let (mut shell, ack) =
            Client::open_on(&linux, &[], size, WorkerLink::start_forwarding).await.unwrap();

        // 1. An upload into the shell's directory, checked by its digest there and its size.
        let here = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..2_500_000_u32).map(|i| (i % 251) as u8).collect();
        let sent = here.path().join("upload.bin");
        std::fs::write(&sent, &bytes).unwrap();
        let xfer = XferId::new();
        let started = Instant::now();
        shell.link.remote().upload(xfer, vec![sent], Dest::SessionCwd(shell.session), false);
        let (path, hash) = shell
            .until_control("the upload", |msg| match msg {
                WorkerMsg::Xfer(XferMsg::Done { xfer: x, path, hash, .. }) if x == xfer => {
                    Some((path, hash))
                }
                WorkerMsg::Xfer(XferMsg::Failed { xfer: x, error, .. }) if x == xfer => {
                    panic!("the upload failed: {error}")
                }
                _other => None,
            })
            .await
            .unwrap();
        let took = started.elapsed();
        assert_eq!(path, format!("{}/upload.bin", ack.home), "in the shell's directory");
        assert!(hash == <[u8; 32]>::from(blake3::hash(&bytes)), "whole");
        let paths = shell
            .until_control("the upload's end", |msg| match msg {
                WorkerMsg::Xfer(XferMsg::Finished { xfer: x, paths }) if x == xfer => Some(paths),
                _other => None,
            })
            .await
            .unwrap();
        assert_eq!(paths, std::slice::from_ref(&path));
        assert_eq!(sh_out(&linux, &format!("stat -c %s {path}")).await, bytes.len().to_string());
        eprintln!("linux upload: {} bytes in {took:?}", bytes.len());

        // 2. A server started in the shell, forwarded here, echoing through the tunnel.
        let port = 47_123_u16;
        shell
            .type_line(&format!(
                "perl -MIO::Socket::INET -e '$s=IO::Socket::INET->new(LocalAddr=>\"127.0.0.1\",\
                 LocalPort=>{port},Listen=>5,ReuseAddr=>1) or die $!; $|=1; \
                 print \"serving on http://127.0.0.1:{port}\\n\"; \
                 while($c=$s->accept){{while(<$c>){{print $c \"echo:$_\"}} close $c}}'"
            ))
            .await
            .unwrap();
        shell.until_row(&format!("serving on http://127.0.0.1:{port}")).await.unwrap();
        let deadline = after(STEP);
        let local = loop {
            let forwarded = shell.forwards.iter().find(|f| f.port.number == port);
            if let Some(local) = forwarded.and_then(|f| f.local) {
                break local;
            }
            shell.step(deadline).await.context("the port forwarded here").unwrap();
        };
        let socket = tokio::net::TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        let (read, mut write) = socket.into_split();
        write.write_all(b"through-linux\n").await.unwrap();
        let mut line = String::new();
        let mut read = tokio::io::BufReader::new(read);
        tokio::time::timeout(STEP, read.read_line(&mut line)).await.unwrap().unwrap();
        assert_eq!(line, "echo:through-linux\n");
        drop(write);

        // Ctrl-C ends the server.
        shell.send(TermRequest::Raw(vec![3])).await.unwrap();
        shell.close().await;
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

        // 3b. The note goes to the freedesktop.org trash, never unlinked: whole in the home
        //     trash's `files`, its info file saying where it was.
        let trash = FsOp::Trash { path: note.clone() };
        shell.link.send(ClientMsg::FsOp { request: 1, op: trash }).await.unwrap();
        let outcome = shell
            .until_control("the trash", |msg| match msg {
                WorkerMsg::FsDone { request: 1, outcome } => Some(outcome),
                _other => None,
            })
            .await
            .unwrap();
        let can = format!("{home}/.local/share/Trash");
        assert_eq!(outcome, FsOutcome::Done { path: format!("{can}/files/note.txt") });
        let info = format!("{can}/info/note.txt.trashinfo");
        shell.link.send(ClientMsg::ReadFile { path: info.clone() }).await.unwrap();
        let read = shell
            .until_control("the trash info", |msg| match msg {
                WorkerMsg::File { path, read } if path == info => Some(read),
                _other => None,
            })
            .await
            .unwrap();
        let FileRead::Text { text, .. } = &read else { panic!("{read:?}") };
        assert!(text.starts_with("[Trash Info]"), "{text}");
        assert!(text.contains(&format!("\nPath={home}/e2e/note.txt\n")), "{text}");
        assert!(text.contains("\nDeletionDate="), "{text}");

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

    /// The clipboard both ways, through the commands a Linux program runs: a copy with `xclip`
    /// in the shell reaches the watching client as an offer, and the client's copy (text, and a
    /// picture it keeps until asked) is what `xclip -o`, `wl-paste` and Claude Code's picture
    /// check read there, the picture fetched from the client when read. The primary selection
    /// stays on the worker.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live: cargo xtask linux e2e"]
    async fn the_clipboard_crosses_between_a_linux_shell_and_the_client() {
        let linux = linux();
        let size = TermSize { cols: 100, rows: 24, ..TermSize::default() };
        let (mut shell, _ack) = Client::open(&linux, &[], size).await.unwrap();
        shell.link.send(ClientMsg::Clip(ClipMsg::Watch(true))).await.unwrap();

        // 1. The worker's copy, announced with its text inline.
        shell.type_line("printf 'from-linux-%s' 42 | xclip -selection clipboard").await.unwrap();
        // Waited for alone: a wait on the screen would pass over an offer that came first.
        let offer = shell
            .until_control("the worker's offer", |msg| match msg {
                WorkerMsg::Clip(ClipMsg::Offer(offer)) => Some(offer),
                _other => None,
            })
            .await
            .unwrap();
        let text = offer.items.iter().flat_map(|item| &item.reps).find_map(|rep| {
            (rep.kind == ClipType::Format(ClipFormat::Text)).then(|| rep.inline.clone())
        });
        assert_eq!(text, Some(Some(b"from-linux-42".to_vec())), "{offer:?}");

        // 2. The client's copy, mirrored there as text and a promised picture.
        let picture: Vec<u8> = (0..70_000_u32).map(|i| (i % 253) as u8).collect();
        let rep = |format, bytes: &[u8], inline: bool| Rep {
            kind: ClipType::Format(format),
            size: Some(bytes.len() as u64),
            hash: Some(blake3::hash(bytes).into()),
            inline: inline.then(|| bytes.to_vec()),
        };
        let theirs = Offer {
            origin: Peer::Client(ClientId::new()),
            generation: 1,
            age_ms: 0,
            concealed: false,
            items: vec![ClipEntry {
                reps: vec![
                    rep(ClipFormat::Text, b"from-the-mac", true),
                    rep(ClipFormat::Png, &picture, false),
                ],
            }],
        };
        let png = theirs.rep_ref(0, ClipType::Format(ClipFormat::Png));
        shell.link.send(ClientMsg::Clip(ClipMsg::Offer(theirs))).await.unwrap();
        shell.type_line("echo \"got-$(xclip -selection clipboard -o)\"").await.unwrap();
        shell.until_row("got-from-the-mac").await.unwrap();
        shell.type_line("echo \"pasted-$(wl-paste)\"").await.unwrap();
        shell.until_row("pasted-from-the-mac").await.unwrap();
        shell
            .type_line(
                "xclip -selection clipboard -t TARGETS -o | grep -qE 'image/(png|jpeg)' && echo has-$((2*3))",
            )
            .await
            .unwrap();
        shell.until_row("has-6").await.unwrap();

        // 3. The picture, read as Claude Code reads one, is asked of this client.
        shell
            .type_line("echo bytes-$(xclip -selection clipboard -t image/png -o | wc -c)")
            .await
            .unwrap();
        let fetch = shell
            .until_control("the picture's fetch", |msg| match msg {
                WorkerMsg::Clip(ClipMsg::Fetch { rep, .. }) => Some(rep),
                _other => None,
            })
            .await
            .unwrap();
        assert_eq!(fetch, png, "the promised picture");
        shell
            .link
            .send(ClientMsg::Clip(ClipMsg::Data { rep: fetch, bytes: picture.clone() }))
            .await
            .unwrap();
        shell.until_row(&format!("bytes-{}", picture.len())).await.unwrap();

        // 4. The primary selection is the worker's own.
        shell.type_line("printf sel | xsel -p -i && echo \"primary-$(xsel -p -o)\"").await.unwrap();
        shell.until_row("primary-sel").await.unwrap();
        shell.type_line("echo \"still-$(xclip -selection clipboard -o)\"").await.unwrap();
        shell.until_row("still-from-the-mac").await.unwrap();

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
