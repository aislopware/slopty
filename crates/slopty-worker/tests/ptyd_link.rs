//! The worker's session table against a real `slopty-ptyd`: what it does with each refusal.

#[cfg(test)]
mod ptyd_link {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_agent::status::SessionAgent;
    use slopty_client::term::{Exit as Ended, TermState};
    use slopty_core::{ClientId, SessionId};
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{OpenSession, SessionState, TermEvent, TermRequest, TermSize};
    use slopty_pty::protocol::Exit;
    use slopty_pty::{PtydClient, SpawnSpec};
    use slopty_worker::manager::{RESTARTING, STILL_RUNS};
    use slopty_worker::orchestrate::Agents;
    use slopty_worker::session::{Outbound, RESTARTED_DIVIDER, SessionHandle};
    use slopty_worker::{Worker, WorkerError};
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(20);

    struct NoAgents;

    impl Agents for NoAgents {
        fn status(&self, _session: SessionId) -> Option<SessionAgent> {
            None
        }

        fn forget(&self, _session: SessionId) {}
    }

    /// `slopty-ptyd` in this build's profile. A workspace build already put it in the profile
    /// directory: the ancestor of this test's executable that sits in the target dir (found
    /// from `CARGO_TARGET_TMPDIR`), whether the test runs from `deps/` or the gate's `run/`.
    /// When no ancestor does (a build-dir outside the target dir) or the binary is missing
    /// (`-p slopty-worker` alone), cargo is asked to build it and name the executable.
    fn ptyd_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let built = exe
            .ancestors()
            .find(|dir| dir.parent() == Some(target))
            .map(|profile| profile.join("slopty-ptyd"))
            .filter(|path| path.exists());
        if let Some(built) = built {
            return built;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = std::process::Command::new(cargo);
        build.args(["build", "-p", "slopty-ptyd", "--bin", "slopty-ptyd", "--message-format=json"]);
        if exe.components().any(|c| c.as_os_str() == "release") {
            build.arg("--release");
        }
        let output = build.stderr(Stdio::inherit()).output().unwrap();
        assert!(output.status.success(), "build slopty-ptyd");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "slopty-ptyd")
            .and_then(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("cargo reports slopty-ptyd's executable")
    }

    /// A ptyd on its own socket in `dir`, killed when the child drops.
    async fn ptyd(dir: &Path) -> (Child, PathBuf) {
        let socket = dir.join("ptyd.sock");
        let mut child = Command::new(ptyd_bin());
        // A clean environment: nothing of the developer's reaches the shells ptyd starts.
        slopty_testkit::env::scrub(child.as_std_mut(), &dir.join("home"));
        let child = child
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(WAIT, async {
            while tokio::net::UnixStream::connect(&socket).await.is_err() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("ptyd listens");
        (child, socket)
    }

    fn sleeper() -> SpawnSpec {
        SpawnSpec {
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
            cwd: None,
            env: Vec::new(),
            size: TermSize::default(),
        }
    }

    /// A worker that starts while the one before it still holds a session (it is exiting) takes
    /// the session once it is let go, and says so, rather than losing it for good.
    #[tokio::test]
    async fn a_session_an_older_worker_holds_is_adopted_once_it_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (mut older, _exits) = PtydClient::connect(&socket).await.unwrap();
        let id = SessionId::new();
        older.spawn(id, sleeper()).await.unwrap();
        let held = older.attach(id).await.unwrap();

        let (worker, mut reports) =
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.path().join("kept"), None)
                .await
                .unwrap();
        assert!(worker.get(id).is_err(), "not the worker's while the older one holds it");

        drop(held);
        drop(older);
        let announced = tokio::time::timeout(WAIT, reports.moves.recv()).await.unwrap();
        assert_eq!(announced, Some(id), "the adoption is announced");
        assert!(worker.get(id).is_ok(), "adopted once the older worker let go");
        worker.close(id).await.unwrap();
    }

    /// A shell ptyd started as `xterm-256color` is answered as one by the worker that adopts
    /// it: XTGETTCAP `TN` names the terminal the shell was told it is on, whatever this host
    /// would give a new shell.
    #[tokio::test]
    async fn an_adopted_shell_is_answered_for_the_term_ptyd_gave_it() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let term = "xterm-256color";
        let digit = |d: u8| char::from_digit(u32::from(d), 16).unwrap().to_ascii_uppercase();
        let hex: String = term.bytes().flat_map(|b| [digit(b >> 4), digit(b & 0xf)]).collect();
        let expected = format!("\x1bP1+r544E={hex}\x1b\\");
        let answer = dir.path().join("answer");
        // Asks only once the worker holds the master, which it shows by typing a line: a query
        // that waited in the backlog would be replayed, never answered.
        let script = format!(
            "read x; stty -icanon -echo; printf '\\033P+q544e\\033\\\\'; \
             dd bs=1 count={} of='{}' 2>/dev/null; sleep 60",
            expected.len(),
            answer.display()
        );
        let (mut spawner, _exits) = PtydClient::connect(&socket).await.unwrap();
        let id = SessionId::new();
        let spec = SpawnSpec {
            command: vec!["/bin/sh".into(), "-c".into(), script],
            cwd: None,
            env: vec![("TERM".into(), term.into())],
            size: TermSize::default(),
        };
        spawner.spawn(id, spec).await.unwrap();
        drop(spawner);

        let (worker, _reports) =
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.path().join("kept"), None)
                .await
                .unwrap();
        let session = worker.get(id).expect("adopted at connect");
        session.request(ClientId::new(), TermRequest::Raw(b"go\r".to_vec())).unwrap();
        let got = tokio::time::timeout(WAIT, async {
            loop {
                match std::fs::read(&answer) {
                    Ok(got) if got.len() == expected.len() => break got,
                    _ => tokio::time::sleep(Duration::from_millis(25)).await,
                }
            }
        })
        .await
        .expect("the shell hears an answer");
        assert_eq!(String::from_utf8_lossy(&got), expected);
        worker.close(id).await.unwrap();
    }

    /// ptyd dies under a worker: the worker stays up, dials the ptyd that starts again and hands
    /// it the session, whose shell lived on through the worker's copy of its master. The new
    /// ptyd holds it for the worker, reports its end (no child of its own, so with no status),
    /// and the shell still hears what is typed into it.
    #[tokio::test]
    async fn a_worker_hands_its_sessions_to_a_ptyd_that_starts_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut first, socket) = ptyd(dir.path()).await;
        let (worker, mut reports) = Worker::connect(
            Some(socket.clone()),
            Arc::new(NoAgents),
            &dir.path().join("kept"),
            None,
        )
        .await
        .unwrap();
        let heard = dir.path().join("heard");
        let open = OpenSession {
            size: TermSize::default(),
            cwd: None,
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("read x; printf %s \"$x\" > '{}'; exit 3", heard.display()),
            ],
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let id = worker.open(&open).await.unwrap().id();

        first.kill().await.unwrap();
        let (second, socket) = ptyd(dir.path()).await;
        let held = tokio::time::timeout(WAIT, async {
            loop {
                if let Ok((mut ptyd, _exits)) = PtydClient::connect(&socket).await
                    && let Ok(list) = ptyd.list().await
                    && let Some(info) = list.into_iter().find(|i| i.id == id)
                {
                    break info;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the worker hands the session to the new ptyd");
        assert!(held.attached, "held for the worker, which reads the master itself");
        assert!(held.pid > 0 && held.exited.is_none(), "{held:?}");
        // Counted by asking the new ptyd, as an install does: the rescued shell is no child of
        // it, so its children would say it holds nothing.
        let counted = std::process::Command::new(ptyd_bin())
            .arg("--sessions")
            .arg("--socket")
            .arg(&socket)
            .output()
            .unwrap();
        assert!(counted.status.success(), "{counted:?}");
        assert_eq!(String::from_utf8_lossy(&counted.stdout).trim(), "1", "the rescued shell");
        let parent = second.id().unwrap().to_string();
        let ps = std::process::Command::new("ps").args(["-A", "-o", "ppid="]).output().unwrap();
        let children = String::from_utf8_lossy(&ps.stdout)
            .lines()
            .filter(|line| line.trim() == parent)
            .count();
        assert_eq!(children, 0, "no child of the new ptyd");

        let session = worker.get(id).expect("the worker kept the session");
        session.request(ClientId::new(), TermRequest::Raw(b"still here\r".to_vec())).unwrap();
        let exit = tokio::time::timeout(WAIT, reports.exits.recv())
            .await
            .expect("the new ptyd reports the end")
            .expect("the exits outlive a lost ptyd");
        assert_eq!(exit, (id, Exit::UNKNOWN), "an adopted child's status is not known");
        assert_eq!(std::fs::read_to_string(&heard).unwrap(), "still here", "the shell lived on");
        worker.close(id).await.unwrap();
    }

    /// The custody this build of ptyd keeps, as `--custody` prints it first.
    fn custody() -> String {
        let said = std::process::Command::new(ptyd_bin()).arg("--custody").output().unwrap();
        let said = String::from_utf8(said.stdout).unwrap();
        said.split_whitespace().next().expect("a custody").to_owned()
    }

    /// A worker dials only a ptyd that says, under the pid the connection reached, the custody
    /// it speaks: one that keeps another is refused, and so is one whose file another process
    /// wrote, each with its sessions left alone.
    #[tokio::test]
    async fn a_worker_dials_only_a_ptyd_that_keeps_its_custody() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let kept = dir.path().join("kept");
        let connect = |custody: &str| {
            Worker::connect(Some(socket.clone()), Arc::new(NoAgents), &kept, Some(custody.into()))
        };
        let (worker, _reports) = connect(&custody()).await.expect("its own custody");
        drop(worker);
        let refused = connect("0000000000000000").await.expect_err("another custody");
        assert!(refused.to_string().contains("keeps custody"), "{refused}");
        let file = socket.with_extension("custody");
        let said = std::fs::read_to_string(&file).unwrap();
        let (_pid, rest) = said.split_once(' ').unwrap();
        std::fs::write(&file, format!("1 {rest}")).unwrap();
        let refused = connect(&custody()).await.expect_err("another process's file");
        assert!(refused.to_string().contains("does not say its custody"), "{refused}");
    }

    /// ptyd runs a new build in place under a worker that holds a session: the worker, which
    /// dials only a ptyd of its own custody, dials the new image, finds its custody under the
    /// same pid, and takes the session back (`Reclaim`), whose shell still hears what is typed.
    #[tokio::test]
    async fn a_worker_takes_its_sessions_back_after_a_handover() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (worker, mut reports) = Worker::connect(
            Some(socket.clone()),
            Arc::new(NoAgents),
            &dir.path().join("kept"),
            Some(custody()),
        )
        .await
        .unwrap();
        let heard = dir.path().join("heard");
        let open = OpenSession {
            size: TermSize::default(),
            cwd: None,
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("read x; printf %s \"$x\" > '{}'; exit 3", heard.display()),
            ],
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let id = worker.open(&open).await.unwrap().id();
        let (mut asker, _exits) = PtydClient::connect(&socket).await.unwrap();
        asker.succeed(ptyd_bin()).await.expect("no answer: the new build runs");
        let reclaimed = tokio::time::timeout(WAIT, async {
            loop {
                tokio::time::sleep(Duration::from_millis(25)).await;
                if let Ok((mut ptyd, _exits)) = PtydClient::connect(&socket).await
                    && let Ok(list) = ptyd.list().await
                    && let Some(info) = list.into_iter().find(|i| i.id == id)
                    && info.attached
                    && let Err(e) = ptyd.attach(id).await
                {
                    break e;
                }
            }
        })
        .await
        .expect("the worker takes the session back");
        assert!(reclaimed.to_string().contains("attached"), "held by the worker: {reclaimed}");
        let session = worker.get(id).expect("the worker kept the session");
        session.request(ClientId::new(), TermRequest::Raw(b"still here\r".to_vec())).unwrap();
        let exit = tokio::time::timeout(WAIT, reports.exits.recv())
            .await
            .expect("the new image reports the end")
            .expect("the exits outlive a handover");
        assert_eq!(exit, (id, Exit::with(3)), "reaped by the same process, with its status");
        assert_eq!(std::fs::read_to_string(&heard).unwrap(), "still here", "the shell lived on");
        worker.close(id).await.unwrap();
    }

    /// Closing a session ptyd has already forgotten is done, not a failure.
    #[tokio::test]
    async fn closing_a_session_ptyd_already_forgot_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (worker, _reports) = Worker::connect(
            Some(socket.clone()),
            Arc::new(NoAgents),
            &dir.path().join("kept"),
            None,
        )
        .await
        .unwrap();
        let open = OpenSession {
            size: TermSize::default(),
            cwd: None,
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let id = worker.open(&open).await.unwrap().id();
        let (mut other, _exits) = PtydClient::connect(&socket).await.unwrap();
        other.close(id).await.unwrap();

        worker.close(id).await.unwrap();
        assert!(worker.get(id).is_err(), "gone from the table");
    }

    const SIZE: TermSize =
        TermSize { cols: 40, rows: 8, metrics: CellMetrics { cell_width: 8, cell_height: 16 } };

    /// A client attached to a session, and the state its events build as the app applies them.
    struct Viewer {
        client: ClientId,
        rx: mpsc::Receiver<Outbound>,
        state: TermState,
        seen: Vec<TermEvent>,
    }

    impl Viewer {
        fn attach(session: &SessionHandle) -> Self {
            let (sink, rx) = mpsc::channel(1024);
            let client = ClientId::new();
            session.attach(client, SIZE, sink).unwrap();
            Self { client, rx, state: TermState::new(SIZE), seen: Vec::new() }
        }

        /// Apply events until `pred` holds of every one seen so far.
        async fn until(&mut self, pred: impl Fn(&[TermEvent]) -> bool) {
            tokio::time::timeout(WAIT, async {
                while !pred(&self.seen) {
                    let out = self.rx.recv().await.expect("the sink stays open");
                    let mut wire = bytes::BytesMut::from(out.wire());
                    let ev: TermEvent =
                        slopty_proto::codec::try_decode(&mut wire).unwrap().expect("an event");
                    let _effects = self.state.apply(ev.clone());
                    self.seen.push(ev);
                }
            })
            .await
            .unwrap_or_else(|_| panic!("events: {:?}\n{}", self.seen, self.screen()));
        }

        fn screen(&self) -> String {
            let lines = self.state.screen().lines();
            lines.iter().map(|l| l.text().trim_end().to_owned()).collect::<Vec<_>>().join("\n")
        }

        /// The row of the screen that holds `text`.
        fn row(&self, text: &str) -> usize {
            let screen = self.screen();
            let found = screen.lines().position(|line| line.contains(text));
            found.unwrap_or_else(|| panic!("{text:?} not on the screen:\n{screen}"))
        }
    }

    fn exits(events: &[TermEvent]) -> usize {
        events.iter().filter(|ev| matches!(ev, TermEvent::Exited { .. })).count()
    }

    /// The state session `id`'s summary says.
    async fn state(worker: &Worker, id: SessionId) -> SessionState {
        worker.summary(id).await.expect("the session is listed").state
    }

    /// The next exit ptyd reports, recorded as the daemon records it.
    async fn ended(
        worker: &Worker,
        exits: &mut mpsc::UnboundedReceiver<(SessionId, Exit)>,
    ) -> Exit {
        let (id, exit) = tokio::time::timeout(WAIT, exits.recv()).await.unwrap().unwrap();
        worker.on_exit(id, exit);
        exit
    }

    /// A session whose program ended starts again at the person's word, and only then: under the
    /// same id, with its viewer attached throughout, the same command with the same variables in
    /// the same directory, below its old screen and a divider. Its summary runs again and the
    /// viewer's ended state goes; the new run's end is told as the first's was. A second word
    /// while the first holds the session is refused.
    #[tokio::test]
    async fn an_ended_session_starts_again_below_its_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (worker, mut reports) =
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.path().join("kept"), None)
                .await
                .unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let open = OpenSession {
            size: SIZE,
            cwd: Some(work.display().to_string()),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                r#"read x; echo "said $x $WORD $(basename "$(pwd -P)")"; exit 3"#.into(),
            ],
            env: vec![("WORD".into(), "kept".into())],
            title: None,
            attach: false,
        };
        let session = worker.open(&open).await.unwrap();
        let id = session.id();
        let mut viewer = Viewer::attach(&session);
        let refused = worker.restarting(id).await.expect_err("its program runs");
        assert_eq!(refused.to_string(), STILL_RUNS);

        let typed = |word: &str| TermRequest::Raw(format!("{word}\r").into_bytes());
        session.request(viewer.client, typed("one")).unwrap();
        assert_eq!(ended(&worker, &mut reports.exits).await, Exit::with(3));
        viewer.until(|seen| exits(seen) == 1).await;
        assert_eq!(viewer.state.exited(), Some(Ended(Some(3))));
        assert_eq!(state(&worker, id).await, SessionState::Exited { status: Some(3) });

        let recipe = worker.restarting(id).await.expect("its program ended");
        let twice = worker.restarting(id).await.expect_err("held by the first");
        assert_eq!(twice.to_string(), RESTARTING);
        worker.restart(id, &recipe.again(), None).await.unwrap();
        worker.restarted(id);
        assert_eq!(state(&worker, id).await, SessionState::Running);
        viewer.until(|seen| seen.contains(&TermEvent::Restarted)).await;
        assert_eq!(viewer.state.exited(), None, "the viewer's ended bar goes");

        session.request(viewer.client, typed("two")).unwrap();
        assert_eq!(ended(&worker, &mut reports.exits).await, Exit::with(3));
        viewer.until(|seen| exits(seen) == 2).await;
        let (first, divider, second) = (
            viewer.row("said one kept work"),
            viewer.row(RESTARTED_DIVIDER),
            viewer.row("said two kept work"),
        );
        assert!(first < divider && divider < second, "{}", viewer.screen());
        assert_eq!(state(&worker, id).await, SessionState::Exited { status: Some(3) });
        assert_eq!(worker.session_count(), 1, "the same session, not another");
        worker.close(id).await.unwrap();
    }

    /// The new program's end that comes while the restart still holds the session is its own,
    /// told once the restart lets go: the session reads as ended again rather than running for
    /// good, and its viewer hears the run start and end in order. A session the worker does not
    /// hold is not started again.
    #[tokio::test]
    async fn a_run_that_ends_at_once_ends_after_the_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (worker, mut reports) =
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.path().join("kept"), None)
                .await
                .unwrap();
        let unknown = worker.restarting(SessionId::new()).await.expect_err("no such session");
        assert!(matches!(unknown, WorkerError::NoSuchSession), "{unknown}");
        let ran = dir.path().join("ran");
        let script = format!("[ -e '{0}' ] && exit 5; touch '{0}'; read x; exit 3", ran.display());
        let open = OpenSession {
            size: SIZE,
            cwd: None,
            command: vec!["/bin/sh".into(), "-c".into(), script],
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let session = worker.open(&open).await.unwrap();
        let id = session.id();
        let mut viewer = Viewer::attach(&session);
        session.request(viewer.client, TermRequest::Raw(b"go\r".to_vec())).unwrap();
        assert_eq!(ended(&worker, &mut reports.exits).await, Exit::with(3));

        let recipe = worker.restarting(id).await.unwrap();
        worker.restart(id, &recipe.again(), None).await.unwrap();
        assert_eq!(ended(&worker, &mut reports.exits).await, Exit::with(5));
        assert_eq!(state(&worker, id).await, SessionState::Running, "held: not the first end");
        worker.restarted(id);
        assert_eq!(state(&worker, id).await, SessionState::Exited { status: Some(5) });
        viewer.until(|seen| exits(seen) == 2).await;
        let told: Vec<&TermEvent> = viewer
            .seen
            .iter()
            .filter(|ev| matches!(ev, TermEvent::Exited { .. } | TermEvent::Restarted))
            .collect();
        let expected = [
            TermEvent::Exited { status: Some(3) },
            TermEvent::Restarted,
            TermEvent::Exited { status: Some(5) },
        ];
        assert_eq!(told, expected.iter().collect::<Vec<_>>());
        assert_eq!(viewer.state.exited(), Some(Ended(Some(5))));
        worker.close(id).await.unwrap();
    }
}
