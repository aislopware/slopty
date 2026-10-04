//! A `claude` typed in a Slopty shell is the person's own Claude Code wired as one Slopty starts:
//! the real `slopty hook wire` answers the shell integration's `claude` in zsh, both bashes and
//! fish, each on a terminal as ptyd starts it. `claude` is a stand-in that writes down the
//! arguments and the switch it was started with.

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::Value;
    use slopty_agent::HOOK_EVENTS;
    use slopty_agent::hooks::has_relay;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;
    use slopty_pty::shell_integration::{self, ShellIntegration};
    use slopty_pty::{Pty, PtyMaster, SpawnSpec};

    /// The shells on this Mac: zsh, both bashes (macOS's 3.2 and Homebrew's) and fish.
    fn shells() -> Vec<&'static str> {
        ["/bin/zsh", "/bin/bash", "/opt/homebrew/bin/bash", "/opt/homebrew/bin/fish"]
            .into_iter()
            .filter(|p| Path::new(p).is_file())
            .collect()
    }

    /// The `slopty` this test runs, as the relay names it.
    fn relay() -> String {
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_slopty"));
        std::fs::canonicalize(&exe).unwrap_or(exe).to_string_lossy().into_owned()
    }

    /// A shell's home, its `claude` stand-in and the mod's directory.
    struct Place {
        dir: tempfile::TempDir,
    }

    impl Place {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            for sub in ["bin", "home/project", "mod", "shell"] {
                std::fs::create_dir_all(dir.path().join(sub)).unwrap();
            }
            let ran = dir.path().join("ran");
            let claude = dir.path().join("bin/claude");
            std::fs::write(
                &claude,
                format!(
                    "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{ran}.args'\n\
                     printf '%s' \"${{CLAUDE_CODE_ENABLE_FUNCTION_HOOKS-unset}}\" > '{ran}.hooks'\n\
                     pwd > '{ran}.cwd'\n",
                    ran = ran.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir }
        }

        fn path(&self, sub: &str) -> PathBuf {
            self.dir.path().join(sub)
        }

        /// What the stand-in was started with: its arguments and the mod's switch.
        fn ran(&self) -> (Vec<String>, String) {
            let args = std::fs::read(self.path("ran.args")).expect("claude ran");
            let args = args
                .split(|b| *b == 0)
                .map(|w| String::from_utf8_lossy(w).into_owned())
                .collect::<Vec<_>>();
            let args = args.split_last().map(|(_, a)| a.to_vec()).unwrap_or_default();
            let hooks = std::fs::read_to_string(self.path("ran.hooks")).expect("claude ran");
            (args, hooks)
        }

        /// Type `line` into `shell` started as ptyd starts it, in a Slopty session served by a
        /// server, then `exit`; what the terminal showed.
        async fn run(&self, shell: &str, line: &str) -> String {
            let si = shell_integration::install(&self.path("shell")).unwrap();
            let si = ShellIntegration {
                original_zdotdir: None,
                original_xdg_data_dirs: None,
                enabled: true,
                cli: Some(PathBuf::from(env!("CARGO_BIN_EXE_slopty"))),
                bin: None,
                own_browser: false,
                own_editor: false,
                ..si
            };
            let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
            let lossy = |p: PathBuf| p.to_string_lossy().into_owned();
            let env = vec![
                pair("HOME", &lossy(self.path("home"))),
                pair("PATH", &format!("{}:/usr/bin:/bin", self.path("bin").display())),
                pair("TERM", "xterm-256color"),
                pair("BASH_SILENCE_DEPRECATION_WARNING", "1"),
                pair(slopty_proto::ctl::SESSION_ENV, &slopty_core::SessionId::new().to_string()),
                pair(slopty_proto::project::SERVER_ENV, "server.tail:7480"),
                pair(slopty_agent::claude_mod::DIR_ENV, &lossy(self.path("mod"))),
                pair(slopty_agent::claude_mod::SOCKET_ENV, &lossy(self.path("mod.sock"))),
                pair("SLOPTY_NO_CLAUDE_MOD", "0"),
            ];
            let size = TermSize { cols: 120, rows: 20, metrics: CellMetrics::default() };
            let pty = Pty::open(size).unwrap();
            let spec = SpawnSpec {
                command: vec![shell.to_owned(), "-i".to_owned()],
                cwd: Some(self.path("home")),
                env,
                size,
            };
            let mut child = pty.spawn_with(&spec, Some(&si)).unwrap().child;
            let master = PtyMaster::new(pty.into_master()).unwrap();
            master.write_all(format!("{line}\nexit\n").as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let mut buf = [0_u8; 4096];
            let mut answered = 0;
            let deadline =
                tokio::time::Instant::now().checked_add(Duration::from_secs(30)).unwrap();
            loop {
                let Ok(Ok(n)) = tokio::time::timeout_at(deadline, master.read(&mut buf)).await
                else {
                    let _killed = child.start_kill();
                    break;
                };
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                // fish 4 asks the terminal who it is before its first prompt.
                let asked = out.windows(4).filter(|w| *w == b"\x1b[0c" || *w == b"\x1b[6n").count();
                for _ in answered..asked {
                    master.write_all(b"\x1b[?62;22c\x1b[1;1R").await.unwrap();
                }
                answered = asked;
            }
            let _status = child.wait().await;
            String::from_utf8_lossy(&out).into_owned()
        }
    }

    /// Typed in a Slopty shell, `claude` starts with the relay's hooks and status line on the
    /// one `--settings`, the person's own settings merged in, the pointer to Slopty's CLI
    /// rather than its tools (it is no project's agent), a pinned conversation and the mod with
    /// its switch; the person's flags and prompt follow as typed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_typed_claude_is_wired_as_one_slopty_starts() {
        let relay = relay();
        for shell in shells() {
            let place = Place::new();
            let line = r#"cd project && claude --model opus --settings '{"theme":"dark"}' 'fix the "bug"'"#;
            let shown = place.run(shell, line).await;
            let (args, hooks) = place.ran();
            assert_eq!(hooks, "1", "{shell}: the mod's switch\n{shown}");
            let module = format!("--plugin-dir={}", place.path("mod").display());
            assert_eq!(args.first(), Some(&module), "{shell}: {args:?}");
            let tail = ["--model", "opus", r#"fix the "bug""#].map(str::to_owned);
            assert!(args.ends_with(&tail), "{shell}: the person's words last: {args:?}");
            let at = |flag: &str| args.iter().position(|a| a == flag);
            let session = at("--session-id").and_then(|i| args.get(i + 1)).expect("pinned");
            assert!(uuid::Uuid::parse_str(session).is_ok(), "{shell}: {session}");
            assert_eq!(args.iter().filter(|a| *a == "--settings").count(), 1, "{shell}: {args:?}");
            let settings = at("--settings").and_then(|i| args.get(i + 1)).expect("settings");
            let settings: Value = serde_json::from_str(settings).unwrap();
            assert_eq!(settings["theme"], "dark", "{shell}: the person's settings kept");
            assert!(HOOK_EVENTS.into_iter().all(|e| has_relay(&settings, e)), "{shell}");
            let hook = &settings["hooks"]["Stop"][0]["hooks"][0]["command"];
            assert!(hook.as_str().is_some_and(|c| c.contains(&relay)), "{shell}: {hook}");
            let line = settings["statusLine"]["command"].as_str().unwrap_or_default();
            assert!(line.contains(&relay), "{shell}: the status-line wrapper: {line}");
            assert!(!args.iter().any(|a| a.starts_with("--mcp-config")), "{shell}: {args:?}");
            let pointer = format!("--append-system-prompt={}", slopty_agent::hooks::POINTER);
            assert!(args.contains(&pointer), "{shell}: {args:?}");
            let cwd = std::fs::read_to_string(place.path("ran.cwd")).unwrap();
            assert_eq!(
                Path::new(cwd.trim()).canonicalize().unwrap(),
                place.path("home/project").canonicalize().unwrap()
            );
        }
    }

    /// A run that prints and exits keeps its words and gets only the mod, as one the worker
    /// starts gets nothing of its relay.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_typed_print_run_gets_only_the_mod() {
        for shell in shells() {
            let place = Place::new();
            let shown = place.run(shell, "claude -p 'say hi'").await;
            let (args, hooks) = place.ran();
            let module = format!("--plugin-dir={}", place.path("mod").display());
            assert_eq!(args, [module.as_str(), "-p", "say hi"], "{shell}\n{shown}");
            assert_eq!(hooks, "1", "{shell}");
        }
    }
}
