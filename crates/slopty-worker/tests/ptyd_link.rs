//! The worker's session table against a real `slopty-ptyd`: what it does with each refusal.

#[cfg(test)]
mod ptyd_link {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_agent::status::SessionAgent;
    use slopty_core::{ClientId, SessionId};
    use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};
    use slopty_pty::{PtydClient, SpawnSpec};
    use slopty_worker::Worker;
    use slopty_worker::orchestrate::Agents;
    use tokio::process::{Child, Command};

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
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.path().join("kept"))
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
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.path().join("kept"))
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

    /// Closing a session ptyd has already forgotten is done, not a failure.
    #[tokio::test]
    async fn closing_a_session_ptyd_already_forgot_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (worker, _reports) =
            Worker::connect(Some(socket.clone()), Arc::new(NoAgents), &dir.path().join("kept"))
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
}
