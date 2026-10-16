//! One worker per data directory: a second worker started on the directory a first one serves
//! from ends at once with its own exit code, before it touches anything there, and the first
//! goes on serving. The lock goes with the process that held it, however it ended.

#[cfg(test)]
mod lock {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::process::{Child, Command};

    /// The exit code of a worker whose data directory another holds (`HELD_ELSEWHERE`).
    const HELD_ELSEWHERE: i32 = 75;

    /// Slack for a loaded machine: each step is a process starting, not a wait.
    const STEP: Duration = Duration::from_secs(30);

    /// A binary of this build (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), name)
    }

    /// `program`, from a clean environment whose home is `dir`'s.
    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, dir: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), &dir.join("home"));
        command.stderr(Stdio::inherit()).process_group(0).kill_on_drop(true);
        command
    }

    /// The daemons, killed with the test.
    struct Children(Vec<Child>);

    impl Drop for Children {
        fn drop(&mut self) {
            let ended = slopty_testkit::group::end(&mut self.0, Child::id, |child| {
                child.try_wait().is_ok_and(|status| status.is_some())
            });
            debug_assert!(ended, "a daemon's group outlived the test");
        }
    }

    /// ptyd on `dir`'s socket, once it listens.
    async fn ptyd(dir: &Path) -> Child {
        let socket = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), dir)
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(ptyd.id());
        while tokio::net::UnixStream::connect(&socket).await.is_err() {
            assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        ptyd
    }

    /// A worker on `dir`'s ptyd and data directory, its control socket `ctl`.
    fn worker(dir: &Path, ctl: &str) -> Child {
        let worker = scrubbed(bin("slopty-worker"), dir)
            .arg("--ptyd-socket")
            .arg(dir.join("ptyd.sock"))
            .arg("--ctl-socket")
            .arg(dir.join(ctl))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--print-addr", "--port", "0"])
            .env("SLOPTY_PASTEBOARD", format!("dev.aislopware.slopty.lock.{ctl}"))
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(worker.id());
        worker
    }

    /// The address `worker` prints once it serves.
    async fn serving(worker: &mut Child) -> String {
        let mut line = String::new();
        let stdout = worker.stdout.as_mut().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("the worker prints its address")
            .unwrap();
        assert!(!line.trim().is_empty(), "it exited first: {:?}", worker.try_wait());
        line.trim().to_owned()
    }

    /// The worker's `doctor` answers on its control socket `ctl`: it serves.
    async fn answers(ctl: &Path) -> bool {
        let Ok(stream) = tokio::net::UnixStream::connect(ctl).await else { return false };
        let (rd, mut wr) = stream.into_split();
        let mut line = serde_json::to_vec(&slopty_proto::ctl::CtlRequest::Doctor).unwrap();
        line.push(b'\n');
        if wr.write_all(&line).await.is_err() {
            return false;
        }
        let _closed = wr.shutdown().await;
        let mut reply = String::new();
        let read = BufReader::new(rd).read_line(&mut reply).await;
        read.is_ok_and(|n| n > 0)
            && matches!(
                serde_json::from_str(reply.trim()),
                Ok(slopty_proto::ctl::CtlReply::Doctor(_))
            )
    }

    /// A second worker on the directory the first serves from exits with its own code within a
    /// moment, having opened no control socket of its own, and the first still answers. Once the
    /// first is killed outright (no chance to let anything go), a third takes the directory.
    #[tokio::test]
    async fn a_second_worker_on_the_same_data_dir_ends_with_its_own_code() {
        let dir = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(dir.path()).unwrap();
        let mut children = Children(vec![ptyd(&dir).await]);
        let mut first = worker(&dir, "first.sock");
        serving(&mut first).await;
        children.0.push(first);
        assert!(answers(&dir.join("first.sock")).await, "the first serves");

        let began = Instant::now();
        let mut second = worker(&dir, "second.sock");
        let status = tokio::time::timeout(STEP, second.wait()).await.expect("it ends").unwrap();
        let took = began.elapsed();
        println!("the second worker ended in {took:?}");
        assert_eq!(status.code(), Some(HELD_ELSEWHERE), "{status}");
        assert!(!dir.join("second.sock").exists(), "it opened nothing");
        assert!(answers(&dir.join("first.sock")).await, "the first still serves");

        let mut first = children.0.pop().unwrap();
        first.start_kill().unwrap();
        first.wait().await.unwrap();
        let mut third = worker(&dir, "third.sock");
        serving(&mut third).await;
        children.0.push(third);
        assert!(answers(&dir.join("third.sock")).await, "a lock never outlives its worker");
    }
}
