//! A terminal opened on `claude` (⌘⇧T, a tile's command) against a real `slopty-ptyd` and the
//! stub agent: the worker starts it the way it starts an agent of its own.

#[cfg(test)]
mod agent_open {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;
    use slopty_agent::status::SessionAgent;
    use slopty_core::SessionId;
    use slopty_proto::terminal::{OpenSession, TermSize};
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

    /// `name` from this build (`slopty_testkit::bins`), found from the profile directory: this
    /// package has no binary of its own to name.
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
    }

    /// A ptyd on its own socket in `dir`, with the stub as `claude` first on its `PATH`.
    async fn ptyd(dir: &Path) -> (Child, PathBuf) {
        let programs = dir.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        // It records whatever it was started with, so it stands in for `codex` too.
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("codex")).unwrap();
        let socket = dir.join("ptyd.sock");
        let mut child = Command::new(bin("slopty-ptyd"));
        slopty_testkit::env::scrub(child.as_std_mut(), &dir.join("home"));
        let child = child
            .env("PATH", slopty_testkit::env::path_with(&programs))
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

    /// Open `command` and read what the stub was started with.
    async fn started(worker: &Worker, dir: &Path, command: &[&str]) -> Value {
        started_in(worker, dir, command, None).await
    }

    /// [`started`], as `project`'s agent when one is named.
    async fn started_in(
        worker: &Worker,
        dir: &Path,
        command: &[&str],
        project: Option<&str>,
    ) -> Value {
        let record = dir.join(format!("record-{}.json", SessionId::new()));
        let mut env = vec![("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned())];
        if let Some(project) = project {
            env.push((slopty_proto::project::PROJECT_ENV.to_owned(), project.to_owned()));
        }
        let open = OpenSession {
            size: TermSize::default(),
            cwd: Some(dir.to_string_lossy().into_owned()),
            command: command.iter().map(|&w| w.to_owned()).collect(),
            env,
            title: None,
            attach: false,
        };
        worker.open(&open).await.unwrap();
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(seen) = std::fs::read(&record)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                {
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the stub's record")
    }

    fn argv(seen: &Value) -> Vec<String> {
        seen["argv"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    /// A bare `claude` gets the relay, so its status and its permission prompts reach the app,
    /// the pointer to Slopty's CLI rather than its tools (it is no project's agent), the mod,
    /// and a conversation id of its own, and the person's mode is not locked. One already wired, a
    /// `--print` run, and a worker with no relay to hand out start as asked.
    #[tokio::test]
    async fn claude_opened_in_a_tile_is_started_as_slopty_starts_its_agents() {
        let dir = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(dir.path()).unwrap();
        let (_ptyd, socket) = ptyd(&dir).await;
        let (worker, _reports) =
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.join("kept"), None)
                .await
                .unwrap();
        let mod_dir = dir.join("claude-mod/abc");
        std::fs::create_dir_all(&mod_dir).unwrap();
        worker.set_session_env(vec![
            (slopty_proto::project::SERVER_ENV.to_owned(), "127.0.0.1:9".to_owned()),
            (slopty_agent::claude_mod::DIR_ENV.to_owned(), mod_dir.to_string_lossy().into_owned()),
            (slopty_agent::claude_mod::SOCKET_ENV.to_owned(), "/tmp/mod.sock".to_owned()),
        ]);
        // A relay that answers every hook with nothing, under the name the hooks know it by.
        let relay = dir.join("relay/slopty");
        std::fs::create_dir_all(relay.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", &relay).unwrap();
        worker.set_relay(Some(relay.clone()));

        let seen = started(&worker, &dir, &["claude", "--model", "opus"]).await;
        let args = argv(&seen);
        let kept = slopty_agent::resume::invocation(&args);
        assert!(kept.relay && !kept.mcp && !kept.locked, "{args:?}");
        assert_eq!(kept.role.as_deref(), Some(slopty_agent::hooks::POINTER), "{args:?}");
        assert_eq!(kept.args, ["--model", "opus"], "its own flags kept");
        let pinned = args.iter().position(|a| a == "--session-id").expect("a conversation id");
        assert!(uuid_like(&args[pinned + 1]), "{args:?}");
        let plugin = format!("--plugin-dir={}", mod_dir.display());
        assert!(args.contains(&plugin), "the mod: {args:?}");
        assert_eq!(seen["env"][slopty_agent::claude_mod::SOCKET_ENV], "/tmp/mod.sock");

        let mut wired = slopty_agent::hooks::with_relay(Vec::new(), &relay.to_string_lossy(), &dir);
        wired.insert(0, "claude".to_owned());
        let words: Vec<&str> = wired.iter().map(String::as_str).collect();
        let seen = started(&worker, &dir, &words).await;
        assert_eq!(argv(&seen), wired[1..], "wired already: as asked");
        let seen = started(&worker, &dir, &["claude", "-p", "hello"]).await;
        assert_eq!(argv(&seen), ["-p", "hello"], "a print run: as asked");

        worker.set_relay(None);
        let seen = started(&worker, &dir, &["claude"]).await;
        assert!(argv(&seen).is_empty(), "no relay to hand out: {:?}", argv(&seen));
    }

    /// A project's `codex` opened on a worker with a server gets Slopty's tools among its MCP
    /// servers, as `<relay> mcp` with the variables that name its server, project, task and
    /// terminal, its own arguments after. A `codex` the person opens in a tile, one that names
    /// Slopty's server already, and one on a worker with no server, start as asked.
    #[tokio::test]
    async fn a_project_s_codex_gets_slopty_s_tools_and_a_tile_s_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(dir.path()).unwrap();
        let (_ptyd, socket) = ptyd(&dir).await;
        let (worker, _reports) =
            Worker::connect(Some(socket), Arc::new(NoAgents), &dir.join("kept"), None)
                .await
                .unwrap();
        let relay = dir.join("relay/slopty");
        std::fs::create_dir_all(relay.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", &relay).unwrap();
        worker.set_relay(Some(relay.clone()));
        worker.set_session_env(vec![(
            slopty_proto::project::SERVER_ENV.to_owned(),
            "127.0.0.1:9".to_owned(),
        )]);

        let tile = ["codex", "--model", "o3", "Read the brief"];
        let seen = started(&worker, &dir, &tile).await;
        assert_eq!(argv(&seen), tile[1..], "a tile's Codex is the person's: as asked");
        let seen = started_in(&worker, &dir, &tile, Some("slopty")).await;
        let config = |key: &str| {
            let args = argv(&seen);
            let at = args.iter().position(|a| a.starts_with(&format!("mcp_servers.slopty.{key}=")));
            let at = at.unwrap_or_else(|| panic!("{key}: {args:?}"));
            assert_eq!(args[at - 1], "-c", "{args:?}");
            args[at].split_once('=').unwrap().1.to_owned()
        };
        assert_eq!(config("command"), format!("\"{}\"", relay.display()));
        assert_eq!(config("args"), r#"["mcp"]"#);
        let vars = config("env_vars");
        for var in ["SLOPTY_SERVER", "SLOPTY_PROJECT", "SLOPTY_TASK", "SLOPTY_SESSION_TOKEN"] {
            assert!(vars.contains(&format!("\"{var}\"")), "{vars}");
        }
        assert_eq!(argv(&seen)[6..], ["--model", "o3", "Read the brief"], "its own after");

        let own = ["codex", "-c", "mcp_servers.slopty.command=\"x\"", "go"];
        let seen = started_in(&worker, &dir, &own, Some("slopty")).await;
        assert_eq!(argv(&seen), own[1..], "wired already: as asked");
        let named = ["codex", "set mcp_servers.slopty up"];
        let seen = started_in(&worker, &dir, &named, Some("slopty")).await;
        assert_eq!(argv(&seen).len(), 7, "a prompt that names it is no config: {:?}", argv(&seen));
        worker.set_session_env(Vec::new());
        let seen = started_in(&worker, &dir, &["codex", "go"], Some("slopty")).await;
        assert_eq!(argv(&seen), ["go"], "no server to serve its tools: as asked");
    }

    fn uuid_like(word: &str) -> bool {
        word.len() == 36 && word.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    }
}
