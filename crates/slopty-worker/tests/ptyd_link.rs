//! The worker's session table against a real `slopty-ptyd`: what it does with each refusal.

#[cfg(test)]
mod ptyd_link {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_core::SessionId;
    use slopty_proto::agent::SessionAgent;
    use slopty_proto::terminal::{OpenSession, TermSize};
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

    /// `slopty-ptyd` from this build's profile directory, built on demand: `-p slopty-worker`
    /// alone does not build it.
    fn ptyd_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let profile = exe.parent().and_then(Path::parent).unwrap();
        let path = profile.join("slopty-ptyd");
        if !path.exists() {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut build = std::process::Command::new(cargo);
            build.args(["build", "-p", "slopty-ptyd", "--bin", "slopty-ptyd"]);
            if profile.ends_with("release") {
                build.arg("--release");
            }
            assert!(build.status().unwrap().success(), "build slopty-ptyd");
        }
        path
    }

    /// A ptyd on its own socket in `dir`, killed when the child drops.
    async fn ptyd(dir: &Path) -> (Child, PathBuf) {
        let socket = dir.join("ptyd.sock");
        let child = Command::new(ptyd_bin())
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
            Worker::connect(Some(socket), Arc::new(NoAgents)).await.unwrap();
        assert!(worker.get(id).is_err(), "not the worker's while the older one holds it");

        drop(held);
        drop(older);
        let announced = tokio::time::timeout(WAIT, reports.moves.recv()).await.unwrap();
        assert_eq!(announced, Some(id), "the adoption is announced");
        assert!(worker.get(id).is_ok(), "adopted once the older worker let go");
        worker.close(id).await.unwrap();
    }

    /// Closing a session ptyd has already forgotten is done, not a failure.
    #[tokio::test]
    async fn closing_a_session_ptyd_already_forgot_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let (_ptyd, socket) = ptyd(dir.path()).await;
        let (worker, _reports) =
            Worker::connect(Some(socket.clone()), Arc::new(NoAgents)).await.unwrap();
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
