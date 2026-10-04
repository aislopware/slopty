//! The orchestration verbs on a session actor over a real bash with Slopty's shell
//! integration, in an in-process PTY (no ptyd): type, wait, read back, list commands,
//! interrupt, find a listening port.

#[cfg(test)]
mod orchestrate {
    use std::time::Duration;

    use slopty_core::{SessionId, WallMs};
    use slopty_proto::WorkerMsg;
    use slopty_proto::agent::{
        AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent,
    };
    use slopty_proto::input::CellMetrics;
    use slopty_proto::orchestration::{Command, ErrorCode, Input, Screen, WaitUntil, Waited};
    use slopty_proto::terminal::TermSize;
    use slopty_pty::shell_integration::{self, ShellIntegration};
    use slopty_pty::{Pty, SpawnSpec};
    use slopty_worker::orchestrate::{
        AgentFeed, Agents, list_commands, may_deliver, may_type, read_output, read_screen,
        send_input, wait_for,
    };
    use slopty_worker::session::{self, SessionHandle, SessionStart};
    use tokio::sync::{broadcast, mpsc};

    const WAIT: Duration = Duration::from_secs(20);

    /// An agent table that holds one status for every session.
    #[derive(Default)]
    struct Table(parking_lot::Mutex<Option<AgentStatus>>);

    impl Table {
        fn with(status: AgentStatus) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self(parking_lot::Mutex::new(Some(status))))
        }
    }

    impl Agents for Table {
        fn status(&self, _session: SessionId) -> Option<SessionAgent> {
            let status = self.0.lock().clone()?;
            Some(SessionAgent {
                kind: AgentKind::ClaudeCode,
                status,
                source: AgentSource::Hook,
                since_ms: WallMs::ZERO,
                mode: None,
            })
        }

        fn forget(&self, _session: SessionId) {}
    }

    /// An agent table that says what the test set of every session: its agent and where its
    /// status came from, and whether an agent ended there.
    #[derive(Default)]
    struct Said(parking_lot::Mutex<(Option<(AgentStatus, AgentSource)>, bool)>);

    impl Said {
        fn agent(&self, status: AgentStatus, source: AgentSource) {
            *self.0.lock() = (Some((status, source)), false);
        }

        fn end(&self) {
            *self.0.lock() = (None, true);
        }
    }

    impl Agents for Said {
        fn status(&self, _session: SessionId) -> Option<SessionAgent> {
            let (status, source) = self.0.lock().0.clone()?;
            Some(SessionAgent {
                kind: AgentKind::ClaudeCode,
                status,
                source,
                since_ms: WallMs::ZERO,
                mode: None,
            })
        }

        fn forget(&self, _session: SessionId) {}

        fn ended(&self, _session: SessionId) -> bool {
            self.0.lock().1
        }
    }

    /// An agent at its prompt by its hooks' word, whose last turn the person stopped or not.
    struct Stopped(parking_lot::Mutex<bool>);

    impl Agents for Stopped {
        fn status(&self, _session: SessionId) -> Option<SessionAgent> {
            Some(SessionAgent {
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::IdlePrompt),
                source: AgentSource::Hook,
                since_ms: WallMs::ZERO,
                mode: None,
            })
        }

        fn forget(&self, _session: SessionId) {}

        fn interrupted(&self, _session: SessionId) -> bool {
            *self.0.lock()
        }
    }

    fn agent_event(session: SessionId, status: AgentStatus) -> WorkerMsg {
        WorkerMsg::Agent(AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: None,
            attention: true,
            source: AgentSource::Hook,
            since_ms: WallMs::ZERO,
            mode: None,
        })
    }

    struct Shell {
        handle: SessionHandle,
        child: slopty_pty::Child,
        _dir: tempfile::TempDir,
    }

    /// An interactive bash with the integration loaded, as ptyd spawns it, in an empty home
    /// on a daemon's `PATH`; returned once its first prompt is drawn.
    async fn shell() -> Shell {
        let dir = tempfile::tempdir().unwrap();
        let si = shell_integration::install(&dir.path().join("shell")).unwrap();
        let si = ShellIntegration {
            original_zdotdir: None,
            original_xdg_data_dirs: None,
            enabled: true,
            ..si
        };
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let size = TermSize {
            cols: 80,
            rows: 24,
            metrics: CellMetrics { cell_width: 8, cell_height: 16 },
        };
        let pty = Pty::open(size).unwrap();
        let spec = SpawnSpec {
            command: vec!["/bin/bash".to_owned(), "-i".to_owned()],
            cwd: Some(home.clone()),
            env: vec![
                ("HOME".to_owned(), home.to_string_lossy().into_owned()),
                ("PATH".to_owned(), "/usr/bin:/bin:/usr/sbin:/sbin".to_owned()),
                ("PS1".to_owned(), "$ ".to_owned()),
                ("BASH_SILENCE_DEPRECATION_WARNING".to_owned(), "1".to_owned()),
            ],
            size,
        };
        let slopty_pty::Spawned { child, term } = pty.spawn_with(&spec, Some(&si)).unwrap();
        let (tap, mut taps) = mpsc::channel(64);
        tokio::spawn(async move { while taps.recv().await.is_some() {} });
        let handle = session::spawn(SessionStart {
            id: SessionId::new(),
            master: pty.into_master(),
            term,
            checkpoint: Vec::new(),
            backlog: Vec::new(),
            tap,
            size,
            scrollback_lines: 1000,
            exited: None,
            port_hints: None,
            moves: None,
            touched: None,
            restored: None,
            divide: false,
        })
        .unwrap();
        until_screen(&handle, |s| last_row(s) == "$").await;
        Shell { handle, child, _dir: dir }
    }

    fn last_row(screen: &Screen) -> &str {
        screen.lines.iter().rev().map(|l| l.text.as_str()).find(|t| !t.is_empty()).unwrap_or("")
    }

    /// Read the screen until `pred` holds, woken by the session's output.
    async fn until_screen(handle: &SessionHandle, pred: impl Fn(&Screen) -> bool) -> Screen {
        let mut activity = handle.activity();
        let looking = async {
            loop {
                let screen = read_screen(handle).await.unwrap();
                if pred(&screen) {
                    return screen;
                }
                activity.changed().await.expect("session alive");
            }
        };
        let Ok(screen) = tokio::time::timeout(WAIT, looking).await else {
            panic!("screen never matched: {:?}", read_screen(handle).await)
        };
        screen
    }

    /// The command blocks once `pred` holds for them, woken by the session's output.
    async fn until_commands(
        handle: &SessionHandle,
        pred: impl Fn(&[Command]) -> bool,
    ) -> Vec<Command> {
        let mut activity = handle.activity();
        let looking = async {
            loop {
                let commands = list_commands(handle, None).await.unwrap();
                if pred(&commands) {
                    return commands;
                }
                activity.changed().await.expect("session alive");
            }
        };
        tokio::time::timeout(WAIT, looking).await.expect("commands never matched")
    }

    fn text(s: &str) -> Input {
        Input::Text(s.to_owned())
    }

    #[tokio::test]
    async fn typed_text_is_waited_for_read_back_and_listed_as_a_command() {
        let sh = shell().await;
        let h = &sh.handle;
        let done = wait_for(h, &WaitUntil::CommandDone, WAIT, None);
        let typed = async {
            send_input(h, &text("echo hi\n")).await.unwrap();
            wait_for(h, &WaitUntil::Output("^hi$".to_owned()), WAIT, None).await.unwrap()
        };
        let (done, seen) = tokio::join!(done, typed);
        assert_eq!(done.unwrap(), Waited::Met { line: None });
        let Waited::Met { line: Some(hi) } = seen else {
            panic!("{seen:?} {:?}", read_screen(h).await)
        };
        assert_eq!(hi.text, "hi");

        let (lines, next) = read_output(h, None, 1000).await.unwrap();
        assert!(lines.iter().any(|l| l.index == hi.index && l.text == "hi"), "{lines:?} {hi:?}");
        assert_eq!(next, lines.last().map_or(0, |l| l.index.saturating_add(1)));
        let (page, _) = read_output(h, Some(hi.index), 1).await.unwrap();
        assert_eq!(page.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["hi"]);

        let commands = list_commands(h, None).await.unwrap();
        let echo = commands.iter().find(|c| c.line == "echo hi").expect("the echo is a block");
        assert_eq!(echo.exit, Some(0));
        assert!(echo.output.0 <= hi.index && hi.index < echo.output.1, "{echo:?} holds {hi:?}");
        assert!(echo.prompt_line < echo.output.0);

        let screen = read_screen(h).await.unwrap();
        assert!(screen.lines.iter().any(|l| l.text == "$ echo hi"), "{screen:?}");
        assert!(!screen.alternate);
        drop(sh.child);
    }

    #[tokio::test]
    async fn ctrl_c_interrupts_a_running_command() {
        let sh = shell().await;
        let h = &sh.handle;
        send_input(h, &text("sleep 100\n")).await.unwrap();
        until_commands(h, |c| c.iter().any(|c| c.line == "sleep 100" && c.exit.is_none())).await;
        // bash writes `133;C` from its DEBUG trap, before it forks the command: a ⌃C that
        // lands in between interrupts the trap and the command runs on. Wait for the
        // foreground process to be sleep itself; it prints nothing to wake on.
        let deadline = tokio::time::Instant::now().checked_add(WAIT).unwrap();
        while h.probe().await.unwrap().foreground.is_none_or(|fg| fg.name != "sleep") {
            assert!(tokio::time::Instant::now() < deadline, "sleep never ran");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let started = std::time::Instant::now();
        let done = wait_for(h, &WaitUntil::CommandDone, WAIT, None);
        let interrupt =
            async { send_input(h, &Input::Keys(vec!["ctrl+c".to_owned()])).await.unwrap() };
        let (done, ()) = tokio::join!(done, interrupt);
        assert_eq!(done.unwrap(), Waited::Met { line: None });
        assert!(started.elapsed() < Duration::from_secs(10), "sleep 100 was interrupted");
        let commands = list_commands(h, None).await.unwrap();
        let sleep = commands.iter().find(|c| c.line == "sleep 100").unwrap();
        assert_eq!(sleep.exit, Some(130), "killed by SIGINT: {commands:?}");
    }

    #[tokio::test]
    async fn a_bad_key_name_is_invalid_and_nothing_is_typed() {
        let sh = shell().await;
        let h = &sh.handle;
        let err = send_input(h, &Input::Keys(vec!["x".to_owned(), "ctrl+nope".to_owned()]))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(err.message.contains("[mods+]key"), "{err:?}");
        let quiet = wait_for(h, &WaitUntil::Quiet { ms: 300 }, WAIT, None).await.unwrap();
        assert_eq!(quiet, Waited::Met { line: None });
        assert_eq!(
            last_row(&read_screen(h).await.unwrap()),
            "$",
            "the valid x was not sent either"
        );
        let bad = wait_for(h, &WaitUntil::Output("(".to_owned()), WAIT, None).await.unwrap_err();
        assert_eq!(bad.code, ErrorCode::Invalid);
    }

    /// Waits read as `expect` does: a command that printed and ended before the wait began
    /// still counts, and what a wait reports is consumed, so it does not end a second one.
    #[tokio::test]
    async fn output_before_the_wait_counts_and_a_match_is_consumed() {
        let sh = shell().await;
        let h = &sh.handle;
        send_input(h, &text("echo one; echo two\n")).await.unwrap();
        until_commands(h, |c| c.iter().any(|c| c.exit.is_some())).await;
        let done = wait_for(h, &WaitUntil::CommandDone, WAIT, None).await.unwrap();
        assert_eq!(done, Waited::Met { line: None }, "it ended before the wait began");
        let short = Duration::from_millis(200);
        let again = wait_for(h, &WaitUntil::CommandDone, short, None).await.unwrap();
        assert_eq!(again, Waited::TimedOut, "that command was reported already");
        let one = WaitUntil::Output("^one$".to_owned());
        let Waited::Met { line: Some(first) } = wait_for(h, &one, WAIT, None).await.unwrap() else {
            panic!("the finished command's output is found");
        };
        assert_eq!(first.text, "one");
        assert_eq!(wait_for(h, &one, short, None).await.unwrap(), Waited::TimedOut, "consumed");
        let two = WaitUntil::Output("^two$".to_owned());
        let Waited::Met { line: Some(second) } = wait_for(h, &two, WAIT, None).await.unwrap()
        else {
            panic!("the line after the match is still unread");
        };
        assert_eq!(second.index, first.index.saturating_add(1));
    }

    #[tokio::test]
    async fn waits_time_out_and_an_exit_is_seen() {
        let mut sh = shell().await;
        let h = &sh.handle;
        let short = Duration::from_millis(200);
        let never = WaitUntil::Output("never printed".to_owned());
        assert_eq!(wait_for(h, &never, short, None).await.unwrap(), Waited::TimedOut);
        let exit = wait_for(h, &WaitUntil::Exit, WAIT, None);
        let bye = async { send_input(h, &text("exit\n")).await.unwrap() };
        let (exit, ()) = tokio::join!(exit, bye);
        assert_eq!(exit.unwrap(), Waited::Met { line: None });
        let _status = sh.child.wait().await.unwrap();
        let never = WaitUntil::Output("never printed".to_owned());
        let after = wait_for(h, &never, WAIT, None).await.unwrap();
        assert_eq!(after, Waited::Closed, "an exited program writes nothing more");
    }

    #[tokio::test]
    async fn an_agent_blocked_on_a_human_ends_the_wait() {
        let sh = shell().await;
        let h = &sh.handle;
        let (events, rx) = broadcast::channel(8);
        let feed = AgentFeed { events: rx, agents: Table::with(AgentStatus::Working) };
        let waiting = wait_for(h, &WaitUntil::AgentNeedsInput, WAIT, Some(feed));
        let event = agent_event;
        let report = async {
            let other = SessionId::new();
            events.send(event(other, AgentStatus::Idle)).unwrap();
            events.send(event(h.id(), AgentStatus::Working)).unwrap();
            let why = BlockReason::Permission { tool: "Bash".to_owned() };
            events.send(event(h.id(), AgentStatus::Blocked(why))).unwrap();
        };
        let (waited, ()) = tokio::join!(waiting, report);
        assert_eq!(waited.unwrap(), Waited::Met { line: None });

        let (_events, rx) = broadcast::channel::<WorkerMsg>(8);
        let blocked = AgentStatus::Blocked(BlockReason::Question);
        let feed = AgentFeed { events: rx, agents: Table::with(blocked) };
        let at_once = wait_for(h, &WaitUntil::AgentNeedsInput, WAIT, Some(feed)).await;
        assert_eq!(at_once.unwrap(), Waited::Met { line: None }, "already blocked");
    }

    /// A wait that falls behind the daemon's events may have missed the very report it waits
    /// for; it reads the status the missed reports left in the table.
    #[tokio::test]
    async fn a_wait_that_fell_behind_the_events_reads_the_status_they_left() {
        let sh = shell().await;
        let h = &sh.handle;
        let (events, rx) = broadcast::channel(2);
        let table = Table::with(AgentStatus::Working);
        let feed = AgentFeed { events: rx, agents: std::sync::Arc::<Table>::clone(&table) };
        // The agent finished its turn; the report is pushed out by other sessions' reports
        // before the wait reads any.
        *table.0.lock() = Some(AgentStatus::Idle);
        events.send(agent_event(h.id(), AgentStatus::Idle)).unwrap();
        for _ in 0..3 {
            events.send(agent_event(SessionId::new(), AgentStatus::Working)).unwrap();
        }
        let waited = wait_for(h, &WaitUntil::AgentNeedsInput, Duration::from_secs(2), Some(feed));
        assert_eq!(waited.await.unwrap(), Waited::Met { line: None });

        let (events, rx) = broadcast::channel(2);
        let feed = AgentFeed { events: rx, agents: Table::with(AgentStatus::Working) };
        for _ in 0..3 {
            events.send(agent_event(SessionId::new(), AgentStatus::Idle)).unwrap();
        }
        let still =
            wait_for(h, &WaitUntil::AgentNeedsInput, Duration::from_millis(300), Some(feed));
        assert_eq!(still.await.unwrap(), Waited::TimedOut, "still working: the wait goes on");
    }

    #[tokio::test]
    async fn a_listener_in_the_session_is_found_with_its_port() {
        let sh = shell().await;
        let h = &sh.handle;
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        send_input(h, &text(&format!("nc -l 127.0.0.1 {port}\n"))).await.unwrap();
        let root = (h.id(), sh.child.id().unwrap());
        // nc says nothing when it listens: look until it is there.
        let deadline = tokio::time::Instant::now().checked_add(WAIT).unwrap();
        let found = loop {
            let ports = slopty_worker::ports::listening(&[root]);
            if let Some(p) = ports.into_iter().find(|p| p.number == port) {
                break p;
            }
            assert!(tokio::time::Instant::now() < deadline, "nc never listened on {port}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(found.process, "nc");
        assert_eq!(found.session, Some(h.id()));
        assert_ne!(found.pid, root.1, "a child of the shell, not the shell");
        send_input(h, &Input::Keys(vec!["ctrl+c".to_owned()])).await.unwrap();
    }

    /// The CPU time this process has spent, all its threads together: what a walk costs. A
    /// loaded machine stretches a walk's wall time by however long it is held off the cores,
    /// and not the work it does. nextest runs each test in a process of its own, so the count
    /// is this test's alone.
    fn cpu() -> Duration {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage(2) (<sys/resource.h>) writes one `struct rusage` through its second
        // argument, which points at one.
        let got = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        assert_eq!(got, 0, "getrusage");
        // SAFETY: a zeroed `rusage` is valid (plain integers), and the call filled it.
        let usage = unsafe { usage.assume_init() };
        let time = |t: libc::timeval| {
            Duration::from_secs(u64::try_from(t.tv_sec).unwrap())
                .saturating_add(Duration::from_micros(u64::try_from(t.tv_usec).unwrap()))
        };
        time(usage.ru_utime).saturating_add(time(usage.ru_stime))
    }

    /// A `Search` whose request is dropped (the server's link went, or the task answering it
    /// was aborted) stops walking at once, rather than walking the rest of the tree for nobody:
    /// the one blocking thread it ran on is free again long before a whole walk would end.
    ///
    /// Judged in CPU time, the work done, against a whole walk's in the same run. In wall time
    /// the walk and the drop are each stretched by whatever else holds the cores at that
    /// moment, and a loaded runner once timed a drop at half a whole walk.
    #[test]
    fn dropping_a_search_stops_its_walk() {
        use slopty_proto::search::{MAX_LINES, SearchQuery};
        use slopty_worker::orchestrate::search;

        let dir = tempfile::tempdir().unwrap();
        for d in 0..64 {
            let sub = dir.path().join(format!("d{d}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..200 {
                std::fs::write(sub.join(format!("f{f}.txt")), "hay\n".repeat(64)).unwrap();
            }
        }
        let query = SearchQuery { pattern: "needle".to_owned(), ..SearchQuery::default() };
        let forever = Duration::from_secs(600);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let root = dir.path().to_path_buf();
            let started = cpu();
            let (_, summary) =
                search(root.clone(), query.clone(), MAX_LINES, forever).await.unwrap();
            let whole = cpu().saturating_sub(started);
            let (tenth, third) = (whole.checked_div(10).unwrap(), whole.checked_div(3).unwrap());
            assert_eq!(summary.searched, 64 * 200);

            // Dropped where it is polled, a tenth of the way through by the work done: the drop
            // is the request's end, with no scheduler between it and the stop.
            let mut walk = Box::pin(search(root, query, MAX_LINES, forever));
            let begun = cpu();
            while cpu().saturating_sub(begun) < tenth {
                tokio::select! {
                    ended = walk.as_mut() => panic!("the walk ended within a tenth: {ended:?}"),
                    () = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
            drop(walk);
            let dropped = cpu();
            // Queued behind the search on the pool's only thread: it runs once the walk ends.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            let after = cpu().saturating_sub(dropped);
            eprintln!(
                "a whole walk took {whole:?} of CPU; the dropped one {after:?} after its drop"
            );
            assert!(after < third, "{after:?} against {whole:?}");
        });
    }

    /// A terminal with an agent takes input only while the agent can: not before its hooks
    /// speak, not while it asks a person, not while a person has a line typed and unsent, not
    /// once it ended. A terminal with none takes anything.
    #[tokio::test]
    async fn nothing_is_typed_into_an_agent_that_cannot_take_it() {
        use slopty_core::ClientId;
        use slopty_proto::terminal::TermRequest;

        let sh = shell().await;
        let h = &sh.handle;
        let said = Said::default();
        let refused = |expects_agent| may_type(h, &said, expects_agent).err();
        let code = |expects_agent| refused(expects_agent).map(|f| f.code);
        assert_eq!(code(false), None, "no agent, no guard");
        assert_eq!(code(true), Some(ErrorCode::AgentNotReady), "started as one, not seen yet");
        for source in [AgentSource::Process, AgentSource::Title, AgentSource::Transcript] {
            said.agent(AgentStatus::Idle, source);
            assert_eq!(code(false), Some(ErrorCode::AgentNotReady), "{source:?}: no hook yet");
        }
        said.agent(AgentStatus::Idle, AgentSource::Hook);
        assert_eq!(code(true), None);
        said.agent(
            AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
            AgentSource::Hook,
        );
        let blocked = refused(false).expect("blocked on a permission");
        assert_eq!(blocked.code, ErrorCode::AwaitsPerson);
        assert!(blocked.message.contains("the person's to answer"), "{blocked:?}");
        said.agent(AgentStatus::Blocked(BlockReason::Question), AgentSource::Hook);
        assert_eq!(code(false), Some(ErrorCode::AwaitsPerson));
        said.agent(AgentStatus::Blocked(BlockReason::IdlePrompt), AgentSource::Hook);
        assert_eq!(code(false), None, "idle at its prompt is not waiting on anyone");
        said.agent(AgentStatus::Working, AgentSource::Hook);
        assert_eq!(code(false), None);

        let person = ClientId::new();
        h.request(person, TermRequest::Raw(b"# half a line".to_vec())).unwrap();
        let typing = refused(false).expect("a person's draft");
        assert_eq!(typing.code, ErrorCode::AwaitsPerson);
        assert!(typing.message.contains("typing"), "{typing:?}");
        send_input(h, &text("# orchestration's own line\n")).await.unwrap();
        assert!(h.draft_pending(), "orchestration's Enter does not send the person's line");
        let enter = slopty_worker::orchestrate::keys::parse("enter", 1).unwrap();
        h.request(person, TermRequest::Key(enter)).unwrap();
        assert_eq!(code(false), None, "sent");

        said.end();
        assert_eq!(code(false), Some(ErrorCode::AgentExited), "its shell would read it");
    }

    /// What is kept for an agent the person stopped waits for their next prompt: its idle
    /// prompt a minute later takes typing as before, but no post wakes it, until it is prompted
    /// again.
    #[tokio::test]
    async fn the_person_s_stop_holds_what_is_kept_for_the_agent() {
        let sh = shell().await;
        let stopped = Stopped(parking_lot::Mutex::new(true));
        assert_eq!(may_type(&sh.handle, &stopped, true).err(), None, "the person may type");
        let held = may_deliver(&sh.handle, &stopped).expect_err("held");
        assert_eq!(held.code, ErrorCode::AwaitsPerson);
        assert!(held.message.contains("the person stopped this agent"), "{held:?}");
        *stopped.0.lock() = false;
        assert_eq!(may_deliver(&sh.handle, &stopped).err(), None, "prompted again");
    }
}
