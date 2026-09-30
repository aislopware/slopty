//! The handoff commands a session puts first on its `PATH` step aside for anything that is not
//! theirs: `open` given a file, a flag or, with no worker to ask, even a web address, runs the
//! system's `open` further down `PATH` with its arguments untouched, and `slopty-editor` with no
//! worker runs `vi`. The system's commands are stubs that write down what they were given.

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::process::Command;

    use slopty_pty::shell_integration::{BIN, BROWSER_SHIM, EDITOR_SHIM, OPENER};

    /// `slopty` linked as each handoff command in `ours`, stubs for the system's `open` and `vi`
    /// in `theirs`, and `PATH` with ours first.
    struct Shell {
        dir: tempfile::TempDir,
    }

    impl Shell {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (ours, theirs) = (dir.path().join("ours"), dir.path().join("theirs"));
            std::fs::create_dir_all(&ours).unwrap();
            std::fs::create_dir_all(&theirs).unwrap();
            for name in [OPENER, EDITOR_SHIM, BROWSER_SHIM] {
                std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_slopty"), ours.join(name)).unwrap();
            }
            for name in [OPENER, "vi"] {
                let said = dir.path().join(format!("{name}-said"));
                let stub = theirs.join(name);
                std::fs::write(
                    &stub,
                    format!(
                        "#!/bin/sh\nprintf '%s|' \"$@\" > '{}'\nexit \"${{STUB_EXIT:-0}}\"\n",
                        said.display()
                    ),
                )
                .unwrap();
                std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        /// Run handoff command `name` with `args`, no worker listening; its exit code.
        fn run(&self, name: &str, args: &[&str]) -> Option<i32> {
            self.run_exiting(name, args, 0)
        }

        /// [`Self::run`], with the system's stubs exiting `code`.
        fn run_exiting(&self, name: &str, args: &[&str], code: i32) -> Option<i32> {
            let ours = self.path("ours");
            let path =
                format!("{}:{}:/usr/bin:/bin", ours.display(), self.path("theirs").display());
            slopty_testkit::env::scrub(&mut Command::new(ours.join(name)), self.dir.path())
                .args(args)
                .current_dir(self.dir.path())
                .env("PATH", path)
                .env(BIN, &ours)
                .env("SLOPTY_WORKER_SOCKET", self.path("no-worker.sock"))
                .env("STUB_EXIT", code.to_string())
                .status()
                .unwrap()
                .code()
        }

        /// What the stub for the system's `name` was given, `|`-separated; `None` if it never ran.
        fn said(&self, name: &str) -> Option<String> {
            let said = self.path(&format!("{name}-said"));
            let text = std::fs::read_to_string(&said).ok();
            let _removed = std::fs::remove_file(said);
            text
        }
    }

    /// `open` hands everything but a bare web address to the system's, as given.
    #[test]
    fn open_steps_aside_for_what_is_not_a_web_page() {
        let shell = Shell::new();
        for args in [
            &["README.md"][..],
            &["-a", "Safari", "https://example.com/"],
            &["https://example.com/", "notes.txt"],
            &["x-apple.systempreferences:com.apple.preference.security"],
        ] {
            assert_eq!(shell.run(OPENER, args), Some(0), "{args:?}");
            assert_eq!(shell.said(OPENER), Some(format!("{}|", args.join("|"))), "{args:?}");
        }
    }

    /// With no worker to ask, a web address opens here as it would without Slopty, and an editor
    /// is `vi` in this terminal, given the same line and file.
    #[test]
    fn with_no_worker_the_systems_own_take_over() {
        let shell = Shell::new();
        assert_eq!(shell.run(OPENER, &["https://example.com/a?b=c"]), Some(0));
        assert_eq!(shell.said(OPENER).as_deref(), Some("https://example.com/a?b=c|"));
        assert_eq!(shell.run(BROWSER_SHIM, &["https://example.com/"]), Some(0));
        assert_eq!(shell.said(OPENER).as_deref(), Some("https://example.com/|"));
        assert_eq!(shell.run(EDITOR_SHIM, &["+3", "notes.txt"]), Some(0));
        assert_eq!(shell.said("vi").as_deref(), Some("+3|notes.txt|"));
    }

    /// The system's own command's exit status comes back as the handoff command's, so a
    /// program that checks its opener or editor sees what it would have without Slopty.
    #[test]
    fn the_systems_exit_status_comes_through() {
        let shell = Shell::new();
        assert_eq!(shell.run_exiting(OPENER, &["README.md"], 3), Some(3));
        assert_eq!(shell.run_exiting(OPENER, &["https://example.com/"], 5), Some(5));
        assert_eq!(shell.run_exiting(EDITOR_SHIM, &["notes.txt"], 4), Some(4));
        assert_eq!(shell.said("vi").as_deref(), Some("notes.txt|"));
    }
}
