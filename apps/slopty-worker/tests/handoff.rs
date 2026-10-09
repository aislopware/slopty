//! ptyd + worker + clients on this machine: what a shell hands the client in front of it (web
//! pages, edits), the presence file that follows focus, the agents that keep the machine awake,
//! a paused turn, and the pull request a status line names. Each program under test is the
//! test's own child, run with the environment a session gives it; nothing is typed into a shell.

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod handoff {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId};
    use slopty_net::client::{WorkerConn, bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::ctl::{Awake, CtlReply, CtlRequest};
    use slopty_proto::handoff::{EditOutcome, HandoffEvent, HandoffReply, OfferReason, Wary};
    use slopty_proto::handshake::Hello;
    use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};
    use slopty_proto::thread::wire::{TableFrame, ThreadRequest};
    use slopty_proto::thread::{Phase, Wait};
    use slopty_worker::handoff::TYPED_RECENTLY;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);

    /// A binary of this build (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), name)
    }

    /// The daemons, killed with the test; their pasteboard released.
    struct Daemons {
        children: Vec<Child>,
        pasteboard: String,
        addr: SocketAddr,
        dir: PathBuf,
    }

    impl Drop for Daemons {
        fn drop(&mut self) {
            // Each daemon leads a process group of its own: ended whole and waited for, nothing
            // it started outlives the test holding its output (`slopty_testkit::group`).
            let ended = slopty_testkit::group::end(&mut self.children, Child::id, |child| {
                child.try_wait().is_ok_and(|status| status.is_some())
            });
            debug_assert!(ended, "a daemon's group outlived the test");
            slopty_input::MacBoard::named(&self.pasteboard).release();
        }
    }

    impl Daemons {
        fn sock(&self) -> PathBuf {
            self.dir.join("worker.sock")
        }

        /// A new client connected.
        async fn client(&self, name: &str) -> (slopty_net::Endpoint, WorkerConn) {
            let endpoint = bind_client().unwrap();
            let hello = Hello { client: ClientId::new(), name: name.to_owned() };
            let dialing = async {
                loop {
                    match connect_addr(&endpoint, self.addr, hello.clone()).await {
                        Err(slopty_net::NetError::Connect(why)) if why.ends_with("no answer") => {}
                        other => break other,
                    }
                }
            };
            let conn = tokio::time::timeout(STEP, dialing).await.unwrap().unwrap();
            (endpoint, conn)
        }

        /// A new client connected that takes pages and edits.
        async fn capable(&self, name: &str) -> (slopty_net::Endpoint, WorkerConn) {
            let (endpoint, mut conn) = self.client(name).await;
            conn.tx.send(&slopty_client::handoff::declare(true, true)).await.unwrap();
            settled(&mut conn).await;
            (endpoint, conn)
        }

        /// The worker's process id.
        fn worker_pid(&self) -> u32 {
            self.children.get(1).and_then(Child::id).unwrap()
        }

        /// A program run as a session runs one (`/bin/sh -c script`, in `session`, finding this
        /// worker), with the handoff commands and `stubs` ahead of the system's on `PATH`, and
        /// `EDITOR` the handoff editor by its absolute path.
        fn run_sh(&self, script: &str, session: Option<SessionId>, stubs: &Path) -> Child {
            self.link("slopty-editor");
            self.link("slopty-browser");
            let links = self.dir.join("links");
            let mut command = scrubbed("/bin/sh", &self.dir);
            command
                .args(["-c", script])
                .current_dir(&self.dir)
                .env("SLOPTY_WORKER_SOCKET", self.sock())
                .env("PATH", format!("{}:{}:{PATH}", links.display(), stubs.display()))
                .env("EDITOR", links.join("slopty-editor"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(session) = session {
                command.env("SLOPTY_SESSION", session.to_string());
            }
            command.spawn().unwrap()
        }

        /// `slopty` under `name` (a handoff command), run as a session runs it: in `session`,
        /// finding this worker, with `path` first on `PATH` and its home in the test's directory.
        fn run_as(
            &self,
            name: &str,
            args: &[&str],
            session: Option<SessionId>,
            path: &str,
        ) -> Child {
            let mut command = scrubbed(self.link(name), &self.dir);
            command
                .args(args)
                .current_dir(&self.dir)
                .env("SLOPTY_WORKER_SOCKET", self.sock())
                .env("PATH", path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(session) = session {
                command.env("SLOPTY_SESSION", session.to_string());
            }
            command.spawn().unwrap()
        }

        /// `slopty` linked as `name`, as a session's handoff commands are.
        fn link(&self, name: &str) -> PathBuf {
            let links = self.dir.join("links");
            std::fs::create_dir_all(&links).unwrap();
            let link = links.join(name);
            if !link.exists() {
                std::os::unix::fs::symlink(bin("slopty"), &link).unwrap();
            }
            link
        }

        /// One request on the control socket, and its reply.
        async fn ctl(&self, request: &CtlRequest) -> CtlReply {
            let mut stream = tokio::net::UnixStream::connect(self.sock()).await.unwrap();
            let mut line = serde_json::to_vec(request).unwrap();
            line.push(b'\n');
            stream.write_all(&line).await.unwrap();
            let mut reply = String::new();
            BufReader::new(stream).read_line(&mut reply).await.unwrap();
            serde_json::from_str(reply.trim()).unwrap()
        }

        async fn awake(&self) -> Awake {
            match self.ctl(&CtlRequest::Wake).await {
                CtlReply::Wake(awake) => awake,
                other => panic!("{other:?}"),
            }
        }

        /// Wait until what keeps the machine awake satisfies `want`.
        async fn awake_until(&self, want: impl Fn(&Awake) -> bool) -> Awake {
            let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
            loop {
                let awake = self.awake().await;
                if want(&awake) {
                    return awake;
                }
                assert!(tokio::time::Instant::now() < deadline, "still {awake:?}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        async fn hook(&self, session: SessionId, payload: &str) {
            let hook = CtlRequest::Hook { session, payload: payload.to_owned() };
            assert!(matches!(self.ctl(&hook).await, CtlReply::Ok { .. }));
        }
    }

    /// `program`, started from a clean environment with its home at `home`
    /// (`slopty_testkit::env::scrub`): nothing of the developer's reaches it.
    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
    }

    /// ptyd and a worker on `dir`, whose home is `dir`.
    async fn daemons(dir: &Path) -> Daemons {
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), dir)
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
            assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
            assert!(tokio::time::Instant::now() < deadline, "ptyd never listened");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let leaf = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let pasteboard = format!("dev.aislopware.slopty.handoff.{leaf}");
        let mut worker = scrubbed(bin("slopty-worker"), dir)
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(dir.join("worker.sock"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--print-addr", "--port", "0"])
            .env("SLOPTY_PASTEBOARD", &pasteboard)
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let bound: SocketAddr = line.trim().parse().unwrap();
        let addr = if bound.ip().is_unspecified() {
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()))
        } else {
            bound
        };
        Daemons { children: vec![ptyd, worker], pasteboard, addr, dir: dir.to_path_buf() }
    }

    /// The first control message `pick` takes within a step.
    async fn next_msg<T>(
        worker: &mut WorkerConn,
        mut pick: impl FnMut(WorkerMsg) -> Option<T>,
    ) -> T {
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        loop {
            let msg = tokio::time::timeout_at(deadline, worker.rx.recv()).await.unwrap().unwrap();
            if let Some(found) = pick(msg) {
                return found;
            }
        }
    }

    /// Whether a message `pick` takes arrives within `wait`.
    async fn arrives<T>(
        worker: &mut WorkerConn,
        wait: Duration,
        mut pick: impl FnMut(WorkerMsg) -> Option<T>,
    ) -> bool {
        let deadline = tokio::time::Instant::now().checked_add(wait).unwrap();
        loop {
            match tokio::time::timeout_at(deadline, worker.rx.recv()).await {
                Err(_elapsed) => return false,
                Ok(msg) => {
                    if pick(msg.unwrap()).is_some() {
                        return true;
                    }
                }
            }
        }
    }

    /// Every message sent before this has been handled.
    async fn settled(worker: &mut WorkerConn) {
        let sent_at = slopty_core::MonoTime::now();
        worker.tx.send(&ClientMsg::Ping { sent_at }).await.unwrap();
        next_msg(worker, |m| {
            matches!(m, WorkerMsg::Pong { sent_at: s } if s == sent_at).then_some(())
        })
        .await;
    }

    /// A shell the client opened, running `command` (a plain `/bin/sh` when empty).
    async fn open(worker: &mut WorkerConn, command: &[&str], attach: bool) -> SessionId {
        let command = if command.is_empty() {
            vec!["/bin/sh".to_owned()]
        } else {
            command.iter().map(|w| (*w).to_owned()).collect()
        };
        let spec = OpenSession {
            size: TermSize { cols: 200, rows: 10, ..TermSize::default() },
            cwd: None,
            command,
            env: Vec::new(),
            title: None,
            attach,
        };
        worker.tx.send(&ClientMsg::OpenSession { request: 1, spec }).await.unwrap();
        next_msg(worker, |m| match m {
            WorkerMsg::SessionOpened { summary, .. } => Some(summary.id),
            _ => None,
        })
        .await
    }

    async fn focus(worker: &mut WorkerConn, session: SessionId, focused: bool) {
        let req = TermRequest::Focus { focused };
        worker.tx.send(&ClientMsg::Term { session, req }).await.unwrap();
        settled(worker).await;
    }

    async fn exits(child: Child) -> (i32, String) {
        let out = tokio::time::timeout(STEP, child.wait_with_output()).await.unwrap().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    async fn close(endpoint: &slopty_net::Endpoint) {
        endpoint.close(0_u32.into(), b"done");
        let _drained = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    }

    const PATH: &str = "/usr/bin:/bin";

    /// The system's `open` and `vi` as stubs in `dir/stubs` that write what they were given to
    /// `dir/<name>-said` and exit `code`; the directory, to put on `PATH`.
    fn stubs(dir: &Path, code: i32) -> PathBuf {
        let stubs = dir.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        for name in ["open", "vi"] {
            let said = dir.join(format!("{name}-said"));
            let script =
                format!("#!/bin/sh\nprintf '%s ' \"$@\" > '{}'\nexit {code}\n", said.display());
            std::fs::write(stubs.join(name), script).unwrap();
            std::fs::set_permissions(
                stubs.join(name),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        }
        stubs
    }

    /// What the stub for `name` was given; `None` when it never ran.
    fn stub_said(dir: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(dir.join(format!("{name}-said"))).ok()
    }

    /// A key typed into `session` (a `cat`, never a shell), as a person's Enter before a login.
    async fn typed(worker: &mut WorkerConn, session: SessionId) {
        let req = TermRequest::Raw(b"x".to_vec());
        worker.tx.send(&ClientMsg::Term { session, req }).await.unwrap();
        settled(worker).await;
    }

    fn asked_open(m: WorkerMsg) -> Option<slopty_proto::handoff::OpenUrl> {
        match m {
            WorkerMsg::Handoff(HandoffEvent::Open(open)) => Some(open),
            _ => None,
        }
    }

    fn asked_edit(m: WorkerMsg) -> Option<slopty_proto::handoff::EditFile> {
        match m {
            WorkerMsg::Handoff(HandoffEvent::Edit(edit)) => Some(edit),
            _ => None,
        }
    }

    /// `BROWSER` in a session goes to the client focused on it that just typed into it, not to
    /// another client, opens there, and returns once taken.
    #[tokio::test]
    async fn a_page_opens_on_the_client_that_just_typed() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_ea, mut front) = d.capable("front").await;
        let (_eb, mut other) = d.capable("other").await;
        let shell = open(&mut front, &["/bin/cat"], false).await;
        focus(&mut front, shell, true).await;
        typed(&mut front, shell).await;
        let url = "https://github.com/login/device";
        let child = d.run_as("slopty-browser", &[url], Some(shell), PATH);
        let asked = next_msg(&mut front, asked_open).await;
        assert_eq!((asked.url.as_str(), asked.session, asked.offer), (url, Some(shell), None));
        front.tx.send(&ClientMsg::Handoff(HandoffReply::Taken { id: asked.id })).await.unwrap();
        assert_eq!(exits(child).await.0, 0);
        let other_asked = arrives(&mut other, Duration::from_millis(300), |m| {
            matches!(m, WorkerMsg::Handoff(_)).then_some(())
        });
        assert!(!other_asked.await, "only the client in front was asked");
    }

    /// A page nobody typed for (a program opening pages on its own), or one at a local
    /// address, is offered, not opened, and the program is told where.
    #[tokio::test]
    async fn a_page_nobody_typed_for_is_offered() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_e, mut client) = d.capable("front").await;
        let shell = open(&mut client, &["/bin/cat"], false).await;
        focus(&mut client, shell, true).await;
        for (url, why) in [
            ("https://example.com/docs", OfferReason::NotTyped),
            ("http://localhost:5173/", OfferReason::Wary(Wary::Loopback)),
        ] {
            typed(&mut client, shell).await;
            let wait = if why == OfferReason::NotTyped { TYPED_RECENTLY } else { Duration::ZERO };
            tokio::time::sleep(wait + Duration::from_millis(200)).await;
            let child = d.run_as("slopty-browser", &[url], Some(shell), PATH);
            let asked = next_msg(&mut client, asked_open).await;
            assert_eq!(asked.offer, Some(why), "{url}");
            let offered = HandoffReply::Offered { id: asked.id, why };
            client.tx.send(&ClientMsg::Handoff(offered)).await.unwrap();
            let (code, said) = exits(child).await;
            assert_eq!(code, 0);
            assert!(said.contains("is offered on front"), "{said}");
        }
    }

    /// A client that does not answer in time is withdrawn and passed over, one that refuses is
    /// passed over, and the next takes it.
    #[tokio::test]
    async fn a_silent_or_refusing_client_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_ec, mut taker) = d.capable("taker").await;
        let (_eb, mut refuser) = d.capable("refuser").await;
        let (_ea, mut silent) = d.capable("silent").await;
        let shell = open(&mut silent, &["/bin/cat"], false).await;
        focus(&mut silent, shell, true).await;
        let child = d.run_as("slopty-browser", &["https://example.com/a"], Some(shell), PATH);
        let id = next_msg(&mut silent, asked_open).await.id;
        let withdrawn = next_msg(&mut silent, |m| match m {
            WorkerMsg::Handoff(HandoffEvent::Withdrawn { id }) => Some(id),
            _ => None,
        })
        .await;
        assert_eq!(withdrawn, id, "withdrawn once its time was up");
        let id = next_msg(&mut refuser, asked_open).await.id;
        refuser.tx.send(&ClientMsg::Handoff(HandoffReply::Refused { id })).await.unwrap();
        let asked = next_msg(&mut taker, asked_open).await;
        let offered = HandoffReply::Offered { id: asked.id, why: OfferReason::NotTyped };
        taker.tx.send(&ClientMsg::Handoff(offered)).await.unwrap();
        assert_eq!(exits(child).await.0, 0);
    }

    /// With no client connected, or none that takes pages, the page opens on this machine at
    /// once, as it would without Slopty, and stderr says why.
    #[tokio::test]
    async fn with_no_client_to_take_it_a_page_opens_here_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let stubs = stubs(dir.path(), 0);
        let path = format!("{}:{PATH}", stubs.display());
        let url = "https://example.com/oauth";
        let started = std::time::Instant::now();
        let (code, said) = exits(d.run_as("slopty-browser", &[url], None, &path)).await;
        assert_eq!(code, 0);
        assert!(said.contains("no client is connected"), "{said}");
        assert_eq!(stub_said(dir.path(), "open").as_deref(), Some(format!("{url} ").as_str()));
        let (_e, _terminal) = d.client("terminal").await;
        let (code, said) = exits(d.run_as("slopty-browser", &[url], None, &path)).await;
        assert_eq!(code, 0);
        assert!(said.contains("no connected client takes it"), "{said}");
        assert!(started.elapsed() < Duration::from_secs(3), "no wait on a client that cannot");
    }

    /// `EDITOR` shows the file on the client and waits. The program that waits reads what the
    /// tile saved, both by the file's name and through a descriptor it opened before the editor
    /// ran, as `crontab -e` does; it gets 1 when the person gave the edit up.
    #[tokio::test]
    async fn an_editor_waits_for_the_tile_and_the_program_reads_the_save() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_e, mut client) = d.capable("app").await;
        let shell = open(&mut client, &["/bin/cat"], false).await;
        focus(&mut client, shell, true).await;
        let file = std::fs::canonicalize(dir.path()).unwrap().join("crontab.tmp");
        std::fs::write(&file, "# an old and longer line\n").unwrap();
        let script = "exec 3< crontab.tmp; \"$EDITOR\" crontab.tmp; s=$?; \
                      echo \"held[$(cat <&3)]\"; echo \"named[$(cat crontab.tmp)]\"; exit $s";
        let stubs = stubs(dir.path(), 0);
        let mut child = d.run_sh(script, Some(shell), &stubs);
        let edit = next_msg(&mut client, asked_edit).await;
        assert_eq!(
            (edit.path.as_str(), edit.wait, edit.session),
            (file.to_str().unwrap(), true, Some(shell))
        );
        client.tx.send(&ClientMsg::Handoff(HandoffReply::Taken { id: edit.id })).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(child.try_wait().unwrap().is_none(), "it waits for the person");
        let text = "5 * * * * new\n";
        let save = ClientMsg::WriteFile {
            path: edit.path.clone(),
            text: text.to_owned(),
            base_modified_ms: None,
        };
        client.tx.send(&save).await.unwrap();
        let done = HandoffReply::Edited { id: edit.id, outcome: EditOutcome::Done };
        client.tx.send(&ClientMsg::Handoff(done)).await.unwrap();
        let out = tokio::time::timeout(STEP, child.wait_with_output()).await.unwrap().unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{printed}");
        assert!(printed.contains("held[5 * * * * new]"), "through the held descriptor: {printed}");
        assert!(printed.contains("named[5 * * * * new]"), "{printed}");

        let child = d.run_as("slopty-editor", &["+3", "crontab.tmp"], Some(shell), PATH);
        let edit = next_msg(&mut client, asked_edit).await;
        assert_eq!(edit.line, Some(3));
        client.tx.send(&ClientMsg::Handoff(HandoffReply::Taken { id: edit.id })).await.unwrap();
        let cancel = HandoffReply::Edited { id: edit.id, outcome: EditOutcome::Cancelled };
        client.tx.send(&ClientMsg::Handoff(cancel)).await.unwrap();
        assert_eq!(exits(child).await.0, 1);
    }

    /// A save and the end of the edit sent just before the client disconnects still land: the
    /// program reads the save and goes on.
    #[tokio::test]
    async fn an_edit_ended_as_the_client_leaves_still_lets_the_program_go() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (endpoint, mut client) = d.capable("app").await;
        let file = std::fs::canonicalize(dir.path()).unwrap().join("MSG");
        std::fs::write(&file, "old\n").unwrap();
        let child = d.run_as("slopty-editor", &["MSG"], None, PATH);
        let edit = next_msg(&mut client, asked_edit).await;
        client.tx.send(&ClientMsg::Handoff(HandoffReply::Taken { id: edit.id })).await.unwrap();
        settled(&mut client).await;
        let save = ClientMsg::WriteFile {
            path: edit.path,
            text: "new\n".to_owned(),
            base_modified_ms: None,
        };
        client.tx.send(&save).await.unwrap();
        let done = HandoffReply::Edited { id: edit.id, outcome: EditOutcome::Done };
        client.tx.send(&ClientMsg::Handoff(done)).await.unwrap();
        // Heard by the worker, and the connection gone before it could answer the save.
        settled(&mut client).await;
        close(&endpoint).await;
        assert_eq!(exits(child).await.0, 0);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
    }

    /// The program giving up (killed, interrupted) withdraws the edit from the tile.
    #[tokio::test]
    async fn an_editor_given_up_withdraws_the_edit() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_e, mut client) = d.capable("app").await;
        let path = dir.path().join("rebase-todo");
        std::fs::write(&path, "pick 1\n").unwrap();
        let mut child = d.run_as("slopty-editor", &[path.to_str().unwrap()], None, PATH);
        let id = next_msg(&mut client, asked_edit).await.id;
        client.tx.send(&ClientMsg::Handoff(HandoffReply::Taken { id })).await.unwrap();
        settled(&mut client).await;
        child.start_kill().unwrap();
        let withdrawn = next_msg(&mut client, |m| match m {
            WorkerMsg::Handoff(HandoffEvent::Withdrawn { id }) => Some(id),
            _ => None,
        })
        .await;
        assert_eq!(withdrawn, id);
    }

    /// With no client that shows files, the editor is `vi` in the terminal at once, given the
    /// same arguments, and its exit status is the program's.
    #[tokio::test]
    async fn an_editor_with_no_client_to_show_it_is_vi_here() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let stubs = stubs(dir.path(), 4);
        let path = format!("{}:{PATH}", stubs.display());
        let (_e, mut pages_only) = d.client("phone").await;
        pages_only.tx.send(&slopty_client::handoff::declare(true, false)).await.unwrap();
        settled(&mut pages_only).await;
        let (code, said) =
            exits(d.run_as("slopty-editor", &["+2", "notes.txt"], None, &path)).await;
        assert_eq!(code, 4, "vi's own status");
        assert!(said.contains("no connected client takes it; editing in vi"), "{said}");
        assert_eq!(stub_said(dir.path(), "vi").as_deref(), Some("+2 notes.txt "));
    }

    /// The presence file of a session exists while a client is focused on it, and goes when the
    /// client lets go or disconnects; the session is told where it is.
    #[tokio::test]
    async fn the_presence_file_follows_focus() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (endpoint, mut client) = d.client("app").await;
        let script = r#"echo "P=$CLAUDE_CLIENT_PRESENCE_FILE"; exec sleep 60"#;
        let shell = open(&mut client, &["/bin/sh", "-c", script], true).await;
        let presence = dir.path().join("data").join("presence").join(shell.to_string());
        let slopty_net::streams::Uni::Session { rx: mut events, .. } =
            tokio::time::timeout(STEP, slopty_net::streams::accept_uni(&client.conn))
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("not a session stream")
        };
        let want = format!("P={}", presence.display());
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        loop {
            let ev = tokio::time::timeout_at(deadline, events.recv()).await.unwrap().unwrap();
            if let slopty_proto::terminal::TermEvent::Frame(f) = ev
                && f.updates.iter().any(|u| u.line.text().contains(&want))
            {
                break;
            }
        }
        let is_there = async |want: bool| {
            let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
            while presence.exists() != want {
                assert!(tokio::time::Instant::now() < deadline, "presence still {}", !want);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        is_there(false).await;
        focus(&mut client, shell, true).await;
        is_there(true).await;
        focus(&mut client, shell, false).await;
        is_there(false).await;
        focus(&mut client, shell, true).await;
        is_there(true).await;
        close(&endpoint).await;
        is_there(false).await;
        let (_again, mut client) = d.client("app").await;
        focus(&mut client, shell, true).await;
        is_there(true).await;
        let pid = d.worker_pid().to_string();
        assert!(
            std::process::Command::new("kill").args(["-TERM", &pid]).status().unwrap().success()
        );
        let dir = presence.parent().unwrap().to_path_buf();
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        while dir.exists() {
            assert!(tokio::time::Instant::now() < deadline, "presence files outlived the worker");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// With nobody connected, a working agent holds the machine awake and its stop lets it go;
    /// a turn paused on background work keeps holding it.
    #[tokio::test]
    async fn a_working_agent_keeps_the_worker_awake_with_nobody_connected() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (endpoint, mut client) = d.client("app").await;
        let shell = open(&mut client, &[], false).await;
        close(&endpoint).await;
        d.awake_until(|a| a.clients == 0 && !a.system).await;
        assert!(!asserted(d.worker_pid()), "nothing held with nobody there");
        d.hook(shell, r#"{"hook_event_name":"UserPromptSubmit","prompt":"build it"}"#).await;
        let awake = d.awake_until(|a| a.agents == 1).await;
        assert!(awake.system, "{awake:?}");
        assert!(asserted(d.worker_pid()), "macOS holds the machine for the worker");
        d.hook(
            shell,
            r#"{"hook_event_name":"Stop","background_tasks":[{"id":"b1","type":"shell","status":"running","description":"cargo build"}],"session_crons":[]}"#,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(1_600)).await;
        assert_eq!(d.awake().await.agents, 1, "background work holds it too");
        d.hook(shell, r#"{"hook_event_name":"Stop","background_tasks":[],"session_crons":[]}"#)
            .await;
        let awake = d.awake_until(|a| a.agents == 0).await;
        assert!(!awake.system, "{awake:?}");
        assert!(!asserted(d.worker_pid()), "and lets it go");
    }

    /// Whether macOS lists an idle-sleep assertion held by `pid` for Slopty (`pmset -g
    /// assertions`, which reads the power manager's own table).
    fn asserted(pid: u32) -> bool {
        let out = std::process::Command::new("/usr/bin/pmset")
            .args(["-g", "assertions"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines().any(|line| {
            line.contains(&format!("pid {pid}("))
                && line.contains("PreventUserIdleSystemSleep")
                && line.contains("Slopty client or agent at work")
        })
    }

    /// The recorded turn that ended with a background command out leaves the agent's thread
    /// waiting on it, which asks nobody's attention; the recorded one that ended with nothing
    /// out is done.
    #[tokio::test]
    async fn a_paused_turn_raises_no_done() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_e, mut client) = d.client("app").await;
        let shell = open(&mut client, &[], false).await;
        let stops = recorded_stops();
        let (paused, finished) = (
            stops.iter().find(|s| s.contains("\"status\":\"running\"")).unwrap(),
            stops.last().unwrap(),
        );
        let table = ClientMsg::Thread(ThreadRequest::Table { have: None });
        client.tx.send(&table).await.unwrap();
        d.hook(shell, r#"{"hook_event_name":"UserPromptSubmit","prompt":"go"}"#).await;
        d.hook(shell, paused).await;
        let at = |phase: Phase| {
            move |m: WorkerMsg| {
                let WorkerMsg::Threads(
                    TableFrame::Snapshot { rows, .. } | TableFrame::Delta { rows, .. },
                ) = m
                else {
                    return None;
                };
                rows.into_iter().find(|r| r.terminal == Some(shell) && r.status.phase == phase)
            }
        };
        let waits = next_msg(&mut client, at(Phase::Waiting)).await;
        let wait = waits.status.wait.expect("what it waits on");
        assert_eq!(
            (wait.kind.as_str(), wait.text.as_str()),
            (Wait::TASK, "Sleep then print a marker")
        );
        d.hook(shell, finished).await;
        next_msg(&mut client, at(Phase::Done)).await;
    }

    /// The `Stop` payloads of the recorded `background` scenario, in order.
    fn recorded_stops() -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/slopty-agent/tests/fixtures/conversation/background/hooks.jsonl");
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["input"].clone())
            .filter(|input| input["hook_event_name"] == "Stop")
            .map(|input| input.to_string())
            .collect()
    }

    /// The worktree an agent's status line names reaches every client, a later one in its
    /// greeting.
    #[tokio::test]
    async fn a_status_lines_worktree_reaches_every_client() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemons(dir.path()).await;
        let (_e, mut client) = d.client("app").await;
        let shell = open(&mut client, &[], false).await;
        d.hook(shell, r#"{"hook_event_name":"SessionStart","session_id":"s1","source":"startup"}"#)
            .await;
        let status = serde_json::json!({
            "session_id": "s1",
            "worktree": { "name": "fix-build", "path": "/r/.claude/worktrees/fix-build", "original_cwd": "/r" },
        });
        let mut wrapper = scrubbed(d.link("slopty"), &d.dir)
            .args(["hook", "statusline"])
            .env("SLOPTY_WORKER_SOCKET", d.sock())
            .env("SLOPTY_SESSION", shell.to_string())
            .env("CLAUDE_CONFIG_DIR", d.dir.join("claude-config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut input = wrapper.stdin.take().unwrap();
        input.write_all(status.to_string().as_bytes()).await.unwrap();
        drop(input);
        assert!(tokio::time::timeout(STEP, wrapper.wait()).await.unwrap().unwrap().success());
        let branch = next_msg(&mut client, |m| match m {
            WorkerMsg::AgentBranch(b) => Some(b),
            _ => None,
        })
        .await;
        assert_eq!(branch.session, shell);
        assert_eq!(branch.worktree.map(|w| w.name).as_deref(), Some("fix-build"));
        let (_e2, mut later) = d.client("later").await;
        let greeted = next_msg(&mut later, |m| match m {
            WorkerMsg::AgentBranch(b) => Some(b),
            _ => None,
        })
        .await;
        assert_eq!(greeted.worktree.map(|w| w.name).as_deref(), Some("fix-build"));
    }
}
