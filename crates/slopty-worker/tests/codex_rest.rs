//! What the person's Codex daemon keeps for threads that rest, kept followed and let go
//! (`thread/unsubscribe`), side by side (`docs/MEASUREMENTS.md`). Each side runs the real
//! `codex app-server` under a `CODEX_HOME` of its own, signed in to nothing, with Slopty's own
//! MCP relay (`slopty mcp`) as the one MCP server every thread loads, and starts threads with no
//! turn, so no model is asked. Codex unloads a thread only once it has had no subscriber and no
//! activity for `thread_unload_delay_secs`, set to [`DELAY`] here so the run is short. Run by hand:
//! `cargo nextest run -p slopty-worker --test codex_rest --run-ignored only --no-capture`.

#[cfg(test)]
mod codex_rest {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_proto::thread::wire::{Outcome, Start};
    use slopty_proto::thread::{AgentId, IntentId};
    use slopty_worker::thread::Host;
    use slopty_worker::thread::codex::{self, Codex};
    use slopty_worker::thread::log::Limits;

    /// Threads started and left to rest, on each side.
    const THREADS: usize = 10;
    /// How long a thread rests before the worker lets it go, on the side that does.
    const REST: Duration = Duration::from_secs(3);
    /// Codex's wait before it unloads a thread with no subscriber and no activity, in seconds.
    const DELAY: u64 = 10;
    /// How long what is kept is watched.
    const UNLOAD: Duration = Duration::from_secs(45);
    /// How often what is kept is counted.
    const EVERY: Duration = Duration::from_secs(5);

    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
    }

    /// One side: a daemon, the worker's Codex threads on it, and its relays' marker.
    struct Side {
        daemon: tokio::process::Child,
        pid: u32,
        marker: String,
        _codex: Codex,
    }

    impl Side {
        /// A daemon under `root`, whose threads the worker lets go after `rest`, with
        /// [`THREADS`] threads started.
        async fn new(root: &Path, rest: Duration) -> Self {
            let (home, data) = (root.join("codex"), root.join("data"));
            let socket = root.join("s.sock");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&data).unwrap();
            let config = format!(
                "thread_unload_delay_secs = {DELAY}\n[mcp_servers.slopty]\ncommand = {:?}\nargs = [\"--data-dir\", {:?}, \
                 \"--server\", \"127.0.0.1:9\", \"mcp\"]\n",
                bin("slopty").to_string_lossy(),
                data.to_string_lossy(),
            );
            std::fs::write(home.join("config.toml"), config).unwrap();
            let mut daemon = tokio::process::Command::new("codex")
                .args(["app-server", "--listen", &format!("unix://{}", socket.display())])
                .env("CODEX_HOME", &home)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("codex is on the PATH");
            let pid = daemon.id().unwrap();
            while tokio::net::UnixStream::connect(&socket).await.is_err() {
                assert!(daemon.try_wait().unwrap().is_none(), "codex app-server exited");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let host = Host::open(&root.join("threads"), Limits::default()).unwrap();
            let (handle, asks) = Codex::channel();
            drop(codex::spawn(host, socket, None, asks.rest_after(rest)));
            for n in 0..THREADS {
                let work = root.join(format!("work-{n}"));
                std::fs::create_dir_all(&work).unwrap();
                let start = Start {
                    agent: AgentId::named(AgentId::CODEX),
                    cwd: work.to_string_lossy().into_owned(),
                    drive: None,
                    prompt: None,
                    model: None,
                    args: Vec::new(),
                };
                let outcome = handle.start(IntentId::new(), start).await;
                assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
            }
            Self { daemon, pid, marker: data.to_string_lossy().into_owned(), _codex: handle }
        }

        /// The MCP relays running for it, their resident size and the daemon's, in KiB.
        fn kept(&self) -> (usize, u64, u64) {
            let listed =
                std::process::Command::new("pgrep").args(["-f", &self.marker]).output().unwrap();
            let relays: Vec<String> =
                String::from_utf8_lossy(&listed.stdout).lines().map(str::to_owned).collect();
            let rss = |pid: &str| -> u64 {
                let ps = std::process::Command::new("ps").args(["-o", "rss=", "-p", pid]).output();
                ps.map_or(0, |ps| String::from_utf8_lossy(&ps.stdout).trim().parse().unwrap_or(0))
            };
            let theirs = relays.iter().map(|pid| rss(pid)).sum();
            (relays.len(), theirs, rss(&self.pid.to_string()))
        }
    }

    #[tokio::test]
    #[ignore = "measurement against the person's own codex, about a minute, run by hand"]
    async fn what_rested_codex_threads_keep_followed_and_let_go() {
        let root = tempfile::Builder::new().prefix("slopty-rest").tempdir_in("/tmp").unwrap();
        let root = root.path().canonicalize().unwrap();
        let (kept_dir, let_dir) = (root.join("k"), root.join("l"));
        let mut followed = Side::new(&kept_dir, Duration::from_hours(24)).await;
        let mut let_go = Side::new(&let_dir, REST).await;
        let started = tokio::time::Instant::now();
        let mut last = (0, 0);
        while started.elapsed() < UNLOAD {
            tokio::time::sleep(EVERY).await;
            let ((kept, kept_mcp, kept_rss), (gone, gone_mcp, gone_rss)) =
                (followed.kept(), let_go.kept());
            let secs = started.elapsed().as_secs();
            eprintln!(
                "{secs:>3} s: followed {kept} relays ({kept_mcp} KiB), daemon {kept_rss} KiB; \
                 let go {gone} relays ({gone_mcp} KiB), daemon {gone_rss} KiB"
            );
            last = (kept, gone);
        }
        drop(followed.daemon.kill().await);
        drop(let_go.daemon.kill().await);
        assert_eq!(last, (THREADS, 0), "Codex unloads what is let go, and only that");
    }
}
