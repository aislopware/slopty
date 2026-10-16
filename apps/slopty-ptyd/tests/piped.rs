//! Piped sessions end to end: a program driven over its standard streams (a `claude -p`
//! speaking stream-json) spawned on a socket pair, kept by ptyd across a worker that goes and
//! across a handover to a new build, as a shell on a PTY is.

#[cfg(test)]
mod piped {
    use std::path::PathBuf;
    use std::time::Duration;

    use slopty_core::SessionId;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::ptyd::PtydError;
    use slopty_proto::terminal::TermSize;
    use slopty_pty::protocol::Exit;
    use slopty_pty::{PtyError, PtyMaster, PtydClient, SpawnSpec};

    struct Daemon {
        child: std::process::Child,
        socket: PathBuf,
        dir: tempfile::TempDir,
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _kill = self.child.kill();
            let _wait = self.child.wait();
        }
    }

    async fn start() -> Daemon {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ptyd.sock");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_slopty-ptyd"));
        // A clean environment, but for a `TERM` a piped child must not be handed.
        let child = slopty_testkit::env::scrub(&mut child, &dir.path().join("home"))
            .env("TERM", "xterm-256color")
            .arg("--socket")
            .arg(&socket)
            .spawn()
            .unwrap();
        let mut daemon = Daemon { child, socket, dir };
        let ready = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Some(status) = daemon.child.try_wait().expect("poll ptyd") {
                    panic!("ptyd exited early: {status}");
                }
                if tokio::net::UnixStream::connect(&daemon.socket).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        ready.await.expect("ptyd listens");
        daemon
    }

    fn script(line: &str) -> SpawnSpec {
        SpawnSpec {
            command: vec!["/bin/sh".into(), "-c".into(), line.into()],
            cwd: None,
            env: Vec::new(),
            size: TermSize::default(),
        }
    }

    /// Echoes each line it reads, after `delay`.
    fn echoer(delay: &str) -> SpawnSpec {
        script(&format!("while read l; do sleep {delay}; echo \"got $l\"; done"))
    }

    async fn read_until(master: &PtyMaster, needle: &[u8], mut acc: Vec<u8>) -> Vec<u8> {
        let mut buf = [0_u8; 4096];
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while !acc.windows(needle.len()).any(|w| w == needle) {
            let n = tokio::time::timeout_at(deadline, master.read(&mut buf))
                .await
                .unwrap_or_else(|_| {
                    let had = String::from_utf8_lossy(&acc);
                    panic!("no {:?} in 10 s, after {had:?}", String::from_utf8_lossy(needle))
                })
                .unwrap();
            assert_ne!(n, 0, "EOF before {needle:?}: {:?}", String::from_utf8_lossy(&acc));
            acc.extend_from_slice(&buf[..n]);
        }
        acc
    }

    /// Attach `id` on a fresh connection, once the one that held it has let go.
    async fn attach_when_free(
        daemon: &Daemon,
        id: SessionId,
    ) -> (PtydClient, slopty_pty::client::Attached) {
        let (mut next, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let attached = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match next.attach(id).await {
                    Ok(attached) => break attached,
                    Err(e) => {
                        assert!(matches!(e, PtyError::Daemon(PtydError::AttachedElsewhere)), "{e}");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        })
        .await
        .expect("the session is handed back");
        (next, attached)
    }

    fn alive(pid: i32) -> bool {
        rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }

    /// A piped child's standard input and output are the socket the attaching worker is
    /// handed, its standard error goes to the file named (the user's alone), it is told of no
    /// terminal, and the list says it is piped. A resize means nothing to it.
    #[tokio::test]
    async fn a_piped_child_speaks_over_the_socket_it_is_handed() {
        let daemon = start().await;
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        let stderr = daemon.dir.path().join("agent.err");
        // What the child's environment holds, not the shell's variable: a `sh` that is bash
        // sets `TERM=dumb` for itself when none is given, without exporting it.
        let line = "echo \"term=<$(printenv TERM || echo none)>\"; echo oops >&2; \
                    while read l; do echo \"got $l\"; done";
        client.spawn_piped(id, script(line), Some(stderr.clone())).await.unwrap();
        let attached = client.attach(id).await.unwrap();
        assert_eq!(attached.term, "", "no terminal");
        let master = PtyMaster::new(attached.master).unwrap();
        let said = read_until(&master, b"term=<none>", attached.backlog).await;
        assert!(!said.contains(&b'\r'), "no tty translated its newlines: {said:?}");
        master.write_all(b"hello\n").await.unwrap();
        read_until(&master, b"got hello\n", Vec::new()).await;

        let size = TermSize { cols: 100, rows: 30, metrics: CellMetrics::default() };
        client.resize(id, size).await.unwrap();
        let list = client.list().await.unwrap();
        let info = list.iter().find(|s| s.id == id).expect("listed");
        assert!(info.piped && info.tty.as_os_str().is_empty(), "{info:?}");
        assert_eq!(info.size, TermSize::default(), "a pipe has no size");
        let errors = std::fs::read_to_string(&stderr).unwrap();
        assert_eq!(errors, "oops\n");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&stderr).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600, "the error file is the user's alone");
        client.shutdown().await.unwrap();
    }

    /// The worker driving a piped child goes mid-answer; the child goes on, ptyd keeps what it
    /// writes meanwhile, and the next worker is handed that as the backlog, then speaks to
    /// the same child.
    #[tokio::test]
    async fn a_piped_child_outlives_its_worker_and_what_it_said_waits() {
        let daemon = start().await;
        let id = SessionId::new();
        {
            let (mut first, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
            first.spawn_piped(id, echoer("0.3"), None).await.unwrap();
            let attached = first.attach(id).await.unwrap();
            let master = PtyMaster::new(attached.master).unwrap();
            master.write_all(b"while-you-were-gone\n").await.unwrap();
            // The worker dies before the answer: its connection and its copy of the socket go.
        }
        let (mut watcher, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let list = watcher.list().await.unwrap();
                let info = list.iter().find(|s| s.id == id).expect("kept");
                assert_eq!(info.exited, None, "the child lives on");
                if !info.attached && info.backlog > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("ptyd read the answer while nobody held the socket");
        let (_next, attached) = attach_when_free(&daemon, id).await;
        assert_eq!(attached.backlog, b"got while-you-were-gone\n", "the answer waited");
        let master = PtyMaster::new(attached.master).unwrap();
        master.write_all(b"again\n").await.unwrap();
        read_until(&master, b"got again\n", Vec::new()).await;
        watcher.shutdown().await.unwrap();
    }

    /// Closing a piped session ends its whole process group: `SIGTERM` first, which ends one
    /// that heeds it at once, and `SIGKILL` 5 s later for one that does not, its children
    /// with it.
    #[tokio::test]
    async fn closing_a_piped_session_ends_its_whole_process_group() {
        let daemon = start().await;
        let (mut client, mut exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let mut grandchildren = Vec::new();
        let (heeds, deaf) = (SessionId::new(), SessionId::new());
        for (id, trap) in [(heeds, ""), (deaf, "trap '' TERM; ")] {
            let line = format!("{trap}sleep 300 & echo \"grandchild $!\"; read x; wait");
            client.spawn_piped(id, script(&line), None).await.unwrap();
            let attached = client.attach(id).await.unwrap();
            let master = PtyMaster::new(attached.master).unwrap();
            let said = read_until(&master, b"\n", attached.backlog).await;
            let said = String::from_utf8(said).unwrap();
            let pid: i32 = said.trim().trim_start_matches("grandchild ").parse().unwrap();
            assert!(alive(pid), "the grandchild runs");
            grandchildren.push((id, pid, master));
        }
        let closed = tokio::time::Instant::now();
        client.close(heeds).await.unwrap();
        client.close(deaf).await.unwrap();
        let mut ended = Vec::new();
        while ended.len() < 2 {
            let (id, exit) = tokio::time::timeout(Duration::from_secs(15), exits.recv())
                .await
                .expect("both end")
                .expect("exit channel open");
            ended.push((id, exit, closed.elapsed()));
        }
        let of = |id| ended.iter().find(|(e, ..)| *e == id).expect("ended");
        let (_, exit, took) = of(heeds);
        assert_eq!(*exit, Exit::with(-rustix::process::Signal::TERM.as_raw()), "{took:?}");
        assert!(*took < Duration::from_secs(4), "at the SIGTERM: {took:?}");
        let (_, exit, took) = of(deaf);
        assert_eq!(*exit, Exit::with(-rustix::process::Signal::KILL.as_raw()));
        assert!(*took >= Duration::from_secs(4), "only at the SIGKILL: {took:?}");
        for (id, pid, _master) in &grandchildren {
            let gone = tokio::time::timeout(Duration::from_secs(10), async {
                while alive(*pid) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            });
            assert!(gone.await.is_ok(), "{id}'s grandchild {pid} went with it");
        }
        let list = client.list().await.unwrap();
        assert_eq!(list, Vec::new(), "forgotten");
        client.shutdown().await.unwrap();
    }

    /// ptyd hands a piped session to a new build run in place like a PTY's: the same child,
    /// still piped, speaking over the same socket.
    #[tokio::test]
    async fn a_piped_session_survives_a_handover_to_a_new_build() {
        let daemon = start().await;
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        let pid = client.spawn_piped(id, echoer("0"), None).await.unwrap();
        let (mut asker, _) = PtydClient::connect(&daemon.socket).await.unwrap();
        let program = PathBuf::from(env!("CARGO_BIN_EXE_slopty-ptyd"));
        asker.succeed(program).await.expect("no answer: the new build runs");
        drop(client);
        // The old build may still take a connection while it hands over, then close it: only an
        // answered list is the new build.
        let (back, list) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok((mut connected, exits)) = PtydClient::connect(&daemon.socket).await
                    && let Ok(list) = connected.list().await
                {
                    break ((connected, exits), list);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the new build answers");
        let (mut back, _exits) = back;
        let info = list.iter().find(|s| s.id == id).expect("handed over");
        assert!(info.piped && info.exited.is_none() && info.pid == pid, "{info:?}");
        let attached = back.attach(id).await.unwrap();
        let master = PtyMaster::new(attached.master).unwrap();
        master.write_all(b"across\n").await.unwrap();
        read_until(&master, b"got across\n", Vec::new()).await;
        back.shutdown().await.unwrap();
    }

    /// A piped spawn of a program that is not there is refused, and nothing is kept of it.
    #[tokio::test]
    async fn a_missing_program_is_no_piped_session() {
        let daemon = start().await;
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        let spec = SpawnSpec {
            command: vec![daemon.dir.path().join("no-such-claude").display().to_string()],
            ..script("")
        };
        let refused = client.spawn_piped(id, spec, None).await;
        assert!(matches!(refused, Err(PtyError::Daemon(PtydError::Os(_)))), "{refused:?}");
        assert_eq!(client.list().await.unwrap(), Vec::new(), "nothing kept of it");
        client.shutdown().await.unwrap();
    }
}
