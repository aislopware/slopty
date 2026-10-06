//! The drift guard of `slopty worker uninstall --purge`: ptyd and the worker run here as their
//! installed services would (the definitions' own programs, arguments and environment, with
//! no permission asked), on an empty data directory under a home of the test's own. A client
//! links, opens a shell, and asks for the thread table, so each daemon writes what it writes
//! at runtime. Once they have ended, the purge must leave nothing in that directory: anything
//! left is worker state that `slopty_platform::service::WORKER_STATE` does not name, and an
//! uninstall would leave behind on every machine.
//!
//! No service manager is asked: the purge's session records whatever it would be asked and
//! answers nothing, and nothing outside the test's directory is read or written.

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod purge {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_core::ClientId;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_platform::service::{self, Manager, PTYD, Session, WORKER, WorkerOpts};
    use slopty_proto::handshake::Hello;
    use slopty_proto::terminal::{OpenSession, TermSize};
    use slopty_proto::thread::wire::ThreadRequest;
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);

    /// The daemons, ended with the test; their pasteboard released.
    struct Daemons {
        children: Vec<Child>,
        pasteboard: String,
    }

    impl Drop for Daemons {
        fn drop(&mut self) {
            // Each leads a process group of its own: ended whole and waited for.
            let ended = slopty_testkit::group::end(&mut self.children, Child::id, |child| {
                child.try_wait().is_ok_and(|status| status.is_some())
            });
            debug_assert!(ended, "a daemon's group outlived the test");
            slopty_input::MacBoard::named(&self.pasteboard).release();
        }
    }

    /// A service manager that is never there: the purge asks it nothing, and would be told no.
    #[derive(Debug)]
    struct Absent;

    impl service::Runner for Absent {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<String> {
            Err(std::io::Error::other(format!("{program} {} asked of no manager", args.join(" "))))
        }
    }

    /// The directory this build's daemons are in.
    fn bin_dir() -> PathBuf {
        let worker = slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), "slopty-ptyd");
        worker.parent().expect("the daemons' directory").to_path_buf()
    }

    /// `job`'s installed service on `data`, started from a clean environment whose home is
    /// `home`: the definition's program, arguments and environment, except that it asks for
    /// no permission (`--installed`).
    fn started(job: service::Job, home: &Path, data: &Path, more: &[&str]) -> Command {
        let services = service::worker_services(&WorkerOpts::default(), &bin_dir(), data);
        let (_, svc) = services.into_iter().find(|(j, _)| *j == job).expect("the job's service");
        let mut command = Command::new(&svc.program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
            .envs(svc.env.iter().map(|(k, v)| (k, v)))
            .args(svc.args.iter().filter(|a| *a != "--installed"))
            .args(more)
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true);
        command
    }

    /// ptyd, then the worker, on `data`; the worker's address.
    async fn daemons(dir: &Path, home: &Path, data: &Path) -> (Daemons, SocketAddr) {
        std::fs::create_dir_all(data.join("run")).unwrap();
        let mut ptyd = started(PTYD, home, data, &[]).stdout(Stdio::null()).spawn().unwrap();
        let socket = service::Layout::new(data).ptyd_socket();
        let up = async {
            while tokio::net::UnixStream::connect(&socket).await.is_err() {
                assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(STEP, up).await.expect("ptyd listens");
        let leaf = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let pasteboard = format!("dev.aislopware.slopty.purge.{leaf}");
        let mut worker = started(WORKER, home, data, &["--print-addr", "--port", "0"])
            .env("SLOPTY_PASTEBOARD", &pasteboard)
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = worker.stdout.take().unwrap();
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        let read = reader.read_line(&mut line);
        tokio::time::timeout(STEP, read).await.expect("the worker's address").unwrap();
        let bound: SocketAddr = line.trim().parse().expect("an address");
        let addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()));
        (Daemons { children: vec![ptyd, worker], pasteboard }, addr)
    }

    /// Every file and directory under `dir`, relative to it, deepest last.
    fn left(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in std::fs::read_dir(&at).into_iter().flatten().flatten() {
                let path = entry.path();
                out.push(path.strip_prefix(dir).unwrap_or(&path).display().to_string());
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn a_purge_leaves_nothing_the_daemons_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let (home, data) = (dir.path().join("home"), dir.path().join("data"));
        let (daemons, addr) = daemons(dir.path(), &home, &data).await;

        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "purge".to_owned() };
        let dialing = async {
            loop {
                match connect_addr(&endpoint, addr, hello.clone()).await {
                    Err(slopty_net::NetError::Connect(why)) if why.ends_with("no answer") => {}
                    other => break other,
                }
            }
        };
        let mut worker = tokio::time::timeout(STEP, dialing).await.unwrap().unwrap();
        let spec = OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: Some("~".to_owned()),
            command: vec!["/bin/sh".to_owned()],
            env: Vec::new(),
            title: None,
            attach: false,
        };
        worker.tx.send(&ClientMsg::OpenSession { request: 1, spec }).await.unwrap();
        worker.tx.send(&ClientMsg::Thread(ThreadRequest::Table { have: None })).await.unwrap();
        let (mut opened, mut tabled) = (false, false);
        let heard = async {
            while !(opened && tabled) {
                match worker.rx.recv().await.expect("the worker's next word") {
                    WorkerMsg::SessionOpened { .. } => opened = true,
                    WorkerMsg::Threads(_) => tabled = true,
                    _ => {}
                }
            }
        };
        tokio::time::timeout(STEP, heard).await.expect("a shell and the thread table");
        endpoint.close(0_u32.into(), b"done");
        let _drained = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        drop(daemons);
        assert!(!left(&data).is_empty(), "the daemons wrote their state");

        let session = Session {
            manager: Manager::Launchd,
            definitions: home.join("Library").join("LaunchAgents"),
            home: home.clone(),
            uid: 501,
            runner: Arc::new(Absent),
        };
        let removed = service::purge_worker(&session, &data).unwrap();
        assert!(!removed.is_empty(), "it found the worker's state");
        let stray = left(&data);
        assert!(
            stray.is_empty(),
            "worker state no purge takes; name it in WORKER_STATE: {stray:?}"
        );
    }
}
